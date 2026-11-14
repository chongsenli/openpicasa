//! M4.3 阶段 3 第一批：IDX-02/03/04/05 导入导出加固的端到端验收。
//!
//! 覆盖（docs/18 §1 / §5 TC-M43-02/03/04/11）：
//! - 干净关闭的本产品 `library.sqlite`（无 OPIX magic）可作为来源导入；
//! - 仍带未合并 WAL 的来源必须拒绝并提示先导出；
//! - `check_export_dir` 在照片根内**一个字节都不写**（R-M43-01）；
//! - 导出报告带产物 SHA-256 / 时区 / 产物口径计数，且目标不能覆盖当前库；
//! - Replace 备份路径与 SHA-256 随报告返回（可一键回退）；
//! - 重复合并幂等（同一包导入两次不产生重复照片）。
//!
//! 全程只用临时目录 + SQLite，不碰真实照片 / 用户文件。

use picasa_db::{
    Db, ImportStrategy, PhotoInput, SortSpec, check_export_dir, cleanup_staging, execute_import,
    export_index, inspect_index,
};
use std::path::Path;
use std::sync::atomic::AtomicBool;

fn photo(name: &str, taken: i64) -> PhotoInput {
    PhotoInput {
        name: name.to_string(),
        name_key: name.to_ascii_lowercase(),
        size_bytes: 1000,
        mtime_ns: taken * 1_000_000_000,
        width: Some(800),
        height: Some(600),
        orientation: Some(1),
        taken_at: Some(taken),
        taken_at_src: 0,
        format: 1,
    }
}

/// 造一个含 3 张照片 + 星标 / 标签 / 说明 / 相册的真实库。
fn seed(path: &Path) -> Db {
    let db = Db::open(path).unwrap();
    let root = db.upsert_root("/photos", 0, false).unwrap();
    let fid = db.upsert_folder("/photos", "/photos", None).unwrap();
    db.set_folder_root(fid, root, None).unwrap();
    let items = vec![photo("a.jpg", 100), photo("b.jpg", 200), photo("c.jpg", 300)];
    db.apply_scan(fid, "/photos", &items, 0).unwrap();
    let rows = db
        .photos_from_visible(None, SortSpec::default(), 20, 0)
        .unwrap();
    let a = rows.iter().find(|p| p.name == "a.jpg").unwrap().id;
    let b = rows.iter().find(|p| p.name == "b.jpg").unwrap().id;
    db.set_starred(&[a], true).unwrap();
    db.add_tag(&[a, b], "旅行").unwrap();
    db.set_caption(a, "京都塔").unwrap();
    let album = db.create_album("2026 京都", 1000).unwrap();
    db.add_to_album(album, &[a, b], 1000).unwrap();
    db
}

/// TC-M43-02：干净关闭的本产品 `library.sqlite`（没有 OPIX magic / 清单）必须能作为
/// 来源导入。stage_and_validate 的第二路径要合成清单，且来源文件 SHA-256 前后不变。
#[test]
fn clean_native_db_imports_with_synthesized_manifest() {
    let tmp = tempfile::tempdir().unwrap();
    let native = tmp.path().join("library.sqlite");
    {
        // drop 最后一个连接即 checkpoint + 删除 WAL，模拟「应用正常退出」后的干净库。
        let _db = seed(&native);
    }
    // 干净库不带任何 -wal/-shm 残留。
    let wal = tmp.path().join("library.sqlite-wal");
    assert!(
        !wal.exists() || std::fs::metadata(&wal).map(|m| m.len() == 0).unwrap_or(true),
        "干净关闭的库不应有非空 -wal"
    );

    let dst = tmp.path().join("dst.sqlite");
    let mut dst_db = Db::open(&dst).unwrap();
    let cancel = AtomicBool::new(false);
    let rep = execute_import(
        &mut dst_db,
        &native,
        tmp.path(),
        ImportStrategy::Merge,
        &cancel,
    )
    .unwrap();
    assert_eq!(rep.imported, 3, "干净本产品 DB 的 3 张照片都要进来");
    assert_eq!(dst_db.photo_count_total().unwrap(), 3);
    assert_eq!(dst_db.starred_ids().unwrap().len(), 1, "星标要一起带过来");
    assert_eq!(dst_db.caption_of(1).unwrap(), "京都塔");
    assert!(!rep.source_sha256.is_empty(), "报告要带来源 SHA-256");

    // 预览同样接受干净 DB，且绝不写任何数据。
    let dst2 = Db::open(tmp.path().join("dst2.sqlite")).unwrap();
    let prev = inspect_index(&dst2, &native, tmp.path()).unwrap();
    assert_eq!(prev.manifest.counts.photos, 3, "合成清单要含正确计数");
    assert_eq!(dst2.photo_count_total().unwrap(), 0, "预览不写库");
}

/// TC-M43-02：仍带未合并 WAL 的来源必须拒绝（提示先导出），不能拿副本瞎猜。
#[test]
fn wal_sidecar_source_is_rejected_with_export_hint() {
    let tmp = tempfile::tempdir().unwrap();
    let src = tmp.path().join("running.sqlite");
    std::fs::write(&src, b"not-even-a-db").unwrap();
    // 非空 -wal 代表还有活动连接 / 崩溃残留。
    std::fs::write(tmp.path().join("running.sqlite-wal"), b"uncheckpointed").unwrap();

    let dst = Db::open_in_memory().unwrap();
    let err = inspect_index(&dst, &src, tmp.path()).unwrap_err();
    assert!(
        err.to_string().contains("WAL") || err.to_string().contains("导出"),
        "错误要提示未合并 WAL / 先导出：{err}"
    );
    // staging 副本必须被清掉，不留垃圾。
    assert_eq!(cleanup_staging(tmp.path()), 0, "被拒的导入不能留 staging 文件");
}

