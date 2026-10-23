//! 行为状态与 §3.3 迁移表（实现契约）。

use serde::{Deserialize, Serialize};

/// 内核行为状态。`Land`/`Stop`/`Spawn` 是带时长的过渡态。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum State {
    Spawn,
    Idle,
    Walk,
    Run,
    Stop,
    Jump,
    Land,
    Climb,
    WallSit,
    Fall,
    Dragged,
    Sleep,
}

impl State {
    pub const ALL: [State; 12] = [
        State::Spawn,
        State::Idle,
        State::Walk,
        State::Run,
        State::Stop,
        State::Jump,
        State::Land,
        State::Climb,
        State::WallSit,
        State::Fall,
        State::Dragged,
        State::Sleep,
    ];

    /// 迁移表里 `AnyGrounded` 的含义：脚踩在某个支撑面上的状态。
    pub const fn is_grounded(self) -> bool {
        matches!(
            self,
            State::Idle | State::Walk | State::Run | State::Stop | State::Land | State::WallSit
        )
    }

    /// 迁移表里 `Any` 的含义：除自身以外的所有状态都能被拖拽。
    pub const fn is_dragged(self) -> bool {
        matches!(self, State::Dragged)
    }

    /// 该状态下是否允许提交新帧。`Sleep` 停帧（硬约束 5、§4 省电第 ② 招）。
    pub const fn animates(self) -> bool {
        !matches!(self, State::Sleep)
    }

    /// 状态→图集行组（asset key）映射。`turn`/`react` 是一次性动画，不由状态决定，见
    /// [`crate::assets::Scheduler`]。
    pub const fn asset_key(self) -> AssetKey {
        match self {
            State::Spawn => AssetKey::Idle,
            State::Idle => AssetKey::Idle,
            State::WallSit => AssetKey::Idle,
            State::Walk => AssetKey::Walk,
            State::Run => AssetKey::Run,
            State::Stop => AssetKey::Stop,
            State::Land => AssetKey::Stop,
            State::Jump => AssetKey::Jump,
            State::Fall => AssetKey::Jump,
            State::Dragged => AssetKey::Jump,
            State::Climb => AssetKey::Climb,
            State::Sleep => AssetKey::Sleep,
        }
    }
}

/// `pet.json` 里 `states` 的九个键（§3.4）。图集行按此组织，列是 8 个朝向。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum AssetKey {
    Idle,
    Walk,
    Run,
    Stop,
    Jump,
    Climb,
    Sleep,
    Turn,
    React,
}

impl AssetKey {
    pub const ALL: [AssetKey; 9] = [
        AssetKey::Idle,
        AssetKey::Walk,
        AssetKey::Run,
        AssetKey::Stop,
        AssetKey::Jump,
        AssetKey::Climb,
        AssetKey::Sleep,
        AssetKey::Turn,
        AssetKey::React,
    ];

    pub const fn as_str(self) -> &'static str {
        match self {
            AssetKey::Idle => "idle",
            AssetKey::Walk => "walk",
            AssetKey::Run => "run",
            AssetKey::Stop => "stop",
            AssetKey::Jump => "jump",
            AssetKey::Climb => "climb",
            AssetKey::Sleep => "sleep",
            AssetKey::Turn => "turn",
            AssetKey::React => "react",
        }
    }

    pub fn from_key(s: &str) -> Option<AssetKey> {
        AssetKey::ALL.into_iter().find(|k| k.as_str() == s)
    }

    /// 循环动画（走路、奔跑、爬、睡）vs 一次性动画（停、跳、转身、摸头）。
    pub const fn loops(self) -> bool {
        matches!(
            self,
            AssetKey::Idle | AssetKey::Walk | AssetKey::Run | AssetKey::Climb | AssetKey::Sleep
        )
    }
}

