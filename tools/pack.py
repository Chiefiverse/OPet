"""把 bake.py 烘出的 PNG 帧拼成图集，并生成 pet.json 与 polygon.json。

用法：
    python3 tools/pack.py --pet cat_shiro
    python3 tools/pack.py --accessory cap_blue --slot head --body <本体>/pet.json \
                          --frames <配件帧目录> --out <配件产物目录> \
                          --body-frames <本体帧目录>        # 可选：把"同一台相机"钉成硬拒绝

行布局的唯一事实源是 `crates/petcore/assets/cat_shiro.pet.json`（那份被
assets_scheduler.rs 的测试钉着）。本脚本按 fixture 的状态顺序，把**已烘出帧**的状态
依次分配到物理行 0..N；所以只烘 idle 时得到 grid.rows=4 的合法子集清单，补烘更多状态
后行号自动前移，不需要在两个语言里各维护一份表。

`--accessory` 那条路径**不用 fixture、也不自派行号**：它抄 `--body` 那份本体清单的
`grid` 与逐状态 `rows/fps/loop`，并把帧贴到本体的**物理行号**上。理由是 D16 路线 B 的
地基（`accessory.rs::validate_against`）要求配件的 `rows` 与本体**逐位相等**，而自派行号
会让一份不从 `idle` 起的配件产出一张过不了那条校验的清单。

⚠️ 但**字段逐位相等不等于相机相等**：`--body-frames` 补的正是那一格，它比两批各自
`frames/meta.json` 的 `feet_px`（本项目唯一不可伪造的相机指纹，同 `bake.py` 测量轮那条
红线②），逐位不等就在写任何产物之前拒绝。不给这个参数则退回"只打印两个数、不判红"。
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
# catalog.rs::Slot 的五个变体（serde(rename_all = "lowercase")）。写第六种挂点在 Rust 侧是
# 解析失败，而这里先拒能给出"你写了什么、合法的是哪五个"这句话，而不是 serde 的字段错误。
ACCESSORY_SLOTS = ("head", "face", "neck", "back", "hand")


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


def load_outfit_anchors(frames_dir, cell):
    """读 <frames>/outfit.json 的 col 0 读数，写成清单里的 outfit_anchors。

    schema 是 `HashMap<String,[f32;2]>` —— 每个 slot 只有一对数、没有朝向维 ⇒ 只能对一个格子。
    这里选 col 0（`Facing::East`），与 `anchor.feet` 同一个约定（那也是 columns[0] 的读数），
    完整的八列 × 逐行数组留在 outfit.json 里，那才是事实源。值为 null 的 slot 直接不放
    （没有几何来源的挂点写进去就是魔数）。
    """
    path = os.path.join(frames_dir, "outfit.json")
    if not os.path.isfile(path):
        return {}
    with open(path, encoding="utf-8") as fh:
        data = json.load(fh)
    if data.get("cell") != cell:
        sys.exit(
            "outfit.json 记录的是 cell=%s，而这次的帧是 cell=%d ⇒ 那批挂点是在另一档格子上量的，"
            "写进清单会当场被内核的『挂点必须落在帧内』拒掉或放行错值。重跑 --outfit-measure-only。"
            % (data.get("cell"), cell)
        )
    anchors = {}
    for slot, pair in sorted((data.get("anchor") or {}).items()):
        if pair is None:
            continue
        point = [int(round(pair[0])), int(round(pair[1]))]
        if not (0 <= point[0] <= cell and 0 <= point[1] <= cell):
            sys.exit("挂点 %s=%s 不在 %dx%d 帧内，内核会拒绝（出处 %s）" % (slot, point, cell, cell, path))
        anchors[slot] = point
    if anchors:
        origin = data.get("anchor_from") or ["?", 0, 0]
        print(
            "PACK 挂点出处：%s（%s 取自状态 %s 第 %d 帧第 %d 列；其余 slot 无几何来源，省略）"
            % (
                path,
                " ".join("%s=%s" % kv for kv in sorted(anchors.items())),
                origin[0],
                origin[1],
                origin[2],
            )
        )
    return anchors


def load_body_manifest(path):
    """读**本体**的 `pet.json` —— 配件清单里 `grid` 与逐状态 `rows/fps/loop` 的唯一事实源。

    这份文件是 `AccessoryManifest::validate_against` 的另一侧，所以它自己必须先是一份**本体**
    清单：配件清单刻意没有 `anchor`/`facing`/`input_polygon`（那两个事实源正是 §9.2 第 17 条
    那次挂点账的成因），拿"有没有 `anchor.feet`"就分得开这两类文件，而它们的其他字段几乎同形。
    """
    if not os.path.isfile(path):
        sys.exit(
            "本体清单不存在: %s ⇒ --body 要的是不带 --accessory 那次 pack.py 产出的 pet.json" % path
        )
    with open(path, encoding="utf-8") as fh:
        body = json.load(fh)

    def bad(msg):
        sys.exit("--body %s: %s" % (path, msg))

    if "slot" in body:
        bad("这份清单有 slot ⇒ 它是一份**配件**清单，而 --body 要的是本体那份 pet.json")
    if not body.get("slug"):
        bad(
            "缺少 slug（或它是空串）⇒ 本体清单的内核校验同样要求 slug 非空（manifest.rs），"
            "而配件的 fits 默认就是抄它；没有 slug 就只能瞎写一个名字，那正是 11-03 那条空转门禁的形状"
        )
    anchor = body.get("anchor") or {}
    if "feet" not in anchor:
        bad("缺少 anchor.feet ⇒ 这不是本体清单（本体必须有脚锚），或者它没经过内核校验")
    grid = body.get("grid") or {}
    for k in ("cols", "rows", "w", "h"):
        if not isinstance(grid.get(k), int):
            bad("grid.%s 缺失或不是整数: %r" % (k, grid.get(k)))
    if grid["cols"] != FACING_COLUMNS:
        bad(
            "grid.cols=%d 而内核的 FACING_COLUMNS=%d ⇒ 本体清单自己就不合法，抄它产出的配件清单只会一样不合法"
            % (grid["cols"], FACING_COLUMNS)
        )
    if grid["w"] != grid["h"]:
        bad("grid 非方格 %dx%d ⇒ 本脚本两边都按一个 cell 数算" % (grid["w"], grid["h"]))
    states = body.get("states") or {}
    if "idle" not in states:
        bad("states 里没有 idle ⇒ 这份本体清单过不了内核校验（accessory.rs 的比对也要求有 idle 可比）")
    for name, track in states.items():
        rows = track.get("rows") or []
        if not rows:
            bad("状态 %s 的 rows 为空" % name)
        if max(rows) >= grid["rows"]:
            bad(
                "状态 %s 引用行 %d 越界（本体自己的 grid.rows=%d）⇒ 抄过去会产出一份同样越界的配件清单"
                % (name, max(rows), grid["rows"])
            )
    return body


def check_same_camera(acc_meta, frames_dir, body_frames):
    """把"两张图集由**同一台相机**烘出"从一句打印变成产任何文件之前的硬拒绝（`--body-frames`）。

    为什么字段相等不够：`AccessoryManifest::validate_against` 比的全是**清单字段**（grid、逐状态
    rows 逐位、fps、loop、fits），而"同一 `(row, col)` 是屏幕上同一块矩形"这件事真正依赖的是
    **产出那两批像素的相机**。配件那批若用漂移的相机烘、而这里又照抄本体的行表，两份清单可以
    逐字段一致而图集逐格对不齐 ⇒ 现有门禁一条都不会红，屏上则是一顶歪帽子（漂移若跟着朝向列变，
    侧视两列几乎对而正脸/背面歪，那种错人眼只看一列会看漏）。

    比的是 `meta.json` 里的 `feet_px` 而不是各个旋钮：`ortho_scale`/`target_z`/`face_rot_z`/模型
    归一化任何一条漂了都会改这个投影 ⇒ 一个数盖住全部来源，这也是本项目唯一不可伪造的相机指纹
    （`bake.py` 的测量轮红线②用的就是它，同一条 `!=` 断言）。⚠️ 它是 **rest pose** 下的量 ⇒ 这条
    守的是相机，不守姿态表（姿态表由行表逐位相等 + "整档重烘齐"那两条守着）。
    """
    path = os.path.join(body_frames, "meta.json")
    if not os.path.isfile(path):
        sys.exit(
            "--body-frames %s 下面没有 meta.json ⇒ 没法比相机。那个目录要的是**产出本体那批像素的"
            "那一次** bake.py 的帧目录（脚锚投影记在它 meta.json 的 feet_px 里），不是 pet.json 所在那层。"
            % body_frames
        )
    with open(path, encoding="utf-8") as fh:
        recorded = json.load(fh)
    want = recorded.get("feet_px")
    got = acc_meta.get("feet_px")
    if want is None:
        sys.exit(
            "%s 没记 feet_px ⇒ 本体那一批比不出相机（那是这个字段存在之前烘的，或不是 "
            "bake.py 现在这条路径产的）。要么换成有 feet_px 的那批，要么去掉 --body-frames 退回只打印。"
            % path
        )
    if got is None:
        sys.exit(
            "%s 没记 feet_px ⇒ 配件这批不是 bake.py --outfit-only 产的（那个字段由它写 "
            "meta 时投影）。没有可比的出处就别产这份产物。" % os.path.join(frames_dir, "meta.json")
        )
    if list(got) != list(want):
        sys.exit(
            "相机忠实性断言失败：配件批次把脚锚投到 %s（%s），而本体批次投到 %s（%s）。\n"
            "  ⇒ 两批像素不是同一台相机产的（model / face_rot_z / ortho_scale / target_z / cell 里"
            "至少一条漂了），此时逐格叠加的偏移**不恒为 0**，屏上配件会画在错位处而清单字段依然全对。\n"
            "  本次拒绝不写任何产物：用与本体那批相同的旋钮重跑 bake.py --outfit-only，或换本体批次。"
            % (got, os.path.join(frames_dir, "meta.json"), want, path)
        )
    print(
        "PACK 相机忠实性通过：两批 feet_px 逐位相等 %s（配件 %s / 本体 %s）⇒ 同一台相机，"
        "逐格叠加的偏移才真的是 0"
        % (got, frames_dir, body_frames)
    )


def discover_accessory_states(frames_dir, body):
    """按**本体清单**的状态顺序返回 [(状态名, 帧数)]，只含帧目录里真存在的状态。

    与本体路径那两条的区别都得是有意的：① 不要求 `idle`（配件允许只覆盖一部分状态，
    `accessory.rs` 模块头第 2 条）；② 帧数必须**等于本体那一档的行数**，一半都不行 ——
    见那句报错。
    """
    found = []
    declared = set(body["states"])
    for name in sorted(
        d for d in os.listdir(frames_dir) if os.path.isdir(os.path.join(frames_dir, d))
    ):
        if name not in declared:
            print(
                "PACK 提示：%s/%s 不在本体清单的 states 里 ⇒ 这一档不进配件清单"
                "（硬写进去会被 validate_against 以『声明了本体没有的状态』拒掉）"
                % (frames_dir, name)
            )
    for name, track in body["states"].items():
        d = os.path.join(frames_dir, name)
        if not os.path.isdir(d):
            continue
        frames = sorted(f for f in os.listdir(d) if f.endswith(".png"))
        if not frames:
            sys.exit(
                "状态目录 %s 里一张 PNG 都没有 ⇒ 要么删掉这个空目录（= 这一档不覆盖），要么把帧烘齐；"
                "pack 不替人猜你想不想覆盖它" % d
            )
        want = len(track["rows"])
        if len(frames) % FACING_COLUMNS:
            sys.exit(
                "状态 %s 的帧数 %d 不是 %d 的倍数（每帧一个朝向列）" % (name, len(frames), FACING_COLUMNS)
            )
        got = len(frames) // FACING_COLUMNS
        if got != want:
            sys.exit(
                "状态 %s 烘了 %d 帧，而本体清单 %s 声明 %d 帧 ⇒ 整档重烘齐，或者把这个状态目录删掉。\n"
                "    为什么半档不行：抄过去的行表是本体那 %d 行，缺的帧在图集里就是全透明格 ⇒\n"
                "    那一帧配件凭空消失，而两份清单的 rows 仍然逐位相等 ⇒ 现有门禁一条都不会红。"
                % (name, got, track["rows"], want, want)
            )
        found.append((name, got))
    if not found:
        sys.exit(
            "帧目录 %s 里没有任何一档能对上本体清单的 states ⇒ 这一批帧烘的是别的宠物，"
            "或者 --state 给全跳过了" % frames_dir
        )
    return found


def build_accessory_sheet(states, frames_dir, cell, body):
    """把配件帧贴到**本体的物理行号**上 ⇒ 两边同一 `(row, col)` 就是屏幕上同一块矩形。

    这条路径**不复用** `build_sheet`：那个的 `row_map[name] = list(range(cursor, cursor + n))`
    是顺序自派行号，对"从 idle 起排"的本体子集是对的，对配件则是错的 —— 一顶只在 `walk` 烘过的
    帽子会被分到第 0 行，而本体清单写着第 4–11 行，于是那份清单过不了 `validate_against`
    （或者更糟：过得了，因为它抄的是自派的那份）。行号在这里不是排版细节，是那条校验的全部内容。
    """
    row_map = {name: list(body["states"][name]["rows"]) for name, _ in states}
    grid_rows = 1 + max(row for rows in row_map.values() for row in rows)
    sheet = Image.new("RGBA", (FACING_COLUMNS * cell, grid_rows * cell), (0, 0, 0, 0))
    placed = 0
    for name, n in states:
        for frame in range(n):
            row = row_map[name][frame]
            for col in range(FACING_COLUMNS):
                img = read_cell(frames_dir, name, col, frame)
                if img.size != (cell, cell):
                    sys.exit("帧 %s 尺寸 %s != cell %d" % (name, img.size, cell))
                sheet.paste(img, (col * cell, row * cell))
                placed += 1
    return sheet, row_map, grid_rows, placed


def pack_accessory(args):
    """`--accessory` 那一档：产 `accessory.json` + `sheet.webp` + `polygon.json` 三件同目录的产物。

    同目录不是风格问题：`app.rs::load_accessory` 把后两件硬写成清单的兄弟路径
    （`dir.join("sheet.webp")` / `dir.join("polygon.json")`），分开放就是"清单加载成功而装备失败"。
    """
    body = load_body_manifest(args.body)
    # `fits` 的默认 = **`--body` 那份清单上的 slug**，而 2026-11-04 起那个章是本体真的身份
    # （见 `main()` 里"本体身份 = --pet"那一段）。这条在 11-03 之前是**空转门禁**：八份本体产物
    # 全被盖成 fixture 的 `cat_shiro` ⇒ 一顶为老虎烘的帽子章上也写着 cat_shiro，于是它对今天
    # 四只模型之间的错配一个都拦不住（`accessory.rs::validate_against` 那句"这件配件不是为这只
    # 宠物烘的"永远不会响）。
    #
    # 这道判断放在**任何磁盘写入与逐格解码之前**：它只读两个字符串，而它拒的是"这份清单刚被
    # 要求抄它、刚被要求与它同相机，产出后却必然在装载那一刻被内核拒掉"。付完 320 格再被告知
    # 就是 `--cell` 那条"一行就能判出来的错先付装载代价"的同一种浪费。
    fits = [s.strip() for s in args.fits.split(",") if s.strip()] or [body["slug"]]
    if body["slug"] not in fits:
        sys.exit(
            "--fits %s 不含 --body 那一只 %r ⇒ 这份清单刚被要求抄它、刚被要求与它同相机，\n"
            "    而产出的配件在装载那一刻必然被 `validate_against` 拒掉。要么去掉 --fits 用默认，\n"
            "    要么把 %r 写进去（真的要多本体通用就逐个列全）。\n"
            "    注意这一拒同时也是「本体那份章可能是错的」的信号：先 grep 一下 %s 里的 slug。"
            % (fits, body["slug"], body["slug"], args.body)
        )
    out_dir = args.out or os.path.join(REPO, "assets", "accessories", args.accessory)
    frames_dir = args.frames or os.path.join(out_dir, "frames")
    os.makedirs(out_dir, exist_ok=True)

    meta = load_meta(frames_dir)
    cell = meta["cell"]["w"]
    if meta["cell"]["h"] != cell:
        sys.exit("meta.json 的 cell 非方格: %s" % meta["cell"])
    if cell != body["grid"]["w"]:
        sys.exit(
            "配件批次烘在 cell=%d 上，而本体 %s 的 grid.w=%d ⇒ 同一格不是同一块矩形，逐格叠加的偏移"
            "不再是 0（内核那条『格子不一致』的校验会拒绝装备）。用同一次 --cell 重烘配件，或换本体清单。"
            % (cell, args.body, body["grid"]["w"])
        )
    if not meta.get("outfit_only"):
        sys.exit(
            "%s 这批帧不是配件图集：meta.json 里 outfit_only=%r。\n"
            "    ⇒ 那是「帽子烘进本体像素」那一批（路线 A），它每一格里已经有身体，叠在本体上等于画两遍身体。\n"
            "    要产配件图集就重跑 bake.py 并把 --outfit-only 一起给（它 hide_render 藏载体，只渲配件）。"
            % (os.path.join(frames_dir, "meta.json"), meta.get("outfit_only"))
        )
    cols = sorted(meta.get("columns") or [])
    if cols != list(range(FACING_COLUMNS)):
        sys.exit(
            "这批配件帧只渲了朝向列 %s，而清单按 grid.cols=%d 产出 ⇒ 缺的列在图集里是全透明格，"
            "那几个朝向上配件会直接消失，没有任何门禁会报。重跑 bake.py，别给 --facing 限制（默认八列齐）。"
            % (cols, FACING_COLUMNS)
        )

    # 相机比对放在**读帧与写文件之前**：它拒的是"这批帧根本没有可比的本体批次"，那种事不该先付
    # 一遍 320 格解码与拼 sheet 的代价才被告知（同 `--cell` 那条"一行就能判出来的错先付装载代价"）。
    if args.body_frames:
        check_same_camera(meta, frames_dir, args.body_frames)

    states = discover_accessory_states(frames_dir, body)
    sheet, row_map, grid_rows, placed = build_accessory_sheet(states, frames_dir, cell, body)
    if grid_rows > body["grid"]["rows"]:
        sys.exit(
            "配件图集要 %d 行，而本体清单只有 %d 行 ⇒ 抄来的行号越过了本体自己的图集。"
            "这份本体清单没过内核校验（`Manifest::validate` 要求 max(rows) < grid.rows），先修它。"
            % (grid_rows, body["grid"]["rows"])
        )

    # 配件清单**刻意没有** anchor / facing / input_polygon / outfit_anchors 四样：定位与朝向全部
    # 借用本体的（两张图集同一台相机 ⇒ 同一个锚点），自己存一份就是第二个事实源。
    manifest = {"format": body["format"], "slug": args.accessory}
    if args.display_name:
        manifest["display_name"] = args.display_name
    manifest["slot"] = args.slot
    # `fits` 与"必须含本体 slug"那道拒绝都在函数开头（读两个字符串就能判，不该付解码代价）。
    manifest["fits"] = fits
    manifest["sheet"] = "sheet.webp"
    manifest["grid"] = {"cols": FACING_COLUMNS, "rows": grid_rows, "w": cell, "h": cell}
    manifest["states"] = {
        name: {
            "rows": row_map[name],
            "fps": body["states"][name]["fps"],
            "loop": bool(body["states"][name].get("loop", False)),
        }
        for name, _ in states
    }

    sheet_path = os.path.join(out_dir, "sheet.webp")
    sheet.save(sheet_path, format="WEBP", lossless=True, quality=90)

    arr = np.asarray(sheet)
    poly = {"cell": {"w": cell, "h": cell}, "threshold": ALPHA_THRESHOLD, "frames": []}
    for name, n in states:
        for frame in range(n):
            row = row_map[name][frame]
            for col in range(FACING_COLUMNS):
                block = arr[row * cell : (row + 1) * cell, col * cell : (col + 1) * cell, 3]
                poly["frames"].append({"row": row, "col": col, "spans": column_spans(block)})

    manifest_path = os.path.join(out_dir, "accessory.json")
    with open(manifest_path, "w", encoding="utf-8") as fh:
        json.dump(manifest, fh, ensure_ascii=False, indent=2)
        fh.write("\n")
    with open(os.path.join(out_dir, "polygon.json"), "w", encoding="utf-8") as fh:
        json.dump(poly, fh, ensure_ascii=False)

    webp_bytes = os.path.getsize(sheet_path)
    decoded = FACING_COLUMNS * cell * grid_rows * cell * 4
    print(
        "PACK 配件 %s(slot=%s fits=%s): %d 状态 %s / %d 格 → sheet %dx%d，行号抄自 %s"
        % (
            args.accessory,
            args.slot,
            ",".join(manifest["fits"]),
            len(states),
            ",".join(n for n, _ in states),
            placed,
            FACING_COLUMNS * cell,
            grid_rows * cell,
            args.body,
        )
    )
    print(
        "PACK 逐状态行表（必须与本体逐位相等 ⇒ validate_against 的那五条比对才有的可比）：%s"
        % " ".join("%s=%s" % (n, row_map[n]) for n, _ in states)
    )
    # 相机出处的两种口径要分得开：给了 --body-frames ⇒ 上面已经按**两批各自的 feet_px**（浮点）
    # 逐位比过并可能已经拒绝；没给 ⇒ 这里只能拿本体的 `anchor.feet`（那是 pack.py 把 feet_px
    # int(round()) 之后的值）并排打出来，那两个数**本来就不该相等**，所以它不是判据、只是线索。
    if args.body_frames:
        print("PACK 相机出处已由 --body-frames %s 逐位校验过 ⇒ 上面那行「忠实性通过」是结论" % args.body_frames)
    else:
        print(
            "PACK 相机出处**只打印、没判红**（没给 --body-frames）：配件批次 feet_px=%s（%s）/ 本体清单"
            " anchor.feet=%s（%s，已 round 过所以不可逐位比）。⇒ 两份清单字段全对而两批像素不是同一台"
            "相机烘的，现有门禁一条都不会红，屏上是一顶歪帽子。要把它钉成拒绝就加 "
            "--body-frames <本体批次 frames>"
            % (meta.get("feet_px"), os.path.join(frames_dir, "meta.json"),
               body["anchor"]["feet"], args.body)
        )
    print(
        "PACK sheet.webp = %d B (%.1f KB)，解码后 RGBA = %.3f MB"
        % (webp_bytes, webp_bytes / 1024, decoded / 1048576)
    )
    print("PACK → %s（accessory.json + sheet.webp + polygon.json，三件必须同目录）" % out_dir)


def main():
    ap = argparse.ArgumentParser(description="OPet 图集打包")
    ap.add_argument("--pet", default="", help="本体宠物 slug（不带 --accessory 那一档）")
    ap.add_argument("--frames", default="", help="帧目录（默认 <产物目录>/frames）")
    ap.add_argument("--out", default="", help="产物目录")
    ap.add_argument("--accessory", default="", help="配件 slug；给了它就走配件那一档")
    ap.add_argument("--slot", default="", help="配件挂点，五值之一: " + " / ".join(ACCESSORY_SLOTS))
    ap.add_argument("--body", default="", help="本体 pet.json；配件的行布局与格子全抄它")
    ap.add_argument(
        "--body-frames",
        default="",
        help="本体批次的 frames 目录；给了它就把两批的 feet_px 逐位比一遍，不同即拒绝（不写任何文件）",
    )
    ap.add_argument(
        "--display-name",
        default="",
        help="正式显示名（两档都能给）：本体档不给就用 fixture 的名字（只有 --pet 就是 fixture "
        "那一只时）或 slug 本身；配件档不给就省略这个字段",
    )
    ap.add_argument("--fits", default="", help="逗号分隔的适配本体 slug；默认 = --body 那一只")
    args = ap.parse_args()

    if args.accessory:
        if args.pet:
            sys.exit(
                "--accessory %s 与 --pet %s 同时给了 ⇒ 不知道要打本体图集还是配件图集。"
                "配件那一档用 --body 指本体清单。" % (args.accessory, args.pet)
            )
        if not args.slot:
            sys.exit(
                "--accessory %s 缺 --slot ⇒ 合法五值：%s（与 accessories.slot 的 CHECK 同源）"
                % (args.accessory, " / ".join(ACCESSORY_SLOTS))
            )
        if args.slot not in ACCESSORY_SLOTS:
            sys.exit(
                "--slot %s 不是合法挂点；合法五值：%s" % (args.slot, " / ".join(ACCESSORY_SLOTS))
            )
        if not args.body:
            sys.exit(
                "--accessory %s 缺 --body ⇒ 配件清单的 grid 与逐状态 rows/fps/loop 必须**抄**本体那份，"
                "没有本体就只能自派行号，而自派出来的清单过不了 validate_against。" % args.accessory
            )
        pack_accessory(args)
        return

    # ⚠️ `--display-name` **不在这一串里**：它两档都用（本体那一档给 `body_name` 用，见下面
    # 「身份与行布局分家」那段）。把它留在"只在 --accessory 有意义"的名单里 = 一条**永远拦住
    # 合法用法**的门禁，而本体档唯一能盖对中文名的路就被它自己堵死（假开关同族）。
    if args.slot or args.body or args.body_frames or args.fits:
        stray = " ".join(
            flag
            for flag, on in (("--slot", args.slot), ("--body", args.body), ("--body-frames", args.body_frames), ("--fits", args.fits))
            if on
        )
        sys.exit(
            "%s 只在 --accessory 那一档有意义，而这次没给 --accessory ⇒ 打了本体图集，"
            "那几个参数会被完全忽略。要么补 --accessory，要么把它们去掉。" % stray
        )

    # **身份必须显式**（拍板③的连带一半）：`--pet` 原先只用来拼默认路径，八份产物
    # （`rabit / spacecat / tiger` 两档）当初都是走文档里那条 `--frames X --out Y`（不带 `--pet`）
    # 烘的 ⇒ 默认值 `cat_shiro` 一路盖到每一只身上。只修 `slug` 那一个赋值不够：**路径被重定向后
    # 默认 slug 就不再指向任何东西**，它是一条没人认领的章。所以这里宁可直接拒绝，也不猜目录名
    # （`rabit_192` ≠ slug `rabit`；猜出来的是第二条假章，只是换了个写法）。
    pet = args.pet
    if not pet:
        if args.frames or args.out:
            redirected = " ".join(
                flag for flag, on in (("--frames", args.frames), ("--out", args.out)) if on
            )
            sys.exit(
                "给了 %s 而没给 --pet ⇒ 这次打的图集**叫什么**没有任何输入说得出。\n"
                "    以前它默默用默认值 cat_shiro，于是给老虎/兔子/太空猫烘的八份产物全被盖成\n"
                "    「小白」，而配件那一档的 fits 抄的还是同一个错章 ⇒ validate_against 空转。\n"
                "    补 --pet <本体 slug>（正式名另给 --display-name）。目录名不能当 slug：\n"
                "    assets/pets/tiger_192 那只的 slug 是 tiger，不是 tiger_192。" % redirected
            )
        pet = "cat_shiro"
    fixture = load_fixture()
    frames_dir = args.frames or os.path.join(REPO, "assets", "pets", pet, "frames")
    out_dir = args.out or os.path.join(REPO, "assets", "pets", pet)
    os.makedirs(out_dir, exist_ok=True)

    meta = load_meta(frames_dir)
    cell = meta["cell"]["w"]
    if meta["cell"]["h"] != cell:
        sys.exit("meta.json 的 cell 非方格: %s" % meta["cell"])
    if cell != fixture["grid"]["w"]:
        print(
            "PACK 提示：烘焙格子 %d 与 fixture grid.w %d 不一致 ⇒ 产物 grid.w/h 按 %d 写；"
            "fixture 仍是行布局（states.*.rows）的唯一事实源"
            % (cell, fixture["grid"]["w"], cell)
        )

    states = discover_states(frames_dir, fixture)
    sheet, row_map, placed = build_sheet(states, frames_dir, cell)

    fx, fy = meta["feet_px"]
    anchor = [int(round(fx)), int(round(fy))]
    if not (0 <= anchor[0] <= cell and 0 <= anchor[1] <= cell):
        sys.exit("anchor.feet %s 不在 %dx%d 帧内，内核会拒绝" % (anchor, cell, cell))

    # ⚠️ **身份与行布局分家**（拍板③，11-03 实测抓到的那条空转门禁的根）：fixture 那份
    # `cat_shiro.pet.json` 是**行布局**（`states.*.rows` / fps / loop / facing）的唯一事实源，
    # 它**从来不是身份的事实源** —— 原先这里写 `"slug": fixture["slug"]`，于是
    # `rabit / spacecat / tiger` 两档共八份产物全被盖成 `cat_shiro`，`display_name` 也全叫「小白」。
    # 连带后果是配件那一档 `fits=[body["slug"]]` 抄的还是同一个错章 ⇒ `validate_against`
    # （`accessory.rs:157`）对今天这四只模型之间的错配**一个都拦不住**：一顶为老虎烘的帽子装到
    # 兔子身上全绿灯，只有像素（两批 `feet_px`）会露馅。所以本体身份 = `--pet` 那一只。
    #
    # 这两列**不进指纹**：`Sim::fingerprint()`（`sim.rs:207-228`）混的是 tick / state / pos / vel /
    # facing / rng.counter / `last_sprite` 的 row·col·frame，一个字符串都不读 ⇒ 改章不动基线
    # `ea97139205536cd8`（这条由本文件末尾那条对照验过，不是推论）。
    body_slug = pet
    if body_slug == fixture.get("slug"):
        # 只有"这一份就是 fixture 那一只"时，fixture 的正式中文名才属于它。
        body_name = args.display_name or fixture.get("display_name", body_slug)
    elif args.display_name:
        body_name = args.display_name
    else:
        # 不认得的名字**不编**：把「小白」发给一只老虎，比只写 slug 更坏（那是第二条假章）。
        body_name = body_slug
        print(
            "PACK 提示：--pet %s 与 fixture 的 %s 不是同一只 ⇒ 不继承 display_name「%s」，"
            "这一栏先写 slug；要正式中文名就加 --display-name。"
            % (body_slug, fixture.get("slug"), fixture.get("display_name"))
        )

    manifest = {
        "format": fixture["format"],
        "slug": body_slug,
        "display_name": body_name,
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
    # outfit_anchors 的唯一生产者：tools/bake.py --outfit-measure-only 写出的 outfit.json。
    # 没有那份文件就省略这个字段（serde(default) 合法），别手抄数字 —— validate 第 15 项会查出处。
    anchors = load_outfit_anchors(frames_dir, cell)
    if anchors:
        manifest["outfit_anchors"] = anchors

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
            pet,
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
