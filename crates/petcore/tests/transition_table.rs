//! M1 验收之一：§3.3 迁移表的每一行都必须真的被走过，且不允许出现表外的迁移。

mod common;

use common::*;
use petcore::behavior::{Action, AssetKey, Command, State, Trigger};
use petcore::geom::{Rect, Vec2};
use petcore::world::{Support, WorldEvent};
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
    assert!(
        h.sim.body.support.is_some(),
        "跳完该有支撑，实际 {:?}",
        h.sim.body.support
    );
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

/// 抓起、抛出、落地。⚠️ 必须走 `cfg_throwing()`：默认 `pin_on_release=true` 之后，
/// 这条场景保留的是**旧抛出语义**的 `(Dragged, Fall, PointerUp)` 非钉样本。
fn drag_and_throw() -> HashSet<EdgeKey> {
    let mut h = Harness::new(cfg_throwing(), 81, stage(1000.0, 600.0));
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
    assert!(!h.sim.machine.pinned(), "抛出语义不许留下钉住旗");
    h.fired
}

/// E1 松手即钉（§3.5-j）：脚落在支撑面上 ⇒ Dragged→Idle 钉坐；
/// 钉坐稳态 = 零骰子（rng counter 在 1200 tick 里不许走）。
fn pin_ground_release() -> HashSet<EdgeKey> {
    let mut h = Harness::new(cfg_quiet(), 82, stage(1000.0, 600.0));
    h.goto_state(State::Idle, 300);
    h.send(Command::DragStart);
    h.run(2);
    let g = h.sim.world.ground_y();
    let x = h.sim.body.pos.x + 150.0;
    h.send(Command::DragMove {
        to: Vec2::new(x, g - 0.2),
    });
    h.run(1);
    h.send(Command::DragEnd);
    h.run(1);
    h.assert_edge(State::Dragged, State::Idle, Trigger::PointerUp);
    assert!(h.sim.machine.pinned(), "支撑面松手必须钉住");
    assert_eq!(h.sim.body.pos.y, g, "settle_on 应把脚吸附到地面线");
    assert_eq!(h.sim.body.motion, petcore::physics::Motion::Grounded);
    let c0 = h.sim.machine.rng().counter();
    h.run(1200);
    assert_eq!(
        h.sim.machine.rng().counter(),
        c0,
        "钉坐稳态的 RNG 消耗必须是 0（骰子、interval、入睡倒计时都不许走）"
    );
    assert_eq!(
        h.state(),
        State::Idle,
        "钉坐不许自己醒来、也不许被拖走后自动入睡"
    );
    h.fired
}

/// E2 + E3：贴墙半空松手 ⇒ 挂墙钉；爬速与体力全冻结；Unpin ⇒ 脱墙坠落。
/// 体力 3s ≪ 观察窗 20s：若"冻结"没生效，`StaminaDepleted` 早把宠物摔下来了（反假绿对照）。
fn pin_wall_release_and_unpin() -> HashSet<EdgeKey> {
    let mut h = Harness::new(cfg_climbing(200.0, 3.0), 83, stage(1000.0, 600.0));
    h.goto_state(State::Idle, 300);
    h.send(Command::DragStart);
    h.run(2);
    let x = h.sim.world.right_wall();
    h.send(Command::DragMove {
        to: Vec2::new(x, 300.0),
    });
    h.run(4);
    h.send(Command::DragEnd);
    h.run(1);
    h.assert_edge(State::Dragged, State::Climb, Trigger::PointerUp);
    assert!(h.sim.machine.pinned(), "贴墙半空松手必须挂墙钉");
    assert_eq!(h.sim.body.pos.y, 300.0);
    let y = h.sim.body.pos.y;
    h.run(20 * 60);
    assert_eq!(
        h.state(),
        State::Climb,
        "20s 后还该挂在墙上（体力冻结、不滑不落）"
    );
    assert_eq!(h.sim.body.pos.y, y, "挂墙钉不许动");
    h.send(Command::Unpin);
    h.run(1);
    h.assert_edge(State::Climb, State::Fall, Trigger::Command);
    assert!(!h.sim.machine.pinned());
    h.run_until_state(State::Idle, 6000);
    h.assert_edge(State::Fall, State::Land, Trigger::TouchedDown);
    h.fired
}

