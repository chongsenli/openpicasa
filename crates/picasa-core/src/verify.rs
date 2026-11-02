//! 两级校验：**L1 目录 mtime 快筛 + L2 文件级全校**（`07` §3，P-16 落定）。
//!
//! ## 为什么必须分成两级
//!
//! M2.2 的启动校验是「全树 `read_dir` + 逐文件 `stat`」。25.9 万张 / 4330 目录实测
//! 约 20 分钟（限速后更长）——用户每次启动都在等这个，而**绝大多数启动时一个文件都没变**。
//!
//! L1 只对**目录**做一次 `stat`：4330 次 `stat` 在 SSD 上是**数百毫秒**量级。
//! 目录 `st_mtime` 没变就跳过整个目录（含它的 `read_dir`），变了才进 L2。
//!
//! ## ⚠️ L1 有一个必须写进文档的盲区
//!
//! **目录 mtime 只在「条目增删改名」时变化**。用别的软件打开一张照片、编辑、保存回同名文件
//! ——目录里还是那几个名字，mtime 完全不变。L1 看不到这种变化。
//!
//! 这不是缺陷，是 L1 的设计边界，也正因为如此：
//!
//! - L2 周期全校必须存在（`verify.full_interval_h`，默认 24 h）；
//! - 监视器 `Active` 时 L2 间隔可以放长（FSEvents 能捉到写事件），
//!   `Polling` 时必须缩短（`full_interval_polling_h`，默认 6 h）；
//! - **外置盘恢复在线 → 强制 L2**（`07` §3.2 VERIFY-06）：离线期间任何事件都没捉到，
//!   快筛在「盘根本没插」这段时间里完全失明，插回来只看目录 mtime 是不够的。
//!
//! ## 与监视的分工（`07` §2.4）
//!
//! 监视器把发现延迟从「下次启动」缩到秒级，L1/L2 负责**兜底正确性**。
//! 所以监视器丢事件只影响时效，不影响正确性——这也是 R-M3-7 要求
//! 「降级可见」而不是「降级即失败」的原因。

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use picasa_db::{Db, FolderFingerprint};

use crate::filters::{is_supported_image, should_skip_dir};
use crate::mgmt::{VerifyPlan, VerifyProgress};

/// **L2 文件级**比对用的 mtime 容差（秒）。
///
/// `exFAT`/`FAT32` 的时间戳精度是 **2 秒**，`ext4` 是纳秒，`APFS` 也是纳秒。
/// ⚠️ 直接按纳秒比对文件 mtime 在 exFAT 盘上会**把整个目录判成有变化**——
/// 库里存的是上次挂载时的读数，这次挂载的量化相位可能不同。
///
/// ## ⚠️ 这个容差**只用于文件级（L2）**，目录级（L1）用的是精确比对
///
/// 两者的误差代价不对称：
///
/// | | 判成「有变化」的代价 | 判成「没变化」的代价 |
/// | --- | --- | --- |
/// | L1 目录 | 多扫一个目录（可接受） | **用户的改动被静默吞掉**（不可接受） |
/// | L2 文件 | 多比一次（可忽略） | 同上 |
///
/// 而 2 秒的容差在 L1 上会吞掉「同一秒内新增的文件」——
/// 那不是理论问题：拷进 1000 张的过程中，目录 mtime 一直在同一秒里跳。
/// 所以 L1 用**精确纳秒**：宁可外置盘重挂后多跑一次全校（它会自我修复，
/// 跑完就把新读数写回去），也不能漏掉一张照片。
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
    /// L1：逐目录 `stat` 快筛
    FastScreening,
    /// L2：有变化的目录做文件级比对
    FullScanning,
    Done,
}

/// 校验级别（`07` §3.1）。任务标签与诊断页按这个显示。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum VerifyLevel {
    /// L1 快筛：每目录 1 次 `stat`
    L1,
    /// L2 全校：`read_dir` + 逐文件 `stat`
    L2,
}

impl VerifyLevel {
    pub fn label(self) -> &'static str {
        match self {
            Self::L1 => "快筛",
            Self::L2 => "全校",
        }
    }
}

