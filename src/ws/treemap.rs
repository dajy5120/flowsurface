//! 市场树图布局：squarified treemap（Bruls / Huizing / van Wijk 2000）。docs/22 §6。
//!
//! **纯几何，不依赖 iced**——布局是这个面板唯一会算错的地方，独立出来全单测。
//!
//! ⚠ 命名：本项目的 `chart/heatmap.rs` 与 `widget/chart/heatmap/` 已经是**订单簿深度热图**
//! （价格×时间）。市场树图叫 `treemap`，别混（docs/22 §9 坑 1）。

/// 布局矩形（左上原点，与 iced canvas 一致）。
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Rect {
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
}

impl Rect {
    pub fn new(x: f32, y: f32, w: f32, h: f32) -> Self {
        Self { x, y, w, h }
    }
    pub fn area(&self) -> f64 {
        (self.w as f64).max(0.0) * (self.h as f64).max(0.0)
    }
    fn shorter_side(&self) -> f64 {
        (self.w.min(self.h)) as f64
    }
}

/// 一块瓦片：`idx` 是输入 `weights` 里的原始下标（调用方据此回查标的）。
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Tile {
    pub idx: usize,
    pub rect: Rect,
}

/// 一行候选的最差长宽比：`max(w²·rmax/s², s²/(w²·rmin))`。越小越接近正方形。
fn worst(row: &[f64], side: f64) -> f64 {
    if row.is_empty() || side <= 0.0 {
        return f64::INFINITY;
    }
    let s: f64 = row.iter().sum();
    if s <= 0.0 {
        return f64::INFINITY;
    }
    let rmax = row.iter().cloned().fold(f64::MIN, f64::max);
    let rmin = row.iter().cloned().fold(f64::MAX, f64::min);
    if rmin <= 0.0 {
        return f64::INFINITY;
    }
    let ss = s * s;
    let w2 = side * side;
    ((w2 * rmax) / ss).max(ss / (w2 * rmin))
}

/// 把一行铺进矩形的短边一侧，返回剩余矩形。
fn place_row(row: &[(usize, f64)], rect: &mut Rect, out: &mut Vec<Tile>) {
    let total: f64 = row.iter().map(|(_, v)| *v).sum();
    if total <= 0.0 {
        return;
    }
    let horizontal = rect.w <= rect.h; // 短边是宽 → 行沿水平方向铺
    if horizontal {
        let band_h = (total / rect.w as f64) as f32;
        let mut x = rect.x;
        for (idx, v) in row {
            let tw = (v / total) as f32 * rect.w;
            out.push(Tile {
                idx: *idx,
                rect: Rect::new(x, rect.y, tw, band_h),
            });
            x += tw;
        }
        rect.y += band_h;
        rect.h -= band_h;
    } else {
        let band_w = (total / rect.h as f64) as f32;
        let mut y = rect.y;
        for (idx, v) in row {
            let th = (v / total) as f32 * rect.h;
            out.push(Tile {
                idx: *idx,
                rect: Rect::new(rect.x, y, band_w, th),
            });
            y += th;
        }
        rect.x += band_w;
        rect.w -= band_w;
    }
}

/// 按权重把 `bounds` 切成瓦片。
///
/// - `weights` 不必有序、不必归一：内部按面积占比缩放并降序处理（squarify 要求降序）。
/// - 非正权重（0/负/NaN）直接丢弃——树图里面积为 0 的格子没有意义，画出来只会是噪声线。
/// - 输出顺序按面积降序，方便调用方「只给大格子画标签」做 LOD（docs/22 §6）。
pub fn squarify(weights: &[f64], bounds: Rect) -> Vec<Tile> {
    let mut items: Vec<(usize, f64)> = weights
        .iter()
        .enumerate()
        .filter(|(_, v)| v.is_finite() && **v > 0.0)
        .map(|(i, v)| (i, *v))
        .collect();
    if items.is_empty() || bounds.area() <= 0.0 {
        return Vec::new();
    }
    items.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));

    // 缩放到「权重和 == 矩形面积」，之后所有长度计算都能直接用面积/边长
    let total: f64 = items.iter().map(|(_, v)| *v).sum();
    let scale = bounds.area() / total;
    for it in &mut items {
        it.1 *= scale;
    }

    let mut out = Vec::with_capacity(items.len());
    let mut rect = bounds;
    let mut row: Vec<(usize, f64)> = Vec::new();

    for it in items {
        let side = rect.shorter_side();
        let mut vals: Vec<f64> = row.iter().map(|(_, v)| *v).collect();
        let cur = worst(&vals, side);
        vals.push(it.1);
        let next = worst(&vals, side);
        if row.is_empty() || next <= cur {
            row.push(it);
        } else {
            place_row(&row, &mut rect, &mut out);
            row.clear();
            row.push(it);
        }
    }
    if !row.is_empty() {
        place_row(&row, &mut rect, &mut out);
    }
    out
}