/// E4 挂墙钉 + 舞台变宽 ⇒ 墙根移走，宠物脱墙坠落、**解钉重活**（悬空钉不走这行，
/// Float 随舞台浮动；2026-11-26 翻向，旧语义"落地改钉坐"随悬空钉批作废）。
/// ⚠️ 本场景最易自欺：钉在**旧右墙** x=936，再把舞台从 1000 扩到 1200 ⇒ 新墙根 1136，
/// 旧墙根成了舞台内部。中间那组断言就是"这确实是个会发生的场景"的手工验。
/// 尾段在新墙**重钉**一次：(Dragged,Climb,PointerUp) 的样本只有这一处，删了会红覆盖门禁。
fn pin_wall_stage_detach() -> HashSet<EdgeKey> {
    let mut h = Harness::new(cfg_climbing(200.0, 600.0), 84, stage(1000.0, 600.0));
    h.goto_state(State::Idle, 300);
    h.send(Command::DragStart);
    h.run(2);
    let x = h.sim.world.right_wall();
    h.send(Command::DragMove {
        to: Vec2::new(x, 300.0),
    });
    h.run(4);
    h.send(Command::DragEnd);
    h.run(1);
    h.assert_edge(State::Dragged, State::Climb, Trigger::PointerUp);
    assert!(h.sim.machine.pinned());

    h.world_event(WorldEvent::MonitorGeometry {
        monitor: "m".into(),
        rect: Rect::new(0.0, 0.0, 1200.0, 600.0),
        scale: 1.0,
        panel_bottom: 0.0,
    });
    // 手工验"场景是真的"：resize 后旧墙根必须已经**不贴**新墙，否则 E4 是永不触发的假场景
    let lo = h.sim.world.left_wall() + h.sim.body.half_width;
    let hi = h.sim.world.right_wall() - h.sim.body.half_width;
    assert!(
        h.sim.body.pos.x > lo + 1.0 && h.sim.body.pos.x < hi - 1.0,
        "resize 后宠物应位于新舞台内部，实际 x={} ∈ [{lo},{hi}]",
        h.sim.body.pos.x
    );
    h.run(1);
    h.assert_edge(State::Climb, State::Fall, Trigger::SupportGone);
    h.run_until_state(State::Idle, 6000);
    assert!(
        !h.sim.machine.pinned(),
        "E4 翻向：墙根没了 = 挂着的东西消失，钉该解开让它重新生活"
    );
    assert_eq!(
        h.sim.body.pos.y,
        h.sim.world.ground_y(),
        "坠落该落地，不许悬空"
    );

    // 重钉段：舞台扩到 1200 后，把宠物拖到**新右墙**再松手 ⇒ (Dragged,Climb,PointerUp) 再现
    h.send(Command::DragStart);
    h.run(2);
    h.send(Command::DragMove {
        to: Vec2::new(h.sim.world.right_wall(), 300.0),
    });
    h.run(4);
    h.send(Command::DragEnd);
    h.run(1);
    h.assert_edge(State::Dragged, State::Climb, Trigger::PointerUp);
    assert!(
        h.sim.machine.pinned(),
        "新墙重钉必须仍然成立 = 拖到墙上松手挂墙，和舞台改没改过无关"
    );
    h.fired
}

