//! 命令行入口的**共用**部分：`opet-overlay-wayland` 独立二进制与 `opet overlay` 都走这里，
//! 免得 hand-rolled 解析写两份（口径与 `opet-cli` 一致：不引 clap）。

use crate::app::{self, Options};
use crate::probe;

pub const USAGE: &str = "\
opet overlay [--probe | --seconds N] …（独立二进制 opet-overlay-wayland 同参数）

  --probe            只读打印 Wayland registry 与显示器几何，不映射任何 surface
  --seconds N        运行 N 秒后自动退出（默认 3；0 = 一直跑）
  --manifest FILE    宠物清单（pet.json）；不给 = 内核内置 fixture（无像素，画不出东西）
  --monitor NAME     指定舞台显示器
  --panel-bottom N   底边面板高度（逻辑像素，默认 0；决定地面往上抬多少）
  --panel-top N      顶边栏高度（逻辑像素，默认 0）。layer-shell 的 margin 从可用区量起，
                     顶部有栏时不给这个值 ⇒ 宠物整体下移一个栏高、脚掉到屏幕外
  --seed N           内核随机种子（默认 1）
  --test-rect        不接内核，画一块纯色矩形（验证 surface 映射与点击穿透）
  --no-cursor        不设光标表面（调试用；代价是鼠标在宠物上方可能隐形）
";

#[derive(Debug, Default, PartialEq)]
pub struct Args {
    pub probe: bool,
    pub seconds: Option<f32>,
    pub manifest: Option<String>,
    pub monitor: Option<String>,
    pub panel_bottom: Option<f32>,
    pub panel_top: Option<f32>,
    pub seed: Option<u64>,
    pub test_rect: bool,
    pub no_cursor: bool,
}

impl Args {
    pub fn options(&self) -> Result<Options, String> {
        if !self.test_rect && self.manifest.is_none() {
            return Err(
                "没给 --manifest 就没有像素可画。要么给清单，要么用 --test-rect 只验 surface。\n\
                 （内置 fixture 只有元数据，M1 时代没有图集）"
                    .to_string(),
            );
        }
        // `--seconds 0` = 一直跑；**负数必须是错**，不能悄悄变成"永远跑" —— live 会话上
        // 一个赖着不走的 overlay 正是我们给自己立的红线。
        if self.seconds.is_some_and(|s| s < 0.0) {
            return Err(format!(
                "--seconds 不能是负数（给了 {:?}）；要一直跑就写 0",
                self.seconds.unwrap_or(0.0)
            ));
        }
        let pb = self.panel_bottom.unwrap_or(0.0);
        if pb < 0.0 {
            return Err(format!("--panel-bottom 不能是负数（给了 {pb}）"));
        }
        let pt = self.panel_top.unwrap_or(0.0);
        if pt < 0.0 {
            return Err(format!("--panel-top 不能是负数（给了 {pt}）"));
        }
        Ok(Options {
            manifest: self.manifest.clone(),
            seconds: match self.seconds {
                Some(0.0) => None,
                s => Some(s.unwrap_or(3.0)),
            },
            monitor: self.monitor.clone(),
            panel_bottom: pb,
            panel_top: pt,
            seed: self.seed.unwrap_or(1),
            test_rect: self.test_rect,
            no_cursor: self.no_cursor,
        })
    }
}

fn num<T: std::str::FromStr>(raw: &str, flag: &str) -> Result<T, String>
where
    T::Err: std::fmt::Display,
{
    raw.parse::<T>()
        .map_err(|e| format!("{flag} 不是数字: {e}"))
}

/// 同时接受 `--flag=值` 与 `--flag 值` 两种写法。
pub fn parse(args: &[String]) -> Result<Args, String> {
    let mut a = Args::default();
    let mut i = 0;
    while i < args.len() {
        let (key, inline) = match args[i].split_once('=') {
            Some((k, v)) => (k, Some(v.to_string())),
            None => (args[i].as_str(), None),
        };
        let next_val = |idx: &mut usize| -> Result<String, String> {
            if let Some(v) = inline.clone() {
                return Ok(v);
            }
            *idx += 1;
            let v = args
                .get(*idx)
                .cloned()
                .ok_or_else(|| format!("{key} 缺少参数值"))?;
            // 值长得像 flag ⇒ 说明用户漏写了值（`--monitor --probe`）。吞掉它的话
            // `--probe` 就永远不会执行而是起一个 surface —— live 会话上最坏的一种错。
            // 负数是合法数值（`--seconds -5` 由 options() 明确拒绝），不能一律当 flag。
            if v.starts_with('-') && v.parse::<f64>().is_err() {
                return Err(format!("{key} 缺少参数值（把 {v} 当成了值）"));
            }
            Ok(v)
        };
        match key {
            "--seconds" => a.seconds = Some(num(&next_val(&mut i)?, key)?),
            "--panel-bottom" => a.panel_bottom = Some(num(&next_val(&mut i)?, key)?),
            "--panel-top" => a.panel_top = Some(num(&next_val(&mut i)?, key)?),
            "--seed" => a.seed = Some(num(&next_val(&mut i)?, key)?),
            "--manifest" => a.manifest = Some(next_val(&mut i)?),
            "--monitor" => a.monitor = Some(next_val(&mut i)?),
            "--probe" => a.probe = true,
            "--test-rect" => a.test_rect = true,
            "--no-cursor" => a.no_cursor = true,
            other => return Err(format!("未知参数: {other}")),
        }
        i += 1;
    }
    Ok(a)
}

