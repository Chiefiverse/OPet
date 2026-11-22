//! 省电口径的计数与归档 —— **各平台共用这一份，因为它是跨平台唯一可比的那个读数**。
//!
//! 本模块只有**类型与纯函数**：没有 buffer、没有 surface、没有任何显示服务器的名字。
//! 出图那个动作在各家客户端里，"这一趟算哪一档、归谁名下"在这里。
//!
//! ## 为什么这一段值得从 `app.rs` 搬出来
//!
//! 退出摘要那句 `核对: 五档合计 N vs tick M ⇒ OK（互斥且齐）` 是本项目判"宠物停着的时候到底
//! 有没有在耗电"的**唯一**量化口径（§4 那三条性能门禁由它供数）。如果 Windows 客户端自己再写
//! 一份 `Counters`，两份的"五档"很快就不再是同一件事 —— 而这件事**不会编译失败、不会让任何
//! 一条测试变红**，只会让两边报出来的数没法比。本项目在共享层上为这句话付过学费的形状：
//! 行布局唯一事实源（`cat_shiro.pet.json`）、alpha 阈值 [`crate::atlas::ALPHA_HIT_THRESHOLD`]、
//! `rates.json` 由引擎生成不许手改。⇒ 同一个办法：搬到一处，两边都从这里读。
//!
//! ## 各平台不都在五档里
//!
//! [`Counters::menu_commits`] 与 [`Counters::panel_commits`] 那两档是**第二、第三块 surface**
//! 的计数，字段留在这里（结构体只有一份，恒等式才只有一条），但一个没有菜单面/面板面的构建
//! （M5 最小片的 Windows 就是这个形状）**不许报 `0`** ⇒ 摘要里那两行要打
//! `本构建未接`，否则"没有"会被读成"零提交 = 省电"。这是同一个数在两边含义不同的那一族。

