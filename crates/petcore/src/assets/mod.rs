//! 资产清单与帧调度。像素解码（WebP/PNG）在 M2 接入，内核只处理元数据与"该显示哪一格"。
//! `zip` 是 `.petpack` 的容器解码（store-only，见该模块文档）；它放在 `petcore` 是因为
//! "包里的清单长什么样、条目名能不能信"这件事属于清单侧，不属于落盘/IO 侧（D13 那条
//! "IO 不进抽样引擎"约束管的是 `gacha`，资产元数据解析本来就在 `petcore::assets`）。

pub mod accessory;
pub mod manifest;
pub mod scheduler;
pub mod source;
pub mod zip;

pub use accessory::AccessoryManifest;
pub use manifest::{Grid, Manifest, ManifestError, SUPPORTED_FORMAT, StateTrack};
pub use scheduler::{Scheduler, SpriteFrame};
pub use source::{
    AssetDir, AssetError, AssetHandle, AssetItem, AssetSource, builtin_dir, installed_dir,
    locate_first, manifest_member, pack_prefix,
};
pub use zip::{ZipEntry, ZipError, ZipIndex};

/// M1 还没有真实美术：这份 9 状态齐全的最小清单同时充当默认资产与测试夹具。
/// M2 的 `bake.py` 会用同样的 schema 产出正式图集。
pub const DEFAULT_MANIFEST_JSON: &str = include_str!("../../assets/cat_shiro.pet.json");

pub fn default_manifest() -> Manifest {
    Manifest::parse(DEFAULT_MANIFEST_JSON).expect("内置默认清单必须合法")
}
