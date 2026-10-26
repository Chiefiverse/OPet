#!/usr/bin/env python3
"""裁 OPet 的 UI 字体子集（`docs/UI_REDESIGN_V2.md` §4.2）。

产物三件，全部入库：
  crates/overlay-wayland/assets/fonts/ui-codepoints.txt  ← **码位的唯一事实源**
  crates/overlay-wayland/assets/fonts/ui.otf             ← 子集本体（CFF 轮廓）
  同目录 LICENSE-ui-font.txt / NOTICE-ui-font.txt        ← OFL-1.1 的再分发义务

为什么不是"手抄一份清单 + 手跑 pyftsubset"：上一轮 M3-b 抓到"白字压金底读不出"之后，
本轮立的规矩是**缺字必须当场红**，而人能写下来的清单一定会过期。所以清单由脚本从
`l10n.rs` 的字面量与 catalog 的 `name` 字段扫出来，Rust 侧另有两条测试兜着（见 §4.2 的门禁 ①②③）。

⚠️ **换 fontTools 版本必然改变产物字节** ⇒ 必须重跑全部内存门禁（VmHWM / 字号档 / 体积）。
所以下面把版本、输入文件摘要、每一个子集选项都写死，不用默认值。

用法：
    python3 tools/subset_font.py --emit-codes   # 重扫码位清单（改了文案/目录名之后）
    python3 tools/subset_font.py --name-report  # 打印上游 name 表（写 NOTICE 的唯一依据）
    python3 tools/subset_font.py --build        # 裁子集并就地改名（OFL §3）
    python3 tools/subset_font.py --verify       # 重建到临时目录并逐字节比对已入库产物
"""

import argparse
import hashlib
import json
import re
import sys
import tempfile
from pathlib import Path

from fontTools.ttLib import TTFont

try:
    import fontTools

    from fontTools import subset as _subset  # noqa: F401  (存在性检查，用它才走这条路)
except ImportError:  # pragma: no cover
    sys.exit("需要 fontTools（Arch: pacman -S python-fonttools）。本机 import 失败 ⇒ 装它。")

REPO = Path(__file__).resolve().parent.parent

# ───────── 输入：写死，不用"最新找到的那份字体" ─────────
SOURCE = Path("/usr/share/fonts/noto-cjk/NotoSansCJK-Regular.ttc")
# 2026-10-26 实测：这个 .ttc 有 10 个 face，**index 2 = Noto Sans CJK SC**（0 是 JP）。
# 默认 0 会静默裁出日文默认字形，而门禁全绿 —— 典型的"看起来对、其实说谎"。
FACE_INDEX = 2
SOURCE_SHA256 = "b76b0433203017ca80401b2ee0dd69350349871c4b19d504c34dbdd80541690a"
FONTTOOLS_PIN = "4.64.0"

OUT_DIR = REPO / "crates/overlay-wayland/assets/fonts"
CODES = OUT_DIR / "ui-codepoints.txt"
FONT_OUT = OUT_DIR / "ui.otf"
L10N_SRC = REPO / "crates/overlay-wayland/src/l10n.rs"
CATALOG = REPO / "crates/petcore/assets/gacha/cat_series_01.json"

# 子字体家族名：OFL §3 规定 Modified Version **不得沿用 Reserved Font Name**（"Noto Sans CJK"）。
# 只在 name ID 0 的版权行里保留 derived 声明，家族名整个换掉。
SUBSET_FAMILY = "OPet UI"
SUBSET_ID = "OPetUI-Regular"

# ASCII 可见区，与 `font.rs` 原来的 FIRST=0x20 LAST=0x7E 同一条口径（95 个）。
ASCII_RANGE = range(0x20, 0x7F)


def sha256(path: Path) -> str:
    h = hashlib.sha256()
    with path.open("rb") as f:
        for chunk in iter(lambda: f.read(1 << 20), b""):
            h.update(chunk)
    return h.hexdigest()


