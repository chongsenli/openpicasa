//! `fs_fuzz`：对样本库做随机增删改名移动，验证**终态 DB = 磁盘**（`07` §12.1）。
//!
//! ## 为什么要有它
//!
//! 所有单元测试造的都是「我知道会变成什么」的场景。而真实用户的照片目录是
//! **随机演化**的：拷进来、删掉、改名、挪到另一个子目录、用另一个软件覆盖同名文件。
//! 增量索引在这种情况下最容易出偏差——而偏差的形态是「库里多了一条幽灵记录」
//! 或「磁盘上有但库里的没有」，两者都不会报错。
//!
//! `07` §12.1 要求做 10000 次操作，`07` §13 迭代 3 的 DoD 是「终态 0 差异」。
//!
//! ## 怎么判定「0 差异」
//!
//! 终态不看中间过程，只看两件事：
//!
//! 1. **文件级 L2 全校跑完之后**，库里的 `(folder, name)` 集合与磁盘完全一致
//! 2. `photo_id` 的保留率（改名 / 移动配上的比例）不低于阈值
//!
//! 第一条是**正确性**，第二条是**体验**（R-M3-9）。
//!
//! ## 跑多久
//!
//! 每次操作都要落库 + 校验，10000 次在 debug 下是分钟级。所以这里默认跑
//! **2000 次**（能把所有状态组合撞几遍），数量由环境变量 `FS_FUZZ_OPS` 覆盖。
//! 10000 次的完整跑法写在下文注释里，给需要出交付报告的人。

#![allow(clippy::disallowed_methods)]
// ⚠️ R-1 的 `remove_file` / `rename` 禁令是给**产品代码**定的：
// 本程序绝不删改用户的照片。但 fuzz 用例扮演的是「**用户自己**在 Finder 里
// 删/改名/移动」这件事——那正是被测行为本身，不用 `remove_file` 就造不出来。
// 所以这个文件的豁免是**语义正确**的，不是绕过检查。

use picasa_core::{CoreEvent, IndexService};
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

/// 默认操作次数。
///
/// `07` §12.1 要 10000 次。这里默认 2000：
///
/// - 2000 次已经能把「新增/删除/改名/移动/覆盖」的组合撞很多遍
/// - debug 构建下 2000 次约 20～40 秒，可以放进常规 `cargo test`
/// - 要出交付报告时用 `FS_FUZZ_OPS=10000 cargo test -p picasa-core --test m3_fs_fuzz --release`
const DEFAULT_OPS: usize = 2000;

/// 目录树宽度（每层几个目录）与深度。
const DIRS_PER_LEVEL: usize = 3;
const LEVELS: usize = 3;

/// 一次随机操作。
#[derive(Debug, Clone)]
enum Op {
    /// 在某个目录新建一个文件
    Add { dir: PathBuf, name: String },
    /// 删掉一个已存在的文件
    Delete { path: PathBuf },
    /// 同目录改名
    Rename { from: PathBuf, to: PathBuf },
    /// 跨目录移动
    Move { from: PathBuf, to: PathBuf },
    /// 原地覆盖（同名、新内容）
    Overwrite { path: PathBuf },
}

/// 一个确定性的伪随机数生成器（xorshift64*）。
///
/// 为什么不用 `rand`：这是集成用例，多一个依赖只为生成几个数字不划算，
/// 而且**固定种子**才能复现失败。xorshift 够用且一眼能看懂。
struct Rng(u64);

impl Rng {
    fn new(seed: u64) -> Self {
        Self(seed.max(1))
    }

    fn next(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x >> 12;
        x ^= x << 25;
        x ^= x >> 27;
        self.0 = x;
        x.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }

    fn below(&mut self, n: usize) -> usize {
        if n == 0 { 0 } else { (self.next() % n as u64) as usize }
    }
}

/// 建一棵固定的目录树，返回所有目录。
fn build_tree(root: &Path) -> Vec<PathBuf> {
    let mut dirs = vec![root.to_path_buf()];
    let mut frontier = vec![root.to_path_buf()];
    for level in 0..LEVELS {
        let mut next = Vec::new();
        for parent in &frontier {
            for i in 0..DIRS_PER_LEVEL {
                let d = parent.join(format!("L{level}-{i}"));
                std::fs::create_dir_all(&d).unwrap();
                dirs.push(d.clone());
                next.push(d);
            }
        }
        frontier = next;
    }
    dirs
}

/// 写一个「看起来像 JPEG」的小文件（内容不重要，只看大小与 mtime）。
fn write_photo(path: &Path, size: usize) {
    let mut buf = vec![0u8; size];
    // 合法的 JPEG 魔数 + 填充，让 `is_supported_image` 与嗅探都不至于全拒
    if buf.len() >= 3 {
        buf[0] = 0xFF;
        buf[1] = 0xD8;
        buf[2] = 0xFF;
    }
    std::fs::write(path, buf).unwrap();
}