/// 启动/轮询校验的执行器（L1 → 有变化的目录进 L2）。
///
/// 用法：UI 每帧调一次 [`VerifyRunner::tick`]，它自己管限速，
/// 调用方**不需要**自己 sleep。跑完后用 [`VerifyRunner::take_changed`]
/// 取出「需要重新扫描的目录」。
///
/// ## 为什么 L1 与 L2 在同一个 state machine 里
///
/// 因为它们共用「一次只处理一个目录 + 限速」这条约束。拆成两个 runner 会让
/// 限速器出现两份，而两份必然漂移（一个按 50 ms、另一个按 50 ms 但各自计时，
/// 合起来就是 25 ms/目录，把「只占 10% 单核」的承诺打掉）。
pub struct VerifyRunner {
    plan: VerifyPlan,
    started: Instant,
    last_dir: Instant,
    pub phase: VerifyPhase,
    /// 一批待处理的目录（L1 用指纹，L2 用同样的指纹结构——它本来就有全部字段）
    dirs: Vec<FolderFingerprint>,
    next: usize,
    /// 库内已知目录路径（用来发现外部新建的目录）
    known: HashSet<String>,
    /// 已累积但还没提交的「校验过」标记
    verified_pending: Vec<i64>,
    progress: VerifyProgress,
    /// L1 判定有变化的目录，接下来要进 L2
    changed: Vec<(i64, String)>,
    new_dirs: Vec<PathBuf>,
    loaded: bool,
    /// 本轮是否**跳过 L1 直接做 L2**（`07` §3.2 VERIFY-06：离线恢复强制全校）
    force_full: bool,
    /// 本轮各级做了多少（`07` §3.2 VERIFY-05 的「已检查 N 目录 / M 文件」）
    l1_dirs: u64,
    l2_dirs: u64,
    /// 目录级 `stat` 次数与文件级 `stat` 次数——**用例要断言的就是这两个数**：
    /// `TC-M3-VERIFY-001` 要求「无变化库启动，文件级 stat = 0，目录级 stat = 目录数」。
    pub dir_stats: u64,
    pub file_stats: u64,
}

impl VerifyRunner {
    pub fn new(plan: VerifyPlan) -> Self {
        Self::with_mode(plan, false)
    }

    /// 强制 L2 的构造器（外置盘恢复在线时用，`07` §3.2 VERIFY-06）。
    ///
    /// 「强制」的含义是**跳过 L1 快筛**，直接对范围内所有目录做文件级比对。
    /// 不这样做的话，离线期间目录 mtime 也没变（盘都没插，怎么可能变），
    /// L1 会把整个根判成「无变化」，而用户明明在别的机器上往里拷了 500 张。
    pub fn new_forced_full(plan: VerifyPlan) -> Self {
        Self::with_mode(plan, true)
    }

