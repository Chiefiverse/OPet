//! 行为状态机：§3.3 迁移表的实现。
//!
//! 每 tick 的顺序是 **物理事实 → 用户指令 → 自主决策**：落地/撞墙/支撑消失是既成事实，必须先被承认；
//! 用户指令优先于自主决策。内核不读时钟、不读鼠标，输入一律以 [`Command`] 形式进来（可复现）。

use super::config::{Action, BehaviorConfig};
use super::state::{AssetKey, State, Transition, Trigger};
use crate::geom::{Facing, Vec2};
use crate::physics::{Body, Motion, StepOutcome};
use crate::rng::Rng;
use crate::world::{SUPPORT_EPS, Support, WallContact, World};

/// 来自 UI / IPC / 系统的事件。指针命中判定在渲染适配层做（§3.7），内核只收到"按下了"。
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Command {
    /// 右键菜单 / `opet action set walk --target 80%`
    Go(Action),
    /// 走到指定 x（舞台逻辑坐标）
    WalkTo {
        x: f32,
    },
    /// 跑到指定 x
    RunTo {
        x: f32,
    },
    /// 指针按下并命中宠物
    DragStart,
    DragMove {
        to: Vec2,
    },
    DragEnd,
    DoubleTap,
    /// 解除钉住（§3.5-j）：单向，未钉住时空转。独立臂——**不**与 DoubleTap 合并，
    /// 否则抽卡反馈那次双击会顺手解钉（D14 复用同一条 Command::DoubleTap）。
    Unpin,
    FocusLost,
    /// swayidle / Hyprland 活动信号
    SystemIdle(bool),
    /// 快捷键/IPC 显式唤醒
    Wake,
    /// 批2 §3.5-a：用户指定的叠加动画（--action-map 路由）。不改状态、不掷骰；
    /// 睡眠中被点名则走既有 (Sleep→Idle, Wakeup) 边醒。
    OneShot(AssetKey),
}

#[derive(Clone, Debug)]
pub struct BehaviorMachine {
    state: State,
    cfg: BehaviorConfig,
    rng: Rng,
    facing: Facing,
    /// 移动目标（§3.5-k 起是二维的：竖直分量只由巡线腿写入，
    /// 所有旧路径的 `y` 恒等于出发时刻的 `body.pos.y` ⇒ x 逻辑逐位旧行为）
    target: Option<Vec2>,
    /// 自主决策倒计时（Idle 骰子、WallSit 坐多久）
    action_timer: f32,
    /// 过渡态剩余时长（Spawn/Stop/Land）
    state_timer: f32,
    /// 距上次用户输入的秒数（Sleep 守卫）
    input_idle: f32,
    climb_stamina: f32,
    system_idle: bool,
    one_shot: Option<AssetKey>,
    /// 拖拽钉住（§3.5-j）：真 = 压制自主骰子与自动入睡，位置由支撑面/墙钉住
    pinned: bool,
    /// 路线链剩余途经点数（§3.5-i）
    route_remaining: u8,
    /// 本次升空还剩几条 Float 腿（§3.5-k）。上限 = route_max，封顶 ⇒ 不卡空中。
    patrol_hops: u8,
    /// 升空巡线出发次数（指纹与覆盖测例的读数）
    patrol_departures: u32,
    /// 上一次舞台变化是否打断了空中段（测例读数）
    descent_clamped: bool,
    /// 最近 8 跳实际落点的半区环形账（true = 左半区）。定长数组，禁 HashMap（硬约束 8）。
    zone_hist: [bool; 8],
    zone_head: usize,
    zone_len: usize,
    tick: u64,
}

impl BehaviorMachine {
    pub fn new(cfg: BehaviorConfig, seed: u64) -> Self {
        let mut rng = Rng::new(seed);
        let action_timer = rng.uniform(cfg.action_interval_min, cfg.action_interval_max);
        Self {
            state: State::Spawn,
            cfg,
            rng,
            facing: Facing::East,
            target: None,
            action_timer,
            state_timer: 0.0,
            input_idle: 0.0,
            climb_stamina: 0.0,
            system_idle: false,
            one_shot: None,
            pinned: false,
            route_remaining: 0,
            patrol_hops: 0,
            patrol_departures: 0,
            descent_clamped: false,
            zone_hist: [false; 8],
            zone_head: 0,
            zone_len: 0,
            tick: 0,
        }
    }

    pub const fn state(&self) -> State {
        self.state
    }

    pub const fn facing(&self) -> Facing {
        self.facing
    }

    pub const fn target(&self) -> Option<Vec2> {
        self.target
    }

    pub const fn tick_count(&self) -> u64 {
        self.tick
    }