/// 省电口径的计数（§4 的三个档位 + 延后 + 失败）。
///
/// **五档互斥且齐**：`commits + moves + idle + deferred + buffer_errors == ticks`（落地见
/// [`Tick`] 与 [`Counters::bump`]）。补投与菜单那块 surface 的计数**不并进五档**（理由见各自
/// 字段注释）。这条恒等式是判据④的硬核对项，而它只在互斥时读得出来：`延后`/`buffer 错误`
/// 那两条分支原先除了记自己那一档还走 `finish_move` 再记一次 `moves` 或 `idle`，帧回调的补投
/// 又记进 `commits`。2026-10-28 真机轮 4 第一次让 `延后` 非零（=2），五档当场比 tick 多 3
/// （= 补投 2 + 双记 1，那 1 就是"延后且同时挪 margin"，`仅移动` 因此是 5618 而非 5617）。
#[derive(Clone, Copy, Debug, Default)]
pub struct Counters {
    /// 提交新 buffer（图集格真的变了）
    pub commits: u64,
    /// 只移 surface（margin 变了，没附新 buffer）
    pub moves: u64,
    /// 整 tick 无事可做（不换格、不延后、不出错）
    pub idle: u64,
    /// 内容变了但帧回调还没回来 ⇒ 本次不提交（丢帧可接受，绝不阻塞）
    pub deferred: u64,
    /// `create_buffer` / attach 失败次数
    pub buffer_errors: u64,
    /// 帧回调里补投的 buffer 数。**不并进五档** —— 它由帧回调驱动、不对应任何 tick，
    /// 混进 `commits` 就让"五档 == tick"永久读不出来（补投几次就超几次）。
    ///
    /// **读法（2026-10-28 实测校正，原先这里写的"M 应当等于 N"是错的判据）**：
    /// `延后 N / 补投 M` 只要求 **M ≤ N**，`M < N` 是**设计使然**而非丢格 ——
    /// 延后那一档只置一个布尔 `force_draw`（"绝不排队"），所以**连续几个延后的 tick 会
    /// 塌缩成回调里的一次补投**（补的是最后那个 intent，旧的已被更新的一帧取代）；另一条
    /// 让 M 变小的路径是看门狗先强制解锁 ⇒ 那一格由**下一个 tick** 按 `Buffer` 画上，
    /// 不经过补投。⇒ 真要警惕的形状是 **M > N**（补投比延后还多 = 有一格被投了两遍），
    /// 而"延后 > 0 但 M 与看门狗全 0"意味着通道锁死没解开。
    /// 首轮非零实测 = 2026-10-28 `--motion off` 那轮（用户在运行中改了屏幕缩放）：
    /// `延后 3 / 补投 2 / 帧回调看门狗 3`，五档仍 `== tick 总数` ⇒ 这条 P0/P1 路径
    /// 第一次有真机证据（前十轮全是 0，那 0 是"没跑到"）。
    ///
    /// ⚠️ **`延后` 与 `replay_commits` 这两档在 Windows 上是结构上恒 0 的**（`UpdateLayeredWindow`
    /// 没有在飞 buffer 的信号，所以既不会"延后"也不需要"补投"）。⇒ 那边摘要里打 `0` 之前
    /// 必须 inline 标出"这一档在本平台没有对应的东西"，不能让一个 `延后 0` 被读成
    /// "通过了的测量"（与现场事实第 10 条"看门狗非零是负载假信号"同一族的反向形状）。
    pub replay_commits: u64,
    /// `configure` 重配置触发的 buffer 数。同 `replay_commits`：**不并进五档**（不由 tick 驱动）。
    /// 2026-10-28 轮 3 首次撞到（5 次重配置 + 4 次看门狗），五档当场比 tick 多 5。
    pub configure_commits: u64,
    /// 帧回调迟迟没回来 ⇒ 看门狗强制解锁（不解锁会永远不再附新 buffer，宠物视觉上冻住）
    pub watchdogs: u64,
    /// 菜单那块 surface 的提交数。**不并进上面三档** —— §4 的省电口径是宠物的，
    /// 第二块 surface 混进去就会让"宠物三档不劣化"这条门禁变成读不出来的混合数。
    pub menu_commits: u64,
    pub menu_watchdogs: u64,
    /// 面板（第三块 surface，收藏册 / 设置）的提交数。同 `menu_commits`：**不并进上面三档**。
    ///
    /// ⚠️ 这条字段的存在理由与 `menu_commits` 不是一句"再来一块面就再来一个数"：面板是
    /// 「**开着一动不动**」的那类面 ⇒ 它唯一的省电判据是**静止恒 1 提交**（`69fd158` 那一族），
    /// 而这个数必须和菜单的分开读，否则"菜单翻页 +1"与"面板静止不涨"混成一个数，
    /// 两边都读不出来。
    pub panel_commits: u64,
    pub panel_watchdogs: u64,
}

impl Counters {
    /// 记这一档：**恰好动一个字段** —— 五档互斥这条恒等式就是靠它落地，别在外面手写 `+=`。
    ///
    /// M5 第一批起 `pub`：调用点在**各家客户端**里（Wayland 是 `App::attribute`，Windows 是它
    /// 自己的同名函数），而"只有一个写入点"这件事由 [`Tick`] 这一层保证，不是由私有性保证。
    /// 放宽可见性换来的正是"两边都不可能各自实现一份 `+=`"。
    pub fn bump(&mut self, tick: Tick) {
        match tick {
            Tick::Buffer => self.commits += 1,
            Tick::Replay => self.replay_commits += 1,
            Tick::Configure => self.configure_commits += 1,
            Tick::Move => self.moves += 1,
            Tick::Idle => self.idle += 1,
            Tick::Deferred => self.deferred += 1,
            Tick::Fault => self.buffer_errors += 1,
        }
    }

    /// 五档加总：应当逐轮等于客户端自己的 `ticks`（`Replay` 与**菜单、面板两块面的计数都在之外**）。
    ///
    /// 这一句从"两块面"变成"三块面"不是补字：面板的提交同样**不走 [`Self::bump`]**（它由
    /// `update_panel` 记进 `panel_commits`）。并进来这条恒等式当场就断 —— 历史上"五档比
    /// tick 多 5"就是 `configure` 那一格被并进去导致的。
    pub fn tick_accounted(&self) -> u64 {
        self.commits + self.moves + self.idle + self.deferred + self.buffer_errors
    }
}

/// 一次出图是被谁驱动的。**只有 `Tick` 归进五档**（[`Counters::tick_accounted`]）。
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Source {
    /// 60 Hz 定时的一个 tick（客户端补帧循环里）
    Tick,
    /// 帧回调回来后的补投：把延后的那一格补上
    Replay,
    /// 合成器又发了一次 `configure` ⇒ 客户端主动重画一帧
    Configure,
}

