//! 内核每帧输出的"呈现意图"（§3.1 分层原则：petcore 不知道窗口是什么）。

use crate::assets::scheduler::SpriteFrame;
use crate::behavior::State;
use crate::geom::{Facing, Rect, Vec2};

/// layer surface 相对宠物包围盒的余量：给阴影描边留位置，同时保证 input region 不越界。
/// 见 AGENTS.md「surface 用宠物包围盒 + 16px 余量，不要铺全屏」。
pub const SURFACE_MARGIN: f32 = 16.0;

/// 输入区域：surface 局部坐标下的多边形（左上角为原点）。
///
/// Wayland 下直接 `set_input_region`；Windows/macOS 用它做"指针是否命中宠物"的判定（§3.3 穿透策略）。
#[derive(Clone, Debug, PartialEq)]
pub struct InputRegion {
    pub polygon: Vec<Vec2>,
}

impl InputRegion {
    /// 整格矩形（M2 之前没有逐帧 alpha 多边形时的退化形式，由 pack.py 之后被真实多边形替换）。
    pub fn rect(w: f32, h: f32, margin: f32) -> InputRegion {
        InputRegion {
            polygon: vec![
                Vec2::new(margin, margin),
                Vec2::new(margin + w, margin),
                Vec2::new(margin + w, margin + h),
                Vec2::new(margin, margin + h),
            ],
        }
    }

    /// 把 pack.py 给出的"帧内多边形"平移到 surface 局部坐标。
    pub fn from_cell_polygon(cell: &[Vec2], margin: f32) -> InputRegion {
        InputRegion {
            polygon: cell
                .iter()
                .map(|p| Vec2::new(p.x + margin, p.y + margin))
                .collect(),
        }
    }

    pub fn is_empty(&self) -> bool {
        self.polygon.len() < 3
    }

    /// 射线法（even-odd）命中测试。
    pub fn contains(&self, p: Vec2) -> bool {
        let n = self.polygon.len();
        if n < 3 {
            return false;
        }
        let mut inside = false;
        let mut j = n - 1;
        for i in 0..n {
            let (a, b) = (self.polygon[i], self.polygon[j]);
            if (a.y > p.y) != (b.y > p.y) {
                let t = (p.x - a.x) * (b.y - a.y) - (b.x - a.x) * (p.y - a.y);
                // 跨边的方向由 (b.y - a.y) 的符号决定，避免除零
                if (t < 0.0) != (b.y - a.y < 0.0) {
                    inside = !inside;
                }
            }
            j = i;
        }
        inside
    }
}

/// 一帧要呈现的全部内容。渲染适配层把它变成平台像素。
#[derive(Clone, Debug)]
pub struct PresentationIntent {
    pub tick: u64,
    pub state: State,
    pub facing: Facing,
    /// 当前图集格；`None` = 本 tick 与上一 tick 完全一致 ⇒ 不提交新 buffer
    pub sprite: Option<SpriteFrame>,
    /// 脚底锚点（全局虚拟桌面逻辑坐标）
    pub anchor: Vec2,
    /// surface 在世界坐标里的位置（含 [`SURFACE_MARGIN`] 余量）
    pub surface: Rect,
    /// surface 局部坐标下的可点击区域
    pub input_region: InputRegion,
    pub needs_redraw: bool,
}

impl PresentationIntent {
    /// surface 左上角在世界坐标的位置。
    pub fn surface_origin(&self) -> Vec2 {
        self.surface.pos
    }

    /// 世界坐标点 → surface 局部坐标点（M4 把 layer surface 内的指针事件换算给命中测试）。
    pub fn to_local(&self, world: Vec2) -> Vec2 {
        Vec2::new(world.x - self.surface.pos.x, world.y - self.surface.pos.y)
    }

    /// 世界坐标里宠物是否包含该点（Win/mac 的近似穿透与 Wayland 的命中判定共用）。
    pub fn hit_test_world(&self, world: Vec2) -> bool {
        self.input_region.contains(self.to_local(world))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rect_region_hits_inside_only() {
        let r = InputRegion::rect(100.0, 100.0, 16.0);
        assert!(r.contains(Vec2::new(50.0, 50.0)));
        assert!(r.contains(Vec2::new(110.0, 110.0)));
        assert!(!r.contains(Vec2::new(5.0, 5.0)), "margin 里不该吃输入");
        assert!(!r.contains(Vec2::new(130.0, 60.0)));
    }

    #[test]
    fn concave_polygon_uses_even_odd() {
        // L 形：右上角缺了一块
        let region = InputRegion {
            polygon: vec![
                Vec2::new(0.0, 0.0),
                Vec2::new(100.0, 0.0),
                Vec2::new(100.0, 50.0),
                Vec2::new(50.0, 50.0),
                Vec2::new(50.0, 100.0),
                Vec2::new(0.0, 100.0),
            ],
        };
        assert!(region.contains(Vec2::new(10.0, 10.0)));
        assert!(!region.contains(Vec2::new(80.0, 80.0)), "L 形缺口外");
        assert!(region.contains(Vec2::new(20.0, 80.0)));
    }

    #[test]
    fn degenerate_regions_never_hit() {
        let empty = InputRegion { polygon: vec![] };
        assert!(empty.is_empty());
        assert!(!empty.contains(Vec2::new(1.0, 1.0)));
        let triangle = InputRegion {
            polygon: vec![
                Vec2::new(0.0, 0.0),
                Vec2::new(1.0, 0.0),
                Vec2::new(0.0, 1.0),
            ],
        };
        assert!(!triangle.is_empty());
    }
}