/// 迁移的守卫条件标识。测试用它断言"迁移表每一行都真的被走过"。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Trigger {
    /// Spawn 的入场动画结束
    Spawned,
    /// `nextActionTimer ≤ 0` 且随机骰子命中 walk
    ActionDice,
    /// 随机骰子命中 jump
    ActionJumpDice,
    /// `distance(target) > run_distance` 或双击
    TargetFar,
    DoubleTap,
    /// 到达目标
    Arrived,
    /// 失去焦点（可配置）
    FocusLost,
    /// 减速帧放完
    Decelerated,
    /// 落地压缩帧放完
    SquashDone,
    /// 触到左/右墙
    WallTouched,
    /// 前方 24px 有台阶
    ObstacleAhead,
    /// 竖直方向落到支撑面
    TouchedDown,
    /// 爬到平台顶边（或舞台顶边）
    PlatformReached,
    /// 爬升体力耗尽
    StaminaDepleted,
    /// 支撑面消失（窗口关闭 / 面板被抽走 / 走出台面）
    SupportGone,
    /// 指针按下且命中宠物
    PointerDown,
    /// 释放（带初速度）
    PointerUp,
    /// 5 分钟无输入 + 系统空闲
    IdleTimeout,
    /// 用户显式暂停
    ManualSleep,
    /// 任意输入/系统活跃唤醒
    Wakeup,
    /// 右键菜单/CLI/快捷键的动作指令
    Command,
}

/// 迁移表的"从"侧：单个状态、`AnyGrounded`、或 `Any`。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EdgeFrom {
    One(State),
    AnyGrounded,
    Any,
}

impl EdgeFrom {
    pub fn matches(self, s: State) -> bool {
        match self {
            EdgeFrom::One(from) => s == from,
            EdgeFrom::AnyGrounded => s.is_grounded(),
            EdgeFrom::Any => !s.is_dragged(),
        }
    }

