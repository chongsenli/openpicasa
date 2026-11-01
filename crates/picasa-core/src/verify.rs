//! §4.8 启动增量校验的**执行器**（`VerifyPlan` 只排计划，这里才是真正去盘的那一半）。
//!
//! ## 为什么必须有它
//!
//! P-12 裁决把启动增量校验定为本版**唯一**的变化发现机制。只排计划不执行，
//! 用户在外部增删照片后软件**永远**发现不了，除非手动刷新——那是产品级缺口。
//!
//! ## 限速
//!
//! 首屏有图之后 [`STARTUP_VERIFY_DELAY_MS`] 才开始，之后每目录间隔
//! [`VERIFY_DIR_INTERVAL_MS`]（50ms）。4330 个目录 ≈ 3.6 分钟走完一轮。
//! 这个数字不是随手定的：一次 `read_dir` + 若干个 `stat` 通常 < 5ms，
//! 50ms 的间隔让校验只占用约 10% 的单核，用户在校验期间仍然能正常翻页。

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use picasa_db::{Db, FolderFingerprint};

use crate::filters::{is_supported_image, should_skip_dir};
use crate::mgmt::{VerifyPlan, VerifyProgress};

/// `exFAT`/`FAT32` 的时间戳精度是 **2 秒**，`ext4` 是纳秒，`APFS` 也是纳秒。
///
/// ⚠️ 直接按纳秒比对 mtime 在 exFAT 盘上会**全部判成「有变化」**——
/// 库里存的是上次挂载时的读数，这次挂载的量化相位可能不同。
/// 所以 mtime 只在**超过这个容差**时才算变化。
pub const MTIME_TOLERANCE_S: u64 = 2;

/// 一次 `tick` 的结果。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum VerifyTick {
    /// 这一轮什么都没做（还在延迟里，或还没到下一个目录的间隔）
    Idle,
    /// 检查完一个目录
    CheckedOne,
    /// 全部走完
    Finished,
}

/// 校验阶段（诊断与测试用）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum VerifyPhase {
    /// 首屏延迟中（把 CPU 让给「先出图」）
    Delay,
    /// 正在逐目录比对
    Walking,
    Done,
}

/// 启动增量校验的执行器。
///
/// 用法：UI 每帧调一次 [`VerifyRunner::tick`]，它自己管限速，
/// 调用方**不需要**自己 sleep。跑完后用 [`VerifyRunner::take_changed`]
/// 取出「需要重新扫描的目录」。
pub struct VerifyRunner {
    plan: VerifyPlan,
    started: Instant,
    last_dir: Instant,
    pub phase: VerifyPhase,
    /// 待校验的目录（库内指纹）
    dirs: Vec<FolderFingerprint>,
    next: usize,
    /// 库内已知目录路径（用来发现外部新建的目录）
    known: HashSet<String>,
    /// 已累积但还没提交的「校验过」标记
    verified_pending: Vec<i64>,
    progress: VerifyProgress,
    changed: Vec<(i64, String)>,
    new_dirs: Vec<PathBuf>,
    loaded: bool,
}

impl VerifyRunner {
    pub fn new(plan: VerifyPlan) -> Self {
        let now = Instant::now();
        Self {
            plan,
            started: now,
            last_dir: now,
            phase: VerifyPhase::Delay,
            dirs: Vec::new(),
            next: 0,
            known: HashSet::new(),
            verified_pending: Vec::new(),
            progress: VerifyProgress::default(),
            changed: Vec::new(),
            new_dirs: Vec::new(),
            loaded: false,
        }
    }

    /// 无事可做（全关或全离线）→ 调用方可以不启动它。
    pub fn is_empty(&self) -> bool {
        self.plan.is_empty()
    }

    /// 覆盖「首屏延迟」（**只给基准/测试用**）。
    ///
    /// 产品路径永远是 [`STARTUP_VERIFY_DELAY_MS`]；基准要量纯 IO 成本，
    /// 等 2 秒延迟纯属浪费。
    pub fn set_delay_ms(&mut self, ms: u64) {
        self.plan.delay_ms = ms;
    }

