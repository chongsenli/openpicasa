//! M4.3 阶段 0 · v8 → v9 迁移验收（`docs/18` §3.1 / §6 阶段 0 / TC-M43-MIG-01）。
//!
//! ## 为什么每条都要「造一个真正的 v8 库」
//!
//! 用 `MIGRATIONS.iter().take(8)` 建库，而不是「建一个 v9 库再把列删掉」：
//! 后者验证的是我删得对不对，前者验证的才是**用户的 v8 库**能不能升上来。
//! 与 `migrate_v6.rs` / `migrate_v5.rs` 同一手法。
//!
//! ## 覆盖什么
//!
//! TC-M43-MIG-01：v8 副本升 v9、失败可回滚、重复启动幂等、整理数据守恒、
//! `foreign_key_check` 无错。本文件覆盖「升级成功 / 数据守恒 / 外键无错 / 幂等 /
//! 备份可降级 / 新列新表都在 / 端到端标记守恒」。「失败回滚」由 `upgrade.rs` 的
//! 单事务 + 备份覆盖机制保证（与本迁移无关的既有能力）。

use picasa_db::{Db, MIGRATIONS, SCHEMA_VERSION, SortSpec};
use std::path::{Path, PathBuf};

/// 造一个 **v8** 库：建到 v8 为止，塞进两层目录 + 3 张照片 + 用户标记
/// （星标 / 标签 / 说明 / 相册），供升级前后对账。
///
/// 目录用绝对路径（`/photos`、`/photos/2020`），与 `folders.path` 真实口径一致；
/// `folders.root_id` 故意留空（真实库里 `upsert_folder` 也不写它），验证 v9 不依赖它。
fn make_v8_db(path: &Path) {
    let conn = rusqlite::Connection::open(path).unwrap();
    conn.execute_batch(
        "CREATE TABLE schema_migrations (
             version INTEGER PRIMARY KEY, applied_at INTEGER NOT NULL);",
    )
    .unwrap();
    for m in MIGRATIONS.iter().take(8) {
        conn.execute_batch(m.sql).unwrap();
        conn.execute(
            "INSERT INTO schema_migrations VALUES (?1, 0)",
            rusqlite::params![m.version],
        )
        .unwrap();
    }
    conn.execute_batch(
        r#"
        INSERT INTO roots (id, path, path_key, state) VALUES
            (1, '/photos', '/photos', 0);
        INSERT INTO folders
            (id, path, path_key, is_online, root_id, parent_id, name, direct_count) VALUES
            (1, '/photos',      '/photos',      1, NULL, NULL, 'photos', 1),
            (2, '/photos/2020', '/photos/2020', 1, NULL, 1,    '2020',   2);
        -- V1 photos 的 NOT NULL 无默认列必须给全；其余列有默认值。
        INSERT INTO photos
            (id, folder_id, name, name_key, size_bytes, mtime_ns, width, height,
             orientation, taken_at, taken_at_src, thumb_state, decoder_ver) VALUES
            (1, 1, 'cover.JPG', 'cover.jpg', 10, 1, 4000, 3000, 1, 100, 0, 1, 0),
            (2, 2, 'a.png',     'a.png',     20, 2, 800,  600,  1, 200, 0, 1, 0),
            (3, 2, 'b.TIFF',    'b.tiff',    30, 3, 800,  600,  1, 300, 2, 1, 0);
        INSERT INTO thumbs (photo_id, tier, w, h, format, data) VALUES
            (1, 0, 8, 8, 0, X'00'),
            (2, 0, 8, 8, 0, X'00');
        -- 用户创作数据（v7/v8 表）：升级必须一个字节都不动。
        UPDATE photos SET starred = 1 WHERE id = 1;
        INSERT INTO captions (photo_id, text) VALUES (1, '封面');
        INSERT INTO tags (photo_id, label) VALUES (1, '旅行'), (1, '精选');
        INSERT INTO albums (id, name, created_at) VALUES (7, '年度最佳', 1);
        INSERT INTO album_photos (album_id, photo_id, added_at) VALUES (7, 1, 1);
        INSERT INTO recent_searches (id, query_json, created_at) VALUES (1, '{}', 1);
        "#,
    )
    .unwrap();
}

fn open_fixture(dir: &Path) -> (Db, PathBuf) {
    let db_path = dir.join("library.sqlite");
    make_v8_db(&db_path);
    let (db, _report) = Db::open_with_report(&db_path).unwrap();
    (db, db_path)
}

fn col_exists(db: &Db, table: &str, col: &str) -> bool {
    db.column_names(table)
        .map(|cols| cols.iter().any(|c| c == col))
        .unwrap_or(false)
}

fn table_exists(db: &Db, name: &str) -> bool {
    db.table_exists(name).unwrap_or(false)
}

