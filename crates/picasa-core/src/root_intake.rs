//! # 添加照片文件夹时的入口判定（M4.3 `IDX-01`）
//!
//! 「添加照片文件夹」看起来只是 `roots` 表插一行，实际上有三件事必须先想清楚：
//!
//! 1. **同路径重复添加**：用户很可能对一个已经关注的目录再点一次。
//!    正确行为是**复用**既有的 `root_id`（连同排除项、已有关注态），不是插第二行。
//! 2. **父子重叠**：先加 `/Photos` 再加 `/Photos/2020`，或者反过来。
//!    照片本身不会重复计数——`photos` 按「文件夹路径 + 文件名」唯一，同一个文件
//!    只有一行；`folders.root_id` 由最长前缀归主（`derive_hierarchy` 里 roots 按
//!    路径长度倒序）。真正会让人困惑的是**树里同一个目录出现两次**、以及
//!    「移除一个根为什么另一个根也少了照片」。所以这里要把关系**说出来**。
//! 3. **权限/可读性**：选到一个没有读权限的目录时，静默加进去只会得到一棵空树
//!    和一条看不懂的失败任务。应当在入口就给出原因，并**不**写入索引。
//!
//! 本模块只有纯函数，不碰数据库、不碰文件系统——文件系统探测留在调用方，
//! 这样判定逻辑本身可以脱离磁盘做单元测试。

/// 一次「添加照片文件夹」与既有根目录的关系。
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RootIntake {
    /// 全新的根，与现有根没有任何重叠。
    Fresh,
    /// 与某个已关注的根**完全同路径**（应当复用它的 `root_id`）。
    SameRoot(String),
    /// 新根是某些既有根的**祖先**：加入后这些既有根会落到它下面。
    ///
    /// 照片不会重复计数（同一文件只有一行），但树里会出现嵌套的两棵树。
    ParentOf(Vec<String>),
    /// 新根是某个既有根的**后代**：它的照片本来就已经被那个根覆盖了。
    ChildOf(String),
}

impl RootIntake {
    /// 给用户看的短标签（面板/提示行用）。
    pub fn label(&self) -> &'static str {
        match self {
            RootIntake::Fresh => "新文件夹",
            RootIntake::SameRoot(_) => "已关注",
            RootIntake::ParentOf(_) => "包含已关注文件夹",
            RootIntake::ChildOf(_) => "已在关注范围内",
        }
    }

    /// 是否会在树里造成「同一批照片出现两次」的观感（父子重叠）。
    pub fn overlaps(&self) -> bool {
        matches!(self, RootIntake::ParentOf(_) | RootIntake::ChildOf(_))
    }

    /// 是否应当**跳过**实际添加。
    ///
    /// 只有完全同路径才跳过（复用既有根）；父子重叠仍然允许添加——
    /// 用户可能就是想单独关注子目录，硬拒会挡住合理用法。重叠只做提示。
    pub fn skips_insert(&self) -> bool {
        matches!(self, RootIntake::SameRoot(_))
    }
}

/// 判定 `candidate` 与既有根目录（已规范化、互不相同的绝对路径）的关系。
///
/// `existing` 里的条目应当是 `roots.path` 的原样值；比较走 `path_key`
/// （NFC + 小写）以免大小写差异被当成两个根——macOS 默认卷不区分大小写。
pub fn classify(candidate: &str, existing: &[String]) -> RootIntake {
    let key = crate::paths::compare_key(candidate);
    if existing
        .iter()
        .any(|r| crate::paths::compare_key(r) == key)
    {
        // 同路径：哪怕大小写写法不同，也是同一个根。
        let hit = existing
            .iter()
            .find(|r| crate::paths::compare_key(r) == key)
            .cloned()
            .unwrap_or_else(|| candidate.to_string());
        return RootIntake::SameRoot(hit);
    }

    // 新根是祖先：既有根里有谁落在它下面（`is_within(既有根, 新根)`）。
    let mut parents: Vec<String> = existing
        .iter()
        .filter(|r| is_within(r, candidate))
        .cloned()
        .collect();
    if !parents.is_empty() {
        parents.sort();
        return RootIntake::ParentOf(parents);
    }

    // 新根是后代：找包住它的、最长的那个既有根——用户需要知道「最贴近的覆盖者」。
    let closest = existing
        .iter()
        .filter(|r| is_within(candidate, r))
        .max_by_key(|r| r.chars().count())
        .cloned();
    match closest {
        Some(r) => RootIntake::ChildOf(r),
        None => RootIntake::Fresh,
    }
}

/// `child` 是否严格位于 `parent` 之下（不含相等）。
fn is_within(child: &str, parent: &str) -> bool {
    if child == parent {
        return false;
    }
    let mut p = parent.to_string();
    if !p.ends_with('/') {
        p.push('/');
    }
    child.starts_with(&p)
}

