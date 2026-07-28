//! DiskLens 入口。
//!
//! 只负责两件事：加载中文字体、创建窗口并启动 `DiskUiApp`。
//! 具体的界面/状态/算法都拆到各自的模块里了，参见各文件顶部的说明：
//! - `model`      递归树数据结构
//! - `scan`       后台扫描线程 + 演示数据
//! - `categorize` 按类型统计
//! - `treemap`    squarified treemap 几何算法
//! - `format`     格式化/文本测量小工具
//! - `app`        顶层状态编排
//! - `ui::*`      各个具体面板/视图

mod app;
mod categorize;
mod format;
mod model;
mod scan;
mod treemap;
mod ui;

use app::DiskUiApp;

/// 尝试加载系统里常见的中文字体，覆盖 Windows / Linux / macOS 几种常见路径。
/// 一个都找不到时不会报错，只是退回 egui 内置字体（不含中文字形）。
fn setup_fonts(ctx: &egui::Context) {
    const CANDIDATES: &[&str] = &[
        r"C:\Windows\Fonts\msyh.ttc",       // Windows 微软雅黑
        r"C:\Windows\Fonts\simhei.ttf",     // Windows 黑体（备选）
        "/usr/share/fonts/truetype/noto/NotoSansCJK-Regular.ttc", // 常见 Linux 发行版
        "/usr/share/fonts/opentype/noto/NotoSansCJK-Regular.ttc",
        "/System/Library/Fonts/PingFang.ttc", // macOS
    ];

    let Some(data) = CANDIDATES.iter().find_map(|p| std::fs::read(p).ok()) else {
        return;
    };

    let mut fonts = egui::FontDefinitions::default();
    fonts.font_data.insert("cjk".to_owned(), egui::FontData::from_owned(data));
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