/// R-M43-01：照片根内的目标必须先拒绝，**再**做写探测——一个字节都不能写。
#[test]
fn check_export_dir_writes_no_probe_inside_photo_root() {
    let tmp = tempfile::tempdir().unwrap();
    let inside = tmp.path().join("MyPhotos");
    std::fs::create_dir_all(&inside).unwrap();
    let roots = vec![inside.to_string_lossy().to_string()];

    let err = check_export_dir(&inside, &roots).unwrap_err();
    assert!(err.to_string().contains("图库照片目录内"), "{err}");

    let leftover: Vec<_> = std::fs::read_dir(&inside).unwrap().collect();
    assert!(
        leftover.is_empty(),
        "照片目录内不得留下写探测文件（哪怕随后删除也算写过）：{leftover:?}"
    );
    // 根外的普通目录仍应通过（可写探测正常进行）。
    assert!(check_export_dir(tmp.path(), &roots).is_ok());
}

/// TC-M43-04：导出报告带产物 SHA-256 / 时区 / 产物口径计数；
/// IDX-05：目标不能覆盖当前正在使用的库。
#[test]
fn export_report_has_sha256_timezone_and_product_counts() {
    let tmp = tempfile::tempdir().unwrap();
    let src_path = tmp.path().join("src.sqlite");
    let src_db = seed(&src_path);
    let out = tmp.path().join("out.opicasaidx");
    let rep = export_index(&src_db, &out, false, "0.4.2-test", 1000).unwrap();

    assert_eq!(rep.created_at, 1000);
    assert!(!rep.timezone.is_empty(), "必须记录生成时区");
    assert_eq!(rep.sha256.len(), 64, "产物 SHA-256 必须是 64 位十六进制");
    assert!(rep.sha256.chars().all(|c| c.is_ascii_hexdigit()));
    assert_eq!(rep.counts.photos, 3);
    assert_eq!(rep.counts.albums, 1);

    // 报告计数取自产物（而非 live db），与包内清单一致。
    let dst = Db::open_in_memory().unwrap();
    let prev = inspect_index(&dst, &out, tmp.path()).unwrap();
    assert_eq!(prev.manifest.counts.photos, rep.counts.photos);

    // 目标 = 当前库 → 拒绝（含符号链接别名由 same_path 处理）。
    let err = export_index(&src_db, &src_path, false, "0.4.2-test", 2).unwrap_err();
    assert!(
        err.to_string().contains("当前正在使用的索引库"),
        "不能把导出目标指回运行库：{err}"
    );
}

/// TC-M43-03：Replace 策略的备份路径 / SHA-256 要随报告返回，且备份真实存在。
#[test]
fn replace_strategy_reports_backup_path_and_sha256() {
    let tmp = tempfile::tempdir().unwrap();
    let src_db = seed(&tmp.path().join("src.sqlite"));
    let out = tmp.path().join("out.opicasaidx");
    export_index(&src_db, &out, false, "0.4.2-test", 1000).unwrap();

    let mut dst = Db::open(tmp.path().join("dst.sqlite")).unwrap();
    let cancel = AtomicBool::new(false);
    let rep = execute_import(
        &mut dst,
        &out,
        tmp.path(),
        ImportStrategy::Replace,
        &cancel,
    )
    .unwrap();
    let backup = rep.backup_path.expect("Replace 必须返回备份路径");
    assert!(backup.is_file(), "备份文件必须真实落盘：{}", backup.display());
    let sha = rep.backup_sha256.expect("Replace 必须返回备份 SHA-256");
    assert_eq!(sha.len(), 64);
    assert!(backup.to_string_lossy().contains("pre-import-"));
}

/// TC-M43-03：默认合并幂等——同一个包导入两次不能把照片再插一遍。
#[test]
fn repeated_merge_is_idempotent() {
    let tmp = tempfile::tempdir().unwrap();
    let src_db = seed(&tmp.path().join("src.sqlite"));
    let out = tmp.path().join("out.opicasaidx");
    export_index(&src_db, &out, false, "0.4.2-test", 1000).unwrap();

    let mut dst = Db::open(tmp.path().join("dst.sqlite")).unwrap();
    let cancel = AtomicBool::new(false);
    let r1 = execute_import(&mut dst, &out, tmp.path(), ImportStrategy::Merge, &cancel).unwrap();
    let n1 = dst.photo_count_total().unwrap();
    let r2 = execute_import(&mut dst, &out, tmp.path(), ImportStrategy::Merge, &cancel).unwrap();
    let n2 = dst.photo_count_total().unwrap();

    assert_eq!(n1, 3, "首次合并导入 3 张");
    assert_eq!(r1.imported, 3);
    assert_eq!(n2, 3, "重复合并必须幂等，不能产生第 4~6 张");
    assert_eq!(r2.imported, 0, "第二次应全部跳过");
    assert_eq!(r2.skipped, 3, "重复项计为跳过");
}
