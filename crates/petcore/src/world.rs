//! 世界模型：显示器=舞台，地面=屏幕底边或面板上沿，墙=舞台左右边，可停靠平台=窗口顶边/面板（F2）。
//!
//! 坐标是"全局虚拟桌面"逻辑像素（支持多屏异分辨率与不同 scale，§3.2）。宠物一次只在一个舞台上活动，
//! 换舞台由 [`WorldEvent`] 驱动，不做跨屏连续行走。

use crate::geom::Rect;
use serde::{Deserialize, Serialize};

/// 判定"同一个 y"用的容差：半像素。序列帧尺寸是整数，落位不需要更高精度。
pub const SUPPORT_EPS: f32 = 0.5;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum PlatformKind {
    /// 任务栏/面板上沿（Omarchy 下由 Quickshell 上报 exclusive zone，L3）
    Panel,
    /// 某个真实窗口的顶边（Omarchy 下由 Hyprland IPC 提供几何，L2）
    WindowTopEdge,
}

/// 宠物可站立的表面。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Support {
    /// 舞台地面（屏幕底边或面板上沿）
    Ground,
    /// 舞台顶边：爬到顶后坐在这里，让 L1 复用 Climb→WallSit 一条路径
    Ceiling,
    /// 注册平台的槽位号
    Platform(u32),
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Platform {
    /// World 分配的内部槽位，`Support::Platform` 引用它；同一 `id` 更新时槽位不变
    pub slot: u32,
    /// 稳定标识：窗口地址、面板名，用于增量更新与外部引用
    pub id: String,
    pub kind: PlatformKind,
    pub monitor: String,
    /// 可停靠区间（含左不含右）与其上沿 y
    pub x0: f32,
    pub x1: f32,
    pub top_y: f32,
}

impl Platform {
    pub fn spans(&self, x: f32) -> bool {
        x >= self.x0 && x < self.x1
    }

    pub fn width(&self) -> f32 {
        self.x1 - self.x0
    }
}

/// 注册平台时提交的几何（槽位由 World 分配）。
#[derive(Clone, Debug, PartialEq)]
pub struct PlatformSpec {
    pub id: String,
    pub kind: PlatformKind,
    pub monitor: String,
    pub x0: f32,
    pub x1: f32,
    pub top_y: f32,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Monitor {
    pub name: String,
    pub rect: Rect,
    pub scale: f32,
    pub primary: bool,
    /// 底边被面板占掉的高度（`exclusive_zone`）。地面 = rect.bottom() - panel_bottom。
    pub panel_bottom: f32,
}

impl Monitor {
    pub fn ground_y(&self) -> f32 {
        self.rect.bottom() - self.panel_bottom
    }

    pub fn ceiling_y(&self) -> f32 {
        self.rect.top()
    }

