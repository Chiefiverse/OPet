//! M1 验收门禁之二/之三：8 小时无漂移、同 seed 可重放、Sleep 停帧。
//!
//! 吞吐门禁由 `opet sim bench --release` 测（§6.1），不在这里跑，
//! 因为 release profile 的 `panic = "abort"` 与测试框架的 unwind 冲突。

use petcore::assets::Manifest;
use petcore::behavior::{BehaviorConfig, Command, EnabledActions, State};
use petcore::sim::{Sim, TICK_HZ, run_headless};
use petcore::world::World;

const HOURS_8_TICKS: u64 = 8 * 3600 * TICK_HZ as u64;

fn world() -> World {
    World::single("m", 1920.0, 1080.0, 1.0)
}

/// §6.1：headless 模拟 8 小时，每 tick 校验不变量，位置不得漂移。
#[test]
fn eight_hour_soak_has_no_drift() {
    let m = Manifest::parse(petcore::assets::DEFAULT_MANIFEST_JSON).unwrap();
    let sim = Sim::new(world(), &m, 7, BehaviorConfig::default());
    let (stats, fp) =
        run_headless(sim, HOURS_8_TICKS).unwrap_or_else(|e| panic!("8 小时长跑里出现漂移: {e}"));
    assert_eq!(stats.ticks, HOURS_8_TICKS);
    assert!(stats.transitions > 0, "8 小时里应至少走过若干迁移");
    assert!(fp != 0, "指纹不应为空");
    // 空闲/睡眠占比与提交帧比例是 §4 省电设计的直接指标
    let idle_like = stats
        .state_ticks
        .iter()
        .filter(|(s, _)| matches!(s, State::Idle | State::Sleep))
        .map(|(_, n)| *n)
        .sum::<u64>();
    assert!(
        idle_like as f64 / stats.ticks as f64 > 0.05,
        "8 小时里待机时间异常少（{idle_like}/{}）",
        stats.ticks
    );
}

/// 硬约束 5：`Sleep` 不提交新 buffer。
#[test]
fn sleep_state_commits_no_frames() {
    let m = Manifest::parse(petcore::assets::DEFAULT_MANIFEST_JSON).unwrap();
    let cfg = BehaviorConfig {
        enabled: EnabledActions {
            sleep: true,
            ..EnabledActions::none()
        },
        sleep_after: 0.0,
        action_interval_min: 1.0e6,
        action_interval_max: 1.0e6,
        walk_share: 0.0,
        jump_share: 0.0,
        ..BehaviorConfig::default()
    };
    let mut sim = Sim::new(world(), &m, 3, cfg);
    sim.send(Command::SystemIdle(true));
    for _ in 0..600 {
        sim.step();
        if sim.machine.state() == State::Sleep {
            break;
        }
    }
    assert_eq!(sim.machine.state(), State::Sleep, "没进入 Sleep");
    let before = sim.stats().frames_presented;
    for _ in 0..600 {
        let (_t, intent) = sim.step();
        assert!(!intent.needs_redraw, "Sleep 中仍请求重绘");
        assert!(intent.sprite.is_none(), "Sleep 中仍产出新帧");
    }
    assert_eq!(
        sim.stats().frames_presented,
        before,
        "Sleep 一小时不该提交任何帧"
    );
}

/// 同 seed 必须逐位重放（M3 的概率审计依赖这一点）。
#[test]
fn same_seed_replays_bit_identically() {
    let m = Manifest::parse(petcore::assets::DEFAULT_MANIFEST_JSON).unwrap();
    let run = |seed: u64| {
        let mut a = Sim::new(world(), &m, seed, BehaviorConfig::default());
        let mut b = Sim::new(world(), &m, seed, BehaviorConfig::default());
        for _ in 0..200_000 {
            a.step();
            b.step();
            assert_eq!(
                a.fingerprint(),
                b.fingerprint(),
                "同 seed 重放在 tick {} 分叉",
                a.machine.tick_count()
            );
            assert_eq!(a.body.pos, b.body.pos);
            assert_eq!(a.machine.state(), b.machine.state());
        }
        a.fingerprint()
    };
    let fp = run(1234);
    assert!(fp != 0);
}

/// 不同 seed 要真的分叉（否则"可重放"是没意义的假象）。
#[test]
fn different_seeds_diverge() {
    let m = Manifest::parse(petcore::assets::DEFAULT_MANIFEST_JSON).unwrap();
    let mut a = Sim::new(world(), &m, 1, BehaviorConfig::default());
    let mut b = Sim::new(world(), &m, 2, BehaviorConfig::default());
    for _ in 0..60_000 {
        a.step();
        b.step();
    }
    assert_ne!(a.fingerprint(), b.fingerprint());
}

