"""把一份已产物本体的图集按行切成两段，用来**彩排**分段存放（DESIGN §6.1.27）。

在册十只本体与二十件配件一律不重打、不重烘（现场事实 11①②）⇒ 本工具只读现成产物，
只往 `--out` 指定的目录里写，默认用法是写到 `/scratch/`。行表 `states.*.rows`、`grid`、
`fps`、`loop`、`display_name` 一字不改：**改的只有像素放在哪几个文件里**。

    python3 tools/split_sheet.py --in assets/pets/tiger --out /scratch/tiger_split --at 20
"""

import argparse
import json
import os
import sys

import numpy as np
from PIL import Image

import sheets as sheet_layout

HERE = os.path.dirname(os.path.abspath(__file__))
REPO = os.path.dirname(HERE)


def main():
    ap = argparse.ArgumentParser(description="按全局行号把图集切成两张（拆分彩排）")
    ap.add_argument("--in", dest="src", required=True, help="已产物目录（含 pet.json/sheet.webp/polygon.json）")
    ap.add_argument("--out", dest="dst", required=True, help="输出目录（不许指向在册产物）")
    ap.add_argument("--at", type=int, required=True, help="第一段的行数（全局行号在此处切开）")
    args = ap.parse_args()

    src = args.src if os.path.isabs(args.src) else os.path.join(REPO, args.src)
    dst = args.dst if os.path.isabs(args.dst) else os.path.join(REPO, args.dst)
    in_register = os.path.realpath(dst).startswith(os.path.realpath(os.path.join(REPO, "assets")) + os.sep)
    if in_register:
        sys.exit("拒绝写进在册产物目录（assets/…）：拆分彩排的产物只落 /scratch/，重打会静默改动盖章字段")
    manifest_path = os.path.join(src, "pet.json")
    if not os.path.exists(manifest_path):
        sys.exit("缺少清单: %s" % manifest_path)
    with open(manifest_path, encoding="utf-8") as fh:
        manifest = json.load(fh)
    if manifest.get("sheets"):
        sys.exit("这份产物已经是分段存放的（%s），本工具只切单张" % ", ".join(sheet_layout.sheet_files(manifest)))
    src_sheet = os.path.join(src, manifest["sheet"])
    if not os.path.exists(src_sheet):
        sys.exit("缺少图集: %s" % src_sheet)

    grid = manifest["grid"]
    w, h, cols, rows = grid["w"], grid["h"], grid["cols"], grid["rows"]
    if not 0 < args.at < rows:
        sys.exit("--at 必须在 (0, %d) 之间，收到 %d ⇒ 有一张会是空的，空段会让行号平移" % (rows, args.at))

    img = Image.open(src_sheet).convert("RGBA")
    arr = np.asarray(img)
    if arr.shape != (rows * h, cols * w, 4):
        sys.exit(
            "图集与清单不符: 解出 %dx%d，应为 %dx%d（%d 行 × %d px）"
            % (arr.shape[1], arr.shape[0], cols * w, rows * h, rows, h)
        )
    cut = args.at * h
    halves = [arr[:cut], arr[cut:]]
    names = ["sheet_0.webp", "sheet_1.webp"]

    os.makedirs(dst, exist_ok=True)
    # 与 pack.py 同一组编码参数（lossless WebP ⇒ 像素必须逐位可复现）
    for name, block in zip(names, halves):
        Image.fromarray(block, "RGBA").save(
            os.path.join(dst, name), format="WEBP", lossless=True, quality=90
        )

    # polygon.json 原样拷：格键是**全局行号**，拆分不改行号 ⇒ 也不需要改它
    poly_src = os.path.join(src, "polygon.json")
    if os.path.exists(poly_src):
        with open(poly_src, encoding="utf-8") as fh:
            poly = json.load(fh)
        with open(os.path.join(dst, "polygon.json"), "w", encoding="utf-8") as fh:
            json.dump(poly, fh, ensure_ascii=False, separators=(",", ":"))

    out_manifest = {
        "format": 3,
        "slug": manifest["slug"],
        "display_name": manifest.get("display_name"),
        "sheets": [
            {"file": names[0], "rows": args.at},
            {"file": names[1], "rows": rows - args.at},
        ],
        "grid": grid,
        "states": manifest["states"],
    }
    for k in ("facing", "anchor", "outfit_anchors", "input_polygon", "petdex_compatible"):
        if k in manifest:
            out_manifest[k] = manifest[k]
    problems = sheet_layout.check(out_manifest)
    if problems:
        sys.exit("产出的清单自己都不合法: %s" % "; ".join(problems))
    with open(os.path.join(dst, "pet.json"), "w", encoding="utf-8") as fh:
        json.dump(out_manifest, fh, ensure_ascii=False, indent=2)
        fh.write("\n")

    # **自校验：切完再读回来**。判据是"预乘后可见像素逐位相同"，不是"RGBA 字节逐位相同"——
    # libwebp 的编码路径会预乘 alpha，所以 a=0 那些格子的 RGB 会被归零（本轮实测 83,928 格，
    # 全在 alpha==0 上，半透明与不透明 0 格）。客户端本来就在 `atlas.rs` 里做一次预乘，
    # 而命中测试只看 alpha ⇒ 这批字节差在屏上是零贡献。若哪天差异跑到 a>0 上，这里必须红。
    back = [np.asarray(Image.open(os.path.join(dst, n)).convert("RGBA")) for n in names]
    joined = np.concatenate(back, axis=0)
    if joined.shape != arr.shape:
        sys.exit("拼回来的形状就变了: %r vs %r" % (joined.shape, arr.shape))
    alpha_bad = int((joined[..., 3] != arr[..., 3]).sum())
    premul = lambda a: (a[..., :3].astype(np.int32) * a[..., 3:4]).astype(np.int64)
    color_bad = int((premul(joined) != premul(arr)).any(axis=-1).sum())
    transparent = joined[..., 3] == 0
    invisible = int((transparent[..., None] & (joined[..., :3] != arr[..., :3])).any(axis=-1).sum())
    raw_bad = int((joined != arr).any(axis=-1).sum())
    if alpha_bad or color_bad:
        sys.exit(
            "拆分后像素变了：alpha 不同 %d 格、预乘颜色不同 %d 格 ⇒ 编码或切点错了"
            % (alpha_bad, color_bad)
        )
    px_before = arr.shape[0] * arr.shape[1]
    px_after = sum(a.shape[0] * a.shape[1] for a in back)
    print(
        "SPLIT 完成 slug=%s %d 行 → %s（%d 行 + %d 行）；像素格 %d == %d；"
        "行表/格子/锚点/polygon 未改动，清单已写 format=3"
        % (
            manifest["slug"],
            rows,
            "+".join(names),
            args.at,
            rows - args.at,
            px_before,
            px_after,
        )
    )
    print(
        "SPLIT 像素对照：alpha 逐位相同、预乘颜色逐位相同；"
        "仅全透明格的 RGB 被编码器归零 %d 格（原始字节不同的格子共 %d 格，屏上零贡献）"
        % (invisible, raw_bad)
    )
    print("SPLIT 产物目录: %s" % dst)


if __name__ == "__main__":
    main()
