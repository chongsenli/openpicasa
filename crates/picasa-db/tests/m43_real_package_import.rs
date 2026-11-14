//! 真实大包端到端导入（默认 `#[ignore]`，需要外部文件）。
//!
//! 背景：用户手里有一个 9.1 GiB / 268 565 张照片的索引包，导出它的版本是
//! `0.4.1-Beta2`（格式 v1、schema v8）。这条测试把那个包**完整导进一个空临时库**，
//! 用来回答三个不能靠单测回答的问题：
//!
//! 1. v1 包的清单校验和能不能被接受（阶段 3B 把 `folder_tree` 拼进 FNV 后
//!    曾把老包全判成「已损坏」，见 `export::tests` 里的回归用例）；
//! 2. v8 → v9 迁移在真实的 26 万张库上能不能跑完；
//! 3. `execute_import` 的整条链路（副本 → 校验 → 迁移 → 单事务拷贝
//!    26.8 万照片 / 26.7 万缩略图 / 4588 文件夹 → 按树还原）会不会在中途失败。
//!
//! 跑法：
//! ```text
//! PERF_HASH_FILE=/path/to/photo-index.opicasaidx.zip \
//!   cargo test -p picasa-db --release --test m43_real_package_import -- \
//!   --ignored --nocapture
//! ```

use picasa_db::{Db, ImportStrategy, execute_import, inspect_index};
use std::sync::atomic::AtomicBool;

fn package() -> Option<std::path::PathBuf> {
    std::env::var_os("PERF_HASH_FILE").map(std::path::PathBuf::from)
}

#[test]
#[ignore = "需要外部大索引包 + 数十分钟，手工跑（PERF_HASH_FILE=...）"]
fn a_real_large_package_imports_completely() {
    let Some(pkg) = package() else {
        return;
    };
    let tmp = tempfile::tempdir().unwrap();
    let mut dst = Db::open(tmp.path().join("library.sqlite")).unwrap();
    let work = tmp.path().join("work");

    let t = std::time::Instant::now();
    let preview = inspect_index(&dst, &pkg, &work).expect("预览校验应通过");
    println!("[1/2] 预览校验完成：{:.1?}", t.elapsed());
    println!(
        "      清单 photos={} roots={} folders={} thumbs={}",
        preview.manifest.counts.photos,
        preview.manifest.counts.roots,
        preview.manifest.counts.folders,
        preview.manifest.counts.thumbs
    );
    println!(
        "      树来源={} 树根={} 树文件夹={}",
        preview.tree_source.label(),
        preview.tree.roots.len(),
        preview.tree.folders.len()
    );
    assert_eq!(
        preview.manifest.counts.photos, 268_565,
        "这份样本的照片数变了，测试基准要跟着更新"
    );

    let t = std::time::Instant::now();
    let cancel = AtomicBool::new(false);
    let report = execute_import(&mut dst, &pkg, &work, ImportStrategy::Merge, &cancel)
        .expect("真实大包必须能整包导入成功");
    println!("[2/2] 导入完成：{:.1?}", t.elapsed());
    println!(
        "      imported={} skipped={} failed={} conflicts={}",
        report.imported, report.skipped, report.failed, report.conflicts
    );

    let photos = dst.total_photos().unwrap();
    println!("      库内照片={photos}");
    assert_eq!(photos as i64, report.imported as i64, "库内照片数应等于本次导入数");
    assert_eq!(report.failed, 0, "整包导入不应有失败项");

    // 还原的层级必须落地：26 万张照片不可能只有根目录。
    let roots = dst.roots(false).unwrap();
    assert_eq!(roots.len(), 3, "样本有 3 个来源根");
    for r in &roots {
        println!("      root {} origin={} imported={}", r.path, r.origin, r.is_imported());
    }

    // 工作目录必须干净：不许留 9 GiB 的 staging 副本。
    let leftovers: Vec<_> = std::fs::read_dir(&work)
        .map(|d| d.filter_map(|e| e.ok()).map(|e| e.file_name()).collect())
        .unwrap_or_default();
    assert!(leftovers.is_empty(), "工作目录残留：{leftovers:?}");
}