    pub const fn cfg(&self) -> &BehaviorConfig {
        &self.cfg
    }

    pub const fn climb_stamina(&self) -> f32 {
        self.climb_stamina
    }

    pub const fn pinned(&self) -> bool {
        self.pinned
    }

    pub const fn route_remaining(&self) -> u8 {
        self.route_remaining
    }

    pub const fn patrol_hops(&self) -> u8 {
        self.patrol_hops
    }

    pub const fn patrol_departures(&self) -> u32 {
        self.patrol_departures
    }

    pub const fn descent_clamped(&self) -> bool {
        self.descent_clamped
    }

    pub fn rng(&self) -> &Rng {
        &self.rng
    }

    /// 一次性动画请求（`react`），由资产调度器取走消费。
    pub fn take_one_shot(&mut self) -> Option<AssetKey> {
        self.one_shot.take()
    }

    /// 世界几何变化后调用：宠物可能被夹回舞台（拔屏/改分辨率/面板 resize）。
    pub fn on_stage_change(&mut self, world: &World, body: &mut Body) {
        body.snap_to_stage(world);
        self.target = None;
        self.route_remaining = 0;
        self.patrol_hops = 0;
        // 空中段被打断 ⇒ 落地由清空的链+自主 Idle 的空中支当格接管；留一个可读数给测例
        self.descent_clamped = body.floating();
        // E4 语义（§3.5-j′ 翻向）：挂墙钉的墙被摘 ⇒ 解钉重活（悬空钉不受影响，
        // snap_to_stage 的 Float 支带着高度走）。解了钉，autonomous 的 Climb 支
        // 才会走体力耗尽 ⇒ 发出既有的 (Climb, Fall, SupportGone) 表边。
        if body.climb_frozen() && !Self::near_any_wall(world, body) {
            self.pinned = false;
        }
    }

    pub fn tick(
        &mut self,
        dt: f32,
        world: &World,
        body: &mut Body,
        outcome: &StepOutcome,
        commands: &[Command],
    ) -> Vec<Transition> {
        self.tick += 1;
        self.action_timer -= dt;
        self.state_timer -= dt;
        self.input_idle += dt;
        let mut out = Vec::new();

        self.apply_physics(body, outcome, &mut out);

        for cmd in commands {
            if self.consume(*cmd, dt, world, body, &mut out) {
                self.input_idle = 0.0;
            }
        }

        self.autonomous(dt, world, body, &mut out);

        out
    }

    fn apply_physics(&mut self, body: &mut Body, outcome: &StepOutcome, out: &mut Vec<Transition>) {
        if outcome.perched.is_some() && self.state == State::Climb {
            debug_assert_eq!(body.motion, Motion::Grounded);
            self.goto(State::WallSit, Trigger::PlatformReached, body, out);
        }

        if outcome.landed.is_some()
            && self.state != State::Land
            && self.state != State::Idle
            && self.state != State::Sleep
        {
            self.goto(State::Land, Trigger::TouchedDown, body, out);
        }

        if outcome.slipped_off && self.state.is_grounded() {
            self.goto(State::Fall, Trigger::SupportGone, body, out);
        }

        if outcome.wall != WallContact::None {
            // 空中撞墙不是事件：巡线的终点本来就可能在墙的那一侧（横向被夹停 ⇒ 竖腿照走）。
            // 若在这里翻 facing 或进 Climb，(Float, Climb, WallTouched) 既是表外边，
            // 又会把"贴着墙继续爬高度"变成"面朝墙顶住 ⇒ x 永不到位 ⇒ 卡死"（实测卡 4 小时）。
            if !body.floating() {
                self.facing = match outcome.wall {
                    WallContact::Left => Facing::West,
                    WallContact::Right => Facing::East,
                    WallContact::None => self.facing,
                };
                if self.state.is_grounded() && body.grounded_at() {
                    if self.cfg.enabled.climb {
                        self.goto(State::Climb, Trigger::WallTouched, body, out);
                    } else {
                        // 到墙即停：爬被禁用时不要一直顶着墙推
                        self.target = None;
                        self.goto(State::Stop, Trigger::Arrived, body, out);
                    }
                }
            }
        }
    }

