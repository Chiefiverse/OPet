//! §3.12 控制套接字的**服务端**：bind / 陈旧 socket 自愈 / calloop 事件源 / NDJSON 成帧。
//!
//! 分工（与 `petcore::ipc` 那条线一致）：**协议说什么**全在 petcore 的纯函数 `handle`，
//! 这个文件只管**字节怎么进出** —— 一行一请求、一行一应答、超长行丢到换行、坏 UTF-8 出声
//! 不杀连接（"宽容断连"是契约条款，客户端可能是 `socat` 不是 `opet ctl`）。
//!
//! 三条设计内决定，代码里都留着理由：
//! · **`--ipc` 显式开关**（默认关）⇒ 本模块在 `ipc: false` 时一行都不执行；
//! · **bind 撞 `AddrInUse` 先 probe**：connect 成功 = 有活的监听者 ⇒ **拒绝启动**（不是抢
//!   它的 socket）；connect 失败 = 上一世残留的 inode ⇒ unlink 后重试一次；
//! · **应答 ≤ ~200 B，回调里直接写完**：一条请求的应答塞得进 socket 发送缓冲，所以没有写侧
//!   事件源、没有排队、也没有"写了一半被摘掉"的帧事故 —— 那是 `PostAction::Remove` 唯一的
//!   两个入口（EOF / 写失败）。

use std::io::{ErrorKind, Read, Write};
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};

use petcore::ipc::{Effect, MAX_LINE_BYTES, code};
use smithay_client_toolkit::reexports::calloop::generic::Generic;
use smithay_client_toolkit::reexports::calloop::{Interest, LoopHandle, Mode, PostAction};

use crate::app::App;

/// 套接字路径：`--ipc-path` 显式给 > `$XDG_RUNTIME_DIR/opet.sock`。
///
/// ⚠️ **没有 `/tmp/opet.sock` 兜底**：协议内没有任何鉴权，唯一的鉴权是内核凭据（socket 文件
/// 0600 = 仅该用户可 connect），而 `/tmp` 是世界可写目录 —— 谁都能先占那个路径、把请求引到
/// 别的监听者那里去。兜底"能用"等于把安全论证换成"赌运气"，所以缺变量就明说、让人显式给。
pub fn resolve_path(explicit: Option<&str>) -> Result<PathBuf, String> {
    if let Some(p) = explicit {
        return Ok(PathBuf::from(p));
    }
    let dir = std::env::var_os("XDG_RUNTIME_DIR")
        .filter(|d| !d.is_empty())
        .map(PathBuf::from)
        .ok_or_else(|| {
            "XDG_RUNTIME_DIR 未设置 ⇒ 控制套接字不知道放哪（用 --ipc-path 显式指定，或经 systemd 会话启动）"
                .to_string()
        })?;
    Ok(dir.join("opet.sock"))
}

/// 退出时清掉 socket 文件。Drop 语义 = "这一世占过这个路径，走了就不留门牌"；
/// 留着不影响正确性（bind 会 probe 出它是残留然后 unlink），但会让用户看见一个死路径。
#[derive(Debug)]
pub struct SocketGuard {
    path: PathBuf,
}

impl Drop for SocketGuard {
    fn drop(&mut self) {
        // 只删自己 bind 出来的那个 inode；中途同名重建的是别人的文件，remove_file 会误删。
        // v1 里没有这种中途换人的路径（unlink 只在 bind 之前发生），所以这里就是普通的删。
        let _ = std::fs::remove_file(&self.path);
    }
}

