//! EXIF **只读**探测。
//!
//! M1 只读 EXIF：Orientation（决定显示方向）、拍摄时间（决定排序与日期分组）、
//! 以及查看器信息面板要展示的拍摄参数（F2-09）。
//! **不写任何 EXIF**——`photos` 表里也没有「待写入」的字段（02 §5.6 的数据边界）。

use crate::{Result, decode};
use chrono::{Local, NaiveDateTime, TimeZone};
use exif::{In, Tag, Value};
use std::path::Path;

/// 单张照片的探测结果。**全部字段都是「文件里本来就有的东西」**，没有任何用户状态。
#[derive(Clone, Debug, Default)]
pub struct Probe {
    /// 原始像素宽（**未按 Orientation 摆正**）。
    pub width: u32,
    /// 原始像素高。
    pub height: u32,
    /// EXIF Orientation 1..8，缺失记 1。
    pub orientation: u32,
    /// EXIF 里的拍摄时间（Unix 秒）。`None` 表示文件里没有。
    pub exif_taken_at: Option<i64>,
    pub summary: ExifSummary,
}

/// 查看器信息面板（F2-09）要显示的字段。
///
/// 全部 `Option`：**缺失就是缺失**，UI 显示「—」，绝不编造。
#[derive(Clone, Debug, Default)]
pub struct ExifSummary {
    pub make: Option<String>,
    pub model: Option<String>,
    pub lens: Option<String>,
    pub focal_length: Option<String>,
    pub f_number: Option<String>,
    pub exposure_time: Option<String>,
    pub iso: Option<String>,
    pub taken_at_text: Option<String>,
    pub color_space: Option<String>,
    pub gps: Option<String>,
}

/// 只读探测一张照片。
///
/// 任何一步失败都**降级而不是报错**：EXIF 读不出来不代表照片不能用。
pub fn probe(path: &Path) -> Result<Probe> {
    let exif = read_exif(path);

    let mut p = Probe {
        orientation: 1,
        ..Default::default()
    };

    if let Some(exif) = exif.as_ref() {
        p.orientation = exif
            .get_field(Tag::Orientation, In::PRIMARY)
            .and_then(|f| f.value.get_uint(0))
            .filter(|o| (1..=8).contains(o))
            .unwrap_or(1);
        p.summary = summarize(exif);
        p.exif_taken_at = exif_taken_at(exif);
    }

    // 像素尺寸：优先 EXIF 的 PixelXDimension/YDimension，其次读文件头，
    // 最后才全解码（顺序按代价从低到高排，避免为拿尺寸解码 24MP）。
    let exif_dims = exif.as_ref().and_then(|e| {
        let w = e
            .get_field(Tag::PixelXDimension, In::PRIMARY)?
            .value
            .get_uint(0)?;
        let h = e
            .get_field(Tag::PixelYDimension, In::PRIMARY)?
            .value
            .get_uint(0)?;
        if w > 0 && h > 0 { Some((w, h)) } else { None }
    });
    match exif_dims {
        Some((w, h)) => {
            p.width = w;
            p.height = h;
        }
        None => match decode::probe_dimensions(path) {
            Ok((w, h)) => {
                p.width = w;
                p.height = h;
            }
            Err(e) => {
                // 尺寸拿不到不算致命：栅格用占位图，仍可浏览。
                tracing::debug!(path = %path.display(), error = %e, "无法探测像素尺寸");
            }
        },
    }

    Ok(p)
}

/// 只读 Orientation（缩略图路径只需要它，读 EXIF 比全解码便宜得多）。
pub fn read_orientation(path: &Path) -> Option<u32> {
    let exif = read_exif(path)?;
    exif.get_field(Tag::Orientation, In::PRIMARY)
        .and_then(|f| f.value.get_uint(0))
        .filter(|o| (1..=8).contains(o))
}

fn read_exif(path: &Path) -> Option<exif::Exif> {
    let file = std::fs::File::open(path).ok()?;
    let mut reader = std::io::BufReader::new(&file);
    exif::Reader::new().read_from_container(&mut reader).ok()
}

fn exif_taken_at(exif: &exif::Exif) -> Option<i64> {
    for tag in [Tag::DateTimeOriginal, Tag::DateTimeDigitized, Tag::DateTime] {
        if let Some(f) = exif.get_field(tag, In::PRIMARY)
            && let Value::Ascii(ref v) = f.value
            && let Some(bytes) = v.first()
            && let Ok(s) = std::str::from_utf8(bytes)
            && let Some(ts) = parse_exif_datetime(s.trim_matches('\0').trim())
        {
            return Some(ts);
        }
    }
    None
}