    /// 返回 `true` 表示这是一次用户输入（Sleep 的 5 分钟判定要看它）。
    fn consume(
        &mut self,
        cmd: Command,
        dt: f32,
        world: &World,
        body: &mut Body,
        out: &mut Vec<Transition>,
    ) -> bool {
        match cmd {
            Command::Go(action) => {
                self.handle_go(action, world, body, out);
                true
            }
            Command::WalkTo { x } => {
                self.route_remaining = 0;
                self.set_target(body, x);
                self.start_moving(Trigger::Command, body, out);
                true
            }
            Command::RunTo { x } => {
                self.route_remaining = 0;
                self.set_target(body, x);
                if self.cfg.enabled.run && self.state.is_grounded() {
                    self.goto(State::Run, Trigger::Command, body, out);
                } else {
                    self.start_moving(Trigger::Command, body, out);
                }
                true
            }
            Command::DragStart => {
                self.route_remaining = 0;
                if self.state != State::Dragged {
                    self.goto(State::Dragged, Trigger::PointerDown, body, out);
                }
                true
            }
            Command::DragMove { to } => {
                if self.state == State::Dragged {
                    body.drag_to(world, to, dt);
                }
                true
            }
            Command::DragEnd => {
                if self.state == State::Dragged {
                    body.end_drag();
                    if self.cfg.pin_on_release {
                        // 松手即钉（§3.5-j/j′）：支撑面 > 墙 > 悬空。悬空那支不再坠落
                        // ——hold_in_air 就地钉住（E1b，Idle 姿零骰子；用户判词"不会怪"）。
                        self.pinned = true;
                        if let Some((support, y)) = Self::release_support(world, body) {
                            body.settle_on(support, y);
                            self.goto(State::Idle, Trigger::PointerUp, body, out);
                        } else if Self::near_any_wall(world, body) {
                            body.stop_horizontal();
                            self.goto(State::Climb, Trigger::PointerUp, body, out);
                        } else {
                            body.hold_in_air(world);
                            self.goto(State::Idle, Trigger::PointerUp, body, out);
                        }
                    } else {
                        self.goto(State::Fall, Trigger::PointerUp, body, out);
                    }
                }
                true
            }
            Command::DoubleTap => {
                self.one_shot = Some(AssetKey::React);
                if self.state == State::Sleep {
                    self.goto(State::Idle, Trigger::Wakeup, body, out);
                }
                // 钉住的宠物被抽卡 react = 原地扭一下，不跑开；放生走 Unpin 那条臂
                if !self.pinned && self.cfg.enabled.run && self.state.is_grounded() {
                    if self.target.is_none() {
                        self.pick_target(world, body);
                    }
                    self.goto(State::Run, Trigger::DoubleTap, body, out);
                }
                true
            }
            Command::Unpin => {
                if self.pinned {
                    self.pinned = false;
                    self.route_remaining = 0;
                    match self.state {
                        State::Climb => {
                            body.release_climb();
                            self.goto(State::Fall, Trigger::Command, body, out);
                        }
                        // E5 放生（§3.5-j′）：悬空钉交还重力 ⇒ Fall→Land→Idle。
                        // 地面钉坐不在这支 —— 就地 settle，"双击掉到地面"只对悬空成立。
                        State::Idle if body.floating() => {
                            body.release_hold();
                            self.goto(State::Fall, Trigger::Command, body, out);
                        }
                        State::Idle => {
                            if let Some((support, y)) = Self::release_support(world, body) {
                                body.settle_on(support, y);
                            }
                        }
                        _ => {}
                    }
                    // 钉坐期间 action_timer 不消耗，可能已是负值：当场补掷一次，
                    // 防"解钉当格抢跑"
                    self.action_timer = self
                        .rng
                        .uniform(self.cfg.action_interval_min, self.cfg.action_interval_max);
                }
                true
            }
            Command::FocusLost => {
                if self.cfg.stop_on_focus_lost && matches!(self.state, State::Walk | State::Run) {
                    self.goto(State::Stop, Trigger::FocusLost, body, out);
                }
                false
            }
            Command::SystemIdle(idle) => {
                self.system_idle = idle;
                if !idle && self.state == State::Sleep {
                    self.goto(State::Idle, Trigger::Wakeup, body, out);
                }
                false
            }
            Command::Wake => {
                if self.state == State::Sleep {
                    self.goto(State::Idle, Trigger::Wakeup, body, out);
                }
                true
            }
            Command::OneShot(key) => {
                if self.state == State::Sleep {
                    self.goto(State::Idle, Trigger::Wakeup, body, out);
                }
                self.one_shot = Some(key);
                true
            }
        }
    }

