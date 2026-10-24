//! `opet-overlay-wayland --probe`：只读探测 Wayland registry 与显示器几何。
//!
//! 这是 Phase B 第 1 步的交付物（设计书 §6.2）：Arch 上不一定有 `wayland-info`/`weston-info`，
//! 所以客户端必须自带诊断。**不映射任何 surface**，在 live 会话上跑完全无副作用。

use smithay_client_toolkit::{
    delegate_output, delegate_registry,
    output::{OutputHandler, OutputState},
    registry::{ProvidesRegistryState, RegistryState},
    registry_handlers,
};
use wayland_client::{
    Connection, QueueHandle,
    globals::{Global, registry_queue_init},
    protocol::wl_output,
};

/// 一个显示器的诊断快照（逻辑像素，与内核 [`petcore::world::Monitor`] 同口径）。
#[derive(Clone, Debug, Default)]
pub struct OutputSnapshot {
    pub name: String,
    pub logical_x: i32,
    pub logical_y: i32,
    pub logical_w: i32,
    pub logical_h: i32,
    pub scale: i32,
    pub transform: String,
    /// 首选模式给出的物理尺寸
    pub preferred: Option<(i32, i32)>,
}

pub struct ProbeReport {
    pub globals: Vec<Global>,
    pub outputs: Vec<OutputSnapshot>,
    pub session_error: Option<String>,
}

impl ProbeReport {
    pub fn version_of(&self, interface: &str) -> Option<u32> {
        self.globals
            .iter()
            .filter(|g| g.interface == interface)
            .map(|g| g.version)
            .max()
    }

    pub fn has(&self, interface: &str) -> bool {
        self.version_of(interface).is_some()
    }
}

struct Probe {
    registry_state: Option<RegistryState>,
    output_state: Option<OutputState>,
}

impl OutputHandler for Probe {
    fn output_state(&mut self) -> &mut OutputState {
        self.output_state
            .as_mut()
            .expect("probe: OutputState 尚未初始化")
    }
    fn new_output(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_output::WlOutput) {}
    fn update_output(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_output::WlOutput) {}
    fn output_destroyed(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_output::WlOutput) {}
}

impl ProvidesRegistryState for Probe {
    fn registry(&mut self) -> &mut RegistryState {
        self.registry_state
            .as_mut()
            .expect("probe: RegistryState 尚未初始化")
    }
    // runtime_add_global / runtime_remove_global 由 registry_handlers! 宏生成
    registry_handlers![OutputState];
}

delegate_registry!(Probe);
delegate_output!(Probe);

/// 连接、枚举 registry 与 output，两次 roundtrip 即返回；不创建任何 surface。
pub fn probe() -> ProbeReport {
    let conn = match Connection::connect_to_env() {
        Ok(c) => c,
        Err(e) => return failed(&format!("连不上 Wayland 合成器: {e}")),
    };

    let (globals, mut queue) = match registry_queue_init::<Probe>(&conn) {
        Ok(g) => g,
        Err(e) => return failed(&format!("registry 初始化失败: {e}")),
    };
    let qh = queue.handle();

    let mut state = Probe {
        registry_state: Some(RegistryState::new(&globals)),
        output_state: Some(OutputState::new(&globals, &qh)),
    };

    // 第一次收 global 列表，第二次收每个 output 的 geometry/name/mode/done。
    if queue.roundtrip(&mut state).is_err() {
        return failed("第一次 roundtrip 失败");
    }
    if queue.roundtrip(&mut state).is_err() {
        return failed("第二次 roundtrip 失败");
    }

    let list = globals.contents().clone_list();
    let outputs = state
        .output_state
        .as_ref()
        .map(|os| {
            os.outputs()
                .filter_map(|o| os.info(&o))
                .map(|i| {
                    let (lx, ly) = i.logical_position.unwrap_or((0, 0));
                    let (lw, lh) = i.logical_size.unwrap_or((0, 0));
                    OutputSnapshot {
                        name: i.name.clone().unwrap_or_default(),
                        logical_x: lx,
                        logical_y: ly,
                        logical_w: lw,
                        logical_h: lh,
                        scale: i.scale_factor,
                        transform: format!("{:?}", i.transform),
                        preferred: i
                            .modes
                            .iter()
                            .find(|m| m.preferred)
                            .map(|m| (m.dimensions.0, m.dimensions.1)),
                    }
                })
                .collect()
        })
        .unwrap_or_default();

    ProbeReport {
        globals: list,
        outputs,
        session_error: None,
    }
}