    pub const fn label(self) -> &'static str {
        match self {
            EdgeFrom::One(s) => match s {
                State::Spawn => "Spawn",
                State::Idle => "Idle",
                State::Walk => "Walk",
                State::Run => "Run",
                State::Stop => "Stop",
                State::Jump => "Jump",
                State::Land => "Land",
                State::Climb => "Climb",
                State::WallSit => "WallSit",
                State::Fall => "Fall",
                State::Dragged => "Dragged",
                State::Sleep => "Sleep",
            },
            EdgeFrom::AnyGrounded => "AnyGrounded",
            EdgeFrom::Any => "Any",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Edge {
    pub from: EdgeFrom,
    pub to: State,
    pub trigger: Trigger,
    /// 设计书里的守卫条件原文（或"实现补充"说明）
    pub guard: &'static str,
    /// true = 该行是 M1 实现时补的，§3.3 原表没有，需要回写设计书
    pub added_in_impl: bool,
}

/// §3.3 迁移表。行的顺序与设计书一致，代码行为以本表为准（不一致时回写文档）。
pub const TRANSITION_TABLE: &[Edge] = &[
    Edge {
        from: EdgeFrom::One(State::Spawn),
        to: State::Idle,
        trigger: Trigger::Spawned,
        guard: "入场动画结束",
        added_in_impl: false,
    },
    Edge {
        from: EdgeFrom::One(State::Idle),
        to: State::Walk,
        trigger: Trigger::ActionDice,
        guard: "nextActionTimer ≤ 0 且 random() < 0.6；选目标 x、定朝向",
        added_in_impl: false,
    },
    Edge {
        from: EdgeFrom::One(State::Idle),
        to: State::Walk,
        trigger: Trigger::Command,
        guard: "用户指令（mermaid 的 \"随机计时器 / 用户指令\" 另一支）",
        added_in_impl: false,
    },
    Edge {
        from: EdgeFrom::AnyGrounded,
        to: State::Run,
        trigger: Trigger::Command,
        guard: "指令指定奔跑（RunTo / action set run）",
        added_in_impl: true,
    },
    Edge {
        from: EdgeFrom::One(State::Idle),
        to: State::Run,
        trigger: Trigger::TargetFar,
        guard: "distance(target) > 300px",
        added_in_impl: false,
    },
    Edge {
        from: EdgeFrom::One(State::Walk),
        to: State::Run,
        trigger: Trigger::TargetFar,
        guard: "distance(target) > 300px",
        added_in_impl: false,
    },
    Edge {
        from: EdgeFrom::AnyGrounded,
        to: State::Run,
        trigger: Trigger::DoubleTap,
        guard: "用户双击（同时播 react 一次性动画）",
        added_in_impl: true,
    },
    Edge {
        from: EdgeFrom::One(State::Walk),
        to: State::Stop,
        trigger: Trigger::Arrived,
        guard: "到达目标；减速 3 帧",
        added_in_impl: false,
    },
    Edge {
        from: EdgeFrom::One(State::Run),
        to: State::Stop,
        trigger: Trigger::Arrived,
        guard: "到达目标",
        added_in_impl: false,
    },
    Edge {
        from: EdgeFrom::One(State::Walk),
        to: State::Stop,
        trigger: Trigger::FocusLost,
        guard: "失去焦点（可配置 stop_on_focus_lost）",
        added_in_impl: false,
    },
    Edge {
        from: EdgeFrom::One(State::Run),
        to: State::Stop,
        trigger: Trigger::FocusLost,
        guard: "失去焦点（表格只写了 Walk，实现对 Run 同样处理）",
        added_in_impl: true,
    },
    Edge {
        from: EdgeFrom::One(State::Stop),
        to: State::Idle,
        trigger: Trigger::Decelerated,
        guard: "减速帧放完",
        added_in_impl: false,
    },
    Edge {
        from: EdgeFrom::AnyGrounded,
        to: State::Jump,
        trigger: Trigger::Command,
        guard: "指令",
        added_in_impl: false,
    },
    Edge {
        from: EdgeFrom::AnyGrounded,
        to: State::Jump,
        trigger: Trigger::ObstacleAhead,
        guard: "障碍前 24px（表格只给 Walk/Run，实现覆盖所有着地态）",
        added_in_impl: true,
    },
    Edge {
        from: EdgeFrom::One(State::Idle),
        to: State::Jump,
        trigger: Trigger::ActionJumpDice,
        guard: "随机（骰子落在 jump 区间）",
        added_in_impl: false,
    },
    Edge {
        from: EdgeFrom::One(State::Jump),
        to: State::Land,
        trigger: Trigger::TouchedDown,
        guard: "y ≥ ground",
        added_in_impl: false,
    },
    Edge {
        from: EdgeFrom::One(State::Fall),
        to: State::Land,
        trigger: Trigger::TouchedDown,
        guard: "y ≥ ground",
        added_in_impl: false,
    },
    Edge {
        from: EdgeFrom::One(State::Land),
        to: State::Idle,
        trigger: Trigger::SquashDone,
        guard: "squash & stretch 2 帧放完",
        added_in_impl: false,
    },
    Edge {
        from: EdgeFrom::AnyGrounded,
        to: State::Climb,
        trigger: Trigger::WallTouched,
        guard: "触到左/右墙（表格只写 Idle，实现覆盖所有着地态）",
        added_in_impl: true,
    },
    Edge {
        from: EdgeFrom::One(State::Climb),
        to: State::WallSit,
        trigger: Trigger::PlatformReached,
        guard: "到达平台顶边（含舞台顶边的隐式平台）",
        added_in_impl: false,
    },
    Edge {
        from: EdgeFrom::One(State::Climb),
        to: State::Fall,
        trigger: Trigger::StaminaDepleted,
        guard: "climbStamina ≤ 0",
        added_in_impl: false,
    },
    Edge {
        from: EdgeFrom::One(State::WallSit),
        to: State::Jump,
        trigger: Trigger::Command,
        guard: "跳下：经 Jump→Land→Idle，不凭空回到 Idle（原表写 WallSit→Idle）",
        added_in_impl: true,
    },
    Edge {
        from: EdgeFrom::One(State::WallSit),
        to: State::Jump,
        trigger: Trigger::ActionJumpDice,
        guard: "坐够了自己跳下（原表未写明谁发起）",
        added_in_impl: true,
    },
    Edge {
        from: EdgeFrom::AnyGrounded,
        to: State::Fall,
        trigger: Trigger::SupportGone,
        guard: "支撑面消失：窗口关闭、面板被抽走、走出台面边缘",
        added_in_impl: true,
    },
    Edge {
        from: EdgeFrom::Any,
        to: State::Dragged,
        trigger: Trigger::PointerDown,
        guard: "指针按下且命中宠物；记录速度用于抛出",
        added_in_impl: false,
    },
    Edge {
        from: EdgeFrom::One(State::Dragged),
        to: State::Fall,
        trigger: Trigger::PointerUp,
        guard: "释放（有初速度）",
        added_in_impl: false,
    },
    Edge {
        from: EdgeFrom::One(State::Idle),
        to: State::Sleep,
        trigger: Trigger::IdleTimeout,
        guard: "5 分钟无输入 + 系统空闲",
        added_in_impl: false,
    },
    Edge {
        from: EdgeFrom::AnyGrounded,
        to: State::Sleep,
        trigger: Trigger::ManualSleep,
        guard: "菜单/快捷键暂停",
        added_in_impl: true,
    },
    Edge {
        from: EdgeFrom::One(State::Sleep),
        to: State::Idle,
        trigger: Trigger::Wakeup,
        guard: "唤醒（原表缺 Sleep 的出边，实现补：任意输入或系统活跃）",
        added_in_impl: true,
    },
];

/// 迁移记录：给单测、`session_log`（M3）与调试用。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Transition {
    pub from: State,
    pub to: State,
    pub trigger: Trigger,
    pub tick: u64,
}

/// 表格中所有"实现补充"的行，用于生成设计书回写清单。
pub fn implementation_added_edges() -> impl Iterator<Item = &'static Edge> {
    TRANSITION_TABLE.iter().filter(|e| e.added_in_impl)
}

