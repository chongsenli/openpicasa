//! IDX-22 ⑧：标签计数在**后台线程**算，UI 帧只读缓存。
//!
//! ## 为什么不能留在 UI 帧里
//!
//! `all_tags()` / `tag_counts_visible()` 是全库聚合：25 万张 / 35 万行 tag 实测
//! 冷调用 27 ms、稳态 15 ms。稳态数字已经进了 25 ms 预算，但仍然**不该**占着
//! UI 线程——它挡的是"这一帧能不能按时交出"，而帧预算只有 16 ms。
//! `docs/18` IDX-22 ⑧ 的原话是「计数来自单条聚合查询或计数缓存、在后台线程算，
//! UI 帧只读缓存」，这里就按这句实现。
//!
//! ## 线程模型
//!
//! 与 `import_task` 同一套：`Db` 不是 `Send`，后台线程用 `AppPaths::db_file()`
//! **自己开一条连接**；主线程只经 `mpsc::Receiver` 非阻塞地取结果。
//! 一次只允许一个在算（`busy` 标志），算完把 `revision` 回传，UI 侧用它丢弃
//! 乱序到达的旧结果。
//!
//! 这里**只读**索引库，绝不碰照片目录（R-M43-01）。

use picasa_db::Db;
use std::path::PathBuf;
use std::sync::mpsc::{self, Receiver, TryRecvError};

/// 一次计算的结果。
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct TagCounts {
    /// 全库口径（左栏主数字）。
    pub all: Vec<(String, i64)>,
    /// 视图口径（P1「本视图 N」）。`view_is_all` 时为 `None`。
    pub in_view: Option<Vec<(String, i64)>>,
}

/// 后台算标签计数的服务。
pub struct TagCountsService {
    db_path: PathBuf,
    rx: Receiver<(u64, TagCounts)>,
    tx: mpsc::Sender<(u64, TagCounts)>,
    /// 有任务在跑时不再派新任务（计数只需最新一次，排队没有意义）。
    busy: bool,
    /// 单调递增；UI 侧用它丢弃「算得慢、回来晚」的旧结果。
    revision: u64,
    /// 忙的时候来的请求：只留**最后一个**，当前任务一结束就接着派。
    ///
    /// 不这样做的话，「在算的时候切了视图」这次请求会被静默丢掉——
    /// 左栏会一直显示上一个视图的「本视图 N」，直到用户再动点别的。
    pending: Option<Option<Vec<i64>>>,
}

impl TagCountsService {
    pub fn new(db_path: PathBuf) -> Self {
        let (tx, rx) = mpsc::channel();
        Self {
            db_path,
            rx,
            tx,
            busy: false,
            revision: 0,
            pending: None,
        }
    }

    pub fn is_busy(&self) -> bool {
        self.busy
    }

    /// 派一次计算。`view_folder_ids` 为 `None` 表示当前就是全库视图
    /// （此时不算「本视图 N」，因为两个数字恒等，画了是噪音）。
    pub fn request(&mut self, view_folder_ids: Option<Vec<i64>>) -> u64 {
        if self.busy {
            // 上一次还没算完：记下这次的意图，等它回来立刻接着派。
            self.pending = Some(view_folder_ids);
            return self.revision;
        }
        self.spawn(view_folder_ids)
    }

    /// 派一次真正的工作线程，返回这份结果的 revision。
    fn spawn(&mut self, view_folder_ids: Option<Vec<i64>>) -> u64 {
        self.revision += 1;
        let revision = self.revision;
        self.busy = true;
        let db_path = self.db_path.clone();
        let tx = self.tx.clone();
        std::thread::spawn(move || {
            let counts = compute(&db_path, view_folder_ids.as_deref());
            // 接收端已丢弃（窗口关了）时 send 失败，静默忽略即可。
            let _ = tx.send((revision, counts));
        });
        revision
    }

    /// 非阻塞取结果。返回 `Some((revision, counts))` 表示有新数据。
    pub fn poll(&mut self) -> Option<(u64, TagCounts)> {
        match self.rx.try_recv() {
            Ok((rev, counts)) => {
                self.busy = false;
                // 忙的时候攒下的请求，现在接着派——否则那次视图切换会永远丢掉。
                if let Some(next) = self.pending.take() {
                    self.spawn(next);
                }
                Some((rev, counts))
            }
            Err(TryRecvError::Empty) => None,
            Err(TryRecvError::Disconnected) => {
                self.busy = false;
                None
            }
        }
    }
}

