//! 物理积分：重力、抛物线、落地检测、贴边、拖拽抛出（§3.3 表格里的 `g = 1800 px/s²`、`v0 = √(2gh)`）。
//!
//! 固定步长积分（60 Hz），不使用系统时钟 ⇒ 结果只取决于 seed 与 tick 数，长稳模拟可复现。
//! 站在地面/平台上时位置每 tick 被"吸附"到支撑面 y，因此 8 小时不累积浮点漂移（M1 验收项）。

use crate::geom::Vec2;
use crate::world::{Support, WallContact, World};

/// 重力加速度，px/s²。
pub const GRAVITY: f32 = 1800.0;
/// 跳跃高度下界/上界，px（§3.3：`h = 90–140 px`）。
pub const JUMP_HEIGHT_MIN: f32 = 90.0;
pub const JUMP_HEIGHT_MAX: f32 = 140.0;

/// 达到高度 `h` 所需的竖直初速度（向上为负 y）。
pub fn jump_speed_for_height(h: f32) -> f32 {
    (2.0 * GRAVITY * h.max(0.0)).sqrt()
}

/// 宠物当前的运动模式。行为状态机通过 [`Body`] 的方法切换它，物理只看这个。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Motion {
    /// 站在某个支撑面上，y 被吸附
    Grounded,
    /// 贴墙向上爬（重力被抵消，速度由行为层给定）
    Climbing,
    /// 自由抛体
    Airborne,
    /// 被指针拖着走：位置由外部输入决定，不积分
    Dragged,
    /// 无重力飘行（§3.5-k）：y 由行为层速度决定、夹进 `[ceiling, ground]`，
    /// **不产任何 StepOutcome** —— 落地/搭台的迁移由行为层用 `settle_on` 收口，
    /// 物理层若打 `landed` 会发出一条迁移表里不存在的 `Walk→Land`。
    Float,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Landing {
    pub support: Support,
    /// 触地瞬间的竖直速度，用于 squash & stretch 的强度（M2 取帧用）
    pub impact_speed: f32,
}

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct StepOutcome {
    pub landed: Option<Landing>,
    pub wall: WallContact,
    /// 支撑面消失或走出台面边缘
    pub slipped_off: bool,
    /// 爬升途中搭上了平台顶边或舞台顶边（新的支撑面）
    pub perched: Option<Support>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Body {
    /// 锚点（默认脚底中心）在全局虚拟桌面里的位置
    pub pos: Vec2,
    pub vel: Vec2,
    pub motion: Motion,
    pub support: Option<Support>,
    /// 精灵包围盒半宽，用于把宠物整只留在舞台内
    pub half_width: f32,
    /// 上一次落地冲击速度，落地后保留供动画取帧
    pub last_impact: f32,
}

impl Body {
    pub fn new(pos: Vec2, half_width: f32) -> Body {
        Body {
            pos,
            vel: Vec2::ZERO,
            motion: Motion::Grounded,
            support: Some(Support::Ground),
            half_width,
            last_impact: 0.0,
        }
    }

    pub fn grounded_at(&self) -> bool {
        self.motion == Motion::Grounded
    }

    pub fn airborne(&self) -> bool {
        self.motion == Motion::Airborne
    }

    pub fn floating(&self) -> bool {
        self.motion == Motion::Float
    }

    /// 冻结在墙上（爬升中被行为层停住）：`on_stage_change` 判"挂墙钉"用。
    pub fn climb_frozen(&self) -> bool {
        self.motion == Motion::Climbing && self.vel.y == 0.0
    }

    /// 起跳：`v0 = √(2gh)`，向上为负。
    pub fn jump(&mut self, height: f32) -> f32 {
        let v0 = jump_speed_for_height(height);
        self.begin_airborne(-v0)
    }

    pub fn begin_airborne(&mut self, vy: f32) -> f32 {
        self.motion = Motion::Airborne;
        self.support = None;
        self.vel.y = vy;
        vy
    }

    pub fn begin_climb(&mut self, speed: f32) {
        self.motion = Motion::Climbing;
        self.support = None;
        self.vel.y = -speed.abs();
    }

    pub fn set_climb_speed(&mut self, speed: f32) {
        if self.motion == Motion::Climbing {
            self.vel.y = -speed.abs();
        }
    }

    /// 体力耗尽或被抓起：脱离墙面，交给重力。
    pub fn release_climb(&mut self) {
        if self.motion == Motion::Climbing {
            self.motion = Motion::Airborne;
            self.vel.y = 0.0;
        }
    }

    pub fn begin_drag(&mut self) {
        self.motion = Motion::Dragged;
        self.support = None;
        self.vel = Vec2::ZERO;
    }

    /// 指针移动：位置跟手，但**裁剪进舞台**（屏幕坐标 y 向下为正，地面 = 舞台底边），
    /// 同时估计抛出速度（低通，避免单帧抖动把宠物弹飞）。
    ///
    /// 裁剪不是"防呆"而是必需的：宠物正常站立时脚**就在地面线上**，所以指针只要往下
    /// 动一点，未裁剪的目标就到了地面以下 —— 而 [`World::landing`] 只拦"从地面上方
    /// 穿过地面"，一旦到了下面就来不回来，表现就是"一按左键宠物掉出屏幕、再也不出现"
    /// （2026-10-25 真机手测报出来的缺陷）。
    pub fn drag_to(&mut self, world: &World, pos: Vec2, dt: f32) {
        if self.motion != Motion::Dragged || dt <= 0.0 {
            return;
        }
        let pos = Vec2::new(
            world.clamp_x(pos.x, self.half_width),
            pos.y.clamp(world.ceiling_y(), world.ground_y()),
        );
        let inst = Vec2::new((pos.x - self.pos.x) / dt, (pos.y - self.pos.y) / dt);
        const ALPHA: f32 = 0.4;
        self.vel = Vec2::new(
            self.vel.x + (inst.x - self.vel.x) * ALPHA,
            self.vel.y + (inst.y - self.vel.y) * ALPHA,
        );
        self.pos = pos;
    }

    /// 松手：保留估计出的速度 ⇒ 进入 Airborne，由 [`crate::behavior`] 走 Dragged→Fall。
    pub fn end_drag(&mut self) -> Vec2 {
        self.motion = Motion::Airborne;
        self.support = None;
        self.vel
    }

    /// 水平速度直接由行为层设定（行走/奔跑是运动学的，不做加速度模型）。
    pub fn set_horizontal_speed(&mut self, speed: f32) {
        self.vel.x = speed;
    }

    /// 竖直速度只在 Float 档生效（与 `set_climb_speed` 同族写法：写入点自守档）。
    pub fn set_vertical_speed(&mut self, v: f32) {
        if self.motion == Motion::Float {
            self.vel.y = v;
        }
    }

    pub fn stop_horizontal(&mut self) {
        self.vel.x = 0.0;
    }

    pub fn step(&mut self, dt: f32, world: &World) -> StepOutcome {
        let mut out = StepOutcome::default();

        self.integrate_horizontal(dt, world, &mut out);
        match self.motion {
            Motion::Dragged => {}
            Motion::Grounded => self.settle_on_support(world, &mut out),
            Motion::Climbing => self.integrate_climb(dt, world, &mut out),
            Motion::Airborne => self.integrate_airborne(dt, world, &mut out),
            Motion::Float => self.integrate_float(dt, world),
        }

        out
    }

    fn integrate_horizontal(&mut self, dt: f32, world: &World, out: &mut StepOutcome) {
        if self.vel.x == 0.0 {
            return;
        }
        self.pos.x += self.vel.x * dt;
        let lo = world.left_wall() + self.half_width;
        let hi = world.right_wall() - self.half_width;
        if hi <= lo {
            // 舞台比宠物还窄：钉在中间，不给墙接触事件（否则会在墙上抖）
            self.pos.x = (world.left_wall() + world.right_wall()) * 0.5;
            self.vel.x = 0.0;
            return;
        }
        if self.pos.x <= lo {
            self.pos.x = lo;
            if self.vel.x < 0.0 {
                self.vel.x = 0.0;
                out.wall = WallContact::Left;
            }
        } else if self.pos.x >= hi {
            self.pos.x = hi;
            if self.vel.x > 0.0 {
                self.vel.x = 0.0;
                out.wall = WallContact::Right;
            }
        }
    }

    fn settle_on_support(&mut self, world: &World, out: &mut StepOutcome) {
        let support = self.support.unwrap_or(Support::Ground);
        match world.support_y(support, self.pos.x) {
            Some(y) => {
                // 吸附：面板高度变化或窗口移动时贴住，且不产生累积误差
                self.pos.y = y;
                self.vel.y = 0.0;
            }
            None => {
                out.slipped_off = true;
                self.motion = Motion::Airborne;
                self.support = None;
                self.vel.y = 0.0;
            }
        }
    }

    /// 无重力飘行（§3.5-k）：y 单调走向目标，两端夹住并归零对应分量。
    /// **不查 `world.landing()`、不产任何 outcome** —— 巡线穿平台顶边是登记在册的限制，
    /// 收口（`settle_on` → Grounded）由行为层在到达时做。
    fn integrate_float(&mut self, dt: f32, world: &World) {
        if self.vel.y == 0.0 {
            return;
        }
        self.pos.y += self.vel.y * dt;
        let ceiling = world.ceiling_y();
        let ground = world.ground_y();
        if self.pos.y <= ceiling {
            self.pos.y = ceiling;
            if self.vel.y < 0.0 {
                self.vel.y = 0.0;
            }
        } else if self.pos.y >= ground {
            self.pos.y = ground;
            if self.vel.y > 0.0 {
                self.vel.y = 0.0;
            }
        }
    }

    fn integrate_climb(&mut self, dt: f32, world: &World, out: &mut StepOutcome) {
        let from_y = self.pos.y;
        self.pos.y += self.vel.y * dt;
        if let Some(p) = world.crossed_platform(self.pos.x, from_y, self.pos.y) {
            return self.perch(p.top_y, Support::Platform(p.slot), out);
        }
        let ceiling = world.ceiling_y();
        if self.pos.y <= ceiling {
            self.perch(ceiling, Support::Ceiling, out);
        }
    }

    fn perch(&mut self, y: f32, support: Support, out: &mut StepOutcome) {
        self.pos.y = y;
        self.vel.y = 0.0;
        self.support = Some(support);
        self.motion = Motion::Grounded;
        out.perched = Some(support);
    }

    fn integrate_airborne(&mut self, dt: f32, world: &World, out: &mut StepOutcome) {
        let from_y = self.pos.y;
        self.vel.y += GRAVITY * dt;
        let to_y = from_y + self.vel.y * dt;
        self.pos.y = to_y;

        if self.vel.y > 0.0 {
            if let Some((y, support)) = world.landing(self.pos.x, from_y, to_y) {
                out.landed = Some(Landing {
                    support,
                    impact_speed: self.vel.y,
                });
                self.last_impact = self.vel.y;
                self.pos.y = y;
                self.vel.y = 0.0;
                self.support = Some(support);
                self.motion = Motion::Grounded;
            }
        }

        // 兜底：向上抛出后越过舞台顶（例如从窗口顶边往上看不到地面）时不要跑到屏外
        if self.pos.y < world.ceiling_y() && self.vel.y < 0.0 {
            self.pos.y = world.ceiling_y();
            self.vel.y = 0.0;
        }

        // 兜底（地面侧，与上面那条对称）：位置已经在地面以下时 `landing()` 的
        // "从上方穿过"条件永远不成立 ⇒ 无限下坠、宠物永久消失。当场接住并当作落地，
        // 让状态机照走 Land。触发途径不止拖拽：显示器几何变小后 `snap_to_stage` 在
        // 非 Grounded 时是早退的。
        if self.pos.y > world.ground_y() && self.vel.y >= 0.0 {
            let impact = self.vel.y;
            self.pos.y = world.ground_y();
            self.vel.y = 0.0;
            self.support = Some(Support::Ground);
            self.motion = Motion::Grounded;
            self.last_impact = impact;
            out.landed = Some(Landing {
                support: Support::Ground,
                impact_speed: impact,
            });
        }
    }

    /// 钉住落位（§3.5-j）：把"坐在某支撑面上"做成唯一的写入点方法，
    /// 与 `drag_to`/`perch`/`snap_to_stage` 同族——不变量立在写入点。
    pub fn settle_on(&mut self, support: Support, y: f32) {
        self.motion = Motion::Grounded;
        self.support = Some(support);
        self.pos.y = y;
        self.vel = Vec2::ZERO;
    }

    /// 悬空钉（§3.5-j′，E1b）：Float + 零速 + 位置夹进舞台，与 `settle_on` 同族
    /// —— "停在半空"这件事只有这一个写入点。
    pub fn hold_in_air(&mut self, world: &World) {
        self.motion = Motion::Float;
        self.support = None;
        self.vel = Vec2::ZERO;
        self.pos.x = world.clamp_x(self.pos.x, self.half_width);
        self.pos.y = self.pos.y.clamp(world.ceiling_y(), world.ground_y());
    }

    /// 起飞进 Float（§3.5-k）。**故意不清 `vel.y`**：dy=0 的指令目标不许真的离地
    /// （见 machine 侧 `apply_motion_speeds` 的 `|dy|>0` 门槛），而万一带着残留竖直速度
    /// 起飞，`settle_on_support` 对 vel≠0 的无支撑体是救不回来的。
    pub fn begin_float(&mut self) {
        self.motion = Motion::Float;
        self.support = None;
    }

    /// 放生悬空钉（E5）：交还重力。`release_climb` 对 Float 是 no-op，故必须独立写入点。
    pub fn release_hold(&mut self) {
        if self.motion == Motion::Float {
            self.motion = Motion::Airborne;
            self.vel.y = 0.0;
        }
    }

    /// 舞台几何变化后强制重新落位（Phase B 第 5 步：拔屏/改分辨率）。
    pub fn snap_to_stage(&mut self, world: &World) {
        self.pos.x = world.clamp_x(self.pos.x, self.half_width);
        // 悬空钉/巡线换屏：保高度、只夹 x/y —— "钉跟着显示器走"（§3.5-j 拍板③的空中支）
        if self.motion == Motion::Float {
            self.pos.y = self.pos.y.clamp(world.ceiling_y(), world.ground_y());
            return;
        }
        if self.motion != Motion::Grounded {
            return;
        }
        let support = self.support.unwrap_or(Support::Ground);
        match world.support_y(support, self.pos.x) {
            Some(y) => {
                self.pos.y = y;
                self.support = Some(support);
            }
            None => {
                self.pos.y = world.ground_y();
                self.support = Some(Support::Ground);
                self.vel.y = 0.0;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::world::{PlatformKind, PlatformSpec};

    const DT: f32 = 1.0 / 60.0;

    fn world() -> World {
        World::single("m", 1000.0, 600.0, 1.0)
    }

    #[test]
    fn jump_speed_matches_height() {
        // v0 = √(2*1800*90) = 569.21
        assert!((jump_speed_for_height(90.0) - 569.2099).abs() < 0.01);
        assert_eq!(jump_speed_for_height(0.0), 0.0);
    }

    #[test]
    fn parabola_apex_matches_requested_height() {
        let w = world();
        let mut b = Body::new(Vec2::new(500.0, 600.0), 32.0);
        let h = 120.0;
        b.jump(h);
        let mut min_y = b.pos.y;
        for _ in 0..120 {
            b.step(DT, &w);
            min_y = min_y.min(b.pos.y);
        }
        // 半隐式欧拉的离散误差在一个步长以内（≈ g·dt² 量级）
        let risen = 600.0 - min_y;
        assert!((risen - h).abs() < 20.0, "名义升高 {h}，实测 {risen}");
    }

    #[test]
    fn falls_back_to_ground_and_snaps() {
        let w = world();
        let mut b = Body::new(Vec2::new(500.0, 600.0), 32.0);
        b.jump(100.0);
        let mut landed = None;
        for _ in 0..300 {
            let out = b.step(DT, &w);
            if let Some(l) = out.landed {
                landed = Some(l);
                break;
            }
        }
        let l = landed.expect("应该落回地面");
        assert_eq!(l.support, Support::Ground);
        assert!(l.impact_speed > 0.0);
        assert_eq!(b.pos.y, 600.0);
        assert_eq!(b.motion, Motion::Grounded);
    }

    #[test]
    fn lands_on_platform_when_falling() {
        let mut w = world();
        let slot = w.upsert_platform(PlatformSpec {
            id: "win".into(),
            kind: PlatformKind::WindowTopEdge,
            monitor: "m".into(),
            x0: 400.0,
            x1: 700.0,
            top_y: 300.0,
        });
        // 从台面上方落下，应当搭在台面而不是穿到地面
        let mut b = Body::new(Vec2::new(500.0, 100.0), 32.0);
        b.begin_airborne(60.0);
        let mut landed = None;
        for _ in 0..300 {
            let out = b.step(DT, &w);
            if let Some(l) = out.landed {
                landed = Some(l);
                break;
            }
        }
        assert_eq!(landed.unwrap().support, Support::Platform(slot));
        assert_eq!(b.pos.y, 300.0);
    }

    #[test]
    fn walking_off_a_platform_edge_starts_falling() {
        let mut w = world();
        let slot = w.upsert_platform(PlatformSpec {
            id: "win".into(),
            kind: PlatformKind::WindowTopEdge,
            monitor: "m".into(),
            x0: 0.0,
            x1: 100.0,
            top_y: 300.0,
        });
        let mut b = Body::new(Vec2::new(50.0, 300.0), 8.0);
        b.support = Some(Support::Platform(slot));
        b.motion = Motion::Grounded;
        b.set_horizontal_speed(120.0);
        let mut slipped = false;
        for _ in 0..200 {
            let out = b.step(DT, &w);
            if out.slipped_off {
                slipped = true;
                break;
            }
        }
        assert!(slipped, "走出台面右沿应转为坠落");
        assert_eq!(b.motion, Motion::Airborne);
    }

    #[test]
    fn wall_contact_is_reported_and_position_clamped() {
        let w = world();
        let mut b = Body::new(Vec2::new(900.0, 600.0), 20.0);
        b.set_horizontal_speed(200.0);
        let mut contact = WallContact::None;
        for _ in 0..60 {
            contact = b.step(DT, &w).wall;
            if contact != WallContact::None {
                break;
            }
        }
        assert_eq!(contact, WallContact::Right);
        assert_eq!(b.pos.x, 980.0, "应被夹在右墙内侧（右墙 - 半宽）");
        assert_eq!(b.vel.x, 0.0);
    }

    #[test]
    fn climb_reaches_ceiling_and_becomes_grounded() {
        let w = world();
        let mut b = Body::new(Vec2::new(20.0, 600.0), 16.0);
        b.begin_climb(30.0);
        let mut perched = None;
        for _ in 0..2000 {
            let out = b.step(DT, &w);
            if out.perched.is_some() {
                perched = out.perched;
                break;
            }
        }
        assert_eq!(perched, Some(Support::Ceiling));
        assert_eq!(b.pos.y, 0.0);
        assert_eq!(b.motion, Motion::Grounded);
        assert_eq!(b.support, Some(Support::Ceiling));
    }

    #[test]
    fn dragged_does_not_integrate_gravity() {
        let w = world();
        let mut b = Body::new(Vec2::new(500.0, 600.0), 16.0);
        b.begin_drag();
        for _ in 0..60 {
            b.step(DT, &w);
        }
        assert_eq!(b.pos, Vec2::new(500.0, 600.0));
        let v = b.end_drag();
        assert_eq!(v, Vec2::ZERO);
        assert_eq!(b.motion, Motion::Airborne);
    }

    /// 真机报出来的缺陷（2026-10-25 用户手测）：**左键一按宠物就掉出屏幕、再也不回来**。
    /// 根因是拖拽把位置**直接跟手、不做任何舞台裁剪**，而正常站立时脚就在地面线上
    /// （地面 = 显示器底边），所以指针只要往下动一点，宠物就到了地面以下。
    #[test]
    fn a_drag_can_not_move_the_pet_outside_the_stage() {
        let w = world();
        let mut b = Body::new(Vec2::new(500.0, w.ground_y()), 16.0);
        b.begin_drag();
        b.drag_to(&w, Vec2::new(500.0, w.ground_y() + 300.0), DT);
        assert_eq!(
            b.pos.y,
            w.ground_y(),
            "往地面以下拖 ⇒ 必须钉在舞台上，否则宠物就丢了"
        );
        b.drag_to(&w, Vec2::new(99_999.0, 300.0), DT);
        assert_eq!(b.pos.x, w.right_wall() - b.half_width, "往墙外拖同理");
    }

    /// 第二个洞，独立于拖拽：`landing()` 只拦"从地面上方穿过地面"，一旦位置已经在地面
    /// 以下（上面那个洞、或显示器几何变小后 `snap_to_stage` 在非 Grounded 时早退），
    /// 之后就再也条件不成立 ⇒ 无限下坠，宠物永久消失。
    #[test]
    fn a_pet_below_the_floor_gets_caught_instead_of_falling_forever() {
        let w = world();
        let mut b = Body::new(Vec2::new(500.0, w.ground_y() + 100.0), 16.0);
        b.motion = Motion::Airborne;
        b.support = None;
        let mut caught = None;
        for i in 0..600 {
            let o = b.step(DT, &w);
            if o.landed.is_some() {
                caught = Some(i);
                break;
            }
        }
        assert!(caught.is_some(), "在地面以下也必须被接住，不能一直掉");
        assert_eq!(b.pos.y, w.ground_y());
        assert_eq!(b.motion, Motion::Grounded);
    }

    #[test]
    fn drag_velocity_estimate_is_smoothed() {
        let w = world();
        let mut b = Body::new(Vec2::new(0.0, 0.0), 16.0);
        b.begin_drag();
        // 每帧往右下拖 10px ⇒ 瞬时速度约 (600, 600)
        for i in 1..12 {
            b.drag_to(&w, Vec2::new(i as f32 * 10.0, i as f32 * 10.0), DT);
        }
        let v = b.end_drag();
        assert!(v.x > 300.0 && v.x < 620.0, "估计速度 {:?}", v.x);
        assert_eq!(b.vel, v);
    }
    #[test]
    fn standing_still_never_drifts_over_8_hours() {
        let w = world();
        let mut b = Body::new(Vec2::new(500.0, 600.0), 16.0);
        for _ in 0..60 * 60 * 8 {
            b.step(DT, &w);
        }
        assert_eq!(b.pos, Vec2::new(500.0, 600.0));
        assert!(b.pos.is_finite());
    }

    #[test]
    fn snap_to_stage_rescues_a_moved_window() {
        let mut w = world();
        let slot = w.upsert_platform(PlatformSpec {
            id: "win".into(),
            kind: PlatformKind::WindowTopEdge,
            monitor: "m".into(),
            x0: 0.0,
            x1: 50.0,
            top_y: 300.0,
        });
        let mut b = Body::new(Vec2::new(900.0, 300.0), 16.0);
        b.support = Some(Support::Platform(slot));
        // 宠物已不在台面上：重新落位应回到地面并留在舞台内
        b.snap_to_stage(&w);
        assert_eq!(b.pos.y, 600.0);
        assert_eq!(b.support, Some(Support::Ground));
        assert_eq!(b.pos.x, 900.0);
    }

    /// §3.5-j′ E1b：悬空钉必须**逐位不动**，且永远不产出 landed。
    #[test]
    fn a_hold_does_not_fall() {
        let w = world();
        let mut b = Body::new(Vec2::new(500.0, 300.0), 16.0);
        b.hold_in_air(&w);
        assert_eq!(b.motion, Motion::Float);
        assert_eq!(b.support, None);
        let pinned_at = b.pos;
        let mut outcomes = 0;
        for _ in 0..600 {
            let o = b.step(DT, &w);
            outcomes += usize::from(o.landed.is_some() || o.perched.is_some() || o.slipped_off);
        }
        assert_eq!(outcomes, 0, "Float 档不许产任何迁移素材");
        assert_eq!(b.pos, pinned_at, "悬空钉 10 秒逐位不动");
        assert_eq!(b.vel, Vec2::ZERO);
    }

    #[test]
    fn float_follows_a_2d_target_and_clamps_at_both_ends() {
        let w = world();
        let mut b = Body::new(Vec2::new(500.0, 300.0), 16.0);
        b.begin_float();
        b.vel = Vec2::new(-60.0, -120.0); // 向左上
        for _ in 0..600 {
            b.step(DT, &w);
        }
        assert!(b.pos.x < 500.0, "x 应随速度走");
        assert_eq!(b.pos.y, w.ceiling_y(), "越顶必须夹住");
        assert_eq!(b.vel.y, 0.0);
        b.vel = Vec2::new(0.0, 240.0); // 直落
        for _ in 0..600 {
            let o = b.step(DT, &w);
            assert!(
                o.landed.is_none(),
                "落到地面也不许打 landed（表外边 Walk→Land）"
            );
        }
        assert_eq!(b.pos.y, w.ground_y());
        assert_eq!(b.vel.y, 0.0);
        assert_eq!(b.motion, Motion::Float, "收口是行为层的事，物理留在 Float");
    }

    #[test]
    fn release_hold_hands_the_body_to_gravity() {
        let w = world();
        let mut b = Body::new(Vec2::new(500.0, 300.0), 16.0);
        b.hold_in_air(&w);
        b.release_hold();
        assert_eq!(b.motion, Motion::Airborne);
        assert_eq!(b.vel.y, 0.0);
        b.step(DT, &w);
        assert!(b.pos.y > 300.0, "放生后必须开始下坠");
        // release_hold 对其他档是 no-op（地面钉不许被误抛空）
        b.motion = Motion::Grounded;
        b.release_hold();
        assert_eq!(b.motion, Motion::Grounded);
    }

    #[test]
    fn snap_to_stage_keeps_a_hover_pin_inside_the_new_stage() {
        let w = world();
        let mut b = Body::new(Vec2::new(900.0, 300.0), 16.0);
        b.hold_in_air(&w);
        assert_eq!(b.pos, Vec2::new(900.0, 300.0));
        // 换成更矮更窄的舞台：x 夹进新墙、y 夹进新地面、档与速度不动
        let narrow = World::single("m", 400.0, 200.0, 1.0);
        b.snap_to_stage(&narrow);
        assert_eq!(b.pos.x, narrow.right_wall() - b.half_width);
        assert_eq!(
            b.pos.y,
            narrow.ground_y(),
            "新地面更高 ⇒ 夹上去，不许沉到屏外"
        );
        assert_eq!(b.motion, Motion::Float);
        // 高度仍在新舞台带内时：保高度（"钉跟着显示器走"的空中支）
        let tall = World::single("m", 1000.0, 600.0, 1.0);
        b.hold_in_air(&tall);
        let at = b.pos;
        b.snap_to_stage(&tall);
        assert_eq!(b.pos, at);
    }
}
