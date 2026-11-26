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

/// **批1 的地基判据**：把像素拆成两张存放（`format = 3` + `sheets`），行表 `states.*.rows`
/// 与 `grid.rows` 逐字不动 ⇒ 内核每 tick 交出来的格子必须**与单张时代完全相同**
/// （含"这格没变所以不提交"的 `None`）。这条等式成立，才允许说"行号空间与像素存放解耦"。
///
/// 反向对照 = 把每档行表整体右移一行 ⇒ 序列必须不同。没有这一句，上面的等式可能
/// 只是"两边都算了同一个错数"（本轮已抓到过两次假绿，见 DESIGN §6.1.27）。
#[test]
fn a_split_sheet_yields_the_same_sprite_stream() {
    use petcore::assets::DEFAULT_MANIFEST_JSON;
    use petcore::sim::Sim;
    use petcore::world::World;
    use std::collections::BTreeSet;

    /// 十只本体的行表都是"一格一行、首尾相接"，40 行拆 20+20 是拆分彩排的最小形状。
    fn split(json: &str, half: usize) -> String {
        json.replace("\"format\": 2", "\"format\": 3").replace(
            "\"sheet\": \"sheet.webp\",",
            &format!(
                "\"sheets\": [ {{ \"file\": \"sheet_0.webp\", \"rows\": {half} }}, \
                 {{ \"file\": \"sheet_1.webp\", \"rows\": {half} }} ],"
            ),
        )
    }

    fn stream(m: &Manifest, ticks: u64) -> Vec<Option<petcore::assets::SpriteFrame>> {
        let mut sim = Sim::new(
            World::single("m", 1920.0, 1080.0, 1.0),
            m,
            24,
            petcore::behavior::BehaviorConfig::default(),
        );
        (0..ticks).map(|_| sim.step().1.sprite).collect()
    }

    let ticks = 6_000;
    let single = Manifest::parse(DEFAULT_MANIFEST_JSON).unwrap();
    let split = Manifest::parse(&split(DEFAULT_MANIFEST_JSON, 20)).unwrap();

    // 拆分确实改了归属，没改行号
    assert_eq!(single.locate(25), Some((0, 25)));
    assert_eq!(split.locate(25), Some((1, 5)));
    for key in AssetKey::ALL {
        assert_eq!(
            single.track(key).map(|t| t.rows.clone()),
            split.track(key).map(|t| t.rows.clone()),
            "{key:?} 的行表被拆分改动了"
        );
    }

    let base = stream(&single, ticks);
    let got = stream(&split, ticks);
    assert_eq!(
        base, got,
        "分段存放改变了内核交出来的格子 ⇒ 行号空间没有被真正解耦"
    );

    // 覆盖面：样本必须真的走过第二段的行，否则上面那条等式是空的
    let rows: BTreeSet<usize> = base.iter().flatten().map(|f| f.row).collect();
    assert!(
        rows.iter().any(|&r| r < 20) && rows.iter().any(|&r| r >= 20),
        "{ticks} tick 只走到这些行 {rows:?} ⇒ 样本没跨进第二段，这条对照不作数"
    );
    assert!(
        base.iter().flatten().count() > 100 && base.iter().any(|f| f.is_none()),
        "样本里既要有提交的格子，也要有『没变所以不提交』的 tick，否则等式退化成比空表"
    );

    // 反向对照：行表整体右移一行（仍然合法，因为平铺没动）
    let mut shifted = split.clone();
    for track in shifted.states.values_mut() {
        for row in &mut track.rows {
            *row = (*row + 1) % shifted.grid.rows;
        }
    }
    shifted.validate().expect("右移后的行表本身仍应合法");
    let moved = stream(&shifted, ticks);
    assert_ne!(
        base, moved,
        "行表动了，格子序列却一模一样 ⇒ 这条等式测的是别的东西，不是寻址"
    );
}

/// 批2：舞步叠加 = 一次性，走 `one_shot` 通道，放完交还基底。
fn dance_manifest(rows: usize) -> Manifest {
    let json = format!(
        r#"{{
  "format": 2,
  "slug": "test_dance",
  "sheet": "sheet.webp",
  "grid": {{ "cols": 8, "rows": {rows}, "w": 192, "h": 192 }},
  "states": {{
    "idle":  {{ "rows": [0,1,2,3], "fps": 8, "loop": true }},
    "walk":  {{ "rows": [4,5,6,7,8,9,10,11], "fps": 12, "loop": true }},
    "run":   {{ "rows": [12,13,14,15,16,17,18,19], "fps": 16, "loop": true }},
    "stop":  {{ "rows": [20,21,22], "fps": 12, "loop": false }},
    "jump":  {{ "rows": [23,24,25,26], "fps": 14, "loop": false }},
    "climb": {{ "rows": [27,28,29,30], "fps": 10, "loop": true }},
    "sleep": {{ "rows": [31,32], "fps": 4, "loop": true }},
    "turn":  {{ "rows": [33,34,35,36], "fps": 12, "loop": false }},
    "react": {{ "rows": [37,38,39], "fps": 12, "loop": false }},
    "dance": {{ "rows": [40,41], "fps": 8, "loop": false }}
  }},
  "anchor": {{ "feet": [96, 180] }},
  "facing": "sprite_rows_are_right_facing",
  "input_polygon": "auto_from_alpha"
}}"#
    );
    Manifest::parse(&json).expect("dance manifest must be valid")
}

