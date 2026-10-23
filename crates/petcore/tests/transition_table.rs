//! M1 验收之一：§3.3 迁移表的每一行都必须真的被走过，且不允许出现表外的迁移。

mod common;

use common::*;
use petcore::behavior::{Action, Command, State, Trigger};
use petcore::geom::Vec2;
use petcore::world::Support;
use std::collections::HashSet;

/// 从 Spawn 走到 Idle。
fn spawn_to_idle() -> HashSet<EdgeKey> {
    let mut h = Harness::new(cfg_quiet(), 1, stage(1000.0, 600.0));
    h.goto_state(State::Idle, 300);
    h.assert_edge(State::Spawn, State::Idle, Trigger::Spawned);
    h.fired
}

/// 窄舞台（任意目标都在 300px 内）+ 骰子必中 ⇒ Idle→Walk。
fn dice_walk() -> HashSet<EdgeKey> {
    let mut h = Harness::new(cfg_always_walk(), 3, stage(300.0, 600.0));
    h.goto_state(State::Walk, 3000);
    h.assert_edge(State::Idle, State::Walk, Trigger::ActionDice);
    h.fired
}

/// 骰子必中跳 ⇒ Idle→Jump。
fn dice_jump() -> HashSet<EdgeKey> {
    let mut h = Harness::new(cfg_always_jump(), 5, stage(1000.0, 600.0));
    h.goto_state(State::Jump, 3000);
    h.assert_edge(State::Idle, State::Jump, Trigger::ActionJumpDice);
    h.fired
}

/// 指令行走/奔跑，以及"距离太远自动升级成奔跑"。
fn commanded_movement() -> HashSet<EdgeKey> {
    let mut h = Harness::new(cfg_quiet(), 7, stage(1000.0, 600.0));
    h.goto_state(State::Idle, 300);
    let here = h.sim.body.pos.x;
    h.send(Command::WalkTo { x: here + 100.0 });
    h.run(2);
    h.assert_edge(State::Idle, State::Walk, Trigger::Command);
    h.goto_state(State::Idle, 3000);
    h.send(Command::RunTo { x: here + 100.0 });
    h.run(2);
    h.assert_edge(State::Idle, State::Run, Trigger::Command);

    // 宽舞台：目标太远 ⇒ 由距离守卫升级成 Run
    let mut wide = Harness::new(cfg_quiet(), 8, stage(4000.0, 600.0));
    wide.goto_state(State::Idle, 300);
    wide.send(Command::WalkTo { x: 3900.0 });
    wide.run(2);
    wide.assert_edge(State::Idle, State::Run, Trigger::TargetFar);

    // 正在走的时候来了一个更远的目标 ⇒ Walk→Run/TargetFar（单独舞台，起点在正中）
    let mut back = Harness::new(cfg_quiet(), 9, stage(4000.0, 600.0));
    back.goto_state(State::Idle, 300);
    let x = back.sim.body.pos.x;
    back.send(Command::WalkTo { x: x + 200.0 });
    back.run_until_state(State::Walk, 3000);
    back.run(1);
    back.send(Command::WalkTo { x: x - 3000.0 });
    back.run(2);
    back.assert_edge(State::Walk, State::Run, Trigger::TargetFar);

    let mut s = h.fired;
    s.extend(wide.fired.iter().cloned());
    s.extend(back.fired.iter().cloned());
    s
}

/// 到达⇒Stop⇒Idle，以及失焦停车。
fn arrival_and_focus() -> HashSet<EdgeKey> {
    let mut h = Harness::new(cfg_quiet(), 11, stage(1000.0, 600.0));
    h.goto_state(State::Idle, 300);
    let here = h.sim.body.pos.x;

    h.send(Command::WalkTo { x: here + 60.0 });
    h.run_until_state(State::Stop, 3000);
    h.assert_edge(State::Walk, State::Stop, Trigger::Arrived);
    h.run_until_state(State::Idle, 3000);
    h.assert_edge(State::Stop, State::Idle, Trigger::Decelerated);

    h.send(Command::RunTo { x: here + 500.0 });
    h.run_until_state(State::Stop, 6000);
    h.assert_edge(State::Run, State::Stop, Trigger::Arrived);

    // 失焦停车
    let mut f = Harness::new(cfg_quiet(), 12, stage(1000.0, 600.0));
    f.goto_state(State::Idle, 300);
    let x = f.sim.body.pos.x;
    f.send(Command::WalkTo { x: x + 300.0 });
    f.run_until_state(State::Walk, 2000);
    f.send(Command::FocusLost);
    f.run(1);
    f.assert_edge(State::Walk, State::Stop, Trigger::FocusLost);

    f.goto_state(State::Idle, 3000);
    f.send(Command::RunTo { x: x - 300.0 });
    f.run_until_state(State::Run, 2000);
    f.send(Command::FocusLost);
    f.run(1);
    f.assert_edge(State::Run, State::Stop, Trigger::FocusLost);

    let mut s = h.fired;
    s.extend(f.fired.iter().cloned());
    s
}

