//! 格式化工具：字节大小、FILETIME（本地时区）、属性位（R/H/S/A/C）。

use egui::{Color32, FontId};

/// "12.3 GB" 格式。
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

/// 紧凑版 "12G" / "345M"，用于磁盘行多行显示。
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

/// FILETIME → "YYYY-MM-DD HH:MM"（本地时区，调用系统 API）。
pub fn format_filetime(ft: u64) -> String {
    if ft == 0 {
        return String::new();
    }
    const FILETIME_UNIX_OFFSET_SECS: u64 = 11_644_473_600;
    let unix_100ns = ft / 10_000_000;
    if unix_100ns < FILETIME_UNIX_OFFSET_SECS {
        return String::new();
    }
    let secs = unix_100ns - FILETIME_UNIX_OFFSET_SECS;
    let secs = secs.wrapping_add(local_tz_offset_secs() as u64);

    let days = (secs / 86400) as i64;
    let secs_of_day = secs % 86400;
    let hour = secs_of_day / 3600;
    let min = (secs_of_day % 3600) / 60;
    let (y, m, d) = days_to_ymd(days);
    format!("{:04}-{:02}-{:02} {:02}:{:02}", y, m, d, hour, min)
}

fn local_tz_offset_secs() -> i64 {
    #[cfg(windows)]
    {
        use windows_sys::Win32::System::Time::{GetTimeZoneInformation, TIME_ZONE_INFORMATION};
        unsafe {
            let mut tzi: TIME_ZONE_INFORMATION = std::mem::zeroed();
            let r = GetTimeZoneInformation(&mut tzi);
            let bias = tzi.Bias as i64;
            let offset = if r == 2 {
                bias + tzi.DaylightBias as i64
            } else {
                bias + tzi.StandardBias as i64
            };
            -(offset) * 60
        }
    }
    #[cfg(not(windows))]
    {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs() as i64)
            .unwrap_or(0);
        unsafe {
            let mut tm: LibcTm = std::mem::zeroed();
            localtime_r(&now, &mut tm);
            tm.tm_gmtoff as i64
        }
    }
}

#[cfg(not(windows))]
#[repr(C)]
struct LibcTm {
    tm_sec: i32, tm_min: i32, tm_hour: i32, tm_mday: i32,
    tm_mon: i32, tm_year: i32, tm_wday: i32, tm_yday: i32,
    tm_isdst: i32, tm_gmtoff: i64, tm_zone: *const u8,
}
#[cfg(not(windows))]
unsafe extern "C" {
    fn localtime_r(time: *const i64, result: *mut LibcTm) -> *mut LibcTm;
}

fn days_to_ymd(days: i64) -> (i64, u32, u32) {
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = if m <= 2 { y + 1 } else { y };
    (year, m as u32, d as u32)
}

/// 属性位 → "RHSA" 字符串（WinDirStat 风格，顺序 R,H,S,A,C）。
pub fn format_attributes(attrs: u32) -> String {
    let mut s = String::with_capacity(8);
    if attrs & 0x01 != 0 { s.push('R'); }
    if attrs & 0x02 != 0 { s.push('H'); }
    if attrs & 0x04 != 0 { s.push('S'); }
    if attrs & 0x20 != 0 { s.push('A'); }
    if attrs & 0x800 != 0 { s.push('C'); }
    if s.is_empty() { "—".into() } else { s }
}

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
    if truncated.is_empty() { String::new() } else { format!("{truncated}…") }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_human_size() {
        assert_eq!(human_size(0), "0.0 B");
        assert_eq!(human_size(1024), "1.0 KB");
        assert_eq!(human_size(1024 * 1024 * 1024), "1.0 GB");
    }

    #[test]
    fn test_format_attributes() {
        assert_eq!(format_attributes(0), "—");
        assert_eq!(format_attributes(0x80), "—"); // NORMAL 不显示
        assert_eq!(format_attributes(0x02 | 0x04), "HS");
        assert_eq!(format_attributes(0x02 | 0x04 | 0x20), "HSA");
        assert_eq!(format_attributes(0x01), "R");
    }
}