/// `--probe` 的通过判定：连得上会话，且有 `zwlr_layer_shell_v1`。
pub fn probe_ok(report: &probe::ProbeReport) -> bool {
    report.session_error.is_none() && report.has("zwlr_layer_shell_v1")
}

/// `dispatch` 的分支结果，调用方据此决定退出码与输出。
pub enum Outcome {
    /// 打的是 `--help`，`String` 是用法文本
    Help(String),
    /// 跑过探测：`bool` = 通过与否
    Probed(bool),
    /// 真跑了一轮 overlay（正常退出或 `--seconds` 到点）
    Ran,
}

/// 解析 + 分派。`Err` = 参数/运行失败，由调用方决定退出码。
pub fn dispatch(args: &[String]) -> Result<Outcome, String> {
    if args.iter().any(|a| a == "--help" || a == "-h") {
        return Ok(Outcome::Help(USAGE.to_string()));
    }
    let parsed = parse(args)?;
    if parsed.probe {
        let report = probe::probe();
        print!("{}", probe::format_report(&report));
        return Ok(Outcome::Probed(probe_ok(&report)));
    }
    app::run(parsed.options()?).map(|()| Outcome::Ran)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn v(items: &[&str]) -> Vec<String> {
        items.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn both_spellings_of_flag_value_are_equal() {
        let a = parse(&v(&["--seconds=7", "--seed=42", "--monitor=eDP-1"])).unwrap();
        let b = parse(&v(&[
            "--seconds",
            "7",
            "--seed",
            "42",
            "--monitor",
            "eDP-1",
        ]))
        .unwrap();
        assert_eq!(a, b);
        assert_eq!(a.seconds, Some(7.0));
        assert_eq!(a.seed, Some(42));
    }

    #[test]
    fn missing_value_is_an_error_not_a_panic() {
        assert!(parse(&v(&["--seconds"])).is_err());
        assert!(parse(&v(&["--nope"])).is_err());
        assert!(parse(&v(&["--seconds", "abc"])).is_err());
    }

    #[test]
    fn seconds_zero_means_forever() {
        let a = parse(&v(&["--test-rect", "--seconds", "0"])).unwrap();
        assert_eq!(a.options().unwrap().seconds, None);
        // 不给 seconds ⇒ 默认 3 秒（live 会话上绝不能无限赖着）
        let b = parse(&v(&["--test-rect"])).unwrap();
        assert_eq!(b.options().unwrap().seconds, Some(3.0));
    }

    #[test]
    fn no_manifest_no_test_rect_is_refused() {
        // 否则会映射一块永远不提交的空 surface
        let a = parse(&v(&[])).unwrap();
        assert!(a.options().is_err());
    }

    #[test]
    fn negative_seconds_is_refused_not_treated_as_forever() {
        // live 会话红线：`--seconds -5` 绝不能静默变成"永远跑"
        let a = parse(&v(&["--test-rect", "--seconds", "-5"])).unwrap();
        assert!(a.options().is_err(), "负数秒必须报错");
        let b = parse(&v(&["--test-rect", "--panel-bottom", "-1"])).unwrap();
        assert!(b.options().is_err(), "负的面板高度没有意义");
        let c = parse(&v(&["--test-rect", "--panel-top", "-1"])).unwrap();
        assert!(c.options().is_err(), "负的栏高没有意义");
    }

    #[test]
    fn panel_top_reaches_the_options() {
        // 本机顶栏 20 逻辑像素：不给这个值 ⇒ 宠物整体下移 20，脚掉到屏幕外
        let a = parse(&v(&["--test-rect", "--panel-top=20"])).unwrap();
        assert_eq!(a.options().unwrap().panel_top, 20.0);
        assert_eq!(
            parse(&v(&["--test-rect"]))
                .unwrap()
                .options()
                .unwrap()
                .panel_top,
            0.0
        );
    }

    #[test]
    fn a_flag_never_swallow_the_next_flag_as_its_value() {
        // `--monitor --probe` 若把 `--probe` 当值吞掉，probe 就不会执行而是起 surface
        assert!(parse(&v(&["--monitor", "--probe"])).is_err());
        assert!(parse(&v(&["--manifest", "--seconds", "3"])).is_err());
        // 但 `=` 写法允许值本身带横杠（负数由 options() 那层拒绝）
        assert_eq!(
            parse(&v(&["--monitor=-L1"])).unwrap().monitor,
            Some("-L1".to_string())
        );
    }
}
