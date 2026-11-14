//! M3 数据层迁移 v5 → v6 的验收测试（`07` §8.3 + `07-A` §6）。
//!
//! ## 规格要求覆盖什么
//!
//! `07` §8.3 列了四条：升级成功、行数不变、备份在容器、幂等；
//! `07-A` §6 又加四条：`format` 预填正确率、`rel_path` 索引存在（本仓库是
//! `idx_folders_root_path`，见 `schema.rs` V6 的偏差表）、设置键存在、幂等。
//!
//! ## 为什么每条都要「造一个真正的 v5 库」
//!
//! 用 `MIGRATIONS.iter().take(5)` 建库，而不是「建一个 v6 库再把列删掉」：
//! 后者验证的是我删得对不对，前者验证的才是**用户的库**能不能升上来。
//! M2.2 的 `migrate_v5.rs` 用同一手法，这里保持一致。

use picasa_db::{Db, MIGRATIONS, PhotoFormat, SCHEMA_VERSION, TakenFrom};
use std::path::{Path, PathBuf};

/// 造一个 **v5** 库：建到 v5 为止，塞进有层级的两层目录 + 4 张照片。
///
/// 目录结构刻意造了一棵两层树（`/photos` → `/photos/2020`），
/// 因为 `total_count` 的递归聚合**只有在有子目录时才可能算错**——
/// 平坦的一层库里「自底向上累加」和「直接取 direct_count」结果相同，测不出问题。
fn make_v5_db(path: &Path) {
    let conn = rusqlite::Connection::open(path).unwrap();
    conn.execute_batch(
        "CREATE TABLE schema_migrations (
             version INTEGER PRIMARY KEY, applied_at INTEGER NOT NULL);",
    )
    .unwrap();
    for m in MIGRATIONS.iter().take(5) {
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
        INSERT INTO folders (id, path, path_key, is_online, root_id, parent_id, name, direct_count) VALUES
            (1, '/photos',        '/photos',        1, 1, NULL, 'photos', 1),
            (2, '/photos/2020',   '/photos/2020',   1, 1, 1,    '2020',   2),
            (3, '/photos/2021',   '/photos/2021',   1, 1, 1,    '2021',   0);
        INSERT INTO photos
            (folder_id, name, name_key, size_bytes, mtime_ns, width, height, orientation,
             taken_at, taken_at_src, thumb_state, decoder_ver) VALUES
            (1, 'cover.JPG',   'cover.jpg',   10, 1, 4000, 3000, 1, 100, 0, 1, 0),
            (2, 'a.png',       'a.png',       20, 2, 800,  600,  1, 200, 0, 1, 0),
            (2, 'b.TIFF',      'b.tiff',      30, 3, 800,  600,  1, 300, 2, 1, 0),
            (2, 'c.unknown',   'c.unknown',   40, 4, 800,  600,  1, 400, 2, 1, 0);
        "#,
    )
    .unwrap();
}

fn open_fixture(dir: &Path) -> (Db, PathBuf) {
    let db_path = dir.join("library.sqlite");
    make_v5_db(&db_path);
    let (db, _report) = Db::open_with_report(&db_path).unwrap();
    (db, db_path)
}

fn col_exists(db: &Db, table: &str, col: &str) -> bool {
    db.column_names(table)
        .map(|cols| cols.iter().any(|c| c == col))
        .unwrap_or(false)
}

#[test]
fn v5_library_upgrades_to_v6_with_rows_intact() {
    let tmp = tempfile::tempdir().unwrap();
    let (db, db_path) = open_fixture(tmp.path());

    assert_eq!(db.schema_version().unwrap(), SCHEMA_VERSION);
    assert_eq!(SCHEMA_VERSION, 9);

    // 行数一条都不能少（§8.3：只增列，不动数据）
    assert_eq!(db.total_photos().unwrap(), 4);
    assert_eq!(db.all_folders().unwrap().len(), 3);

    let report = db.last_upgrade_note().expect("升级现场应被记录");
    assert_eq!(report.from, 5);
    assert_eq!(report.to, SCHEMA_VERSION);
    assert!(report.error.is_none());

    let backup = report.backup.clone().expect("升级前必须先备份");
    assert!(backup.exists());
    assert!(
        backup.starts_with(Db::backup_dir(&db_path)),
        "备份必须落在应用容器，绝不能落在照片目录（R-1）"
    );
    // 备份是**自洽的 v5 库**：降级要用它覆盖
    let b = rusqlite::Connection::open(&backup).unwrap();
    let v: i64 = b
        .query_row("SELECT MAX(version) FROM schema_migrations", [], |r| {
            r.get(0)
        })
        .unwrap();
    assert_eq!(v, 5, "备份必须是升级前的版本");
}

