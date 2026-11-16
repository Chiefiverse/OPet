//! `opet ctl` —— §3.12 控制套接字的**客户端**（插件的 `run.sh` 与人都用它）。
//!
//! 形状照设计书 §10.2：一条请求一行应答，**200 ms 超时 ⇒ 退出码 3**（"连不上在跑的实例"），
//! 应答原文进 stdout（脚本自己 `jq`，不在这里发明第二套解析口径），而 `ok` 字段同时决定
//! 退出码（0/1）⇒ `opet ctl pet.stop; echo $?` 这类用法不需要读 JSON 也能判。
//!
//! 三条刻意的"不做"：
//! · **不抄白名单**：方法集直接读 [`petcore::ipc::METHODS`]（第五处抄表迟早变成谎话）；
//! · **不重试、不排队**：宠物活着就一条，不活就退出码 3，控制面没有"稍后再说"的语义；
//! · **不碰 `--ipc-path` 那一档的默认推导**：路径的"谁赢"逻辑在
//!   [`overlay_wayland::ipc::resolve_path`] 一份，客户端与服务端读同一个函数 = 同一个口径，
//!   漂移在这一格被构造掉（`settings.rs` 模块头同一手法）。

use std::io::{Read, Write};
use std::time::Duration;

/// 请求编码 + 参数校验。`params_json` = 用户给的原始 JSON（可缺省 ⇒ `{}`）。
///
/// 坏 JSON 在**本地**就拒（连都不连）：服务端会回 `parse_error`，但把一条明知发出去
/// 必被拒的请求放上本机 socket，只是把"你少写了个引号"变成"协议错"级别的噪音。
pub fn build_request(method: &str, params_json: Option<&str>) -> Result<String, String> {
    if !petcore::ipc::METHODS.contains(&method) {
        return Err(format!(
            "ctl 只认白名单里的方法: {}（大小写敏感，`pet.` 前缀不是可省的装饰）",
            petcore::ipc::METHODS.join(" / ")
        ));
    }
    let params = match params_json {
        None => serde_json::json!({}),
        Some(raw) => serde_json::from_str(raw).map_err(|e| {
            format!("params 不是合法 JSON: {e}（应当是对象，如 '{{\"count\":10}}'）")
        })?,
    };
    // 与 `petcore::ipc::encode` 同一条契约：一行、\n 结尾（这里不 import 它是因为它收
    // 的是应答 Value，请求侧手拼同样只有一处，不构成第二个事实源）。
    let line = serde_json::json!({ "method": method, "params": params }).to_string();
    if line.len() > petcore::ipc::MAX_LINE_BYTES {
        return Err(format!(
            "请求 {} B 超过服务端上限 {} B ⇒ 不发送",
            line.len(),
            petcore::ipc::MAX_LINE_BYTES
        ));
    }
    Ok(format!("{line}\n"))
}

/// 应答分类：stdout 永远打原文，这里只决定**退出码**。
/// 非 JSON / 没有 `ok` 字段都算协议违例（1），但原文照打 —— 排查的人要的就是那行原文。
pub fn classify(reply: &str) -> u8 {
    match serde_json::from_str::<serde_json::Value>(reply) {
        Ok(v) => match v.get("ok").and_then(|ok| ok.as_bool()) {
            Some(true) => 0,
            Some(false) => 1,
            None => 1,
        },
        Err(_) => 1,
    }
}

/// 默认超时（设计书 §10.2 的口径；Phase B 第 6 步脚本读的也是这个数）。
pub const DEFAULT_TIMEOUT_MS: u64 = 200;

#[derive(Debug)]
pub enum CtlError {
    /// 连不上 ⇒ 退出码 3（"没有在跑的实例"这一档，与"宠物答了 but 拒绝"的 1 分开）
    Unreachable(String),
    /// 用法错 / 参数错 ⇒ 退出码 2（还没上 socket）
    Usage(String),
}

