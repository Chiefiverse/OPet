//! `polygon.json` 的逐列 y 区间 → `wl_region` 能吃的矩形列表。
//!
//! 内核的 [`petcore::intent::InputRegion`] 是**多边形**，且 `Sim::intent()` 只给整格粗近似；
//! Wayland 的点击穿透靠 `wl_surface.set_input_region`，而 `wl_region` **只接受矩形**，
//! 所以逐帧精确轮廓必须在客户端从 `pack.py` 产的 span 表还原。
//!
//! `polygon.json` 的 `spans` 是 `[[x, y_top, y_bottom], …]`，两端**闭区间**（`tools/pack.py:column_spans`）。
//! 相邻且 y 区间相同的列合并成一个 run，实测把每格 ~40 个 span 压到几个矩形。

use std::collections::BTreeMap;
use std::io::Read;

use petcore::intent::SURFACE_MARGIN;
use serde::Deserialize;

/// surface 局部坐标里的一个矩形，整数像素（`wl_region` 的口径）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CellRect {
    pub x: i32,
    pub y: i32,
    pub w: i32,
    pub h: i32,
}

impl CellRect {
    pub fn new(x: i32, y: i32, w: i32, h: i32) -> Self {
        CellRect { x, y, w, h }
    }

    pub fn is_empty(&self) -> bool {
        self.w <= 0 || self.h <= 0
    }
}

/// 一列的 y 覆盖区间（闭区间）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Span {
    pub x: u32,
    pub y0: u32,
    pub y1: u32,
}

/// 把 span 表合并成 run，返回 `(x, y, w, h)`（**格子局部**，`h` 已按闭区间 +1）。
///
/// 输入可以不有序（`pack.py` 是有序的，但这里不依赖它）；同列重复只保留并集范围，
/// 否则一个畸形 polygon.json 会让 region 出现负宽高。
pub fn coalesce_spans(spans: &[Span]) -> Vec<(u32, u32, u32, u32)> {
    if spans.is_empty() {
        return Vec::new();
    }
    // 同列取并集：BTreeMap 保证按 x 有序且唯一
    let mut by_x: BTreeMap<u32, (u32, u32)> = BTreeMap::new();
    for s in spans {
        // ⚠️ 先把区间摆正：畸形 span（`y1 < y0`）若直接取并集，下面 `y1 - y0 + 1`
        // 会在 debug 下 panic、release 下回绕成一个巨大高度 ⇒ 整列都可点，穿透失效。
        let (lo, hi) = (s.y0.min(s.y1), s.y0.max(s.y1));
        let e = by_x.entry(s.x).or_insert((lo, hi));
        e.0 = e.0.min(lo);
        e.1 = e.1.max(hi);
    }
    let mut runs: Vec<(u32, u32, u32, u32)> = Vec::new();
    for (x, (y0, y1)) in by_x {
        let h = y1 - y0 + 1;
        match runs.last_mut() {
            // 紧邻且 y 区间相同 ⇒ 同一个矩形
            Some(last) if last.0 + last.2 == x && last.1 == y0 && last.3 == h => last.2 += 1,
            _ => runs.push((x, y0, 1, h)),
        }
    }
    runs
}

/// run + 边距 → surface 局部矩形，并 clamp 进 `[0, surface_w] × [0, surface_h]`。
///
/// 越界的 run 会被裁掉而不是报错：`polygon.json` 是烘焙产物，一列超出格子说明上游 bug，
/// 但穿透区域宁可少一点也不能让 `wl_region` 收到负尺寸（协议层会直接杀连接）。
pub fn rects_with_margin(
    spans: &[Span],
    margin: i32,
    surface_w: i32,
    surface_h: i32,
) -> Vec<CellRect> {
    coalesce_spans(spans)
        .into_iter()
        .filter_map(|(x, y, w, h)| {
            let mut rr = CellRect::new(x as i32 + margin, y as i32 + margin, w as i32, h as i32);
            if rr.is_empty() {
                return None;
            }
            // clamp 左/上
            if rr.x < 0 {
                rr.w += rr.x;
                rr.x = 0;
            }
            if rr.y < 0 {
                rr.h += rr.y;
                rr.y = 0;
            }
            // clamp 右/下
            if rr.x + rr.w > surface_w {
                rr.w = surface_w - rr.x;
            }
            if rr.y + rr.h > surface_h {
                rr.h = surface_h - rr.y;
            }
            (!rr.is_empty()).then_some(rr)
        })
        .collect()
}