    fn handle_go(
        &mut self,
        action: Action,
        world: &World,
        body: &mut Body,
        out: &mut Vec<Transition>,
    ) {
        if action == Action::Sleep {
            // 悬空不许睡（§3.5-k 登记限制）：Sleep 是着地态，先双击落地再暂停
            if self.cfg.enabled.sleep && self.state.is_grounded() && body.grounded_at() {
                self.goto(State::Sleep, Trigger::ManualSleep, body, out);
            }
            return;
        }
        if self.state == State::Sleep {
            self.goto(State::Idle, Trigger::Wakeup, body, out);
        }
        // 显式指令优先于路线链
        self.route_remaining = 0;
        if !self.cfg.is_enabled(action) || !self.state.is_grounded() {
            return;
        }
        match action {
            Action::Walk => {
                self.pick_target(world, body);
                self.start_moving(Trigger::Command, body, out);
            }
            Action::Run => {
                if self.target.is_none() {
                    self.pick_target(world, body);
                }
                self.goto(State::Run, Trigger::Command, body, out);
            }
            Action::Jump => self.do_jump(Trigger::Command, body, out),
            Action::Climb => {
                // 先走到最近的墙，触墙后由 WallTouched 进入 Climb
                self.set_target(body, self.nearest_wall_x(world, body));
                self.start_moving(Trigger::Command, body, out);
            }
            Action::Sleep => {}
        }
    }

    fn autonomous(&mut self, dt: f32, world: &World, body: &mut Body, out: &mut Vec<Transition>) {
        match self.state {
            State::Spawn | State::Stop | State::Land => {
                if self.state_timer <= 0.0 {
                    let (to, trigger) = match self.state {
                        State::Spawn => (State::Idle, Trigger::Spawned),
                        State::Stop => (State::Idle, Trigger::Decelerated),
                        _ => (State::Idle, Trigger::SquashDone),
                    };
                    self.goto(to, trigger, body, out);
                }
            }
            State::Idle => {
                // 钉坐/钉悬空：不吃骰子、不自动入睡（本案钉住 = 不许自主入睡）。
                // 稳态 RNG 消耗 = 0，钉一夜 counter 也不走。
                if self.pinned {
                    return;
                }
                // 空中驻留（§3.5-k）：巡线的腿间停留走固定 route_pause（0 枚），
                // 到点只有两个去处 —— 续跳（3 枚）或直落下降腿（0 枚）。
                // 不掷行动骰、不入睡：Float 的 Sleep/Climb 在不变量里结构不可能。
                if body.floating() {
                    if self.action_timer <= 0.0 {
                        if self.route_remaining > 0 {
                            self.route_remaining -= 1;
                            self.patrol_hops = self.patrol_hops.saturating_sub(1);
                            let band = self.air_band(world, body);
                            self.pick_route_target(world, body, band);
                            self.start_moving(Trigger::ActionDice, body, out);
                        } else {
                            self.begin_descent(world, body, out);
                        }
                    }
                    return;
                }
                if self.cfg.enabled.sleep
                    && self.system_idle
                    && self.input_idle >= self.cfg.sleep_after
                {
                    self.goto(State::Sleep, Trigger::IdleTimeout, body, out);
                    return;
                }
                if self.action_timer <= 0.0 {
                    if self.route_remaining > 0 {
                        // 路线链续跳（§3.5-i）：复用 (Idle,Walk,ActionDice)/(Idle,Run,TargetFar)
                        // 两条既有表键，不新增 Trigger
                        self.route_remaining -= 1;
                        self.pick_route_target(world, body, None);
                        self.start_moving(Trigger::ActionDice, body, out);
                    } else {
                        self.roll_action(world, body, out);
                    }
                }
            }
            State::Walk | State::Run => {
                self.update_facing(body);
                let eps = self.cfg.arrive_epsilon;
                let arrived = match self.target {
                    None => true,
                    Some(t) => {
                        if !body.floating() {
                            // 地面腿逐位旧形状（`|dx| ≤ eps` 那一刀不许改，见等价门）
                            (body.pos.x - t.x).abs() <= eps
                        } else {
                            // 空中腿 = 匀速直线，**会冲过目标**：max 归一定速让两轴同时到位，
                            // 而 `|d| ≤ eps` 的窗口只有 1/16 格宽 ⇒ 判定必然漏过，漏过之后
                            // 身体继续飞到被夹住，`dx` 与 `dy` 再没有同时成立的一格（实测卡死）。
                            // 所以逐轴判"到达或越过"；第三项是被夹住的兜底（含天花板、同向过冲）。
                            let at_x = (body.pos.x - t.x).abs() <= eps
                                || (body.pos.x - t.x) * body.vel.x >= 0.0;
                            let at_y = (body.pos.y - t.y).abs() <= eps
                                || (body.pos.y - t.y) * body.vel.y >= 0.0;
                            let x_pinned = (body.pos.x
                                <= world.left_wall() + body.half_width + eps
                                && t.x < body.pos.x)
                                || (body.pos.x >= world.right_wall() - body.half_width - eps
                                    && t.x > body.pos.x);
                            let y_pinned = (body.pos.y <= world.ceiling_y() + eps
                                && t.y < body.pos.y)
                                || (body.pos.y >= world.ground_y() - eps && t.y > body.pos.y);
                            (at_x || x_pinned) && (at_y || y_pinned)
                        }
                    }
                };
                if arrived {
                    self.target = None;
                    if body.floating() {
                        // 巡线腿收口（§3.5-k）：Float 不产 outcome，这里自己站住 ——
                        // 够得着支撑面（不含天花板）就当场 settle 成地面态，否则零速悬停
                        // 等 Stop→Idle 的空中支决定续跳还是下降。
                        body.stop_horizontal();
                        body.set_vertical_speed(0.0);
                        if let Some((support, y)) = Self::ground_or_floor_support(world, body, eps)
                        {
                            body.settle_on(support, y);
                        }
                    }
                    self.goto(State::Stop, Trigger::Arrived, body, out);
                    return;
                }
                if self.cfg.enabled.jump {
                    let dir = self.facing.horizontal_sign();
                    // 只跳"跳得过去"的台阶；更高的当作墙，交给 WallTouched
                    if world
                        .obstacle_ahead(
                            body.pos.x,
                            dir,
                            self.cfg.obstacle_lookahead,
                            body.pos.y,
                            self.cfg.jump_height_max,
                        )
                        .is_some()
                    {
                        self.do_jump(Trigger::ObstacleAhead, body, out);
                    }
                }
            }
            State::Climb => {
                if self.pinned {
                    // 挂墙钉（§3.5-j）：爬速 0 ⇒ slip/登顶结构上不可能，体力冻结。
                    // 唯一出边 = 舞台几何变化把墙摘了（E4，解钉在 on_stage_change）。
                    if !Self::near_any_wall(world, body) {
                        body.release_climb();
                        self.goto(State::Fall, Trigger::SupportGone, body, out);
                    }
                    return;
                }
                if body.climb_frozen() && !Self::near_any_wall(world, body) {
                    // E4 的下半场：on_stage_change 已把摘墙的挂墙钉解了钉（§3.5-j′ 翻向），
                    // 冻结爬速没了墙 ⇒ 按"支撑消失"脱墙坠落，之后恢复普通生活。
                    body.release_climb();
                    self.goto(State::Fall, Trigger::SupportGone, body, out);
                    return;
                }
                self.climb_stamina -= dt;
                if self.climb_stamina <= 0.0 {
                    body.release_climb();
                    self.goto(State::Fall, Trigger::StaminaDepleted, body, out);
                }
            }
            State::WallSit => {
                if self.pinned {
                    return;
                }
                // 坐够了自己跳下（表里只写了"跳下"，没写谁发起）
                if self.action_timer <= 0.0 {
                    self.do_jump(Trigger::ActionJumpDice, body, out);
                }
            }
            State::Jump | State::Fall | State::Dragged | State::Sleep => {}
        }
    }

