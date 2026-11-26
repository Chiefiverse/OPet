//! 音效映射与触发决策（PAID-EDITION §3.5-d/b）。
//!
//! 本模块**不链 `rodio`**——它是纯数据 + 纯函数：给定一份 `sounds.json`、本轮 `Vec<Transition>`
//! 和前后两格 `SpriteFrame`，返回"该播哪些裸名文件"。各平台客户端拿到文件名列表后自行解码/播放。
//!
//! 落点纪律 = `overlay-common` 的立身之本：三平台共用、零平台绑定（D12/D13/D8）。
//! 实际播放 = `overlay-wayland/src/audio.rs` 和 `overlay-win32/src/app.rs` 里 `rodio` 那一层。

use std::collections::HashMap;
use std::path::Path;

use petcore::assets::SpriteFrame;
use petcore::behavior::{State, Transition};
use serde::Deserialize;

/// 一份 `sounds.json` 的解析结果。
#[derive(Clone, Debug, Default, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SoundMap {
    /// 键 = `State` 变体名（"Jump"/"Land"/"Sleep"…），值 = 裸文件名（"sfx_jump.ogg"）。
    /// 进入该状态时触发一次。
    #[serde(default)]
    pub transitions: HashMap<String, String>,
    /// 键 = `AssetKey::as_str()`（"dance"/"react"…），值 = 按帧号排的拍点列表。
    /// 叠层动画从 frame N-1 前进到 frame N 时，该帧上所有拍点各触发一次。
    #[serde(default)]
    pub beats: HashMap<String, Vec<BeatEntry>>,
}

#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BeatEntry {
    /// 目标帧号（0-based，对应 SpriteFrame.frame）
    pub frame: usize,
    /// 裸文件名
    pub sound: String,
}

impl SoundMap {
    /// 从资产目录读 `sounds.json`。文件不存在 = `None`（静默无音，不是错误）。
    /// 读到了但解析失败 = `None` + 调用方应打一行 stderr（与 polygon.json 同族处理）。
    pub fn load(dir: &Path) -> Option<Self> {
        let path = dir.join("sounds.json");
        let text = std::fs::read_to_string(&path).ok()?;
        match serde_json::from_str::<Self>(&text) {
            Ok(map) => {
                if let Err(bad) = validate_bare_names(&map) {
                    eprintln!("audio: sounds.json 引用了非法文件名 {bad:?} ⇒ 整份忽略");
                    return None;
                }
                Some(map)
            }
            Err(e) => {
                eprintln!("audio: 读 sounds.json 失败（{e}）⇒ 无音效");
                None
            }
        }
    }

    pub fn is_empty(&self) -> bool {
        self.transitions.is_empty() && self.beats.is_empty()
    }
}

/// 所有引用的文件名必须是裸名（无 `/`、无 `..`、不以 `.` 开头），与 `Manifest::validate`
/// 对 `sheets[].file` 的 `is_bare_name` 闸同一条规则。
fn validate_bare_names(map: &SoundMap) -> Result<(), Vec<String>> {
    let mut bad = Vec::new();
    for v in map.transitions.values() {
        if !is_bare_name(v) {
            bad.push(v.clone());
        }
    }
    for entries in map.beats.values() {
        for e in entries {
            if !is_bare_name(&e.sound) {
                bad.push(e.sound.clone());
            }
        }
    }
    if bad.is_empty() { Ok(()) } else { Err(bad) }
}

fn is_bare_name(s: &str) -> bool {
    !s.is_empty() && !s.contains('/') && !s.contains('\\') && !s.starts_with('.') && s != ".."
}

/// 给定本轮的 transitions、前后 sprite 格，返回该播的裸文件名列表（去重前）。
///
/// 调用方（各平台客户端的 tick loop）拿到非空结果后逐个 `dir.join(name)` → 解码 → 入混音器。
/// 文件不存在（打包时遗漏）在客户端播放层静默跳过，不 panic。
pub fn sounds_for_tick<'a>(
    map: &'a SoundMap,
    transitions: &[Transition],
    sprite: Option<&SpriteFrame>,
    prev_sprite: Option<&SpriteFrame>,
) -> Vec<&'a str> {
    let mut out = Vec::new();

    // ① 状态进入触发
    for t in transitions {
        let state_name = state_to_key(&t.to);
        if let Some(name) = map.transitions.get(&state_name) {
            out.push(name.as_str());
        }
    }

    // ② 拍点触发：当前格 frame 递增了，且 key 在 beats 表里
    if let (Some(cur), Some(prev)) = (sprite, prev_sprite) {
        if cur.key == prev.key && cur.frame > prev.frame {
            let key_name = cur.key.as_str();
            if let Some(entries) = map.beats.get(key_name) {
                for e in entries {
                    // frame 从 prev 递增到 cur，落在 (prev.frame, cur.frame] 区间内的拍点都触发
                    if e.frame > prev.frame && e.frame <= cur.frame {
                        out.push(e.sound.as_str());
                    }
                }
            }
        }
    }

    out
}

