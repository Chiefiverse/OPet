//! 渲染适配层接口（§3.7）。M1 只定义契约与一个无窗口的实现，供 headless 模拟与契约测试使用；
//! `overlay-wayland` / `overlay-win32` / `overlay-cocoa` 在 M4–M6 各自实现同一组用例。

use crate::intent::PresentationIntent;
use crate::world::Monitor;

#[derive(Clone, Debug, PartialEq)]
pub struct OverlayConfig {
    /// 初始舞台；实现方要在显示器变化时上报 [`crate::world::WorldEvent`]
    pub monitor: String,
    /// surface 尺寸（逻辑像素），来自 [`PresentationIntent::surface`]
    pub width: f32,
    pub height: f32,
    /// HiDPI：逻辑→物理的缩放
    pub scale: f32,
    /// Wayland: `layer = overlay`、`keyboard_interactivity = none`、`exclusive_zone = 0`
    pub wants_passthrough: bool,
}

#[derive(Clone, Debug, PartialEq)]
pub enum PointerEvent {
    Press { at: crate::geom::Vec2 },
    Release { at: crate::geom::Vec2 },
    DoubleTap { at: crate::geom::Vec2 },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BackendError {
    pub message: String,
}

impl BackendError {
    pub fn new(msg: impl Into<String>) -> Self {
        BackendError {
            message: msg.into(),
        }
    }
}

pub type BackendResult<T> = Result<T, BackendError>;

/// 每平台一个实现，接口相同。**实现方必须是独立进程**（硬约束 3），崩溃只丢宠物不丢桌面。
pub trait OverlayBackend: Send {
    fn create(&mut self, cfg: &OverlayConfig) -> BackendResult<()>;

    /// 呈现一帧。**省电契约**：`intent.sprite == None` 时不得提交新 buffer（内容与上一帧相同）；
    /// `intent.needs_redraw == false` 时连 surface 移动都不必做（§4 省电第 ② 招）。
    /// 移动 surface（`set_window_geometry` + 重设 input region）比提交 buffer 便宜得多，
    /// 所以实现方按这两个标志分级处理。
    fn present(&mut self, intent: &PresentationIntent) -> BackendResult<()>;

    /// Windows/macOS 的整窗近似穿透；Wayland 实现可以忽略它（input region 是协议级的）。
    fn set_passthrough(&mut self, on: bool) -> BackendResult<()>;

    fn move_to(&mut self, pos: crate::geom::Vec2) -> BackendResult<()>;

    fn monitors(&self) -> Vec<Monitor>;

    fn on_pointer(&mut self, tx: std::sync::mpsc::Sender<PointerEvent>);

    fn shutdown(&mut self) -> BackendResult<()>;
}

/// 无窗口实现：只记账，用于 M1 的 headless 模拟、停帧断言与 §4 省电指标测量。
#[derive(Clone, Debug, Default)]
pub struct NullBackend {
    /// 真正提交新 buffer 的次数（省电口径的分母）
    pub presented: u64,
    /// 只移动 surface、没有新内容的次数
    pub moved: u64,
    /// 整 tick 无事可做的次数
    pub skipped: u64,
    pub passthrough_on: bool,
    pub created: bool,
    last: Option<PresentationIntent>,
}

impl OverlayBackend for NullBackend {
    fn create(&mut self, _cfg: &OverlayConfig) -> BackendResult<()> {
        self.created = true;
        Ok(())
    }

    fn present(&mut self, intent: &PresentationIntent) -> BackendResult<()> {
        if intent.sprite.is_some() {
            self.presented += 1;
            self.last = Some(intent.clone());
        } else if intent.needs_redraw {
            self.moved += 1;
            self.last = Some(intent.clone());
        } else {
            self.skipped += 1;
        }
        Ok(())
    }

    fn set_passthrough(&mut self, on: bool) -> BackendResult<()> {
        self.passthrough_on = on;
        Ok(())
    }

    fn move_to(&mut self, _pos: crate::geom::Vec2) -> BackendResult<()> {
        Ok(())
    }

    fn monitors(&self) -> Vec<Monitor> {
        Vec::new()
    }

    fn on_pointer(&mut self, _tx: std::sync::mpsc::Sender<PointerEvent>) {}

    fn shutdown(&mut self) -> BackendResult<()> {
        self.created = false;
        Ok(())
    }
}

impl NullBackend {
    pub fn last_intent(&self) -> Option<&PresentationIntent> {
        self.last.as_ref()
    }
}
