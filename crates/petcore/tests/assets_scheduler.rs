//! 帧调度器契约（M2 的 bake.py 必须产出满足这些断言的图集布局）。

use petcore::assets::{Manifest, Scheduler};
use petcore::behavior::{AssetKey, State};
use petcore::geom::Facing;
use petcore::sim::DT;

fn manifest() -> &'static Manifest {
    use std::sync::OnceLock;
    static M: OnceLock<Manifest> = OnceLock::new();
    M.get_or_init(petcore::assets::default_manifest)
}

const TICKS_PER_SEC: u32 = (1.0 / DT) as u32;

/// 统计 `seconds` 秒内真正"变了"的帧数 = 渲染层需要 commit 的次数。
fn committed(s: &mut Scheduler<'_>, state: State, facing: Facing, seconds: u32) -> usize {
    (0..TICKS_PER_SEC * seconds)
        .filter(|_| s.advance(DT, state, facing, None).is_some())
        .count()
}

#[test]
fn first_tick_emits_frame_zero_of_the_state() {
    let mut s = Scheduler::new(manifest(), Facing::East);
    let f = s
        .advance(DT, State::Idle, Facing::East, None)
        .expect("首帧必须有");
    assert_eq!((f.key, f.frame, f.row, f.col), (AssetKey::Idle, 0, 0, 0));
    assert_eq!(f.frame_count, 4);
}

#[test]
fn facing_selects_the_atlas_column() {
    for facing in Facing::ALL {
        let mut s = Scheduler::new(manifest(), facing);
        let f = s
            .advance(DT, State::Walk, facing, None)
            .expect("walk 首帧必须有");
        assert_eq!(
            f.col,
            facing.index() as usize,
            "{facing:?} 应落在第 {} 列",
            facing.index()
        );
        assert_eq!(f.row, 4, "walk 行组从第 4 行开始");
    }
}

#[test]
fn idle_is_capped_to_fps_idle() {
    // 待机轨道本身 8fps，但 [render] fps_idle=5 封顶 ⇒ 10 秒约 50 帧
    let mut s = Scheduler::new(manifest(), Facing::East);
    s.set_fps_caps(30.0, 5.0);
    let n = committed(&mut s, State::Idle, Facing::East, 10);
    assert!((49..=52).contains(&n), "Idle 10 秒提交 {n} 帧，应约 50 帧");
}

#[test]
fn walk_uses_track_fps_when_under_the_active_cap() {
    // walk 轨道 12fps < fps_active=30 ⇒ 10 秒约 120 帧
    let mut s = Scheduler::new(manifest(), Facing::East);
    s.set_fps_caps(30.0, 5.0);
    let n = committed(&mut s, State::Walk, Facing::East, 10);
    assert!(
        (119..=122).contains(&n),
        "Walk 10 秒提交 {n} 帧，应约 120 帧"
    );
}

#[test]
fn active_cap_clamps_fast_tracks() {
    // run 轨道 16fps，把 fps_active 压到 8 ⇒ 10 秒约 80 帧
    let mut s = Scheduler::new(manifest(), Facing::East);
    s.set_fps_caps(8.0, 5.0);
    let n = committed(&mut s, State::Run, Facing::East, 10);
    assert!(
        (79..=82).contains(&n),
        "Run 10 秒提交 {n} 帧，应约 80 帧（cap=8）"
    );
}

#[test]
fn looping_tracks_wrap_around_their_row_group() {
    let mut s = Scheduler::new(manifest(), Facing::East);
    s.set_fps_caps(30.0, 5.0);
    let seen: Vec<usize> = (0..TICKS_PER_SEC * 4)
        .filter_map(|_| s.advance(DT, State::Walk, Facing::East, None))
        .map(|f| f.frame)
        .collect();
    assert!(seen.iter().all(|f| *f < 8), "帧号越界: {seen:?}");
    assert!(
        seen.windows(2).any(|w| w == [7, 0]),
        "循环动画应能从最后一帧回到第 0 帧: {seen:?}"
    );
}

#[test]
fn one_shot_plays_every_frame_then_returns_to_idle() {
    let mut s = Scheduler::new(manifest(), Facing::East);
    s.set_fps_caps(30.0, 5.0);
    s.advance(DT, State::Idle, Facing::East, None);
    let react = s
        .advance(DT, State::Idle, Facing::East, Some(AssetKey::React))
        .expect("react 首帧");
    assert_eq!((react.key, react.frame), (AssetKey::React, 0));

    let mut played = vec![react.frame];
    let mut back_to_idle = false;
    for _ in 0..60 {
        if let Some(f) = s.advance(0.05, State::Idle, Facing::East, None) {
            match f.key {
                AssetKey::React => played.push(f.frame),
                AssetKey::Idle => {
                    back_to_idle = true;
                    break;
                }
                k => panic!("不该出现 {k:?}"),
            }
        }
    }
    assert!(
        played.iter().all(|f| *f <= 2),
        "一次性动画只有 3 帧，越界: {played:?}"
    );
    assert!(played.contains(&2), "最后一帧被跳过: {played:?}");
    assert!(back_to_idle, "放完应回到 idle");
}

#[test]
fn facing_change_plays_the_turn_row_group_when_present() {
    let mut s = Scheduler::new(manifest(), Facing::East);
    s.set_fps_caps(30.0, 5.0);
    s.advance(DT, State::Idle, Facing::East, None);
    let turn = s
        .advance(DT, State::Idle, Facing::South, None)
        .expect("转身帧");
    assert_eq!(turn.key, AssetKey::Turn);
    assert_eq!(
        turn.col,
        Facing::South.index() as usize,
        "转身也用新朝向的列"
    );
}

#[test]
fn sleep_commits_exactly_one_frame_then_freezes() {
    let mut s = Scheduler::new(manifest(), Facing::East);
    s.set_fps_caps(30.0, 5.0);
    assert!(
        s.advance(DT, State::Sleep, Facing::East, None).is_some(),
        "刚入睡要提交一次睡姿帧"
    );
    for _ in 0..TICKS_PER_SEC * 600 {
        assert!(
            s.advance(DT, State::Sleep, Facing::East, None).is_none(),
            "硬约束 5：Sleep 期间不得再产出新帧"
        );
    }
}