#[test]
fn v6_adds_every_column_the_plan_names() {
    let tmp = tempfile::tempdir().unwrap();
    let (db, _p) = open_fixture(tmp.path());

    // 07 §8.1
    for (table, col) in [
        ("folders", "dir_mtime"),
        ("folders", "total_count"),
        ("roots", "watch_enabled"),
        ("roots", "poll_interval_s"),
        ("photos", "head_hash"),
    ] {
        assert!(col_exists(&db, table, col), "{table}.{col} 缺失");
    }
    // 07-A §6
    for col in ["format", "taken_from"] {
        assert!(col_exists(&db, "photos", col), "photos.{col} 缺失");
    }
}

#[test]
fn format_is_backfilled_from_the_extension() {
    // `07-A` §6：「format 预填正确率（抽样扩展名）」。这里不走抽样——夹具只有 4 行，
    // 全量断言更严格。
    let tmp = tempfile::tempdir().unwrap();
    let (db, _p) = open_fixture(tmp.path());

    let id_of = |name: &str| -> i64 {
        db.photo_by_name(name)
            .unwrap()
            .unwrap_or_else(|| panic!("{name} 应在库里"))
            .id
    };

    assert_eq!(
        PhotoFormat::from_code(db.photo_format(id_of("cover.JPG")).unwrap()),
        PhotoFormat::Jpeg,
        "扩展名大写也要认（.JPG）"
    );
    assert_eq!(
        PhotoFormat::from_code(db.photo_format(id_of("a.png")).unwrap()),
        PhotoFormat::Png
    );
    assert_eq!(
        PhotoFormat::from_code(db.photo_format(id_of("b.TIFF")).unwrap()),
        PhotoFormat::Tiff
    );
    // 认不出的扩展名**保持 Unknown**，不能瞎猜成 JPEG——那会让 M4 的类型筛选说谎。
    assert_eq!(
        PhotoFormat::from_code(db.photo_format(id_of("c.unknown")).unwrap()),
        PhotoFormat::Unknown
    );
}

#[test]
fn taken_from_is_backfilled_from_the_exif_source() {
    let tmp = tempfile::tempdir().unwrap();
    let (db, _p) = open_fixture(tmp.path());

    let taken_from = |name: &str| -> TakenFrom {
        TakenFrom::from_code(
            db.photo_taken_from_by_name(name)
                .unwrap()
                .unwrap_or_else(|| panic!("{name} 应在库里")),
        )
    };

    // taken_at_src = 0 → EXIF
    assert_eq!(taken_from("cover.JPG"), TakenFrom::Exif);
    assert_eq!(taken_from("a.png"), TakenFrom::Exif);
    // taken_at_src = 2 → 回退到文件时间（M4「无日期」筛选就是找这批）
    assert_eq!(taken_from("b.TIFF"), TakenFrom::Mtime);
}

#[test]
fn total_count_aggregates_the_subtree() {
    let tmp = tempfile::tempdir().unwrap();
    let (db, _p) = open_fixture(tmp.path());

    let totals = db.folder_totals().unwrap();
    // `/photos/2020` 直接 2 张（a.png + b.TIFF + c.unknown 里 c 没被数进 direct_count，
    // 因为夹具里 direct_count 是手写的 2）——递归数与自有数一致，没有子目录。
    assert_eq!(totals.get(&2).copied().unwrap_or(0), 2);
    // `/photos` 自己有 1 张 + 子目录 2 张 + 2021 的 0 张 = 3
    assert_eq!(
        totals.get(&1).copied().unwrap_or(0),
        3,
        "根节点必须是递归合计（自有 1 + 2020 的 2）"
    );
    assert_eq!(totals.get(&3).copied().unwrap_or(0), 0);
}