/// 解析 `YYYY:MM:DD HH:MM:SS`。
///
/// EXIF 不带时区，按**本地时区**解释——这与「用户在自己电脑上看这张照片是哪天拍的」一致，
/// 也是 Picasa 的行为。跨时区旅行拍摄的照片会偏，这是 EXIF 格式本身的缺陷，不是我们的 bug。
pub fn parse_exif_datetime(s: &str) -> Option<i64> {
    let naive = NaiveDateTime::parse_from_str(s, "%Y:%m:%d %H:%M:%S")
        .or_else(|_| NaiveDateTime::parse_from_str(s, "%Y-%m-%d %H:%M:%S"))
        .ok()?;
    // 单一时区映射失败（夏令时切换的空洞）时退回 UTC，而不是丢掉时间。
    match Local.from_local_datetime(&naive).single() {
        Some(dt) => Some(dt.timestamp()),
        None => Some(naive.and_utc().timestamp()),
    }
}

fn summarize(exif: &exif::Exif) -> ExifSummary {
    let text = |tag: Tag| -> Option<String> {
        exif.get_field(tag, In::PRIMARY)
            .map(|f| f.display_value().with_unit(exif).to_string())
            .filter(|s| !s.trim().is_empty())
    };
    let ascii = |tag: Tag| -> Option<String> {
        match exif.get_field(tag, In::PRIMARY).map(|f| &f.value) {
            Some(Value::Ascii(v)) => v
                .first()
                .and_then(|b| std::str::from_utf8(b).ok())
                .map(|s| s.trim_matches('\0').trim().to_string())
                .filter(|s| !s.is_empty()),
            _ => None,
        }
    };

    ExifSummary {
        make: ascii(Tag::Make),
        model: ascii(Tag::Model),
        lens: ascii(Tag::LensModel).or_else(|| ascii(Tag::LensSpecification)),
        focal_length: text(Tag::FocalLength),
        f_number: text(Tag::FNumber),
        exposure_time: text(Tag::ExposureTime),
        iso: text(Tag::PhotographicSensitivity),
        taken_at_text: ascii(Tag::DateTimeOriginal).or_else(|| ascii(Tag::DateTime)),
        color_space: text(Tag::ColorSpace),
        gps: gps_text(exif),
    }
}

fn gps_text(exif: &exif::Exif) -> Option<String> {
    let lat = exif.get_field(Tag::GPSLatitude, In::PRIMARY)?;
    let lat_ref = exif.get_field(Tag::GPSLatitudeRef, In::PRIMARY);
    let lon = exif.get_field(Tag::GPSLongitude, In::PRIMARY)?;
    let lon_ref = exif.get_field(Tag::GPSLongitudeRef, In::PRIMARY);

    let dms = |v: &Value| -> Option<String> {
        // GPS 度分秒是三个 RATIONAL（d/1, m/1, s/100）
        let nums: Vec<f64> = match v {
            Value::Rational(r) => r.iter().take(3).map(|x| x.to_f64()).collect(),
            _ => return None,
        };
        if nums.len() == 3 {
            Some(format!("{:.0}°{:.0}'{:.1}\"", nums[0], nums[1], nums[2]))
        } else {
            None
        }
    };
    let ref_of = |f: Option<&exif::Field>| -> String {
        match f.map(|f| &f.value) {
            Some(Value::Ascii(v)) => v
                .first()
                .and_then(|b| std::str::from_utf8(b).ok())
                .map(|s| s.trim_matches('\0').trim().to_string())
                .unwrap_or_default(),
            _ => String::new(),
        }
    };

    let lat_txt = format!("{}{}", dms(&lat.value)?, ref_of(lat_ref));
    let lon_txt = format!("{}{}", dms(&lon.value)?, ref_of(lon_ref));
    Some(format!("{lat_txt} {lon_txt}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_standard_exif_datetime() {
        assert!(parse_exif_datetime("2011:05:13 10:20:30").is_some());
    }

    #[test]
    fn rejects_garbage_datetime() {
        assert!(parse_exif_datetime("not a date").is_none());
        assert!(parse_exif_datetime("0000:00:00 00:00:00").is_none());
    }

    #[test]
    fn missing_exif_does_not_panic() {
        let p = probe(Path::new("/definitely/not/here.jpg")).unwrap();
        // 文件不存在时：orientation 回落 1、尺寸 0、summary 全 None。
        assert_eq!(p.orientation, 1);
        assert_eq!((p.width, p.height), (0, 0));
        assert!(p.summary.make.is_none());
    }
}
