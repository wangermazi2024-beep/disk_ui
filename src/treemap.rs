//! Simple recursive proportional-split treemap algorithm.
//!
//! Preserves original item order (no sorting), alternating split direction
//! at each recursion level. Produces naturally staggered, irregular blocks
//! reminiscent of SpaceSniffer — no aspect-ratio optimization.

/// 间距（像素），在算法层面预留，不在渲染层 shrink
pub const LAYOUT_PAD: f32 = 2.0;

fn shrink(r: egui::Rect, pad: f32) -> egui::Rect {
    let half = pad * 0.5;
    egui::Rect::from_min_max(
        egui::pos2(r.min.x + half, r.min.y + half),
        egui::pos2(r.max.x - half, r.max.y - half),
    )
}

/// Recursive split: divide items into two groups at the point where cumulative
/// size is closest to half of total, split the rect proportionally, then recurse
/// with the opposite orientation (vertical ↔ horizontal).
fn split_treemap(
    items: &[(usize, f32)],
    rect: egui::Rect,
    out: &mut [egui::Rect],
    pad: f32,
    vertical: bool,
) {
    if items.is_empty() {
        return;
    }
    if items.len() == 1 {
        out[items[0].0] = shrink(rect, pad);
        return;
    }

    let total: f32 = items.iter().map(|(_, s)| s).sum();

    // When all sizes are zero (edge case), split evenly by count
    if total <= 0.0 {
        let mid = items.len() / 2;
        let frac = mid as f32 / items.len() as f32;
        let (r1, r2) = split_rect(rect, frac, vertical);
        split_treemap(&items[..mid], r1, out, pad, !vertical);
        split_treemap(&items[mid..], r2, out, pad, !vertical);
        return;
    }

    let half = total * 0.5;

    // Find split index where cumulative size is closest to half.
    // At least one item must go into each group.
    let mut split_idx = 1;
    let mut best_diff = f32::MAX;
    let mut cumul = 0.0_f32;

    for i in 0..items.len().saturating_sub(1) {
        cumul += items[i].1;
        let diff = (cumul - half).abs();
        if diff < best_diff {
            best_diff = diff;
            split_idx = i + 1;
        }
    }

    let group1_sum: f32 = items[..split_idx].iter().map(|(_, s)| s).sum();
    let frac = (group1_sum / total).clamp(0.01, 0.99);

    let (r1, r2) = split_rect(rect, frac, vertical);
    split_treemap(&items[..split_idx], r1, out, pad, !vertical);
    split_treemap(&items[split_idx..], r2, out, pad, !vertical);
}

/// Split `rect` into two sub-rectangles at a fractional position along the
/// given axis (`vertical = true` → left/right split).
fn split_rect(rect: egui::Rect, frac: f32, vertical: bool) -> (egui::Rect, egui::Rect) {
    if vertical {
        let split_x = rect.min.x + rect.width() * frac;
        let r1 = egui::Rect::from_min_max(rect.min, egui::pos2(split_x, rect.max.y));
        let r2 = egui::Rect::from_min_max(egui::pos2(split_x, rect.min.y), rect.max);
        (r1, r2)
    } else {
        let split_y = rect.min.y + rect.height() * frac;
        let r1 = egui::Rect::from_min_max(rect.min, egui::pos2(rect.max.x, split_y));
        let r2 = egui::Rect::from_min_max(egui::pos2(rect.min.x, split_y), rect.max);
        (r1, r2)
    }
}

/// 计算 treemap 布局。
///
/// 返回每个子项对应的 `egui::Rect`（已包含 `LAYOUT_PAD` 间距，直接用于渲染，不需再 shrink）。
///
/// 使用递归比例分割：保持原始文件顺序不变，每层按累计大小比例分割矩形，
/// 方向在垂直/水平之间交替。不优化宽高比，产生自然错落的"SpaceSniffer 风格"色块。
pub fn compute_treemap(sizes: &[u64], rect: egui::Rect) -> Vec<egui::Rect> {
    let n = sizes.len();
    let mut out = vec![egui::Rect::NOTHING; n];
    if n == 0 || rect.width() <= 2.0 || rect.height() <= 2.0 {
        return out;
    }

    let total: f64 = sizes.iter().map(|s| (*s).max(1) as f64).sum();
    let area = (rect.width() * rect.height()) as f64;

    // Size compression: when a single file dominates (>50%), apply pow scaling
    // to avoid extreme aspect ratios while preserving ordering.
    let max_frac = sizes
        .iter()
        .map(|s| (*s).max(1) as f64 / total)
        .fold(0.0_f64, f64::max);

    let scaled: Vec<f32> = if max_frac > 0.5 {
        let pow = 0.72_f64;
        let ct: f64 = sizes
            .iter()
            .map(|s| ((*s).max(1) as f64).powf(pow))
            .sum();
        sizes
            .iter()
            .map(|s| (((*s).max(1) as f64).powf(pow) / ct * area) as f32)
            .collect()
    } else {
        let scale = area / total;
        sizes
            .iter()
            .map(|s| ((*s).max(1) as f64 * scale) as f32)
            .collect()
    };

    // Preserve original order — no sorting
    let items: Vec<(usize, f32)> = (0..n).map(|i| (i, scaled[i])).collect();

    split_treemap(&items, rect, &mut out, LAYOUT_PAD, true);
    out
}
