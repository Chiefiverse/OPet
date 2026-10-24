//! OPet 的 Wayland 适配层（设计书 §3.7）。
//!
//! 契约来自 [`petcore::render::OverlayBackend`]：**内核只给图集格坐标，不经手像素**，
//! 所以解码 sheet、预乘 alpha、写 shm buffer、把 alpha 区域翻成 `wl_region` 都在这里做。

pub mod app;
pub mod atlas;
pub mod cli;
pub mod probe;
pub mod region;
