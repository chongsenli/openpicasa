//! 解码隔离的**实测**验收（`07-A` §4.3 R-M3-10 / `TC-M3-GUARD-004`）。
//!
//! ## 这个文件在验什么
//!
//! R-M3-10 的原话：
//!
//! > 任何解码器对任何输入都不得：崩进程、无限循环、写文件、分配超过
//! > `decode_mem_cap` 的内存。
//!
//! 上面四条里，「不崩」和「不超时」**只能靠真的跑一遍**验证——
//! 代码里写了 `catch_unwind` 不等于它罩住了该罩的东西；
//! 尺寸预检写了不等于所有格式都走到了它。
//!
//! 所以这里把 `picasa_imaging::fixtures` 造的每一类畸形文件**真的喂给解码链路**
//! （`probe` + `thumb_t0`），断言：
//!
//! 1. 每个都返回 `Result`（`Err` 完全可接受——**崩溃或挂起不可接受**）
//! 2. 整体在**时间上限**内跑完（挂起 = 失败）
//! 3. 超过尺寸上限的样本被**预检**拦下，不进入解码器
//!
//! ## 为什么这是「实测」而不是「单测」
//!
//! 单测能测嗅探器、能测守门器，但测不到「真把 5000 层嵌套的 SVG 递给解析器会怎样」。
//! 那个行为取决于依赖库的实现，只有跑一遍才知道。
//!
//! ## 跑多久
//!
//! 全部样本约 50 个，每个都做一次「探测 + 生成 T0」。畸形文件通常在解析头部
//! 的几毫秒内就失败，所以整个文件在 debug 构建下也能在几秒内跑完。
//! 只有 `png-huge-claim` 这类会被预检直接挡掉（不花时间）。
//!
//! **注意**：这个用例**故意不设「必须全部失败」的断言**。一个畸形文件被成功解码
//! （比如「位深 7 其实解码器能容错」）不是缺陷——缺陷是它崩了或者卡住了。

use picasa_imaging::fixtures::{MalformedSample, all_samples, write_samples};
use std::time::{Duration, Instant};

/// 单个样本的处理时间上限。
///
/// 取 3 秒的理由：正常的小图解码是毫秒级，畸形文件通常在解析头部时立刻失败。
/// 3 秒还没结束就说明**卡住了**（无限循环 / 等待不可能到来的数据）——
/// 那正是 R-M3-10 的「不得无限循环」。留 3 秒而不是 100 ms 是给 debug 构建的余量
/// （debug 下解码慢一个数量级）。
const PER_SAMPLE_BUDGET: Duration = Duration::from_secs(3);

/// 全部样本的总预算。50 个 × 平均 1 ms 远低于它；超了说明有系统性卡顿。
const TOTAL_BUDGET: Duration = Duration::from_secs(60);

struct Outcome {
    name: String,
    intent: &'static str,
    elapsed: Duration,
    probe: &'static str,
    thumb: &'static str,
}

#[test]
fn no_malformed_sample_crashes_or_hangs_the_decoder() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().join("malformed");
    let samples = all_samples();
    let paths = write_samples(&dir, &samples).unwrap();
    assert_eq!(paths.len(), samples.len());

    let started = Instant::now();
    let mut outcomes: Vec<Outcome> = Vec::new();
    let mut slow: Vec<(String, Duration)> = Vec::new();
    let mut panicked: Vec<(String, String)> = Vec::new();

    for (sample, path) in samples.iter().zip(paths.iter()) {
        let t0 = Instant::now();
        // 整个解码链路都要罩在 `catch_unwind` 里：
        // 我们要测的是「进程会不会崩」，而 panic 正是「会崩」的前身。
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            // ① EXIF / 尺寸探测（扫描主路径的第一步）
            let probe = picasa_imaging::probe(path);
            // ② T0 缩略图（解码主路径）
            let thumb = picasa_imaging::thumb_t0(path);
            (probe.is_ok(), thumb.is_ok())
        }));
        let elapsed = t0.elapsed();

        match result {
            Ok((probe_ok, thumb_ok)) => {
                if elapsed > PER_SAMPLE_BUDGET {
                    slow.push((sample.name.clone(), elapsed));
                }
                outcomes.push(Outcome {
                    name: sample.name.clone(),
                    intent: sample.intent,
                    elapsed,
                    probe: if probe_ok { "ok" } else { "err" },
                    thumb: if thumb_ok { "ok" } else { "err" },
                });
            }
            Err(e) => {
                let msg = panic_message(&e);
                panicked.push((sample.name.clone(), msg));
            }
        }
    }

    let total = started.elapsed();

    // ---- 断言 1：不许 panic
    assert!(
        panicked.is_empty(),
        "★ R-M3-10 违反：{} 个畸形样本让解码链路 panic。\
         这些必须在解码段外层被 catch 住，变成可见失败：\n{:#?}",
        panicked.len(),
        panicked
    );

    // ---- 断言 2：不许卡住
    assert!(
        slow.is_empty(),
        "★ R-M3-10 违反：{} 个样本超过 {PER_SAMPLE_BUDGET:?}（疑似无限循环）：\n{:#?}",
        slow.len(),
        slow
    );
    assert!(
        total < TOTAL_BUDGET,
        "全部样本用了 {total:?}，超过总预算 {TOTAL_BUDGET:?}"
    );

    // ---- 断言 3：每个样本都必须有确定结局（ok / err 都行，但必须有）
    assert_eq!(outcomes.len(), samples.len(), "每个样本都要跑到底");

    // 打印一张表：失败时能一眼看出是哪一类没被拦住
    eprintln!("\n畸形样本解码结果（{total:?}，{} 个）：", outcomes.len());
    eprintln!("{:<32} {:<8} {:<6} {:<6}", "样本", "耗时", "probe", "thumb");
    for o in &outcomes {
        eprintln!(
            "{:<32} {:<8} {:<6} {:<6} {}",
            o.name,
            format!("{}ms", o.elapsed.as_millis()),
            o.probe,
            o.thumb,
            o.intent
        );
    }
}

