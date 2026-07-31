//! 左侧面板：磁盘概览 + 分类统计。

use egui::{Color32, CornerRadius, FontId, RichText, Vec2};
use crate::format::human_size;
use crate::model::CategoryStat;

pub fn show(
    ui: &mut egui::Ui, used_size: u64, total_size: u64, free_size: u64,
    categories: &[CategoryStat], file_system: &str,
    scanned_logical: u64, scanned_physical: u64, scanned_files: u64, scanned_folders: u64,
) {
    egui::Panel::left("stats_panel").resizable(false).exact_size(230.0)
        .frame(egui::Frame::default().fill(Color32::from_rgb(0x36, 0x36, 0x3A))
            .inner_margin(egui::Margin::same(14)))
        .show(ui, |ui| {
            ui.label(RichText::new("磁盘概览").strong().size(15.0));
            if !file_system.is_empty() {
                ui.label(RichText::new(format!("文件系统: {}", file_system)).size(11.0).color(Color32::from_rgb(0xA0, 0xA0, 0xA0)));
            }
            ui.add_space(10.0);
            let (rect, _) = ui.allocate_exact_size(Vec2::new(200.0, 90.0), egui::Sense::hover());
            let painter = ui.painter_at(rect);
            let ratio = if total_size > 0 { used_size as f32 / total_size as f32 } else { 0.0 };
            painter.rect_filled(rect, CornerRadius::same(10), Color32::from_rgb(0x42, 0x44, 0x48));
            painter.rect_filled(egui::Rect::from_min_size(rect.min, Vec2::new(rect.width()*ratio, rect.height())), CornerRadius::same(10), Color32::from_rgb(0x4C, 0x8B, 0xF5));
            painter.text(rect.center(), egui::Align2::CENTER_CENTER, format!("{:.0}% 已用", ratio*100.0), FontId::proportional(16.0), Color32::WHITE);
            ui.add_space(16.0); ui.separator(); ui.add_space(6.0);

            let cat_total: u64 = categories.iter().map(|c| c.size).sum::<u64>().max(1);
            for c in categories {
                let r = c.size as f32 / cat_total as f32;
                ui.horizontal(|ui| {
                    let (rr, _) = ui.allocate_exact_size(Vec2::new(10.0, 10.0), egui::Sense::hover());
                    ui.painter().rect_filled(rr, CornerRadius::same(2), c.color);
                    ui.label(RichText::new(c.label).size(12.5).color(Color32::from_rgb(0xE0, 0xE0, 0xE0)));
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        ui.label(RichText::new(human_size(c.size)).size(12.0).color(Color32::from_rgb(0xA0, 0xA0, 0xA0)));
                    });
                });
                let (br, _) = ui.allocate_exact_size(Vec2::new(200.0, 5.0), egui::Sense::hover());
                let bp = ui.painter_at(br);
                bp.rect_filled(br, CornerRadius::same(2), Color32::from_rgb(0x42, 0x44, 0x48));
                bp.rect_filled(egui::Rect::from_min_size(br.min, Vec2::new(br.width()*r, br.height())), CornerRadius::same(2), c.color);
                ui.add_space(4.0);
            }
            ui.add_space(10.0); ui.separator(); ui.add_space(8.0);
            ui.label(RichText::new("空间统计").strong().size(12.0).color(Color32::from_rgb(0xE0, 0xE0, 0xE0)));
            ui.label(RichText::new(format!("逻辑大小: {}", human_size(scanned_logical))).size(11.5).color(Color32::from_rgb(0x4C, 0x8B, 0xF5)));
            ui.label(RichText::new(format!("物理大小: {}", human_size(scanned_physical))).size(11.5).color(Color32::from_rgb(0xF5, 0xA6, 0x23)));
            ui.label(RichText::new(format!("系统已用: {}", human_size(used_size))).size(11.0).color(Color32::from_rgb(0xA0, 0xA0, 0xA0)));
            ui.label(RichText::new(format!("剩余空间: {}", human_size(free_size))).size(11.0).color(Color32::from_rgb(0xA0, 0xA0, 0xA0)));
            ui.label(RichText::new(format!("文件: {}  文件夹: {}", scanned_files, scanned_folders)).size(11.0).color(Color32::from_rgb(0xA0, 0xA0, 0xA0)));
        });
}