/// 一个分组的布局结果。
#[derive(Clone, Debug, PartialEq)]
pub struct GroupLayout {
    /// 在入参 `members` 里的下标。
    pub group_idx: usize,
    /// 整组矩形（含标题带）。
    pub rect: Rect,
    /// 标题带（`header_h = 0` 时高度为 0）。
    pub header: Rect,
    /// 组内瓦片。`Tile::idx` 是该组 `members[group_idx]` 内的下标，**不是全局下标**。
    pub tiles: Vec<Tile>,
}

/// 分组树图（TradingView 股票热图那种：先按板块切大块，块内再切个股）。
///
/// 两级都用 [`squarify`]：外层按各组权重和切分，内层在扣掉标题带后的剩余矩形里切分。
/// 组太小以致扣完标题带没剩下空间时，该组只保留标题带、不出瓦片——
/// 强行画会得到高度为负的格子。
pub fn squarify_nested(members: &[Vec<f64>], bounds: Rect, header_h: f32) -> Vec<GroupLayout> {
    let totals: Vec<f64> = members
        .iter()
        .map(|m| m.iter().filter(|v| v.is_finite() && **v > 0.0).sum())
        .collect();
    let mut out = Vec::new();
    for g in squarify(&totals, bounds) {
        let r = g.rect;
        let hh = header_h.min(r.h);
        let header = Rect::new(r.x, r.y, r.w, hh);
        let inner = Rect::new(r.x, r.y + hh, r.w, r.h - hh);
        let tiles = if inner.area() > 0.0 {
            squarify(&members[g.idx], inner)
        } else {
            Vec::new()
        };
        out.push(GroupLayout {
            group_idx: g.idx,
            rect: r,
            header,
            tiles,
        });
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    const B: Rect = Rect {
        x: 0.0,
        y: 0.0,
        w: 400.0,
        h: 300.0,
    };

    fn overlaps(a: &Rect, b: &Rect) -> bool {
        let eps = 1e-3;
        a.x + eps < b.x + b.w && b.x + eps < a.x + a.w && a.y + eps < b.y + b.h && b.y + eps < a.y + a.h
    }

    #[test]
    fn tiles_cover_bounds_without_overlap() {
        let w: Vec<f64> = (1..=17).map(|i| i as f64 * 1.7).collect();
        let tiles = squarify(&w, B);
        assert_eq!(tiles.len(), w.len());

        let covered: f64 = tiles.iter().map(|t| t.rect.area()).sum();
        assert!(
            (covered - B.area()).abs() / B.area() < 1e-3,
            "覆盖面积 {covered} vs {}",
            B.area()
        );
        for (i, a) in tiles.iter().enumerate() {
            for b in &tiles[i + 1..] {
                assert!(!overlaps(&a.rect, &b.rect), "瓦片重叠：{a:?} / {b:?}");
            }
        }
    }

    #[test]
    fn tiles_stay_inside_bounds() {
        let w: Vec<f64> = (1..=40).map(|i| (41 - i) as f64).collect();
        for t in squarify(&w, B) {
            let e = 1e-3;
            assert!(t.rect.x >= B.x - e && t.rect.y >= B.y - e, "{t:?}");
            assert!(t.rect.x + t.rect.w <= B.x + B.w + e, "{t:?}");
            assert!(t.rect.y + t.rect.h <= B.y + B.h + e, "{t:?}");
        }
    }

    #[test]
    fn area_is_proportional_to_weight() {
        let w = vec![50.0, 30.0, 20.0];
        let tiles = squarify(&w, B);
        let total = B.area();
        for t in &tiles {
            let expect = total * w[t.idx] / 100.0;
            assert!(
                (t.rect.area() - expect).abs() / expect < 1e-2,
                "idx={} area={} expect={expect}",
                t.idx,
                t.rect.area()
            );
        }
    }

    #[test]
    fn output_is_area_descending_for_lod() {
        let w = vec![1.0, 9.0, 3.0, 7.0];
        let tiles = squarify(&w, B);
        let areas: Vec<f64> = tiles.iter().map(|t| t.rect.area()).collect();
        for pair in areas.windows(2) {
            assert!(pair[0] >= pair[1] - 1e-6, "输出未按面积降序：{areas:?}");
        }
        assert_eq!(tiles[0].idx, 1, "最大格应是原下标 1");
    }

    #[test]
    fn aspect_ratios_are_reasonable() {
        // squarify 的全部意义就是别出细长条。20 个同权重格子的长宽比该在个位数内。
        let w = vec![1.0; 20];
        for t in squarify(&w, B) {
            let ar = (t.rect.w / t.rect.h).max(t.rect.h / t.rect.w);
            assert!(ar < 5.0, "长宽比 {ar} 过大：{t:?}");
        }
    }

    #[test]
    fn drops_non_positive_and_nan_weights() {
        let w = vec![5.0, 0.0, -3.0, f64::NAN, 5.0];
        let tiles = squarify(&w, B);
        assert_eq!(tiles.len(), 2);
        let idxs: Vec<usize> = tiles.iter().map(|t| t.idx).collect();
        assert!(idxs.contains(&0) && idxs.contains(&4));
    }

    #[test]
    fn degenerate_inputs_return_empty() {
        assert!(squarify(&[], B).is_empty());
        assert!(squarify(&[1.0], Rect::new(0.0, 0.0, 0.0, 100.0)).is_empty());
        assert!(squarify(&[0.0, -1.0], B).is_empty());
    }

    #[test]
    fn nested_groups_partition_bounds_and_reserve_headers() {
        let members = vec![vec![3.0, 2.0, 1.0], vec![4.0, 4.0], vec![10.0]];
        let hh = 14.0;
        let gs = squarify_nested(&members, B, hh);
        assert_eq!(gs.len(), 3);

        // 组矩形之间不重叠，且合起来铺满 bounds
        let covered: f64 = gs.iter().map(|g| g.rect.area()).sum();
        assert!((covered - B.area()).abs() / B.area() < 1e-3);
        for (i, a) in gs.iter().enumerate() {
            for b in &gs[i + 1..] {
                assert!(!overlaps(&a.rect, &b.rect), "组矩形重叠");
            }
        }

        for g in &gs {
            assert!((g.header.h - hh).abs() < 1e-3, "标题带高度不对：{:?}", g.header);
            assert_eq!(g.tiles.len(), members[g.group_idx].len());
            // 组内瓦片必须落在扣掉标题带之后的区域里
            for t in &g.tiles {
                assert!(
                    t.rect.y >= g.rect.y + hh - 1e-3,
                    "瓦片侵入标题带：{:?} vs 组 {:?}",
                    t.rect,
                    g.rect
                );
                assert!(t.rect.y + t.rect.h <= g.rect.y + g.rect.h + 1e-3);
            }
        }
    }

    #[test]
    fn nested_group_too_small_for_header_emits_no_tiles() {
        // 组高度不足标题带 → 只留标题带，不画负高度的格子
        let members = vec![vec![1.0; 4], vec![1e-6]];
        let gs = squarify_nested(&members, Rect::new(0.0, 0.0, 400.0, 20.0), 18.0);
        let tiny = gs.iter().find(|g| g.group_idx == 1).unwrap();
        assert!(tiny.tiles.is_empty() || tiny.rect.h > 18.0);
        for g in &gs {
            for t in &g.tiles {
                assert!(t.rect.h >= 0.0 && t.rect.w >= 0.0, "出现负尺寸瓦片 {t:?}");
            }
        }
    }

    #[test]
    fn nested_with_zero_header_is_plain_two_level_split() {
        let members = vec![vec![1.0, 1.0], vec![2.0]];
        let gs = squarify_nested(&members, B, 0.0);
        let area: f64 = gs.iter().flat_map(|g| &g.tiles).map(|t| t.rect.area()).sum();
        assert!((area - B.area()).abs() / B.area() < 1e-3, "零标题带时应铺满");
    }

    #[test]
    fn single_weight_fills_bounds() {
        let t = squarify(&[42.0], B);
        assert_eq!(t.len(), 1);
        assert!((t[0].rect.area() - B.area()).abs() / B.area() < 1e-4);
    }
}
