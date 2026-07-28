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

// 文件类型分类（对应 WizTree/TreeSize 的"按类型统计"视图）
#[derive(Clone)]
struct CategoryStat {
    label: &'static str,
    size: u64,
    color: Color32,
}

struct DiskUiApp {
    root_path: String,
    total_size: u64,
    free_size: u64,
    used_size: u64,
    nodes: Vec<FileNode>,
    categories: Vec<CategoryStat>,
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
            FileNode { name: "AppData".into(),        size: 3_200_000_000,  kind: "folder", color: Color32::from_rgb(0x6C, 0x75, 0x7D) },
            FileNode { name: "Temp".into(),            size: 1_100_000_000,  kind: "folder", color: Color32::from_rgb(0x50, 0x58, 0x60) },
        ];
        let used: u64 = nodes.iter().map(|n| n.size).sum();

        // 真实实现里这份数据来自扫描时按扩展名累加；这里用示例数字演示分类效果
        let categories = vec![
            CategoryStat { label: "视频",     size: 98_000_000_000, color: Color32::from_rgb(0xE0, 0x55, 0x5B) },
            CategoryStat { label: "压缩包",   size: 41_000_000_000, color: Color32::from_rgb(0xF5, 0xA6, 0x23) },
            CategoryStat { label: "程序/exe", size: 63_000_000_000, color: Color32::from_rgb(0x4C, 0x8B, 0xF5) },
            CategoryStat { label: "文档",     size: 12_500_000_000, color: Color32::from_rgb(0x34, 0xC7, 0x59) },
            CategoryStat { label: "图片",     size: 8_200_000_000,  color: Color32::from_rgb(0x9C, 0x6A, 0xDE) },
            CategoryStat { label: "其他",     size: 21_000_000_000, color: Color32::from_rgb(0x6C, 0x75, 0x7D) },
        ];

        Self {
            root_path: r"C:\".into(),
            total_size: 512_000_000_000,
            free_size: 512_000_000_000u64.saturating_sub(used),
            used_size: used,
            nodes,
            categories,
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

// 按可用宽度截断文字，超出部分用省略号代替，而不是让文字溢出块外
fn truncate_text(ctx: &egui::Context, text: &str, font: FontId, max_width: f32) -> String {
    let measure = |s: &str| -> f32 {
        ctx.fonts(|f| f.layout_no_wrap(s.to_owned(), font.clone(), Color32::WHITE).size().x)
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

// ---------- Squarified Treemap 算法 ----------
// 参考 Bruls, Huizing, van Wijk (2000) "Squarified Treemaps"
// 核心思路：把数据按面积比例映射到矩形，并尽量让每个子矩形接近正方形，
// 这样既保证"面积=数据大小"这个直觉，也避免出现又细又长看不清的长条。

fn worst_ratio(row_sizes: &[f32], side: f32) -> f32 {
    if row_sizes.is_empty() || side <= 0.0 {
        return f32::INFINITY;
    }
    let sum: f32 = row_sizes.iter().sum();
    if sum <= 0.0 {
        return f32::INFINITY;
    }
    let row_max = row_sizes.iter().cloned().fold(f32::MIN, f32::max);
    let row_min = row_sizes.iter().cloned().fold(f32::MAX, f32::min);
    let s2 = sum * sum;
    let side2 = side * side;
    (side2 * row_max / s2).max(s2 / (side2 * row_min))
}

// 把一整行（一组条目）沿着矩形较短的一边铺满，向长边方向延伸出"厚度"
fn layout_row(
    row_idx: &[usize],
    row_sizes: &[f32],
    rect: egui::Rect,
    out: &mut [egui::Rect],
) -> egui::Rect {
    let sum: f32 = row_sizes.iter().sum();
    if rect.width() >= rect.height() {
        let thickness = (sum / rect.height().max(1.0)).min(rect.width());
        let mut y = rect.min.y;
        for (&idx, &s) in row_idx.iter().zip(row_sizes.iter()) {
            let h = rect.height() * (s / sum);
            out[idx] = egui::Rect::from_min_size(egui::pos2(rect.min.x, y), egui::vec2(thickness, h));
            y += h;
        }
        egui::Rect::from_min_max(egui::pos2(rect.min.x + thickness, rect.min.y), rect.max)
    } else {
        let thickness = (sum / rect.width().max(1.0)).min(rect.height());
        let mut x = rect.min.x;
        for (&idx, &s) in row_idx.iter().zip(row_sizes.iter()) {
            let w = rect.width() * (s / sum);
            out[idx] = egui::Rect::from_min_size(egui::pos2(x, rect.min.y), egui::vec2(w, thickness));
            x += w;
        }
        egui::Rect::from_min_max(egui::pos2(rect.min.x, rect.min.y + thickness), rect.max)
    }
}

fn squarify(indices: &[usize], sizes: &[f32], rect: egui::Rect, out: &mut [egui::Rect]) {
    if indices.is_empty() {
        return;
    }
    if indices.len() == 1 {
        out[indices[0]] = rect;
        return;
    }
    let short_side = rect.width().min(rect.height());
    let mut row_idx: Vec<usize> = Vec::new();
    let mut row_sizes: Vec<f32> = Vec::new();
    let mut i = 0;
    loop {
        if i >= indices.len() {
            layout_row(&row_idx, &row_sizes, rect, out);
            return;
        }
        let mut test_sizes = row_sizes.clone();
        test_sizes.push(sizes[i]);
        let cur_worst = worst_ratio(&row_sizes, short_side);
        let new_worst = worst_ratio(&test_sizes, short_side);
        if row_sizes.is_empty() || new_worst <= cur_worst {
            row_idx.push(indices[i]);
            row_sizes.push(sizes[i]);
            i += 1;
        } else {
            let remaining_rect = layout_row(&row_idx, &row_sizes, rect, out);
            squarify(&indices[i..], &sizes[i..], remaining_rect, out);
            return;
        }
    }
}

fn compute_treemap(nodes: &[FileNode], rect: egui::Rect) -> Vec<egui::Rect> {
    let mut out = vec![egui::Rect::NOTHING; nodes.len()];
    if nodes.is_empty() || rect.width() <= 1.0 || rect.height() <= 1.0 {
        return out;
    }
    let total: f32 = nodes.iter().map(|n| n.size.max(1) as f32).sum();
    if total <= 0.0 {
        return out;
    }
    let area = rect.width() * rect.height();
    let scale = area / total;

    // 按大小降序排列，squarify 算法在这个顺序下效果最好
    let mut order: Vec<usize> = (0..nodes.len()).collect();
    order.sort_by(|&a, &b| nodes[b].size.cmp(&nodes[a].size));
    let sizes: Vec<f32> = order.iter().map(|&i| nodes[i].size.max(1) as f32 * scale).collect();

    squarify(&order, &sizes, rect, &mut out);
    out
}

impl eframe::App for DiskUiApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        // 关键修复：从 egui 内置的暗色主题起步，而不是零散地强制文字颜色。
        // 之前用 override_text_color 只对"没有显式设色"的文字生效，
        // .strong() 这类样式会绕过它，导致暗底配深色字看不清。
        let mut visuals = egui::Visuals::dark();
        visuals.window_fill = Color32::from_rgb(0x1E, 0x1F, 0x22);
        visuals.panel_fill = Color32::from_rgb(0x1E, 0x1F, 0x22);
        let mut style = (*ctx.style()).clone();
        style.visuals = visuals;
        style.spacing.item_spacing = Vec2::new(10.0, 8.0);
        style.spacing.button_padding = Vec2::new(12.0, 6.0);
        style.interaction.tooltip_delay = 0.05; // 即时显示气泡（默认 0.5s 太慢）
        ctx.set_style(style);

        egui::TopBottomPanel::top("top_bar")
            .exact_height(56.0)
            .frame(egui::Frame::default()
                .fill(Color32::from_rgb(0x25, 0x27, 0x2B))
                .inner_margin(egui::Margin::symmetric(16.0, 8.0)))
            .show(ctx, |ui| {
                ui.horizontal_centered(|ui| {
                    ui.label(RichText::new("⛁ DiskLens").size(18.0).strong().color(Color32::from_rgb(0x6F, 0xA8, 0xFF)));
                    ui.add_space(16.0);
                    ui.label(RichText::new("路径:").color(Color32::from_rgb(0xC8, 0xC8, 0xC8)));
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
                        ui.label(RichText::new("正在读取 MFT…").color(Color32::from_rgb(0xA0, 0xA0, 0xA0)));
                    }
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        ui.label(RichText::new(format!(
                            "已用 {} / 共 {}",
                            human_size(self.used_size),
                            human_size(self.total_size)
                        )).color(Color32::from_rgb(0xA0, 0xA0, 0xA0)));
                    });
                });
            });

        // ---------- 左侧：按文件类型统计（视频/文档/压缩包/程序 等） ----------
        egui::SidePanel::left("stats_panel")
            .resizable(false)
            .exact_width(230.0)
            .frame(egui::Frame::default()
                .fill(Color32::from_rgb(0x23, 0x24, 0x28))
                .inner_margin(egui::Margin::same(14.0)))
            .show(ctx, |ui| {
                ui.label(RichText::new("磁盘概览").strong().size(15.0));
                ui.add_space(10.0);

                let (rect, _) = ui.allocate_exact_size(Vec2::new(200.0, 90.0), egui::Sense::hover());
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

                ui.add_space(16.0);
                ui.separator();
                ui.add_space(6.0);

                let cat_total: u64 = self.categories.iter().map(|c| c.size).sum::<u64>().max(1);
                for c in &self.categories {
                    let ratio = c.size as f32 / cat_total as f32;
                    ui.horizontal(|ui| {
                        let (r, _) = ui.allocate_exact_size(Vec2::new(10.0, 10.0), egui::Sense::hover());
                        ui.painter().rect_filled(r, Rounding::same(2.0), c.color);
                        ui.label(RichText::new(c.label).size(12.5).color(Color32::from_rgb(0xE0, 0xE0, 0xE0)));
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            ui.label(RichText::new(human_size(c.size)).size(12.0).color(Color32::from_rgb(0xA0, 0xA0, 0xA0)));
                        });
                    });
                    // 小型比例条，直观展示占比
                    let (bar_rect, _) = ui.allocate_exact_size(Vec2::new(200.0, 5.0), egui::Sense::hover());
                    let bp = ui.painter_at(bar_rect);
                    bp.rect_filled(bar_rect, Rounding::same(2.0), Color32::from_rgb(0x30, 0x32, 0x36));
                    let filled = egui::Rect::from_min_size(bar_rect.min, Vec2::new(bar_rect.width() * ratio, bar_rect.height()));
                    bp.rect_filled(filled, Rounding::same(2.0), c.color);
                    ui.add_space(4.0);
                }

                ui.add_space(10.0);
                ui.separator();
                ui.add_space(8.0);
                ui.label(RichText::new(format!("剩余空间: {}", human_size(self.free_size)))
                    .color(Color32::from_rgb(0xA0, 0xA0, 0xA0)));
            });

        egui::CentralPanel::default()
            .frame(egui::Frame::default()
                .fill(Color32::from_rgb(0x1E, 0x1F, 0x22))
                .inner_margin(egui::Margin::same(16.0)))
            .show(ctx, |ui| {
                ui.label(RichText::new("空间分布 (Treemap)").strong().size(15.0));
                ui.add_space(8.0);

                // treemap 给一个相对固定的高度比例，剩下的空间交给下面
                // 独立的 ScrollArea（文件明细）自己管理，不再用脆弱的"瞎减常数"写法。
                let total_h = ui.available_height();
                let treemap_h = (total_h * 0.5).clamp(220.0, 420.0);
                let (rect, _resp) = ui.allocate_exact_size(
                    Vec2::new(ui.available_width(), treemap_h),
                    egui::Sense::click(),
                );
                let rects = compute_treemap(&self.nodes, rect);

                for (i, (r, n)) in rects.iter().zip(self.nodes.iter()).enumerate() {
                    let inset = r.shrink(1.5);
                    if inset.width() < 1.0 || inset.height() < 1.0 {
                        continue;
                    }
                    let is_sel = self.selected == Some(i);
                    // 用 painter_at(inset) 把绘制严格裁剪在这个块内部，
                    // 文字/边框都不会溢出到相邻块上。
                    let painter = ui.painter_at(inset);
                    painter.rect_filled(inset, Rounding::same(4.0), n.color);
                    if is_sel {
                        painter.rect_stroke(inset, Rounding::same(4.0), Stroke::new(2.0_f32, Color32::WHITE));
                    }

                    // 文字按可用宽度截断（超出部分省略号代替），而不是超过阈值就整段隐藏，
                    // 这样即使块比较窄也能看到"名称开头几个字"，完整信息靠 hover 气泡补全。
                    let pad = 6.0;
                    let text_max_w = inset.width() - pad * 2.0;
                    if inset.width() > 22.0 && inset.height() > 18.0 && text_max_w > 8.0 {
                        let name_font = FontId::proportional(12.5);
                        let shown_name = truncate_text(ui.ctx(), &n.name, name_font.clone(), text_max_w);
                        if !shown_name.is_empty() {
                            painter.text(
                                inset.left_top() + Vec2::new(pad, 5.0),
                                egui::Align2::LEFT_TOP,
                                &shown_name,
                                name_font,
                                Color32::from_rgba_unmultiplied(255, 255, 255, 240),
                            );
                        }
                        if inset.height() > 38.0 {
                            let size_font = FontId::proportional(11.0);
                            let size_text = human_size(n.size);
                            let shown_size = truncate_text(ui.ctx(), &size_text, size_font.clone(), text_max_w);
                            painter.text(
                                inset.left_bottom() + Vec2::new(pad, -5.0),
                                egui::Align2::LEFT_BOTTOM,
                                &shown_size,
                                size_font,
                                Color32::from_rgba_unmultiplied(255, 255, 255, 205),
                            );
                        }
                    }

                    // 手动气泡：不用 egui 内置 on_hover_text（按下鼠标键时会被抑制）
                    // 直接检测鼠标位置，只要在块范围内就立刻显示，跟按没按键完全无关
                    let id = ui.id().with(("treemap_block", i));
                    let resp = ui.interact(inset, id, egui::Sense::click());
                    if ui.rect_contains_pointer(inset) {
                        let tip_pos = ui.ctx().pointer_latest_pos().unwrap_or(inset.left_bottom());
                        egui::Area::new(id.with("tip"))
                            .fixed_pos(tip_pos + Vec2::new(14.0, 0.0))
                            .order(egui::Order::Tooltip)
                            .interactable(false)
                            .show(ui.ctx(), |ui| {
                                egui::Frame::default()
                                    .fill(Color32::from_rgb(0x33, 0x33, 0x38))
                                    .rounding(4.0)
                                    .inner_margin(egui::Margin::same(6.0))
                                    .show(ui, |ui| {
                                        ui.label(RichText::new(
                                            format!("{} · {}", n.name, human_size(n.size))
                                        ).color(Color32::WHITE));
                                    });
                            });
                    }
                    if resp.clicked() {
                        self.selected = Some(i);
                    }
                }

                ui.add_space(12.0);
                ui.separator();
                ui.add_space(8.0);

                // ---------- 固定表头（不随表格滚动） ----------
                ui.label(RichText::new("文件明细").strong().size(14.0));
                ui.add_space(4.0);

                let header_bg = Color32::from_rgb(0x2A, 0x2C, 0x30);
                let header_h = 22.0;
                egui::Frame::default()
                    .fill(header_bg)
                    .inner_margin(egui::Margin::symmetric(6.0, 0.0))
                    .show(ui, |ui| {
                        ui.horizontal(|ui| {
                            // 名称列（剩余宽度）
                            let name_w = ui.available_width() - 90.0 - 110.0;
                            ui.allocate_ui_with_layout(
                                Vec2::new(name_w.max(50.0), header_h),
                                egui::Layout::left_to_right(egui::Align::Center),
                                |ui| {
                                    ui.label(RichText::new("名称").strong().size(12.5).color(Color32::from_rgb(0xE0, 0xE0, 0xE0)));
                                },
                            );
                            // 类型列
                            ui.allocate_ui_with_layout(
                                Vec2::new(90.0, header_h),
                                egui::Layout::left_to_right(egui::Align::Center),
                                |ui| {
                                    ui.label(RichText::new("类型").strong().size(12.5).color(Color32::from_rgb(0xE0, 0xE0, 0xE0)));
                                },
                            );
                            // 大小列
                            ui.allocate_ui_with_layout(
                                Vec2::new(110.0, header_h),
                                egui::Layout::left_to_right(egui::Align::Center),
                                |ui| {
                                    ui.label(RichText::new("大小").strong().size(12.5).color(Color32::from_rgb(0xE0, 0xE0, 0xE0)));
                                },
                            );
                        });
                    });

                // ---------- 表格体（可滚动） ----------
                egui::ScrollArea::vertical()
                    .auto_shrink([false, false])
                    .show(ui, |ui| {
                        egui_extras::TableBuilder::new(ui)
                            .striped(true)
                            .column(egui_extras::Column::remainder().at_least(200.0))
                            .column(egui_extras::Column::exact(90.0))
                            .column(egui_extras::Column::exact(110.0))
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
                        // 底部留白，确保滚到最下面时最后一行完整可见、不贴边
                        ui.add_space(16.0);
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
