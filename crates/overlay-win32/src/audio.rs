//! 音频播放层（Windows）：接 `rodio`、持有设备输出流、按 [`overlay_common::audio`] 的决策结果播放文件。
//!
//! 设备不可用 ⇒ `AudioPlayer::new` 返回 `None`，tick loop 静默跳过。

use std::io::BufReader;
use std::path::PathBuf;

use overlay_common::audio::SoundMap;
use petcore::behavior::Transition;
use rodio::{Decoder, DeviceSinkBuilder, MixerDeviceSink, Source};

pub struct AudioPlayer {
    _device: MixerDeviceSink,
    map: SoundMap,
    dir: PathBuf,
    volume: f32,
}

impl AudioPlayer {
    pub fn new(dir: PathBuf, volume: u8) -> Option<Self> {
        let map = SoundMap::load(&dir)?;
        if map.is_empty() {
            return None;
        }
        let device = DeviceSinkBuilder::open_default_sink().ok()?;
        Some(Self {
            _device: device,
            map,
            dir,
            volume: volume as f32 / 100.0,
        })
    }

    pub fn set_volume(&mut self, volume: u8) {
        self.volume = volume as f32 / 100.0;
    }

    pub fn play_for_tick(
        &self,
        transitions: &[Transition],
        sprite: Option<&petcore::assets::SpriteFrame>,
        prev_sprite: Option<&petcore::assets::SpriteFrame>,
    ) {
        if transitions.is_empty() && sprite.is_none() {
            return;
        }
        if self.volume == 0.0 {
            return;
        }
        let names =
            overlay_common::audio::sounds_for_tick(&self.map, transitions, sprite, prev_sprite);
        for name in names {
            let path = self.dir.join(name);
            let Ok(file) = std::fs::File::open(&path) else {
                continue;
            };
            let Ok(source) = Decoder::try_from(BufReader::new(file)) else {
                continue;
            };
            self._device
                .mixer()
                .add(source.amplify_normalized(self.volume));
        }
    }
}
