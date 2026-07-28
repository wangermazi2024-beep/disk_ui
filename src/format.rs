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