#[test]
fn dir_mtime_stays_null_so_the_first_startup_is_still_a_full_verify() {
    // `07` §8.2 第一条：**故意不填** `dir_mtime`。
    // 填了它，升级后的首次启动就会以为「所有目录都没变过」，跳过 L2 全校——
    // 而那一次全校正是把「软件没运行期间的变化」补上的唯一机会。
    let tmp = tempfile::tempdir().unwrap();
    let (db, _p) = open_fixture(tmp.path());
    for f in db.all_folders().unwrap() {
        assert!(
            db.folder_dir_mtime(f.id).unwrap().is_none(),
            "{} 的 dir_mtime 升级时必须留空",
            f.path
        );
    }
}

#[test]
fn settings_rows_are_seeded() {
    // 07-A §6：两个设置键必须预置（NULL 表示「用默认刻度」，空 = 没这一行会让
    // 读取方分不清「默认」和「用户显式设成空」）。
    let tmp = tempfile::tempdir().unwrap();
    let (db, _p) = open_fixture(tmp.path());

    assert!(
        db.setting_row_present("grid_cell_px").unwrap(),
        "grid_cell_px 这一行必须预置（值是 NULL = 用默认刻度）"
    );
    assert_eq!(
        db.setting_raw("grid_cell_px").unwrap(),
        None,
        "预置的值是 NULL，不是某个具体刻度"
    );
    assert_eq!(
        db.setting_raw("recurse_subfolders").unwrap().as_deref(),
        Some("0"),
        "recurse_subfolders 默认关（原版行为）"
    );
    // 没有的值返回 None，而不是空串
    assert!(db.setting_raw("no_such_key").unwrap().is_none());
    assert!(!db.setting_row_present("no_such_key").unwrap());
    // 布尔读取对坏值有兜底（设置项坏掉不该让应用起不来）
    assert!(db.setting_bool("no_such_key", true).unwrap());
}

#[test]
fn v6_indexes_exist() {
    let tmp = tempfile::tempdir().unwrap();
    let (db, _p) = open_fixture(tmp.path());

    let names = db.index_names().unwrap();
    for want in [
        "idx_photos_taken_id",
        "idx_photos_size_mtime",
        "idx_folders_verified",
        "idx_photos_format",
        "idx_folders_root_path",
    ] {
        assert!(
            names.iter().any(|n| n == want),
            "{want} 缺失（现有索引：{names:?}）"
        );
    }
}

#[test]
fn migration_is_idempotent() {
    let tmp = tempfile::tempdir().unwrap();
    let (db, db_path) = open_fixture(tmp.path());

    // 第二次打开：DDL 不重跑、不重复备份
    let (db2, report2) = Db::open_with_report(&db_path).unwrap();
    assert_eq!(db2.schema_version().unwrap(), SCHEMA_VERSION);
    assert_eq!(report2.from, SCHEMA_VERSION);
    assert!(report2.backup.is_none(), "已是最新就不该再备份一次");

    // 数据步骤跑两次结果相同：第二次必须是空操作
    let first = db.apply_v6_data().unwrap();
    let second = db.apply_v6_data().unwrap();
    assert_eq!(second, 0, "第二次补数必须什么都没改（第一次改了 {first}）");

    // 补数之后 total_count 没有被重复叠加（递归聚合最容易在这里错）
    let totals = db2.folder_totals().unwrap();
    assert_eq!(totals.get(&1).copied().unwrap_or(0), 3);
}

#[test]
fn a_v6_library_created_from_scratch_has_the_same_shape() {
    // 空库直接建到 v6：与「v5 升上来」的库列结构必须一致。
    // 这条守的是「新增库路径与升级路径不许分叉」——分叉过一次（v3 的 eff_taken_at），
    // 表现是全新安装的库缺列，只在运行期炸。
    let tmp = tempfile::tempdir().unwrap();
    let fresh = Db::open(tmp.path().join("fresh.sqlite")).unwrap();
    let (upgraded, _p) = open_fixture(tmp.path());

    for table in ["folders", "photos", "roots", "settings"] {
        let a = fresh.column_names(table).unwrap();
        let b = upgraded.column_names(table).unwrap();
        assert_eq!(a, b, "{table} 的列结构在两条路径上必须一致");
    }
    assert_eq!(fresh.schema_version().unwrap(), SCHEMA_VERSION);
}
