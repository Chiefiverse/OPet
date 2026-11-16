//! §3.12 `opet.sock` 本地控制协议 —— **只有类型与纯函数**（D12/D13 同族，
//! 见 §3.12 placement 段："契约进内核"收窄成"只有类型进内核"）。
//!
//! 这一模块**不碰 socket、不碰墙钟、不碰浮点**：socket 监听与成帧在
//! `overlay-wayland::ipc`（唯一有事件循环的地方），客户端在 `opet-cli`。
//! 拆开的判据 = 可测性：`App` 在没有 compositor 的机器上构造不起来，而方法集
//! 真值表与成帧门禁全都必须能在这台机器上 headless 跑到 —— 所以判定在这里，
//! 接线在那里。
//!
//! 协议一句话：NDJSON 一行一条请求、一行一答、请求 ≤ 8 KiB；
//! `{"method":…,"params":…}` → `{"ok":true,…}` / `{"ok":false,"error":{"code":…,"message":…}}`；
//! **JSON-RPC-lite：无 id、无批量、无通知**（单线程同步处理）。
//!
//! 这里的 [`handle`] 做的是**协议形状**判定（帧长、JSON、字段类型、取值域）；
//! **语义**判定（banner 名、语言码、id 在不在候选表里）故意不在这里 —— 那些事实源
//! 在 petstore/overlay 侧（`petstore::DEFAULT_BANNER`、`l10n::Lang::parse`、
//! `App::candidates`/`App::bodies`），在这里再抄一张表就是第二个事实源。

use serde_json::{Value, json};

/// 一条请求的字节上限（§3.12："请求 ≤ 8 KB"）。按**字节**量，不是字符 ——
/// 成帧层的对端是不可信进程，UTF-8 边界由调用方在读取时处理。
pub const MAX_LINE_BYTES: usize = 8 * 1024;

/// 方法白名单 v1（§3.12 方法集表）。**这张表是门禁而不是提示**：不在表里的
/// method 一律 `unknown_method`，将来加方法 = 改这张表 + 改 `handle` + 过真值表。
pub const METHODS: [&str; 8] = [
    "pet.status",
    "pet.stop",
    "pet.pause",
    "pet.resume",
    "pet.pull",
    "pet.swap",
    "pet.equip",
    "lang.set",
];

/// 错误码。协议层五个在这里产生；运行时两个只在这里**登记**（服务端产生），
/// 客户端与 widget 按码分支、不按 message 文案分支。
pub mod code {
    /// 单条请求超过 [`super::MAX_LINE_BYTES`]。
    pub const OVERSIZE: &str = "oversize";
    /// 不成 JSON。
    pub const PARSE_ERROR: &str = "parse_error";
    /// 是 JSON 但形状不对（顶层非对象、`method` 缺失或不是字符串）。
    pub const BAD_REQUEST: &str = "bad_request";
    /// `method` 合法但不在 [`super::METHODS`]。
    pub const UNKNOWN_METHOD: &str = "unknown_method";
    /// method 认得，但 `params` 形状不对（非对象、缺字段、类型错、取值域外）。
    pub const BAD_PARAMS: &str = "bad_params";
    /// 服务端运行时：这个请求在此刻做不了（无库 ⇒ 抽取、id 不在候选表、语言码不认识）。
    pub const UNAVAILABLE: &str = "unavailable";
    /// 服务端运行时：本该能做的事失败了（落库炸了之类）。message 带一句人读的原因。
    pub const INTERNAL: &str = "internal";
}