/// 直接 SQL 读库里的标记（不经过 Db 方法，避免「方法自己补数据」掩盖守恒问题）。
fn scalar(conn: &rusqlite::Connection, sql: &str) -> i64 {
    conn.query_row(sql, [], |r| r.get(0)).unwrap()
}

#[test]
fn v8_library_upgrades_to_v9_with_rows_intact() {
    let tmp = tempfile::tempdir().unwrap();
    let (db, db_path) = open_fixture(tmp.path());

    // 版本钉子：v9 接在 v8 后面
    assert_eq!(SCHEMA_VERSION, 9);
    assert_eq!(MIGRATIONS.last().unwrap().version, 9);
    assert_eq!(db.schema_version().unwrap(), SCHEMA_VERSION);

    // 行数一条都不能少（只增列/表，不动数据）
    assert_eq!(db.total_photos().unwrap(), 3);
    assert_eq!(db.all_folders().unwrap().len(), 2);
    assert_eq!(db.roots(true).unwrap().len(), 1);

    let report = db.last_upgrade_note().expect("升级现场应被记录");
    assert_eq!(report.from, 8);
    assert_eq!(report.to, SCHEMA_VERSION);
    assert!(report.error.is_none(), "迁移不应失败");
    assert!(!report.restored_from_backup, "成功路径不应触发备份恢复");

    let backup = report.backup.clone().expect("升级前必须先备份");
    assert!(backup.exists());
    assert!(
        backup.starts_with(Db::backup_dir(&db_path)),
        "备份必须落在应用容器，绝不能落在照片目录（R-M43-01）"
    );
    // 备份是**自洽的 v8 库**：出了事要用它覆盖回去降级
    let b = rusqlite::Connection::open(&backup).unwrap();
    let v: i64 = b
        .query_row("SELECT MAX(version) FROM schema_migrations", [], |r| {
            r.get(0)
        })
        .unwrap();
    assert_eq!(v, 8, "备份必须是升级前的 v8 版本");
    assert_eq!(scalar(&b, "SELECT COUNT(*) FROM photos"), 3);
    assert_eq!(scalar(&b, "SELECT COUNT(*) FROM captions"), 1);
    assert_eq!(scalar(&b, "SELECT COUNT(*) FROM album_photos"), 1);
}

#[test]
fn v9_adds_identity_checkpoint_and_import_tables() {
    let tmp = tempfile::tempdir().unwrap();
    let (db, _p) = open_fixture(tmp.path());

    // 1. 身份骨架（R-M43-02 的地基）
    for t in [
        "photo_identities",
        "photo_identity_tags",
        "photo_identity_albums",
    ] {
        assert!(table_exists(&db, t), "缺表 {t}");
    }
    // 2. 任务持久意图/检查点（阶段 1 地基，本阶段只建列）
    for (table, col) in [
        ("index_tasks", "user_paused"),
        ("index_tasks", "generation"),
        ("index_tasks", "checkpoint"),
        ("index_tasks", "scope_kind"),
        ("index_tasks", "cancel_reason"),
    ] {
        assert!(col_exists(&db, table, col), "缺列 {table}.{col}");
    }
    // 3. 导入来源分组地基（3C 用，本阶段空表）
    for col in ["origin", "import_source_id"] {
        assert!(col_exists(&db, "roots", col), "缺列 roots.{col}");
    }
    for t in ["import_sources", "import_batches", "import_batch_members"] {
        assert!(table_exists(&db, t), "缺表 {t}");
    }

    // v8 的列/表不能因为新增 v9 被弄丢
    for col in [
        "camera_make",
        "camera_model",
        "exif_status",
        "starred",
        "format",
        "taken_from",
    ] {
        assert!(col_exists(&db, "photos", col), "v8/v7 的 photos.{col} 丢了");
    }
    for t in ["albums", "album_photos", "captions", "tags", "recent_searches", "index_manifest"] {
        assert!(table_exists(&db, t), "v7/v8 的 {t} 表丢了");
    }
}

#[test]
fn upgrade_preserves_user_created_data_and_foreign_keys_are_clean() {
    let tmp = tempfile::tempdir().unwrap();
    let (db, _p) = open_fixture(tmp.path());

    // 用户创作数据一条都不能少（v7/v8 的 live 表原样保留）
    assert_eq!(db.tags_of(1).unwrap().len(), 2, "标签守恒");
    assert_eq!(db.caption_of(1).unwrap(), "封面", "说明守恒");
    assert!(db.starred_ids().unwrap().contains(&1), "星标守恒");
    assert!(
        db.albums_of(1).unwrap().iter().any(|a| a.name == "年度最佳"),
        "相册归属守恒"
    );

    // 外键完整性：新表用逻辑外键、老表 CASCADE 不变，升级后不能留悬空引用。
    // 直接开一个只读短连接跑 PRAGMA（Db 没暴露 conn，但同一文件可读）。
    let fk = {
        let c = rusqlite::Connection::open(tmp.path().join("library.sqlite")).unwrap();
        let mut st = c.prepare("PRAGMA foreign_key_check").unwrap();
        let rows: Vec<String> = st
            .query_map([], |r| r.get::<_, String>(0))
            .unwrap()
            .collect::<std::result::Result<Vec<_>, _>>()
            .unwrap();
        rows
    };
    assert!(fk.is_empty(), "foreign_key_check 必须无错，实际：{fk:?}");
}

