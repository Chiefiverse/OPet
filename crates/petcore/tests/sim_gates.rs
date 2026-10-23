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
