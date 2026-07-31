//! DiskLens 入口（v2 WinDirStat 风格重构）。

pub use disk_ui::{applog, categorize, disk_info, export, format, model, scan};
#[cfg(windows)]
pub use disk_ui::mft_scan;

mod app;
mod ui;
use app::DiskUiApp;

fn setup_fonts(ctx: &egui::Context) {
    // 以前是写死几个候选路径（C:\Windows\Fonts\msyh.ttc 等），在精简版 Windows / WinPE / Server Core
    // 上可能路径不一致或字体被精简。现在用 GetWindowsDirectoryW 动态拿系统目录，
    // 拼出 Fonts 路径，再试几个常见中文字体。找不到不静默退出一一遇零返回，
    // 至少保留 egui 默认字体（中文会变方块，但不会崩溃，并打日志提醒）。
    let candidates: Vec<String> = windows_fonts_dir()
        .into_iter()
        .flat_map(|dir| {
            vec![
                format!("{}\\msyh.ttc", dir),    // 微软雅黑
                format!("{}\\msyhbd.ttc", dir),   // 微软雅黑粗体
                format!("{}\\simhei.ttf", dir),  // 黑体
                format!("{}\\simsun.ttc", dir),  // 宋体
                format!("{}\\Deng.ttf", dir),    // 等线
            ]
        })
        .chain([
            "/usr/share/fonts/truetype/noto/NotoSansCJK-Regular.ttc".to_string(),
            "/usr/share/fonts/opentype/noto/NotoSansCJK-Regular.ttc".to_string(),
            "/System/Library/Fonts/PingFang.ttc".to_string(),
        ])
        .collect();

    let Some(data) = candidates.iter().find_map(|p| std::fs::read(p).ok()) else {
        eprintln!("[font] 未找到中文字体，中文将显示为方块。尝试过的路径:");
        for p in &candidates { eprintln!("  - {}", p); }
        return;
    };
    let mut fonts = egui::FontDefinitions::default();
    fonts.font_data.insert("cjk".to_owned(), egui::FontData::from_owned(data).into());
    fonts.families.entry(egui::FontFamily::Proportional).or_default().insert(0, "cjk".to_owned());
    fonts.families.entry(egui::FontFamily::Monospace).or_default().push("cjk".to_owned());
    ctx.set_fonts(fonts);
}

/// 用 GetWindowsDirectoryW 拿 Windows 目录（通常是 C:\Windows），再拼出 Fonts 子目录。
/// 拿不到就返回空 Vec，上层会退化到写死的候选路径。
#[cfg(windows)]
fn windows_fonts_dir() -> Vec<String> {
    use windows_sys::Win32::System::SystemInformation::GetWindowsDirectoryW;
    let mut buf = [0u16; 260];
    let len = unsafe { GetWindowsDirectoryW(buf.as_mut_ptr(), buf.len() as u32) };
    if len == 0 || len as usize >= buf.len() {
        return Vec::new();
    }
    let win_dir = String::from_utf16_lossy(&buf[..len as usize]);
    vec![format!("{}\\Fonts", win_dir)]
}

#[cfg(not(windows))]
fn windows_fonts_dir() -> Vec<String> { Vec::new() }

fn main() -> eframe::Result<()> {
    applog::init();
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default().with_inner_size([1200.0, 750.0]),
        ..Default::default()
    };
    eframe::run_native("DiskLens v2 (WinDirStat 风格)", options, Box::new(|cc| {
        setup_fonts(&cc.egui_ctx);
        Ok(Box::new(DiskUiApp::default()))
    }))
}