/// 真正干活的地方：开一条只读连接，算两种口径。
///
/// 失败一律返回空——标签区显示为空比整个侧栏报错好，而且下一次脏标记会重试。
fn compute(db_path: &std::path::Path, view_folder_ids: Option<&[i64]>) -> TagCounts {
    let Ok(db) = Db::open(db_path) else {
        return TagCounts::default();
    };
    let all = db.tag_counts_visible(None).unwrap_or_default();
    let in_view = view_folder_ids.and_then(|ids| db.tag_counts_visible(Some(ids)).ok());
    TagCounts { all, in_view }
}

#[cfg(test)]
mod tests {
    use super::*;
    use picasa_db::PhotoInput;
    use std::time::{Duration, Instant};

    fn photo(name: &str, taken: i64) -> PhotoInput {
        PhotoInput {
            name: name.to_string(),
            name_key: name.to_lowercase(),
            size_bytes: 1000,
            mtime_ns: taken * 1_000_000_000,
            width: Some(800),
            height: Some(600),
            orientation: Some(1),
            taken_at: Some(taken),
            taken_at_src: 1,
            format: 1,
        }
    }

    fn seeded(path: &std::path::Path) {
        let db = Db::open(path).unwrap();
        let root = db.upsert_root("/albums", 0, false).unwrap();
        let fid = db.upsert_folder("/albums", "/albums", None).unwrap();
        db.set_folder_root(fid, root, None).unwrap();
        db.apply_scan(
            fid,
            "/albums",
            &[photo("a.jpg", 1), photo("b.jpg", 2), photo("c.jpg", 3)],
            0,
        )
        .unwrap();
        let ids: Vec<i64> = db
            .photos(None, picasa_db::SortSpec::default(), 10)
            .unwrap()
            .iter()
            .map(|p| p.id)
            .collect();
        db.add_tag(&ids[..2], "旅行").unwrap();
        db.add_tag(&ids[2..], "京都").unwrap();
    }

    fn wait_for(service: &mut TagCountsService) -> (u64, TagCounts) {
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            if let Some(got) = service.poll() {
                return got;
            }
            assert!(Instant::now() < deadline, "后台标签计数超时未返回");
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    #[test]
    fn background_count_computes_both_scopes() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("library.sqlite");
        seeded(&path);

        let mut service = TagCountsService::new(path);
        assert!(!service.is_busy());
        service.request(Some(vec![1]));
        assert!(service.is_busy(), "派出去之后应处于忙碌态");

        let (rev, counts) = wait_for(&mut service);
        assert_eq!(rev, 1);
        assert!(!service.is_busy(), "取到结果后必须复位忙碌态");
        let all: std::collections::HashMap<String, i64> = counts.all.iter().cloned().collect();
        assert_eq!(all.get("旅行"), Some(&2));
        assert_eq!(all.get("京都"), Some(&1));
        let in_view = counts.in_view.expect("要求了视图口径就必须有");
        assert!(in_view.iter().any(|(label, _)| label == "旅行"));
    }

    #[test]
    fn view_is_all_skips_the_second_scope() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("library.sqlite");
        seeded(&path);

        let mut service = TagCountsService::new(path);
        service.request(None);
        let (_, counts) = wait_for(&mut service);
        assert!(
            counts.in_view.is_none(),
            "全库视图下两个数字恒等，不该再算一遍"
        );
        assert_eq!(counts.all.len(), 2);
    }

    #[test]
    fn a_request_made_while_busy_is_not_lost() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("library.sqlite");
        seeded(&path);
        let mut service = TagCountsService::new(path);

        // 第一次：全库口径。
        let first = service.request(None);
        assert!(service.is_busy());
        // 算的过程中切到某个视图 —— 这次请求必须被记住。
        service.request(Some(vec![1]));

        let (rev1, counts1) = wait_for(&mut service);
        assert_eq!(rev1, first);
        assert!(counts1.in_view.is_none(), "第一次是全库口径");

        // 上一次回来后，攒下的那个应当被自动派出去并终有结果。
        let (rev2, counts2) = wait_for(&mut service);
        assert!(rev2 > first, "接续的那次必须是更新的 revision");
        assert!(
            counts2.in_view.is_some(),
            "忙时攒下的『本视图 N』请求不能被丢掉"
        );
    }

    #[test]
    fn revision_monotonically_increases_so_stale_results_are_droppable() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("library.sqlite");
        seeded(&path);
        let mut service = TagCountsService::new(path);
        let r1 = service.request(None);
        let _ = wait_for(&mut service);
        let r2 = service.request(None);
        assert!(r2 > r1, "revision 必须单调递增（UI 靠它丢弃乱序旧结果）");
    }
}