    fn roll_action(&mut self, world: &World, body: &mut Body, out: &mut Vec<Transition>) {
        let dice = self.rng.next_f32();
        self.action_timer = self
            .rng
            .uniform(self.cfg.action_interval_min, self.cfg.action_interval_max);
        let walk_share = self.cfg.walk_share;

        if self.cfg.enabled.walk && dice < walk_share {
            // walk 带内再分地面/巡线（§3.5-k）：patrol_share=0 时 `walk_share*(1.0-0.0)`
            // 逐位 = walk_share ⇒ 旧形状。升不升最终由几何（air_band）定，不由骰子流定。
            let ground_band = walk_share * (1.0 - self.cfg.patrol_share.clamp(0.0, 1.0));
            let air = if dice >= ground_band {
                self.air_band(world, body)
            } else {
                None
            };
            self.pick_target(world, body);
            // 随机路线（§3.5-i）：walk 那一支起一条 0..route_max-1 的途经点链
            let extra = self.cfg.route_max.saturating_sub(1) as u32;
            self.route_remaining = self.rng.range(0, extra) as u8;
            if let Some((alo, ahi)) = air {
                // 巡线出发（账本：地面三枚 + 高度一枚**最后**，共 4 枚）
                let x = self.target.map_or(body.pos.x, |t| t.x);
                let y = self.rng.uniform(alo, ahi);
                self.target = Some(Vec2::new(x, y));
                self.patrol_hops = self.route_remaining + 1;
                self.patrol_departures += 1;
            }
            self.start_moving(Trigger::ActionDice, body, out);
        } else if self.cfg.enabled.jump && dice < walk_share + self.cfg.jump_share {
            self.do_jump(Trigger::ActionJumpDice, body, out);
        }
        // 其余情况：原地不动，下个 interval 再骰
    }