/// E1b 悬空钉 + E5 双击放生（§3.5-j′）：半空松手钉住 ⇒ Idle+Float 逐格纹丝不动、
/// 稳态零骰子；`Unpin` 才交还重力 ⇒ (Idle,Fall,Command) → (Fall,Land,TouchedDown) → 落地。
/// ⚠️ 悬空段必须**逐 tick** 比位置：只比末尾会把"慢慢漏下去"读成通过。
fn pin_hover_release_and_drop() -> HashSet<EdgeKey> {
    let mut h = Harness::new(cfg_quiet(), 87, stage(1000.0, 600.0));
    h.goto_state(State::Idle, 300);
    h.send(Command::DragStart);
    h.run(2);
    h.send(Command::DragMove {
        to: Vec2::new(h.sim.body.pos.x + 150.0, 320.0),
    });
    h.run(1);
    h.send(Command::DragEnd);
    h.run(1);
    h.assert_edge(State::Dragged, State::Idle, Trigger::PointerUp);
    assert!(h.sim.machine.pinned());
    assert!(h.sim.body.floating(), "半空松手该悬空，不该已落地");
    assert!(
        h.sim.body.pos.y < h.sim.world.ground_y() - 1.0,
        "悬空高度要真实存在"
    );
    let hover = h.sim.body.pos;
    let c0 = h.sim.machine.rng().counter();
    for _ in 0..1200 {
        h.tick();
        assert_eq!(h.sim.body.pos, hover, "悬空钉每 tick 都该纹丝不动");
        assert_eq!(h.state(), State::Idle, "悬空钉期间不许离开 Idle");
    }
    assert_eq!(
        h.sim.machine.rng().counter(),
        c0,
        "悬空钉稳态不该吃任何骰子"
    );

    h.send(Command::Unpin);
    h.run(1);
    h.assert_edge(State::Idle, State::Fall, Trigger::Command);
    assert!(!h.sim.body.floating(), "放生后该交还重力");
    h.run_until_state(State::Idle, 6000);
    h.assert_edge(State::Fall, State::Land, Trigger::TouchedDown);
    assert_eq!(
        h.sim.body.pos.y,
        h.sim.world.ground_y(),
        "双击放生最终必须落回地面"
    );
    h.fired
}

/// 全屏升空巡线（§3.5-k）：patrol_share=1.0 ⇒ 每次出发都试图升空。
/// 断言形状 = 活性而不是运气：**升到过空**（min y 明显高于地面）、**落得回地**
/// （终点 grounded、hops 归零）、单段悬空有界（不许有一次飞行接近一小时）。
fn patrol_full_screen() -> HashSet<EdgeKey> {
    let mut h = Harness::new(cfg_patrol(3, 0.9), 88, stage(1000.0, 600.0));
    h.goto_state(State::Idle, 300);
    let ground = h.sim.world.ground_y();
    let mut min_y = f32::MAX;
    let mut max_float_run = 0u32;
    let mut float_run = 0u32;
    let mut departures = 0;
    let mut prev_departures = 0;
    let mut ever_walk_2d = false;
    for _ in 0..60 * 60 * 20 {
        h.tick();
        if h.sim.body.floating() {
            float_run += 1;
            max_float_run = max_float_run.max(float_run);
            if matches!(h.state(), State::Walk | State::Run) {
                ever_walk_2d = true;
            }
            min_y = min_y.min(h.sim.body.pos.y);
        } else {
            float_run = 0;
        }
        let d = h.sim.machine.patrol_departures();
        if d != prev_departures {
            departures = d;
            prev_departures = d;
        }
    }
    assert!(
        departures > 0,
        "patrol_share=1.0 却一次都没升空 = 开关没接线"
    );
    assert!(
        min_y < ground - 200.0,
        "20 分钟里最高只到 y={min_y}（地面 {ground}）⇒ 没有真正升空"
    );
    assert!(ever_walk_2d, "空中腿必须以 Walk/Run 姿态行进（裁决①=侧滑）");
    assert!(
        max_float_run < 3600,
        "最长一段连续悬空 {max_float_run} tick（= {} s）⇒ 疑似卡空中",
        max_float_run as f64 / 60.0
    );
    // 收尾不要求"正好在地上"（停表时刻可以合法地在飞行腿中间），只要求**回得来**：
    // 从任意悬空中断点起，一模拟分钟内必须落回地面态且飞行预算归零。
    let mut back = 0;
    while h.sim.body.floating() && back < 3600 {
        h.tick();
        back += 1;
    }
    assert!(!h.sim.body.floating(), "悬空中断点后 60 s 内没落回地面");
    assert_eq!(
        h.sim.machine.patrol_hops(),
        0,
        "落地后不该留未归零的飞行预算"
    );
    assert!(
        h.sim.body.grounded_at(),
        "巡线终点必须回到地面态，实际 {:?}",
        h.sim.body.support
    );
    h.fired
}