    /// 覆盖「每目录间隔」（**只给基准/测试用**）。设 0 = 不限速。
    pub fn set_dir_interval_ms(&mut self, ms: u64) {
        self.plan.dir_interval_ms = ms;
    }

    pub fn progress(&self) -> VerifyProgress {
        self.progress
    }

    /// 取出「需要重新扫描的目录」`(folder_id, 路径)`（取走即清空，不重复触发）。
    pub fn take_changed(&mut self) -> Vec<(i64, String)> {
        std::mem::take(&mut self.changed)
    }

    /// 外部新建、库里还没有的目录（给调用方决定要不要加入索引）。
    pub fn take_new_dirs(&mut self) -> Vec<PathBuf> {
        std::mem::take(&mut self.new_dirs)
    }

    /// 推进一步。**每帧调一次**就够了，内部自己按 50ms 限速。
    pub fn tick(&mut self, db: &Db) -> VerifyTick {
        match self.phase {
            VerifyPhase::Delay => {
                if self.started.elapsed() < Duration::from_millis(self.plan.delay_ms) {
                    return VerifyTick::Idle;
                }
                if !self.loaded {
                    self.load(db);
                    self.loaded = true;
                    self.phase = VerifyPhase::Walking;
                    return VerifyTick::Idle;
                }
                self.phase = VerifyPhase::Walking;
                VerifyTick::Idle
            }
            VerifyPhase::Walking => {
                if self.last_dir.elapsed() < Duration::from_millis(self.plan.dir_interval_ms) {
                    return VerifyTick::Idle;
                }
                self.last_dir = Instant::now();
                match self.step(db) {
                    Some(true) => VerifyTick::CheckedOne,
                    Some(false) => {
                        self.flush(db);
                        self.phase = VerifyPhase::Done;
                        self.progress.done = true;
                        VerifyTick::Finished
                    }
                    // 加载失败或目录列表为空：直接结束，不要卡在 Walking 里空转
                    None => {
                        self.phase = VerifyPhase::Done;
                        self.progress.done = true;
                        VerifyTick::Finished
                    }
                }
            }
            VerifyPhase::Done => VerifyTick::Finished,
        }
    }

    /// 从库里拉指纹 + 已知目录集合。
    ///
    /// `VerifyPlan` 里只有路径（它是从快照排的，快照按路径展示），
    /// 所以这里先取一次 roots 表把路径换成 id。
    fn load(&mut self, db: &Db) {
        let Ok(all_roots) = db.roots(true) else {
            return;
        };
        let mut dirs = Vec::new();
        let mut known = HashSet::new();
        for root in &self.plan.roots {
            let want = root.to_string_lossy().to_string();
            let Some(rid) = all_roots.iter().find(|r| r.path == want).map(|r| r.id) else {
                continue;
            };
            if let Ok(mut fps) = db.folder_fingerprints(rid) {
                dirs.append(&mut fps);
            }
            if let Ok(paths) = db.folder_paths_of_root(rid) {
                known.extend(paths);
            }
        }
        self.dirs = dirs;
        self.known = known;
    }

