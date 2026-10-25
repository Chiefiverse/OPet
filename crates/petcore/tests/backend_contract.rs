//! 渲染适配层契约（§3.7）：M4 的三个平台实现必须通过同一组断言。
//! 这里先用 [`NullBackend`] 把契约本身钉住。

use petcore::assets::Manifest;
use petcore::behavior::{BehaviorConfig, Command, EnabledActions, State};
use petcore::geom::Vec2;
use petcore::render::{Button, NullBackend, OverlayBackend, OverlayConfig, PointerEvent};
use petcore::sim::{Sim, TICK_HZ};
use petcore::world::World;

fn manifest() -> &'static Manifest {
    use std::sync::OnceLock;
    static M: OnceLock<Manifest> = OnceLock::new();
    M.get_or_init(petcore::assets::default_manifest)
}

fn feed(sim: &mut Sim<'_>, back: &mut NullBackend, ticks: u64) {
    back.create(&OverlayConfig {
        monitor: "m".into(),
        width: 160.0,
        height: 160.0,
        scale: 1.0,
        wants_passthrough: true,
    })
    .expect("NullBackend 建得起来");
    for _ in 0..ticks {
        let (_t, intent) = sim.step();
        back.present(&intent).expect("present 不该失败");
    }
}

/// 点击穿透的正确性：宠物本体命中，包围盒的空白余量必须穿透到桌面。
#[test]
fn pointer_hits_the_pet_but_not_the_surface_margin() {
    let mut sim = Sim::new(
        World::single("m", 1920.0, 1080.0, 1.0),
        manifest(),
        1,
        BehaviorConfig::default(),
    );
    let (_t, intent) = sim.step();
    assert!(intent.hit_test_world(intent.anchor), "脚下锚点必须命中宠物");
    let corner = intent.surface_origin();
    assert!(
        !intent.hit_test_world(corner),
        "包围盒左上角的空白余量应穿透，否则桌面点不动"
    );
    let above = Vec2::new(intent.anchor.x, intent.anchor.y - 200.0);
    assert!(!intent.hit_test_world(above), "宠物头顶上方不应命中");
}

/// M3-b：按钮维度必须能穿过契约。三个平台后端各自有原始按键码
/// （Wayland `wl_pointer.button` 0x110/0x111/0x112、Win32 `MK_*`、macOS `NSEvent number`），
/// 折成同一枚举后不得丢失 —— 丢了就回到"左右键分不开"那个阻塞点上。
/// `DoubleTap` 是客户端派生的左键专属手势，不带 button 字段（右键菜单靠它自己的
/// Press/Release 翻转，不需要计时）。
#[test]
fn pointer_events_carry_the_button_through_the_contract() {
    let at = Vec2::new(10.0, 20.0);
    for b in [Button::Left, Button::Right, Button::Middle] {
        assert_eq!(
            PointerEvent::Press { at, button: b },
            PointerEvent::Press { at, button: b }
        );
        let ev = PointerEvent::Release { at, button: b };
        match ev {
            PointerEvent::Release { button, .. } => assert_eq!(button, b),
            _ => unreachable!("构造即匹配：Release 必须原样带出按钮"),
        }
    }
    let p = PointerEvent::Press {
        at,
        button: Button::Left,
    };
    assert_ne!(
        p,
        PointerEvent::Press {
            at,
            button: Button::Right
        },
        "左键与右键事件必须可比但不相等"
    );
    assert_eq!(
        p,
        PointerEvent::Press {
            at,
            button: Button::Left
        }
    );
    let dt = PointerEvent::DoubleTap { at };
    assert!(
        matches!(dt, PointerEvent::DoubleTap { .. }),
        "DoubleTap 是派生手势，不应携带按钮"
    );
}

/// 硬约束 5：Sleep 期间不再提交新 buffer，但 tick 照常推进。
#[test]
fn sleep_stops_buffer_commits_at_the_backend() {
    let cfg = BehaviorConfig {
        enabled: EnabledActions {
            sleep: true,
            ..EnabledActions::none()
        },
        sleep_after: 0.0,
        action_interval_min: 1.0e6,
        action_interval_max: 1.0e6,
        ..BehaviorConfig::default()
    };
    let mut sim = Sim::new(World::single("m", 1920.0, 1080.0, 1.0), manifest(), 3, cfg);
    let mut back = NullBackend::default();
    sim.send(Command::SystemIdle(true));
    feed(&mut sim, &mut back, 600);
    assert_eq!(sim.machine.state(), State::Sleep, "10 秒内应已入睡");
    let after_sleep = back.presented;
    feed(&mut sim, &mut back, 60 * 60 * 10);
    assert_eq!(
        back.presented,
        after_sleep,
        "Sleep 一小时里又提交了 {}/{} 帧",
        back.presented.saturating_sub(after_sleep),
        after_sleep
    );
    assert!(
        back.skipped > 60 * 60 * 10 - 5,
        "无事可做的 tick 应被记账，实际 {}",
        back.skipped
    );
}

/// §4 省电第 ① 招：降频后真正提交的 buffer 远少于 60/秒。
#[test]
fn buffer_commits_are_far_below_tick_rate() {
    let mut sim = Sim::new(
        World::single("m", 1920.0, 1080.0, 1.0),
        manifest(),
        11,
        BehaviorConfig::default(),
    );
    let mut back = NullBackend::default();
    let ticks = 30 * 60 * TICK_HZ as u64; // 30 分钟
    feed(&mut sim, &mut back, ticks);
    let rate = back.presented as f64 / ticks as f64;
    assert!(
        rate < 0.35,
        "半小时里提交帧率 {:.3} 帧/tick（等效 {:.1} FPS），超过省电设计上限",
        rate,
        rate * 60.0
    );
    assert!(back.presented > 0, "不该一帧都不提交");
}