def check_inputs() -> None:
    if not SOURCE.exists():
        sys.exit(f"找不到源字体：{SOURCE}")
    got = sha256(SOURCE)
    if got != SOURCE_SHA256:
        sys.exit(
            f"源字体摘要不对：\n  期望 {SOURCE_SHA256}\n  实际 {got}\n"
            "⇒ 系统换过字体包。**先确认新版本的许可仍是 OFL-1.1，再更新 SOURCE_SHA256**，"
            "不许直接改这个数字让脚本过去。"
        )
    if fontTools.version != FONTTOOLS_PIN:
        sys.exit(
            f"fontTools 版本 {fontTools.version} ≠ 钉住的 {FONTTOOLS_PIN}。"
            "子集产物字节会变 ⇒ 先重跑全部内存门禁（VmHWM/体积/字号档），"
            "确认没问题再把 FONTTOOLS_PIN 与入库产物一起更新。"
        )


# ───────── 扫描 Rust 字面量（只取字符串里的非 ASCII，注释一个都不取） ─────────
def rust_string_literals(src: str) -> list[str]:
    """返回源码里所有字符串字面量的内容，**已剔除注释**。

    状态机而不是正则，理由：`tools`/`crates` 里的中文注释比字面量多得多，用"整文件找非
    ASCII"会把几百个永不渲染的字塞进子集（体积翻几倍而门禁照样绿）。也不处理原始字符串
    `r#"…"#` —— 本仓库没有用到，真用到了再补，宁可漏也不静默。
    """
    out: list[str] = []
    i, n = 0, len(src)
    while i < n:
        c = src[i]
        if c == "/" and i + 1 < n and src[i + 1] == "/":
            while i < n and src[i] != "\n":
                i += 1
            continue
        if c == "/" and i + 1 < n and src[i + 1] == "*":
            depth = 1
            i += 2
            while i < n and depth:
                if src[i] == "*" and i + 1 < n and src[i + 1] == "/":
                    depth -= 1
                    i += 2
                elif src[i] == "/" and i + 1 < n and src[i + 1] == "*":
                    depth += 1
                    i += 2
                else:
                    i += 1
            continue
        if c == '"':
            j = i + 1
            buf = []
            while j < n:
                if src[j] == "\\":
                    nxt = src[j + 1 : j + 2]
                    if nxt == "u":  # \u{XXXX} 直接按码位收
                        close = src.index("}", j)
                        buf.append(chr(int(src[j + 3 : close], 16)))
                        j = close + 1
                        continue
                    buf.append(nxt)
                    j += 2
                    continue
                if src[j] == '"':
                    break
                buf.append(src[j])
                j += 1
            out.append("".join(buf))
            i = j + 1
            continue
        i += 1
    return out


def non_ascii(texts) -> set[str]:
    return {ch for t in texts for ch in t if ord(ch) > 0x7F}


def codes_from_l10n() -> set[str]:
    src = L10N_SRC.read_text(encoding="utf-8")
    # 只扫 `#[cfg(test)]` 以上的**生产**部分。本仓库的 assert/expect 文案是中文写的，
    # 那些字永不画上卡片 ⇒ 收进子集就是白涨几 KB 常驻内存（实测多出 ~50 个字）。
    # 反向门禁 `codes_list_covers_nothing_unrendered` 专门抓这个方向。
    cut = src.find("#[cfg(test)]")
    if cut >= 0:
        src = src[:cut]
    return non_ascii(rust_string_literals(src))


def codes_from_catalog() -> set[str]:
    items = json.loads(CATALOG.read_text(encoding="utf-8"))["items"]
    # 只有 `name` 会画上卡片（`id` 是 ASCII slug，查不到名字时才回退显示它）。
    return non_ascii([i["name"] for i in items])


def provenance() -> dict[int, str]:
    """码位 → 来源标签，写进清单的注释列，改文案时看得出是谁要的。"""
    tags: dict[int, str] = {}
    for cp in ASCII_RANGE:
        tags[cp] = "ascii"
    for ch in codes_from_catalog():
        tags.setdefault(ord(ch), "catalog-name")
    for ch in codes_from_l10n():
        tags[ord(ch)] = "l10n" if tags.get(ord(ch)) != "catalog-name" else "l10n+catalog-name"
    return tags