/// 解析 `opet ctl` 的参数：`[--socket PATH] [--timeout MS] <method> [json-params]`。
/// 位置参数只有两个，所以手写，不进 [`overlay_wayland::cli`] 那套（那套是 overlay 的）。
pub fn parse(args: &[String]) -> Result<(String, Option<String>, String, Duration), CtlError> {
    let mut socket: Option<String> = None;
    let mut timeout_ms: Option<u64> = None;
    let mut positional: Vec<String> = Vec::new();
    let mut i = 0;
    while i < args.len() {
        let (key, inline) = match args[i].split_once('=') {
            Some((k, v)) => (k, Some(v.to_string())),
            None => (args[i].as_str(), None),
        };
        let next_val = |i: &mut usize| -> Result<String, CtlError> {
            if let Some(v) = inline.clone() {
                return Ok(v);
            }
            *i += 1;
            let v = args
                .get(*i)
                .cloned()
                .ok_or_else(|| CtlError::Usage(format!("{key} 缺少参数值")))?;
            if v.starts_with('-') && v.parse::<f64>().is_err() {
                return Err(CtlError::Usage(format!(
                    "{key} 缺少参数值（把 {v} 当成了值）"
                )));
            }
            Ok(v)
        };
        match key {
            "--socket" => socket = Some(next_val(&mut i)?),
            "--timeout" => {
                let raw = next_val(&mut i)?;
                timeout_ms = Some(
                    raw.trim()
                        .parse::<u64>()
                        .map_err(|e| CtlError::Usage(format!("--timeout 不是非负整数: {e}")))?,
                );
            }
            other if other.starts_with('-') => {
                return Err(CtlError::Usage(format!("未知选项: {other}")));
            }
            other => positional.push(other.to_string()),
        }
        i += 1;
    }
    if positional.is_empty() {
        return Err(CtlError::Usage(
            "ctl 需要一个方法名（白名单见 --usage）".to_string(),
        ));
    }
    if positional.len() > 2 {
        return Err(CtlError::Usage(
            "方法名之后最多一个 params（JSON 是一整个参数，别拆）".to_string(),
        ));
    }
    // 空 socket / 空 timeout=0 都是 `--db=` 那一族：给一个"看起来开了档但其实没档"的值，
    // 必须出声而不是猜。（timeout 0 = 永不超时，那是 ctl 最不该有的默认。）
    if socket.as_deref().is_some_and(|p| p.trim().is_empty()) {
        return Err(CtlError::Usage("--socket 给了空路径".to_string()));
    }
    if timeout_ms == Some(0) {
        return Err(CtlError::Usage(
            "--timeout 0 = 永不超时；ctl 是去问活人的，不接受赖着等".to_string(),
        ));
    }
    let path = overlay_wayland::ipc::resolve_path(socket.as_deref())
        .map_err(CtlError::Usage)?
        .to_string_lossy()
        .into_owned();
    Ok((
        positional[0].clone(),
        positional.get(1).cloned(),
        path,
        Duration::from_millis(timeout_ms.unwrap_or(DEFAULT_TIMEOUT_MS)),
    ))
}

