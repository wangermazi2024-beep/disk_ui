//! 顶部工具栏：路径输入、扫描按钮、扫描中的进度提示。
//!
//! 这个模块只负责"画出来 + 汇报用户点了扫描按钮"，
//! 真正发起后台扫描线程的逻辑留在 `app.rs`，模块本身不持有 channel/线程状态，
//! 保持职责单一。

use egui::{Color32, CornerRadius, RichText};

use crate::format::human_size;

pub enum TopbarAction {
    None,
    StartScan,
}

pub struct TopbarState<'a> {
    pub root_path: &'a mut String,
    pub scanning: bool,
    pub scanned_count: u64,
    pub scan_error: Option<&'a str>,
    pub used_size: u64,
    pub total_size: u64,
}

pub fn show(ui: &mut egui::Ui, state: TopbarState) -> TopbarAction {
    let mut action = TopbarAction::None;

    egui::Panel::top("top_bar")
        .exact_size(56.0)
        .frame(
            egui::Frame::default()
                .fill(Color32::from_rgb(0x2D, 0x2D, 0x30))
                .inner_margin(egui::Margin::symmetric(16, 8)),
        )
        .show(ui, |ui| {
            ui.horizontal_centered(|ui| {
                ui.label(RichText::new("⛁ DiskLens").size(18.0).strong().color(Color32::from_rgb(0x6F, 0xA8, 0xFF)));
                ui.add_space(16.0);
                ui.label(RichText::new("路径:").color(Color32::from_rgb(0xC8, 0xC8, 0xC8)));
                ui.add(egui::TextEdit::singleline(state.root_path).desired_width(220.0));
                if ui
                    .add(
                        egui::Button::new(RichText::new("  扫描  ").color(Color32::WHITE))
                            .fill(Color32::from_rgb(0x4C, 0x8B, 0xF5))
                            .corner_radius(CornerRadius::same(6)),
                    )
                    .clicked()
                {
                    action = TopbarAction::StartScan;
                }

                if state.scanning {
                    ui.add(egui::Spinner::new());
                    ui.label(
                        RichText::new(format!("正在扫描… 已发现 {} 项", state.scanned_count))
                            .color(Color32::from_rgb(0xA0, 0xA0, 0xA0)),
                    );
                } else if let Some(err) = state.scan_error {
                    ui.label(RichText::new(err).color(Color32::from_rgb(0xE0, 0x55, 0x5B)));
                }

                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    ui.label(
                        RichText::new(format!(
                            "已用 {} / 共 {}",
                            human_size(state.used_size),
                            human_size(state.total_size)
                        ))
                        .color(Color32::from_rgb(0xA0, 0xA0, 0xA0)),
                    );
                });
            });
        });

    action
}
