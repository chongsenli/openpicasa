//! 在**真实库的副本**上跑一次 v5 → v6 迁移，打印结果。
//!
//! 为什么要有这个 example：迁移测试的夹具是手写的 v5 库，那只能证明「我写的 DDL
//! 能跑」。真实库有几十万张照片、几千个目录、以及历史遗留的脏数据，
//! 那才是迁移真正的考场。
//!
//! 用法（**必须传副本**，本程序拒绝直接指向真实库）：
//!
//! ```text
//! cp ~/Library/"Application Support"/app.openpicasa/library.sqlite /tmp/real.sqlite
//! cargo run -p picasa-db --example m3_migrate_real -- /tmp/real.sqlite
//! ```

use picasa_db::{Db, PhotoFormat, SortSpec};
use std::time::Instant;

fn scalar_i64(path: &str, sql: &str) -> i64 {
    let conn = rusqlite::Connection::open(path).unwrap();
    conn.query_row(sql, [], |r| r.get::<_, i64>(0)).unwrap_or(0)
}

fn main() {
    let Some(path) = std::env::args().nth(1) else {
        eprintln!("用法: m3_migrate_real <库文件副本路径>");
        std::process::exit(2);
    };
    if path.contains("Application Support") {
        eprintln!("✋ 拒绝直接操作真实库。请先 `cp` 一份到 /tmp 再跑。");
        std::process::exit(2);
    }

    let before_ver = scalar_i64(
        &path,
        "SELECT COALESCE(MAX(version),0) FROM schema_migrations",
    );
    let before_photos = scalar_i64(&path, "SELECT COUNT(*) FROM photos");
    let before_folders = scalar_i64(&path, "SELECT COUNT(*) FROM folders");
    println!("迁移前：v{before_ver} · {before_photos} 张 · {before_folders} 个目录");

    let t0 = Instant::now();
    let (db, report) = Db::open_with_report(&path).expect("迁移失败");
    let elapsed = t0.elapsed();

    println!("迁移后：v{} · 耗时 {elapsed:?}", db.schema_version().unwrap());
    println!(
        "备份：{}",
        report
            .backup
            .as_ref()
            .map(|p| p.display().to_string())
            .unwrap_or_else(|| "（无，说明本来已是最新）".into())
    );
    if let Some(e) = &report.error {
        println!("⚠️ 迁移报错：{e}");
    }

    let after_photos = db.photo_count_total().unwrap();
    println!(
        "照片行数 {before_photos} → {after_photos} {}",
        if after_photos as i64 == before_photos { "✅" } else { "❌ 变了！" }
    );

    let by_fmt = db.format_histogram().unwrap_or_default();
    let total: u64 = by_fmt.iter().map(|(_, n)| *n).sum();
    let unknown: u64 = by_fmt
        .iter()
        .filter(|(f, _)| *f == PhotoFormat::Unknown.code())
        .map(|(_, n)| *n)
        .sum();
    println!("\nformat 分布（共 {total}）：");
    for (code, n) in &by_fmt {
        println!("  {:<6} {n}", PhotoFormat::from_code(*code).label());
    }
    if total > 0 {
        println!("未知占比 {:.2}%", unknown as f64 / total as f64 * 100.0);
    }

    let t1 = Instant::now();
    let snap = db.count_snapshot().unwrap();
    println!(
        "\ncount_snapshot：{:?} · {} 个目录 · 所有照片 {}",
        t1.elapsed(),
        snap.direct.len(),
        snap.all_photos
    );
    let bad = snap
        .direct
        .iter()
        .filter(|(id, d)| snap.totals.get(id).copied().unwrap_or(0) < **d)
        .count();
    println!("total < direct 的目录数：{bad} {}", if bad == 0 { "✅" } else { "❌" });

    let plan = db.explain_keyset_plan(SortSpec::default()).unwrap_or_default();
    let joined = plan.join(" | ");
    println!("\n键集计划：{joined}");
    println!(
        "  {}",
        if joined.contains("SCAN p") { "❌ 退化成全表扫描" } else { "✅ 走索引" }
    );
}