/// 一次问答。返回 `(应答原文, 退出码)`；连不上时给 `CtlError::Unreachable` ⇒ 调用方打 3。
pub fn talk(args: &[String]) -> Result<(String, u8), CtlError> {
    let (method, params, path, timeout) = parse(args)?;
    let line = build_request(&method, params.as_deref()).map_err(CtlError::Usage)?;
    let mut stream = std::os::unix::net::UnixStream::connect(&path).map_err(|e| {
        // ENOENT（文件不在）与 ECONNREFUSED（文件在而没人听 = 上一世残留）都是"没有在跑的
        // 带 --ipc 的宠物"，分两类说反而让用户去找不存在的差异；错误原文带上。
        CtlError::Unreachable(format!("{path}: {e}"))
    })?;
    // 写侧同样限时（非阻塞语义在客户端侧的等价物）：对端卡住时 3 而不是永远挂。
    stream
        .set_write_timeout(Some(timeout))
        .map_err(|e| CtlError::Unreachable(format!("{path}: {e}")))?;
    stream
        .set_read_timeout(Some(timeout))
        .map_err(|e| CtlError::Unreachable(format!("{path}: {e}")))?;
    stream
        .write_all(line.as_bytes())
        .map_err(|e| CtlError::Unreachable(format!("{path}: 写请求失败: {e}")))?;
    // 读一行：逐字节到 '\n'。应答 ≤ ~200 B，没有缓冲策略可做；8 KiB 封顶读满还没换行
    // 就按协议违例收（服务端本来也不该回这种东西，这是客户端侧的自保）。
    let mut buf: Vec<u8> = Vec::new();
    let mut byte = [0u8; 1];
    loop {
        match stream.read(&mut byte) {
            Ok(0) => {
                return Err(CtlError::Unreachable(format!(
                    "{path}: 对端在给出应答前关掉了连接"
                )));
            }
            Ok(_) => {
                buf.push(byte[0]);
                if byte[0] == b'\n' || buf.len() > petcore::ipc::MAX_LINE_BYTES {
                    break;
                }
            }
            Err(e)
                if e.kind() == std::io::ErrorKind::WouldBlock
                    || e.kind() == std::io::ErrorKind::TimedOut =>
            {
                return Err(CtlError::Unreachable(format!(
                    "{path}: {} ms 内没有应答（超时）",
                    timeout.as_millis()
                )));
            }
            Err(e) => return Err(CtlError::Unreachable(format!("{path}: {e}"))),
        }
    }
    let reply = String::from_utf8_lossy(&buf).trim_end().to_string();
    let code = classify(&reply);
    Ok((reply, code))
}

pub const USAGE: &str = "\
opet ctl [--socket PATH] [--timeout MS] <method> [json-params]

  给在跑的宠物（overlay 带 --ipc 起的那个）发一条控制请求；一行一答。
  method 白名单：pet.status / pet.stop / pet.pause / pet.resume /
                pet.pull / pet.swap / pet.equip / lang.set
  params 是一个 JSON 对象字符串，可缺省：'{\"count\":10}'、'{\"id\":\"tiger\"}' …

  --socket PATH    套接字路径（默认 $XDG_RUNTIME_DIR/opet.sock；缺该变量时必须显式给）
  --timeout MS     读写超时毫秒（默认 200；0 被拒绝 —— 问活人不许赖着等）

退出码：0 = ok:true ｜ 1 = ok:false（宠物答了而拒绝）或应答不合协议
        2 = 用法/参数错（还没上 socket）
        3 = 连不上在跑的实例（设计书 §10.2：插件 widget 读这一档决定显示什么）

示例：
  opet ctl pet.status
  opet ctl pet.pull '{\"count\": 10}'
  opet ctl --timeout 3000 pet.equip '{\"slot\": \"head\", \"id\": \"cowboy_hat\"}'
  opet ctl --socket /tmp/dev.sock lang.set '{\"code\": \"ja\"}'

超时怎么给：
  默认 200 ms 是 §10.2 给插件 widget 探活（pet.status）的口径。
  pet.swap 与 pet.equip 在服务端**同步解 45 MiB 图集**（实测 113–260 ms），
  200 ms 会把「在跑但慢」误报成「没在跑」（退出码 3 而动作其实成了）⇒
  这两条请显式带 --timeout 3000。
";

#[cfg(test)]
mod tests {
    use super::*;

    fn v(items: &[&str]) -> Vec<String> {
        items.iter().map(|s| s.to_string()).collect()
    }

