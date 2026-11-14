//! M4.3 阶段 4 · `TC-M43-12` / `TC-M43-18` 的性能标尺（25 万张规模）。
//!
//! 默认 `#[ignore]`：跑一次要建 25 万行，普通回归不该背这个时间。需要出数时：
//!
//! ```text
//! cargo test -p picasa-db --test m43_perf25w -- --ignored --nocapture --test-threads=1
//! ```
//!
//! 断言的是**查询侧**的 p99：左栏标签计数（IDX-22 ⑧，UI 只读缓存、后台算）与
//! 网格分页（栅格滚动）。两者都必须 ≤ 25 ms，否则「25 万张不卡帧」只是说法。
//!
//! 这里只读，不写照片目录。

use picasa_db::{Db, SortSpec};
use std::path::Path;
use std::time::Instant;

const PHOTOS: i64 = 250_000;
const FOLDERS: i64 = 500;
const TAGS: i64 = 20;

/// 用一条事务灌 25 万张：直接 SQL 造数据（等价于扫描完的落地状态）。
/// 先 `Db::open` 让迁移跑完，再关掉、用裸连接批量插入，最后重新 `Db::open`。
fn seed_250k(path: &Path) {
    {
        let db = Db::open(path).unwrap();
        drop(db);
    }
    let conn = rusqlite::Connection::open(path).unwrap();
    conn.pragma_update(None, "synchronous", "OFF").unwrap();
    let tx = conn.unchecked_transaction().unwrap();
    tx.execute(
        "INSERT INTO roots (id, path, path_key, state) VALUES (1, '/photos', '/photos', 0)",
        [],
    )
    .unwrap();
    for f in 0..FOLDERS {
        let fid = f + 1;
        let p = if f == 0 {
            "/photos".to_string()
        } else {
            format!("/photos/d{:03}", f)
        };
        tx.execute(
            "INSERT INTO folders (id, path, path_key, is_online, root_id, parent_id, name,
                                  direct_count, has_children)
             VALUES (?1, ?2, ?3, 1, 1, NULL, ?4, 0, 0)",
            rusqlite::params![fid, p, p, format!("d{f:03}")],
        )
        .unwrap();
    }
    {
        let mut st = tx
            .prepare(
                "INSERT INTO photos (id, folder_id, name, name_key, size_bytes, mtime_ns,
                                     width, height, orientation, taken_at, taken_at_src,
                                     thumb_state, decoder_ver)
                 VALUES (?1, ?2, ?3, ?4, 1000, ?1, 6000, 4000, 1, ?1, 0, 1, 0)",
            )
            .unwrap();
        for i in 1..=PHOTOS {
            let folder = (i % FOLDERS) + 1;
            let name = format!("img{i:06}.jpg");
            st.execute(rusqlite::params![i, folder, name, name.to_lowercase()])
                .unwrap();
        }
    }
    // 每个标签打在一部分照片上（真实库的标签是长尾，不是全量）。
    {
        let mut st = tx
            .prepare("INSERT OR IGNORE INTO tags (photo_id, label) VALUES (?1, ?2)")
            .unwrap();
        for t in 0..TAGS {
            let step = 7 + t;
            let mut i = t + 1;
            while i <= PHOTOS {
                st.execute(rusqlite::params![i, format!("标签{}", t + 1)])
                    .unwrap();
                i += step;
            }
        }
    }
    tx.execute(
        "UPDATE folders SET direct_count = (SELECT COUNT(*) FROM photos p
                                            WHERE p.folder_id = folders.id)",
        [],
    )
    .unwrap();
    tx.commit().unwrap();
    let _ = conn.execute_batch("PRAGMA optimize;");
}

fn p99(mut samples: Vec<f64>) -> f64 {
    samples.sort_by(|a, b| a.partial_cmp(b).unwrap());
    let idx = ((samples.len() as f64 * 0.99).ceil() as usize).clamp(1, samples.len()) - 1;
    samples[idx]
}

