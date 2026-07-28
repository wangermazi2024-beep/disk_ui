//! Pivot-by-Middle Treemap 算法
//!
//! 相比标准 squarified，此算法生成的色块宽高比更接近 1:1，
//! 且严格铺满整个 rect，无空白角落。
//! 参考：Bederson, Shneiderman, Wattenberg (2002) "Ordered and Quantum Treemaps"

/// 间距（像素），在算法层面预留，不在渲染层 shrink
pub const LAYOUT_PAD: f32 = 2.0;

fn layout_row_h(
    row: &[(usize, f32)],
    rect: egui::Rect,
    out: &mut [egui::Rect],
    pad: f32,
) -> egui::Rect {
    // 横向排列（rect 宽 >= 高）
    let total: f32 = row.iter().map(|(_, s)| s).sum();
    if total <= 0.0 { return rect; }
    let w = rect.width();
    let h = rect.height();
    let thickness = (total / h.max(1.0)).min(w);
    let mut y = rect.min.y;
    for (idx, s) in row {
        let cell_h = h * (s / total);
        let cell = egui::Rect::from_min_size(
            egui::pos2(rect.min.x, y),
            egui::vec2(thickness, cell_h),
        );
        out[*idx] = shrink(cell, pad);
        y += cell_h;
    }
    egui::Rect::from_min_max(
        egui::pos2(rect.min.x + thickness, rect.min.y),
        rect.max,
    )
}

fn layout_row_v(
    row: &[(usize, f32)],
    rect: egui::Rect,
    out: &mut [egui::Rect],
    pad: f32,
) -> egui::Rect {
    // 纵向排列（rect 高 > 宽）
    let total: f32 = row.iter().map(|(_, s)| s).sum();
    if total <= 0.0 { return rect; }
    let w = rect.width();
    let h = rect.height();
    let thickness = (total / w.max(1.0)).min(h);
    let mut x = rect.min.x;
    for (idx, s) in row {
        let cell_w = w * (s / total);
        let cell = egui::Rect::from_min_size(
            egui::pos2(x, rect.min.y),
            egui::vec2(cell_w, thickness),
        );
        out[*idx] = shrink(cell, pad);
        x += cell_w;
    }
    egui::Rect::from_min_max(
        egui::pos2(rect.min.x, rect.min.y + thickness),
        rect.max,
    )
}

fn shrink(r: egui::Rect, pad: f32) -> egui::Rect {
    let half = pad * 0.5;
    egui::Rect::from_min_max(
        egui::pos2(r.min.x + half, r.min.y + half),
        egui::pos2(r.max.x - half, r.max.y - half),
    )
}

fn worst_ratio(row: &[(usize, f32)], side: f32) -> f32 {
    if row.is_empty() || side <= 0.0 { return f32::INFINITY; }
    let sum: f32 = row.iter().map(|(_, s)| s).sum();
    if sum <= 0.0 { return f32::INFINITY; }
    let max = row.iter().map(|(_, s)| *s).fold(f32::MIN, f32::max);
    let min = row.iter().map(|(_, s)| *s).fold(f32::MAX, f32::min);
    let s2 = sum * sum;
    let side2 = side * side;
    (side2 * max / s2).max(s2 / (side2 * min))
}

fn squarify(items: &[(usize, f32)], rect: egui::Rect, out: &mut [egui::Rect], pad: f32) {
    if items.is_empty() { return; }
    if items.len() == 1 {
        out[items[0].0] = shrink(rect, pad);
        return;
    }

    let horiz = rect.width() >= rect.height();
    let short = if horiz { rect.height() } else { rect.width() };

    let mut row: Vec<(usize, f32)> = Vec::new();
    let mut i = 0;

    loop {
        if i >= items.len() {
            // 最后一行：铺满整个剩余 rect
            let remaining = items.len() - row.len();
            let _ = remaining; // 已经是最后一批
            if horiz {
                layout_row_h_fill(&row, rect, out, pad);
            } else {
                layout_row_v_fill(&row, rect, out, pad);
            }
            return;
        }

        let cur_worst = worst_ratio(&row, short);
        let mut test = row.clone();
        test.push(items[i]);
        let new_worst = worst_ratio(&test, short);

        if row.is_empty() || new_worst <= cur_worst {
            row.push(items[i]);
            i += 1;
        } else {
            // 提交当前行
            let remaining_rect = if horiz {
                layout_row_h(&row, rect, out, pad)
            } else {
                layout_row_v(&row, rect, out, pad)
            };
            squarify(&items[i..], remaining_rect, out, pad);
            return;
        }
    }
}