def emit_codes() -> None:
    tags = provenance()
    lines = [
        "# OPet UI 字体子集的码位清单 —— **唯一事实源**，由 `tools/subset_font.py --emit-codes` 生成。",
        "# 每行一个码位：`U+XXXX # 来源`。改 `l10n.rs` 的文案或改目录里的 `name` 之后",
        "# 重跑 --emit-codes 再 --build，然后 --verify。Rust 侧 `tests/font_assets.rs` 把这份清单",
        "# 夹在中间双向钉死：`everything_the_ui_can_render_is_in_the_codes_list`（缺字 ⇒ 卡片上",
        "# 静默画 ?）与 `codes_list_covers_nothing_unrendered`（多字 ⇒ 白涨常驻内存）。",
        f"# 源字体：{SOURCE.name} face {FACE_INDEX} (Noto Sans CJK SC)",
        f"# fontTools：{FONTTOOLS_PIN}",
    ]
    for cp in sorted(tags):
        lines.append(f"U+{cp:04X} # {tags[cp]}")
    OUT_DIR.mkdir(parents=True, exist_ok=True)
    CODES.write_text("\n".join(lines) + "\n", encoding="utf-8")
    print(f"码位清单：{len(tags)} 个（ASCII {len(list(ASCII_RANGE))} + 非 ASCII {len(tags) - len(list(ASCII_RANGE))}）→ {CODES.relative_to(REPO)}")


def read_codes() -> list[int]:
    cps = []
    for line in CODES.read_text(encoding="utf-8").splitlines():
        m = re.match(r"^\s*U\+([0-9A-Fa-f]{4,6})\b", line)
        if m:
            cps.append(int(m.group(1), 16))
    if not cps:
        sys.exit(f"{CODES} 里一个码位都没有 —— 先跑 --emit-codes")
    return sorted(set(cps))


def subset_args(out: Path, cps: list[int]) -> list[str]:
    # 每一个选项都显式写：靠默认值 = 换机器/换版本时产物悄悄变。
    return [
        f"--font-number={FACE_INDEX}",
        f"--output-file={out}",
        # 全角冒号、省略号这类标点都在清单里，不做 --unicodes=* 之类的兜底扩张。
        "--unicodes=" + ",".join(f"U+{c:04X}" for c in cps),
        "--layout-features=",  # 不做连字/字距：排版只用 hmtx 的 advance
        "--no-legacy-kern",
        "--drop-tables+=DSIG,BASE,GPOS,GSUB,VORG,vhea,vmtx",
        "--notdef-outline",
        "--no-glyph-names",  # 名字不进产物 ⇒ 更小，且没有"看起来还是 Noto"的暗示
        "--no-recalc-timestamp",  # 确定性：不写当前时间
        "--canonical-order",
        "--desubroutinize",  # 展开子程序：栅格化路径最短，代价是体积（--verify 里量）
        "--name-IDs=0,1,2,3,4,6,8,13",
    ]


def rename(font: TTFont, copyright_line: str) -> None:
    """OFL §3：子集是 Modified Version，不得沿用 Reserved Font Name。"""
    name = font["name"]
    records = {
        0: f"{copyright_line}\nDerived from Noto Sans CJK SC, subset & renamed to {SUBSET_FAMILY}.",
        1: SUBSET_FAMILY,
        2: "Regular",
        3: f"{SUBSET_FAMILY} — subset of Noto Sans CJK SC for the OPet desktop pet UI.",
        4: f"{SUBSET_FAMILY}",
        6: SUBSET_ID,
        8: "The original authors. See NOTICE-ui-font.txt.",
        13: "This Font Software is licensed under the SIL Open Font License, Version 1.1. "
        "See LICENSE-ui-font.txt.",
    }
    for nid, value in records.items():
        name.setName(value, nid, 3, 1, 0x409)
    font["head"].fontRevision = 1.0


def build(out: Path, cps: list[int]) -> TTFont:
    check_inputs()
    tmp = Path(tempfile.mkdtemp()) / "ui.otf"
    _subset.main([str(SOURCE), *subset_args(tmp, cps)])
    # recalcTimestamp=False 是这条路径的命门：TTFont 默认 True，于是 save 时把 head.modified
    # 盖成"当时的秒"⇒ 跨秒就产出不同字节（实测两种产物交替出现，差 1 秒 + checkSumAdjustment）。
    font = TTFont(tmp, recalcTimestamp=False)
    src = TTFont(SOURCE, fontNumber=FACE_INDEX, recalcTimestamp=False)
    copyright_line = src["name"].getDebugName(0) or ""
    if not copyright_line.strip():
        sys.exit("源字体 name 表 ID 0 是空的 ⇒ NOTICE 的版权行不许凭记忆编。")
    rename(font, copyright_line)
    head, src_head = font["head"], src["head"]
    head.created, head.modified = src_head.created, src_head.modified
    out.parent.mkdir(parents=True, exist_ok=True)
    font.save(out)
    return font


