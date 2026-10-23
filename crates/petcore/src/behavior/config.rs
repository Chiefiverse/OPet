//! 行为参数。数值取自 §3.3 迁移表；表里没写的（体力、计时器区间）在这里定 v1 值并写清理由。

use crate::physics::{JUMP_HEIGHT_MAX, JUMP_HEIGHT_MIN};

/// `[behavior] enabled = ["walk","climb"]`（§3.10）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct EnabledActions {
    pub walk: bool,
    pub run: bool,
    pub jump: bool,
    pub climb: bool,
    pub sleep: bool,
}

impl EnabledActions {
    pub const fn all() -> Self {
        Self {
            walk: true,
            run: true,
            jump: true,
            climb: true,
            sleep: true,
        }
    }

    pub const fn none() -> Self {
        Self {
            walk: false,
            run: false,
            jump: false,
            climb: false,
            sleep: false,
        }
    }

    /// 解析 TOML 里的名字列表；未识别的名字被忽略（前向兼容新动作）。
    pub fn from_names(names: &[&str]) -> Self {
        let mut e = Self::none();
        for n in names {
            match *n {
                "walk" => e.walk = true,
                "run" => e.run = true,
                "jump" => e.jump = true,
                "climb" => e.climb = true,
                "sleep" => e.sleep = true,
                _ => {}
            }
        }
        e
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct BehaviorConfig {
    pub enabled: EnabledActions,
    /// 全局速度倍率（§3.10 `speed_scale`）
    pub speed_scale: f32,
    pub walk_speed: f32,
    pub run_speed: f32,
    pub climb_speed: f32,
    /// 超过这个距离就改用奔跑（§3.3：300px）
    pub run_distance: f32,
    /// 跳跃高度区间（§3.3：90–140px）
    pub jump_height_min: f32,
    pub jump_height_max: f32,
    /// Idle 骰子：走的比例（§3.3：0.6），其余按 jump_share 分配
    pub walk_share: f32,
    pub jump_share: f32,
    /// 自主决策的间隔秒数区间（表里只写了"随机计时器"）
    pub action_interval_min: f32,
    pub action_interval_max: f32,
    /// Stop 减速时长：3 帧 @12fps
    pub stop_duration: f32,
    /// Land 压缩时长：2 帧 @12fps
    pub land_duration: f32,
    /// Spawn 入场时长
    pub spawn_duration: f32,
    /// 爬升体力秒数（表里只写了"体力耗尽"）
    pub climb_stamina: f32,
    /// 无输入多久后允许进入 Sleep（§3.3：5 分钟）
    pub sleep_after: f32,
    /// 障碍探测距离（§3.3：24px）
    pub obstacle_lookahead: f32,
    /// 行走中失去焦点是否停下（§3.3 "可配置"）
    pub stop_on_focus_lost: bool,
    /// 到达目标判定半径
    pub arrive_epsilon: f32,
}

impl Default for BehaviorConfig {
    fn default() -> Self {
        Self {
            enabled: EnabledActions::all(),
            speed_scale: 1.0,
            walk_speed: 40.0,
            run_speed: 160.0,
            climb_speed: 30.0,
            run_distance: 300.0,
            jump_height_min: JUMP_HEIGHT_MIN,
            jump_height_max: JUMP_HEIGHT_MAX,
            // 0.6 走 / 0.15 跳 / 0.25 原地不动（§3.3 只约束了 0.6 这一刀）
            walk_share: 0.6,
            jump_share: 0.15,
            action_interval_min: 2.0,
            action_interval_max: 8.0,
            stop_duration: 3.0 / 12.0,
            land_duration: 2.0 / 12.0,
            spawn_duration: 0.5,
            climb_stamina: 8.0,
            sleep_after: 300.0,
            obstacle_lookahead: 24.0,
            stop_on_focus_lost: true,
            arrive_epsilon: 1.0,
        }
    }
}

impl BehaviorConfig {
    pub fn walk_speed(&self) -> f32 {
        self.walk_speed * self.speed_scale
    }

    pub fn run_speed(&self) -> f32 {
        self.run_speed * self.speed_scale
    }

    pub fn climb_speed(&self) -> f32 {
        self.climb_speed * self.speed_scale
    }

    /// 着地时水平方向是否算"落地即停"的门槛。
    pub fn is_enabled(&self, a: Action) -> bool {
        match a {
            Action::Walk => self.enabled.walk,
            Action::Run => self.enabled.run,
            Action::Jump => self.enabled.jump,
            Action::Climb => self.enabled.climb,
            Action::Sleep => self.enabled.sleep,
        }
    }
}

/// 菜单/CLI 可指定的动作（F4 右键菜单"动作"项、§10.2 `opet action set walk`）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Action {
    Walk,
    Run,
    Jump,
    Climb,
    Sleep,
}
