//! 测试公共脚手架：一个能收集"走过哪些迁移"的 Sim 外壳。

#![allow(dead_code)]

use std::collections::HashSet;
use std::sync::OnceLock;

use petcore::assets::Manifest;
use petcore::behavior::{
    BehaviorConfig, Command, Edge, EnabledActions, State, TRANSITION_TABLE, Trigger,
};
use petcore::sim::Sim;
use petcore::world::{PlatformKind, PlatformSpec, World, WorldEvent};

pub type EdgeKey = (State, State, Trigger);

pub fn manifest() -> &'static Manifest {
    static M: OnceLock<Manifest> = OnceLock::new();
    M.get_or_init(petcore::assets::default_manifest)
}

pub fn describe(edges: &HashSet<EdgeKey>) -> String {
    let mut lines: Vec<String> = edges
        .iter()
        .map(|(f, t, tr)| format!("{f:?}->{t:?}/{tr:?}"))
        .collect();
    lines.sort();
    lines.join("\n  ")
}

pub struct Harness {
    pub sim: Sim<'static>,
    pub fired: HashSet<EdgeKey>,
}

impl Harness {
    pub fn new(cfg: BehaviorConfig, seed: u64, world: World) -> Harness {
        Harness {
            sim: Sim::new(world, manifest(), seed, cfg),
            fired: HashSet::new(),
        }
    }

    pub fn tick(&mut self) {
        let (transitions, _intent) = self.sim.step();
        for t in transitions {
            self.fired.insert((t.from, t.to, t.trigger));
        }
        self.sim
            .check_invariants()
            .unwrap_or_else(|e| panic!("不变量失败: {e}"));
    }

    pub fn run(&mut self, ticks: u64) {
        for _ in 0..ticks {
            self.tick();
        }
    }

    /// 跑到状态机进入 `to` 为止（最多 `max` tick）。
    pub fn run_until_state(&mut self, to: State, max: u64) -> bool {
        for _ in 0..max {
            self.tick();
            if self.sim.machine.state() == to {
                return true;
            }
        }
        false
    }

    pub fn state(&self) -> State {
        self.sim.machine.state()
    }

    pub fn send(&mut self, cmd: Command) {
        self.sim.send(cmd);
    }

    pub fn world_event(&mut self, ev: WorldEvent) {
        self.sim.apply_world_event(ev);
    }

    /// 断言某条迁移确实被走过，失败时打印全部已走过的迁移。
    pub fn assert_edge(&self, from: State, to: State, t: Trigger) {
        assert!(
            self.fired.contains(&(from, to, t)),
            "缺少迁移 {from:?}->{to:?}/{t:?}\n实际走过:\n  {}",
            describe(&self.fired)
        );
    }

    /// 从当前状态起步，要求它先到达 `via` 状态（用于串联场景）。
    pub fn goto_state(&mut self, to: State, max: u64) {
        assert!(
            self.run_until_state(to, max),
            "没能进入 {to:?}；当前 {:?}",
            self.state()
        );
    }
}

/// 单向校验：只要求表里每一行都被走过（用于单个场景，表外行由全局测试兜住）。
pub fn assert_all_table_rows_covered(fired: &HashSet<EdgeKey>) {
    for row in TRANSITION_TABLE {
        let hit = fired
            .iter()
            .any(|(f, t, tr)| row.from.matches(*f) && *t == row.to && *tr == row.trigger);
        assert!(
            hit,
            "§3.3 表里这行没被走过: {:?} -> {:?} via {:?}（守卫: {}）\n已走过:\n  {}",
            row.from,
            row.to,
            row.trigger,
            row.guard,
            describe(fired)
        );
    }
}

/// 红线：走过 §3.3 表外的迁移即视为 bug（改状态机必须同步回写设计书）。
pub fn assert_no_off_table_edges(fired: &HashSet<EdgeKey>) {
    for (f, t, tr) in fired {
        let known: bool = TRANSITION_TABLE
            .iter()
            .any(|row: &Edge| row.from.matches(*f) && row.to == *t && row.trigger == *tr);
        assert!(known, "走过了一条 §3.3 表外的迁移: {f:?}->{t:?}/{tr:?}");
    }
}

/// 双向校验：表里每一行都被走过，且没有走过表外的迁移。
pub fn assert_matches_transition_table(fired: &HashSet<EdgeKey>) {
    assert_all_table_rows_covered(fired);
    assert_no_off_table_edges(fired);
}

/// 不让自主行为插队的干净配置：多数场景用它来精确控制状态机。
pub fn cfg_quiet() -> BehaviorConfig {
    BehaviorConfig {
        enabled: EnabledActions {
            walk: true,
            run: true,
            jump: true,
            climb: false,
            sleep: false,
        },
        action_interval_min: 1.0e6,
        action_interval_max: 1.0e6,
        walk_share: 0.0,
        jump_share: 0.0,
        ..BehaviorConfig::default()
    }
}

/// 骰子必中行走。
pub fn cfg_always_walk() -> BehaviorConfig {
    BehaviorConfig {
        action_interval_min: 0.0,
        action_interval_max: 0.001,
        walk_share: 1.0,
        jump_share: 0.0,
        ..BehaviorConfig::default()
    }
}

/// 骰子必中跳跃。
pub fn cfg_always_jump() -> BehaviorConfig {
    BehaviorConfig {
        action_interval_min: 0.0,
        action_interval_max: 0.001,
        walk_share: 0.0,
        jump_share: 1.0,
        ..BehaviorConfig::default()
    }
}

/// 允许爬墙的配置。
pub fn cfg_climbing(speed: f32, stamina: f32) -> BehaviorConfig {
    BehaviorConfig {
        enabled: EnabledActions {
            climb: true,
            jump: true,
            ..EnabledActions::all()
        },
        climb_speed: speed,
        climb_stamina: stamina,
        action_interval_min: 1.0e6,
        action_interval_max: 1.0e6,
        walk_share: 0.0,
        jump_share: 0.0,
        ..BehaviorConfig::default()
    }
}

/// 会自己跳下的 WallSit 配置（坐没两帧就走）。
pub fn cfg_restless_sitter() -> BehaviorConfig {
    BehaviorConfig {
        action_interval_min: 0.0,
        action_interval_max: 0.001,
        ..cfg_climbing(200.0, 600.0)
    }
}

/// 很快就能睡着（配合 `SystemIdle(true)`）。
pub fn cfg_sleepy() -> BehaviorConfig {
    BehaviorConfig {
        enabled: EnabledActions {
            sleep: true,
            ..EnabledActions::none()
        },
        action_interval_min: 1.0e6,
        action_interval_max: 1.0e6,
        walk_share: 0.0,
        jump_share: 0.0,
        sleep_after: 0.1,
        ..BehaviorConfig::default()
    }
}

pub fn win(id: &str, x0: f32, x1: f32, top_y: f32) -> PlatformSpec {
    PlatformSpec {
        id: id.into(),
        kind: PlatformKind::WindowTopEdge,
        monitor: STAGE.into(),
        x0,
        x1,
        top_y,
    }
}

pub const STAGE: &str = "m";

pub fn stage(w: f32, h: f32) -> World {
    World::single(STAGE, w, h, 1.0)
}

pub fn world_with_platform(w: f32, h: f32, spec: PlatformSpec) -> World {
    let mut world = stage(w, h);
    world.upsert_platform(spec);
    world
}

pub fn removed(id: &str) -> WorldEvent {
    WorldEvent::PlatformRemoved { id: id.into() }
}
