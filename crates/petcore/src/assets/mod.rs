//! 资产清单与帧调度。像素解码（WebP/PNG）在 M2 接入，内核只处理元数据与"该显示哪一格"。

pub mod manifest;
pub mod scheduler;

pub use manifest::{Grid, Manifest, ManifestError, SUPPORTED_FORMAT, StateTrack};
pub use scheduler::{Scheduler, SpriteFrame};

/// M1 还没有真实美术：这份 9 状态齐全的最小清单同时充当默认资产与测试夹具。
/// M2 的 `bake.py` 会用同样的 schema 产出正式图集。
pub const DEFAULT_MANIFEST_JSON: &str = include_str!("../../assets/cat_shiro.pet.json");

pub fn default_manifest() -> Manifest {
    Manifest::parse(DEFAULT_MANIFEST_JSON).expect("内置默认清单必须合法")
}
