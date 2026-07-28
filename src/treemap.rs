use egui::Rect;

// SpaceSniffer 风格约束：面积接近真实比例，但优先保证可见和可点击。
// 小块不会无限缩小，剩余面积回收到邻近大块。
const MIN_TILE: f32 = 18.0;

pub fn compute_treemap(sizes: &[u64], rect: Rect) -> Vec<Rect> {
    let mut out = vec![Rect::NOTHING; sizes.len()];
    if sizes.is_empty() || rect.width() < 1.0 || rect.height() < 1.0 { return out; }

    let mut order: Vec<usize> = (0..sizes.len()).collect();
    order.sort_by_key(|&i| std::cmp::Reverse(sizes[i]));

    let total: f32 = sizes.iter().map(|x| (*x).max(1) as f32).sum();
    let mut x = rect.min.x;
    let mut y = rect.min.y;
    let mut remain_w = rect.width();
    let mut remain_h = rect.height();

    // 近似 squarified + minimum tile constraint
    let mut current_y = y;
    let mut row_height = 0.0;
    let mut row: Vec<(usize,f32)> = Vec::new();

    for idx in order {
        let area = rect.width()*rect.height()*((sizes[idx].max(1) as f32)/total);
        row.push((idx, area));
        row_height = (area / remain_w.max(1.0)).max(MIN_TILE);
        if row_height > remain_h || row.len() >= 8 {
            let used: f32 = row.iter().map(|(_,a)| *a).sum();
            let h = (used / remain_w.max(1.0)).max(MIN_TILE).min(remain_h);
            let mut cx = x;
            for (i,a) in row.drain(..) {
                let w = (a/h).max(MIN_TILE).min(rect.max.x-cx);
                out[i]=Rect::from_min_max(egui::pos2(cx,current_y),egui::pos2(cx+w,current_y+h));
                cx += w;
            }
            current_y += h;
            remain_h = rect.max.y-current_y;
            if remain_h <= 1.0 { break; }
        }
    }
    if !row.is_empty() {
        let used: f32=row.iter().map(|(_,a)|*a).sum();
        let h=(used/remain_w.max(1.0)).max(MIN_TILE).min(remain_h);
        let mut cx=x;
        for (i,a) in row {
            let w=(a/h).max(MIN_TILE).min(rect.max.x-cx);
            out[i]=Rect::from_min_max(egui::pos2(cx,current_y),egui::pos2(cx+w,current_y+h));
            cx+=w;
        }
    }
    out
}
