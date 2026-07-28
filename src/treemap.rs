//! Binary Treemap 算法 — 两遍扫描 + 最小矩形保护
//!
//! 1) 第一遍：按比例二分分割，计算所有子矩形。
//! 2) 检查：任何宽或高 < MIN_BLOCK_PX 的块被标记为"不渲染"。
//! 3) 剔除不渲染块的面积，按比例重新分配给剩余块。
//! 4) 重复 1-3 直到所有剩余块 >= MIN_BLOCK_PX。
//!
//! 这样保证画出来的每个块都是可见可点击的，不会出现细条或点状块。
//! 参考：Speedy37/streemap-rs Binary 算法 + 两遍扫描思路

pub const LAYOUT_PAD: f32 = 2.0;
/// 最小可见块边长（像素）。小于此值的块在算法层就被剔除，不占用空间。
const MIN_BLOCK_PX: f32 = 6.0;

fn shrink(r: egui::Rect, pad: f32) -> egui::Rect {
    let h = pad * 0.5;
    egui::Rect::from_min_max(
        egui::pos2(r.min.x + h, r.min.y + h),
        egui::pos2(r.max.x - h, r.max.y - h),
    )
}

/// 二分分割（核心布局）
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

    let mut prefix = Vec::with_capacity(n);
    let mut total = 0.0_f32;
    for &idx in indices.iter() {
        total += scaled[idx];
        prefix.push(total);
    }

    let target = total * 0.5;
    let mid = match prefix.binary_search_by(|p| {
        if *p > target {
            std::cmp::Ordering::Greater
        } else {
            std::cmp::Ordering::Less
        }
    }) {
        Ok(i) => i + 1,
        Err(0) => 1,
        Err(i) => i,
    }
    .min(n - 1);

    if rect.width() >= rect.height() {
        let left_frac = prefix[mid - 1] / total;
        let xm = rect.min.x + rect.width() * left_frac;
        let left = egui::Rect::from_min_max(rect.min, egui::pos2(xm, rect.max.y));
        let right = egui::Rect::from_min_max(egui::pos2(xm, rect.min.y), rect.max);
        binary_split(left, &mut indices[..mid], scaled, out, pad);
        binary_split(right, &mut indices[mid..], scaled, out, pad);
    } else {
        let top_frac = prefix[mid - 1] / total;
        let ym = rect.min.y + rect.height() * top_frac;
        let top = egui::Rect::from_min_max(rect.min, egui::pos2(rect.max.x, ym));
        let bottom = egui::Rect::from_min_max(egui::pos2(rect.min.x, ym), rect.max);
        binary_split(top, &mut indices[..mid], scaled, out, pad);
        binary_split(bottom, &mut indices[mid..], scaled, out, pad);
    }
}

/// 计算 treemap 布局（两遍扫描 + 最小矩形保护）。
///
/// 返回的 `Vec<Rect>` 与输入 `sizes` ——对应。
/// - 宽 >= MIN_BLOCK_PX 且 高 >= MIN_BLOCK_PX 的块是有效矩形。
/// - 小于此值的块返回 `Rect::NOTHING`，调用方跳过绘制即可。
///
/// `Rect::NOTHING` 块不占用实际像素空间（其面积已重新分配给剩余块）。
pub fn compute_treemap(sizes: &[u64], rect: egui::Rect) -> Vec<egui::Rect> {
    let n = sizes.len();
    let mut out = vec![egui::Rect::NOTHING; n];
    if n == 0 || rect.width() <= LAYOUT_PAD || rect.height() <= LAYOUT_PAD {
        return out;
    }

    // 活跃标记：true = 参与布局，false = 已被剔除
    let mut active: Vec<bool> = vec![true; n];

    for _iter in 0..12 {
        // 最多迭代 12 轮

        // ── 第一遍：计算总大小 & 缩放 ──
        let total: f64 = active
            .iter()
            .enumerate()
            .filter(|(_, a)| **a)
            .map(|(i, _)| sizes[i].max(1) as f64)
            .sum();
        if total <= 0.0 {
            break;
        }
        let area = rect.width() as f64 * rect.height() as f64;

        // 大文件压缩
        let max_frac = active
            .iter()
            .enumerate()
            .filter(|(_, a)| **a)
            .map(|(i, _)| sizes[i].max(1) as f64 / total)
            .fold(0.0_f64, f64::max);
        let scaled: Vec<f32> = if max_frac > 0.5 {
            let pow = 0.72_f64;
            let ct: f64 = sizes
                .iter()
                .enumerate()
                .map(|(i, _)| {
                    if active[i] {
                        (sizes[i].max(1) as f64).powf(pow)
                    } else {
                        0.0
                    }
                })
                .sum();
            sizes
                .iter()
                .enumerate()
                .map(|(i, &s)| {
                    if active[i] {
                        ((s.max(1) as f64).powf(pow) / ct * area) as f32
                    } else {
                        0.0
                    }
                })
                .collect()
        } else {
            let scale = area / total;
            sizes
                .iter()
                .enumerate()
                .map(|(i, &s)| {
                    if active[i] {
                        (s.max(1) as f64 * scale) as f32
                    } else {
                        0.0
                    }
                })
                .collect()
        };

        // ── 布局：降序排列后二分分割 ──
        let mut order: Vec<usize> = (0..n).filter(|i| active[*i]).collect();
        order.sort_by(|a, b| sizes[*b].cmp(&sizes[*a]));
        let mut tmp = vec![egui::Rect::NOTHING; n];
        binary_split(rect, &mut order, &scaled, &mut tmp, LAYOUT_PAD);

        // ── 第二遍：检查最小尺寸，剔除不合格项 ──
        let mut any_small = false;
        for &i in &order {
            let r = tmp[i];
            if r.width() < MIN_BLOCK_PX || r.height() < MIN_BLOCK_PX {
                active[i] = false;
                any_small = true;
            }
        }

        if !any_small {
            // 全部合格 → 输出最终结果
            for &i in &order {
                out[i] = tmp[i];
            }
            break;
        }
        // 有小块 → 剔除后重新迭代
    }

    out
}