fn failed(msg: &str) -> ProbeReport {
    ProbeReport {
        globals: Vec::new(),
        outputs: Vec::new(),
        session_error: Some(msg.to_string()),
    }
}

const HEADLINES: &[&str] = &[
    "zwlr_layer_shell_v1",
    "wp_viewporter",
    // 本机实测（Hyprland 0.56.2）：接口名就是 `wp_` 前缀，**不是** `zwp_`
    "wp_fractional_scale_manager_v1",
    // 有它 sctk 才能用 `set_shape` 换光标（否则退回从 xcursor 主题贴图）
    "wp_cursor_shape_manager_v1",
    "zwlr_output_manager_v1",
    "wl_shm",
    "wl_compositor",
    "wl_seat",
    "wl_output",
];

/// Phase B 报告里贴的就是它。
pub fn format_report(r: &ProbeReport) -> String {
    use std::fmt::Write;
    let mut s = String::new();
    let _ = writeln!(s, "== opet overlay --probe");
    if let Some(e) = &r.session_error {
        let _ = writeln!(s, "会话错误: {e}");
        let _ = writeln!(
            s,
            "XDG_SESSION_TYPE={} WAYLAND_DISPLAY={} DISPLAY={}",
            env_or("<未设>", "XDG_SESSION_TYPE"),
            env_or("<未设>", "WAYLAND_DISPLAY"),
            env_or("<未设>", "DISPLAY")
        );
        return s;
    }

    let mut g = r.globals.clone();
    g.sort_by(|a, b| a.interface.cmp(&b.interface).then(a.name.cmp(&b.name)));
    let _ = writeln!(s, "绑定到 {} 个 global：", g.len());
    for itf in HEADLINES {
        let _ = writeln!(
            s,
            "  {itf:<38} {}",
            match r.version_of(itf) {
                Some(v) => format!("v{v} 可用"),
                None => "缺席 ⇒ 走降级路径（§4）".into(),
            }
        );
    }
    let _ = writeln!(s, "  —— 其余 global ——");
    for x in &g {
        if !HEADLINES.contains(&x.interface.as_str()) {
            let _ = writeln!(s, "  {:<38} v{}", x.interface, x.version);
        }
    }

    let _ = writeln!(s, "== outputs");
    for o in &r.outputs {
        let _ = writeln!(
            s,
            " {:<10} 逻辑 {}x{} @({},{}) scale={} transform={} 首选物理 {:?}",
            if o.name.is_empty() { "?" } else { &o.name },
            o.logical_w,
            o.logical_h,
            o.logical_x,
            o.logical_y,
            o.scale,
            o.transform,
            o.preferred
        );
    }
    if r.outputs.is_empty() {
        let _ = writeln!(s, "  （没枚举到 wl_output）");
    }
    let _ = writeln!(
        s,
        "== 判定：layer-shell {}、viewporter {}、fractional-scale {}",
        yesno(r.has("zwlr_layer_shell_v1")),
        yesno(r.has("wp_viewporter")),
        yesno(r.has("wp_fractional_scale_manager_v1"))
    );
    s
}

fn yesno(b: bool) -> &'static str {
    if b { "有" } else { "无" }
}

fn env_or(def: &str, k: &str) -> String {
    std::env::var(k).unwrap_or_else(|_| def.into())
}