#[derive(Deserialize)]
struct PolygonFile {
    cell: CellJson,
    frames: Vec<FrameJson>,
}

#[derive(Deserialize)]
struct CellJson {
    w: u32,
    h: u32,
}

#[derive(Deserialize)]
struct FrameJson {
    row: u32,
    col: u32,
    spans: Vec<[u32; 3]>,
}

/// 全部格子的穿透矩形，按 `(row, col)` 索引。
#[derive(Debug)]
pub struct Regions {
    pub cell_w: u32,
    pub cell_h: u32,
    frames: BTreeMap<(u32, u32), Vec<CellRect>>,
    margin: i32,
    surface_w: i32,
    surface_h: i32,
}

impl Regions {
    /// 读 `polygon.json`。`grid_*` 与清单必须一致，否则说明资产不同批产出。
    pub fn load(path: &str, grid_w: u32, grid_h: u32, margin: f32) -> Result<Regions, String> {
        let mut buf = Vec::new();
        std::fs::File::open(path)
            .map_err(|e| format!("读 {path} 失败: {e}"))?
            .read_to_end(&mut buf)
            .map_err(|e| format!("读 {path} 失败: {e}"))?;
        let file: PolygonFile =
            serde_json::from_slice(&buf).map_err(|e| format!("解析 {path} 失败: {e}"))?;
        if file.cell.w != grid_w || file.cell.h != grid_h {
            return Err(format!(
                "{path} 的 cell {}x{} 与清单 grid {grid_w}x{grid_h} 不一致",
                file.cell.w, file.cell.h
            ));
        }
        let margin = margin.round() as i32;
        // surface = 格子 + 四边余量（内核 sim.rs:195-201 的口径）
        let surface_w = grid_w as i32 + margin * 2;
        let surface_h = grid_h as i32 + margin * 2;
        let mut frames: BTreeMap<(u32, u32), Vec<CellRect>> = BTreeMap::new();
        for f in file.frames {
            let spans: Vec<Span> = f
                .spans
                .iter()
                .map(|[x, y0, y1]| Span {
                    x: *x,
                    y0: *y0,
                    y1: *y1,
                })
                .collect();
            let rects = rects_with_margin(&spans, margin, surface_w, surface_h);
            frames.insert((f.row, f.col), rects);
        }
        Ok(Regions {
            cell_w: grid_w,
            cell_h: grid_h,
            frames,
            margin,
            surface_w,
            surface_h,
        })
    }

    /// 该格的矩形列表。`None` = 资产里没有这格（调用方退化成整格区域）。
    /// 全透明帧给的是 `Some(空)`：此时应当**什么都不遮**，即空 region = 完全穿透。
    pub fn rects(&self, row: u32, col: u32) -> Option<&[CellRect]> {
        self.frames.get(&(row, col)).map(|v| &v[..])
    }

    pub fn frame_count(&self) -> usize {
        self.frames.len()
    }

    /// 整块 surface（拖拽期间用：快速甩动会让指针滑出剪影，滑出去就丢 pointer focus，
    /// `Release` 永不到达 ⇒ 卡在 `Dragged`）。
    pub fn full_surface(&self) -> Vec<CellRect> {
        vec![CellRect::new(0, 0, self.surface_w, self.surface_h)]
    }

    pub fn surface_size(&self) -> (i32, i32) {
        (self.surface_w, self.surface_h)
    }

    pub fn margin(&self) -> i32 {
        self.margin
    }
}

/// 缺省边距：与内核同源。
pub const DEFAULT_MARGIN: f32 = SURFACE_MARGIN;

#[cfg(test)]
mod tests {
    use super::*;

    fn span(x: u32, y0: u32, y1: u32) -> Span {
        Span { x, y0, y1 }
    }

    #[test]
    fn adjacent_same_span_columns_merge_into_one_run() {
        // 闭区间 10..20 ⇒ 高 11
        let runs = coalesce_spans(&[span(3, 10, 20), span(4, 10, 20), span(5, 10, 20)]);
        assert_eq!(runs, vec![(3, 10, 3, 11)]);
    }

    #[test]
    fn differing_y_range_starts_a_new_run() {
        let runs = coalesce_spans(&[span(0, 5, 9), span(1, 5, 8)]);
        assert_eq!(runs, vec![(0, 5, 1, 5), (1, 5, 1, 4)]);
    }

    #[test]
    fn gap_between_columns_breaks_the_run() {
        let runs = coalesce_spans(&[span(0, 5, 9), span(2, 5, 9)]);
        assert_eq!(runs, vec![(0, 5, 1, 5), (2, 5, 1, 5)]);
    }