/// 拔屏/改分辨率时宠物被救回舞台，之后仍能继续正常生活。
#[test]
fn hotplug_monitor_keeps_pet_alive() {
    let m = Manifest::parse(petcore::assets::DEFAULT_MANIFEST_JSON).unwrap();
    let mut sim = Sim::new(
        World::single("m", 1920.0, 1080.0, 1.0),
        &m,
        5,
        BehaviorConfig::default(),
    );
    for _ in 0..600 {
        sim.step();
    }
    sim.apply_world_event(petcore::world::WorldEvent::MonitorGeometry {
        monitor: "m".into(),
        rect: petcore::Rect::new(0.0, 0.0, 1280.0, 720.0),
        scale: 1.0,
        panel_bottom: 40.0,
    });
    let (stats, _fp) = run_headless(sim, 60 * 60 * 30).expect("改分辨率后 30 分钟不应出现非法状态");
    assert_eq!(
        stats.ticks,
        600 + 60 * 60 * 30,
        "统计应包含换分辨率前的 600 tick"
    );
}

/// P5 那条红线的唯一连接：**换一份等格清单再跑 == 全程没换**，只多一帧提交。
///
/// 抄 [`same_seed_replays_bit_identically`] 的重放形状（逐 tick 比，而不是只比末尾），
/// 因为 `swap_manifest` 若把 `stats.ticks` 归零、把 `last_pos` 留在舞台中心、或把 `pending`
/// 丢掉，**只有中途那一两格的读数会分叉**，末尾相同反而说明不了什么。
///
/// ⚠️ 两份清单是**内容相同而对象不同**：这正好把"换了个指针但没重建 `Scheduler`"这类错误
/// 变成不可见的，所以这一条测的不是 `Scheduler` 重建得对不对，而是**其余状态有没有被误清零**。
///
/// ⚠️ 本轮实测出来的**唯一**一处真实差异（三次探针读数相同，所以它是语义不是偶然）：
/// 重建 `Scheduler` 会让**换清单那一格必提交一帧**（新调度器从第 0 帧起放），于是
/// `frames_presented` 此后比"全程没换"多 1、`frames_skipped` 少 1，而动画相位在下一次状态
/// 迁移处重新对齐（实测最后一个分叉 tick = swap+200）。这**正是换本体要的效果**：新本体的
/// 像素必须有一格被提交。所以断言写成"至多多一帧 + 有界窗口后逐位重聚"，而不是"完全相同"。
#[test]
fn swap_manifest_continues_a_never_swapped_run() {
    let a = Manifest::parse(petcore::assets::DEFAULT_MANIFEST_JSON).unwrap();
    let b = Manifest::parse(petcore::assets::DEFAULT_MANIFEST_JSON).unwrap();
    assert_eq!(a, b, "fixture 前提：两份清单内容必须相同");
    assert_eq!(a.grid, b.grid, "fixture 前提：格必须逐字段相等");

    let mut swapped = Sim::new(world(), &a, 11, BehaviorConfig::default());
    let mut plain = Sim::new(world(), &a, 11, BehaviorConfig::default());

    let at: u64 = 3_000;
    // 指纹重聚的上界：实测 200 tick（下一次状态迁移把两条动画都重新锚到第 0 帧），留 6 倍余量
    // 是为了让"行为内核被人改了迁移节奏"红在别的测上，而不是红在这条 P5 的测上。
    let reconverge: u64 = 1_200;
    let horizon: u64 = 30_000;
    let mut extra_frames: Vec<i64> = Vec::new();

    for t in 0..horizon {
        if t == at {
            let before = (
                swapped.body.pos,
                swapped.body.vel,
                swapped.stats().ticks,
                swapped.machine.tick_count(),
                swapped.machine.state(),
            );
            swapped.swap_manifest(&b);
            // 这四样是"其余一切原样"的全部含义；单独断言一次，是为了让将来有人往 swap 里
            // 顺手加一句 reset 时，红的是一条说得出名字的断言，而不是 6 千格之后的指纹。
            assert_eq!(swapped.body.pos, before.0, "换清单不该挪位置");
            assert_eq!(swapped.body.vel, before.1, "换清单不该改速度");
            assert_eq!(swapped.stats().ticks, before.2, "换清单不该归零统计");
            assert_eq!(
                swapped.machine.tick_count(),
                before.3,
                "换清单不该归零状态机"
            );
            assert_eq!(swapped.machine.state(), before.4, "换清单不该改状态");
            assert_eq!(
                swapped.fingerprint(),
                plain.fingerprint(),
                "换清单当刻指纹就该相同（指纹含 last_sprite，见指针陷阱第 1 条）"
            );
        }
        let (sw_tr, sw_intent) = swapped.step();
        let (pl_tr, _) = plain.step();
        // 行为面：迁移、物理、状态机、rng 流必须逐格相同（红线 1 的准确形状 = 行为不变）。
        assert_eq!(sw_tr, pl_tr, "tick {t} 之后迁移序列分叉");
        assert_eq!(swapped.body.pos, plain.body.pos, "tick {t} 之后位置分叉");
        assert_eq!(swapped.body.vel, plain.body.vel, "tick {t} 之后速度分叉");
        assert_eq!(
            swapped.machine.state(),
            plain.machine.state(),
            "tick {t} 之后状态分叉"
        );
        assert_eq!(
            swapped.machine.tick_count(),
            plain.machine.tick_count(),
            "tick {t} 之后状态机计时分叉"
        );
        assert_eq!(
            swapped.machine.rng().counter(),
            plain.machine.rng().counter(),
            "tick {t} 之后 rng 流被换清单动过（红线 1：不许碰 Rng）"
        );
        // 省电口径：ticks / 只移 surface / 迁移数 / 逐状态时长必须**完全**相同。
        let s = swapped.stats();
        let p = plain.stats();
        assert_eq!(s.ticks, p.ticks, "tick {t} 之后总 tick 数分叉");
        assert_eq!(
            s.surface_moves, p.surface_moves,
            "tick {t} 之后 surface 移动数分叉"
        );
        assert_eq!(s.transitions, p.transitions, "tick {t} 之后迁移计数分叉");
        assert_eq!(s.state_ticks, p.state_ticks, "tick {t} 之后逐状态时长分叉");
        // 唯一的容许差异：提交帧数在 [0, +1] 之间，少提交一格都不许（那才是真回归）。
        let delta = s.frames_presented as i64 - p.frames_presented as i64;
        extra_frames.push(delta);
        assert!(
            (0..=1).contains(&delta),
            "tick {t}：换本体之后提交帧数差 {delta}，允许的只有 0 或 +1"
        );
        assert_eq!(
            s.frames_presented as i64 + s.frames_skipped as i64 + s.surface_moves as i64,
            s.ticks as i64,
            "tick {t}：三档计数没有覆盖全部 tick"
        );
        if t > at + reconverge {
            assert_eq!(
                swapped.fingerprint(),
                plain.fingerprint(),
                "tick {t}：早已越过重聚上界，指纹仍分叉 ⇒ swap_manifest 泄漏了持久状态"
            );
        }
        // 换清单那一格必须真的提交一帧：这就是"新本体上屏"在内核侧的唯一读数。
        // ⚠️ 它同时也把动画相位推后了一格（实测 tick 3009 对照提交而换过的没提交），所以
        // **不许**断言"每一格两边是否提交相同"；能断言的只有上面那个 [0, +1] 的帧数差界。
        if t == at {
            assert!(
                sw_intent.sprite.is_some(),
                "换本体当刻必须提交一帧，否则屏上留的是旧本体的像素"
            );
        }
        swapped.check_invariants().expect("换本体之后不该出现漂移");
    }
    // 末尾读数 == 全程没换：这条把 P5 与 8h 基线那个指纹直接接上（默认路径不调用 swap ⇒ 基线不动）。
    assert_eq!(
        swapped.fingerprint(),
        plain.fingerprint(),
        "{horizon} tick 之后指纹必须与从未换过本体的那一跑逐位相同"
    );
    assert_eq!(
        extra_frames.iter().max(),
        Some(&1),
        "换本体那一格的 +1 必须出现在读数里，否则 swap 没有把新本体提交上屏"
    );
}

