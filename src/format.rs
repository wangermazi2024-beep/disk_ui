//! 通用格式化/文本测量工具。

use egui::{Color32, FontId};

/// 把一个 UTC FILETIME 转换成"本地时区"的 FILETIME。
///
/// **之前的 bug**：`format_filetime` 直接把 FILETIME（UTC）当成本地时间来拆年月日时分，
/// 导致显示的修改时间跟 UTC 差了一个时区偏移（比如东八区会显示成比实际早 8 小时）。
///
/// 这里调用 Windows API `FileTimeToLocalFileTime`，它会查系统当前的时区设置
/// （包括夏令时规则），而不是自己写死一个固定偏移量——固定偏移在有夏令时的地区
/// 会算错，而且用户系统改了时区这段代码也不用跟着改。
#[cfg(windows)]
fn filetime_to_local(ft: u64) -> u64 {
    use windows_sys::Win32::Foundation::FILETIME;
    use windows_sys::Win32::Storage::FileSystem::FileTimeToLocalFileTime;
    if ft == 0 {
        return 0;
    }
    let utc = FILETIME {
        dwLowDateTime: (ft & 0xFFFF_FFFF) as u32,
        dwHighDateTime: (ft >> 32) as u32,
    };
    let mut local = FILETIME {
        dwLowDateTime: 0,
        dwHighDateTime: 0,
    };
    let ok = unsafe { FileTimeToLocalFileTime(&utc, &mut local) };
    if ok == 0 {
        // 转换失败（极少见），退回 UTC 时间总比崩溃/显示垃圾值强。
        return ft;
    }
    ((local.dwHighDateTime as u64) << 32) | (local.dwLowDateTime as u64)
}

/// 非 Windows 平台（单元测试/开发机）没有系统时区 API 可调，原样返回（等价于 UTC）。
/// 这个 crate 的实际运行环境（打包出去的 exe）永远是 Windows，所以这个分支
/// 只影响本地跑 `cargo test` 时的行为，不影响最终用户看到的时间。
#[cfg(not(windows))]
fn filetime_to_local(ft: u64) -> u64 {
    ft
}

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
    const FILETIME_UNIX_OFFSET_SECS: u64 = 11_644_473_600;
    let unix_100ns = ft / 10_000_000;
    if unix_100ns < FILETIME_UNIX_OFFSET_SECS {
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

/// 把 Windows FILETIME（UTC）格式化成本地时区的 "YYYY-MM-DD HH:MM"。
///
/// UI 显示修改时间应该用这个函数，而不是直接用 `format_filetime`（那个是纯 UTC，
/// 只在测试里验证换算算法本身对不对用）。
pub fn format_filetime_local(ft: u64) -> String {
    format_filetime(filetime_to_local(ft))
}

/// 把"自 1970-01-01 起的天数"换算成 (year, month, day)。
/// 算法来自 Howard Hinnant 的 date algorithms（civil_from_days）。
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
pub fn format_attributes(attrs: u32) -> String {
    let mut s = String::with_capacity(8);
    if attrs & 0x01 != 0 { s.push('R'); }
    if attrs & 0x02 != 0 { s.push('H'); }
    if attrs & 0x04 != 0 { s.push('S'); }
    if attrs & 0x10 != 0 { s.push('D'); }
    if attrs & 0x20 != 0 { s.push('A'); }
    if attrs & 0x80 != 0 { s.push('N'); }
    if attrs & 0x100 != 0 { s.push('T'); }
    if attrs & 0x800 != 0 { s.push('C'); }
    if attrs & 0x1000 != 0 { s.push('I'); }
    if attrs & 0x4000 != 0 { s.push('X'); }
    if s.is_empty() {
        "—".into()
    } else {
        s
    }
}

/// 按可用宽度截断文字，超出部分用省略号代替。
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_human_size_bytes() {
        assert_eq!(human_size(0), "0.0 B");
        assert_eq!(human_size(1), "1.0 B");
        assert_eq!(human_size(1023), "1023.0 B");
    }

    #[test]
    fn test_human_size_kb_mb_gb() {
        assert_eq!(human_size(1024), "1.0 KB");
        assert_eq!(human_size(1024 * 1024), "1.0 MB");
        assert_eq!(human_size(1024 * 1024 * 1024), "1.0 GB");
        assert_eq!(human_size(1024_u64.pow(4)), "1.0 TB");
    }

    #[test]
    fn test_human_size_compact() {
        assert_eq!(human_size_compact(0), "0B");
        assert_eq!(human_size_compact(1024), "1K");
        assert_eq!(human_size_compact(1024 * 1024), "1M");
        assert_eq!(human_size_compact(1024 * 1024 * 1024), "1G");
        assert_eq!(human_size_compact(1024_u64.pow(4)), "1T");
    }

    #[test]
    fn test_format_filetime_zero() {
        assert_eq!(format_filetime(0), "");
    }

    #[test]
    fn test_format_filetime_unix_epoch() {
        let ft = 11_644_473_600u64 * 10_000_000;
        let s = format_filetime(ft);
        assert!(s.starts_with("1970-01-01"), "got: {}", s);
    }

    #[test]
    fn test_format_filetime_known_date() {
        // 2024-01-15 10:30:00 UTC, Unix ts = 1705314600
        let ft = 13_349_788_200u64 * 10_000_000;
        let s = format_filetime(ft);
        assert_eq!(s, "2024-01-15 10:30");
    }

    #[test]
    fn test_format_attributes_empty() {
        assert_eq!(format_attributes(0), "—");
    }

    #[test]
    fn test_format_attributes_normal() {
        assert_eq!(format_attributes(0x80), "N");
    }

    #[test]
    fn test_format_attributes_directory() {
        assert_eq!(format_attributes(0x10), "D");
    }

    #[test]
    fn test_format_attributes_system_archive() {
        let s = format_attributes(0x24);
        assert!(s.contains('S'), "should contain S: {}", s);
        assert!(s.contains('A'), "should contain A: {}", s);
        assert!(!s.contains('D'), "should not contain D: {}", s);
    }

    #[test]
    fn test_format_attributes_hidden_system_directory() {
        let s = format_attributes(0x16);
        assert!(s.contains('H'));
        assert!(s.contains('S'));
        assert!(s.contains('D'));
    }
}