/// `handle` 的产出 = 服务端要施加的效果，或一条**可以直接发回去**的应答
/// （协议层拒绝全走 [`Effect::Reply`]，成功路径走动作臂、由 App 应用后再应答）。
///
/// 七个动作臂与 §3.12 方法集表逐行对应；每个动作臂都只产 **intent**，
/// 应用点仍在既有单入口函数（`swap_body` / `pick_and_wear` / `do_pull` / …）——
/// 这是"IPC 不授予任何 UI 点击之外的能力"那句话在代码里的形状。
#[derive(Clone, Debug, PartialEq)]
pub enum Effect {
    /// 纯读：pid / tick / body id / equipped 行 / locale / 是否带 `--db`。
    /// 数据由服务端从现成分段拼出（`ok_reply(Some(...))`）。
    Status,
    /// 优雅退出 = `--seconds` 到点同一条路径（`signal.stop()` ⇒ `report()` 照跑）。
    Stop,
    /// 与菜单"暂停"行同语义（`Command::Go(Sleep)` 形状，客户端不预判内核响应）。
    Pause,
    /// 与菜单"暂停"行同语义的反向；唤醒仍由内核 `handle_go` 从 Sleep 回 Idle。
    Resume,
    /// 与菜单点击抽取同一条同步路径（D14）。`count` 只许 1 或 10（单抽/十连两档，
    /// 与菜单一致 ⇒ 没有"百连"这种新能力）。`banner` 缺省 = 服务端默认池。
    Pull { banner: Option<String>, count: u64 },
    /// 换本体 = P5 九步，唯一入口 `App::swap_body`。
    Swap { id: String },
    /// 穿上 = P2/P3 路径（"能穿 ⊆ 抽到"门禁照过）。`slot` 在协议层就按
    /// [`crate::gacha::catalog::Slot`] 的六值校验 —— 那是 petcore 自己的事实源。
    Equip {
        slot: crate::gacha::catalog::Slot,
        id: String,
    },
    /// 与菜单语言行同路径（改本进程 + `write_ui_key` 写回）。四码校验在
    /// `l10n::Lang::parse`（服务端），这里只保证"是个非空字符串"。
    SetLang { code: String },
    /// 一条不必经过 App 的应答（协议层拒绝，或 Status 的成功数据）。
    Reply(Value),
}

/// 一行请求 → 一个效果。纯函数：不读环境、不看时钟、不产生副作用。
///
/// 宽容点（都是设计里明写的）：`params` 整个缺省 = 空对象；未知**额外**字段忽略；
/// 断开时机宽容由成帧层负责，不在这里。
pub fn handle(line: &str) -> Effect {
    if line.len() > MAX_LINE_BYTES {
        return reject(code::OVERSIZE, "请求超过 8 KiB 上限");
    }
    let obj: Value = match serde_json::from_str(line) {
        Ok(v) => v,
        Err(_) => return reject(code::PARSE_ERROR, "请求不是合法 JSON"),
    };
    let Some(map) = obj.as_object() else {
        return reject(code::BAD_REQUEST, "请求顶层必须是 JSON 对象");
    };
    let Some(method) = map.get("method").and_then(Value::as_str) else {
        return reject(code::BAD_REQUEST, "缺 method（必须是非空字符串）");
    };
    if !METHODS.contains(&method) {
        return reject(
            code::UNKNOWN_METHOD,
            format!(
                "不认识的方法 {method:?}（v1 白名单：{}）",
                METHODS.join(", ")
            ),
        );
    }
    let empty = serde_json::Map::new();
    let params = match map.get("params") {
        None => &empty,
        Some(Value::Object(p)) => p,
        Some(_) => return reject(code::BAD_PARAMS, "params 必须是 JSON 对象"),
    };
    match method {
        "pet.status" => Effect::Status,
        "pet.stop" => Effect::Stop,
        "pet.pause" => Effect::Pause,
        "pet.resume" => Effect::Resume,
        "pet.pull" => match (pull_count(params), field(params, "banner")) {
            (None, _) => reject(
                code::BAD_PARAMS,
                "pull.count 只能是 1 或 10（与菜单两档一致）",
            ),
            (Some(_), Field::Bad) => reject(code::BAD_PARAMS, "pull.banner 必须是非空字符串"),
            (Some(count), Field::Absent) => Effect::Pull {
                banner: None,
                count,
            },
            (Some(count), Field::Val(banner)) => Effect::Pull {
                banner: Some(banner.to_string()),
                count,
            },
        },
        "pet.swap" => match field(params, "id") {
            Field::Val(id) => Effect::Swap { id: id.to_string() },
            _ => reject(code::BAD_PARAMS, "swap.id 必须是非空字符串"),
        },
        "pet.equip" => match (field(params, "slot"), field(params, "id")) {
            (Field::Val(slot_raw), Field::Val(id)) => match parse_slot(slot_raw) {
                Some(slot) => Effect::Equip {
                    slot,
                    id: id.to_string(),
                },
                None => reject(
                    code::BAD_PARAMS,
                    format!("equip.slot {slot_raw:?} 不在槽位六值里"),
                ),
            },
            _ => reject(
                code::BAD_PARAMS,
                "equip.slot 与 equip.id 都必须是非空字符串",
            ),
        },
        "lang.set" => match field(params, "code") {
            Field::Val(c) => Effect::SetLang {
                code: c.to_string(),
            },
            _ => reject(code::BAD_PARAMS, "lang.set.code 必须是非空字符串"),
        },
        // METHODS 与上面的 match 同源；走到这里说明加方法没加臂 —— 那正是
        // 真值表里 `every_advertised_method_is_an_action_arm` 要红的地方。
        other => reject(
            code::UNKNOWN_METHOD,
            format!("方法 {other:?} 在表里但没有实现臂"),
        ),
    }
}