/// 指令跳 ⇒ Jump→Land→Idle 整条链。
fn jump_chain() -> HashSet<EdgeKey> {
    let mut h = Harness::new(cfg_quiet(), 21, stage(1000.0, 600.0));
    h.goto_state(State::Idle, 300);
    h.send(Command::Go(Action::Jump));
    h.run_until_state(State::Idle, 3000);
    h.assert_edge(State::Idle, State::Jump, Trigger::Command);
    h.assert_edge(State::Jump, State::Land, Trigger::TouchedDown);
    h.assert_edge(State::Land, State::Idle, Trigger::SquashDone);
    assert_eq!(h.sim.body.support, Some(Support::Ground));
    h.fired
}

/// 走/跑时前方 24px 有台阶 ⇒ 自己跳上去。
fn obstacle_jump() -> HashSet<EdgeKey> {
    // 起点在舞台正中 x=1000，目标距离压在 run_distance(300) 以内 ⇒ 全程保持 Walk
    let world = world_with_platform(2000.0, 600.0, win("desk", 600.0, 900.0, 500.0));
    let mut h = Harness::new(cfg_quiet(), 31, world);
    h.goto_state(State::Idle, 300);
    assert!(
        h.sim.body.pos.x > 900.0,
        "起点应在台面右侧，实际 {}",
        h.sim.body.pos.x
    );
    h.send(Command::WalkTo { x: 780.0 });
    h.run_until_state(State::Jump, 4000);
    h.assert_edge(State::Walk, State::Jump, Trigger::ObstacleAhead);

    // 奔跑中遇到台阶同样跳（表格只给 Walk/Run 两支）
    let mut r = Harness::new(
        cfg_quiet(),
        32,
        world_with_platform(2000.0, 600.0, win("desk", 600.0, 900.0, 500.0)),
    );
    r.goto_state(State::Idle, 300);
    r.send(Command::RunTo { x: 100.0 });
    r.run_until_state(State::Jump, 4000);
    r.assert_edge(State::Run, State::Jump, Trigger::ObstacleAhead);

    let mut s = h.fired;
    s.extend(r.fired.iter().cloned());
    s
}

/// 撞墙⇒爬⇒坐到舞台顶边⇒指令跳下。
fn climb_to_ceiling() -> HashSet<EdgeKey> {
    // 舞台宽度压到 400：到墙根的距离 < run_distance，接近过程保持步行（§3.3 "走到墙根再爬"）
    let mut h = Harness::new(cfg_climbing(200.0, 600.0), 41, stage(400.0, 300.0));
    h.goto_state(State::Idle, 300);
    h.send(Command::WalkTo { x: 0.0 });
    h.run_until_state(State::Climb, 6000);
    h.assert_edge(State::Walk, State::Climb, Trigger::WallTouched);
    h.run_until_state(State::WallSit, 6000);
    h.assert_edge(State::Climb, State::WallSit, Trigger::PlatformReached);
    assert_eq!(h.sim.body.support, Some(Support::Ceiling));
    h.send(Command::Go(Action::Jump));
    h.run(1);
    h.assert_edge(State::WallSit, State::Jump, Trigger::Command);
    h.fired
}

/// 坐到窗台上，窗口被关掉 ⇒ 坠落到地面。
fn climb_to_window_then_it_closes() -> HashSet<EdgeKey> {
    let world = world_with_platform(400.0, 600.0, win("win", 0.0, 100.0, 300.0));
    let mut h = Harness::new(cfg_climbing(200.0, 600.0), 51, world);
    h.goto_state(State::Idle, 300);
    h.send(Command::WalkTo { x: 0.0 });
    h.run_until_state(State::WallSit, 12000);
    h.assert_edge(State::Climb, State::WallSit, Trigger::PlatformReached);
    assert_eq!(h.sim.body.support, Some(Support::Platform(1)));

    h.world_event(removed("win"));
    h.run_until_state(State::Idle, 6000);
    h.assert_edge(State::WallSit, State::Fall, Trigger::SupportGone);
    h.assert_edge(State::Fall, State::Land, Trigger::TouchedDown);
    h.assert_edge(State::Land, State::Idle, Trigger::SquashDone);
    assert_eq!(h.sim.body.pos.y, 600.0, "应落回地面");
    h.fired
}

/// 体力耗尽 ⇒ 从墙上掉下来。
fn climb_runs_out_of_stamina() -> HashSet<EdgeKey> {
    let mut h = Harness::new(cfg_climbing(1.0, 0.5), 61, stage(400.0, 600.0));
    h.goto_state(State::Idle, 300);
    h.send(Command::WalkTo { x: 0.0 });
    h.run_until_state(State::Climb, 6000);
    h.run_until_state(State::Fall, 6000);
    h.assert_edge(State::Climb, State::Fall, Trigger::StaminaDepleted);
    h.fired
}

