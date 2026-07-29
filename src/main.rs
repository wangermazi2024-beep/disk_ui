//! DiskLens 入口。
//!
//! 只负责两件事：加载中文字体、创建窗口并启动 `DiskUiApp`。
//!
//! 纯逻辑模块（model / scan / format / categorize / disk_info / mft_parse /
//! mft_scan）放在 `lib.rs` 里，作为 lib target 暴露给 `src/bin/verify_mft.rs`
//! 和单元测试使用，这样它们可以在没有 eframe 的环境下也编过 / 跑测试。
//! 本文件（main.rs）只保留 GUI 专有的 `app` / `ui` / `treemap` 模块。

// 把 lib 暴露的模块重新引入到 bin 的 crate 命名空间，
// 这样 `mod app` 里的代码可以继续用 `crate::model::Node` 之类的路径。
pub use disk_ui::{
    categorize, disk_info, format, mft_parse, model, scan,
};
#[cfg(windows)]
pub use disk_ui::mft_scan;

mod app;
mod treemap;
mod ui;

use app::DiskUiApp;

/// 尝试加载系统里常见的中文字体，覆盖 Windows / Linux / macOS 几种常见路径。
fn setup_fonts(ctx: &egui::Context) {
    const CANDIDATES: &[&str] = &[
        r"C:\Windows\Fonts\msyh.ttc",
        r"C:\Windows\Fonts\simhei.ttf",
        "/usr/share/fonts/truetype/noto/NotoSansCJK-Regular.ttc",
        "/usr/share/fonts/opentype/noto/NotoSansCJK-Regular.ttc",
        "/System/Library/Fonts/PingFang.ttc",
    ];

    let Some(data) = CANDIDATES.iter().find_map(|p| std::fs::read(p).ok()) else {
        return;
    };

    let mut fonts = egui::FontDefinitions::default();
    fonts.font_data.insert("cjk".to_owned(), egui::FontData::from_owned(data).into());
    fonts.families.entry(egui::FontFamily::Proportional).or_default().insert(0, "cjk".to_owned());
    fonts.families.entry(egui::FontFamily::Monospace).or_default().push("cjk".to_owned());
    ctx.set_fonts(fonts);
}

fn main() -> eframe::Result<()> {
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default().with_inner_size([980.0, 680.0]),
        ..Default::default()
    };
    eframe::run_native(
        "DiskLens - Rust Disk Analyzer",
        options,
        Box::new(|cc| {
            setup_fonts(&cc.egui_ctx);
            Ok(Box::new(DiskUiApp::default()))
        }),
    )
}
