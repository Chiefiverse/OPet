//! Headless 模拟器：把世界 + 物理 + 状态机 + 帧调度串成固定步长循环。
//!
//! M1 的三条验收都落在这里：迁移表路径覆盖（`tests/transition_table.rs`）、
//! 8 小时无位置漂移（[`Sim::check_invariants`]）、纯逻辑吞吐（[`Sim::run`] + `opet sim bench`）。

use crate::assets::manifest::Manifest;
use crate::assets::scheduler::{Scheduler, SpriteFrame};
use crate::behavior::config::BehaviorConfig;
use crate::behavior::machine::Command;
use crate::behavior::state::{State, Transition};
use crate::geom::{Rect, Vec2};
use crate::intent::{InputRegion, PresentationIntent, SURFACE_MARGIN};
use crate::physics::Body;
use crate::world::{Support, World, WorldEvent};

/// 内核固定步长（§3.2：接受 60 Hz tick，渲染层可降频采样）。
pub const TICK_HZ: f32 = 60.0;
pub const DT: f32 = 1.0 / TICK_HZ;

#[derive(Clone, Debug)]
pub struct SimStats {
    pub ticks: u64,
    /// 真正需要提交新 buffer 的 tick（省电口径）
    pub frames_presented: u64,
    /// 只移动 surface 的 tick
    pub surface_moves: u64,
    pub frames_skipped: u64,
    pub transitions: u64,
    pub state_ticks: [(State, u64); 12],
}

impl Default for SimStats {
    fn default() -> Self {
        SimStats {
            ticks: 0,
            frames_presented: 0,
            surface_moves: 0,
            frames_skipped: 0,
            transitions: 0,
            state_ticks: [
                (State::Spawn, 0),
                (State::Idle, 0),
                (State::Walk, 0),
                (State::Run, 0),
                (State::Stop, 0),
                (State::Jump, 0),
                (State::Land, 0),
                (State::Climb, 0),
                (State::WallSit, 0),
                (State::Fall, 0),
                (State::Dragged, 0),
                (State::Sleep, 0),
            ],
        }
    }
}

#[derive(Clone, Debug)]
pub struct Sim<'m> {
    pub world: World,
    pub body: Body,
    pub machine: crate::behavior::BehaviorMachine,
    scheduler: Scheduler<'m>,
    manifest: &'m Manifest,
    pending: Vec<Command>,
    last_pos: Vec2,
    last_sprite: Option<SpriteFrame>,
    stats: SimStats,
}

#[derive(Debug)]
pub struct DriftError {
    pub tick: u64,
    pub reason: String,
}

impl std::fmt::Display for DriftError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "tick {} 不变量被破坏: {}", self.tick, self.reason)
    }
}