    /// 按目标距离决定 Walk 还是 Run。距离判定**保持 x-only**（§3.5-k 设计内：
    /// 纯竖直腿永不升级成 Run，竖直观感由 patrol_speed 的方向向量给）。
    fn start_moving(&mut self, trigger: Trigger, body: &mut Body, out: &mut Vec<Transition>) {
        let Some(target) = self.target else {
            return;
        };
        let distance = (target.x - body.pos.x).abs();
        // 走还是跑由距离决定（§3.3：distance > run_distance ⇒ Run），指令只表达"动"
        let (to, trigger) = if distance > self.cfg.run_distance && self.cfg.enabled.run {
            (State::Run, Trigger::TargetFar)
        } else if self.cfg.enabled.walk {
            (State::Walk, trigger)
        } else if self.cfg.enabled.run {
            (State::Run, Trigger::TargetFar)
        } else {
            return;
        };
        self.update_facing(body);
        self.goto(to, trigger, body, out);
    }

    /// Walk/Run 入场定速的**唯一**出口（§3.5-k）：腿带竖直分量 ⇒ max 归一定速
    /// （无 sqrt/hypot）+ `begin_float`；`dy == 0` ⇒ 逐位旧行为（所有地面目标与指令
    /// 目标的 y 都取自采样/设定时刻的 `body.pos.y`，恒为 0）。
    fn apply_motion_speeds(&self, body: &mut Body, speed: f32) {
        let sign = self.facing.horizontal_sign();
        match self.target {
            Some(t) => {
                let dx = t.x - body.pos.x;
                let dy = (t.y - body.pos.y) * self.cfg.patrol_vertical_scale;
                if dy != 0.0 {
                    // 空中腿吃 `patrol_speed_scale`（§3.5-k 默认 2.0）：`walk_speed × 40 px/s`
                    // 直落 944 px 要 23.6 s ⇒ "升空"看起来像慢动作，且一段飞行轻松超过活性门的
                    // 秒数上界。地面支（dy==0）不受影响 ⇒ 逐位旧形状。
                    let air_speed = speed * self.cfg.patrol_speed_scale;
                    let m = dx.abs().max(dy.abs());
                    body.vel = Vec2::new(dx / m * air_speed, dy / m * air_speed);
                    body.begin_float();
                } else {
                    body.set_horizontal_speed(speed * sign);
                }
            }
            None => body.set_horizontal_speed(speed * sign),
        }
    }

    /// 巡线高度带（§3.5-k）：顶/地各留 gap，且带宽必须容得下 `2×arrive_epsilon`
    /// （否则"到达判定"在带内无处成立 = 卡空中）。退化舞台 = None = 不升空。
    fn air_band(&self, world: &World, body: &Body) -> Option<(f32, f32)> {
        if self.cfg.patrol_share <= 0.0 || !self.cfg.enabled.walk {
            return None;
        }
        let lo = world.ceiling_y() + self.cfg.patrol_top_gap;
        let hi = world.ground_y() - self.cfg.patrol_floor_gap;
        let eps = self.cfg.arrive_epsilon;
        if world.reachable_x(body.half_width).is_some() && hi - lo >= 2.0 * eps {
            Some((lo, hi))
        } else {
            None
        }
    }

    /// 下降腿（账本 0 枚）：x 不变直落地面。`patrol_hops` 在这里清零，
    /// 所以"悬空且 hops==0 且链也空"只存在于这一格之内。
    fn begin_descent(&mut self, world: &World, body: &mut Body, out: &mut Vec<Transition>) {
        self.target = Some(Vec2::new(body.pos.x, world.ground_y()));
        self.patrol_hops = 0;
        self.descent_clamped = false;
        self.start_moving(Trigger::ActionDice, body, out);
    }

    fn do_jump(&mut self, trigger: Trigger, body: &mut Body, out: &mut Vec<Transition>) {
        if !self.cfg.enabled.jump || !self.state.is_grounded() {
            return;
        }
        let h = self
            .rng
            .uniform(self.cfg.jump_height_min, self.cfg.jump_height_max);
        body.jump(h);
        self.goto(State::Jump, trigger, body, out);
    }

    fn pick_target(&mut self, world: &World, body: &Body) {
        // y = 当前高度：所有非巡线目标都是水平目标（逐位旧行为）
        match world.reachable_x(body.half_width) {
            Some((lo, hi)) => {
                self.target = Some(Vec2::new(self.rng.uniform(lo, hi), body.pos.y));
            }
            None => self.target = Some(Vec2::new(body.pos.x, body.pos.y)),
        }
        self.update_facing(body);
    }