/// 一个 tick 归哪一档。档位与"发什么协议"是两条轴：`Deferred`/`Fault` 在 margin 动了时照样发
/// 一个不附 buffer 的状态 commit，但那不改变归属（历史上正是这里双记让五档超了 tick）。
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Tick {
    /// 附了新 buffer
    Buffer,
    /// 帧回调补投的 buffer ⇒ 不对应任何 tick
    Replay,
    /// `configure` 重配置触发的 buffer ⇒ 不对应任何 tick
    Configure,
    /// 只发一个不附 buffer 的状态 commit（margin 变了）
    Move,
    /// 什么都没发
    Idle,
    /// 内容要换格，但在途 buffer 没回来 ⇒ 本次不提交
    Deferred,
    /// 出图失败（`create_buffer` / attach）
    Fault,
}

/// 逐档遍历用（只服务测试；生产路径按具体档位分支，不需要这张表）。
#[cfg(test)]
const ALL_TICKS: [Tick; 7] = [
    Tick::Buffer,
    Tick::Replay,
    Tick::Configure,
    Tick::Move,
    Tick::Idle,
    Tick::Deferred,
    Tick::Fault,
];

/// `present` 可能产出的**归档结果**（`Replay`/`Configure` 不是结果、是来源，见 `ALL_TICKS`）。
#[cfg(test)]
const ALL_OUTCOMES: [Tick; 5] = [
    Tick::Buffer,
    Tick::Move,
    Tick::Idle,
    Tick::Deferred,
    Tick::Fault,
];

impl Tick {
    /// 这一档要不要单独发一个"只挪位置"的状态 commit。`Buffer` 已经把它带在同一次 commit 里。
    ///
    /// ⚠️ 这一条对 Wayland 是"发一个不附 buffer 的 commit"，对 Windows 是"做一次不带像素的
    /// `UpdateLayeredWindow`"—— 语义同、协议物不同，所以判档留在共用层而**执行在各家客户端**。
    pub fn flushes_margin(self) -> bool {
        matches!(self, Tick::Move | Tick::Deferred | Tick::Fault)
    }
}

/// 纯判定：这一 tick 归哪一档（副作用由各平台客户端按返回值执行）。
///
/// `new_cell` = 内核给了一个和已画不同的格，**或** `force_draw` 要求补画；`frame_pending` =
/// 有 buffer 在途等帧回调。出图成不成、以及是不是补投，要等真画了才知道，故 `Fault`/`Replay`
/// 不由这里产出。
pub fn classify_tick(new_cell: bool, moved: bool, frame_pending: bool) -> Tick {
    if !new_cell {
        if moved { Tick::Move } else { Tick::Idle }
    } else if frame_pending {
        Tick::Deferred
    } else {
        Tick::Buffer
    }
}