/// 格不同时 `half_width` 与 x 裁剪必须**当场**跟着新清单走（不变量立在写入点，不等下一次积分）。
///
/// 这一条存在的理由：等格那条测不到 `half_width` 那行，而那行是"新清单按旧体型"唯一还可能
/// 活下来的地方。用一份 `grid.w` 减半的清单 ⇒ 半宽减半、位置必须仍落在舞台内。
#[test]
fn swap_manifest_moves_half_width_and_clamps_x_immediately() {
    let big = Manifest::parse(petcore::assets::DEFAULT_MANIFEST_JSON).unwrap();
    let mut small = Manifest::parse(petcore::assets::DEFAULT_MANIFEST_JSON).unwrap();
    small.grid.w = big.grid.w / 2;
    small.grid.h = big.grid.h / 2;
    assert_ne!(big.grid, small.grid, "fixture 前提：两份清单必须不同档");

    let mut sim = Sim::new(world(), &big, 13, BehaviorConfig::default());
    // 把宠物拖到贴着左墙，再换清单：若只改 `half_width` 而不裁 x，它会停在"新半宽之外的
    // 老位置"，第一格就会被 `Body::step` 的裁剪救回，屏上看是"抖一下"。
    let stage = sim.world.stage().rect;
    sim.body.pos.x = stage.left() + big.grid.w as f32 / 2.0;
    for _ in 0..30 {
        sim.step();
    }
    let before = sim.body.half_width;
    sim.swap_manifest(&small);
    assert_eq!(
        sim.body.half_width,
        small.grid.w as f32 / 2.0,
        "体型必须跟着新清单的 grid.w 走"
    );
    assert_ne!(sim.body.half_width, before, "半宽没变 = 这行代码没被执行");
    let stage = sim.world.stage().rect;
    assert!(
        sim.body.pos.x >= stage.left() + sim.body.half_width - 0.001,
        "换完当刻 x 就该裁进舞台（实测 {}，舞台左 {}，半宽 {}）",
        sim.body.pos.x,
        stage.left(),
        sim.body.half_width
    );
    for _ in 0..600 {
        sim.step();
        sim.check_invariants()
            .expect("不同档换本体之后不该出现漂移");
    }
}