    /// 路线链一跳（§3.5-i）：左右半区按"最近去过哪边少"加权 → 半区内均匀采样 →
    /// 步距不足 `route_min_step` 时**确定性平移不重掷**。地面跳每跳吃 2 枚骰子；
    /// `air = Some(band)` 时追加第 3 枚采高度（账本：chance→uniform(x)→uniform(y)）。
    /// 落点半区记进环形账（记实际落点，平移越区也自洽）。
    fn pick_route_target(&mut self, world: &World, body: &Body, air: Option<(f32, f32)>) {
        let Some((lo, hi)) = world.reachable_x(body.half_width) else {
            self.target = Some(Vec2::new(body.pos.x, body.pos.y));
            return;
        };
        let mid = (lo + hi) * 0.5;
        let p_left = if self.zone_len == 0 {
            0.5
        } else {
            let window = (self.cfg.route_zone_window as usize).min(self.zone_len);
            let share = self.zone_lefts(window) as f32 / window as f32;
            (0.5 + self.cfg.route_zone_bias * (0.5 - share)).clamp(0.1, 0.9)
        };
        let go_left = self.rng.chance(p_left);
        let (zlo, zhi) = if go_left { (lo, mid) } else { (mid, hi) };
        let mut x = self.rng.uniform(zlo, zhi);
        if (x - body.pos.x).abs() < self.cfg.route_min_step {
            x = if x >= body.pos.x {
                body.pos.x + self.cfg.route_min_step
            } else {
                body.pos.x - self.cfg.route_min_step
            }
            .clamp(zlo, zhi);
        }
        // 高度在 push_zone 之后单独采（第 3 枚）；route_min_step 的平移只管 x，设计内
        let y = match air {
            Some((alo, ahi)) => self.rng.uniform(alo, ahi),
            None => body.pos.y,
        };
        self.target = Some(Vec2::new(x, y));
        self.push_zone(x < mid);
        self.update_facing(body);
    }

    /// 最近 `window` 跳里落在左半区的次数（环形账从最新往回读）。
    fn zone_lefts(&self, window: usize) -> usize {
        (0..window)
            .map(|i| self.zone_hist[(self.zone_head + 7 - i) % 8])
            .filter(|z| *z)
            .count()
    }

    fn push_zone(&mut self, left: bool) {
        self.zone_hist[self.zone_head] = left;
        self.zone_head = (self.zone_head + 1) % 8;
        if self.zone_len < 8 {
            self.zone_len += 1;
        }
    }

    /// 松手点是否踩在某个支撑面上（地面 / 舞台顶边 / 平台顶，各 ±SUPPORT_EPS）。
    /// 多个同时成立取差值最小者。
    fn release_support(world: &World, body: &Body) -> Option<(Support, f32)> {
        let mut best: Option<(f32, Support, f32)> = None;
        let mut consider = |y: f32, support: Support| {
            let d = (body.pos.y - y).abs();
            if d <= SUPPORT_EPS && best.as_ref().is_none_or(|(bd, _, _)| d < *bd) {
                best = Some((d, support, y));
            }
        };
        consider(world.ground_y(), Support::Ground);
        consider(world.ceiling_y(), Support::Ceiling);
        for p in world.stage_platforms() {
            if p.spans(body.pos.x) {
                consider(p.top_y, Support::Platform(p.slot));
            }
        }
        best.map(|(_, s, y)| (s, y))
    }

    /// 锚点是否贴在任何一面墙上（含舞台左右边；退化舞台不成立）。
    fn near_any_wall(world: &World, body: &Body) -> bool {
        let lo = world.left_wall() + body.half_width;
        let hi = world.right_wall() - body.half_width;
        hi > lo && (body.pos.x <= lo + SUPPORT_EPS || body.pos.x >= hi - SUPPORT_EPS)
    }

    /// 巡线到达点正下方/上方是否就有可站的支撑面（地面 / 平台顶，**不含天花板** ——
    /// 悬停位不许倒挂成 Ceiling 钉坐）。与 `release_support` 同族，差在天花板那一支。
    /// ⚠️ 容差**必须**由调用方传入 `arrive_epsilon`：判定用的是"到没到"（eps=1.0），
    /// 若这里仍用 `SUPPORT_EPS=0.5`，腿就会停在 0.66 px 的"毫厘悬空"里 —— 不变量 #7
    /// 与 `max idle gap` 双双判它卡住（实测下降腿每次都差这一格落不了地）。
    fn ground_or_floor_support(world: &World, body: &Body, within: f32) -> Option<(Support, f32)> {
        let mut best: Option<(f32, Support, f32)> = None;
        let mut consider = |y: f32, support: Support| {
            let d = (body.pos.y - y).abs();
            if d <= within && best.as_ref().is_none_or(|(bd, _, _)| d < *bd) {
                best = Some((d, support, y));
            }
        };
        consider(world.ground_y(), Support::Ground);
        for p in world.stage_platforms() {
            if p.spans(body.pos.x) {
                consider(p.top_y, Support::Platform(p.slot));
            }
        }
        best.map(|(_, s, y)| (s, y))
    }