/// 几何定升不升：矮舞台（带宽 < 2×eps 或被 floor_gap 压没）⇒ **永不 Float**、
/// `patrol_departures == 0`，但行走照旧发生（= 退化舞台是"退回旧形状"，不是死机）。
fn patrol_blocked_by_geometry() -> HashSet<EdgeKey> {
    let mut h = Harness::new(cfg_patrol(3, 0.9), 89, stage(1000.0, 100.0));
    h.goto_state(State::Idle, 300);
    let mut walked = 0;
    let mut prev = h.state();
    for _ in 0..60 * 60 * 10 {
        h.tick();
        assert!(!h.sim.body.floating(), "矮舞台不许升空");
        let st = h.state();
        if prev == State::Idle && st == State::Walk {
            walked += 1;
        }
        prev = st;
    }
    assert!(walked > 0, "窄矮舞台仍应正常地面漫游，实测 0 次出发");
    assert_eq!(
        h.sim.machine.patrol_departures(),
        0,
        "air_band 退化成 None 时不该计数巡线出发"
    );
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
        pin_ground_release(),
        pin_wall_release_and_unpin(),
        pin_wall_stage_detach(),
        pin_hover_release_and_drop(),
        patrol_full_screen(),
        patrol_blocked_by_geometry(),
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

/// 抽卡反馈的双击（只发 `DoubleTap`，D14 那条复用）**不解钉、不跑开**：
/// 钉住的宠物原地扭一下。放生走客户端 dbl 臂那两条 `Unpin → DoubleTap`。
#[test]
fn gacha_double_tap_never_unpins() {
    let mut h = Harness::new(cfg_quiet(), 85, stage(1000.0, 600.0));
    h.goto_state(State::Idle, 300);
    let g = h.sim.world.ground_y();
    h.send(Command::DragStart);
    h.run(2);
    h.send(Command::DragMove {
        to: Vec2::new(h.sim.body.pos.x + 150.0, g - 0.2),
    });
    h.run(1);
    h.send(Command::DragEnd);
    h.run(1);
    assert!(h.sim.machine.pinned());
    let c0 = h.sim.machine.rng().counter();
    h.send(Command::DoubleTap);
    h.run(2);
    assert!(h.sim.machine.pinned(), "抽卡 DoubleTap 不许顺手解钉");
    assert_eq!(h.state(), State::Idle, "钉住的宠物被 react 不许跑开");
    assert_eq!(h.sim.machine.rng().counter(), c0, "钉住时纯 react 不掷骰子");

    // 放生形状（客户端 dbl 臂 = Unpin 先、DoubleTap 后，同一 tick 消费）⇒ 解钉 + 跑开
    h.send(Command::Unpin);
    h.send(Command::DoubleTap);
    h.run(1);
    assert!(!h.sim.machine.pinned(), "Unpin 必须先解钉");
    h.assert_edge(State::Idle, State::Run, Trigger::DoubleTap);

    // 悬空钉（E1b）同判：只发 DoubleTap ⇒ 钉、位置、高度、骰子全不动（不顺手放生）。
    // ⚠️ 这里杀的是"悬空钉其实会掉、只是没人看见"那类假绿：y 与 motion 逐格断言，不比末尾。
    let mut w = Harness::new(cfg_quiet(), 86, stage(1000.0, 600.0));
    w.goto_state(State::Idle, 300);
    w.send(Command::DragStart);
    w.run(2);
    w.send(Command::DragMove {
        to: Vec2::new(w.sim.body.pos.x + 150.0, 320.0),
    });
    w.run(1);
    w.send(Command::DragEnd);
    w.run(1);
    assert!(w.sim.machine.pinned(), "半空松手应钉住");
    assert!(w.sim.body.floating(), "半空钉应是 Float，不是已落地");
    let hover = w.sim.body.pos;
    let c1 = w.sim.machine.rng().counter();
    for _ in 0..600 {
        w.tick();
        assert_eq!(
            w.sim.body.pos, hover,
            "悬空钉被 DoubleTap 之前不许有一格漂移"
        );
    }
    w.send(Command::DoubleTap);
    w.run(2);
    assert!(w.sim.machine.pinned(), "抽卡 DoubleTap 不许顺手解悬空钉");
    assert!(w.sim.body.floating(), "抽卡 DoubleTap 不许把悬空钉打落");
    assert_eq!(w.sim.body.pos, hover, "react 不该挪位置");
    assert_eq!(w.state(), State::Idle, "悬空钉 react 后仍应停着");
    assert_eq!(
        w.sim.machine.rng().counter(),
        c1,
        "悬空钉纯 react 不掷骰子（同地面钉的口径）"
    );
}

/// 路线链（§3.5-i）：一次出发、多跳途经点；**续跳只吃 2 枚骰子**（选半区 + 采样），
/// 不再掷出发骰/interval —— 这是"续跳复用 ActionDice/TargetFar 表键但不走 roll_action"的证据。
#[test]
fn route_multi_hop_single_departure() {
    // 400 窄舞台（默认清单半宽 64 ⇒ 可达区间 272px < run_distance ⇒ 全程 Walk）；pause 0 ⇒ 到点即走
    let mut h = Harness::new(cfg_routes(6, 0.0, 0.0), 301, stage(400.0, 600.0));
    h.goto_state(State::Idle, 300);
    // 出发骰里的链长是 rng 决定的：固定 seed 下循环定位，不是碰运气
    let mut hops = 0;
    for _ in 0..64 {
        h.goto_state(State::Walk, 6000);
        hops = h.sim.machine.route_remaining();
        if hops > 0 {
            break;
        }
        h.goto_state(State::Idle, 6000);
    }
    assert!(hops > 0, "64 次出发没有起过一条路线链");
    h.goto_state(State::Idle, 12000);
    let c0 = h.sim.machine.rng().counter();
    h.goto_state(State::Walk, 12000);
    let d = h.sim.machine.rng().counter() - c0;
    assert_eq!(
        d, 2,
        "路线续跳应只吃 2 枚骰子（chance+uniform），实测 {d} ⇒ 混进了出发骰/interval"
    );
    assert_eq!(h.sim.machine.route_remaining(), hops - 1);
}

/// 半区偏好绊线（非概率证明，固定 seed 的回归栅栏）：5 模拟分钟里两个半区都要有
/// 出发，且偏多一边不许超过 75%；bias=0 的对照同样不许窝在一边。
#[test]
fn route_zone_balance() {
    for bias in [0.35_f32, 0.0] {
        let mut h = Harness::new(cfg_routes(9, bias, 0.0), 302, stage(1000.0, 600.0));
        h.goto_state(State::Idle, 3000);
        let mid = (h.sim.world.left_wall() + h.sim.world.right_wall()) * 0.5;
        let mut left = 0usize;
        let mut right = 0usize;
        let mut prev = h.state();
        for _ in 0..5 * 60 * 60 {
            h.tick();
            let st = h.state();
            if prev == State::Idle && matches!(st, State::Walk | State::Run) {
                // enter(Walk/Run) 不清 target，出发当刻的落点可读
                if let Some(t) = h.sim.machine.target() {
                    if (t.x - mid).abs() > 0.5 {
                        if t.x < mid {
                            left += 1;
                        } else {
                            right += 1;
                        }
                    }
                }
            }
            prev = st;
        }
        let total = left + right;
        assert!(total >= 20, "bias={bias}: 5 模拟分钟出发次数不足: {total}");
        let share = left.max(right) as f64 / total as f64;
        assert!(
            share <= 0.75,
            "bias={bias}: 半区分布失衡 L={left} R={right}（偏好机制失效或装反）"
        );
    }
}

/// `route_max ≤ 1` 必须逐位保住旧单跳形状：与"完全关链"（route_max=0）同 seed 对照，
/// 每 tick 骰子消耗 / 位置 / 状态全同。`Rng::range(0,0)` 空区间不吃子是这条等价的另一半依据。
#[test]
fn route_max_1_matches_old_shape() {
    let mut a = Harness::new(cfg_routes(0, 0.35, 0.9), 303, stage(1000.0, 600.0));
    let mut b = Harness::new(cfg_routes(1, 0.35, 0.9), 303, stage(1000.0, 600.0));
    for t in 0..5 * 60 * 60 {
        a.tick();
        b.tick();
        assert_eq!(
            a.sim.machine.rng().counter(),
            b.sim.machine.rng().counter(),
            "tick {t}: route_max=1 比关链多吃了子"
        );
        assert_eq!(a.sim.body.pos, b.sim.body.pos, "tick {t}: 轨迹分叉");
        assert_eq!(a.state(), b.state(), "tick {t}: 状态分叉");
        assert_eq!(a.sim.machine.route_remaining(), 0);
        assert_eq!(b.sim.machine.route_remaining(), 0, "第 {t} tick 起了续跳");
    }
    assert!(
        a.fired
            .iter()
            .any(|(f, t, _)| *f == State::Idle && *t == State::Walk),
        "对照组从未走过路 = 这条测例是空的"
    );
}

#[test]
fn one_shot_touches_nothing_but_the_overlay_slot() {
    let mut h = Harness::new(cfg_quiet(), 501, stage(1000.0, 600.0));
    h.goto_state(State::Idle, 300);
    let pre_state = h.state();
    let pre_pinned = h.sim.machine.pinned();
    let pre_route = h.sim.machine.route_remaining();
    let pre_patrol = h.sim.machine.patrol_hops();
    let pre_departures = h.sim.machine.patrol_departures();
    let pre_rng = h.sim.machine.rng().counter();

    h.sim.send(Command::OneShot(AssetKey::Dance));
    let (transitions, _intent) = h.sim.step();

    assert_eq!(h.state(), pre_state, "OneShot 不改状态");
    assert_eq!(h.sim.machine.pinned(), pre_pinned);
    assert_eq!(h.sim.machine.route_remaining(), pre_route);
    assert_eq!(h.sim.machine.patrol_hops(), pre_patrol);
    assert_eq!(h.sim.machine.patrol_departures(), pre_departures);
    assert_eq!(h.sim.machine.rng().counter(), pre_rng, "OneShot 不掷骰");
    assert!(
        transitions.is_empty(),
        "OneShot 在 Idle 下不发迁移: {transitions:?}"
    );
}

#[test]
fn one_shot_wakes_sleep_on_the_existing_edge() {
    let mut h = Harness::new(cfg_sleepy(), 502, stage(1000.0, 600.0));
    h.goto_state(State::Idle, 300);
    h.send(Command::SystemIdle(true));
    h.run_until_state(State::Sleep, 3000);
    assert_eq!(h.state(), State::Sleep);

    h.sim.send(Command::OneShot(AssetKey::Dance));
    let (transitions, _intent) = h.sim.step();

    let woke = transitions
        .iter()
        .any(|t| t.from == State::Sleep && t.to == State::Idle && t.trigger == Trigger::Wakeup);
    assert!(
        woke,
        "OneShot 在 Sleep 下必须走 Wakeup 边醒来: {transitions:?}"
    );
}

#[test]
fn one_shot_after_double_tap_wins_the_slot() {
    let mut a = Harness::new(cfg_quiet(), 503, stage(1000.0, 600.0));
    a.goto_state(State::Idle, 300);
    a.sim.send(Command::DoubleTap);
    let (_, intent_a) = a.sim.step();

    let mut b = Harness::new(cfg_quiet(), 503, stage(1000.0, 600.0));
    b.goto_state(State::Idle, 300);
    b.sim.send(Command::DoubleTap);
    b.sim.send(Command::OneShot(AssetKey::Dance));
    let (_, intent_b) = b.sim.step();

    assert_eq!(
        a.state(),
        b.state(),
        "DoubleTap 的 Run 迁移不受 OneShot 影响"
    );
    let key_a = intent_a.sprite.map(|s| s.key);
    let key_b = intent_b.sprite.map(|s| s.key);
    assert_eq!(
        key_a,
        Some(AssetKey::React),
        "对照组: DoubleTap 的 react 叠加应生效（manifest 有 react 行）"
    );
    assert_ne!(
        key_b,
        Some(AssetKey::React),
        "实验组: OneShot(Dance) 占了槽位后 react 不再生效（Dance 不在 manifest ⇒ 无叠加）"
    );
}