def spot_check(font: TTFont, cps: list[int]) -> None:
    """产物必须真的含每一个请求的码位，并对假名/谚文/汉字各抽查 —— 缺字在这里炸，
    而不是等到卡片上画出一个 `?`。"""
    cmap = font.getBestCmap()
    missing = [c for c in cps if c not in cmap]
    if missing:
        raise SystemExit(
            "子集缺字：" + ", ".join(f"U+{c:04X}" for c in missing[:20])
            + (f" …共 {len(missing)} 个" if len(missing) > 20 else "")
        )
    groups = {"假名": (0x3040, 0x30FF), "谚文": (0xAC00, 0xD7A3), "汉字": (0x4E00, 0x9FFF)}
    for label, (lo, hi) in groups.items():
        have = [c for c in cps if lo <= c <= hi]
        print(f"  {label}：请求 {len(have)}，命中 {sum(1 for c in have if c in cmap)}")
        if not have:
            print(f"  ⚠️ 清单里一个{label}码位都没有 —— 四 locale 的文案是不是没进 `l10n.rs`？")
    # 可重放性的两个前置条件，坏了会以"下次跑 --verify 逐字节不同"这种难查的形式表现出来，
    # 所以在这里点名验，而不是留给哈希比对去猜。
    head = font["head"]
    src_head = TTFont(SOURCE, fontNumber=FACE_INDEX, lazy=True, recalcTimestamp=False)["head"]
    if (head.created, head.modified) != (src_head.created, src_head.modified):
        raise SystemExit(
            f"head 时间戳被改了：产物 {head.created}/{head.modified} ≠ 上游 "
            f"{src_head.created}/{src_head.modified} ⇒ save 在盖当前时间，产物不可重放"
        )
    family = font["name"].getDebugName(1) or ""
    if family != SUBSET_FAMILY:
        raise SystemExit(f"家族名是 {family!r}，不是 {SUBSET_FAMILY!r} —— OFL §3 禁止沿用保留名")
    if "noto" in family.lower():
        raise SystemExit(f"家族名 {family!r} 里还留着 Noto —— Reserved Font Name 违规")


def name_report() -> None:
    check_inputs()
    src = TTFont(SOURCE, fontNumber=FACE_INDEX)
    n = src["name"]
    print(f"face {FACE_INDEX} = {n.getDebugName(1)} {n.getDebugName(2)}")
    for nid in (0, 7, 8, 9, 11, 13, 14, 18):
        print(f"  name ID {nid}: {n.getDebugName(nid)}")
    print(f"  轮廓表：{'CFF ' if 'CFF ' in src else 'glyf'}")
    print("⇒ NOTICE-ui-font.txt 的版权行**只能**用上面 ID 0 的原文。")


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    ap.add_argument("--emit-codes", action="store_true", help="从 l10n.rs + catalog 重扫码位清单")
    ap.add_argument("--build", action="store_true", help="裁子集并就地改名")
    ap.add_argument("--verify", action="store_true", help="重建并逐字节比对已入库产物")
    ap.add_argument("--name-report", action="store_true", help="打印上游 name 表")
    a = ap.parse_args()

    if a.name_report:
        name_report()
        return 0
    if a.emit_codes:
        emit_codes()
        return 0
    cps = read_codes()
    if a.verify:
        if not FONT_OUT.exists():
            sys.exit(f"{FONT_OUT} 还不存在 —— 先 --build")
        tmp = Path(tempfile.mkdtemp()) / "ui.otf"
        build(tmp, cps)
        want, got = sha256(FONT_OUT), sha256(tmp)
        print(f"入库产物 {want}\n重建产物 {got}")
        if want != got:
            sys.exit("逐字节不一致 ⇒ 子集不再可重放，查 FONTTOOLS_PIN / 选项 / 清单")
        print(f"可重放 ✅  {FONT_OUT.stat().st_size:,} B")
        spot_check(TTFont(FONT_OUT), cps)
        return 0
    font = build(FONT_OUT, cps)
    size = FONT_OUT.stat().st_size
    print(f"写出 {FONT_OUT.relative_to(REPO)}：{size:,} B（{size / 1024:.1f} KiB），码位 {len(cps)}")
    spot_check(font, cps)
    return 0


if __name__ == "__main__":
    sys.exit(main())