    pub fn contains_x(&self, x: f32) -> bool {
        self.rect.contains_x(x)
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct World {
    pub monitors: Vec<Monitor>,
    pub platforms: Vec<Platform>,
    pub stage: usize,
    next_slot: u32,
}

/// 世界几何变化事件。内核全部事件驱动，不做定时轮询（硬约束 5）。
#[derive(Clone, Debug, PartialEq)]
pub enum WorldEvent {
    /// 某个显示器的几何变了（分辨率/缩放/面板高度调整）
    MonitorGeometry {
        monitor: String,
        rect: Rect,
        scale: f32,
        panel_bottom: f32,
    },
    /// 显示器被拔掉：宠物迁移到主屏（Phase B 第 5 步）
    MonitorRemoved {
        monitor: String,
    },
    PlatformAdded(PlatformSpec),
    PlatformRemoved {
        id: String,
    },
    PlatformsCleared {
        monitor: String,
    },
}

/// 事件是否影响当前舞台（影响 ⇒ 调用方需要让宠物重新落位）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct StageImpact {
    /// 舞台换到了另一台显示器
    pub stage_changed: bool,
    /// 舞台自身几何变了（分辨率/缩放/面板高度）⇒ 宠物需要重新落位
    pub geometry_changed: bool,
    /// 舞台上的平台增删 ⇒ 不动宠物，由物理在下一次 `settle` 里给出 slipped_off
    pub platforms_changed: bool,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum WallContact {
    #[default]
    None,
    Left,
    Right,
}

impl World {
    /// 至少要有一个显示器，否则没有舞台可言。
    pub fn new(monitors: Vec<Monitor>) -> Option<World> {
        if monitors.is_empty() {
            return None;
        }
        let stage = monitors.iter().position(|m| m.primary).unwrap_or(0);
        Some(World {
            monitors,
            platforms: Vec::new(),
            stage,
            next_slot: 1,
        })
    }

    pub fn single(name: &str, w: f32, h: f32, scale: f32) -> World {
        World::new(vec![Monitor {
            name: name.into(),
            rect: Rect::new(0.0, 0.0, w, h),
            scale,
            primary: true,
            panel_bottom: 0.0,
        }])
        .expect("single monitor is non-empty")
    }

    pub fn stage(&self) -> &Monitor {
        &self.monitors[self.stage]
    }

    pub fn stage_name(&self) -> &str {
        &self.stage().name
    }

    pub fn ground_y(&self) -> f32 {
        self.stage().ground_y()
    }

    pub fn ceiling_y(&self) -> f32 {
        self.stage().ceiling_y()
    }

    pub fn left_wall(&self) -> f32 {
        self.stage().rect.left()
    }

    pub fn right_wall(&self) -> f32 {
        self.stage().rect.right()
    }

    pub fn monitor_named(&self, name: &str) -> Option<usize> {
        self.monitors.iter().position(|m| m.name == name)
    }

    pub fn set_stage(&mut self, index: usize) -> bool {
        if index < self.monitors.len() {
            self.stage = index;
            true
        } else {
            false
        }
    }

    pub fn set_stage_by_name(&mut self, name: &str) -> bool {
        match self.monitor_named(name) {
            Some(i) => self.set_stage(i),
            None => false,
        }
    }

    /// `id` 已存在则更新几何并复用槽位，否则分配新槽位。
    pub fn upsert_platform(&mut self, spec: PlatformSpec) -> u32 {
        if let Some(existing) = self.platforms.iter_mut().find(|p| p.id == spec.id) {
            existing.kind = spec.kind;
            existing.monitor = spec.monitor;
            existing.x0 = spec.x0;
            existing.x1 = spec.x1;
            existing.top_y = spec.top_y;
            existing.slot
        } else {
            let slot = self.next_slot;
            self.next_slot += 1;
            self.platforms.push(Platform {
                slot,
                id: spec.id,
                kind: spec.kind,
                monitor: spec.monitor,
                x0: spec.x0,
                x1: spec.x1,
                top_y: spec.top_y,
            });
            slot
        }
    }

    pub fn remove_platform(&mut self, id: &str) -> Option<u32> {
        let idx = self.platforms.iter().position(|p| p.id == id)?;
        Some(self.platforms.remove(idx).slot)
    }

    pub fn platform(&self, slot: u32) -> Option<&Platform> {
        self.platforms.iter().find(|p| p.slot == slot)
    }

    pub fn stage_platforms(&self) -> impl Iterator<Item = &Platform> {
        let stage = self.stage_name();
        self.platforms.iter().filter(move |p| p.monitor == stage)
    }

    /// 某个支撑在 `x` 处是否还是实的（窗口被关掉、宠物走出台面 ⇒ 应当坠落）。
    pub fn support_is_solid(&self, support: Support, x: f32) -> bool {
        self.support_y(support, x).is_some()
    }

    /// 支撑面对应的 y。平台消失或宠物已经走出台面范围时返回 `None`。
    pub fn support_y(&self, support: Support, x: f32) -> Option<f32> {
        match support {
            Support::Ground => Some(self.ground_y()),
            Support::Ceiling => Some(self.ceiling_y()),
            Support::Platform(slot) => self
                .platform(slot)
                .filter(|p| p.monitor == self.stage_name() && p.spans(x))
                .map(|p| p.top_y),
        }
    }

    /// 覆盖 x 的最高平台（给"站在哪个平台上"的兜底查询用）。
    pub fn surface_spanning(&self, x: f32) -> Option<&Platform> {
        self.stage_platforms()
            .filter(|p| p.spans(x))
            .min_by(|a, b| a.top_y.total_cmp(&b.top_y))
    }

    /// 下落途中 `(from_y, to_y]` 会撞到的第一个表面。同 x 有多个平台时取最靠上的那个。
    pub fn landing(&self, x: f32, from_y: f32, to_y: f32) -> Option<(f32, Support)> {
        let mut best: Option<(f32, Support)> = None;
        for p in self.stage_platforms().filter(|p| p.spans(x)) {
            if p.top_y > from_y && p.top_y <= to_y {
                let better = match best {
                    None => true,
                    Some((y, _)) => p.top_y < y,
                };
                if better {
                    best = Some((p.top_y, Support::Platform(p.slot)));
                }
            }
        }
        let ground = self.ground_y();
        if ground > from_y && ground <= to_y && best.is_none() {
            best = Some((ground, Support::Ground));
        }
        best
    }

    /// 站在 `y` 上时，朝 `dir` 方向 `lookahead` 处是否有需要跳上去的台阶。
    /// `max_step_up` 是宠物一次跳跃能越过的高度差：比它高的不是"障碍"而是"墙"，该由爬墙逻辑处理。
    pub fn obstacle_ahead(
        &self,
        x: f32,
        dir: f32,
        lookahead: f32,
        current_y: f32,
        max_step_up: f32,
    ) -> Option<&Platform> {
        let probe = x + dir * lookahead;
        if probe < self.left_wall() || probe > self.right_wall() {
            return None;
        }
        self.stage_platforms()
            .filter(|p| {
                p.spans(probe)
                    && p.top_y < current_y - SUPPORT_EPS
                    && current_y - p.top_y <= max_step_up
            })
            .max_by(|a, b| a.top_y.total_cmp(&b.top_y))
    }

    /// 沿墙上爬时（`to_y < from_y`）最先撞到的平台顶边。取区间内 `top_y` 最大（最低）的那个。
    pub fn crossed_platform(&self, x: f32, from_y: f32, to_y: f32) -> Option<&Platform> {
        if to_y >= from_y {
            return None;
        }
        self.stage_platforms()
            .filter(|p| p.spans(x) && p.top_y < from_y && p.top_y >= to_y)
            .max_by(|a, b| a.top_y.total_cmp(&b.top_y))
    }

    pub fn is_on_stage(&self, x: f32) -> bool {
        self.stage().contains_x(x)
    }

    /// 舞台内可放置锚点的 x 区间（留出宠物半宽）。
    pub fn reachable_x(&self, margin: f32) -> Option<(f32, f32)> {
        let lo = self.left_wall() + margin;
        let hi = self.right_wall() - margin;
        if hi <= lo { None } else { Some((lo, hi)) }
    }

    pub fn clamp_x(&self, x: f32, margin: f32) -> f32 {
        match self.reachable_x(margin) {
            Some((lo, hi)) => x.clamp(lo, hi),
            None => (self.left_wall() + self.right_wall()) * 0.5,
        }
    }

    pub fn apply(&mut self, ev: WorldEvent) -> StageImpact {
        match ev {
            WorldEvent::MonitorGeometry {
                monitor,
                rect,
                scale,
                panel_bottom,
            } => match self.monitor_named(&monitor) {
                Some(i) => {
                    let m = &mut self.monitors[i];
                    m.rect = rect;
                    m.scale = scale;
                    m.panel_bottom = panel_bottom;
                    StageImpact {
                        stage_changed: false,
                        geometry_changed: i == self.stage,
                        platforms_changed: false,
                    }
                }
                None => StageImpact {
                    stage_changed: false,
                    geometry_changed: false,
                    platforms_changed: false,
                },
            },
            WorldEvent::MonitorRemoved { monitor } => {
                let Some(i) = self.monitor_named(&monitor) else {
                    return StageImpact {
                        stage_changed: false,
                        geometry_changed: false,
                        platforms_changed: false,
                    };
                };
                self.monitors.remove(i);
                self.platforms.retain(|p| p.monitor != monitor);
                if self.monitors.is_empty() {
                    // 没有任何显示器了：调用方负责优雅退出，这里不放空舞台
                    return StageImpact {
                        stage_changed: true,
                        geometry_changed: true,
                        platforms_changed: false,
                    };
                }
                let stage_changed = i == self.stage;
                if stage_changed {
                    self.stage = self.monitors.iter().position(|m| m.primary).unwrap_or(0);
                } else if self.stage > i {
                    self.stage -= 1;
                }
                StageImpact {
                    stage_changed,
                    geometry_changed: true,
                    platforms_changed: false,
                }
            }
            WorldEvent::PlatformAdded(spec) => {
                let touches_stage = spec.monitor == self.stage_name();
                self.upsert_platform(spec);
                StageImpact {
                    stage_changed: false,
                    geometry_changed: false,
                    platforms_changed: touches_stage,
                }
            }
            WorldEvent::PlatformRemoved { id } => {
                let touched = self
                    .platforms
                    .iter()
                    .any(|p| p.id == id && p.monitor == self.stage_name());
                self.remove_platform(&id);
                StageImpact {
                    stage_changed: false,
                    geometry_changed: false,
                    platforms_changed: touched,
                }
            }
            WorldEvent::PlatformsCleared { monitor } => {
                let touched = monitor == self.stage_name();
                self.platforms.retain(|p| p.monitor != monitor);
                StageImpact {
                    stage_changed: false,
                    geometry_changed: false,
                    platforms_changed: touched,
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn spec(id: &str, x0: f32, x1: f32, top_y: f32) -> PlatformSpec {
        PlatformSpec {
            id: id.into(),
            kind: PlatformKind::WindowTopEdge,
            monitor: "m".into(),
            x0,
            x1,
            top_y,
        }
    }

    #[test]
    fn ground_respects_panel() {
        let mut w = World::single("HDMI-A-1", 1920.0, 1080.0, 1.0);
        assert_eq!(w.ground_y(), 1080.0);
        let impact = w.apply(WorldEvent::MonitorGeometry {
            monitor: "HDMI-A-1".into(),
            rect: Rect::new(0.0, 0.0, 1920.0, 1080.0),
            scale: 1.0,
            panel_bottom: 48.0,
        });
        assert!(impact.geometry_changed);
        assert_eq!(w.ground_y(), 1032.0);
    }

    #[test]
    fn landing_prefers_highest_platform() {
        let mut w = World::single("m", 1000.0, 600.0, 1.0);
        let slot = w.upsert_platform(spec("win1", 100.0, 300.0, 400.0));
        assert_eq!(slot, 1);
        assert_eq!(
            w.landing(200.0, 350.0, 500.0),
            Some((400.0, Support::Platform(1)))
        );
        // 平台区间外 ⇒ 落到地面
        assert_eq!(
            w.landing(500.0, 350.0, 700.0),
            Some((600.0, Support::Ground))
        );
        // 已经在平台下方穿过 ⇒ 不再拦截
        assert_eq!(w.landing(200.0, 450.0, 500.0), None);
    }

    #[test]
    fn upsert_reuses_slot() {
        let mut w = World::single("m", 1000.0, 600.0, 1.0);
        assert_eq!(w.upsert_platform(spec("win1", 0.0, 100.0, 300.0)), 1);
        assert_eq!(w.upsert_platform(spec("win1", 0.0, 200.0, 320.0)), 1);
        assert_eq!(w.upsert_platform(spec("win2", 300.0, 400.0, 100.0)), 2);
        assert_eq!(w.platform(1).unwrap().x1, 200.0);
        assert_eq!(w.support_y(Support::Platform(1), 50.0), Some(320.0));
        // 走出台面右沿 ⇒ 不再有支撑
        assert_eq!(w.support_y(Support::Platform(1), 250.0), None);
    }

    #[test]
    fn support_vanishes_when_platform_removed() {
        let mut w = World::single("m", 1000.0, 600.0, 1.0);
        w.upsert_platform(spec("win1", 0.0, 500.0, 300.0));
        assert!(w.support_is_solid(Support::Platform(1), 100.0));
        let impact = w.apply(WorldEvent::PlatformRemoved { id: "win1".into() });
        assert!(impact.platforms_changed);
        // 平台增删不该把宠物重新落位（那是物理 slipped_off / Fall 的职责）
        assert!(!impact.geometry_changed && !impact.stage_changed);
        assert!(!w.support_is_solid(Support::Platform(1), 100.0));
        assert!(w.support_is_solid(Support::Ground, 100.0));
    }

    #[test]
    fn obstacle_ahead_only_when_step_up() {
        let mut w = World::single("m", 1000.0, 600.0, 1.0);
        w.upsert_platform(spec("desk", 200.0, 400.0, 500.0));
        // 站在 600 的地面，往右探 30px 碰到 500 的台面
        assert!(w.obstacle_ahead(190.0, 1.0, 30.0, 600.0, 140.0).is_some());
        assert!(w.obstacle_ahead(190.0, -1.0, 30.0, 600.0, 140.0).is_none());
        // 台面比脚下低 ⇒ 不是障碍
        assert!(w.obstacle_ahead(250.0, 1.0, 30.0, 400.0, 140.0).is_none());
        // 高出台阶 100px 的落差跳不过去 ⇒ 不当障碍（该由爬墙处理）
        assert!(w.obstacle_ahead(190.0, 1.0, 30.0, 600.0, 80.0).is_none());
    }

    #[test]
    fn monitor_removal_migrates_stage() {
        let mut w = World::new(vec![
            Monitor {
                name: "a".into(),
                rect: Rect::new(0.0, 0.0, 1920.0, 1080.0),
                scale: 1.0,
                primary: true,
                panel_bottom: 0.0,
            },
            Monitor {
                name: "b".into(),
                rect: Rect::new(1920.0, 0.0, 1280.0, 720.0),
                scale: 2.0,
                primary: false,
                panel_bottom: 0.0,
            },
        ])
        .unwrap();
        w.set_stage_by_name("b");
        assert_eq!(w.stage_name(), "b");
        assert_eq!(w.ground_y(), 720.0);
        let impact = w.apply(WorldEvent::MonitorRemoved {
            monitor: "b".into(),
        });
        assert!(impact.stage_changed);
        assert_eq!(w.stage_name(), "a");
        assert_eq!(w.ground_y(), 1080.0);
    }

    #[test]
    fn removal_of_other_monitor_keeps_stage() {
        let mut w = World::new(vec![
            Monitor {
                name: "a".into(),
                rect: Rect::new(0.0, 0.0, 1920.0, 1080.0),
                scale: 1.0,
                primary: true,
                panel_bottom: 0.0,
            },
            Monitor {
                name: "b".into(),
                rect: Rect::new(1920.0, 0.0, 1280.0, 720.0),
                scale: 1.0,
                primary: false,
                panel_bottom: 0.0,
            },
        ])
        .unwrap();
        assert_eq!(w.stage_name(), "a");
        let impact = w.apply(WorldEvent::MonitorRemoved {
            monitor: "b".into(),
        });
        assert!(!impact.stage_changed);
        assert_eq!(w.stage_name(), "a");
    }

    #[test]
    fn clamp_x_centers_when_stage_too_narrow() {
        let w = World::single("m", 40.0, 600.0, 1.0);
        assert_eq!(w.reachable_x(64.0), None);
        assert_eq!(w.clamp_x(999.0, 64.0), 20.0);
    }
}