impl<'m> Sim<'m> {
    pub fn new(world: World, manifest: &'m Manifest, seed: u64, cfg: BehaviorConfig) -> Sim<'m> {
        let half_width = manifest.grid.w as f32 / 2.0;
        let start = Vec2::new(
            world.stage().rect.left() + world.stage().rect.size.x * 0.5,
            world.ground_y(),
        );
        let mut body = Body::new(start, half_width);
        body.support = Some(Support::Ground);
        let machine = crate::behavior::BehaviorMachine::new(cfg, seed);
        let facing = machine.facing();
        let mut scheduler = Scheduler::new(manifest, facing);
        scheduler.set_fps_caps(30.0, 5.0);
        Sim {
            world,
            body,
            machine,
            scheduler,
            manifest,
            pending: Vec::new(),
            last_pos: start,
            last_sprite: None,
            stats: SimStats::default(),
        }
    }

    pub fn send(&mut self, cmd: Command) {
        self.pending.push(cmd);
    }

    pub fn apply_world_event(&mut self, ev: WorldEvent) {
        let impact = self.world.apply(ev);
        if impact.stage_changed || impact.geometry_changed {
            self.machine.on_stage_change(&self.world, &mut self.body);
        }
    }

    /// 推进一个 tick，返回本 tick 的迁移与呈现意图（无变化时 `intent.needs_redraw == false`）。
    pub fn step(&mut self) -> (Vec<Transition>, PresentationIntent) {
        let outcome = self.body.step(DT, &self.world);
        let cmds = std::mem::take(&mut self.pending);
        let transitions = self
            .machine
            .tick(DT, &self.world, &mut self.body, &outcome, &cmds);

        let sprite = self.scheduler.advance(
            DT,
            self.machine.state(),
            self.machine.facing(),
            self.machine.take_one_shot(),
        );

        let moved = (self.body.pos.x - self.last_pos.x).abs() > 0.001
            || (self.body.pos.y - self.last_pos.y).abs() > 0.001;
        let needs_redraw = sprite.is_some() || moved || self.last_sprite.is_none();
        self.last_pos = self.body.pos;
        if let Some(s) = sprite {
            self.last_sprite = Some(s);
        }

        self.stats.ticks += 1;
        self.stats.transitions += transitions.len() as u64;
        if sprite.is_some() {
            self.stats.frames_presented += 1;
        } else if needs_redraw {
            self.stats.surface_moves += 1;
        } else {
            self.stats.frames_skipped += 1;
        }
        if let Some(entry) = self
            .stats
            .state_ticks
            .iter_mut()
            .find(|(s, _)| *s == self.machine.state())
        {
            entry.1 += 1;
        }

        let intent = self.intent(sprite, needs_redraw);
        (transitions, intent)
    }

    pub fn run(&mut self, ticks: u64) -> &SimStats {
        for _ in 0..ticks {
            self.step();
        }
        &self.stats
    }

    /// 跑 `hours` 小时的模拟时间（60Hz ⇒ 216k tick/小时）。
    pub fn run_hours(&mut self, hours: f32) -> &SimStats {
        self.run((hours * 3600.0 * TICK_HZ) as u64)
    }

    pub fn stats(&self) -> &SimStats {
        &self.stats
    }

    pub fn intent(&self, sprite: Option<SpriteFrame>, needs_redraw: bool) -> PresentationIntent {
        let w = self.manifest.grid.w as f32;
        let h = self.manifest.grid.h as f32;
        let feet = self.manifest.feet_anchor();
        let origin = Vec2::new(
            self.body.pos.x - feet[0] - SURFACE_MARGIN,
            self.body.pos.y - feet[1] - SURFACE_MARGIN,
        );
        PresentationIntent {
            tick: self.machine.tick_count(),
            state: self.machine.state(),
            facing: self.machine.facing(),
            sprite,
            anchor: self.body.pos,
            surface: Rect::new(
                origin.x,
                origin.y,
                w + SURFACE_MARGIN * 2.0,
                h + SURFACE_MARGIN * 2.0,
            ),
            input_region: InputRegion::rect(w, h, SURFACE_MARGIN),
            needs_redraw,
        }
    }

    /// 状态指纹：用于"同 seed 重放必须逐位一致"的断言。
    pub fn fingerprint(&self) -> u64 {
        fn mix(h: u64, v: u64) -> u64 {
            let mut x = h ^ v;
            x = x.wrapping_mul(0x100000001b3);
            x.rotate_left(13)
        }
        let mut h = 0xcbf2_9ce4_8422_2325;
        h = mix(h, self.machine.tick_count());
        h = mix(h, self.machine.state() as u64);
        h = mix(h, self.body.pos.x.to_bits() as u64);
        h = mix(h, self.body.pos.y.to_bits() as u64);
        h = mix(h, self.body.vel.x.to_bits() as u64);
        h = mix(h, self.body.vel.y.to_bits() as u64);
        h = mix(h, self.machine.facing().index() as u64);
        h = mix(h, self.machine.rng().counter());
        if let Some(s) = self.last_sprite {
            h = mix(h, s.row as u64);
            h = mix(h, s.col as u64);
            h = mix(h, s.frame as u64);
        }
        h
    }

    /// 每 tick 都该成立的世界不变量（M1 验收："headless 模拟 8h 无位置漂移"）。
    pub fn check_invariants(&self) -> Result<(), DriftError> {
        let bad = |reason: String| DriftError {
            tick: self.machine.tick_count(),
            reason,
        };
        if !self.body.pos.is_finite() || !self.body.vel.is_finite() {
            return Err(bad(format!("非有限坐标 {:?}", self.body.pos)));
        }
        let lo = self.world.left_wall() + self.body.half_width;
        let hi = self.world.right_wall() - self.body.half_width;
        if hi > lo && !(lo - 0.01..=hi + 0.01).contains(&self.body.pos.x) {
            return Err(bad(format!("x {} 跑出舞台 [{lo}, {hi}]", self.body.pos.x)));
        }
        let top = self.world.ceiling_y();
        let bottom = self.world.ground_y();
        if !(top - 0.01..=bottom + 0.01).contains(&self.body.pos.y) {
            return Err(bad(format!(
                "y {} 跑出舞台 [{top}, {bottom}]",
                self.body.pos.y
            )));
        }
        if self.body.motion == crate::physics::Motion::Grounded {
            let support = self.body.support.unwrap_or(Support::Ground);
            if let Some(y) = self.world.support_y(support, self.body.pos.x) {
                if (self.body.pos.y - y).abs() > 0.001 {
                    return Err(bad(format!(
                        "着地态但 y {} 不等于支撑面 {y}（漂移 {:?}）",
                        self.body.pos.y,
                        self.body.pos.y - y
                    )));
                }
            }
        }
        // 状态与物理模式不能互相矛盾
        let grounded_state = self.machine.state().is_grounded();
        let grounded_body = self.body.motion == crate::physics::Motion::Grounded;
        if grounded_state && !grounded_body {
            return Err(bad(format!(
                "状态 {:?} 应是着地态，但物理是 {:?}",
                self.machine.state(),
                self.body.motion
            )));
        }
        if matches!(
            self.machine.state(),
            State::Walk | State::Run | State::Stop | State::Land | State::Idle | State::WallSit
        ) && self.body.motion == crate::physics::Motion::Climbing
        {
            return Err(bad(format!("状态 {:?} 不该处于攀爬", self.machine.state())));
        }
        Ok(())
    }
}

/// 面向"8 方向"的高层封装：给 CLI 与测试用，把 tick 循环包起来。
pub fn run_headless(mut sim: Sim<'_>, ticks: u64) -> Result<(SimStats, u64), DriftError> {
    for _ in 0..ticks {
        sim.step();
        sim.check_invariants()?;
    }
    let fp = sim.fingerprint();
    Ok((sim.stats().clone(), fp))
}
