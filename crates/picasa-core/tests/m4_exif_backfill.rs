//! M4 EXIF 回填的验收用例（docs/11 §3、验收重点 6）。
//!
//! 用真实临时库 + 临时「照片」（纯文本，EXIF 读不出来）验证：
//! 回填后 `exif_status` 从 0 变成终态，且不会重复处理同一行。

use picasa_core::m4_exif;
use picasa_db::{Db, PhotoInput};

fn photo(name: &str) -> PhotoInput {
    PhotoInput {
        name: name.to_string(),
        name_key: name.to_ascii_lowercase(),
        size_bytes: 10,
        mtime_ns: 1_000_000_000,
        width: Some(1),
        height: Some(1),
        orientation: Some(1),
        taken_at: Some(1000),
        taken_at_src: 2,
        format: 1,
    }
}

#[test]
fn backfill_marks_pending_rows_terminal_and_is_idempotent() {
    let tmp = tempfile::tempdir().unwrap();
    let db = Db::open(tmp.path().join("lib.sqlite")).unwrap();
    let root = db.upsert_root(tmp.path().to_str().unwrap(), 0, false).unwrap();
    let fid = db
        .upsert_folder(tmp.path().to_str().unwrap(), tmp.path().to_str().unwrap(), None)
        .unwrap();
    db.set_folder_root(fid, root, None).unwrap();
    // 造两个真实存在的文件（内容是文本，EXIF 探测会降级但不 panic）
    std::fs::write(tmp.path().join("a.jpg"), b"not a jpeg").unwrap();
    std::fs::write(tmp.path().join("b.jpg"), b"also not a jpeg").unwrap();
    db.apply_scan(fid, tmp.path().to_str().unwrap(), &[photo("a.jpg"), photo("b.jpg")], 0)
        .unwrap();

    assert_eq!(db.photos_pending_exif(10).unwrap().len(), 2);
    let (ok, failed) = m4_exif::backfill_batch(&db, 10).unwrap();
    // probe() 对坏文件返回 Ok(降级)，所以两张都写成 status=1（无 EXIF 字段）
    assert_eq!(ok + failed, 2, "两张都要被处理，不能留 0 状态反复重跑");
    assert_eq!(db.photos_pending_exif(10).unwrap().len(), 0, "回填后无待处理行");

    // 幂等：再跑一次不处理任何行
    let (ok2, _f2) = m4_exif::backfill_batch(&db, 10).unwrap();
    assert_eq!(ok2, 0);

    // invalidate 后重新变为待处理
    let rows = db
        .photos_from_visible(None, picasa_db::SortSpec::default(), 10, 0)
        .unwrap();
    m4_exif::invalidate_exif(&db, rows[0].id).unwrap();
    assert_eq!(db.photos_pending_exif(10).unwrap().len(), 1);
}