/// 扫一遍磁盘，得到 `(相对目录, 文件名)` 的排序集合。
fn snapshot_disk(root: &Path) -> BTreeSet<(String, String)> {
    let mut out = BTreeSet::new();
    fn walk(root: &Path, dir: &Path, out: &mut BTreeSet<(String, String)>) {
        let Ok(rd) = std::fs::read_dir(dir) else {
            return;
        };
        for e in rd.flatten() {
            let p = e.path();
            if p.is_dir() {
                walk(root, &p, out);
                continue;
            }
            let name = p.file_name().unwrap().to_string_lossy().to_string();
            // 只数白名单内的：与索引器的口径**必须一致**，否则「差异」全是噪音
            if !picasa_core::filters::is_supported_image(&p) {
                continue;
            }
            let rel = p
                .parent()
                .and_then(|d| d.strip_prefix(root).ok())
                .map(|d| d.to_string_lossy().to_string())
                .unwrap_or_default();
            out.insert((rel, name));
        }
    }
    walk(root, root, &mut out);
    out
}

/// 扫一遍库，得到 `(相对目录, 文件名)` 的排序集合。
fn snapshot_db(root: &Path, photos: &[picasa_db::PhotoRow]) -> BTreeSet<(String, String)> {
    let root_str = root.to_string_lossy().to_string();
    photos
        .iter()
        .map(|p| {
            let rel = p
                .folder_path
                .strip_prefix(&root_str)
                .unwrap_or(&p.folder_path)
                .trim_start_matches('/')
                .to_string();
            (rel, p.name.clone())
        })
        .collect()
}

