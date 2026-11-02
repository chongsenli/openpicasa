//! 可见作用域（排除已关闭关注文件夹）**不能**让排序退化。
//!
//! ## 为什么单独有这一条
//!
//! `photo_columns.rs::sort_by_taken_at_uses_index_not_temp_btree` 测的是
//! `explain_photos_plan`——SQL 里**没有** `active_root` 过滤。那条查询一直是绿的，
//! 但并不等于栅格真正走的那条也没退化：两边 WHERE 结构不同，SQLite 挑索引的策略
//! 就可能不同。修「关闭根目录后照片还在」这条 bug 时，栅格从 `photos_from`
//! 换成了 `photos_from_visible`，所以必须**对带过滤的那条**重新取证。
//!
//! ## 断言口径
//!
//! 不是「不许出现 TEMP B-TREE」——按文件名/大小/像素数排序**本来**就要建临时 B-tree
//! （没有对应的索引，这是 v1 建库时的既定状态，与本修复无关）。
//! 真正的判据是：**加过滤前后，执行计划必须逐字相同**。
//! 只要一致，就说明这次改动没有引入任何排序退化；不一致才是回归。
use picasa_db::{Db, SortKey, SortSpec};

#[test]
fn visible_scope_does_not_change_any_query_plan() {
    let tmp = tempfile::tempdir().unwrap();
    let db = Db::open(tmp.path().join("p.sqlite")).unwrap();

    for key in [
        SortKey::TakenAt,
        SortKey::Name,
        SortKey::ModifiedAt,
        SortKey::Size,
        SortKey::Dimensions,
    ] {
        for descending in [true, false] {
            let sort = SortSpec { key, descending };
            let plain = db.explain_photos_plan(sort).unwrap();
            let scoped = db.explain_visible_plan(sort).unwrap();

            // 唯一允许的差异：可见作用域多了一次 `SEARCH r USING INTEGER PRIMARY KEY`
            // （那是 `EXISTS (SELECT 1 FROM roots ...)` 的半连接探测，代价 O(log n)）。
            let strip = |v: &[String]| -> Vec<String> {
                v.iter()
                    .filter(|s| !s.contains("SEARCH r USING INTEGER PRIMARY KEY"))
                    .filter(|s| !s.contains("CORRELATED SCALAR SUBQUERY"))
                    .cloned()
                    .collect()
            };
            assert_eq!(
                strip(&plain),
                strip(&scoped),
                "{sort:?} 加可见作用域后执行计划变了（可能引入排序退化）\n\
                 无作用域：{plain:?}\n有作用域：{scoped:?}"
            );
        }
    }
}

#[test]
fn taken_at_still_uses_the_committed_index() {
    let tmp = tempfile::tempdir().unwrap();
    let db = Db::open(tmp.path().join("p.sqlite")).unwrap();
    let plan = db.explain_visible_plan(SortSpec::default()).unwrap().join("\n");
    assert!(
        plan.contains("idx_photos_eff_taken"),
        "默认排序（按拍摄时间）的可见作用域查询没走 idx_photos_eff_taken：\n{plan}"
    );
    assert!(
        !plan.contains("TEMP B-TREE FOR ORDER BY"),
        "默认排序在可见作用域下退回了临时 B-tree（5 万张会从 1.8ms 变 199ms）：\n{plan}"
    );
}