    #[test]
    fn unsorted_input_is_sorted_and_duplicated_columns_unioned() {
        // 乱序 + 同列重复：x=1 的两条并成 6..12，不产生负宽高
        let runs = coalesce_spans(&[span(1, 8, 12), span(0, 8, 12), span(1, 6, 9)]);
        assert_eq!(runs, vec![(0, 8, 1, 5), (1, 6, 1, 7)]);
    }

    #[test]
    fn empty_spans_give_no_rects() {
        assert!(coalesce_spans(&[]).is_empty());
        assert!(rects_with_margin(&[], 16, 128, 128).is_empty());
    }

    #[test]
    fn margin_offsets_cell_local_to_surface_local() {
        let r = rects_with_margin(&[span(0, 0, 0)], 16, 128, 128);
        assert_eq!(r, vec![CellRect::new(16, 16, 1, 1)]);
    }

    #[test]
    fn closed_interval_span_becomes_height_one_longer() {
        // y0=10,y1=20 闭区间 ⇒ 覆盖 11 行
        let r = rects_with_margin(&[span(2, 10, 20)], 0, 96, 96);
        assert_eq!(r, vec![CellRect::new(2, 10, 1, 11)]);
    }

    #[test]
    fn reversed_span_is_sorted_instead_of_underflowing() {
        // 畸形产物 `y1 < y0`：不摆正的话 `y1 - y0 + 1` 在 debug 下 panic、release 下回绕
        // 成一个巨高矩形 ⇒ 整列可点、穿透失效。这里只允许"摆正"。
        let r = rects_with_margin(&[span(2, 20, 10)], 0, 96, 96);
        assert_eq!(r, vec![CellRect::new(2, 10, 1, 11)]);
        let mixed = rects_with_margin(&[span(2, 20, 10), span(3, 10, 20)], 0, 96, 96);
        assert_eq!(
            mixed,
            vec![CellRect::new(2, 10, 2, 11)],
            "摆正后应与正序结果合并"
        );
    }

    #[test]
    fn rects_are_clamped_into_surface_and_dropped_if_empty() {
        // 格子 96 + 边距 16 ⇒ surface 128。一列伸到 x=200 必须被裁进 surface 内。
        let r = rects_with_margin(&[span(200, 0, 50)], 16, 128, 128);
        assert!(
            r.is_empty(),
            "完全在 surface 外的列应被丢弃，不能给 wl_region 负尺寸"
        );
        // y 方向越界 ⇒ 裁短
        let r = rects_with_margin(&[span(10, 100, 400)], 16, 128, 128);
        assert_eq!(r, vec![CellRect::new(26, 116, 1, 12)]);
    }

    #[test]
    fn full_surface_covers_the_whole_surface() {
        let spans: Vec<Span> = (0..96).map(|x| span(x, 0, 95)).collect();
        let file = format!(
            "{{\"cell\":{{\"w\":96,\"h\":96}},\"threshold\":8,\"frames\":[{{\"row\":0,\"col\":0,\"spans\":{}}}]}}",
            serde_json::to_string(
                &spans
                    .iter()
                    .map(|s| [s.x, s.y0, s.y1])
                    .collect::<Vec<[u32; 3]>>()
            )
            .unwrap()
        );
        let dir = std::env::temp_dir().join(format!("opet-region-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("polygon.json");
        std::fs::write(&path, &file).unwrap();
        let reg = Regions::load(path.to_str().unwrap(), 96, 96, DEFAULT_MARGIN).unwrap();
        assert_eq!(reg.frame_count(), 1);
        assert_eq!(reg.surface_size(), (128, 128));
        // 一整列连续的格子被合成一个矩形，且整体加了边距
        assert_eq!(reg.rects(0, 0), Some(&[CellRect::new(16, 16, 96, 96)][..]));
        assert_eq!(reg.rects(9, 9), None, "资产里没有的格要退化成整格");
        assert_eq!(reg.full_surface(), vec![CellRect::new(0, 0, 128, 128)]);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn load_rejects_cell_size_mismatch() {
        let dir = std::env::temp_dir().join(format!("opet-region-bad-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("polygon.json");
        std::fs::write(
            &path,
            r#"{"cell":{"w":128,"h":128},"threshold":8,"frames":[]}"#,
        )
        .unwrap();
        let e = Regions::load(path.to_str().unwrap(), 96, 96, 16.0).unwrap_err();
        assert!(e.contains("不一致"), "{e}");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