/// 入口处就探一次目录可读性（`IDX-01` 验收要求「权限失败给原因」）。
///
/// 只读一次目录项、不递归、不建任何文件。返回 `Some(说明)` 表示**不要添加**：
/// 加进去只会得到一棵空树加一条看不懂的失败任务。
///
/// 注意这里只判「能不能 list」——单个文件的读取权限在扫描时才暴露，
/// 由缩略图/元数据的失败分类去报（IDX-17）。
pub fn probe_dir(dir: &std::path::Path) -> Option<String> {
    match std::fs::read_dir(dir) {
        Ok(mut it) => {
            // 目录能被打开但立即读出错（外置盘半挂载）也算不可用。
            if let Some(Err(e)) = it.next() {
                return unreadable_reason(&e);
            }
            None
        }
        Err(e) => unreadable_reason(&e),
    }
}

/// 权限探测失败时给用户的说明（`IDX-01` 验收要求「权限失败给原因」）。
///
/// 返回 `None` 表示没读出错、可以继续添加。
pub fn unreadable_reason(err: &std::io::Error) -> Option<String> {
    use std::io::ErrorKind;
    let why = match err.kind() {
        ErrorKind::PermissionDenied => "没有读取权限（系统设置 → 隐私与安全性 → 文件与文件夹）",
        ErrorKind::NotFound => "目录不存在，或所在磁盘没有挂载",
        _ => "无法读取该目录",
    };
    Some(format!("{why}：{err}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn s(v: &[&str]) -> Vec<String> {
        v.iter().map(|x| x.to_string()).collect()
    }

    #[test]
    fn fresh_root_has_no_overlap() {
        let c = classify("/Volumes/New/Photos", &s(&["/Users/me/Pictures"]));
        assert_eq!(c, RootIntake::Fresh);
        assert!(!c.overlaps());
        assert!(!c.skips_insert());
        assert_eq!(c.label(), "新文件夹");
    }

    #[test]
    fn same_path_is_reused_including_case_differences() {
        let existing = s(&["/Users/me/Pictures"]);
        assert_eq!(
            classify("/Users/me/Pictures", &existing),
            RootIntake::SameRoot("/Users/me/Pictures".into())
        );
        // macOS 默认卷不区分大小写：换一种写法仍然是同一个根。
        assert_eq!(
            classify("/users/me/pictures", &existing),
            RootIntake::SameRoot("/Users/me/Pictures".into())
        );
        assert!(classify("/Users/me/Pictures", &existing).skips_insert());
    }

    #[test]
    fn adding_a_parent_lists_the_children_it_covers() {
        let existing = s(&["/Users/me/Pictures/2020", "/Users/me/Pictures/2021"]);
        let c = classify("/Users/me/Pictures", &existing);
        match &c {
            RootIntake::ParentOf(v) => {
                assert_eq!(v.len(), 2, "两个既有子根都要列出来：{v:?}");
                assert!(v.contains(&"/Users/me/Pictures/2020".to_string()));
                assert!(v.contains(&"/Users/me/Pictures/2021".to_string()));
            }
            other => panic!("应判定为祖先，实际 {other:?}"),
        }
        assert!(c.overlaps());
        assert!(
            !c.skips_insert(),
            "父子重叠仍允许添加，只做提示——硬拒会挡住「单独关注子目录」的合理用法"
        );
    }

    #[test]
    fn adding_a_child_reports_the_closest_covering_root() {
        let existing = s(&["/Users/me/Pictures", "/Users/me/Pictures/2020"]);
        assert_eq!(
            classify("/Users/me/Pictures/2020/01", &existing),
            RootIntake::ChildOf("/Users/me/Pictures/2020".into()),
            "应当报最贴近的覆盖者，而不是最外层那个"
        );
        // 同一批既有根里再加一个子目录：同路径走 SameRoot，真正的新子目录走 ChildOf。
        assert_eq!(
            classify("/Users/me/Pictures/2020", &existing),
            RootIntake::SameRoot("/Users/me/Pictures/2020".into())
        );
        assert!(classify("/Users/me/Pictures/2020/12", &existing).overlaps());
    }

    #[test]
    fn sibling_with_shared_prefix_is_not_mistaken_for_a_child() {
        // `/Pictures2` 不是 `/Pictures` 的子目录——纯粹的前缀比较会误判。
        assert_eq!(
            classify("/Users/me/Pictures2", &s(&["/Users/me/Pictures"])),
            RootIntake::Fresh
        );
        // 反过来也一样。
        assert_eq!(
            classify("/Users/me/Pictures", &s(&["/Users/me/Pictures2"])),
            RootIntake::Fresh
        );
    }

    #[test]
    fn permission_failures_explain_themselves() {
        let denied = std::io::Error::from(std::io::ErrorKind::PermissionDenied);
        let msg = unreadable_reason(&denied).expect("权限错误必须有说明");
        assert!(msg.contains("没有读取权限"), "要给出可操作的下一步：{msg}");
        assert!(msg.contains("文件与文件夹"), "要指到系统设置的位置：{msg}");

        let missing = std::io::Error::from(std::io::ErrorKind::NotFound);
        assert!(unreadable_reason(&missing)
            .expect("缺失目录也要说明")
            .contains("没有挂载"));
    }
}