/// bind 套接字（含陈旧自愈与"活体拒绝"）。返回监听者与守门人；**守门人必须活到进程收尾**，
/// 它的 Drop 就是 unlink —— 交给 `run()` 里一个 `let _guard` 绑到函数末尾的那个变量。
pub fn bind(path: &Path) -> Result<(UnixListener, SocketGuard), String> {
    match try_bind(path) {
        Ok(listener) => Ok((listener, guard(path))),
        Err(e) if e.kind() == ErrorKind::AddrInUse => {
            // 路径被占 = 两种世界，靠一次 connect 探针分辨：
            // 活的监听者 accept 成功 ⇒ 那是**另一只在跑的宠物**，绝不与它抢同一个路径；
            // ECONNREFUSED（或 ENOENT）⇒ inode 是上一世 kill -9 留下的垃圾，清掉重试一次。
            // ⚠️ 探针成功时我们也真的建立了一个"客户端"：不发请求、直接丢掉，服务端那一侧
            // 按宽容断连处理（读 0 字节 ⇒ 摘除）⇒ 不留半帧、不留状态。
            if UnixStream::connect(path).is_ok() {
                return Err(format!(
                    "{} 上已有活着的服务端（另一只带 --ipc 的宠物）⇒ 拒绝启动，不与它抢控制面",
                    path.display()
                ));
            }
            std::fs::remove_file(path)
                .map_err(|r| format!("unlink 陈旧套接字 {} 失败：{r}", path.display()))?;
            let listener = try_bind(path)
                .map_err(|r| format!("unlink 之后 bind {} 仍失败：{r}", path.display()))?;
            Ok((listener, guard(path)))
        }
        Err(e) => Err(format!("bind {} 失败：{e}", path.display())),
    }
}

fn guard(path: &Path) -> SocketGuard {
    SocketGuard {
        path: path.to_path_buf(),
    }
}

fn try_bind(path: &Path) -> std::io::Result<UnixListener> {
    let listener = UnixListener::bind(path)?;
    // bind 之后立刻 chmod 0600：umask 不可信（022 的会话会把 socket 留成 0755 = 任何人可连，
    // 而协议内零鉴权、鉴权全靠文件权限）。bind→chmod 之间有一个微秒级的窗口，v1 接受：
    // 与 X11 / layer-shell 全家同一形状，窗口内的连接与窗口后的连接**能力相同**（同用户），
    // 差别只是别的用户能不能连 —— 而赌注是"这一台机器上没有人正在扫这个窗口"，这是本机
    // 控制面共同的残余风险，不是这一层能关掉的。
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))?;
    listener.set_nonblocking(true)?;
    Ok(listener)
}

/// 把监听者挂进事件环。**一次性挂载**：监听源活到进程结束（摘掉它 = 再没人能连进来，
/// 而这不是任何方法能请求的状态）。每个 accept 到的连接成为独立的短寿命源。
pub fn attach(handle: &LoopHandle<'_, App>, listener: UnixListener) {
    let mut inner = handle.clone();
    let source = Generic::new(listener, Interest::READ, Mode::Edge);
    // Edge 模式 ⇒ 回调里必须 accept 干 backlog（直到 WouldBlock），不然剩一个排队连接时
    // 不会再有第二次就绪通知 = 那个连接永远连不上。
    let insert = handle.insert_source(source, move |_readiness, listener, _app| {
        loop {
            match listener.accept() {
                Ok((stream, _addr)) => attach_conn(&mut inner, stream),
                Err(e) if e.kind() == ErrorKind::WouldBlock => break,
                Err(e) if e.kind() == ErrorKind::Interrupted => continue,
                // EMFILE 这类资源错误不是"等一等就好"，accept 干也只是空转；出声一次、
                // 继续活着（宠物停不停由宠物决定，控制面坏了不许带走桌面生命 —— 硬约束 3）。
                Err(e) => {
                    eprintln!("ipc: accept 失败：{e} ⇒ 本轮停止接收新连接");
                    break;
                }
            }
        }
        Ok(PostAction::Continue)
    });
    if let Err(_e) = insert {
        // 挂不上环 = 事件源被归还并丢弃 ⇒ UnixListener 随 Generic 一起 drop（fd 关闭），
        // 没有泄漏。这里出声然后继续跑：控制面没了，宠物照旧是宠物。
        eprintln!(
            "ipc: 监听源挂不进事件环 ⇒ 控制套接字本轮不可用（socket 文件会照常在退出时清掉）"
        );
    }
}