    fn with_mode(plan: VerifyPlan, force_full: bool) -> Self {
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
            force_full,
            l1_dirs: 0,
            l2_dirs: 0,
            dir_stats: 0,
            file_stats: 0,
        }
    }

    /// 无事可做（全关或全离线）→ 调用方可以不启动它。
    pub fn is_empty(&self) -> bool {
        self.plan.is_empty()
    }

    /// 本轮实际执行的级别（L1 快筛 / L2 全校）。
    pub fn level(&self) -> VerifyLevel {
        if self.force_full {
            VerifyLevel::L2
        } else {
            VerifyLevel::L1
        }
    }

    /// 覆盖「首屏延迟」（**只给基准/测试用**）。
    ///
    /// 产品路径永远是 [`crate::mgmt::STARTUP_VERIFY_DELAY_MS`]；基准要量纯 IO 成本，
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

    /// 「已检查 N 目录 / M 文件」（`07` §3.2 VERIFY-05）。
    ///
    /// 分开报两个数是有意义的：L1 轮里 `files = 0` 是**正确行为**，不是「什么都没干」。
    /// 若只报一个总数，用户会以为快筛没跑。
    pub fn progress_detail(&self) -> String {
        format!(
            "{}：已检查 {} 目录 / {} 文件",
            self.level().label(),
            self.l1_dirs + self.l2_dirs,
            self.file_stats
        )
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
                    self.phase = if self.force_full {
                        VerifyPhase::FullScanning
                    } else {
                        VerifyPhase::FastScreening
                    };
                    return VerifyTick::Idle;
                }
                self.phase = if self.force_full {
                    VerifyPhase::FullScanning
                } else {
                    VerifyPhase::FastScreening
                };
                VerifyTick::Idle
            }
            VerifyPhase::FastScreening => {
                if self.last_dir.elapsed() < Duration::from_millis(self.plan.dir_interval_ms) {
                    return VerifyTick::Idle;
                }
                self.last_dir = Instant::now();
                match self.step_fast(db) {
                    Some(true) => VerifyTick::CheckedOne,
                    Some(false) => {
                        // L1 走完：有变化的目录已进 `changed`，本轮结束。
                        //
                        // ⚠️ 这里**不**在同一个 tick 里串 L2：L2 是逐文件 `stat`，
                        // 一个刚拷进 5000 张的目录能吃掉几十秒。让它走下一轮
                        // （由 `enqueue_refresh` 触发的正常扫描路径），
                        // 启动阶段的 UI 才不会被一个热目录顶死。
                        self.flush(db);
                        self.phase = VerifyPhase::Done;
                        self.progress.done = true;
                        VerifyTick::Finished
                    }
                    None => {
                        self.phase = VerifyPhase::Done;
                        self.progress.done = true;
                        VerifyTick::Finished
                    }
                }
            }
            VerifyPhase::FullScanning => {
                if self.last_dir.elapsed() < Duration::from_millis(self.plan.dir_interval_ms) {
                    return VerifyTick::Idle;
                }
                self.last_dir = Instant::now();
                match self.step_full(db) {
                    Some(true) => VerifyTick::CheckedOne,
                    Some(false) => {
                        self.flush(db);
                        self.phase = VerifyPhase::Done;
                        self.progress.done = true;
                        VerifyTick::Finished
                    }
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

    /// 从库里拉指纹 + 已知目录集合 + `dir_mtime`。
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

    /// L1：只 `stat` 目录，比 `dir_mtime`。
    ///
    /// 三种结果：
    /// - `dir_mtime` 是 `NULL` → **进 L2**（`07` §8.2：空 = 还没量过，等价于 M2.2 行为）。
    ///   这是升级后首次启动会慢的原因，也是它之后变快的原因。
    /// - mtime 相同 → 跳过整个目录，**零 `read_dir`**。
    /// - mtime 不同 → 进 L2 路径（记进 `changed`，由调用方排队重扫）。
    fn step_fast(&mut self, db: &Db) -> Option<bool> {
        let fp = self.dirs.get(self.next)?.clone();
        let idx = self.next;
        self.next += 1;

        let Some(dir_mtime_ns) = stat_dir_mtime_ns(Path::new(&fp.path)) else {
            // 打不开：盘被拔了、权限变了、目录被删了。
            // **不算失败也不进角标**——离线根的处置在快照里另有逻辑（§4.2）。
            // 但也不更新 last_verified_at：这一轮并没有真的校验过它。
            return Some(self.next < self.dirs.len());
        };
        self.dir_stats += 1;
        self.l1_dirs += 1;
        self.progress.dirs_checked += 1;

        let stored = db.folder_dir_mtime(fp.folder_id).unwrap_or(None);
        match stored {
            Some(stored) if stored == dir_mtime_ns => {
                // 快筛通过：目录没变。**不碰文件**，但仍要记 last_verified_at——
                // 「我确认它没变」也是一次校验结论。
                self.verified_pending.push(fp.folder_id);
                if self.verified_pending.len() >= 32 {
                    self.flush(db);
                }
            }
            // 没量过 / 变了 → 这个目录要真看一遍
            _ => {
                self.changed.push((fp.folder_id, fp.path.clone()));
                // ⚠️ 不在这里写 `dir_mtime`：它是「遍历完成后」由落库线程写的
                // （VERIFY-02 要求同一事务）。在这里写就等于把「扫描结果」与
                // 「目录指纹」拆成两笔，中间崩了就会出现「指纹说已扫、库里没数据」。
            }
        }

        // 外部新建的子目录只有在目录本身有变化时才可能发现，留到 L2 做。
        let _ = idx;
        Some(self.next < self.dirs.len())
    }

    /// L2：M2.2 §4.8 原逻辑——`read_dir` + 逐文件 `stat`。
    fn step_full(&mut self, db: &Db) -> Option<bool> {
        let fp = self.dirs.get(self.next)?.clone();
        self.next += 1;
        let path = PathBuf::from(&fp.path);

        match stat_dir(&path, &mut self.file_stats) {
            Some((count, size, mtime_max, subdirs)) => {
                self.progress.dirs_checked += 1;
                self.l2_dirs += 1;

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
                    // ⚠️ 有变化时**不**写 `dir_mtime`：
                    // 这轮的 `read_dir` 结果还没来得及落库，此刻写下的 mtime 会与
                    // 库里的照片行错位——下次 L1 就会自信地说「没变」，把这次变化吞掉。
                    // 由真正做完扫描的那条路径写（VERIFY-02：与遍历同事务）。
                } else {
                    // 确认无变化 → 这个目录的指纹就是当前 mtime，补上。
                    // 于是**下一次**启动走 L1 —— 这就是「升级后第一次全校」的意义。
                    if let Ok(Some(dir_mtime)) = stat_dir_mtime_ns_result(&path) {
                        let _ = db.set_folder_dir_mtime(fp.folder_id, Some(dir_mtime));
                    }
                }
            }
            None => {
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

/// 把 `SystemTime` 转成自 epoch 起的纳秒。
///
/// i64 的容量足够：1.79e18 ns（2026 年）离 9.2e18 的边界还有几十年，
/// 而 SQLite 的 INTEGER 本来就是 64 位。
#[expect(clippy::disallowed_types)]
fn to_nanos(t: std::time::SystemTime) -> Option<i64> {
    i64::try_from(t.duration_since(std::time::UNIX_EPOCH).ok()?.as_nanos()).ok()
}

/// 只 `stat` 目录本身，返回 `st_mtime`（**纳秒**）。`None` = 打不开。
///
/// 这是 L1 的**全部成本**：一次 `stat`，不 `read_dir`、不开任何文件。
/// 精确到纳秒而不是秒，理由见 [`MTIME_TOLERANCE_S`] 的对照表。
fn stat_dir_mtime_ns(path: &Path) -> Option<i64> {
    let meta = std::fs::metadata(path).ok()?;
    if !meta.is_dir() {
        return None;
    }
    to_nanos(meta.modified().ok()?)
}

/// 同 [`stat_dir_mtime_ns`]，但把 IO 错误显式返回（L2 的补写路径用）。
fn stat_dir_mtime_ns_result(path: &Path) -> std::io::Result<Option<i64>> {
    let meta = std::fs::metadata(path)?;
    Ok(meta.modified().ok().and_then(to_nanos))
}

/// 实时统计一个目录：`(图片文件数, 字节总和, 最大 mtime_ns, 子目录列表)`。
///
/// `None` = 打不开（离线/权限/已删除）。
/// `file_stats` 是**出参**：每 `stat` 一个文件就 +1。用例靠它断言
/// 「L1 轮里文件级 stat 必须是 0」——那是快筛快在哪里的唯一证据。
fn stat_dir(path: &Path, file_stats: &mut u64) -> Option<(u64, u64, u64, Vec<PathBuf>)> {
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
        *file_stats += 1;
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

use std::collections::HashSet;

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
        let mut files = 0;
        let (count, size, _, _) = stat_dir(&dir, &mut files).expect("能读");
        assert_eq!(count, 1, "只数白名单内的图片");
        assert_eq!(size, 100, "非图片文件的字节也不该计入");
        assert_eq!(files, 1, "文件级 stat 次数 = 图片文件数（非图片根本不该 stat 进去）");
    }

    #[test]
    fn stat_dir_mtime_does_not_walk_the_directory() {
        // L1 的全部价值是「不 read_dir」。这条用例把那个契约钉住：
        // 目录里放 200 个文件，`stat_dir_mtime` 的 file_stats 无从 +1（它根本不收这个参数）。
        let dir = std::env::temp_dir().join(format!("op-verify-l1-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        for i in 0..200 {
            std::fs::write(dir.join(format!("p{i}.jpg")), b"x").unwrap();
        }
        let m = stat_dir_mtime_ns(&dir).expect("目录能 stat");
        assert!(m > 0);
        // 对比：L2 会逐个 stat 进去
        let mut files = 0;
        stat_dir(&dir, &mut files).expect("能读");
        assert_eq!(files, 200, "L2 才会碰到每个文件——这正是两级校验的分界");
    }
}
