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
IDLE_BOTTOM_JITTER_PX = 2  # @96 基准档的"待机时脚不该滑动"；格子提档后按 w/96 同比放大


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

    # 第 15 项：挂点的**出处**。这条存在的理由是一条已经发生过的事故 —— 内核里那条"挂点必须
    # 落在帧内"的校验（manifest.rs）守的是 cat_shiro.pet.json 里三个手写从没量过的数，而八份
    # 烘出的 pet.json 的 outfit_anchors 全是 null（pack.py 当时根本没有产出它的路径）。
    # ⇒ "我给 X 加了校验"必须一起问"X 有没有生产者"，而生产者产了就要能被追到。
    #
    # 三个方向都查：清单有而 outfit.json 没有 ⇒ 红（手抄的数再也混不进产物）；
    # outfit.json 在而清单没写出该字段 ⇒ 也红（产了没接）；逐 slot 落在帧内；
    # 再加一条**方向 sanity**：head.y < neck.y < feet.y —— 帧内检查对 y 和 h−y 同样放行，
    # "整组挂点上下翻面"这种错现在没人抓得住。
    anchors = manifest.get("outfit_anchors") or {}
    outfit_path = os.path.join(frames_dir, "outfit.json")
    outfit = load_json(outfit_path) if os.path.exists(outfit_path) else None
    if anchors and outfit is None:
        rep.add(
            "outfit_anchors 有出处",
            "清单写了 %s，而 %s 不存在" % (sorted(anchors), outfit_path),
            "两者同时存在或同时不存在",
            False,
            "没有 outfit.json 就等于这几个数是手抄的 ⇒ 重跑 bake.py --outfit-measure-only 再 pack.py",
        )
    elif outfit is not None and not anchors:
        rep.add(
            "outfit_anchors 有出处",
            "%s 存在，而清单里没写出该字段" % outfit_path,
            "两者同时存在或同时不存在",
            False,
            "生产者产了、打包器没接（pack.py 的 load_outfit_anchors）；或那份 outfit.json 是旧批次的",
        )
    elif outfit is None:
        rep.add(
            "outfit_anchors 有出处",
            "清单与该文件都没有",
            "两者同时存在或同时不存在",
            True,
            "这只宠物没量挂点（合法：字段是 serde(default)，省略即'无配件'）",
        )
    else:
        reported = {k: v for k, v in sorted((outfit.get("anchor") or {}).items()) if v is not None}
        bad_range = [
            "%s=%s" % (slot, pair)
            for slot, pair in sorted(anchors.items())
            if not (0 <= pair[0] <= w and 0 <= pair[1] <= h)
        ]
        same = all(k in anchors and [int(round(v[0])), int(round(v[1]))] == list(anchors[k]) for k, v in reported.items())
        where = outfit.get("anchor_from") or ["?", "?", "?"]
        rep.add(
            "outfit_anchors 与出处逐位相符",
            "%s vs %s" % (dict(sorted(anchors.items())), reported),
            "整数化后相等且都在帧内",
            same and not bad_range,
            "越界项 %s；出处 cell=%s，取自状态 %s 第 %s 帧第 %s 列"
            % (bad_range or "无", outfit.get("cell"), where[0], where[1], where[2]),
        )
        # y 在帧内是"越大越低"（屏幕坐标）⇒ 头顶必须比脖子高、脖子必须比脚高。
        ys = {slot: pair[1] for slot, pair in anchors.items()}
        if feet:
            ys["feet"] = feet[1]
        chain = [("head", "neck"), ("neck", "feet")]
        inverted = [
            "%s.y=%s 不在 %s.y=%s 上方" % (a, ys[a], b, ys[b])
            for a, b in chain
            if a in ys and b in ys and not (ys[a] < ys[b])
        ]
        rep.add(
            "挂点上下序 head<neck<feet",
            " ".join("%s.y=%s" % (k, ys[k]) for k in ("head", "neck", "feet") if k in ys),
            "严格递增",
            not inverted,
            "; ".join(inverted) or "'落在帧内'对 y 与 h−y 同样放行 ⇒ 翻面那种错只有这条抓得住",
        )

    # 待机底边抖动：anchor 不等于底行（四足在俯视下腿投影低于原点），所以查帧间抖动。
    #
    # ⚠️ 比较轴必须是"同一朝向、跨帧"，不是"同一帧、跨朝向"。后者量的是 15° 俯视把
    # 前后脚投影到不同屏幕行的**固定透视差**（脚越靠前投影越低），模型站得一动不动
    # 也会有 2–3 px；按它判等于把"换个骨架"自动判成"脚在滑"。
    # 换轴后用三个已知样本标定（同一套 96px/16samples 烘焙）：
    #   占位小白（腿是刚体图元）      跨帧 0 px / 跨朝向 1 px
    #   太空猫参数化骨骼 idle         跨帧 2 px（开 --plant-feet）/ 4 px（不开）
    #   太空猫套现成人类 HumanDrunkIdle 跨帧 8 px
    # ⇒ 新轴仍然会红（剪辑那条就是），只是不再把投影差误判成抖动。
    #
    # 阈值单位是"格子的百分之几"，裸 px 会随格子漂（1 px 在 @96 是 1/96、在 @192 是 1/192），
    # 所以下面按 w/96 同比走。@192 重标定（2026-10-29 idle 子集实测）：老虎 **0 px**、兔子 **0 px**
    # —— 两只在 @96 也都是 0 px ⇒ 这条门禁的判定序没被提档改变，只是刻度跟着格子走。
    if "idle" in manifest["states"]:
        grid = {}
        for row in manifest["states"]["idle"]["rows"]:
            for col in range(cols):
                block = sheet[row * h : (row + 1) * h, col * w : (col * w) + w, 3] > ALPHA_THRESHOLD
                ys = np.nonzero(block.any(axis=1))[0]
                grid[(row, col)] = int(ys.max()) if ys.size else -1
        by_col = {}
        for (row, col), y in grid.items():
            by_col.setdefault(col, []).append(y)
        frame_spread = {col: max(v) - min(v) for col, v in by_col.items()}
        worst = max(frame_spread.values())
        jitter_gate = IDLE_BOTTOM_JITTER_PX * w / 96.0
        by_row = {}
        for (row, col), y in grid.items():
            by_row.setdefault(row, []).append(y)
        face_spread = {row: max(v) - min(v) for row, v in by_row.items()}
        rep.add(
            "idle 底边帧间抖动",
            "最大 %d px (逐朝向 %s)" % (worst, dict(sorted(frame_spread.items()))),
            "<= %.0f px" % jitter_gate,
            worst <= jitter_gate,
            "同朝向跨帧；同帧跨朝向极差 %s 是俯视投影差，不算抖动" % dict(sorted(face_spread.items())),
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
