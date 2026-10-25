//! OPet 行为内核（`petcore`）——平台无关、无窗口依赖。
//!
//! 分层原则（设计书 §3.1）：内核每帧只输出一条 [`intent::PresentationIntent`]
//! （当前精灵格、逻辑坐标、朝向、输入区域多边形、是否需要重绘），渲染适配层负责把它变成平台像素。
//! 好处：单元测试不需要显示服务器，v2 换实时 3D 后端时业务层不动。
//!
//! ```no_run
//! use petcore::*;
//! use petcore::behavior::BehaviorConfig;
//!
//! let json = r#"{"format":2,"slug":"blob","sheet":"sheet.webp",
//!   "grid":{"cols":8,"rows":1,"w":128,"h":128},
//!   "states":{"idle":{"rows":[0],"fps":8,"loop":true}},
//!   "anchor":{"feet":[64,120]}}"#;
//! let manifest = assets::Manifest::parse(json).unwrap();
//! let world = world::World::single("HDMI-A-1", 1920.0, 1080.0, 1.0);
//! let mut sim = sim::Sim::new(world, &manifest, 42, BehaviorConfig::default());
//! for _ in 0..600 {
//!     let (_transitions, intent) = sim.step();
//!     sim.check_invariants().unwrap();
//!     // intent 交给 overlay-wayland / overlay-win32 / overlay-cocoa
//! }
//! ```

pub mod assets;
pub mod behavior;
pub mod gacha;
pub mod geom;
pub mod intent;
pub mod physics;
pub mod render;
pub mod rng;
pub mod sim;
pub mod world;

pub use behavior::{
    AssetKey, BehaviorConfig, BehaviorMachine, Command, State, Transition, Trigger,
};
pub use geom::{Facing, Rect, Vec2};
pub use intent::{InputRegion, PresentationIntent};
pub use physics::Body;
pub use render::{BackendError, BackendResult, OverlayBackend, OverlayConfig, PointerEvent};
pub use rng::Rng;
pub use sim::{DT, Sim, SimStats, TICK_HZ};
pub use world::{Monitor, Platform, PlatformSpec, Support, World, WorldEvent};
