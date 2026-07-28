use eframe::egui;
use egui::{Color32, Rounding, FontId, RichText, Stroke, Vec2};

// ---------- 数据模型 ----------
#[derive(Clone)]
struct FileNode {
    name: String,
    size: u64,     // bytes
    kind: &'static str,
    color: Color32,
}

struct DiskUiApp {
    root_path: String,
    total_size: u64,
    free_size: u64,
    used_size: u64,
    nodes: Vec<FileNode>,
    scanning: bool,
    scan_progress: f32,
    selected: Option<usize>,
}

impl Default for DiskUiApp {
    fn default() -> Self {
        let nodes = vec![
            FileNode { name: "Windows".into(),        size: 42_100_000_000, kind: "folder", color: Color32::from_rgb(0x4C, 0x8B, 0xF5) },
            FileNode { name: "Program Files".into(),  size: 28_600_000_000, kind: "folder", color: Color32::from_rgb(0x34, 0xC7, 0x59) },
            FileNode { name: "Users".into(),          size: 65_800_000_000, kind: "folder", color: Color32::from_rgb(0xF5, 0xA6, 0x23) },
            FileNode { name: "steamapps".into(),      size: 120_400_000_000,kind: "folder", color: Color32::from_rgb(0xE0, 0x55, 0x5B) },
            FileNode { name: "node_modules".into(),   size: 9_200_000_000,  kind: "folder", color: Color32::from_rgb(0x9C, 0x6A, 0xDE) },
            FileNode { name: "pagefile.sys".into(),   size: 16_000_000_000, kind: "file",   color: Color32::from_rgb(0x5A, 0x6B, 0x7C) },
            FileNode { name: "big_video.mp4".into(),  size: 7_400_000_000,  kind: "file",   color: Color32::from_rgb(0x2E, 0xC4, 0xB6) },
            FileNode { name: "其他".into(),           size: 21_500_000_000, kind: "folder", color: Color32::from_rgb(0x6C, 0x75, 0x7D) },
        ];
        let used: u64 = nodes.iter().map(|n| n.size).sum();
        Self {
            root_path: r"C:\".into(),
            total_size: 512_000_000_000,
            free_size: 512_000_000_000u64.saturating_sub(used),
            used_size: used,
            nodes,
            scanning: false,
            scan_progress: 0.0,
            selected: None,
        }
    }
}

fn human_size(bytes: u64) -> String {
    let units = ["B", "KB", "MB", "GB", "TB"];
    let mut v = bytes as f64;
    let mut u = 0;
    while v >= 1024.0 && u < units.len() - 1 {
        v /= 1024.0;
        u += 1;
    }
    format!("{:.1} {}", v, units[u])
}

fn layout_treemap(nodes: &[FileNode], rect: egui::Rect) -> Vec<egui::Rect> {
    let mut result = Vec::with_capacity(nodes.len());
    let row_h = rect.height() / 3.0;
    let per_row = (nodes.len() + 2) / 3;
    let mut idx = 0;
    for row in 0..3 {
        let row_rect = egui::Rect::from_min_size(
            egui::pos2(rect.min.x, rect.min.y + row as f32 * row_h),
            egui::vec2(rect.width(), row_h),
        );
        let row_nodes: Vec<&FileNode> = nodes.iter().skip(idx).take(per_row).collect();
        let row_total: u64 = row_nodes.iter().map(|n| n.size.max(1)).sum::<u64>().max(1);
        let mut x = row_rect.min.x;
        for n in &row_nodes {
            let w = row_rect.width() * (n.size.max(1) as f32 / row_total as f32);
            result.push(egui::Rect::from_min_size(
                egui::pos2(x, row_rect.min.y),
                egui::vec2(w.max(1.0), row_rect.height()),
            ));
            x += w;
        }
        idx += per_row;
    }
    result
}

