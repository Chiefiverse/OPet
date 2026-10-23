//! `opet` 命令行入口。M1 只有 headless 模拟与吞吐基准，
//! overlay/IPC/资产子命令在 M3/M4 补（§10.2）。

use std::path::PathBuf;
use std::process::ExitCode;
use std::time::Instant;

use petcore::assets::Manifest;
use petcore::behavior::BehaviorConfig;
use petcore::sim::{Sim, TICK_HZ, run_headless};
use petcore::world::World;

const USAGE: &str = "\
opet <子命令>

M1 可用：
  opet sim headless [--hours H] [--seed S] [--width W] [--height H] [--monitor NAME] [--manifest FILE]
      固定步长跑内核模拟，每 tick 校验世界不变量（§6.1 验收：8h 无位置漂移）
  opet sim bench [--ticks N]
      纯逻辑吞吐（§6.1 验收：≥ 1_000_000 tick/s，release 构建）
  opet sim table
      打印 §3.3 迁移表的 Markdown（改状态机后用它回写设计书）

  opet --help
";

/// M1 门槛：每秒至少这么多 tick。
const TICK_RATE_GATE: f64 = 1_000_000.0;

#[derive(Debug)]
struct Opts {
    hours: f32,
    ticks: Option<u64>,
    seed: u64,
    width: f32,
    height: f32,
    monitor: String,
    manifest: Option<PathBuf>,
}

impl Default for Opts {
    fn default() -> Self {
        Self {
            hours: 8.0,
            ticks: None,
            seed: 1,
            width: 1920.0,
            height: 1080.0,
            monitor: "DP-1".into(),
            manifest: None,
        }
    }
}

fn num(args: &[String], i: &mut usize, name: &str) -> Result<f64, String> {
    *i += 1;
    let raw = args.get(*i).ok_or_else(|| format!("{name} 缺少参数值"))?;
    raw.parse::<f64>()
        .map_err(|e| format!("{name} 不是数字: {e}"))
}

fn word(args: &[String], i: &mut usize, name: &str) -> Result<String, String> {
    *i += 1;
    args.get(*i)
        .cloned()
        .ok_or_else(|| format!("{name} 缺少参数值"))
}

fn parse_opts(args: &[String]) -> Result<Opts, String> {
    let mut o = Opts::default();
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--hours" => o.hours = num(args, &mut i, "--hours")? as f32,
            "--width" => o.width = num(args, &mut i, "--width")? as f32,
            "--height" => o.height = num(args, &mut i, "--height")? as f32,
            "--seed" => o.seed = num(args, &mut i, "--seed")? as u64,
            "--ticks" => o.ticks = Some(num(args, &mut i, "--ticks")? as u64),
            "--monitor" => o.monitor = word(args, &mut i, "--monitor")?,
            "--manifest" => o.manifest = Some(PathBuf::from(word(args, &mut i, "--manifest")?)),
            other => return Err(format!("未知选项: {other}")),
        }
        i += 1;
    }
    Ok(o)
}

fn load_manifest(path: Option<&PathBuf>) -> Result<Manifest, String> {
    match path {
        None => Ok(petcore::assets::default_manifest()),
        Some(p) => {
            let s = p
                .to_str()
                .ok_or_else(|| "清单路径不是合法 UTF-8".to_string())?;
            Manifest::load(s).map_err(|e| format!("清单加载失败: {e}"))
        }
    }
}

fn sim_from<'m>(o: &Opts, manifest: &'m Manifest) -> Sim<'m> {
    let world = World::single(&o.monitor, o.width, o.height, 1.0);
    Sim::new(world, manifest, o.seed, BehaviorConfig::default())
}

fn headless(o: &Opts) -> Result<(), String> {
    let manifest = load_manifest(o.manifest.as_ref())?;
    let ticks = (o.hours as f64 * 3600.0 * TICK_HZ as f64).round() as u64;
    let sim = sim_from(o, &manifest);
    let started = Instant::now();
    let (stats, fingerprint) =
        run_headless(sim, ticks).map_err(|e| format!("漂移/不变量失败: {e}"))?;
    let wall = started.elapsed().as_secs_f64().max(1.0 / 1_000_000.0);

    println!(
        "headless: {} tick（{} 小时模拟时间）seed={} 指纹={:016x}",
        stats.ticks, o.hours, o.seed, fingerprint
    );
    println!(
        "  实速 {:.0} tick/s，墙钟 {:.1}s",
        stats.ticks as f64 / wall,
        wall
    );
    let ticks = stats.ticks.max(1) as f64;
    println!(
        "  迁移 {} 次；新 buffer {}（{:.1}%）、仅移动 surface {}（{:.1}%）、无事 {}（{:.1}%）",
        stats.transitions,
        stats.frames_presented,
        100.0 * stats.frames_presented as f64 / ticks,
        stats.surface_moves,
        100.0 * stats.surface_moves as f64 / ticks,
        stats.frames_skipped,
        100.0 * stats.frames_skipped as f64 / ticks,
    );
    for (state, n) in stats.state_ticks.iter().filter(|(_, n)| *n > 0) {
        let name = format!("{state:?}");
        println!(
            "  {name:<9} {n:>10} tick  {:.2}%",
            *n as f64 * 100.0 / stats.ticks as f64
        );
    }
    Ok(())
}

fn bench(o: &Opts) -> Result<(), String> {
    let manifest = load_manifest(o.manifest.as_ref())?;
    let ticks = o.ticks.unwrap_or(3_000_000);
    let mut sim = sim_from(o, &manifest);
    // 吞吐数字不含不变量校验，和 §6.1 的"纯逻辑"口径一致
    let started = Instant::now();
    sim.run(ticks);
    let secs = started.elapsed().as_secs_f64().max(1.0 / 1_000_000.0);
    let rate = ticks as f64 / secs;
    println!(
        "bench: {ticks} tick 用时 {:.3}s ⇒ {:.0} tick/s（门槛 {TICK_RATE_GATE:.0}）",
        secs, rate
    );
    if rate < TICK_RATE_GATE {
        return Err(format!(
            "吞吐未达标：{rate:.0} tick/s < {TICK_RATE_GATE:.0}。请用 --release 构建。"
        ));
    }
    println!("  通过 §6.1 吞吐门禁");
    Ok(())
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let result = match args.first().map(String::as_str) {
        Some("sim") => {
            let sub = args.get(1).map(String::as_str).unwrap_or("");
            let rest: Vec<String> = args.iter().skip(2).cloned().collect();
            match parse_opts(&rest) {
                Err(e) => Err(e),
                Ok(o) => match sub {
                    "headless" => headless(&o),
                    "bench" => bench(&o),
                    "table" => {
                        print!("{}", petcore::behavior::transition_table_markdown());
                        Ok(())
                    }
                    other => Err(format!(
                        "未知的 sim 子命令: {other}（可用：headless, bench, table）"
                    )),
                },
            }
        }
        Some("-h") | Some("--help") | Some("help") => {
            print!("{USAGE}");
            return ExitCode::SUCCESS;
        }
        Some(other) => Err(format!("未知子命令: {other}")),
        None => {
            print!("{USAGE}");
            return ExitCode::SUCCESS;
        }
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("错误: {e}");
            eprintln!("\n{USAGE}");
            ExitCode::FAILURE
        }
    }
}
