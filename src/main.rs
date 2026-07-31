//! DiskLens 入口（v2 WinDirStat 风格重构）。

pub use disk_ui::{applog, categorize, disk_info, export, format, model, scan};
#[cfg(windows)]
pub use disk_ui::mft_scan;

mod app;
mod ui;
use app::DiskUiApp;

fn setup_fonts(ctx: &egui::Context) {
    // 优先用 %SystemRoot% 环境变量拼字体目录，而不是写死 "C:\Windows"——
    // 系统盘不一定是 C 盘（企业环境、多系统机器上很常见装在别的盘）。
    // 环境变量拿不到时才退回 C:\Windows 这个绝大多数机器上成立的默认值。
    let system_root = std::env::var("SystemRoot").unwrap_or_else(|_| r"C:\Windows".to_string());
    let dynamic_candidates = [
        format!(r"{system_root}\Fonts\msyh.ttc"),
        format!(r"{system_root}\Fonts\msyh.ttf"),
        format!(r"{system_root}\Fonts\simhei.ttf"),
        format!(r"{system_root}\Fonts\simsun.ttc"),
    ];
    const STATIC_CANDIDATES: &[&str] = &[
        "/usr/share/fonts/truetype/noto/NotoSansCJK-Regular.ttc",
        "/usr/share/fonts/opentype/noto/NotoSansCJK-Regular.ttc",
        "/System/Library/Fonts/PingFang.ttc",
    ];
    let found = dynamic_candidates.iter().map(|s| s.as_str())
        .chain(STATIC_CANDIDATES.iter().copied())
        .find_map(|p| std::fs::read(p).ok().map(|data| (p, data)));
    let Some((path, data)) = found else {
        // 找不到任何中文字体：不能让它在没有任何提示的情况下悄悄退化成方块字。
        // 换电脑（精简版 Windows / Server Core / 没装东亚语言包）复现"中文显示不出来"的
        // bug 报告时，这行日志能直接告诉你是不是这个原因。
        eprintln!("[main] 未找到可用的中文字体，界面中文可能显示为方块（已尝试: {:?} + Linux/macOS 候选路径）", dynamic_candidates);
        return;
    };
    eprintln!("[main] 使用中文字体: {path}");
    let mut fonts = egui::FontDefinitions::default();
    fonts.font_data.insert("cjk".to_owned(), egui::FontData::from_owned(data).into());
    fonts.families.entry(egui::FontFamily::Proportional).or_default().insert(0, "cjk".to_owned());
    fonts.families.entry(egui::FontFamily::Monospace).or_default().push("cjk".to_owned());
    ctx.set_fonts(fonts);
}

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