impl eframe::App for DiskUiApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        let mut style = (*ctx.style()).clone();
        style.visuals.window_fill = Color32::from_rgb(0x1E, 0x1F, 0x22);
        style.visuals.panel_fill = Color32::from_rgb(0x1E, 0x1F, 0x22);
        style.visuals.override_text_color = Some(Color32::from_rgb(0xE8, 0xE8, 0xE8));
        style.spacing.item_spacing = Vec2::new(10.0, 8.0);
        style.spacing.button_padding = Vec2::new(12.0, 6.0);
        ctx.set_style(style);

        egui::TopBottomPanel::top("top_bar")
            .exact_height(56.0)
            .frame(egui::Frame::default()
                .fill(Color32::from_rgb(0x25, 0x27, 0x2B))
                .inner_margin(egui::Margin::symmetric(16.0, 8.0)))
            .show(ctx, |ui| {
                ui.horizontal_centered(|ui| {
                    ui.label(RichText::new("⛁ DiskLens").size(18.0).strong().color(Color32::from_rgb(0x4C, 0x8B, 0xF5)));
                    ui.add_space(16.0);
                    ui.label("路径:");
                    ui.add(egui::TextEdit::singleline(&mut self.root_path).desired_width(200.0));
                    if ui.add(egui::Button::new(RichText::new("  扫描  ").color(Color32::WHITE))
                        .fill(Color32::from_rgb(0x4C, 0x8B, 0xF5))
                        .rounding(6.0))
                        .clicked()
                    {
                        self.scanning = true;
                        self.scan_progress = 0.0;
                    }
                    if self.scanning {
                        self.scan_progress += 0.02;
                        if self.scan_progress >= 1.0 {
                            self.scanning = false;
                        }
                        ui.add(egui::ProgressBar::new(self.scan_progress).desired_width(140.0));
                        ui.label(RichText::new("正在读取 MFT…").weak());
                    }
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        ui.label(RichText::new(format!(
                            "已用 {} / 共 {}",
                            human_size(self.used_size),
                            human_size(self.total_size)
                        )).weak());
                    });
                });
            });

        egui::SidePanel::left("stats_panel")
            .resizable(false)
            .exact_width(220.0)
            .frame(egui::Frame::default()
                .fill(Color32::from_rgb(0x23, 0x24, 0x28))
                .inner_margin(egui::Margin::same(14.0)))
            .show(ctx, |ui| {
                ui.label(RichText::new("磁盘概览").strong().size(15.0));
                ui.add_space(10.0);

                let (rect, _) = ui.allocate_exact_size(Vec2::new(190.0, 90.0), egui::Sense::hover());
                let painter = ui.painter_at(rect);
                let used_ratio = self.used_size as f32 / self.total_size as f32;
                painter.rect_filled(rect, Rounding::same(10.0), Color32::from_rgb(0x30, 0x32, 0x36));
                let used_rect = egui::Rect::from_min_size(rect.min, Vec2::new(rect.width() * used_ratio, rect.height()));
                painter.rect_filled(used_rect, Rounding::same(10.0), Color32::from_rgb(0x4C, 0x8B, 0xF5));
                painter.text(
                    rect.center(),
                    egui::Align2::CENTER_CENTER,
                    format!("{:.0}% 已用", used_ratio * 100.0),
                    FontId::proportional(16.0),
                    Color32::WHITE,
                );

                ui.add_space(14.0);
                ui.separator();
                ui.add_space(6.0);

                for n in &self.nodes {
                    ui.horizontal(|ui| {
                        let (r, _) = ui.allocate_exact_size(Vec2::new(10.0, 10.0), egui::Sense::hover());
                        ui.painter().rect_filled(r, Rounding::same(2.0), n.color);
                        ui.label(RichText::new(&n.name).size(12.5));
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            ui.label(RichText::new(human_size(n.size)).weak().size(12.0));
                        });
                    });
                }

                ui.add_space(14.0);
                ui.separator();
                ui.label(RichText::new(format!("剩余空间: {}", human_size(self.free_size))).weak());
            });

        egui::CentralPanel::default()
            .frame(egui::Frame::default()
                .fill(Color32::from_rgb(0x1E, 0x1F, 0x22))
                .inner_margin(egui::Margin::same(16.0)))
            .show(ctx, |ui| {
                ui.label(RichText::new("空间分布 (Treemap)").strong().size(15.0));
                ui.add_space(8.0);

                let avail = ui.available_size() - Vec2::new(0.0, 220.0);
                let (rect, _resp) = ui.allocate_exact_size(avail.max(Vec2::new(100.0, 100.0)), egui::Sense::click());
                let rects = layout_treemap(&self.nodes, rect);
                let painter = ui.painter_at(rect);

                for (i, (r, n)) in rects.iter().zip(self.nodes.iter()).enumerate() {
                    let inset = r.shrink(2.0);
                    let is_sel = self.selected == Some(i);
                    painter.rect_filled(inset, Rounding::same(6.0), n.color);
                    if is_sel {
                        painter.rect_stroke(inset, Rounding::same(6.0), Stroke::new(2.0, Color32::WHITE));
                    }
                    if inset.width() > 60.0 && inset.height() > 30.0 {
                        painter.text(
                            inset.left_top() + Vec2::new(8.0, 8.0),
                            egui::Align2::LEFT_TOP,
                            &n.name,
                            FontId::proportional(13.0),
                            Color32::from_rgba_unmultiplied(255, 255, 255, 235),
                        );
                        painter.text(
                            inset.left_bottom() + Vec2::new(8.0, -8.0),
                            egui::Align2::LEFT_BOTTOM,
                            human_size(n.size),
                            FontId::proportional(11.5),
                            Color32::from_rgba_unmultiplied(255, 255, 255, 200),
                        );
                    }
                    if ui.rect_contains_pointer(inset) && ui.input(|i| i.pointer.any_click()) {
                        self.selected = Some(i);
                    }
                }

                ui.add_space(12.0);
                ui.separator();
                ui.add_space(8.0);

                ui.label(RichText::new("文件明细").strong().size(14.0));
                egui_extras::TableBuilder::new(ui)
                    .striped(true)
                    .column(egui_extras::Column::remainder().at_least(200.0))
                    .column(egui_extras::Column::exact(90.0))
                    .column(egui_extras::Column::exact(110.0))
                    .header(22.0, |mut header| {
                        header.col(|ui| { ui.label(RichText::new("名称").strong()); });
                        header.col(|ui| { ui.label(RichText::new("类型").strong()); });
                        header.col(|ui| { ui.label(RichText::new("大小").strong()); });
                    })
                    .body(|mut body| {
                        for n in &self.nodes {
                            body.row(20.0, |mut row| {
                                row.col(|ui| {
                                    let icon = if n.kind == "folder" { "📁" } else { "📄" };
                                    ui.label(format!("{icon} {}", n.name));
                                });
                                row.col(|ui| { ui.label(n.kind); });
                                row.col(|ui| { ui.label(human_size(n.size)); });
                            });
                        }
                    });
            });

        ctx.request_repaint();
    }
}

fn setup_fonts(ctx: &egui::Context) {
    let mut fonts = egui::FontDefinitions::default();
    fonts.font_data.insert(
        "noto_sc".to_owned(),
        egui::FontData::from_static(include_bytes!("../assets/NotoSansSC-subset.ttf")),
    );
    // 插到 Proportional 字族最前面，中英文混排都优先用它
    fonts
        .families
        .entry(egui::FontFamily::Proportional)
        .or_default()
        .insert(0, "noto_sc".to_owned());
    fonts
        .families
        .entry(egui::FontFamily::Monospace)
        .or_default()
        .push("noto_sc".to_owned());
    ctx.set_fonts(fonts);
}

fn main() -> eframe::Result<()> {
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default().with_inner_size([980.0, 640.0]),
        ..Default::default()
    };
    eframe::run_native(
        "DiskLens - Rust Disk Analyzer Demo",
        options,
        Box::new(|cc| {
            setup_fonts(&cc.egui_ctx);
            Ok(Box::new(DiskUiApp::default()))
        }),
    )
}