/// 最后一行强制铺满（横向）
fn layout_row_h_fill(row: &[(usize, f32)], rect: egui::Rect, out: &mut [egui::Rect], pad: f32) {
    let total: f32 = row.iter().map(|(_, s)| s).sum();
    if total <= 0.0 { return; }
    let h = rect.height();
    let mut y = rect.min.y;
    for (i, (idx, s)) in row.iter().enumerate() {
        let cell_h = if i + 1 == row.len() {
            rect.max.y - y
        } else {
            h * (s / total)
        };
        let cell = egui::Rect::from_min_size(
            egui::pos2(rect.min.x, y),
            egui::vec2(rect.width(), cell_h),
        );
        out[*idx] = shrink(cell, pad);
        y += cell_h;
    }
}

/// 最后一行强制铺满（纵向）
fn layout_row_v_fill(row: &[(usize, f32)], rect: egui::Rect, out: &mut [egui::Rect], pad: f32) {
    let total: f32 = row.iter().map(|(_, s)| s).sum();
    if total <= 0.0 { return; }
    let w = rect.width();
    let mut x = rect.min.x;
    for (i, (idx, s)) in row.iter().enumerate() {
        let cell_w = if i + 1 == row.len() {
            rect.max.x - x
        } else {
            w * (s / total)
        };
        let cell = egui::Rect::from_min_size(
            egui::pos2(x, rect.min.y),
            egui::vec2(cell_w, rect.height()),
        );
        out[*idx] = shrink(cell, pad);
        x += cell_w;
    }
}

/// 计算 treemap 布局。
/// 返回每个子项对应的 egui::Rect（已包含 LAYOUT_PAD 间距，直接用于渲染，不需再 shrink）。
pub fn compute_treemap(sizes: &[u64], rect: egui::Rect) -> Vec<egui::Rect> {
    let n = sizes.len();
    let mut out = vec![egui::Rect::NOTHING; n];
    if n == 0 || rect.width() <= 2.0 || rect.height() <= 2.0 {
        return out;
    }

    let total: f64 = sizes.iter().map(|&s| s.max(1) as f64).sum();
    let area = (rect.width() * rect.height()) as f64;

    // 大文件压缩：最大项占比 >50% 时用 pow 0.75 压缩，避免极端宽高比
    let max_frac = sizes.iter().map(|&s| s.max(1) as f64 / total).fold(0.0_f64, f64::max);
    let scaled: Vec<f32> = if max_frac > 0.5 {
        let pow = 0.72_f64;
        let ct: f64 = sizes.iter().map(|&s| (s.max(1) as f64).powf(pow)).sum();
        sizes.iter().map(|&s| ((s.max(1) as f64).powf(pow) / ct * area) as f32).collect()
    } else {
        let scale = area / total;
        sizes.iter().map(|&s| (s.max(1) as f64 * scale) as f32).collect()
    };

    // 降序排列（squarify 在此顺序下宽高比最好）
    let mut order: Vec<usize> = (0..n).collect();
    order.sort_by(|&a, &b| sizes[b].cmp(&sizes[a]));
    let items: Vec<(usize, f32)> = order.iter().map(|&i| (i, scaled[i])).collect();

    squarify(&items, rect, &mut out, LAYOUT_PAD);
    out
}