/// 归属规则（纯函数）：非 tick 来源里唯有"真发了一格 buffer"才占一个**另计**档
/// （`replay_commits` / `configure_commits`），其余（挪 margin、无事、延后、出错）协议动作照做
/// 但不占归属 —— 一次重配置什么都改不了，可它照样会走一遍出图。
///
/// 这条规则一天内断过两次（补投混进 `commits`、重配置混进 `idle`），所以做成表驱动、逐组合测。
pub fn attribute_of(source: Source, outcome: Tick) -> Option<Tick> {
    match source {
        Source::Tick => Some(outcome),
        Source::Replay => (outcome == Tick::Buffer).then_some(Tick::Replay),
        Source::Configure => (outcome == Tick::Buffer).then_some(Tick::Configure),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 判据④"五档相加 == tick 总数"的结构前提：一档只动一个字段。
    /// 真机上这条第一次露馅是 2026-10-28 轮 4（`延后 2` ⇒ 五档比 tick 多 3 = 补投 2 + 双记 1），
    /// 而那条路径撞不撞得上全看合成器时序 ⇒ 没有 headless 覆盖就随时可能悄悄退回去。
    #[test]
    fn a_tick_bumps_exactly_one_counter() {
        for tick in ALL_TICKS {
            let mut c = Counters::default();
            c.bump(tick);
            let touched = [
                c.commits,
                c.moves,
                c.idle,
                c.deferred,
                c.buffer_errors,
                c.replay_commits,
                c.configure_commits,
            ];
            assert_eq!(
                touched.iter().filter(|&&n| n != 0).count(),
                1,
                "{tick:?} 动了不止一个字段: {touched:?}"
            );
            assert_eq!(
                c.tick_accounted() == 1,
                matches!(
                    tick,
                    Tick::Buffer | Tick::Move | Tick::Idle | Tick::Deferred | Tick::Fault
                ),
                "{tick:?} 算不算一个 tick 与五档加总不一致: {touched:?}"
            );
        }
    }

    /// 归属表逐组合钉死：非 tick 来源只有在真发 buffer 时才占档，且占的是**另计**那一档。
    /// 这条断了就是判据④读不出真相（2026-10-28 一天内断过两次：补投进 `commits`、重配置进 `idle`）。
    #[test]
    fn attribution_is_a_table_over_source_and_outcome() {
        for source in [Source::Tick, Source::Replay, Source::Configure] {
            for outcome in ALL_OUTCOMES {
                let hit = attribute_of(source, outcome);
                let mut c = Counters::default();
                if let Some(hit) = hit {
                    c.bump(hit);
                }
                match source {
                    Source::Tick => {
                        assert_eq!(hit, Some(outcome), "{outcome:?} 必须原样占档");
                        assert_eq!(c.tick_accounted(), 1, "{outcome:?} 没占上一档");
                    }
                    non_tick => {
                        let want = match non_tick {
                            Source::Configure => Some(Tick::Configure),
                            _ => Some(Tick::Replay),
                        };
                        assert_eq!(
                            hit,
                            if outcome == Tick::Buffer { want } else { None },
                            "{outcome:?} 在 {non_tick:?} 下判错了另计档"
                        );
                        // 另计档一律不碰五档
                        assert_eq!(c.tick_accounted(), 0, "{outcome:?} 抢了一个 tick 的归属");
                    }
                }
            }
        }
    }

    /// 回归钉：延后那一 tick 即使同时挪了 margin，协议上照发状态 commit，但**不双记**。
    #[test]
    fn a_deferred_tick_flushes_the_margin_without_counting_a_move() {
        assert_eq!(classify_tick(true, true, true), Tick::Deferred);
        assert!(
            Tick::Deferred.flushes_margin(),
            "延后时 margin 动了必须把状态 commit 冲出去，否则合成器停在旧位置"
        );
        let mut c = Counters::default();
        c.bump(Tick::Deferred);
        assert_eq!((c.moves, c.idle, c.deferred), (0, 0, 1));

        // 出错同理（这条分支也会发状态 commit）
        assert!(Tick::Fault.flushes_margin());
        let mut c = Counters::default();
        c.bump(Tick::Fault);
        assert_eq!((c.moves, c.idle, c.buffer_errors), (0, 0, 1));
    }

    /// 归档全覆盖：8 种输入组合各自只落一档，且 `Move`/`Idle` 不可能吃掉一个"该换格"的 tick。
    #[test]
    fn classification_partitions_the_inputs() {
        for new_cell in [false, true] {
            for moved in [false, true] {
                for frame_pending in [false, true] {
                    let tick = classify_tick(new_cell, moved, frame_pending);
                    // `Fault`/`Replay` 要等真出图的结果，不归这里判
                    assert!(
                        matches!(
                            tick,
                            Tick::Buffer | Tick::Move | Tick::Idle | Tick::Deferred
                        ),
                        "{new_cell}/{moved}/{frame_pending} 判出了 {tick:?}"
                    );
                    assert_eq!(
                        matches!(tick, Tick::Move | Tick::Idle),
                        !new_cell,
                        "{new_cell}/{moved}/{frame_pending} ⇒ {tick:?}：不换格才会落进这两档"
                    );
                    assert_eq!(tick == Tick::Move, !new_cell && moved);
                    assert_eq!(tick == Tick::Idle, !new_cell && !moved);
                    assert_eq!(tick == Tick::Deferred, new_cell && frame_pending);
                    assert_eq!(tick == Tick::Buffer, new_cell && !frame_pending);
                }
            }
        }
    }
}
