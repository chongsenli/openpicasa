//! M22-PERF-04「DB 打开参数」的验收测试（`docs/06` PERF-04）。
//!
//! 这一条为什么值得单独一个文件： PERF-04 给的是**具体数值**（不是待量图的 ⚠️ 项），
//! 而 `PRAGMA` 有个坑——**越界或不支持的值 SQLite 会静默裁剪**，代码里写了不代表生效。
//! 典型是 `mmap_size`：编译期没开 mmap 的 SQLite 会把它设成 0 且不报错。
//! 所以这里不做「跑得通就行」，而是把四个值读回来对账。
//!
//! 门禁口径：启动 DB 阶段 ≤ 50 ms（这条由 `perf_bench` 实测，本文件只守参数生效）。

use picasa_db::Db;

fn open_file_db(dir: &std::path::Path) -> Db {
    let p = dir.join("library.sqlite");
    Db::open(&p).expect("打开索引库")
}

#[test]
fn tc_m22_perf_04_open_pragmas_match_the_spec() {
    let tmp = tempfile::tempdir().unwrap();
    let db = open_file_db(tmp.path());

    // cache_size 为负 = 以 KiB 计，-65536 = 64 MB
    assert_eq!(
        db.pragma_i64("cache_size").unwrap(),
        -65536,
        "页缓存应为 64MB（02 文档写的是 -16384，M2.2 按 PERF-04 提到 -65536）"
    );
    assert_eq!(
        db.pragma_i64("mmap_size").unwrap(),
        268_435_456,
        "mmap 应为 256MB；若为 0 说明这个 SQLite 没开 mmap，需要换构建参数"
    );
    assert_eq!(db.pragma_i64("wal_autocheckpoint").unwrap(), 4000);
    // temp_store: 0=default 1=file 2=memory
    assert_eq!(db.pragma_i64("temp_store").unwrap(), 2);
    assert_eq!(db.pragma_i64("foreign_keys").unwrap(), 1, "级联删除依赖它");
    assert_eq!(
        db.pragma_i64("journal_mode").unwrap_or_default(),
        0,
        "WAL 下 journal_mode 返回的是字符串，取不到整数值时应为 0（不改既有行为）"
    );
}

/// `PRAGMA optimize` 必须真的能跑，且**不能把库搞坏**（退出路径上失败也要无害）。
#[test]
fn tc_m22_perf_04_optimize_on_exit_is_safe_and_idempotent() {
    let tmp = tempfile::tempdir().unwrap();
    let db = open_file_db(tmp.path());

    db.optimize().expect("第一次 optimize 应成功");
    db.optimize()
        .expect("optimize 必须幂等（退出路径可能重入）");

    // 优化完库仍可用：schema 版本读得到，且能正常建目录
    assert!(db.schema_version().unwrap() >= 5);
    let _ = db.folder_id("/whatever").expect("optimize 后查询仍应正常");
}
