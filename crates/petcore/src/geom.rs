//! 逻辑几何类型。坐标单位为"逻辑像素"（已除以 scale），原点在全局虚拟桌面左上角，y 向下增大。

use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Vec2 {
    pub x: f32,
    pub y: f32,
}

impl Vec2 {
    pub const ZERO: Vec2 = Vec2 { x: 0.0, y: 0.0 };

    pub const fn new(x: f32, y: f32) -> Self {
        Self { x, y }
    }

    pub fn dot(self, other: Vec2) -> f32 {
        self.x * other.x + self.y * other.y
    }

    pub fn length(self) -> f32 {
        self.dot(self).sqrt()
    }

    pub fn is_finite(self) -> bool {
        self.x.is_finite() && self.y.is_finite()
    }

    pub fn clamp_x(self, lo: f32, hi: f32) -> Vec2 {
        Vec2 {
            x: self.x.clamp(lo, hi),
            y: self.y,
        }
    }
}

/// 轴对齐矩形。`y` 是上沿（y 向下增大 ⇒ `bottom() > top()`）。
#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Rect {
    pub pos: Vec2,
    pub size: Vec2,
}

impl Rect {
    pub const fn new(x: f32, y: f32, w: f32, h: f32) -> Self {
        Self {
            pos: Vec2::new(x, y),
            size: Vec2::new(w, h),
        }
    }

    pub fn top(&self) -> f32 {
        self.pos.y
    }

    pub fn bottom(&self) -> f32 {
        self.pos.y + self.size.y
    }

    pub fn left(&self) -> f32 {
        self.pos.x
    }

    pub fn right(&self) -> f32 {
        self.pos.x + self.size.x
    }

    pub fn contains_x(&self, x: f32) -> bool {
        x >= self.left() && x < self.right()
    }

    pub fn contains(&self, p: Vec2) -> bool {
        self.contains_x(p.x) && p.y >= self.top() && p.y < self.bottom()
    }

    pub fn inflated(self, margin: f32) -> Rect {
        Rect::new(
            self.left() - margin,
            self.top() - margin,
            self.size.x + margin * 2.0,
            self.size.y + margin * 2.0,
        )
    }
}

/// 序列帧图集的 8 个朝向（D1）。方位角以"水平面内顺时针"度量，0° = 屏幕右（East）。
///
/// `index()` 即图集列号，是与 `tools/bake.py` 的契约：Blender 相机按本顺序在 0°..315° 每 45° 摆一次，
/// 俯视 15°。行走类动作只会用到 `East`/`West` 两列，`South*`/`North*` 供转身与爬升的过渡帧使用。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Facing {
    East,
    SouthEast,
    South,
    SouthWest,
    West,
    NorthWest,
    North,
    NorthEast,
}

impl Facing {
    pub const ALL: [Facing; 8] = [
        Facing::East,
        Facing::SouthEast,
        Facing::South,
        Facing::SouthWest,
        Facing::West,
        Facing::NorthWest,
        Facing::North,
        Facing::NorthEast,
    ];

    pub const fn index(self) -> u8 {
        match self {
            Facing::East => 0,
            Facing::SouthEast => 1,
            Facing::South => 2,
            Facing::SouthWest => 3,
            Facing::West => 4,
            Facing::NorthWest => 5,
            Facing::North => 6,
            Facing::NorthEast => 7,
        }
    }

    pub const fn yaw_deg(self) -> f32 {
        self.index() as f32 * 45.0
    }

    pub fn from_yaw(mut yaw: f32) -> Facing {
        yaw = yaw.rem_euclid(360.0);
        Facing::ALL[((yaw + 22.5) / 45.0) as usize % 8]
    }

    /// 朝屏幕左半边（决定动画的锚点与配件镜像）。
    pub const fn is_leftward(self) -> bool {
        matches!(
            self,
            Facing::West | Facing::SouthWest | Facing::NorthWest | Facing::South | Facing::North
        )
    }

    /// 水平方向单位向量，用于物理积分。
    pub const fn horizontal_sign(self) -> f32 {
        match self {
            Facing::East | Facing::SouthEast | Facing::NorthEast => 1.0,
            Facing::West | Facing::SouthWest | Facing::NorthWest => -1.0,
            Facing::South | Facing::North => 0.0,
        }
    }

    pub fn from_horizontal(right: bool) -> Facing {
        if right { Facing::East } else { Facing::West }
    }
}

/// 锚点：宠物精灵图上用于对齐世界坐标的像素点（默认脚底）。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Anchor {
    Feet,
    Center,
    Head,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn facing_index_roundtrip() {
        for f in Facing::ALL {
            assert_eq!(Facing::from_yaw(f.yaw_deg()), f);
        }
    }

    #[test]
    fn facing_quantizes_to_nearest_octant() {
        assert_eq!(Facing::from_yaw(20.0), Facing::East);
        assert_eq!(Facing::from_yaw(25.0), Facing::SouthEast);
        // y 向下增大 ⇒ 负 yaw 是"抬头"，但 0° 附近仍归 East
        assert_eq!(Facing::from_yaw(-1.0), Facing::East);
        assert_eq!(Facing::from_yaw(-30.0), Facing::NorthEast);
        assert_eq!(Facing::from_yaw(359.0), Facing::East);
        assert_eq!(Facing::from_yaw(180.0), Facing::West);
        assert_eq!(Facing::West.horizontal_sign(), -1.0);
        assert!(Facing::West.is_leftward());
        assert!(!Facing::East.is_leftward());
    }
}