    fn nearest_wall_x(&self, world: &World, body: &Body) -> f32 {
        let lo = world.left_wall();
        let hi = world.right_wall();
        if (body.pos.x - lo) <= (hi - body.pos.x) {
            lo
        } else {
            hi
        }
    }

    fn set_target(&mut self, body: &Body, x: f32) {
        // 指令目标不预剪枝：允许指向墙外，由物理在撞墙时给出 WallContact，
        // 从而区分"走到墙根停下"与"到墙就爬"（§3.3 WallTouched）。
        // y = 当前高度 ⇒ 指令永远是水平目标（§3.5-k：指令不许把宠物吊起来）。
        self.target = Some(Vec2::new(x, body.pos.y));
        self.update_facing(body);
    }

    fn update_facing(&mut self, body: &Body) {
        if let Some(t) = self.target {
            let delta = t.x - body.pos.x;
            // 悬空腿的死区 = arrive_epsilon（近垂直腿不逐帧翻列）；地面腿照旧
            let dead = if body.floating() {
                self.cfg.arrive_epsilon
            } else {
                f32::EPSILON
            };
            if delta.abs() > dead {
                self.facing = Facing::from_horizontal(delta > 0.0);
            }
        }
    }

    fn goto(&mut self, to: State, trigger: Trigger, body: &mut Body, out: &mut Vec<Transition>) {
        let from = self.state;
        if from == to {
            return;
        }
        self.state = to;
        self.enter(to, body);
        out.push(Transition {
            from,
            to,
            trigger,
            tick: self.tick,
        });
    }

    /// 进入状态时的副作用：设速度、起计时器、复位体力。
    fn enter(&mut self, to: State, body: &mut Body) {
        match to {
            State::Spawn => {
                self.state_timer = self.cfg.spawn_duration;
                body.stop_horizontal();
            }
            State::Idle => {
                body.stop_horizontal();
                if body.floating() {
                    body.set_vertical_speed(0.0);
                }
                self.target = None;
                if self.pinned {
                    // 钉坐/钉悬空：不掷骰子、不留入睡倒计时基础 —— 稳态 RNG = 0
                    self.route_remaining = 0;
                } else if self.route_remaining > 0 || body.floating() {
                    // 路线途经点停留 / 巡线腿间驻留：固定常数，不吃骰子
                    self.action_timer = self.cfg.route_pause;
                } else {
                    self.action_timer = self
                        .rng
                        .uniform(self.cfg.action_interval_min, self.cfg.action_interval_max);
                }
            }
            State::Walk => self.apply_motion_speeds(body, self.cfg.walk_speed()),
            State::Run => self.apply_motion_speeds(body, self.cfg.run_speed()),
            State::Stop => {
                self.state_timer = self.cfg.stop_duration;
                body.stop_horizontal();
                if body.floating() {
                    // 空中中停（FocusLost 也会走到这）：驻停不许继续漂
                    body.set_vertical_speed(0.0);
                }
            }
            State::Land => {
                self.state_timer = self.cfg.land_duration;
                body.stop_horizontal();
            }
            // 抛物线：水平速度保持，竖直初速已由 Body::jump 设定
            State::Jump => {}
            State::Climb => {
                if self.pinned {
                    // 挂墙钉：爬速 0、体力不设（Frozen 语义在 autonomous）
                    body.begin_climb(0.0);
                } else {
                    body.begin_climb(self.cfg.climb_speed());
                    self.climb_stamina = self.cfg.climb_stamina;
                }
            }
            State::WallSit => {
                body.stop_horizontal();
                self.action_timer = self
                    .rng
                    .uniform(self.cfg.action_interval_min, self.cfg.action_interval_max);
            }
            // 保留抛出/滑落时的速度
            State::Fall => {}
            State::Dragged => {
                body.begin_drag();
                self.target = None;
                self.route_remaining = 0;
                // 被打断的飞行不恢复（§3.5-k）：松手按 pin_on_release 重新落位/悬空钉
                self.patrol_hops = 0;
            }
            State::Sleep => {
                body.stop_horizontal();
                self.input_idle = 0.0;
            }
        }
    }
}