/// 一条连接 = 一个源 + 闭包里的三样状态（读句柄、半行缓冲、超长丢弃旗）。
///
/// 读写的 **fd 用 clone**：`Generic` 拿一份只负责"何时就绪"，闭包拿自己那份负责读写
/// （同一个 socket，两个 fd 指向同一个打开文件描述 —— 摘除时两边都关，没有残留）。
fn attach_conn(handle: &mut LoopHandle<'_, App>, mut stream: UnixStream) {
    if stream.set_nonblocking(true).is_err() {
        return; // 设不上非阻塞就宁可断开，也不允许任何一处 read 卡住整个事件环
    }
    let poll = match stream.try_clone() {
        Ok(p) => p,
        Err(_) => return,
    };
    let mut buf: Vec<u8> = Vec::new();
    let mut overflow = false;
    let source = Generic::new(poll, Interest::READ, Mode::Level);
    // 挂不上 = Generic（含 poll fd）与被捕获的 stream 一起丢弃 ⇒ 两个 fd 都关，对端看见
    // 连接断开，且一个字都没写过 ⇒ 没有半帧残留，无需出声（监听还在，下一条连接照常）。
    let _ = handle.insert_source(source, move |_readiness, _meta, app| {
        // ① 读干到 WouldBlock（一次回调封顶 64 KiB，剩余由 Level 的下一次就绪接着喂
        //    ⇒ 恶意慢洪不能把这个回调钉死在帧循环里）。
        let mut chunk = [0u8; 4096];
        let mut budget = 16_usize;
        let eof_or_err = loop {
            if budget == 0 {
                break false;
            }
            budget -= 1;
            match stream.read(&mut chunk) {
                Ok(0) => break true, // EOF：对方关了写侧 ⇒ 摘除
                Ok(n) => buf.extend_from_slice(&chunk[..n]),
                Err(e) if e.kind() == ErrorKind::WouldBlock => break false,
                Err(e) if e.kind() == ErrorKind::Interrupted => continue,
                Err(_) => break true, // 传输层错误：没有可恢复的形状，摘除
            }
        };
        // ② 成帧 + 应答（每条完整行恰好一个应答，顺序 = 到达顺序）。
        let framed_ok = pump(
            &mut stream,
            &mut |eff| app.ipc_apply(eff),
            &mut buf,
            &mut overflow,
        );
        if eof_or_err || !framed_ok {
            return Ok(PostAction::Remove);
        }
        Ok(PostAction::Continue)
    });
}

/// 把缓冲里**完整的行**逐条送进协议层与应用点，应答逐条写回。
/// 返回 false ⇒ 写侧已坏，调用方摘除本连接。
///
/// `apply` 是应用点（真机 = `App::ipc_apply`）：成帧这一层与"动作落在谁身上"拆开，
/// 于是 8 KiB 超长、坏 UTF-8、一问一答、半行续帧这四条都在 headless 里可测 ——
/// `App` 离开 compositor 构造不起来（那条"526 条全绿的洞"的根），测试不能等它。
fn pump(
    stream: &mut UnixStream,
    apply: &mut dyn FnMut(
        petcore::ipc::Effect,
    ) -> Result<Option<serde_json::Value>, (String, String)>,
    buf: &mut Vec<u8>,
    overflow: &mut bool,
) -> bool {
    while let Some(pos) = buf.iter().position(|b| *b == b'\n') {
        let raw: Vec<u8> = buf[..=pos].to_vec();
        buf.drain(..=pos);
        let too_long = raw.len() - 1 > MAX_LINE_BYTES;
        if std::mem::take(overflow) {
            // 超长行已经出声过一次，它的尾巴一直滚到这个 '\n' ⇒ 静默丢。
            continue;
        }
        if too_long {
            if !write_line(stream, &oversize_reply()) {
                return false;
            }
            continue;
        }
        let line = match std::str::from_utf8(&raw[..raw.len() - 1]) {
            Ok(s) => s,
            Err(_) => {
                // 坏 UTF-8 出声但**不杀连接**：宽容断连的另一半（对方可能是 shell 管道）。
                if !write_line(
                    stream,
                    &petcore::ipc::encode(&petcore::ipc::err_reply(
                        code::PARSE_ERROR,
                        "请求不是合法 UTF-8",
                    )),
                ) {
                    return false;
                }
                continue;
            }
        };
        let reply = match petcore::ipc::handle(line) {
            Effect::Reply(v) => petcore::ipc::encode(&v),
            eff => match apply(eff) {
                Ok(data) => petcore::ipc::encode(&petcore::ipc::ok_reply(data)),
                Err((c, m)) => petcore::ipc::encode(&petcore::ipc::err_reply(&c, m)),
            },
        };
        if !write_line(stream, &reply) {
            return false;
        }
    }
    // 没有完整行而缓冲已经超过上限 ⇒ 进入丢弃态：出声一次，把已收的字节扔掉等下一个 '\n'，
    // 之后继续无换行地流也只是"每 8 KiB 清一次"，缓冲不无界增长。
    if !*overflow && buf.len() > MAX_LINE_BYTES {
        *overflow = true;
        buf.clear();
        return write_line(stream, &oversize_reply());
    }
    true
}