/// `{"ok":true}` / `{"ok":true,"data":…}`。
pub fn ok_reply(data: Option<Value>) -> Value {
    match data {
        None => json!({ "ok": true }),
        Some(d) => json!({ "ok": true, "data": d }),
    }
}

/// `{"ok":false,"error":{"code":…,"message":…}}`（§3.12 形状逐字）。
pub fn err_reply(code: &str, message: impl Into<String>) -> Value {
    json!({ "ok": false, "error": { "code": code, "message": message.into() } })
}

/// 应答 → 线上的一行（含结尾换行）。成帧层只负责写它。
pub fn encode(reply: &Value) -> String {
    let mut s = reply.to_string();
    s.push('\n');
    s
}

fn reject(code: &str, message: impl Into<String>) -> Effect {
    Effect::Reply(err_reply(code, message))
}

/// `count`：缺省 = 1（单抽，最保守的一档）；给了就必须是整数 1 或 10。
/// 刻意不收 f64：`serde_json` 把 `1.0` 也报成 number，而 `as_u64` 只认整数 ——
/// JSON 里写 `1.0` 的客户端是在要求一个小数档，那不在两档语义里。
fn pull_count(params: &serde_json::Map<String, Value>) -> Option<u64> {
    match params.get("count") {
        None => Some(1),
        Some(v) => match v.as_u64() {
            Some(n @ (1 | 10)) => Some(n),
            _ => None,
        },
    }
}

/// 一个字符串字段的三态判定，两个调用方共用（必填位把 `Absent` 也当 `Bad`）。
enum Field<'a> {
    /// 字段整个没给。
    Absent,
    /// 给了，但不是非空字符串。
    Bad,
    Val(&'a str),
}

fn field<'a>(params: &'a serde_json::Map<String, Value>, key: &str) -> Field<'a> {
    match params.get(key) {
        None | Some(Value::Null) => Field::Absent,
        Some(Value::String(s)) if !s.is_empty() => Field::Val(s),
        Some(_) => Field::Bad,
    }
}

