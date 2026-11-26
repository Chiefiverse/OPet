//! 音频播放层：按 [`overlay_common::audio`] 的决策结果，通过 `paplay` 子进程播放 `.ogg` 文件。
//!
//! 设备不可用（CI、无 PipeWire/Pulse 的 headless 环境）⇒ `AudioPlayer::new` 返回 `None`，
//! tick loop 静默跳过。`panic = "abort"` profile 下不允许任何 unwrap 走到这里。

use std::path::PathBuf;
use std::process::{Command, Stdio};

use overlay_common::audio::SoundMap;
use petcore::behavior::Transition;

pub struct AudioPlayer {
    map: SoundMap,
    dir: PathBuf,
    volume: u8,
    player: &'static str,
}

impl AudioPlayer {
    /// 加载 `dir` 下的 `sounds.json`，确认系统有 `paplay` 或 `pw-play`。
    pub fn new(dir: PathBuf, volume: u8) -> Option<Self> {
        let map = SoundMap::load(&dir)?;
        if map.is_empty() {
            return None;
        }
        let player = if Self::which("paplay") {
            "paplay"
        } else if Self::which("pw-play") {
            "pw-play"
        } else {
            return None;
        };
        Some(Self {
            map,
            dir,
            volume,
            player,
        })
    }

    fn which(cmd: &str) -> bool {
        Command::new("sh")
            .args(["-c", &format!("command -v {cmd}")])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .is_ok_and(|s| s.success())
    }

    pub fn dir(&self) -> &PathBuf {
        &self.dir
    }

    pub fn set_volume(&mut self, volume: u8) {
        self.volume = volume;
    }

    /// 每 tick 调用一次：拿到 transitions 和前后 sprite，问共用层"该播什么"，逐个 spawn。
    pub fn play_for_tick(
        &self,
        transitions: &[Transition],
        sprite: Option<&petcore::assets::SpriteFrame>,
        prev_sprite: Option<&petcore::assets::SpriteFrame>,
    ) {
        if transitions.is_empty() && sprite.is_none() {
            return;
        }
        if self.volume == 0 {
            return;
        }
        let names =
            overlay_common::audio::sounds_for_tick(&self.map, transitions, sprite, prev_sprite);
        for name in names {
            let path = self.dir.join(name);
            if !path.exists() {
                continue;
            }
            let mut cmd = Command::new(self.player);
            if self.player == "paplay" {
                cmd.arg("--volume");
                cmd.arg((self.volume as u32 * 655).to_string());
            }
            let _ = cmd
                .arg(&path)
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn();
        }
    }
}
