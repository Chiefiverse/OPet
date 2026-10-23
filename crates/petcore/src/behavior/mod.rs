//! 行为内核：§3.3 状态机 + 物理 + 世界模型的组合。

pub mod config;
pub mod machine;
pub mod state;

pub use config::{Action, BehaviorConfig, EnabledActions};
pub use machine::{BehaviorMachine, Command};
pub use state::{
    AssetKey, Edge, EdgeFrom, State, TRANSITION_TABLE, Transition, Trigger,
    implementation_added_edges, transition_table_markdown,
};
