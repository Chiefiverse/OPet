//! 字体子集与文案表的**资产门禁**（`docs/UI_REDESIGN_V2.md` §4.2）。
//!
//! 放在集成测试目标而不是 `src/` 单测：它读的是仓库里的**资产文件**，与运行中的客户端无关，
//! 而 `include_bytes!`/`include_str!` 把资产钉进测试二进制最直接。
//!
//! ⚠️ **一条都不 `#[ignore]`、一条都不看环境变量。** M3-b 学到"计数器为 0 不等于代码对"：
//! 一个"本机才跑"的字体门禁，在缺字体的机器上就是永远绿的空测试。资产此刻就在库里。
//!
//! ## 为什么是"双向夹死"而不是原计划的"扫五个源文件的字面量"
//!
//! 计划 §4.2 门禁 ③ 想防的是"有人在 `menu.rs` 里硬写中文文案 ⇒ 既不进翻译表也不进子集"。
//! 本轮实测两条，都否掉了那条做法：
//! 1. 本仓库的 assert/expect 消息是**中文**写的（`menu.rs` 注释之外就有 192 个非 ASCII 字符，
//!    全部来自 `"candidates 非空"` 这类诊断串）。它们永不画上卡片，扫进来就是白涨几 KB 常驻
//!    内存 —— 而 VmHWM 余量只有 ≈580 KB（§2）。
//! 2. 源文件扫描抓不住它自己的反面：**多**收字（无人渲染）与**漏**收字（有人渲染）都得管。
//!
//! 所以这里用一对方向相反的断言把清单**夹成等式**：
//!   · `everything_the_ui_can_render_is_in_the_codes_list` —— 会渲染的 ⊆ 清单（缺字 = 卡片静默画 `?`）
//!   · `codes_list_covers_nothing_unrendered` —— 清单 ⊆ 会渲染的（多字 = 白占内存 + 没人知道为什么）
//! "什么会渲染"由**函数**给出（`Lang::ui_strings()` + 目录 `name`），不由人抄清单给出。
//! 至于"`menu.rs` 里硬写了一句中文"这种形状，正解是**落笔时**的门禁（[`overlay_wayland::text::missing`]
//! 对每个真的画上去的串查 cmap）—— 它看的是到达画布的内容，比任何源码正则都准。
//! ⚠️ 那条门禁的**函数已经在了，接线在 `card.rs` 换成 `text::draw` 的那一步**；在它接上之前，
//! 这三条等式（会渲染 ⊆ 清单、清单 ⊆ 会渲染、清单 ⊆ 真 cmap）就是全部防线。

use std::collections::BTreeSet;

use overlay_wayland::l10n::Lang;
use petcore::gacha::Catalog;

const FONT: &[u8] = include_bytes!("../assets/fonts/ui.otf");
const LICENSE: &str = include_str!("../assets/fonts/LICENSE-ui-font.txt");
const NOTICE: &str = include_str!("../assets/fonts/NOTICE-ui-font.txt");

/// `--verify` 三次连续重建逐字节一致后钉下来的摘要。改任何文案 → 重跑 `--emit-codes` +
/// `--build` + `--verify` → **这里的常数必须一起改**（它就是"资产与清单漂移"的证据）。
const FONT_SHA256: &str = "8400976ab130783d64074963ea11414f741f45dc900fae72989124b7dc93991d";
/// 源 .ttc 的摘要，与 `tools/subset_font.py` 的 `SOURCE_SHA256` 同一个数。
const SOURCE_SHA256: &str = "b76b0433203017ca80401b2ee0dd69350349871c4b19d504c34dbdd80541690a";
/// §4.2 的体积门禁（i18n 把子集从"只中文"涨到四 locale 并集后重算的档）。实测 43,404 B
/// （283 码位：分页那批新标签 +46）。预算留的是**加一整批文案**的余量，不是当前余量。
const FONT_BUDGET: usize = 320 * 1024;

