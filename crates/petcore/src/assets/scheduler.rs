//! 帧调度：把 (状态, 朝向, 经过时间) 变成图集里的一格，并按 §3.10 的 `[render]` 上限降频。
//!
//! 这里不做任何像素解码（WebP 解码属于 M2），只负责"该显示哪一格"和"这格跟上一格有没有区别"，
//! 后者决定 `needs_redraw` ⇒ Sleep/静止时不提交新 buffer（§4 省电第 ② 招）。

use crate::assets::manifest::Manifest;
use crate::behavior::{AssetKey, State};
use crate::geom::Facing;

/// 一帧的定位：图集单元格 (row, col) 与其语义。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SpriteFrame {
    pub key: AssetKey,
    /// 该状态内的第几帧（0..frame_count）
    pub frame: usize,
    pub frame_count: usize,
    /// 图集物理行列
    pub row: usize,
    pub col: usize,
}

#[derive(Clone, Copy, Debug)]
struct Clock {
    key: AssetKey,
    elapsed: f32,
}

#[derive(Clone, Debug)]
pub struct Scheduler<'m> {
    manifest: &'m Manifest,
    fps_active: f32,
    fps_idle: f32,
    base: Clock,
    /// 一次性叠加动画（react / turn），放完自动回到 base
    overlay: Option<Clock>,
    facing: Facing,
    current: Option<SpriteFrame>,
}

impl<'m> Scheduler<'m> {
    pub fn new(manifest: &'m Manifest, facing: Facing) -> Self {
        Self {
            manifest,
            fps_active: 30.0,
            fps_idle: 5.0,
            base: Clock {
                key: State::Spawn.asset_key(),
                elapsed: 0.0,
            },
            overlay: None,
            facing,
            current: None,
        }
    }

    /// `[render] fps_active / fps_idle`（§3.10）。
    pub fn set_fps_caps(&mut self, fps_active: f32, fps_idle: f32) {
        self.fps_active = fps_active.max(1.0);
        self.fps_idle = fps_idle.max(1.0);
    }

    pub fn current(&self) -> Option<SpriteFrame> {
        self.current
    }

    fn cap_for(&self, state: State) -> f32 {
        match state {
            State::Idle | State::WallSit | State::Stop | State::Land | State::Sleep => {
                self.fps_idle
            }
            _ => self.fps_active,
        }
    }

    /// 叠层一次性动画（转身、摸头反应）按 `fps_active` 放：它按 `fps_idle` 封顶会慢到
    /// 看不清是什么动作，而"待机时被抓一下"正是最常见的交互。
    fn rate_cap(&self, state: State) -> f32 {
        if self.overlay.is_some() {
            self.fps_active
        } else {
            self.cap_for(state)
        }
    }

    /// 推进一 tick，返回当前应显示的帧。`one_shot` 是状态机本轮请求的一次性动画。
    ///
    /// 返回 `None` 表示"和上一帧完全一样"⇒ 渲染层可以跳过 commit。
    pub fn advance(
        &mut self,
        dt: f32,
        state: State,
        facing: Facing,
        one_shot: Option<AssetKey>,
    ) -> Option<SpriteFrame> {
        let base_key = state.asset_key();
        if self.base.key != base_key {
            self.base = Clock {
                key: base_key,
                elapsed: 0.0,
            };
        }

        // 朝向变化 ⇒ 若图集有 turn 行组，先用一次性的转身盖住切列的硬跳
        if facing != self.facing {
            self.facing = facing;
            if let Some(turn) = self.pick_turn() {
                self.overlay = Some(Clock {
                    key: turn,
                    elapsed: 0.0,
                });
            }
        }

        if let Some(key) = one_shot {
            if self.manifest.has(key) {
                self.overlay = Some(Clock { key, elapsed: 0.0 });
            }
        }

        // Sleep 停帧：时间不推进，帧不变
        if !state.animates() {
            return self.resolve_and_detect_change(state);
        }

        let cap = self.rate_cap(state);
        match self.overlay.as_mut() {
            Some(clock) => {
                let frames = self.manifest.frame_count(clock.key).max(1);
                clock.elapsed += dt;
                let fps = self.manifest.fps(clock.key).min(cap).max(f32::EPSILON);
                // 索引越过最后一帧才算放完，否则最后一帧永远看不到
                let done = (clock.elapsed * fps) as usize >= frames;
                if done && !self.manifest.loops(clock.key) {
                    self.overlay = None;
                    self.base.elapsed = 0.0;
                }
            }
            None => {
                self.base.elapsed += dt;
            }
        }

        self.resolve_and_detect_change(state)
    }

    fn pick_turn(&self) -> Option<AssetKey> {
        if self.manifest.has(AssetKey::Turn) {
            Some(AssetKey::Turn)
        } else {
            None
        }
    }

    fn resolve_and_detect_change(&mut self, state: State) -> Option<SpriteFrame> {
        let clock = self.overlay.unwrap_or(self.base);
        let key = clock.key;
        let frames = self.manifest.frame_count(key).max(1);
        let fps = self
            .manifest
            .fps(key)
            .min(self.rate_cap(state))
            .max(f32::EPSILON);
        let raw = (clock.elapsed * fps) as usize;
        let frame = if self.manifest.loops(key) {
            raw % frames
        } else {
            raw.min(frames - 1)
        };
        let (row, col) = self.manifest.cell(key, frame, self.facing);
        let next = SpriteFrame {
            key,
            frame,
            frame_count: frames,
            row,
            col,
        };
        if self.current == Some(next) {
            None
        } else {
            self.current = Some(next);
            Some(next)
        }
    }
}