fn parse_slot(raw: &str) -> Option<crate::gacha::catalog::Slot> {
    crate::gacha::catalog::Slot::ALL
        .into_iter()
        .find(|s| s.as_str() == raw)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::gacha::catalog::Slot;

    /// 每个在表白名单 method 都必须落成**动作臂**，不是 `Reply`。
    /// 加方法忘加臂 ⇒ `handle` 落到 match 的 `other` 臂 ⇒ 这里红。
    #[test]
    fn every_advertised_method_is_an_action_arm() {
        for m in METHODS {
            let eff = handle(&json!({ "method": m, "params": {} }).to_string());
            // pull/swap/equip/lang.set 缺必填参数会被拒成 Reply —— 那是形状门禁在
            // 白名单门禁**之后**，所以这里用各自的合法 params 再验一次。
            let eff = match m {
                "pet.pull" => handle(r#"{"method":"pet.pull","params":{"count":1}}"#),
                "pet.swap" => handle(r#"{"method":"pet.swap","params":{"id":"spacecat"}}"#),
                "pet.equip" => {
                    handle(r#"{"method":"pet.equip","params":{"slot":"head","id":"ballcap"}}"#)
                }
                "lang.set" => handle(r#"{"method":"lang.set","params":{"code":"ja"}}"#),
                _ => eff,
            };
            assert!(
                !matches!(eff, Effect::Reply(_)),
                "{m} 落成了 Reply ⇒ 表里有条目但 match 没有臂：{eff:?}"
            );
        }
    }

    /// 方法集真值表（`plan_pick_wear` / `plan_body_override` 同族形状）。
    #[test]
    fn valid_requests_map_to_exact_effects() {
        let cases: [(&str, Effect); 10] = [
            (r#"{"method":"pet.status"}"#, Effect::Status),
            (r#"{"method":"pet.stop","params":{}}"#, Effect::Stop),
            (r#"{"method":"pet.pause","params":{}}"#, Effect::Pause),
            (r#"{"method":"pet.resume","params":{}}"#, Effect::Resume),
            // count 缺省 = 单抽那一档（最保守），params 整个缺省也走同一臂。
            (
                r#"{"method":"pet.pull"}"#,
                Effect::Pull {
                    banner: None,
                    count: 1,
                },
            ),
            (
                r#"{"method":"pet.pull","params":{"count":10}}"#,
                Effect::Pull {
                    banner: None,
                    count: 10,
                },
            ),
            (
                r#"{"method":"pet.pull","params":{"banner":"cat_series","count":1}}"#,
                Effect::Pull {
                    banner: Some("cat_series".into()),
                    count: 1,
                },
            ),
            (
                r#"{"method":"pet.swap","params":{"id":"dinosaur"}}"#,
                Effect::Swap {
                    id: "dinosaur".into(),
                },
            ),
            (
                r#"{"method":"pet.equip","params":{"slot":"hand","id":"flag"}}"#,
                Effect::Equip {
                    slot: Slot::Hand,
                    id: "flag".into(),
                },
            ),
            (
                r#"{"method":"lang.set","params":{"code":"ja"}}"#,
                Effect::SetLang { code: "ja".into() },
            ),
        ];
        for (line, want) in cases {
            assert_eq!(handle(line), want, "行：{line}");
        }
    }

    /// 协议层拒绝逐条点名错误码；每条都必须是 `Effect::Reply`，message 供人读。
    #[test]
    fn protocol_rejections_carry_the_right_code() {
        let mut cases: Vec<(&str, &str)> = vec![
            (r#"{"method""pet.status"}"#, code::PARSE_ERROR),
            (r#"[1,2,3]"#, code::BAD_REQUEST),
            (r#"42"#, code::BAD_REQUEST),
            (r#"{}"#, code::BAD_REQUEST),
            (r#"{"method":42}"#, code::BAD_REQUEST),
            (r#"{"method":"pet.teleport"}"#, code::UNKNOWN_METHOD),
            (r#"{"method":"pet.status","params":[]}"#, code::BAD_PARAMS),
            (
                r#"{"method":"pet.pull","params":{"count":5}}"#,
                code::BAD_PARAMS,
            ),
            (
                r#"{"method":"pet.pull","params":{"count":1.0}}"#,
                code::BAD_PARAMS,
            ),
            (
                r#"{"method":"pet.swap","params":{"id":""}}"#,
                code::BAD_PARAMS,
            ),
            (
                r#"{"method":"pet.equip","params":{"slot":"tail","id":"x"}}"#,
                code::BAD_PARAMS,
            ),
            (
                r#"{"method":"lang.set","params":{"code":null}}"#,
                code::BAD_PARAMS,
            ),
        ];
        let oversize = format!(
            r#"{{"method":"pet.status","params":{{"pad":"{}"}}}}"#,
            "x".repeat(MAX_LINE_BYTES)
        );
        assert!(oversize.len() > MAX_LINE_BYTES);
        cases.insert(0, (&oversize, code::OVERSIZE));
        for (line, want_code) in cases {
            let Effect::Reply(reply) = handle(line) else {
                panic!("该行应被协议层拒掉：{line}");
            };
            assert_eq!(reply["ok"], json!(false), "行：{line}");
            assert_eq!(
                reply["error"]["code"].as_str(),
                Some(want_code),
                "行：{line} 给了 {:?}",
                reply["error"]
            );
            assert!(
                reply["error"]["message"]
                    .as_str()
                    .is_some_and(|m| !m.is_empty()),
                "错误必须带一句人读的 message：{line}"
            );
        }
    }

    /// 宽容面是设计里明写的，所以也要有读数：额外字段忽略、null banner = 缺省。
    /// 不钉住的话，"顺手改成严格校验"就是一次无声的协议换代。
    #[test]
    fn extra_fields_and_null_banner_are_tolerated() {
        assert_eq!(
            handle(r#"{"method":"pet.stop","params":{"junk":1},"id":7}"#),
            Effect::Stop
        );
        assert_eq!(
            handle(r#"{"method":"pet.pull","params":{"banner":null,"count":10}}"#),
            Effect::Pull {
                banner: None,
                count: 10
            }
        );
    }

    /// §3.12 逐字给过应答形状，这两条把它钉住（示例即测试素材）。
    #[test]
    fn reply_shapes_are_the_spec_verbatim() {
        assert_eq!(
            serde_json::to_string(&ok_reply(None)).unwrap(),
            r#"{"ok":true}"#
        );
        let err = err_reply(code::UNKNOWN_METHOD, "no such thing");
        assert_eq!(err["ok"], json!(false));
        assert_eq!(err["error"]["code"], json!("unknown_method"));
        assert_eq!(err["error"]["message"], json!("no such thing"));
        assert_eq!(
            ok_reply(Some(json!({"pid": 12}))),
            json!({"ok": true, "data": {"pid": 12}})
        );
    }

    /// 应答**恰好一行**：NDJSON 的成帧契约在 encode 这一处，多一个换行就是一条
    /// 对端解不出的假消息。
    #[test]
    fn encode_produces_exactly_one_line() {
        for reply in [
            ok_reply(None),
            ok_reply(Some(json!({"ticks": 3600, "body": "spacecat"}))),
            err_reply(code::OVERSIZE, "请求超过 8 KiB 上限"),
        ] {
            let line = encode(&reply);
            assert!(line.ends_with('\n'), "行尾必须有换行：{line:?}");
            assert_eq!(
                line.matches('\n').count(),
                1,
                "应答必须恰好一行，内嵌 message 不许带换行：{line:?}"
            );
            // 且它自己必须是合法 JSON 对象（对端同样按 NDJSON 解它）。
            let back: Value = serde_json::from_str(line.trim_end()).expect("应答要可被解回");
            assert!(back.get("ok").is_some());
        }
    }

    /// message 里带上换行（比如对端塞了恶意文本进 method 名）会把一答拆成两帧
    /// ⇒ `encode` 前必须没有裸换行。err_reply 走 serde 字符串转义，天然安全，
    /// 这里钉住这个性质。
    #[test]
    fn hostile_method_name_cannot_inject_a_second_frame() {
        let evil = "{\"method\":\"pe\\nt.stop\"}";
        let Effect::Reply(reply) = handle(evil) else {
            panic!("坏 method 必须被拒");
        };
        let line = encode(&reply);
        assert_eq!(line.matches('\n').count(), 1);
    }
}
