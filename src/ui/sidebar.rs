//! 左侧面板：磁盘整体使用率 + 按文件类型的分类统计。
//!
//! 纯展示模块，不产生任何用户交互动作，所以不需要返回值，
//! 也不需要知道 treemap/文件树那边发生了什么。

use egui::{Color32, CornerRadius, FontId, RichText, Vec2};

use crate::format::human_size;
use crate::model::CategoryStat;

pub fn show(
    ui: &mut egui::Ui,
    used_size: u64,
    total_size: u64,
    free_size: u64,
    categories: &[CategoryStat],
    file_system: &str,
) {
    egui::Panel::left("stats_panel")
        .resizable(false)
        .exact_size(230.0)
        .frame(
            egui::Frame::default()
                .fill(Color32::from_rgb(0x36, 0x36, 0x3A))
                .inner_margin(egui::Margin::same(14)),
        )
        .show(ui, |ui| {
            ui.label(RichText::new("磁盘概览").strong().size(15.0));
            if !file_system.is_empty() {
                ui.label(
                    RichText::new(format!("文件系统: {}", file_system))
                        .size(11.0)
                        .color(Color32::from_rgb(0xA0, 0xA0, 0xA0)),
                );
            }
            ui.add_space(10.0);

            let (rect, _) = ui.allocate_exact_size(Vec2::new(200.0, 90.0), egui::Sense::hover());
            let painter = ui.painter_at(rect);
            let used_ratio = if total_size > 0 { used_size as f32 / total_size as f32 } else { 0.0 };
            painter.rect_filled(rect, CornerRadius::same(10), Color32::from_rgb(0x42, 0x44, 0x48));
            let used_rect = egui::Rect::from_min_size(rect.min, Vec2::new(rect.width() * used_ratio, rect.height()));
            painter.rect_filled(used_rect, CornerRadius::same(10), Color32::from_rgb(0x4C, 0x8B, 0xF5));
            painter.text(
                rect.center(),
                egui::Align2::CENTER_CENTER,
                format!("{:.0}% 已用", used_ratio * 100.0),
                FontId::proportional(16.0),
                Color32::WHITE,
            );

            ui.add_space(16.0);
            ui.separator();
            ui.add_space(6.0);

            let cat_total: u64 = categories.iter().map(|c| c.size).sum::<u64>().max(1);
            for c in categories {
                let ratio = c.size as f32 / cat_total as f32;
                ui.horizontal(|ui| {
                    let (r, _) = ui.allocate_exact_size(Vec2::new(10.0, 10.0), egui::Sense::hover());
                    ui.painter().rect_filled(r, CornerRadius::same(2), c.color);
                    ui.label(RichText::new(c.label).size(12.5).color(Color32::from_rgb(0xE0, 0xE0, 0xE0)));
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        ui.label(RichText::new(human_size(c.size)).size(12.0).color(Color32::from_rgb(0xA0, 0xA0, 0xA0)));
                    });
                });
                let (bar_rect, _) = ui.allocate_exact_size(Vec2::new(200.0, 5.0), egui::Sense::hover());
                let bp = ui.painter_at(bar_rect);
                bp.rect_filled(bar_rect, CornerRadius::same(2), Color32::from_rgb(0x42, 0x44, 0x48));
                let filled = egui::Rect::from_min_size(bar_rect.min, Vec2::new(bar_rect.width() * ratio, bar_rect.height()));
                bp.rect_filled(filled, CornerRadius::same(2), c.color);
                ui.add_space(4.0);
            }

            ui.add_space(10.0);
            ui.separator();
            ui.add_space(8.0);
            ui.label(RichText::new(format!("剩余空间: {}", human_size(free_size))).color(Color32::from_rgb(0xA0, 0xA0, 0xA0)));
        });
}