/// `State` → `sounds.json` 里 transitions 段的键名。用 Debug 格式的前缀（变体名 = PascalCase），
/// 与 `State::ALL` 逐一对应。
fn state_to_key(s: &State) -> String {
    format!("{s:?}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_sound_map_roundtrips_and_names_keys() {
        let json = r#"{
            "transitions": {"Jump": "sfx_jump.ogg", "Land": "sfx_land.ogg"},
            "beats": {"dance": [{"frame": 0, "sound": "beat1.ogg"}, {"frame": 1, "sound": "beat2.ogg"}]}
        }"#;
        let map: SoundMap = serde_json::from_str(json).unwrap();
        assert_eq!(map.transitions.len(), 2);
        assert_eq!(map.transitions["Jump"], "sfx_jump.ogg");
        assert_eq!(map.beats["dance"].len(), 2);
        assert_eq!(map.beats["dance"][1].frame, 1);
        assert!(!map.is_empty());
    }

    #[test]
    fn a_missing_sounds_json_is_absence_not_a_crash() {
        let dir = Path::new("/nonexistent-path-for-testing-xyz");
        assert!(SoundMap::load(dir).is_none());
    }

    #[test]
    fn transitions_map_to_named_sounds() {
        let mut map = SoundMap::default();
        map.transitions.insert("Jump".into(), "sfx_jump.ogg".into());
        map.transitions
            .insert("Sleep".into(), "sfx_yawn.ogg".into());

        let transitions = vec![Transition {
            from: State::Idle,
            to: State::Jump,
            trigger: petcore::behavior::Trigger::Command,
            tick: 100,
        }];
        let got = sounds_for_tick(&map, &transitions, None, None);
        assert_eq!(got, vec!["sfx_jump.ogg"]);
    }

    #[test]
    fn beats_fire_only_on_the_frame_they_name() {
        let mut map = SoundMap::default();
        map.beats.insert(
            "dance".into(),
            vec![
                BeatEntry {
                    frame: 1,
                    sound: "beat_a.ogg".into(),
                },
                BeatEntry {
                    frame: 3,
                    sound: "beat_b.ogg".into(),
                },
            ],
        );

        use petcore::behavior::AssetKey;
        let prev = SpriteFrame {
            key: AssetKey::Dance,
            frame: 0,
            frame_count: 4,
            row: 40,
            col: 0,
        };
        // 前进到 frame 1 → 触发 beat_a
        let cur1 = SpriteFrame { frame: 1, ..prev };
        let got = sounds_for_tick(&map, &[], Some(&cur1), Some(&prev));
        assert_eq!(got, vec!["beat_a.ogg"]);

        // 前进到 frame 2 → 不触发（没 beat 在 2）
        let cur2 = SpriteFrame { frame: 2, ..prev };
        let got = sounds_for_tick(&map, &[], Some(&cur2), Some(&cur1));
        assert!(got.is_empty());

        // 跳帧 2→3 → 触发 beat_b
        let cur3 = SpriteFrame { frame: 3, ..prev };
        let got = sounds_for_tick(&map, &[], Some(&cur3), Some(&cur2));
        assert_eq!(got, vec!["beat_b.ogg"]);
    }

    #[test]
    fn an_animation_without_beats_emits_no_sounds() {
        let mut map = SoundMap::default();
        map.beats.insert(
            "dance".into(),
            vec![BeatEntry {
                frame: 1,
                sound: "beat.ogg".into(),
            }],
        );
        use petcore::behavior::AssetKey;
        let prev = SpriteFrame {
            key: AssetKey::Walk,
            frame: 0,
            frame_count: 4,
            row: 10,
            col: 0,
        };
        let cur = SpriteFrame {
            key: AssetKey::Walk,
            frame: 1,
            ..prev
        };
        let got = sounds_for_tick(&map, &[], Some(&cur), Some(&prev));
        assert!(got.is_empty(), "Walk 不在 beats 表里，不该出声：{got:?}");
    }
}
