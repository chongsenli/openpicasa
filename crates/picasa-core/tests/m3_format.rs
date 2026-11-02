//! 多格式与解码隔离的验收测试（`07-A` §12.2 `TC-M3-FORMAT-001～008` / R-M3-10）。
//!
//! ## 这一层最容易被忽略的风险
//!
//! 解码器是**唯一把外部字节变成内存分配**的入口。一个声称自己是
//! 30000×30000 的 PNG，解码器会照着分配 3.6 GB——进程当场被系统杀掉，
//! 而用户看到的是「软件崩了」，没有任何可操作的线索。
//!
//! 所以「在进解码器之前拦下」不是优化，是**稳定性前提**。
//!
//! ## 哪些能测
//!
//! - 嗅探（纯函数，最全面）
//! - 尺寸预检（纯函数）
//! - 熔断（状态机）
//! - 扩展名清单（常量）
//!
//! 不能在这里测的是**真实解码各格式**（需要样本文件 + 各格式解码器）。
//! 那部分由 `fixtures/formats/` 与 `idx_bench --by-format` 承担（`07-A` §4.4）。

use picasa_core::filters::is_supported_image;
use picasa_imaging::{
    Admit, DECODE_MAX_PIXELS, DecodeGuard, RejectReason, SniffedFormat, sniff, sniff_dimensions,
};
use std::path::PathBuf;

fn png_head(w: u32, h: u32) -> Vec<u8> {
    let mut v = vec![
        0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A, 0x00, 0x00, 0x00, 0x0D, b'I', b'H', b'D',
        b'R',
    ];
    v.extend_from_slice(&w.to_be_bytes());
    v.extend_from_slice(&h.to_be_bytes());
    v
}

#[test]
fn tc_m3_format_001_extension_and_magic_mismatch_decodes_by_magic() {
    // FORMAT-001：`.jpg` 内容为 PNG 可正常索引（按魔数解，不是失败）。
    let guard = DecodeGuard::new();
    let (admit, fmt) = guard.admit("jpg", &png_head(800, 600));
    assert_eq!(admit, Admit::Go, "★ 扩展名错不是失败——它是用户改错过后缀");
    assert_eq!(fmt, SniffedFormat::Png, "按魔数认出真实格式");

    // `.jpg` 内容是文本 → 可见失败
    let (admit, fmt) = guard.admit("jpg", b"this is definitely not an image");
    assert_eq!(fmt, SniffedFormat::Unknown);
    assert_eq!(admit, Admit::Reject(RejectReason::UnknownMagic));
}

#[test]
fn tc_m3_format_002_actual_format_is_recorded_not_the_extension() {
    // FORMAT-02：`photos.format` 记**实际**格式，这样 M4 的类型筛选不会把
    // 「叫 .jpg 的 PNG」归到 JPEG 组里。
    //
    // 这里验的是判定本身：同一份 PNG 字节，无论扩展名是什么，嗅探结果都是 PNG。
    for ext in ["jpg", "jpeg", "bmp", "tif", ""] {
        assert_eq!(
            sniff(&png_head(100, 100)),
            SniffedFormat::Png,
            "扩展名 {ext:?} 不该影响实际格式判定"
        );
    }
    // 而 `PhotoFormat::from_extension` 是**另一个**概念（候选），两者不一致是正常的
    use picasa_db::PhotoFormat;
    assert_eq!(PhotoFormat::from_extension("jpg"), Some(PhotoFormat::Jpeg));
    assert_eq!(PhotoFormat::from_extension("png"), Some(PhotoFormat::Png));
}

