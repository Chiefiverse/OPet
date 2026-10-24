"""把 bake.py 烘出的 PNG 帧拼成图集，并生成 pet.json 与 polygon.json。

用法：
    python3 tools/pack.py --pet cat_shiro

行布局的唯一事实源是 `crates/petcore/assets/cat_shiro.pet.json`（那份被
assets_scheduler.rs 的测试钉着）。本脚本按 fixture 的状态顺序，把**已烘出帧**的状态
依次分配到物理行 0..N；所以只烘 idle 时得到 grid.rows=4 的合法子集清单，补烘更多状态
后行号自动前移，不需要在两个语言里各维护一份表。
"""

import argparse
import json
import os
import sys

import numpy as np
from PIL import Image

HERE = os.path.dirname(os.path.abspath(__file__))
REPO = os.path.dirname(HERE)
FIXTURE = os.path.join(REPO, "crates", "petcore", "assets", "cat_shiro.pet.json")

FACING_COLUMNS = 8  # manifest.rs 的 FACING_COLUMNS，硬校验
ALPHA_THRESHOLD = 8  # 与 polygon.json、validate.py 同一口径


def load_fixture():
    with open(FIXTURE, encoding="utf-8") as fh:
        return json.load(fh)


def load_meta(frames_dir):
    path = os.path.join(frames_dir, "meta.json")
    if not os.path.isdir(frames_dir):
        sys.exit("帧目录不存在，先跑 bake.py: %s" % frames_dir)
    with open(path, encoding="utf-8") as fh:
        return json.load(fh)


def discover_states(frames_dir, fixture):
    """按 fixture 顺序返回 (状态名, 帧数)，只含帧目录里真存在的状态。"""
    found = []
    for name in fixture["states"]:
        d = os.path.join(frames_dir, name)
        if not os.path.isdir(d):
            continue
        frames = sorted(f for f in os.listdir(d) if f.endswith(".png"))
        if not frames:
            continue
        expected = len(fixture["states"][name]["rows"])
        if len(frames) % FACING_COLUMNS != 0:
            sys.exit("状态 %s 的帧数 %d 不是 8 的倍数（每帧一个朝向列）" % (name, len(frames)))
        per_frame = len(frames) // FACING_COLUMNS
        if per_frame != expected:
            sys.exit("状态 %s 烘了 %d 帧，fixture 声明 %d 帧" % (name, per_frame, expected))
        found.append((name, per_frame))
    if not found:
        sys.exit("没找到任何已烘帧，先跑 bake.py")
    if "idle" not in [n for n, _ in found]:
        sys.exit("缺少 idle 状态，宠物无法待机（内核也会拒绝）")
    return found


def read_cell(frames_dir, state, col, frame):
    path = os.path.join(frames_dir, state, "f%02d_%02d.png" % (col, frame))
    if not os.path.exists(path):
        sys.exit("缺帧: %s" % path)
    img = Image.open(path)
    return img


def build_sheet(states, frames_dir, cell):
    rows = sum(n for _, n in states)
    sheet = Image.new("RGBA", (FACING_COLUMNS * cell, rows * cell), (0, 0, 0, 0))
    row_map = {}
    cursor = 0
    placed = 0
    for name, n in states:
        row_map[name] = list(range(cursor, cursor + n))
        for frame in range(n):
            for col in range(FACING_COLUMNS):
                img = read_cell(frames_dir, name, col, frame)
                if img.size != (cell, cell):
                    sys.exit("帧 %s 尺寸 %s != cell %d" % (name, img.size, cell))
                sheet.paste(img, (col * cell, (cursor + frame) * cell))
                placed += 1
        cursor += n
    return sheet, row_map, placed


def column_spans(alpha):
    """逐列取不透明 y 区间。比轮廓点集紧凑，且纯 numpy 可算（本机没有 scikit-image）。"""
    mask = alpha > ALPHA_THRESHOLD
    spans = []
    for x in range(mask.shape[1]):
        ys = np.nonzero(mask[:, x])[0]
        if ys.size:
            spans.append([int(x), int(ys.min()), int(ys.max())])
    return spans