    /// 白名单**不抄第二遍**：客户端直接读 petcore 那张表，这里断言的是"读得到、拼得出请求"。
    #[test]
    fn request_shape_is_one_json_line_with_params_defaulting_to_empty_object() {
        let got = build_request("pet.status", None).unwrap();
        assert_eq!(got, "{\"method\":\"pet.status\",\"params\":{}}\n");
        let got = build_request("pet.pull", Some(r#"{"count":10}"#)).unwrap();
        assert!(
            got.starts_with("{\"method\":\"pet.pull\"") && got.trim_end().ends_with('}'),
            "{got}"
        );
        assert_eq!(got.matches('\n').count(), 1, "请求必须恰好一行");
    }

    #[test]
    fn client_side_rejections_never_reach_the_socket() {
        // 方法不在白名单 / params 坏 JSON / 超上限 ⇒ 本地 Usage（还没 connect 就返回）
        assert!(build_request("pet.teleport", None).is_err());
        assert!(build_request("pet.status", Some("{count:")).is_err());
        let big = format!("{{\"id\":\"{}\"}}", "x".repeat(9000));
        assert!(build_request("pet.swap", Some(&big)).is_err());
        // 报错要点名"白名单"三选一之外的什么形状都不算（把 pet.status 写成 Pet.Status 的人
        // 值得看到整张表，而不是一个 unknown_method）
        let e = build_request("Pet.Status", None).unwrap_err();
        assert!(e.contains("pet.status") && e.contains("白名单"), "{e}");
    }

    /// 退出码的三档在**应答文本**上的真值表。stdout 永远打原文，这里只数判定面。
    #[test]
    fn exit_codes_are_ok_true_zero_and_everything_else_one() {
        assert_eq!(classify(r#"{"ok":true,"data":{"pid":1}}"#), 0);
        assert_eq!(classify(r#"{"ok":true}"#), 0);
        assert_eq!(
            classify(r#"{"ok":false,"error":{"code":"unknown_method"}}"#),
            1
        );
        assert_eq!(classify("not json at all"), 1);
        assert_eq!(classify("{}"), 1, "没有 ok 字段的裸对象不该被读成应答");
        assert_eq!(classify(r#"{"ok":"yes"}"#), 1, "ok 必须是布尔");
    }

    #[test]
    fn parse_flags_positionals_and_two_lies() {
        let (m, p, _sock, to) = parse(&v(&["pet.swap", r#"{"id":"x"}"#])).unwrap();
        assert_eq!(m, "pet.swap");
        assert_eq!(p.as_deref(), Some(r#"{"id":"x"}"#));
        assert_eq!(to, Duration::from_millis(DEFAULT_TIMEOUT_MS));
        // --socket 显式给 ⇒ 原样；这是测试注入点，真机默认走 XDG_RUNTIME_DIR
        let (_, _, sock, _) = parse(&v(&["--socket", "/tmp/t.sock", "pet.status"])).unwrap();
        assert_eq!(sock, "/tmp/t.sock");
        let (_, _, _, to) = parse(&v(&["--timeout", "50", "pet.status"])).unwrap();
        assert_eq!(to, Duration::from_millis(50));
        // 空 socket / timeout 0 / 方法缺失 / params 拆成两段 ⇒ 全部 Usage
        assert!(matches!(
            parse(&v(&["--socket=", "pet.status"])),
            Err(CtlError::Usage(_))
        ));
        assert!(matches!(
            parse(&v(&["--timeout", "0", "pet.status"])),
            Err(CtlError::Usage(_))
        ));
        assert!(matches!(parse(&v(&[])), Err(CtlError::Usage(_))));
        assert!(matches!(
            parse(&v(&["pet.pull", "{\"count\":", "10}"])),
            Err(CtlError::Usage(_))
        ));
        // 值长得像 flag 的吞并（overlay 那套的同一规矩）
        assert!(matches!(
            parse(&v(&["--socket", "--timeout", "50", "pet.status"])),
            Err(CtlError::Usage(_))
        ));
    }
}
