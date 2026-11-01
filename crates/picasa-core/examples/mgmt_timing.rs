//! 临时计时程序（用完即删）：在**真实库**上量一次「点开索引管理页」要多久。
//!
//! 用法：
//! ```sh
//! cargo run --example mgmt_timing -p picasa-core -- "$HOME/Library/Application Support/app.openpicasa/library.sqlite"
//! ```
//! 环境变量 `COLD=1` 时先 `echo 3 > /proc/sys/vm/drop_caches`（需 root，失败就跳过）。

use std::time::Instant;

fn main() {
    let path = std::env::args().nth(1).expect("给一个 library.sqlite 路径");
    let db = picasa_db::Db::open(&path).expect("open");

    // ---- 轻量快照：点开管理页那一帧同步跑的部分
    let roots_probe = picasa_core::default_offline_probe();
    let root_ids: Vec<i64> = {
        let t = Instant::now();
        let r = db.root_stats().unwrap();
        println!("root_stats               {:>8.3}s", t.elapsed().as_secs_f64());
        r.iter().map(|x| x.id).collect()
    };
    let live = Vec::new();
    for i in 0..3 {
        let t = Instant::now();
        let input = picasa_core::SnapshotInput {
            seq: i,
            live: &live,
            paused_by_system: false,
            offline: &roots_probe,
            prev_dataset_version: 0,
            prev_photo_count: 0,
        };
        let snap = picasa_core::build_snapshot(&db, &input).unwrap();
        println!(
            "build_snapshot #{:<2}      {:>8.3}s  (roots={}, photo_count={})",
            i,
            t.elapsed().as_secs_f64(),
            snap.roots.len(),
            snap.storage.photo_count
        );
    }

    // ---- 慢的那部分：只应由后台线程跑
    let t = Instant::now();
    let st = picasa_core::collect_storage(&db, &root_ids).unwrap();
    println!(
        "collect_storage(后台)     {:>8.3}s  (thumb_count={}, bytes={})",
        t.elapsed().as_secs_f64(),
        st.thumb_count,
        st.thumb_bytes
    );
}