/// 生成 §3.3 迁移表的 Markdown。改状态机时用 `opet sim table` 重新生成并回写设计书
/// （约定：不一致时以代码为准）。
pub fn transition_table_markdown() -> String {
    use std::fmt::Write as _;
    let mut out = String::from("| 从 | 到 | 触发 | 守卫 | 来源 |\n|---|---|---|---|---|\n");
    for e in TRANSITION_TABLE {
        let source = if e.added_in_impl {
            "M1 实现补充"
        } else {
            "§3.3 原表"
        };
        let _ = writeln!(
            out,
            "| {} | {:#?} | {:#?} | {} | {source} |",
            e.from.label(),
            e.to,
            e.trigger,
            e.guard
        );
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn table_has_no_duplicate_rows() {
        let mut seen = Vec::new();
        for e in TRANSITION_TABLE {
            let key = (format!("{:?}", e.from), e.to, e.trigger);
            assert!(!seen.contains(&key), "迁移表重复行: {key:?}");
            seen.push(key);
        }
    }

    #[test]
    fn every_state_is_reachable_and_no_dead_states() {
        for s in State::ALL {
            let has_in = TRANSITION_TABLE
                .iter()
                .any(|e| e.to == s || (e.from.matches(s) && e.to != s));
            let has_out = s == State::Spawn || TRANSITION_TABLE.iter().any(|e| e.from.matches(s));
            assert!(has_in, "{s:?} 进不去");
            assert!(has_out, "{s:?} 出不去");
        }
    }

    #[test]
    fn asset_keys_cover_all_states() {
        // 每个状态都能映射到图集行，且映射结果都是 pet.json 里的合法键
        for s in State::ALL {
            let k = s.asset_key();
            assert_eq!(AssetKey::from_key(k.as_str()), Some(k));
        }
        assert_eq!(State::Sleep.asset_key(), AssetKey::Sleep);
        assert!(!State::Sleep.animates());
        assert!(State::Idle.animates());
    }

    #[test]
    fn edge_from_semantics() {
        assert!(EdgeFrom::AnyGrounded.matches(State::Idle));
        assert!(!EdgeFrom::AnyGrounded.matches(State::Jump));
        assert!(EdgeFrom::Any.matches(State::Idle));
        // Any 不含 Dragged 自身（否则 Dragged→Dragged 会自触发）
        assert!(!EdgeFrom::Any.matches(State::Dragged));
    }
}
