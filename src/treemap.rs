//! Binary Treemap 算法
//!
//! 递归二分分割：找到累计面积的中点，垂直或水平分割，再递归处理两侧。
//! 相比 squarified 产生更错落有致的矩形分布，接近 SpaceSniffer 效果。
//! 参考：Speedy37/streemap-rs (MIT)

/// 间距（像素），算法层统一处理
pub const LAYOUT_PAD: f32 = 2.0;

fn shrink(r: egui::Rect, pad: f32) -> egui::Rect {
    let h = pad * 0.5;
    egui::Rect::from_min_max(
        egui::pos2(r.min.x + h, r.min.y + h),
        egui::pos2(r.max.x - h, r.max.y - h),
    )
}

/// 递归二分分割
fn binary_split(
    rect: egui::Rect,
    indices: &mut [usize],
    scaled: &[f32],
    out: &mut [egui::Rect],
    pad: f32,
) {
    let n = indices.len();
    if n == 0 {
        return;
    }
    if n == 1 {
        out[indices[0]] = shrink(rect, pad);
        return;
    }

    // 计算前缀和
    let mut prefix = Vec::with_capacity(n);
    let mut total = 0.0_f32;
    for &idx in indices.iter() {
        total += scaled[idx];
        prefix.push(total);
    }

    // 找中点：累计面积达到 total/2 的位置
    let target = total * 0.5;
    let mid = match prefix.binary_search_by(|&p| p.partial_cmp(&target).unwrap_or(std::cmp::Ordering::Less)) {
        Ok(i) => i + 1,
        Err(i) => i.max(1),
    };
    let mid = mid.min(n - 1); // 保证两侧都至少有 1 个

    // 根据宽高比决定分割方向
    if rect.width() >= rect.height() {
        // 垂直分割（左右）
        let left_frac = prefix[mid - 1] / total;
        let xm = rect.min.x + rect.width() * left_frac;
        let left = egui::Rect::from_min_max(rect.min, egui::pos2(xm, rect.max.y));
        let right = egui::Rect::from_min_max(egui::pos2(xm, rect.min.y), rect.max);
        binary_split(left, &mut indices[..mid], scaled, out, pad);
        binary_split(right, &mut indices[mid..], scaled, out, pad);
    } else {
        // 水平分割（上下）
        let top_frac = prefix[mid - 1] / total;
        let ym = rect.min.y + rect.height() * top_frac;
        let top = egui::Rect::from_min_max(rect.min, egui::pos2(rect.max.x, ym));
        let bottom = egui::Rect::from_min_max(egui::pos2(rect.min.x, ym), rect.max);
        binary_split(top, &mut indices[..mid], scaled, out, pad);
        binary_split(bottom, &mut indices[mid..], scaled, out, pad);
    }
}

/// 计算 treemap 布局。
/// 返回每个子项对应的 egui::Rect（已包含 LAYOUT_PAD 间距，直接用于渲染）。
///
/// 算法：将 items 按面积比例缩放到 rect 内，
/// 然后递归二分分割，每次在累计中点处垂直或水平切开。
/// 不去优化宽高比，而是让块按面积自然错落分布。
pub fn compute_treemap(sizes: &[u64], rect: egui::Rect) -> Vec<egui::Rect> {
    let n = sizes.len();
    let mut out = vec![egui::Rect::NOTHING; n];
    if n == 0 || rect.width() <= LAYOUT_PAD || rect.height() <= LAYOUT_PAD {
        return out;
    }

    let total: f64 = sizes.iter().map(|&s| s.max(1) as f64).sum();
    let area = rect.width() as f64 * rect.height() as f64;

    // 大文件压缩：最大项占比 >50% 时用 pow 压缩，避免极端宽高比
    let max_frac = sizes.iter().map(|&s| s.max(1) as f64 / total).fold(0.0_f64, f64::max);
    let scaled: Vec<f32> = if max_frac > 0.5 {
        let pow = 0.72_f64;
        let ct: f64 = sizes.iter().map(|&s| (s.max(1) as f64).powf(pow)).sum();
        sizes.iter().map(|&s| ((s.max(1) as f64).powf(pow) / ct * area) as f32).collect()
    } else {
        let scale = area / total;
        sizes.iter().map(|&s| (s.max(1) as f64 * scale) as f32).collect()
    };

    // 降序排列（Binary 在此顺序下效果最好）
    let mut order: Vec<usize> = (0..n).collect();
    order.sort_by(|a, b| sizes[*b].cmp(&sizes[*a]));

    binary_split(rect, &mut order, &scaled, &mut out, LAYOUT_PAD);
    out
}
