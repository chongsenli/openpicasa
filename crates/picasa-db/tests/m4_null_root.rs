//! 回归：`root_stats()` 在「有 root_id IS NULL 的 folder 行」时不能崩。
//!
//! 现场：全新空库启动时 `folders` 可能残留一行未归属根目录的记录（root_id 为 NULL）。
//! 旧 SQL `SELECT root_id, ... FROM folders GROUP BY root_id` 会返回 NULL 列，
//! `r.get::<i64>(0)` 直接报 `Invalid column type Null at index: 0, name: root_id`，
//! 导致「索引管理快照刷新失败」每秒刷屏。该行不属于任何根，跳过即可。

use picasa_db::Db;

#[test]
fn root_stats_skips_folders_without_a_root() {
    let tmp = tempfile::tempdir().unwrap();
    let db = Db::open(tmp.path().join("lib.sqlite")).unwrap();
    // 一个正常根 + 一个未归属根的文件夹行（模拟空库启动时的残留）
    let rid = db.upsert_root("/photos", 0, false).unwrap();
    let fid = db.upsert_folder("/photos", "/photos", None).unwrap();
    db.set_folder_root(fid, rid, None).unwrap();
    let orphan = db.upsert_folder("/orphan", "/orphan", None).unwrap();
    // 故意不调 set_folder_root：这一行的 root_id 保持 NULL（模拟空库启动残留）
    let _ = orphan;

    // 关键断言：不再返回 Err（旧代码在这里报 Invalid column type Null at index: 0）
    let stats = db.root_stats().unwrap();
    assert_eq!(stats.len(), 1, "只应返回有归属根的那一行");
    assert_eq!(stats[0].path, "/photos");
    // 带失败项的查询路径同样不能崩
    let _ = db.failure_groups().unwrap();
}