    /// 检查一个目录。`Ok(Some(还有下一个))`。
    fn step(&mut self, db: &Db) -> Option<bool> {
        let fp = self.dirs.get(self.next)?.clone();
        self.next += 1;
        let path = PathBuf::from(&fp.path);

        match stat_dir(&path) {
            Some((count, size, mtime_max, subdirs)) => {
                self.progress.dirs_checked += 1;

                // 外部新建的子目录：库里没有 → 记下来（父目录也要重扫，因为树要更新）
                let mut new_here = 0usize;
                for sub in &subdirs {
                    let key = sub.to_string_lossy().to_string();
                    if !self.known.contains(&key) {
                        self.new_dirs.push(sub.clone());
                        new_here += 1;
                    }
                }

                let mtime_differs = fp.mtime_max.abs_diff(mtime_max)
                    > MTIME_TOLERANCE_S.saturating_mul(1_000_000_000);
                let differs = count != fp.photo_count || size != fp.size_sum || mtime_differs;

                if differs || new_here > 0 {
                    self.changed.push((fp.folder_id, fp.path.clone()));
                    self.progress.changed_files += count.abs_diff(fp.photo_count);
                    self.progress.new_dirs += new_here as u64;
                }
            }
            None => {
                // 打不开：盘被拔了、权限变了、目录被删了。
                // **不**算失败也不进角标——离线根的处置在快照里另有逻辑（§4.2）。
                // 但也不更新 last_verified_at：这一轮并没有真的校验过它。
                return Some(self.next < self.dirs.len());
            }
        }

        self.verified_pending.push(fp.folder_id);
        if self.verified_pending.len() >= 32 {
            self.flush(db);
        }
        Some(self.next < self.dirs.len())
    }

    /// 把攒起来的「已校验」批量落盘（单条 UPDATE 在 4330 个目录上会造成 4330 次写事务）。
    fn flush(&mut self, db: &Db) {
        if self.verified_pending.is_empty() {
            return;
        }
        let ids = std::mem::take(&mut self.verified_pending);
        let _ = db.mark_verified_many(&ids);
    }
}

/// 实时统计一个目录：`(图片文件数, 字节总和, 最大 mtime_ns, 子目录列表)`。
///
/// `None` = 打不开（离线/权限/已删除）。
fn stat_dir(path: &Path) -> Option<(u64, u64, u64, Vec<PathBuf>)> {
    let rd = std::fs::read_dir(path).ok()?;
    let mut count = 0u64;
    let mut size = 0u64;
    let mut mtime_max = 0u64;
    let mut subdirs = Vec::new();

    for entry in rd.flatten() {
        let p = entry.path();
        let Ok(meta) = entry.metadata() else { continue };
        if meta.is_dir() {
            let skip = p
                .file_name()
                .and_then(|n| n.to_str())
                .map(should_skip_dir)
                .unwrap_or(false);
            if !skip {
                subdirs.push(p);
            }
            continue;
        }
        // ⚠️ 只数格式白名单内的文件：目录里躺着的 .txt/.ini 不能算「照片数变了」，
        //    否则每个有点杂项的目录都会被判成有变化（假阳性）。
        if !is_supported_image(&p) {
            continue;
        }
        count += 1;
        size += meta.len();
        let mt = meta
            .modified()
            .ok()
            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
            .map(|d| d.as_nanos() as u64)
            .unwrap_or(0);
        mtime_max = mtime_max.max(mt);
    }
    Some((count, size, mtime_max, subdirs))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mtime_tolerance_covers_exfat_2s_granularity() {
        // exFAT 上同一文件的 mtime 读数可能差整整 2 秒 → 必须被容差吸收
        let a = 1_700_000_000_000_000_000u64;
        let b = a + 2_000_000_000; // +2s
        assert!(a.abs_diff(b) <= MTIME_TOLERANCE_S * 1_000_000_000);
        let c = a + 5_000_000_000; // +5s：真的变了
        assert!(a.abs_diff(c) > MTIME_TOLERANCE_S * 1_000_000_000);
    }

    #[test]
    fn stat_dir_only_counts_supported_images() {
        let dir = std::env::temp_dir().join(format!("op-verify-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        std::fs::write(dir.join("a.jpg"), vec![0u8; 100]).unwrap();
        std::fs::write(dir.join("b.txt"), vec![0u8; 999]).unwrap(); // 非图片：不该被数进去
        let (count, size, _, _) = stat_dir(&dir).expect("能读");
        assert_eq!(count, 1, "只数白名单内的图片");
        assert_eq!(size, 100, "非图片文件的字节也不该计入");
    }
}
