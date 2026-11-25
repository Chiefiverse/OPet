//! `opet-overlay-win32` 独立进程入口（D3：宠物绝不进常驻 shell 进程 —— Windows 上没有
//! Quickshell，但同一条理由成立：崩溃不许带走桌面，所以它必须是自己那个进程）。
//!
//! 参数解析与语义拒绝全在 [`overlay_win32::cli`]，这里只把结果翻译成退出码。
//!
//! ## 退出码的口径（与 Wayland 侧**逐位相同**，因为脚本读的是数字不是文字）
//!
//! - **0** = 跑过一轮并正常退出（含 `--seconds` 到点自毁），或 `--help`，或 `--probe` 通过。
//! - **1** = `--probe` 跑了而**没通过**（`cli::Outcome::Probed(false)`）。这一档在
//!   M5/M6/M7e 那几片里是**留空**的 —— 不是"未用所以随便给"，而是当时这片没有可探测的后端；
//!   `AGENTS.md` 与这里那句"下一片若加 `--probe` 的对应物，必须就用这个数"是同一个约定，
//!   **M7-f 落地的就是它**（后端 = [`overlay_win32::probe`]，判据 = `probe_ok`）。
//!   与 Wayland 那家逐位相同，因为 Phase B 的脚本读的是数字不是文字。
//! - **2** = `cli::dispatch` 返回 `Err`：参数错，**或**运行期失败。这两类在 Wayland 侧
//!   也是同一个数（`Outcome` 里没有这个分岔），本片不单独改形状 —— 改了脚本读到的
//!   就不再是同一个东西。
//!
//! ⚠️ `opet ctl` 那侧另有一条 **3 = 「没有在跑的宠物」**（设计书 §10.2 钉死，widget 与脚本
//! 读它）。本二进制**永不**返回 3：它不是控制面，把 3 用在别处会让读脚本的人以为
//! "宠物没起"，而那与"命令写错了"是两件完全不同的事。

use std::process::ExitCode;

use overlay_win32::cli::{self, Outcome};

fn main() -> ExitCode {
    let argv: Vec<String> = std::env::args().skip(1).collect();
    match cli::dispatch(&argv) {
        Ok(Outcome::Help(usage)) => {
            println!("{usage}");
            ExitCode::SUCCESS
        }
        Ok(Outcome::Ran) => ExitCode::SUCCESS,
        // 与 `opet-cli` 那侧逐位相同：0 = 探测通过，1 = 探测跑了而没通过。
        // 报告正文已经在 `dispatch` 里打到 stdout 了，这里只翻数字 —— 脚本读的是数字。
        Ok(Outcome::Probed(true)) => ExitCode::SUCCESS,
        Ok(Outcome::Probed(false)) => ExitCode::FAILURE,
        Err(e) => {
            eprintln!("{e}\n{USAGE}", USAGE = cli::USAGE);
            ExitCode::from(2)
        }
    }
}