def main():
    ap = argparse.ArgumentParser(description="OPet 图集打包")
    ap.add_argument("--pet", default="cat_shiro")
    ap.add_argument("--frames", default="")
    ap.add_argument("--out", default="")
    args = ap.parse_args()

    fixture = load_fixture()
    frames_dir = args.frames or os.path.join(REPO, "assets", "pets", args.pet, "frames")
    out_dir = args.out or os.path.join(REPO, "assets", "pets", args.pet)
    os.makedirs(out_dir, exist_ok=True)

    meta = load_meta(frames_dir)
    cell = meta["cell"]["w"]
    if meta["cell"]["h"] != cell:
        sys.exit("meta.json 的 cell 非方格: %s" % meta["cell"])
    if cell != fixture["grid"]["w"]:
        sys.exit(
            "烘焙格子 %d 与 fixture grid.w %d 不一致（bake.py 读的是 fixture，这里不该发生）"
            % (cell, fixture["grid"]["w"])
        )

    states = discover_states(frames_dir, fixture)
    sheet, row_map, placed = build_sheet(states, frames_dir, cell)

    fx, fy = meta["feet_px"]
    anchor = [int(round(fx)), int(round(fy))]
    if not (0 <= anchor[0] <= cell and 0 <= anchor[1] <= cell):
        sys.exit("anchor.feet %s 不在 %dx%d 帧内，内核会拒绝" % (anchor, cell, cell))

    manifest = {
        "format": fixture["format"],
        "slug": fixture["slug"],
        "display_name": fixture.get("display_name", fixture["slug"]),
        "sheet": "sheet.webp",
        "grid": {
            "cols": FACING_COLUMNS,
            "rows": sum(n for _, n in states),
            "w": cell,
            "h": cell,
        },
        "states": {
            name: {
                "rows": row_map[name],
                "fps": fixture["states"][name]["fps"],
                "loop": bool(fixture["states"][name].get("loop", False)),
            }
            for name, _ in states
        },
        "facing": fixture["facing"],
        "anchor": {"feet": anchor},
        "input_polygon": "auto_from_alpha",
    }
    # outfit_anchors 本轮省略：head/neck/hand 三个挂点都需要几何来源，
    # 占位模型没有配件挂点，硬写就是魔数。该字段在 Rust 侧是 serde(default)。

    sheet_path = os.path.join(out_dir, "sheet.webp")
    sheet.save(sheet_path, format="WEBP", lossless=True, quality=90)

    arr = np.asarray(sheet)
    poly = {
        "cell": {"w": cell, "h": cell},
        "threshold": ALPHA_THRESHOLD,
        "frames": [],
    }
    for name, n in states:
        for frame in range(n):
            row = row_map[name][frame]
            for col in range(FACING_COLUMNS):
                block = arr[row * cell : (row + 1) * cell, col * cell : (col + 1) * cell, 3]
                poly["frames"].append(
                    {"row": row, "col": col, "spans": column_spans(block)}
                )

    with open(os.path.join(out_dir, "pet.json"), "w", encoding="utf-8") as fh:
        json.dump(manifest, fh, ensure_ascii=False, indent=2)
        fh.write("\n")
    with open(os.path.join(out_dir, "polygon.json"), "w", encoding="utf-8") as fh:
        json.dump(poly, fh, ensure_ascii=False)

    webp_bytes = os.path.getsize(sheet_path)
    decoded = FACING_COLUMNS * cell * manifest["grid"]["rows"] * cell * 4
    print(
        "PACK %s: %d 状态 %s / %d 格 → sheet %dx%d"
        % (
            args.pet,
            len(states),
            ",".join(n for n, _ in states),
            placed,
            FACING_COLUMNS * cell,
            manifest["grid"]["rows"] * cell,
        )
    )
    print("PACK sheet.webp = %d B (%.1f KB)，解码后 RGBA = %.3f MB" % (webp_bytes, webp_bytes / 1024, decoded / 1048576))
    print("PACK anchor.feet = %s" % anchor)
    print("PACK → %s" % out_dir)


main()
