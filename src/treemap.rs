//! Squarified Treemap 算法。
//!
//! 参考 Bruls, Huizing, van Wijk (2000) "Squarified Treemaps"。
//! 核心思路：把数据按面积比例映射到矩形，并尽量让每个子矩形接近正方形，
//! 这样既保证"面积=数据大小"这个直觉，也避免出现又细又长看不清的长条。
//!
//! 这一版和原来单文件里的实现相比，逻辑完全没变，唯一的区别是
//! `compute_treemap` 现在只依赖 `&[u64]` 大小数组，不再耦合 `FileNode`，
//! 这样它既能给 treemap 色块用，将来给别的"按大小分布"的可视化复用也没问题。

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

// 把一整行（一组条目）沿着矩形较短的一边铺满，向长边方向延伸出"厚度"。
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

/// 给定一组大小（字节数）和一个矩形区域，按面积比例算出每一项对应的子矩形。
/// 返回的 `Vec<Rect>` 与输入 `sizes` 一一对应（顺序不变，内部排序只是算法细节）。
pub fn compute_treemap(sizes: &[u64], rect: egui::Rect) -> Vec<egui::Rect> {
    let mut out = vec![egui::Rect::NOTHING; sizes.len()];
    if sizes.is_empty() || rect.width() <= 1.0 || rect.height() <= 1.0 {
        return out;
    }
    let total: f32 = sizes.iter().map(|&s| s.max(1) as f32).sum();
    if total <= 0.0 {
        return out;
    }
    let area = rect.width() * rect.height();
    let scale = area / total;

    // 按大小降序排列，squarify 算法在这个顺序下效果最好。
    let mut order: Vec<usize> = (0..sizes.len()).collect();
    order.sort_by(|&a, &b| sizes[b].cmp(&sizes[a]));
    let scaled_sizes: Vec<f32> = order.iter().map(|&i| sizes[i].max(1) as f32 * scale).collect();

    squarify(&order, &scaled_sizes, rect, &mut out);
    out
}
