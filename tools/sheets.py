"""清单里的行号 → 像素文件。Python 侧的唯一一份，与 Rust 的 `Manifest::locate` 同源。

行号永远是**全局连续**的：`grid.rows` 是逻辑行号空间，`sheets` 只说明这些行分段存在
哪些文件里（DESIGN §6.1.27 裁决 1）。工具不许自己拼 `sheet.webp`，也不许按段序猜行号。
"""


def segments(manifest):
    """[(文件名, 起始全局行, 行数)]。没有 `sheets` 就是单张 = 从 0 数到 grid.rows。"""
    sheets = manifest.get("sheets") or []
    if not sheets:
        return [(manifest["sheet"], 0, int(manifest["grid"]["rows"]))]
    out = []
    base = 0
    for s in sheets:
        n = int(s["rows"])
        out.append((s["file"], base, n))
        base += n
    return out


def sheet_files(manifest):
    return [f for f, _, _ in segments(manifest)]


def locate(manifest, row):
    """全局行号 → `(段序号, 文件名, 段内行号)`；越界给 None。"""
    row = int(row)
    for i, (name, base, n) in enumerate(segments(manifest)):
        if base <= row < base + n:
            return i, name, row - base
    return None


def check(manifest):
    """返回问题列表（空 = 这份清单的分段描述可用）。与 Rust `validate` 同口径，
    但工具侧要先拿到能用行号的映射，所以这里只做"够不够切像素"那几条。"""
    grid = manifest.get("grid") or {}
    total = int(grid.get("rows", 0))
    segs = segments(manifest)
    problems = []
    if manifest.get("sheets"):
        if int(manifest.get("format", 0)) != 3:
            problems.append("带 sheets 的清单必须是 format 3，实际 %r" % manifest.get("format"))
        if manifest.get("sheet"):
            problems.append("带 sheets 时不许再写 sheet：%r ⇒ 像素有两个事实源" % manifest["sheet"])
    seen = set()
    for name, base, n in segs:
        if not name:
            problems.append("有一段没有文件名")
        elif name in seen:
            problems.append("段文件名重复: %s" % name)
        seen.add(name)
        if n <= 0:
            problems.append("%s 的行数 %d 不合法" % (name, n))
    got = sum(n for _, _, n in segs)
    if got != total:
        problems.append("各段行数之和 %d != grid.rows %d ⇒ 分段没有平铺全局行号" % (got, total))
    return problems
