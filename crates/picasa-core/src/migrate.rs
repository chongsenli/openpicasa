//! M2.1 迁移的**上层胶水**：这里才有「偏好文件」与「文件系统」的知识。
//!
//! `picasa-db` 是叶子 crate，既不认识 `preferences.json`，也不该去 `lstat` 用户的卷。
//! 所以两件事放在这儿：
//!
//! 1. **把 `Preferences.roots` 变成 `roots` 表的种子**——迁完之后 prefs 的 `roots`
//!    降级为只读兼容字段，任何「加/关根目录」都不再写它（`docs/05` §5 第 1 条）。
//! 2. **`case_insensitive` 的可移植探测**——初稿写的是 `diskutil`（macOS 专有），
//!    而本项目 Linux 已提前打通，测试都在 Linux 上跑，必须换掉（`docs/05` §1.3.2）。

use crate::{Result, paths, prefs::Preferences};
use picasa_db::{Db, RootSeed, V2DataReport};
use std::path::{Path, PathBuf};

/// 探测一个根目录所在卷**是否不区分大小写**，结果会固化进 `roots.case_insensitive`。
///
/// 口径（`docs/05` §1.3.2）：
///
/// 1. 取根目录下任一「含大小写差异」的条目，尝试用它的大写变体去 `lstat`；
/// 2. 命中同一 inode → 不区分大小写 → `true`；明确不命中 → `false`；
/// 3. 目录为空 / 全是无大小写字符 / 探测失败 → 默认 **`true`**。
///
/// 为什么默认 `true`：宁可多归一，不要漏判。漏判的后果是「关掉 `Photos` 对 `photos`
/// 不生效」，这类 bug 用户说不清、开发者也不好复现；多归一的后果只是两个本来就同名的
/// 目录被当成一个——在区分大小写的卷上极少发生，且诊断页能看到。
pub fn probe_case_insensitive(root: &Path) -> bool {
    let Ok(entries) = std::fs::read_dir(root) else {
        return true;
    };
    for entry in entries.flatten().take(64) {
        let Some(name) = entry.file_name().to_str().map(str::to_string) else {
            continue;
        };
        let upper = name.to_uppercase();
        if upper == name {
            // 全大写或没有大小写字符（中文/数字）：这个条目判定不了，换下一个
            continue;
        }
        let probe = root.join(&upper);
        let Ok(probe_meta) = std::fs::symlink_metadata(&probe) else {
            // 大写变体不存在 → 这个卷区分大小写
            return false;
        };
        let Ok(orig_meta) = entry.metadata() else {
            continue;
        };
        if same_file(&orig_meta, &probe_meta) {
            return true;
        }
        // 大写变体是另一个文件/目录（卷上真的同时存在两份）→ 区分大小写
        return false;
    }
    true
}

#[cfg(unix)]
fn same_file(a: &std::fs::Metadata, b: &std::fs::Metadata) -> bool {
    use std::os::unix::fs::MetadataExt;
    a.dev() == b.dev() && a.ino() == b.ino()
}

#[cfg(not(unix))]
fn same_file(a: &std::fs::Metadata, b: &std::fs::Metadata) -> bool {
    // 非 Unix 上没有 inode 可比：只要能被 lstat 到就当命中（macOS/Linux 都走上面的分支，
    // 这里只是为了让 `cfg` 完整，不在实际交付平台上运行）。
    a.is_dir() == b.is_dir() && a.len() == b.len()
}

/// 偏好文件的 mtime（Unix 秒）。**取不到就返回 0，不编造**。
///
/// 用 `MetadataExt::mtime()` 而不是 `SystemTime`：`clippy.toml` 明令禁用后者
/// （跨平台序列化语义不稳），时间统一成 i64。
#[cfg(unix)]
fn file_mtime_secs(path: &Path) -> Option<i64> {
    use std::os::unix::fs::MetadataExt;
    std::fs::metadata(path).ok().map(|m| m.mtime())
}

#[cfg(not(unix))]
fn file_mtime_secs(_path: &Path) -> Option<i64> {
    None
}

/// 从偏好构造 roots 种子。这是 `Preferences.roots` **最后一次被读**的场合。
pub fn seed_roots(prefs: &Preferences, paths: &crate::AppPaths) -> Vec<RootSeed> {
    let added_at = file_mtime_secs(&paths.preferences_file()).unwrap_or(0);
    prefs
        .roots
        .iter()
        .map(|r| {
            let p = PathBuf::from(r);
            RootSeed {
                path: paths::canonical_string(&p),
                path_key: paths::compare_key(r),
                added_at,
                case_insensitive: Some(probe_case_insensitive(&p)),
            }
        })
        .collect()
}

/// 需要时把 v1 库补成 v2 数据（roots 迁入 + 层级推导）。**幂等**：跑过就返回 `None`。
pub fn ensure_v2_data(
    db: &Db,
    prefs: &Preferences,
    paths: &crate::AppPaths,
) -> Result<Option<V2DataReport>> {
    if !db.needs_v2_data()? {
        return Ok(None);
    }
    let seeds = seed_roots(prefs, paths);
    let rep = db.apply_v2_data(&seeds)?;
    tracing::info!(
        roots = rep.roots,
        folders_created = rep.folders_created,
        folders_linked = rep.folders_linked,
        photos = rep.photos_stamped,
        "v2 数据步骤完成（roots 迁入 + 文件夹层级推导）"
    );
    Ok(Some(rep))
}

/// 当前 active 的根目录路径。**树只认这个**——DB 是唯一真相源。
///
/// 表都还没有（老库降级运行）时返回空，调用方自己回退到偏好。
pub fn active_root_paths(db: &Db) -> Vec<String> {
    match db.roots(false) {
        Ok(rows) => rows.into_iter().map(|r| r.path).collect(),
        Err(e) => {
            tracing::warn!(error = %e, "读 roots 失败，本次以偏好里的根目录兜底");
            Vec::new()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn case_sensitive_volume_is_detected() {
        // 在 ext4/APFS(默认) 这种区分大小写的卷上：造 `Photos`，探测应得 false
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir(dir.path().join("Photos")).unwrap();
        assert!(
            !probe_case_insensitive(dir.path()),
            "大写变体不存在时，必须判定为区分大小写"
        );
    }

    #[test]
    fn empty_directory_defaults_to_insensitive() {
        let dir = tempfile::tempdir().unwrap();
        assert!(
            probe_case_insensitive(dir.path()),
            "探测不了就默认不区分（宁可多归一，不要漏判）"
        );
        assert!(probe_case_insensitive(Path::new("/definitely/not/here")));
    }

    #[test]
    fn names_without_case_are_skipped() {
        // 只有中文与数字时无从判定 → 默认 true
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir(dir.path().join("苏州行2011")).unwrap();
        assert!(probe_case_insensitive(dir.path()));
    }
}
