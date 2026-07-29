//! 通用格式化/文本测量工具。
//!
//! 从原来的单文件 main.rs 中原样抽出，不改变行为，
//! 只是让 UI 层各模块都能直接 `use crate::format::*;`。

use egui::{Color32, FontId};

/// 把字节数格式化成 "12.3 GB" 这种人类可读的字符串。
pub fn human_size(bytes: u64) -> String {
    let units = ["B", "KB", "MB", "GB", "TB"];
    let mut v = bytes as f64;
    let mut u = 0;
    while v >= 1024.0 && u < units.len() - 1 {
        v /= 1024.0;
        u += 1;
    }
    format!("{:.1} {}", v, units[u])
}

/// 紧凑版本：不带小数，"12GB" / "345MB"，用于磁盘行多行显示，避免换行。
pub fn human_size_compact(bytes: u64) -> String {
    let units = ["B", "K", "M", "G", "T"];
    let mut v = bytes as f64;
    let mut u = 0;
    while v >= 1024.0 && u < units.len() - 1 {
        v /= 1024.0;
        u += 1;
    }
    if u == 0 {
        format!("{}{}", v as u64, units[u])
    } else {
        format!("{:.0}{}", v, units[u])
    }
}

/// 把 Windows FILETIME（1601-01-01 起 100ns 单位）格式化成 "YYYY-MM-DD HH:MM"。
///
/// 不依赖 chrono / time 之类的外部 crate，自己用一份经典的日期算法换算。
/// 输入为 0 视为"未知"，返回空串（UI 上就会显示成 "—"）。
pub fn format_filetime(ft: u64) -> String {
    if ft == 0 {
        return String::new();
    }
    // FILETIME 是 1601-01-01 起的 100ns 计数；先折算到 1970-01-01 起的秒数。
    // 1601-01-01 到 1970-01-01 共 11644473600 秒。
    const FILETIME_UNIX_OFFSET_SECS: u64 = 11_644_473_600;
    let unix_100ns = ft / 10_000_000;
    if unix_100ns <= FILETIME_UNIX_OFFSET_SECS {
        return String::new();
    }
    let secs = unix_100ns - FILETIME_UNIX_OFFSET_SECS;

    let days = (secs / 86400) as i64;
    let secs_of_day = secs % 86400;
    let hour = secs_of_day / 3600;
    let min = (secs_of_day % 3600) / 60;

    let (year, month, day) = days_to_ymd(days);
    format!(
        "{:04}-{:02}-{:02} {:02}:{:02}",
        year, month, day, hour, min
    )
}

/// 把"自 1970-01-01 起的天数"换算成 (year, month, day)。
/// 算法来自 Howard Hinnant 的 date algorithms（civil_from_days）。
fn days_to_ymd(days: i64) -> (i64, u32, u32) {
    let z = days + 719_468; // 调到 0000-03-01 起算
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097; // [0, 146096]
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146_096) / 365; // [0, 399]
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100); // [0, 365]
    let mp = (5 * doy + 2) / 153; // [0, 11]
    let d = doy - (153 * mp + 2) / 5 + 1; // [1, 31]
    let m = if mp < 10 { mp + 3 } else { mp - 9 }; // [1, 12]
    let year = if m <= 2 { y + 1 } else { y };
    (year, m as u32, d as u32)
}

/// 把 Windows 文件属性位格式化成一个紧凑的字母串，类似 Unix 的 `rwx`：
///
/// - `R` ReadOnly         (0x01)
/// - `H` Hidden           (0x02)
/// - `S` System           (0x04)
/// - `D` Directory        (0x10)
/// - `A` Archive          (0x20)
/// - `N` Normal           (0x80)
/// - `T` Temporary        (0x100)
/// - `C` Compressed       (0x800)
/// - `I` NotContentIndexed(0x1000)
/// - `X` Encrypted        (0x4000)
///
/// 没有任何属性位（理论上不会发生）时返回 "—"。
pub fn format_attributes(attrs: u32) -> String {
    let mut s = String::with_capacity(8);
    if attrs & 0x01 != 0 {
        s.push('R');
    }
    if attrs & 0x02 != 0 {
        s.push('H');
    }
    if attrs & 0x04 != 0 {
        s.push('S');
    }
    if attrs & 0x10 != 0 {
        s.push('D');
    }
    if attrs & 0x20 != 0 {
        s.push('A');
    }
    if attrs & 0x80 != 0 {
        s.push('N');
    }
    if attrs & 0x100 != 0 {
        s.push('T');
    }
    if attrs & 0x800 != 0 {
        s.push('C');
    }
    if attrs & 0x1000 != 0 {
        s.push('I');
    }
    if attrs & 0x4000 != 0 {
        s.push('X');
    }
    if s.is_empty() {
        "—".into()
    } else {
        s
    }
}

/// 按可用宽度截断文字，超出部分用省略号代替，而不是让文字溢出块外。
pub fn truncate_text(ctx: &egui::Context, text: &str, font: FontId, max_width: f32) -> String {
    let measure = |s: &str| -> f32 {
        ctx.fonts_mut(|f| f.layout_no_wrap(s.to_owned(), font.clone(), Color32::WHITE).size().x)
    };
    if measure(text) <= max_width {
        return text.to_owned();
    }
    let mut truncated = String::new();
    for ch in text.chars() {
        let candidate = format!("{truncated}{ch}…");
        if measure(&candidate) > max_width {
            break;
        }
        truncated.push(ch);
    }
    if truncated.is_empty() {
        String::new()
    } else {
        format!("{truncated}…")
    }
}