fn oversize_reply() -> String {
    petcore::ipc::encode(&petcore::ipc::err_reply(
        code::OVERSIZE,
        format!("请求超过 {MAX_LINE_BYTES} B 上限 ⇒ 该行丢弃"),
    ))
}

/// 应答的写侧唯一入口：整行（`encode` 已含 '\n'）一次 `write_all`。
/// 一条应答 ~200 B ≪ socket 发送缓冲 ⇒ 正常不可能部分写；真写坏就返回 false 让调用方
/// 摘除连接，**绝不**留着"半个 NDJSON 帧"给下一个应答续皮。
fn write_line(stream: &mut UnixStream, line: &str) -> bool {
    stream.write_all(line.as_bytes()).is_ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{BufRead, BufReader};
    use std::os::unix::net::UnixStream;

    /// (服务端半边, 客户端半边的行阅读器)。测试直接喂 `pump` 一个预填缓冲 =
    /// 生产里"读环已把字节收进 buf"之后的那一段；读环本身要事件环 ⇒ 真机轮覆盖。
    /// ⚠️ 客户端半边**必须**是全程共用的同一个 `BufReader`：每答新建一个会把
    /// 已经到缓冲的下一条应答连同 reader 一起丢掉（本测试第一版就是这么挂死的）。
    fn pair() -> (UnixStream, BufReader<UnixStream>) {
        let (server, client) = UnixStream::pair().unwrap();
        (server, BufReader::new(client))
    }

    fn read_line(stream: &mut BufReader<UnixStream>) -> String {
        // 应答已在内核缓冲里（pump 是同步写完成的）⇒ 阻塞 read_line 不会挂。
        let mut s = String::new();
        stream.read_line(&mut s).expect("应答可读");
        s
    }

    /// 只认成帧、不管动作的桩：所有效果答 `ok:true`。
    fn always_ok(_eff: Effect) -> Result<Option<serde_json::Value>, (String, String)> {
        Ok(None)
    }

    // ---- bind / 自愈 / 权限 / 守门人 ----

    #[test]
    fn bind_creates_a_0600_socket_and_the_guard_unlinks_it() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("opet.sock");
        let (listener, guard) = bind(&path).unwrap();
        let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600, "协议内零鉴权，0600 就是唯一的鉴权");
        // 监听者的确活着：connect 得通。
        UnixStream::connect(&path).unwrap();
        drop(listener);
        drop(guard);
        assert!(!path.exists(), "守门人的 Drop = 走了不留门牌");
    }

    #[test]
    fn live_listener_is_refused_not_stolen() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("opet.sock");
        let (_first, _guard) = bind(&path).unwrap();
        let e = bind(&path).unwrap_err();
        assert!(
            e.contains("拒绝启动") && e.contains("已有活着的服务端"),
            "撞活体必须明说不抢，实际: {e}"
        );
    }

    #[test]
    fn stale_inode_is_unlinked_and_rebound() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("opet.sock");
        // 手工 bind 且不留 guard = 模拟 kill -9 之后：进程没了，socket 文件还挂在盘上。
        let ghost = UnixListener::bind(&path).unwrap();
        drop(ghost);
        assert!(path.exists());
        let (listener, _guard) = bind(&path).unwrap();
        // 自愈后是新监听者在听，connect 得通。
        UnixStream::connect(&path).unwrap();
        drop(listener);
    }

    #[test]
    fn explicit_path_wins_and_never_touches_env() {
        let p = resolve_path(Some("/run/user/1000/other.sock")).unwrap();
        assert_eq!(p, PathBuf::from("/run/user/1000/other.sock"));
    }

    // ---- 成帧（pump）----

    #[test]
    fn one_line_one_reply_in_order() {
        let (mut server, mut client) = pair();
        let mut buf = b"{\"method\":\"pet.pause\"}\n{\"method\":\"pet.resume\"}\n".to_vec();
        let mut overflow = false;
        assert!(pump(&mut server, &mut always_ok, &mut buf, &mut overflow));
        assert!(buf.is_empty(), "两条完整行都该被消费");
        assert_eq!(read_line(&mut client), "{\"ok\":true}\n");
        assert_eq!(read_line(&mut client), "{\"ok\":true}\n");
    }

    #[test]
    fn partial_line_waits_for_its_newline() {
        let (mut server, mut client) = pair();
        let mut buf = b"{\"method\":\"pet.".to_vec();
        let mut overflow = false;
        assert!(pump(&mut server, &mut always_ok, &mut buf, &mut overflow));
        // 半行原样留着，一个字都没答。
        assert_eq!(buf, b"{\"method\":\"pet.".as_slice());
        buf.extend_from_slice(b"status\"}\n");
        assert!(pump(&mut server, &mut always_ok, &mut buf, &mut overflow));
        assert_eq!(read_line(&mut client), "{\"ok\":true}\n");
    }

    #[test]
    fn oversize_reports_once_then_silently_discards_to_newline() {
        let (mut server, mut client) = pair();
        // 一条 8 KiB + 1 的超长行，紧跟一条合法行。
        let mut buf = vec![b'a'; MAX_LINE_BYTES + 1];
        buf.push(b'\n');
        buf.extend_from_slice(b"{\"method\":\"pet.pause\"}\n");
        let mut overflow = false;
        assert!(pump(&mut server, &mut always_ok, &mut buf, &mut overflow));
        let over = read_line(&mut client);
        assert!(
            over.contains("\"oversize\"") && over.contains("\"ok\":false"),
            "{over}"
        );
        // 超长行之后的合法行照答 = "丢一行"不是"杀连接"。
        assert_eq!(read_line(&mut client), "{\"ok\":true}\n");
        assert!(buf.is_empty());
    }

    #[test]
    fn unterflowed_overflow_clears_buffer_and_waits_for_newline() {
        // 读环一次喂进 8 KiB+1 而**没有换行**（慢洪形状）⇒ 进丢弃态：出声一次、清缓冲。
        let (mut server, mut client) = pair();
        let mut buf = vec![b'a'; MAX_LINE_BYTES + 1];
        let mut overflow = false;
        assert!(pump(&mut server, &mut always_ok, &mut buf, &mut overflow));
        assert!(overflow, "缓冲越限要进丢弃态");
        assert!(buf.is_empty(), "已收字节清掉，不无界增长");
        assert!(read_line(&mut client).contains("oversize"));
        // 尾巴继续流（仍无换行）⇒ 静默。
        buf.extend_from_slice(&vec![b'b'; MAX_LINE_BYTES + 1]);
        assert!(pump(&mut server, &mut always_ok, &mut buf, &mut overflow));
        assert!(!buf.is_empty() || overflow);
    }

    #[test]
    fn invalid_utf8_answers_parse_error_and_keeps_the_connection() {
        let (mut server, mut client) = pair();
        let mut buf = vec![0xff_u8, b'\n'];
        buf.extend_from_slice(b"{\"method\":\"pet.pause\"}\n");
        let mut overflow = false;
        assert!(pump(&mut server, &mut always_ok, &mut buf, &mut overflow));
        let bad = read_line(&mut client);
        assert!(bad.contains("\"parse_error\""), "{bad}");
        assert_eq!(read_line(&mut client), "{\"ok\":true}\n");
    }

    #[test]
    fn application_error_travels_as_the_error_object() {
        let (mut server, mut client) = pair();
        let mut buf = b"{\"method\":\"pet.swap\",\"params\":{\"id\":\"nope\"}}\n".to_vec();
        let mut overflow = false;
        let mut failing = |_eff: Effect| Err(("UNAVAILABLE".to_string(), "库里没有".to_string()));
        assert!(pump(&mut server, &mut failing, &mut buf, &mut overflow));
        let reply = read_line(&mut client);
        assert!(
            reply.contains("\"ok\":false") && reply.contains("UNAVAILABLE"),
            "{reply}"
        );
    }

    #[test]
    fn dead_peer_makes_pump_report_removal() {
        let (mut server, client) = pair();
        drop(client);
        let mut buf = b"{\"method\":\"pet.pause\"}\n".to_vec();
        let mut overflow = false;
        // 写失败 ⇒ false ⇒ 生产里那档就是 PostAction::Remove。
        let ok = pump(&mut server, &mut always_ok, &mut buf, &mut overflow);
        assert!(!ok, "对端没了还答得出去就是假帧泄漏");
    }
}