#[test]
fn migration_is_idempotent_and_fresh_matches_upgraded_shape() {
    let tmp = tempfile::tempdir().unwrap();
    let (db, db_path) = open_fixture(tmp.path());

    // 第二次打开：DDL 不重跑、不重复备份
    let (db2, report2) = Db::open_with_report(&db_path).unwrap();
    assert_eq!(db2.schema_version().unwrap(), SCHEMA_VERSION);
    assert_eq!(report2.from, SCHEMA_VERSION);
    assert!(report2.backup.is_none(), "已是最新就不该再备份一次");
    assert_eq!(db2.total_photos().unwrap(), 3);

    // 新建库（从零 migrate 到 v9）与「v8 升上来」的库列结构必须一致，
    // 不许两条路径分叉（migrate_v6.rs 的同类钉子）。
    let fresh = Db::open(tmp.path().join("fresh.sqlite")).unwrap();
    for table in [
        "photos",
        "folders",
        "roots",
        "index_tasks",
        "photo_identities",
        "import_batches",
    ] {
        let a = fresh.column_names(table).unwrap();
        let b = db.column_names(table).unwrap();
        assert_eq!(a, b, "{table} 的列结构在两条路径上必须一致");
    }
    assert_eq!(fresh.schema_version().unwrap(), SCHEMA_VERSION);
}

/// 端到端：v8 升 v9 之后，清索引再重扫，用户标记按身份挂回新的 `photo_id`。
/// 这是 R-M43-02 的真正验收点（TC-M43-MIG-01「整理数据守恒」）。
#[test]
fn upgraded_v9_keeps_markers_across_clear_and_rescan() {
    let tmp = tempfile::tempdir().unwrap();
    let (db, _p) = open_fixture(tmp.path());

    // 清索引：v9 的 clear_index 会先把 live 标记快照进骨架表
    let (photos, thumbs) = db.clear_index().unwrap();
    assert_eq!((photos, thumbs), (3, 2));
    assert_eq!(db.total_photos().unwrap(), 0);

    // 先插一张无关照片占住 rowid 1（清表后 SQLite 会复用 1）——
    // 这样 cover 重扫后必然拿到不同的 id，才能真正区分「按身份恢复」与「按旧 id 恢复」。
    let other = "/other".to_string();
    let fid_other = db.upsert_folder(&other, &other, None).unwrap();
    db.apply_scan(fid_other, &other, &[dummy_input()], 0).unwrap();

    // 重新扫描：INSERT 后 restore_photo 按 (folder_path, name_key) 挂回
    let key = "/photos".to_string();
    let fid = db.upsert_folder(&key, &key, None).unwrap();
    db.apply_scan(fid, &key, &[cover_input()], 0).unwrap();

    let rows = db.photos(None, SortSpec::default(), 10).unwrap();
    let new_id = rows
        .iter()
        .find(|r| r.name == "cover.JPG")
        .expect("cover.JPG 已重新扫描")
        .id;
    assert_ne!(new_id, 1, "重建后 photo_id 已变化，标记必须按身份而非旧 id 恢复");
    assert!(db.starred_ids().unwrap().contains(&new_id), "星标按身份恢复");
    assert_eq!(db.caption_of(new_id).unwrap(), "封面", "说明按身份恢复");
    let tags = db.tags_of(new_id).unwrap();
    assert!(tags.iter().any(|t| t == "旅行") && tags.iter().any(|t| t == "精选"), "标签按身份恢复");
    assert!(
        db.albums_of(new_id).unwrap().iter().any(|a| a.name == "年度最佳"),
        "相册按名字恢复"
    );
}

fn cover_input() -> picasa_db::PhotoInput {
    picasa_db::PhotoInput {
        name: "cover.JPG".into(),
        name_key: "cover.jpg".into(),
        size_bytes: 10,
        mtime_ns: 1,
        width: Some(4000),
        height: Some(3000),
        orientation: Some(1),
        taken_at: Some(100),
        taken_at_src: 0,
        format: 0,
    }
}

fn dummy_input() -> picasa_db::PhotoInput {
    picasa_db::PhotoInput {
        name: "dummy.png".into(),
        name_key: "dummy.png".into(),
        size_bytes: 1,
        mtime_ns: 1,
        width: Some(1),
        height: Some(1),
        orientation: Some(1),
        taken_at: Some(1),
        taken_at_src: 0,
        format: 0,
    }
}
