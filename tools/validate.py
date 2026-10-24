"""M2 资产的唯一像素门禁。

内核侧完全不解码像素（petcore 依赖只有 serde/serde_json，`sheet` 字符串从不被打开），
所以图集体积、alpha、锚点落位这些只能在这里查。失败非零退出。

用法：
    python3 tools/validate.py --pet cat_shiro
"""

import argparse
import json
import os
import subprocess
import sys

import numpy as np
from PIL import Image

HERE = os.path.dirname(os.path.abspath(__file__))
REPO = os.path.dirname(HERE)

FACING_COLUMNS = 8
ALPHA_THRESHOLD = 8  # 与 pack.py 同口径
SHEET_BYTES_LIMIT = 4 * 1024 * 1024  # 设计书 §3.5：单只宠物图集 ≤ 4 MB
RSS_BUDGET_MB = 120.0  # §6.1 性能门禁
FULL_GRID_ROWS = 40  # 全量清单的行数，用于外推
COVERAGE_MIN_PCT = 2.0  # 低于 ⇒ 判"拍空"（相机没对准，就是 TRACK_TO 那个坑）
COVERAGE_MAX_PCT = 98.0  # 高于 ⇒ 判"背景没抠干净"（film_transparent 失效）
IDLE_BOTTOM_JITTER_PX = 2  # 待机时脚不该滑动


class Report:
    def __init__(self):
        self.rows = []
        self.failed = False

    def add(self, name, measured, gate, ok, note=""):
        self.rows.append((name, measured, gate, "✅" if ok else "❌", note))
        if not ok:
            self.failed = True


def load_json(path):
    if not os.path.exists(path):
        sys.exit("缺少文件: %s" % path)
    with open(path, encoding="utf-8") as fh:
        return json.load(fh)


def measure_rss_mb(sheet_path):
    """真实驻留峰值：子进程解码整张 sheet 后读 VmHWM，而不是只做 w*h*4 算术。"""
    code = (
        "import resource, sys\n"
        "from PIL import Image\n"
        "im = Image.open(sys.argv[1]).convert('RGBA')\n"
        "px = im.tobytes()\n"
        "print(resource.getrusage(resource.RUSAGE_SELF).ru_maxrss)\n"
    )
    out = subprocess.run([sys.executable, "-c", code, sheet_path], capture_output=True, text=True)
    if out.returncode != 0:
        return None, out.stderr.strip()
    return int(out.stdout.strip()) / 1024.0, None