#[test]
fn tc_m3_format_003_oversized_image_is_rejected_without_decoding() {
    // FORMAT-003：1 张 30000×30000 PNG 不分配 > 64 MB，进可见失败。
    //
    // 「不分配」这件事在这里被表达为「根本没让它进解码器」——
    // 预检在 `sniff_dimensions` 阶段就返回了，`Admit::Reject` 之后没有任何
    // 像素分配发生。
    let guard = DecodeGuard::new();
    let head = png_head(30_000, 30_000);
    let (admit, _) = guard.admit("png", &head);
    let Admit::Reject(RejectReason::TooLarge { w, h, max_pixels }) = admit else {
        panic!("30000×30000 必须在解码前被拒")
    };
    assert_eq!((w, h), (30_000, 30_000));
    assert_eq!(max_pixels, DECODE_MAX_PIXELS);
    // 9 亿像素 × 4 B（RGBA8）= 3.6 GB —— 这就是「必须预检」的量化理由。
    // （48 位加法在这里会溢出，所以用 u64 算；`sniff_dimensions` 也是 u64。）
    let bytes = (w as u128) * (h as u128) * 4;
    assert!(
        bytes >= 3_600_000_000,
        "30000×30000 RGBA8 需要 {bytes} 字节，远超 decode_mem_cap"
    );

    // 边界内的图正常放行
    let ok = png_head(10_000, 8_000); // 80 MP，正好是上限
    assert_eq!(guard.admit("png", &ok).0, Admit::Go);
    let over = png_head(10_001, 8_000);
    assert!(matches!(guard.admit("png", &over).0, Admit::Reject(_)));
}

#[test]
fn tc_m3_format_005_every_listed_extension_is_a_candidate() {
    // 扩展名清单是**候选**：入候选 → 读文件头 → 按魔数解。
    // 清单漏一个 = 用户看到文件但软件完全不认（连失败列表里都没有）。
    for ext in [
        "jpg", "jpeg", "png", "gif", "bmp", "tif", "tiff", "webp", "svg", "heic", "avif", "dng",
        "cr2", "cr3", "nef", "arw", "raf", "rwl", "rw2",
    ] {
        assert!(
            is_supported_image(&PathBuf::from(format!("a.{ext}"))),
            "{ext} 必须在候选清单里"
        );
    }
    // 不在清单里的照旧排除
    for ext in ["mp4", "mov", "psd", "txt", "ini", "picasa.ini"] {
        assert!(!is_supported_image(&PathBuf::from(format!("a.{ext}"))));
    }
}

#[test]
fn tc_m3_format_006_no_format_is_silently_dropped() {
    // R-M2-4（失败永不隐藏）在多格式上的形式：
    // B/C 级没编译进来时，文件仍然**入库**，缩略图进**可见失败**——
    // 而不是根本不出现。
    //
    // 这里验的是「入库路径不依赖解码能力」：守门器拒绝的文件仍然产生一条
    // `PhotoInput`（scanner 在 `Admit::Reject` 分支里照样构造记录）。
    // 由于那是 scanner 内部逻辑，这里用「清单包含 + 守门器不 panic」间接守住。
    let guard = DecodeGuard::new();
    let head = b"\x00\x00\x00\x18ftypheic\x00\x00\x00\x00mif1heic".to_vec();
    // HEIC 能嗅探出来（即使解码器没编译进来）
    assert_eq!(sniff(&head), SniffedFormat::Heic);
    // 且不因为「不会解 HEIC」就在守门阶段被静默丢弃
    let (admit, fmt) = guard.admit("heic", &head);
    assert!(!matches!(admit, Admit::Reject(RejectReason::UnknownMagic)));
    assert_eq!(fmt, SniffedFormat::Heic);
}

#[test]
fn tc_m3_format_007_dimension_probe_is_cheap_and_offline() {
    // FORMAT-007：非 JPEG 没有 DCT 缩放，走「全解码 → 一次下采样」。
    // 「一次解码」的前提是**尺寸从文件头就能拿到**（不用先解一遍量尺寸）。
    for (w, h) in [(100u32, 200u32), (3840, 2160), (8000, 6000)] {
        assert_eq!(sniff_dimensions(&png_head(w, h)), Some((w as u64, h as u64)));
    }
    // GIF 的尺寸在逻辑屏幕描述符
    let gif = b"GIF89a\x40\x01\xF0\x00".to_vec();
    assert_eq!(sniff_dimensions(&gif), Some((320, 240)));
}

