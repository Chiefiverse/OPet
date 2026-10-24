//! `opet-overlay-wayland` 独立进程入口（D3：宠物绝不进常驻 shell 进程）。
//!
//! 参数解析与 `opet overlay` 子命令共用 [`overlay_wayland::cli`]（hand-rolled，不引 clap），
//! 这里只把结果翻译成退出码。

use std::process::ExitCode;

use overlay_wayland::cli::{self, Outcome};

fn main() -> ExitCode {
    let argv: Vec<String> = std::env::args().skip(1).collect();
    match cli::dispatch(&argv) {
        Ok(Outcome::Help(usage)) => {
            println!("{usage}");
            ExitCode::SUCCESS
        }
        Ok(Outcome::Ran) => ExitCode::SUCCESS,
        // 探测没通过 ⇒ 非零，Phase B 脚本据此判定
        Ok(Outcome::Probed(true)) => ExitCode::SUCCESS,
        Ok(Outcome::Probed(false)) => ExitCode::FAILURE,
        Err(e) => {
            eprintln!("{e}\n{USAGE}", USAGE = cli::USAGE);
            ExitCode::from(2)
        }
    }
}
