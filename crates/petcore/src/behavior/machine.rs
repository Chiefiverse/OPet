//! 行为状态机：§3.3 迁移表的实现。
//!
//! 每 tick 的顺序是 **物理事实 → 用户指令 → 自主决策**：落地/撞墙/支撑消失是既成事实，必须先被承认；
//! 用户指令优先于自主决策。内核不读时钟、不读鼠标，输入一律以 [`Command`] 形式进来（可复现）。

use super::config::{Action, BehaviorConfig};
use super::state::{AssetKey, State, Transition, Trigger};
use crate::geom::{Facing, Vec2};
use crate::physics::{Body, Motion, StepOutcome};
use crate::rng::Rng;
use crate::world::{WallContact, World};

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
    FocusLost,
    /// swayidle / Hyprland 活动信号
    SystemIdle(bool),
    /// 快捷键/IPC 显式唤醒
    Wake,
}

#[derive(Clone, Debug)]
pub struct BehaviorMachine {
    state: State,
    cfg: BehaviorConfig,
    rng: Rng,
    facing: Facing,
    target: Option<f32>,
    /// 自主决策倒计时（Idle 骰子、WallSit 坐多久）
    action_timer: f32,
    /// 过渡态剩余时长（Spawn/Stop/Land）
    state_timer: f32,
    /// 距上次用户输入的秒数（Sleep 守卫）
    input_idle: f32,
    climb_stamina: f32,
    system_idle: bool,
    one_shot: Option<AssetKey>,
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
            tick: 0,
        }
    }

    pub const fn state(&self) -> State {
        self.state
    }

    pub const fn facing(&self) -> Facing {
        self.facing
    }

    pub const fn target(&self) -> Option<f32> {
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
            self.facing = match outcome.wall {
                WallContact::Left => Facing::West,
                WallContact::Right => Facing::East,
                WallContact::None => self.facing,
            };
            if self.state.is_grounded() {
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
                self.set_target(body, x);
                self.start_moving(Trigger::Command, body, out);
                true
            }
            Command::RunTo { x } => {
                self.set_target(body, x);
                if self.cfg.enabled.run && self.state.is_grounded() {
                    self.goto(State::Run, Trigger::Command, body, out);
                } else {
                    self.start_moving(Trigger::Command, body, out);
                }
                true
            }
            Command::DragStart => {
                if self.state != State::Dragged {
                    self.goto(State::Dragged, Trigger::PointerDown, body, out);
                }
                true
            }
            Command::DragMove { to } => {
                if self.state == State::Dragged {
                    body.drag_to(to, dt);
                }
                true
            }
            Command::DragEnd => {
                if self.state == State::Dragged {
                    body.end_drag();
                    self.goto(State::Fall, Trigger::PointerUp, body, out);
                }
                true
            }
            Command::DoubleTap => {
                self.one_shot = Some(AssetKey::React);
                if self.state == State::Sleep {
                    self.goto(State::Idle, Trigger::Wakeup, body, out);
                }
                if self.cfg.enabled.run && self.state.is_grounded() {
                    if self.target.is_none() {
                        self.pick_target(world, body);
                    }
                    self.goto(State::Run, Trigger::DoubleTap, body, out);
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
            if self.cfg.enabled.sleep && self.state.is_grounded() {
                self.goto(State::Sleep, Trigger::ManualSleep, body, out);
            }
            return;
        }
        if self.state == State::Sleep {
            self.goto(State::Idle, Trigger::Wakeup, body, out);
        }
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
                if self.cfg.enabled.sleep
                    && self.system_idle
                    && self.input_idle >= self.cfg.sleep_after
                {
                    self.goto(State::Sleep, Trigger::IdleTimeout, body, out);
                    return;
                }
                if self.action_timer <= 0.0 {
                    self.roll_action(world, body, out);
                }
            }
            State::Walk | State::Run => {
                self.update_facing(body);
                let arrived = match self.target {
                    None => true,
                    Some(t) => (body.pos.x - t).abs() <= self.cfg.arrive_epsilon,
                };
                if arrived {
                    self.target = None;
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
                self.climb_stamina -= dt;
                if self.climb_stamina <= 0.0 {
                    body.release_climb();
                    self.goto(State::Fall, Trigger::StaminaDepleted, body, out);
                }
            }
            State::WallSit => {
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
            self.pick_target(world, body);
            self.start_moving(Trigger::ActionDice, body, out);
        } else if self.cfg.enabled.jump && dice < walk_share + self.cfg.jump_share {
            self.do_jump(Trigger::ActionJumpDice, body, out);
        }
        // 其余情况：原地不动，下个 interval 再骰
    }

    /// 按目标距离决定 Walk 还是 Run。
    fn start_moving(&mut self, trigger: Trigger, body: &mut Body, out: &mut Vec<Transition>) {
        let Some(target) = self.target else {
            return;
        };
        let distance = (target - body.pos.x).abs();
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
        match world.reachable_x(body.half_width) {
            Some((lo, hi)) => self.target = Some(self.rng.uniform(lo, hi)),
            None => self.target = Some(body.pos.x),
        }
        self.update_facing(body);
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
        self.target = Some(x);
        self.update_facing(body);
    }

    fn update_facing(&mut self, body: &Body) {
        if let Some(t) = self.target {
            let delta = t - body.pos.x;
            if delta.abs() > f32::EPSILON {
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
                self.target = None;
                self.action_timer = self
                    .rng
                    .uniform(self.cfg.action_interval_min, self.cfg.action_interval_max);
            }
            State::Walk => {
                body.set_horizontal_speed(self.cfg.walk_speed() * self.facing.horizontal_sign());
            }
            State::Run => {
                body.set_horizontal_speed(self.cfg.run_speed() * self.facing.horizontal_sign());
            }
            State::Stop => {
                self.state_timer = self.cfg.stop_duration;
                body.stop_horizontal();
            }
            State::Land => {
                self.state_timer = self.cfg.land_duration;
                body.stop_horizontal();
            }
            // 抛物线：水平速度保持，竖直初速已由 Body::jump 设定
            State::Jump => {}
            State::Climb => {
                body.begin_climb(self.cfg.climb_speed());
                self.climb_stamina = self.cfg.climb_stamina;
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
            }
            State::Sleep => {
                body.stop_horizontal();
                self.input_idle = 0.0;
            }
        }
    }
}