#[test]
fn tc_m3_guard_004_fuse_stops_a_bad_directory_from_killing_the_pipeline() {
    // GUARD-004 / R-M3-10：「连续 panic > 20/分钟 → 熔断该格式」。
    //
    // 一个坏目录里可能有几千个畸形文件，全是同一个解析器崩的。
    // 逐文件熔断挡不住它（每个文件都「值得试一次」），按格式熔断才能真的停下来。
    let guard = DecodeGuard::new();
    let head = png_head(100, 100);
    // 未熔断时正常放行
    assert_eq!(guard.admit("png", &head).0, Admit::Go);
    for _ in 0..picasa_imaging::format::DECODE_PANIC_FUSE_PER_MIN {
        guard.note_panic("PNG");
    }
    assert!(guard.is_fused("PNG"));
    let (admit, _) = guard.admit("png", &head);
    let Admit::Reject(RejectReason::Fused { format }) = admit else {
        panic!("熔断后必须拒绝")
    };
    assert_eq!(format, "PNG");
    // 其他格式仍然可用（隔离）
    assert_eq!(
        guard.admit("jpg", &[0xFF, 0xD8, 0xFF, 0xE0]).0,
        Admit::Go,
        "★ 熔断必须按格式隔离，不能一个坏了就全停"
    );
}

#[test]
fn tc_m3_format_008_old_library_pngs_enter_as_added() {
    // FORMAT-010：v5 库里从未入库的 PNG 在首次 L2 全校时作为 `added` 自然进入。
    // 那条路径由 `verify` + `apply_scan` 保证（各自有独立用例）。
    // 这里守住它的**前提**：PNG 一定在白名单里，否则 L2 根本不会把它当候选。
    assert!(is_supported_image(&PathBuf::from("retro/scan.png")));
    assert!(is_supported_image(&PathBuf::from("retro/scan.PNG")));
}

#[test]
fn sniffing_never_panics_on_arbitrary_bytes() {
    // 解码器是**不可信输入**的入口。守门器自己必须先扛得住任意字节：
    // 随机数据、截断的魔数、超长重复字节，都不能 panic。
    let cases: Vec<Vec<u8>> = vec![
        vec![],
        vec![0u8; 1],
        vec![0xFF],
        vec![0xFF, 0xD8],
        vec![0x89, b'P', b'N', b'G'],
        b"RIFF".to_vec(),
        b"RIFF\x00\x00\x00\x00WEBP".to_vec(),
        b"II\x2A\x00".to_vec(),
        b"II\x55\x00".to_vec(),
        b"ftyp".to_vec(),
        (0u8..=255).collect(),
        vec![0x89; 100],
        b"<svg".to_vec(),
        b"<?xml".to_vec(),
    ];
    for c in cases {
        let f = sniff(&c);
        // 不 panic 就行；结果是什么不重要
        let _ = f.label();
        let _ = sniff_dimensions(&c);
        let guard = DecodeGuard::new();
        let _ = guard.admit("jpg", &c);
    }
}

#[test]
fn png_dimensions_with_garbage_after_ihdr_do_not_overflow() {
    // 尺寸字段溢出：0xFFFFFFFF × 0xFFFFFFFF = 1.8e19，u64 都放不下。
    // `sniff_dimensions` 用 u64 承接 u32 → 不会溢出；
    // 而 `w.saturating_mul(h)` 保证守门判断不会 wrap 成小数。
    let head = png_head(u32::MAX, u32::MAX);
    let (w, h) = sniff_dimensions(&head).unwrap();
    assert_eq!((w, h), (u32::MAX as u64, u32::MAX as u64));
    let guard = DecodeGuard::new();
    assert!(matches!(
        guard.admit("png", &head).0,
        Admit::Reject(RejectReason::TooLarge { .. })
    ));
}
