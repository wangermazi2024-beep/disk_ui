//! Squarified Treemap 算法（改进版）。
//!
//! 改进：
//! 1. **大文件压缩**：单个条目占比 > 50% 时使用 `s^0.75` 压缩，
//!    避免一个超大文件挤掉其它兄弟条目（参考 SpaceSniffer 视觉直觉）。
//! 2. **最后一行铺满**：squarify 递归到最后一组时强制 thickness = 剩余边长，
//!    消除"右下角空白"。
//!
//! 参考：Bruls, Huizing, van Wijk (2000) "Squarified Treemaps"。

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

/// `fill = true` 表示这是最后一行，强制 thickness 铺满整个 rect 的长边，
/// 避免右下角出现空白条。
fn layout_row(
    row_idx: &[usize],
    row_sizes: &[f32],
    rect: egui::Rect,
    out: &mut [egui::Rect],
    fill: bool,
) -> egui::Rect {
    let sum: f32 = row_sizes.iter().sum();
    if sum <= 0.0 {
        return rect;
    }
    if rect.width() >= rect.height() {
        let thickness = if fill {
            rect.width()
        } else {
            (sum / rect.height().max(1.0)).min(rect.width())
        };
        let mut y = rect.min.y;
        for (&idx, &s) in row_idx.iter().zip(row_sizes.iter()) {
            let h = rect.height() * (s / sum);
            out[idx] = egui::Rect::from_min_size(egui::pos2(rect.min.x, y), egui::vec2(thickness, h));
            y += h;
        }
        egui::Rect::from_min_max(egui::pos2(rect.min.x + thickness, rect.min.y), rect.max)
    } else {
        let thickness = if fill {
            rect.height()
        } else {
            (sum / rect.width().max(1.0)).min(rect.height())
        };
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
            // 最后一组：铺满整个 rect 消除空白
            layout_row(&row_idx, &row_sizes, rect, out, true);
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
            let remaining_rect = layout_row(&row_idx, &row_sizes, rect, out, false);
            squarify(&indices[i..], &sizes[i..], remaining_rect, out);
            return;
        }
    }
}

/// 给定一组大小（字节数）和一个矩形区域，按面积比例算出每一项对应的子矩形。
///
/// 改进：
/// - 单条目占比 > 50% 时启用 `s^0.75` 压缩（大块更小，小块更大，整体更可读）。
/// - 最后一行强制铺满，消除右下角空白。
pub fn compute_treemap(sizes: &[u64], rect: egui::Rect) -> Vec<egui::Rect> {
    let mut out = vec![egui::Rect::NOTHING; sizes.len()];
    if sizes.is_empty() || rect.width() <= 1.0 || rect.height() <= 1.0 {
        return out;
    }
    let total: f64 = sizes.iter().map(|&s| s.max(1) as f64).sum();
    if total <= 0.0 {
        return out;
    }
    let area = rect.width() * rect.height();

    // 大文件压缩检测
    let max_frac = sizes
        .iter()
        .map(|&s| s.max(1) as f64 / total)
        .fold(0.0, f64::max);

    let scaled_sizes: Vec<f32> = if max_frac > 0.5 {
        let pow = 0.75_f64;
        let compressed_total: f64 = sizes.iter().map(|&s| (s.max(1) as f64).powf(pow)).sum();
        if compressed_total > 0.0 {
            sizes
                .iter()
                .map(|&s| {
                    ((s.max(1) as f64).powf(pow) / compressed_total * area as f64) as f32
                })
                .collect()
        } else {
            let scale = (area as f64 / total) as f32;
            sizes.iter().map(|&s| s.max(1) as f32 * scale).collect()
        }
    } else {
        let scale = (area as f64 / total) as f32;
        sizes.iter().map(|&s| s.max(1) as f32 * scale).collect()
    };

    // 按大小降序排列，squarify 算法在这个顺序下效果最好。
    let mut order: Vec<usize> = (0..sizes.len()).collect();
    order.sort_by(|&a, &b| sizes[b].cmp(&sizes[a]));
    let ordered_sizes: Vec<f32> = order.iter().map(|&i| scaled_sizes[i]).collect();

    squarify(&order, &ordered_sizes, rect, &mut out);
    out
}
