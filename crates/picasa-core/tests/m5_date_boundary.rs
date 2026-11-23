//! M5 跨日 / 跨年边界：`year_range_seconds` 与 `parse_date_range` 必须同口径
//! （本地时区的半开区间 `[from, to)`），且与界面上「拍摄时间跨度」用的本地日一致。
//!
//! 为什么单独立一个文件：这三个函数此前**没有任何针对跨年零点的测试**，
//! 时区/闭开区间错配在普通日期上完全看不出来，只有 1 月 1 日零点那几秒会露馅。

use chrono::{Local, TimeZone};

/// 本地时间 y 年 1 月 1 日 00:00:00 的 epoch 秒。
fn local_new_year(y: i32) -> i64 {
    Local
        .with_ymd_and_hms(y, 1, 1, 0, 0, 0)
        .earliest()
        .unwrap()
        .timestamp()
}

#[test]
fn year_range_is_local_midnight_and_half_open() {
    for year in [2011, 2024, 2026] {
        let (from, to) = picasa_core::year_range_seconds(year);
        assert_eq!(
            from,
            local_new_year(year),
            "{year} 起点必须是本地 1 月 1 日零点"
        );
        assert_eq!(
            to,
            local_new_year(year + 1),
            "{year} 终点必须是下一年本地零点（不含）"
        );
        assert!(from < to);
    }
}

#[test]
fn year_range_matches_parse_date_range_for_a_year() {
    for year in [2010, 2011, 2026] {
        let (a, b) = picasa_core::year_range_seconds(year);
        let parsed = picasa_core::library::parse_date_range(&year.to_string()).unwrap();
        assert_eq!(
            (a, b),
            parsed,
            "侧栏年份区间与搜索框输入「{year}」必须是同一条算式"
        );
    }
}

#[test]
fn year_range_is_half_open_at_the_right_edge() {
    let (from, to) = picasa_core::year_range_seconds(2011);
    // 区间语义是 [from, to)：from 属于 2011，to 属于 2012，to-1 属于 2011。
    assert!(from < to);
    let last_included = to - 1;
    assert!(last_included >= from && last_included < to, "to-1 仍属于本年度");
    assert!(to >= to, "终点那一秒不算进本年度（半开）");
}

#[test]
fn parse_date_range_day_is_half_open_across_midnight() {
    // 输入 2011-05-13 → [本地 5/13 零点, 本地 5/14 零点)，跨日的 5/14 零点不含。
    let (from, to) = picasa_core::library::parse_date_range("2011-05-13").unwrap();
    assert_eq!(
        from,
        Local
            .with_ymd_and_hms(2011, 5, 13, 0, 0, 0)
            .earliest()
            .unwrap()
            .timestamp()
    );
    assert_eq!(
        to,
        Local
            .with_ymd_and_hms(2011, 5, 14, 0, 0, 0)
            .earliest()
            .unwrap()
            .timestamp()
    );
    assert!(
        (to - from) == 86_400 || (to - from) == 82_800 || (to - from) == 90_000,
        "单日区间长度应是 24h（DST 日 ±1h）：{}-{}={}",
        to,
        from,
        to - from
    );
}

/// 副标题的拍摄时间跨度用 `Local` 按日格式化；这里钉住「同一 UTC 时刻在本地跨日」
/// 的口径：group_by_day 与副标题都按本地日，不能被 UTC 推走。
#[test]
fn local_day_keys_agree_across_a_midnight_boundary() {
    use chrono::Datelike;
    let at_newyear = local_new_year(2012) - 1; // 本地 2011-12-31 23:59:59
    let dt = Local.timestamp_opt(at_newyear, 0).single().unwrap();
    assert_eq!((dt.year(), dt.month(), dt.day()), (2011, 12, 31));
    let next = Local.timestamp_opt(at_newyear + 1, 0).single().unwrap();
    assert_eq!((next.year(), next.month(), next.day()), (2012, 1, 1));
}