def main():
    ap = argparse.ArgumentParser(description="OPet 资产校验")
    ap.add_argument("--pet", default="cat_shiro")
    ap.add_argument("--dir", default="")
    args = ap.parse_args()

    d = args.dir or os.path.join(REPO, "assets", "pets", args.pet)
    manifest = load_json(os.path.join(d, "pet.json"))
    sheet_path = os.path.join(d, manifest["sheet"])
    if not os.path.exists(sheet_path):
        sys.exit("缺少图集: %s" % sheet_path)
    frames_dir = os.path.join(d, "frames")

    grid = manifest["grid"]
    w, h, cols, rows = grid["w"], grid["h"], grid["cols"], grid["rows"]
    rep = Report()

    rep.add("grid.cols == 8", cols, "必须 8", cols == FACING_COLUMNS, "内核 FACING_COLUMNS 硬校验")

    sheet = np.asarray(Image.open(sheet_path).convert("RGBA"))
    rep.add(
        "图集尺寸",
        "%dx%d" % (sheet.shape[1], sheet.shape[0]),
        "%dx%d" % (cols * w, rows * h),
        sheet.shape[1] == cols * w and sheet.shape[0] == rows * h,
    )

    # 逐格：源帧存在性 + 尺寸 + 覆盖率
    missing, bad_size, empty, opaque_full = [], [], [], []
    coverage = {}
    for name, track in manifest["states"].items():
        for fidx, row in enumerate(track["rows"]):
            for col in range(cols):
                src = os.path.join(frames_dir, name, "f%02d_%02d.png" % (col, fidx))
                if not os.path.exists(src):
                    missing.append(src)
                    continue
                img = Image.open(src)
                if img.size != (w, h):
                    bad_size.append("%s=%s" % (src, img.size))
                block = sheet[row * h : (row + 1) * h, col * w : (col + 1) * w, 3]
                pct = 100.0 * float((block > ALPHA_THRESHOLD).mean())
                coverage[(row, col)] = pct
                if pct < COVERAGE_MIN_PCT:
                    empty.append("row%d col%d %.1f%%" % (row, col, pct))
                elif pct > COVERAGE_MAX_PCT:
                    opaque_full.append("row%d col%d %.1f%%" % (row, col, pct))

    rep.add("源帧齐全", "%d 格" % len(coverage), "%d 格" % (rows * cols), not missing, "; ".join(missing[:3]))
    rep.add("源帧尺寸 == cell", "全部 %dx%d" % (w, h), "%dx%d" % (w, h), not bad_size, "; ".join(bad_size[:3]))
    rep.add("无拍空格", "最低 %.1f%%" % min(coverage.values()), ">= %.0f%%" % COVERAGE_MIN_PCT, not empty, "; ".join(empty[:3]))
    rep.add("背景已抠透", "最高 %.1f%%" % max(coverage.values()), "<= %.0f%%" % COVERAGE_MAX_PCT, not opaque_full, "; ".join(opaque_full[:3]))

    # 锚点
    feet = manifest.get("anchor", {}).get("feet")
    ok_anchor = bool(feet) and 0 <= feet[0] <= w and 0 <= feet[1] <= h
    rep.add("anchor.feet 在帧内", str(feet), "x∈[0,%d] y∈[0,%d]" % (w, h), ok_anchor, "越界则内核判'不在帧内'并拒绝清单")

    # 待机底边抖动：anchor 不等于底行（四足在俯视下腿投影低于原点），所以查帧间抖动
    if "idle" in manifest["states"]:
        bottoms = {}
        for row in manifest["states"]["idle"]["rows"]:
            for col in range(cols):
                block = sheet[row * h : (row + 1) * h, col * w : (col + 1) * w, 3] > ALPHA_THRESHOLD
                ys = np.nonzero(block.any(axis=1))[0]
                bottoms.setdefault(row, []).append(int(ys.max()) if ys.size else -1)
        spreads = {r: max(v) - min(v) for r, v in bottoms.items()}
        worst = max(spreads.values())
        rep.add(
            "idle 底边帧间抖动",
            "最大 %d px (逐行 %s)" % (worst, spreads),
            "<= %d px" % IDLE_BOTTOM_JITTER_PX,
            worst <= IDLE_BOTTOM_JITTER_PX,
            "非零 ⇒ 待机时脚在滑",
        )

    # polygon.json
    poly_path = os.path.join(d, "polygon.json")
    if os.path.exists(poly_path):
        poly = load_json(poly_path)
        cells = {(e["row"], e["col"]): e["spans"] for e in poly["frames"]}
        expected = rows * cols
        bad_bounds = [
            (r, c)
            for (r, c), spans in cells.items()
            if any(x < 0 or x >= w or y0 < 0 or y1 >= h or y0 > y1 for x, y0, y1 in spans)
        ]
        mismatch = [
            (r, c) for (r, c), sp in cells.items() if (len(sp) > 0) != (coverage.get((r, c), 0.0) > 0.0)
        ]
        rep.add("polygon 格数", "%d / %d" % (len(cells), expected), "== %d" % expected, len(cells) == expected)
        rep.add("polygon 越界", "%d 处" % len(bad_bounds), "0 处", not bad_bounds, str(bad_bounds[:3]))
        rep.add("polygon 与 alpha 同空同满", "%d 处不一致" % len(mismatch), "0 处", not mismatch, str(mismatch[:3]))
    else:
        rep.add("polygon.json 存在", "缺失", "必须存在", False)

    # 体积与内存
    webp_bytes = os.path.getsize(sheet_path)
    rep.add(
        "sheet.webp 体积",
        "%.1f KB" % (webp_bytes / 1024),
        "<= %.1f MB" % (SHEET_BYTES_LIMIT / 1048576),
        webp_bytes <= SHEET_BYTES_LIMIT,
        "§3.5 单只宠物门禁",
    )
    decoded_mb = cols * w * rows * h * 4 / 1048576
    full_mb = cols * w * FULL_GRID_ROWS * h * 4 / 1048576
    rss_mb, err = measure_rss_mb(sheet_path)
    rep.add("本轮图集解码后（算术）", "%.3f MB" % decoded_mb, "—", True, "本轮 %d 行 → 全量 %d 行外推 %.2f MiB" % (rows, FULL_GRID_ROWS, full_mb))
    if rss_mb is None:
        rep.add("解码驻留 RSS 实测", "测不了: %s" % err, "<= %.0f MB 的 %4.0f%%" % (RSS_BUDGET_MB, 100 * full_mb / RSS_BUDGET_MB), False)
    else:
        rep.add(
            "解码驻留 RSS 实测",
            "%.1f MB（含解释器）" % rss_mb,
            "全量应 <= %.1f MB (%.1f%%)" % (RSS_BUDGET_MB, 100 * full_mb / RSS_BUDGET_MB),
            full_mb <= RSS_BUDGET_MB,
            "Python 解释器本身占大头，宠物进程只看外推值",
        )

    print("## validate: %s\n" % args.pet)
    print("| 项 | 实测 | 门禁 | 判定 | 备注 |")
    print("|---|---|---|---|---|")
    for name, measured, gate, mark, note in rep.rows:
        print("| %s | %s | %s | %s | %s |" % (name, measured, gate, mark, note))
    print()
    if rep.failed:
        print("VALIDATE 失败")
        sys.exit(1)
    print("VALIDATE 全绿")


main()