/// 码位清单：每行 `U+XXXX # 来源`。**唯一事实源**，Rust 侧不留第二份清单。
///
/// 解析器用的是 [`overlay_wayland::text::codes_list`] —— 单元测试（`text::one_tier_of_every_glyph_fits_the_arena`
/// 与 `every_listed_codepoint_has_a_glyph_in_the_asset`）读的是同一个函数。两处各写一份，就会在
/// "哪一行算码位"上漂移，而漂移的方向恰好是"门禁绿着、清单其实没被解析全"。
/// ⚠️ 那条坑本身值得记：`"0050".parse::<u32>()` 是**五十**而 `0x50` 是 `P`；本项目第一版就栽在
/// 这儿 —— 解析出 264 个"码位"、严格递增、无重复，三条结构断言全绿，实际值却一半是错的。
fn codepoints() -> Vec<u32> {
    overlay_wayland::text::codes_list()
}

/// **会渲染的东西**的全集：四 locale 的全部 UI 串（函数给的，不是人抄的）+ 目录里 30 个
/// 物品名的非 ASCII 字 + 四个语言自名。返回 (字符 → 来源) 方便把门禁失败时说清楚是谁要的。
fn rendered_chars() -> BTreeSet<(u32, &'static str)> {
    let mut out: BTreeSet<(u32, &'static str)> = BTreeSet::new();
    for l in Lang::ALL {
        for s in l.ui_strings() {
            for ch in s.chars().filter(|c| *c > '\u{7e}') {
                out.insert((ch as u32, "l10n"));
            }
        }
    }
    let catalog = Catalog::default_v1().expect("默认目录读不出");
    assert_eq!(
        catalog.items.len(),
        30,
        "目录应该是 10 宠 + 20 配件 = 30 件"
    );
    for item in &catalog.items {
        for ch in item.name.chars().filter(|c| *c > '\u{7e}') {
            out.insert((ch as u32, "catalog-name"));
        }
    }
    out
}

fn hex_set(chars: &BTreeSet<(u32, &'static str)>) -> BTreeSet<u32> {
    chars.iter().map(|(cp, _)| *cp).collect()
}

/// 清单自身：排序、去重、可打印 ASCII 一个不缺（缺一个空格，`余额 1,240` 就断在空白上），
/// 而且不许混进控制字符。
#[test]
fn codepoints_list_is_sorted_unique_and_covers_printable_ascii() {
    let cps = codepoints();
    let set: BTreeSet<u32> = cps.iter().copied().collect();
    assert_eq!(
        set.len(),
        cps.len(),
        "清单里有重复码位 ⇒ --emit-codes 之后被人手加过"
    );
    assert!(
        cps.windows(2).all(|w| w[0] < w[1]),
        "清单必须严格递增（不然 --verify 的逐字节比对没有意义）"
    );
    let miss: Vec<u32> = (0x20u32..0x7f).filter(|cp| !set.contains(cp)).collect();
    assert!(
        miss.is_empty(),
        "清单解析出 {} 个，可打印 ASCII 缺 {miss:?}",
        set.len()
    );
    let ctrl: Vec<u32> = cps
        .iter()
        .copied()
        .filter(|cp| *cp < 0x20 || (0x7f..0xa0).contains(cp))
        .collect();
    assert!(ctrl.is_empty(), "清单混进控制字符 {ctrl:?}");
    println!(
        "码位 {} 个（ASCII 95 + 非 ASCII {}）",
        cps.len(),
        set.len() - 95
    );
}

/// §4.2 门禁 ①+② 合成：**缺字当场红**。新增一只宠物忘了重裁字体、或给 `Key` 加了一行没进
/// `Key::ALL`，都从这里炸出来，而不是等卡片上画个 `?`。
#[test]
fn everything_the_ui_can_render_is_in_the_codes_list() {
    let have: BTreeSet<u32> = codepoints().into_iter().collect();
    let need = rendered_chars();
    let miss: Vec<String> = need
        .iter()
        .filter(|(cp, _)| !have.contains(cp))
        .map(|(cp, src)| format!("U+{cp:04X} {} ← {src}", char::from_u32(*cp).unwrap_or('?')))
        .collect();
    assert!(
        miss.is_empty(),
        "会渲染的字不在清单里：{} —— 跑 `python3 tools/subset_font.py --emit-codes && --build && --verify`",
        miss.join("、")
    );
    println!("UI 实际会渲染的非 ASCII 字 {} 个，全在清单里", need.len());
}

/// 反方向：**清单里不许有没人渲染的字**。这条不是洁癖 —— 每个多余的 glyph 都是常驻内存
/// （§2 的 VmHWM 余量只有 ≈580 KB），而且它意味着扫描器把注释/测试消息当成了文案。
/// 本轮实测：`l10n.rs` 的 `#[cfg(test)]` 断言消息一度多出 26 个汉字（子集 39.1 → 34.5 KiB）。
#[test]
fn codes_list_covers_nothing_unrendered() {
    let allowed = hex_set(&rendered_chars());
    let extra: Vec<String> = codepoints()
        .into_iter()
        .filter(|cp| *cp >= 0x7f && !allowed.contains(cp))
        .map(|cp| format!("U+{cp:04X} {}", char::from_u32(cp).unwrap_or('?')))
        .collect();
    assert!(
        extra.is_empty(),
        "清单里有 {} 个谁都渲染不到的码位：{} —— 要么它真的会上屏（那就补进 `l10n`，别塞清单），\
         要么是扫描器多收了（`tools/subset_font.py::codes_from_l10n` 只该扫 `#[cfg(test)]` 以上）",
        extra.len(),
        extra.join("、")
    );
}

/// 体积 + 字节钉死 + 轮廓类型。CFF 不是审美选择：本机所有中文字体都是 CFF（实测
/// `fc-list :lang=zh fontformat` 唯一值），产物变成 glyf 就说明子集流程被人改过。
#[test]
fn subset_is_within_budget_pinned_and_cff() {
    assert!(
        FONT.len() <= FONT_BUDGET,
        "子集 {} B 超预算 {FONT_BUDGET} B",
        FONT.len()
    );
    println!(
        "ui.otf = {} B（{:.1} KiB），预算 {} B",
        FONT.len(),
        FONT.len() as f64 / 1024.0,
        FONT_BUDGET
    );
    let digest = {
        use sha2::{Digest, Sha256};
        let mut h = Sha256::new();
        h.update(FONT);
        h.finalize()
    };
    // hex 用 petcore 审计链那同一个 helper，不在这里另写一套小写十六进制
    assert_eq!(
        petcore::gacha::hex(&digest),
        FONT_SHA256,
        "入库字体与 `--verify` 钉下来的摘要不一致 ⇒ 清单/子集选项/fontTools 版本漂移了"
    );
    assert_eq!(&FONT[..4], b"OTTO", "sfnt 标签不是 OTTO ⇒ 轮廓不是 CFF");
    assert!(FONT.windows(4).any(|w| w == b"CFF "), "表目录里没有 CFF 表");
}

/// OFL-1.1 §2/§5：许可证原文与署名文件必须随包在场。
/// 这条不是形式主义：根 `LICENSE` 是代码的 AGPL-3.0-only（D8），**不覆盖**这份字体；
/// 反过来字体也不许被并进 AGPL。两个方向都要在文件里写明白。
#[test]
fn ofl_license_and_notice_ship_with_the_font() {
    for needle in [
        "SIL OPEN FONT LICENSE Version 1.1",
        "Reserved Font Name",
        "THE FONT SOFTWARE IS PROVIDED \"AS IS\"",
    ] {
        assert!(
            LICENSE.contains(needle),
            "LICENSE-ui-font.txt 缺原文片段：{needle}"
        );
    }
    assert!(
        !LICENSE.contains("AGPL"),
        "许可证正文里不该出现 AGPL —— 那会让人以为字体也跟着代码的授权走（§5 禁止）"
    );

    // 版权与署名：全部取本机 `--name-report` 从字体 name 表读出来的原文，不是凭记忆写的
    for needle in [
        "© 2014-2021 Adobe (http://www.adobe.com/).",
        "Noto is a trademark of Google Inc.",
        "Ryoko NISHIZUKA",
        "SIL Open Font License, Version 1.1",
        "AGPL-3.0-only",
        FONT_SHA256,
        SOURCE_SHA256,
    ] {
        assert!(NOTICE.contains(needle), "NOTICE-ui-font.txt 缺：{needle}");
    }
    assert!(
        NOTICE.contains("face index 2"),
        "NOTICE 里没写 face index ⇒ 换机器上无法复核是从哪个 face 裁的（这个 .ttc 的 0 是 JP）"
    );
}

/// OFL §3 里**唯一能自动化**的那一半：子集是 Modified Version，呈现给用户的字体名不许再是
/// Reserved Font Name。"改名那步跑了没有"不该靠读脚本源码确认 ⇒ 直接解 name 表。
#[test]
fn subset_name_table_does_not_present_the_reserved_font_name() {
    let names = name_records(FONT);
    assert!(
        !names.is_empty(),
        "name 表解出 0 条记录（解析坏了这条也要红，不能静默通过）"
    );

    for (id, want) in [
        (1u16, "OPet UI"),
        (4u16, "OPet UI"),
        (6u16, "OPetUI-Regular"),
    ] {
        let got: Vec<&str> = names
            .iter()
            .filter(|n| n.name_id == id)
            .map(|n| n.text.as_str())
            .collect();
        assert!(!got.is_empty(), "name ID {id} 一条都没有");
        assert!(
            got.iter().all(|v| *v == want),
            "name ID {id} = {got:?}，必须全是 {want:?}"
        );
    }
    // §3 限的是 "primary font name as presented to the users" ⇒ 名字类记录不许有 Noto。
    // ID 0（版权）与 ID 13（许可证说明）里出现 Noto 恰恰是 §2 **要求**的署名，ID 3 是
    // unique identifier（不呈现给用户，这里放 derived 声明），所以按 nameID 分流。
    for n in names
        .iter()
        .filter(|n| !matches!(n.name_id, 0 | 3 | 10 | 13) && !n.text.is_empty())
    {
        assert!(
            !n.text.to_lowercase().contains("noto"),
            "名字类记录 ID {} 里还留着 Reserved Font Name：{:?}",
            n.name_id,
            n.text
        );
    }
    let find = |id: u16| -> String {
        names
            .iter()
            .find(|n| n.name_id == id)
            .map(|n| n.text.clone())
            .unwrap_or_default()
    };
    assert!(
        find(0).contains("© 2014-2021 Adobe") && find(0).contains("Derived from Noto Sans CJK SC"),
        "ID 0 版权行 = {:?} —— 上游署名与 derived 声明都得在",
        find(0)
    );
    assert!(
        find(13).contains("SIL Open Font License, Version 1.1"),
        "ID 13 许可证说明 = {:?}（§5：字体整体继续待在 OFL 下）",
        find(13)
    );
}

struct NameRecord {
    name_id: u16,
    text: String,
}

/// 只够读 name 表的最小 sfnt 解析：表目录里找 `name`，逐记录解 string area。
/// 产物是 sha 钉死的入库文件 ⇒ 解不出来直接 panic（红），不降级、不吞错。
fn name_records(font: &[u8]) -> Vec<NameRecord> {
    let be16 = |b: &[u8], o: usize| u16::from_be_bytes([b[o], b[o + 1]]);
    let be32 = |b: &[u8], o: usize| u32::from_be_bytes([b[o], b[o + 1], b[o + 2], b[o + 3]]);
    for i in 0..be16(font, 4) as usize {
        let e = 12 + 16 * i;
        if &font[e..e + 4] != b"name" {
            continue;
        }
        let (off, len) = (be32(font, e + 8) as usize, be32(font, e + 12) as usize);
        let t = &font[off..off + len];
        let strings = off + be16(t, 4) as usize;
        let mut out = Vec::with_capacity(be16(t, 2) as usize);
        for r in 0..be16(t, 2) as usize {
            let p = 6 + 12 * r;
            let (platform, name_id) = (be16(t, p), be16(t, p + 6));
            let (nlen, o) = (be16(t, p + 8) as usize, be16(t, p + 10) as usize);
            let bytes = &font[strings + o..strings + o + nlen];
            // platform 0/3 = UTF-16BE。实测产物只有 Windows 记录（子集只写 p3）；真出现
            // platform 1 就按字节读，含非 ASCII 时上面那条 Noto 断言会抓住。
            let text = match platform {
                0 | 3 => String::from_utf16(
                    &bytes
                        .chunks_exact(2)
                        .map(|c| be16(c, 0))
                        .collect::<Vec<u16>>(),
                )
                .unwrap_or_default(),
                _ => bytes.iter().map(|&b| b as char).collect(),
            };
            out.push(NameRecord { name_id, text });
        }
        return out;
    }
    Vec::new()
}