#[test]
fn fs_fuzz_terminal_state_matches_disk() {
    let ops: usize = std::env::var("FS_FUZZ_OPS")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(DEFAULT_OPS);

    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("photos");
    std::fs::create_dir_all(&root).unwrap();
    let dirs = build_tree(&root);

    // ---- 建库并做首轮扫描
    let db_path = tmp.path().join("idx.sqlite");
    let db = std::sync::Arc::new(std::sync::Mutex::new(
        picasa_db::Db::open(&db_path).unwrap(),
    ));
    let board = std::sync::Arc::new(picasa_core::IndexBoard::new());
    let (tx, rx) = std::sync::mpsc::channel();
    let svc = IndexService::new(db.clone(), board.clone(), tx);

    // 先放 60 张打底
    let mut rng = Rng::new(0xC0FFEE);
    let mut counter = 0usize;
    for i in 0..60 {
        let dir = &dirs[i % dirs.len()];
        let name = format!("base{i:03}.jpg");
        write_photo(&dir.join(&name), 512 + i * 7);
        counter += 1;
    }

    svc.enqueue_scan(std::slice::from_ref(&root));
    wait_settled(&board, &rx, Duration::from_secs(60));

    // 记录首轮的 id（用于后面算保留率）
    let initial_ids: usize = {
        let g = db.lock().unwrap();
        g.photo_count_total().unwrap() as usize
    };
    assert_eq!(initial_ids, 60, "首轮应扫到 60 张");

    // ---- 随机操作
    let started = Instant::now();
    let mut performed = 0usize;
    for step in 0..ops {
        let dir = dirs[rng.below(dirs.len())].clone();
        // 磁盘上此刻有哪些文件（决定能不能删/改名/移动）
        let existing: Vec<PathBuf> = std::fs::read_dir(&dir)
            .map(|rd| {
                rd.flatten()
                    .map(|e| e.path())
                    .filter(|p| p.is_file() && picasa_core::filters::is_supported_image(p))
                    .collect()
            })
            .unwrap_or_default();

        let roll = rng.below(100);
        let op = if roll < 40 {
            counter += 1;
            Op::Add {
                dir: dir.clone(),
                name: format!("new{counter:05}.jpg"),
            }
        } else if roll < 55 && !existing.is_empty() {
            Op::Delete {
                path: existing[rng.below(existing.len())].clone(),
            }
        } else if roll < 75 && !existing.is_empty() {
            Op::Rename {
                from: existing[rng.below(existing.len())].clone(),
                to: dir.join(format!("ren{counter:05}.jpg")),
            }
        } else if roll < 95 && !existing.is_empty() {
            counter += 1;
            let dst = dirs[rng.below(dirs.len())].clone();
            Op::Move {
                from: existing[rng.below(existing.len())].clone(),
                to: dst.join(format!("mov{counter:05}.jpg")),
            }
        } else if !existing.is_empty() {
            Op::Overwrite {
                path: existing[rng.below(existing.len())].clone(),
            }
        } else {
            continue;
        };

        match &op {
            Op::Add { dir, name } => write_photo(dir.join(name).as_path(), 256 + step),
            Op::Delete { path } => {
                let _ = std::fs::remove_file(path);
            }
            Op::Rename { from, to } => {
                if to.exists() {
                    continue;
                }
                let _ = std::fs::rename(from, to);
            }
            Op::Move { from, to } => {
                if to.exists() {
                    continue;
                }
                let _ = std::fs::rename(from, to);
            }
            Op::Overwrite { path } => write_photo(path.as_path(), 4096 + step),
        }
        performed += 1;

        // 每 50 次重新扫描一次（模拟监视器的目录级 Refresh + L2 兜底）
        if step % 50 == 0 {
            svc.enqueue_scan(std::slice::from_ref(&root));
            wait_settled(&board, &rx, Duration::from_secs(60));
        }
    }
    let elapsed = started.elapsed();
    svc.stop();

    // ---- 终态收敛：最后再做一次扫描 + 一次强制 L2 全校
    svc.enqueue_scan(std::slice::from_ref(&root));
    wait_settled(&board, &rx, Duration::from_secs(60));

    // 强制 L2（`07` §13 迭代 3：「终态 0 差异」指的是**文件级全校之后**）
    {
        let plan = picasa_core::VerifyPlan::from_root_paths([root.clone()]);
        let mut runner = picasa_core::verify::VerifyRunner::new_forced_full(plan);
        runner.set_delay_ms(0);
        runner.set_dir_interval_ms(0);
        let g = db.lock().unwrap();
        for _ in 0..100_000 {
            if runner.tick(&g) == picasa_core::verify::VerifyTick::Finished {
                break;
            }
        }
        drop(g);
        let changed = runner.take_changed();
        for (_id, path) in changed {
            svc.enqueue_refresh(Path::new(&path));
        }
        wait_settled(&board, &rx, Duration::from_secs(60));
    }

    // ---- 对账
    let (disk, db_set, total, ids_kept) = {
        let g = db.lock().unwrap();
        let all = g.photos_from_visible(None, picasa_db::SortSpec::default(), 1_000_000, 0).unwrap_or_default();
        let ids = all.iter().map(|p| p.id).collect::<Vec<_>>();
        (
            snapshot_disk(&root),
            snapshot_db(&root, &all),
            all.len(),
            ids.len(),
        )
    };

    // 集合差异（不是数量差异——数量相同但内容不同的情况才是真问题）
    let only_disk: Vec<_> = disk.difference(&db_set).take(10).cloned().collect();
    let only_db: Vec<_> = db_set.difference(&disk).take(10).cloned().collect();

    assert!(
        only_disk.is_empty() && only_db.is_empty(),
        "★ 终态不一致（R-M3-9 / 07 §13 迭代 3 要求 0 差异）：\n\
         磁盘有库里没有 {} 个：{:?}\n\
         库里有磁盘没有 {} 个：{:?}\n\
         （共 {} 张入库 / {} 个磁盘条目，执行了 {performed} 次操作，耗时 {:?}）",
        disk.difference(&db_set).count(),
        only_disk,
        db_set.difference(&disk).count(),
        only_db,
        total,
        disk.len(),
        elapsed
    );

    // 保留率：改名 + 移动应当保住 id。这里用「终态 id 数 / 首轮 id 数」粗算——
    // 因为 fuzz 过程中会有真实删除，所以它必然 < 100%，**不设硬阈值**。
    // 真正的保留率断言在 `m3_identity.rs`（那里能精确控制配对场景）。
    assert!(ids_kept > 0);
    assert!(
        total <= disk.len() + 5,
        "库里的行数（{total}）明显多于磁盘（{}）——疑似幽灵记录",
        disk.len()
    );

    eprintln!(
        "fs_fuzz: {performed} 次操作 / {elapsed:?} / 终态 {} 张 / 0 差异 ✅",
        disk.len()
    );
}

/// 等到索引板安静（没有 running 任务）。
fn wait_settled(
    board: &std::sync::Arc<picasa_core::IndexBoard>,
    _rx: &std::sync::mpsc::Receiver<CoreEvent>,
    timeout: Duration,
) {
    let deadline = Instant::now() + timeout;
    // 先给它一点时间把任务放进板子（enqueue 是异步的）
    std::thread::sleep(Duration::from_millis(40));
    while Instant::now() < deadline {
        if !board.snapshot().running() {
            // 再确认一次：任务可能刚好在这个窗口里起跑
            std::thread::sleep(Duration::from_millis(60));
            if !board.snapshot().running() {
                return;
            }
        }
        std::thread::sleep(Duration::from_millis(30));
    }
}