/// 坐久了自己跳下。
fn restless_sitter() -> HashSet<EdgeKey> {
    let mut h = Harness::new(cfg_restless_sitter(), 71, stage(400.0, 300.0));
    h.goto_state(State::Idle, 300);
    h.send(Command::WalkTo { x: 0.0 });
    h.run_until_state(State::WallSit, 12000);
    h.run_until_state(State::Jump, 6000);
    h.assert_edge(State::WallSit, State::Jump, Trigger::ActionJumpDice);
    h.fired
}

/// 抓起、抛出、落地。
fn drag_and_throw() -> HashSet<EdgeKey> {
    let mut h = Harness::new(cfg_quiet(), 81, stage(1000.0, 600.0));
    h.goto_state(State::Idle, 300);
    h.send(Command::DragStart);
    h.run(2);
    h.assert_edge(State::Idle, State::Dragged, Trigger::PointerDown);
    let p = h.sim.body.pos;
    h.send(Command::DragMove {
        to: Vec2::new(p.x + 40.0, p.y - 200.0),
    });
    h.run(10);
    h.send(Command::DragEnd);
    h.run_until_state(State::Idle, 6000);
    h.assert_edge(State::Dragged, State::Fall, Trigger::PointerUp);
    h.assert_edge(State::Fall, State::Land, Trigger::TouchedDown);
    h.fired
}

/// 双击摸头 ⇒ react 一次性动画 + 跑开。
fn double_tap() -> HashSet<EdgeKey> {
    let mut h = Harness::new(cfg_quiet(), 91, stage(1000.0, 600.0));
    h.goto_state(State::Idle, 300);
    h.send(Command::DoubleTap);
    h.run(2);
    h.assert_edge(State::Idle, State::Run, Trigger::DoubleTap);
    h.fired
}

/// 空闲超时入睡、唤醒、手动暂停。
fn sleeping() -> HashSet<EdgeKey> {
    let mut h = Harness::new(cfg_sleepy(), 101, stage(1000.0, 600.0));
    h.goto_state(State::Idle, 300);
    h.send(Command::SystemIdle(true));
    h.run_until_state(State::Sleep, 3000);
    h.assert_edge(State::Idle, State::Sleep, Trigger::IdleTimeout);
    h.send(Command::Wake);
    h.run(1);
    h.assert_edge(State::Sleep, State::Idle, Trigger::Wakeup);

    // 系统重新活跃也要醒
    let mut b = Harness::new(cfg_sleepy(), 102, stage(1000.0, 600.0));
    b.goto_state(State::Idle, 300);
    b.send(Command::SystemIdle(true));
    b.run_until_state(State::Sleep, 3000);
    b.send(Command::SystemIdle(false));
    b.run(1);
    b.assert_edge(State::Sleep, State::Idle, Trigger::Wakeup);

    // 菜单暂停（睡眠开关要开着）
    let mut c = Harness::new(cfg_climbing(30.0, 8.0), 103, stage(1000.0, 600.0));
    c.goto_state(State::Idle, 300);
    c.send(Command::Go(Action::Sleep));
    c.run(1);
    c.assert_edge(State::Idle, State::Sleep, Trigger::ManualSleep);
    c.send(Command::Wake);
    c.run(1);
    c.assert_edge(State::Sleep, State::Idle, Trigger::Wakeup);

    let mut s = h.fired;
    s.extend(b.fired.iter().cloned());
    s.extend(c.fired.iter().cloned());
    s
}

fn all_scenarios() -> HashSet<EdgeKey> {
    let mut all: HashSet<EdgeKey> = HashSet::new();
    for fired in [
        spawn_to_idle(),
        dice_walk(),
        dice_jump(),
        commanded_movement(),
        arrival_and_focus(),
        jump_chain(),
        obstacle_jump(),
        climb_to_ceiling(),
        climb_to_window_then_it_closes(),
        climb_runs_out_of_stamina(),
        restless_sitter(),
        drag_and_throw(),
        double_tap(),
        sleeping(),
    ] {
        assert_no_off_table_edges(&fired);
        all.extend(fired);
    }
    all
}

/// 所有场景合起来必须恰好覆盖 §3.3 表：每行都被走过，且没有表外迁移。
#[test]
fn transition_table_is_fully_covered() {
    let fired = all_scenarios();
    assert_matches_transition_table(&fired);
    assert!(
        fired.len() >= petcore::behavior::TRANSITION_TABLE.len(),
        "覆盖的迁移数 {} 少于表行数 {}",
        fired.len(),
        petcore::behavior::TRANSITION_TABLE.len()
    );
}

/// 表外迁移的检查本身要真的会失败（防止 helper 写错变成空断言）。
#[test]
#[should_panic(expected = "表外")]
fn off_table_check_catches_extras() {
    let mut fired: HashSet<EdgeKey> = HashSet::new();
    fired.insert((State::Idle, State::Climb, Trigger::Arrived));
    assert_no_off_table_edges(&fired);
}

/// 覆盖率检查本身也要真的会失败。
#[test]
#[should_panic(expected = "没被走过")]
fn coverage_check_catches_missing_rows() {
    assert_all_table_rows_covered(&HashSet::new());
}