#[test]
#[ignore = "25 万行建库较慢；出性能数据时手动跑"]
fn quarter_million_photo_library_keeps_p99_under_25ms() {
    let path = std::path::PathBuf::from(
        std::env::var("PERF25K_DB").unwrap_or_else(|_| "/tmp/perf25k/library.sqlite".into()),
    );
    if !path.exists() {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        let t0 = Instant::now();
        seed_250k(&path);
        eprintln!("建库 25 万张：{:.1}s", t0.elapsed().as_secs_f64());
    }

    let db = Db::open(&path).unwrap();
    assert_eq!(db.total_photos().unwrap(), PHOTOS as u64);

    // 1) 左栏标签计数（IDX-22 ⑧）：保持单条聚合、不含 N+1。
    //
    // 先预热一次再计时：帧预算约束的是**稳态**（每次刷新都付的钱），
    // 而调用方本来就是「后台线程算、UI 只读缓存」——冷启动那一次由后台线程吸收。
    // 冷调用单独打印，不藏在 p99 里。
    let cold = {
        let t = Instant::now();
        let counts = db.tag_counts_visible(None).unwrap();
        assert_eq!(counts.len(), TAGS as usize);
        t.elapsed().as_secs_f64() * 1000.0
    };
    let mut tag_samples = Vec::new();
    for _ in 0..100 {
        let t = Instant::now();
        let counts = db.tag_counts_visible(None).unwrap();
        tag_samples.push(t.elapsed().as_secs_f64() * 1000.0);
        assert_eq!(counts.len(), TAGS as usize);
    }
    let tag_p99 = p99(tag_samples.clone());

    // 2) 栅格分页（滚动时的主查询）：首页用 OFFSET 0，其余各页用键集游标
    //    ——这正是 UI 的真实路径（`Library::extend_photos` 传上一页末行的
    //    `(eff_taken_at, name_key)`）。测 OFFSET 深页等于测一条已经没人走的路径。
    let mut page_samples = Vec::new();
    for _ in 0..10 {
        let t = Instant::now();
        let first = db
            .photos_from_visible(None, SortSpec::default(), 100, 0)
            .unwrap();
        page_samples.push(t.elapsed().as_secs_f64() * 1000.0);
        assert_eq!(first.len(), 100);

        // 逐页往下翻到 12 万张的位置，每页都重新计时。
        let mut cursor = None;
        let mut seen = 0usize;
        let mut row = first;
        while seen < 120_000 {
            let last = row.last().unwrap();
            cursor = db.page_cursor(last.id).unwrap();
            let t = Instant::now();
            row = db
                .photos_from_visible_after(None, SortSpec::default(), 4_000, cursor.clone())
                .unwrap();
            page_samples.push(t.elapsed().as_secs_f64() * 1000.0);
            assert_eq!(row.len(), 4_000, "键集页必须取满");
            seen += row.len();
        }
        // 末页也测一次（游标在 12 万处，一次跳到最后）。
        let tail = db
            .photos_from_visible_after(None, SortSpec::default(), 100, cursor.clone())
            .unwrap();
        assert_eq!(tail.len(), 100);
    }
    let page_p99 = p99(page_samples.clone());

    // 3) 视图范围内的标签计数（P1「本视图 N」）——单个文件夹子树。
    let fid = db.folder_id("/photos/d001").unwrap().unwrap();
    let t = Instant::now();
    let scoped = db.tag_counts_visible(Some(&[fid])).unwrap();
    let scoped_ms = t.elapsed().as_secs_f64() * 1000.0;
    assert!(!scoped.is_empty());

    eprintln!("tag_counts_visible 首次（冷）= {cold:.2} ms");
    eprintln!("tag_counts_visible p99 = {tag_p99:.2} ms");
    eprintln!("photos_from_visible(100) p99 = {page_p99:.2} ms");
    eprintln!("tag_counts_visible(scoped) = {scoped_ms:.2} ms");

    assert!(
        tag_p99 <= 25.0,
        "25 万张时左栏标签计数 p99 必须 ≤ 25 ms，实测 {tag_p99:.2} ms"
    );
    assert!(
        page_p99 <= 25.0,
        "25 万张时网格分页 p99 必须 ≤ 25 ms，实测 {page_p99:.2} ms"
    );
}