#[test]
fn oversized_claims_are_stopped_before_the_decoder_runs() {
    // FORMAT-03 / R-M3-10：尺寸预检必须在**进解码器之前**拦下。
    //
    // 这一条的证明方式不是「解码失败了」，而是「守门器直接拒绝」——
    // 因为交给解码器时它已经按声称尺寸分配好内存了。
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().join("oversized");
    let samples: Vec<MalformedSample> = all_samples()
        .into_iter()
        .filter(|s| s.name == "png-huge-claim.png" || s.name == "bmp-huge.bmp")
        .collect();
    assert_eq!(samples.len(), 2, "两个巨型声称样本都要在");
    let paths = write_samples(&dir, &samples).unwrap();

    for (sample, path) in samples.iter().zip(paths.iter()) {
        let head = std::fs::read(path).unwrap();
        let head32 = &head[..head.len().min(32)];
        let ext = sample.name.rsplit('.').next().unwrap_or("");
        let guard = picasa_imaging::format::DecodeGuard::new();
        let (admit, _fmt) = guard.admit(ext, head32);
        match admit {
            picasa_imaging::Admit::Reject(picasa_imaging::RejectReason::TooLarge { .. }) => {}
            other => panic!(
                "★ {} 必须被尺寸预检拦下，实际是 {other:?}——\
                 这意味着它进了解码器，会按声称尺寸分配内存",
                sample.name
            ),
        }
    }

    // PNG 的样本声称 30000×30000 = 9 亿像素；RGBA8 = 3.6 GB。
    // 对照 `decode_mem_cap`（512 MB）与 `DECODE_MAX_PIXELS`（80 MP）——
    // 这条是**编译期**就该成立的常数关系，写成 `const` 块让它在编译时检查。
    const { assert!(900_000_000u64 > picasa_imaging::DECODE_MAX_PIXELS) };
}

#[test]
fn a_broken_file_becomes_a_typed_error_not_a_silent_skip() {
    // R-M2-4：失败永不隐藏。畸形文件必须产生 `Err`（→ 进可见失败队列），
    // 而不是「返回 Ok 但什么都没做」——后者会让它看起来像已成功。
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().join("one");
    let s = MalformedSample {
        name: "totally-broken.jpg".into(),
        bytes: b"this is emphatically not a jpeg".to_vec(),
        intent: "纯文本但扩展名是 .jpg",
    };
    let paths = write_samples(&dir, &[s]).unwrap();
    let r = picasa_imaging::thumb_t0(&paths[0]);
    assert!(
        r.is_err(),
        "★ 坏文件必须返回 Err（进可见失败队列），不能假装成功"
    );
    // 错误信息要能说清「是什么坏了」，不能是空串
    let msg = format!("{}", r.unwrap_err());
    assert!(!msg.is_empty(), "错误必须带原因");
}

#[test]
fn an_empty_file_does_not_panic() {
    // 空文件是最常见的「畸形」来源（下载中断、0 字节占位）。
    // 它必须走 `Err` 路径，不能碰巧走到「读 0 字节 → 解 0×0 图 → 除零」。
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().join("empty");
    let s = MalformedSample {
        name: "empty.jpg".into(),
        bytes: Vec::new(),
        intent: "0 字节",
    };
    let paths = write_samples(&dir, &[s]).unwrap();
    assert!(picasa_imaging::thumb_t0(&paths[0]).is_err());
    // probe 对空文件是**容错**的（返回默认值），这也不算失败——
    // 它只是「没读到任何元数据」，不是「文件坏了」。
    let _ = picasa_imaging::probe(&paths[0]);
}

/// 从 `catch_unwind` 的 `Box<dyn Any>` 里尽力取出可读信息。
fn panic_message(e: &Box<dyn std::any::Any + Send>) -> String {
    if let Some(s) = e.downcast_ref::<&str>() {
        (*s).to_string()
    } else if let Some(s) = e.downcast_ref::<String>() {
        s.clone()
    } else {
        "(不可读的 panic 负载)".to_string()
    }
}