#[test]
fn dance_plays_over_the_base_and_hands_back() {
    let m = dance_manifest(42);
    assert!(m.has(AssetKey::Dance));
    assert_eq!(m.track(AssetKey::Dance).unwrap().rows, vec![40, 41]);
    assert!(!m.loops(AssetKey::Dance));

    let mut s = Scheduler::new(&m, Facing::East);
    s.set_fps_caps(30.0, 5.0);
    let step = 0.1;
    s.advance(step, State::Idle, Facing::East, None);
    let d0 = s
        .advance(step, State::Idle, Facing::East, Some(AssetKey::Dance))
        .expect("dance frame 0");
    assert_eq!((d0.key, d0.frame, d0.row), (AssetKey::Dance, 0, 40));

    let d1 = s
        .advance(step, State::Idle, Facing::East, None)
        .expect("dance frame 1");
    assert_eq!((d1.key, d1.frame, d1.row), (AssetKey::Dance, 1, 41));

    let back = s
        .advance(step, State::Idle, Facing::East, None)
        .expect("after dance: idle");
    assert_eq!(back.key, AssetKey::Idle, "dance 放完必须交还基底");
}

#[test]
fn a_dance_spanning_two_sheets_streams_correct_cells() {
    let split_json = r#"{
  "format": 3,
  "slug": "test_dance",
  "sheets": [
    { "file": "sheet_0.webp", "rows": 20 },
    { "file": "sheet_1.webp", "rows": 22 }
  ],
  "grid": { "cols": 8, "rows": 42, "w": 192, "h": 192 },
  "states": {
    "idle":  { "rows": [0,1,2,3], "fps": 8, "loop": true },
    "walk":  { "rows": [4,5,6,7,8,9,10,11], "fps": 12, "loop": true },
    "run":   { "rows": [12,13,14,15,16,17,18,19], "fps": 16, "loop": true },
    "stop":  { "rows": [20,21,22], "fps": 12, "loop": false },
    "jump":  { "rows": [23,24,25,26], "fps": 14, "loop": false },
    "climb": { "rows": [27,28,29,30], "fps": 10, "loop": true },
    "sleep": { "rows": [31,32], "fps": 4, "loop": true },
    "turn":  { "rows": [33,34,35,36], "fps": 12, "loop": false },
    "react": { "rows": [37,38,39], "fps": 12, "loop": false },
    "dance": { "rows": [40,41], "fps": 8, "loop": false }
  },
  "anchor": { "feet": [96, 180] },
  "facing": "sprite_rows_are_right_facing",
  "input_polygon": "auto_from_alpha"
}"#
    .to_string();
    let split = Manifest::parse(&split_json).unwrap();

    assert_eq!(split.locate(40), Some((1, 20)));
    assert_eq!(split.locate(41), Some((1, 21)));

    let mut s = Scheduler::new(&split, Facing::East);
    s.set_fps_caps(30.0, 5.0);
    let step = 0.1;
    s.advance(step, State::Idle, Facing::East, None);
    let d0 = s
        .advance(step, State::Idle, Facing::East, Some(AssetKey::Dance))
        .expect("dance frame 0");
    assert_eq!((d0.key, d0.frame, d0.row), (AssetKey::Dance, 0, 40));
    let d1 = s
        .advance(step, State::Idle, Facing::East, None)
        .expect("dance frame 1");
    assert_eq!((d1.key, d1.frame, d1.row), (AssetKey::Dance, 1, 41));
}

#[test]
fn a_pack_without_dance_never_touches_the_overlay() {
    let m = manifest();
    assert!(!m.has(AssetKey::Dance));
    let mut s = Scheduler::new(m, Facing::East);
    s.set_fps_caps(30.0, 5.0);
    for _ in 0..200 {
        if let Some(f) = s.advance(DT, State::Idle, Facing::East, Some(AssetKey::Dance)) {
            assert_ne!(f.key, AssetKey::Dance, "无 dance 行的包不该产出 Dance 帧");
        }
    }
}
