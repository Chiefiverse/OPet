"""在 Blender 内执行的序列帧烘焙：模型 → 8 朝向 × N 帧 PNG。

用法（本机 headless 已验证）：
    blender -b --factory-startup --python tools/bake.py -- --state idle
    blender -b --factory-startup --python tools/bake.py -- --model /path/cat.glb \
        --face-rot-z 90 --state idle --action-map idle=HumanDrunkIdle

不给 --model 时烘的是本脚本用图元搭出来的程序化占位小白（姿态由 apply_pose 参数化）。
给了 --model 就装载外部资产：脚底归零、xy 居中、按 --face-rot-z 把脸转到 +X。姿态来源二选一，
不给 --action-map 就用骨架自带的骨按语义角色（BONE_ROLES）跑参数化姿态，九支状态全部由
apply_bone_pose 生成（2026-10-26 从只支持 idle 扩齐；缺某支必需的骨会按 STATE_BONE_NEEDS 当场
报错，而不是静默烘成一个不动的 rest pose）；给了 --action-map 就采样模型自带剪辑。

状态行数与格子尺寸都读 `crates/petcore/assets/cat_shiro.pet.json`，不在本文件里复制
一份 0–39 行布局 —— 那份布局被 assets_scheduler.rs 的测试钉着，两处各写必然漂移。

材质注记：5.2 已移除 Shader-to-RGB（实测 `hasattr(bpy.types,'ShaderNodeToRGB')==False`），
所以占位小白的卡通感走 Principled 平涂 + 定向光，不做色阶分档。外部模型保留它自己的材质。
"""

import argparse
import json
import math
import os
import sys

import bpy
from bpy_extras.object_utils import world_to_camera_view
from mathutils import Matrix, Vector

HERE = os.path.dirname(os.path.abspath(__file__))
REPO = os.path.dirname(HERE)
FIXTURE = os.path.join(REPO, "crates", "petcore", "assets", "cat_shiro.pet.json")

# 模型的"脸朝"= 世界 +X。朝向自检与锚点都由这个约定推出来。
NOSE = Vector((0.34, 0.0, 0.80))
REAR = Vector((-0.34, 0.0, 0.42))
FEET = Vector((0.0, 0.0, 0.0))

CAM_RADIUS = 6.0
CAM_ELEV_DEG = 15.0
ORTHO_SCALE = 1.55
TARGET_Z = 0.52
# 占位小白（build_cat 搭出来的那副图元）从脚底到耳尖的高度，只用来给外部模型推一个
# ortho_scale 初值：1.55 / 1.07 这一档比例是第二批按"全朝向全帧留边 ≥1px"试出来的。
PLACEHOLDER_H = 1.07

# 配件（--outfit）定标与落座的两个系数。参照物是**看得见的头**（head_region 从宠物网格几何
# 量出来的那颗），不是 head 骨：太空猫那份 rig 的 head 骨在 z 0.71 而颅顶在 1.83，而头顶
# 顶点 100% 归 Spine2 管 ⇒ 骨长既不是"头颅的尺寸"也不是"头颅在哪"（两条都是实测）。
OUTFIT_SIZE_RATIO = 1.25  # 配件**整组水平足迹** = 该倍数 × 头宽（head_region 量出来的那颗头）
# ⚠️ 这条参考量**不是帽檐直径**：定标取的是 max(x1-x0, y1-y0) = 整组（含歪出去的绒球）的
# 水平足迹，而不是最低那件（帽圈）的直径。两者在这份圣诞帽上差 5.341 vs 3.872 ⇒ 按本公式
# 定标后**帽檐实际只有 0.91 × 头宽**，不是字面上的 1.25。测量会把两个量都打进 outfit.json，
# 别再拿"1.25 × 头宽"读成帽檐。（要不要改成按帽圈定标 = 美术口径，等用户判，不动默认值。）
OUTFIT_SEAT = 0.40  # 帽檐落在头区的哪一段（0=颈收缩处，1=颅顶）；0.4 ≈ 包住颅骨下半
NECK_SLOT_SEAT = 0.50  # neck 这个 slot 没有配件几何可量，取 Neck 骨中段（领圈的位置）
# 颈收缩的拓扑判据：从颅顶往下，宽度超过"到目前为止最窄那带"的 NECK_WIDEN 倍且**连续三带**
# 都超 ⇒ 认定颈找到了。1.15 是"耳朵/发髻的抖动"与"肩膀"分得开的最小档（太空猫 1.302 头宽
# vs 肩更宽、老虎 0.79 头 vs 肩甲 2.313，两支都量得开）。
NECK_WIDEN = 1.15

# Facing::index() 顺时针：E=0 SE=1 S=2 SW=3 W=4 NW=5 N=6 NE=7
# 列 → 鼻尖应落在屏幕哪一侧（S/N 是正对与背对，左右本来就该接近 0）
FACE_SCREEN_SIDE = {0: 1, 1: 1, 2: 0, 3: -1, 4: -1, 5: -1, 6: 0, 7: 1}


def parse_args():
    argv = sys.argv
    argv = argv[argv.index("--") + 1 :] if "--" in argv else []
    p = argparse.ArgumentParser(description="OPet 序列帧烘焙（Blender 内执行）")
    p.add_argument("--state", default="idle", help="逗号分隔，如 idle,walk")
    p.add_argument("--facing", default="0,1,2,3,4,5,6,7", help="逗号分隔的列号")
    p.add_argument("--cell", type=int, default=0, help="0 = 用 fixture 的 grid.w/h")
    p.add_argument(
        "--out", default=os.path.join(REPO, "assets", "pets", "cat_shiro", "frames")
    )
    p.add_argument("--samples", type=int, default=16)
    p.add_argument("--force", action="store_true", help="重烘已存在的帧")
    p.add_argument(
        "--skip-facing-check", action="store_true", help="跳过朝向断言（调试用）"
    )
    p.add_argument(
        "--skip-framing-check", action="store_true", help="跳过构图/可见性断言（调试用）"
    )
    p.add_argument(
        "--check-only",
        action="store_true",
        help="只跑朝向/构图自检就退出（换模型调边距时不用等渲染）",
    )
    p.add_argument(
        "--model",
        default="",
        help="装载外部模型（.blend/.glb/.gltf）；不给就用本脚本程序化的占位小白",
    )
    p.add_argument(
        "--action-map",
        default="",
        help="状态→剪辑名，逗号分隔，如 idle=HumanDrunkIdle,walk=HumanWalking",
    )
    p.add_argument(
        "--face-rot-z",
        type=float,
        default=0.0,
        help="装载后绕世界 Z 轴转多少度让脸朝 +X（脸朝 -Y 就填 90）",
    )
    p.add_argument(
        "--ortho-scale",
        type=float,
        default=0.0,
        help="正交相机尺度；0 = 外部模型按高度自动定初值，占位小白用 %s" % ORTHO_SCALE,
    )
    p.add_argument(
        "--target-z",
        type=float,
        default=-1.0,
        help="相机注视高度；<0 = 外部模型自动取高度的一半，占位小白用 %s" % TARGET_Z,
    )
    p.add_argument("--nose", default="", help="鼻尖世界坐标 x,y,z；留空 = 从网格顶点自动取")
    p.add_argument("--rear", default="", help="尾部世界坐标 x,y,z；留空 = 从网格顶点自动取")
    p.add_argument(
        "--lock-root-motion",
        action=argparse.BooleanOptionalAction,
        default=True,
        help="把位移型剪辑（走路/跑步）的水平漂移拉回原地",
    )
    p.add_argument(
        "--plant-feet",
        action=argparse.BooleanOptionalAction,
        default=False,
        help="每帧把最低点压回 z=0（默认关）。绑骨不干净的外部模型需要它：太空猫贴地的那圈顶点"
        "本应只受腿骨影响，但有一部分被 Spine2 的权重梯度覆盖 ⇒ 胸腔一抬、脚就跟着离地 "
        "2–4 px（validate 的'idle 底边抖动 ≤2px' 当场红）。开着它烘等于用烘焙端盖住模型的权重"
        "缺陷，补偿量会打出来（本模型 ≈2px，代价是头顶峰峰从 5.8 掉到 3.9 px）；正式模型应当"
        "把脚部顶点权重清到只剩腿骨",
    )
    p.add_argument(
        "--bones",
        default="",
        help="语义角色→骨名覆盖，如 chest=Spine2,head=Head；不给就按候选名自动对",
    )
    p.add_argument(
        "--bone-gain",
        type=float,
        default=1.0,
        help="参数化骨骼姿态的总幅度倍率（观感旋钮，试不同档不用改代码）",
    )
    p.add_argument(
        "--outfit",
        default="",
        help="装载第二份网格作为配件（.blend/.glb/.gltf），刚性附着到 head 骨上",
    )
    p.add_argument(
        "--outfit-objects",
        default="",
        help="配件白名单（逗号分隔的物体名）；白名单外逐个打印后删掉。"
        "按有无材质筛在这里不行：帽子的背景板也带材质",
    )
    p.add_argument(
        "--outfit-scale",
        type=float,
        default=0.0,
        help="配件缩放；0 = 自动，按看得见的头定标（**整组水平足迹** = %g × 头宽；不是帽檐直径，见 OUTFIT_SIZE_RATIO）" % OUTFIT_SIZE_RATIO,
    )
    p.add_argument(
        "--outfit-rot-z",
        type=float,
        default=180.0,
        help="配件绕世界 Z 轴转多少度（归一化后脸朝 +X ⇒ 180 = 垂下来的那头朝背）",
    )
    p.add_argument(
        "--outfit-tilt",
        type=float,
        default=0.0,
        help="配件绕**自己的局部 Y 轴**倾多少度（在链里于 --outfit-rot-z 之前作用于帽子自身坐标，"
        "所以倒的是「整顶帽子」而不是「世界的一半」）：正数把最高点朝 +X 那侧拉。"
        "唯一用途是把最高点从格子顶边拉回来（jump 背面列实测被平切），默认 0 = 一字不变。",
    )
    p.add_argument(
        "--outfit-anchor",
        default="",
        help="挂点取配件自身哪个点 x,y,z（装载后、任何配件变换前的世界坐标）；"
        "留空 = 自动取帽檐圈心（整组里最矮那件网格的包围盒中心）",
    )
    p.add_argument(
        "--outfit-seat",
        type=float,
        default=OUTFIT_SEAT,
        help="帽檐落在看得见的头的哪一段：0 = 颈收缩处、1 = 颅顶（区间由 head_region 量）；默认 %g" % OUTFIT_SEAT,
    )
    p.add_argument(
        "--outfit-subsurf",
        type=int,
        default=-1,
        help="把配件所有细分修改器钉到这一档（0 = 原始网格，最大 6）；-1 = 沿用配件文件自带的档。"
        "配件的细分档是每格烘焙成本的最大单项（这份 3 档 = 56.9 s/格 vs 不戴帽 0.895）；"
        "但**顶点数不能按正比换算成秒**（实测四档双对数斜率 ≈0.35、档 0 仍有 ≈10 s/格地板）"
        "⇒ 换一份配件文件先看这两行打印，要 s/格 只能实测",
    )
    p.add_argument(
        "--outfit-measure-only",
        action="store_true",
        help="只对已烘产物做纯测量（一个像素都不重渲）：按 <out>/meta.json 还原场景、"
        "断言 feet_px 逐位相等、逐状态逐列投影挂点、写 <out>/outfit.json 后退出",
    )
    p.add_argument(
        "--outfit-only",
        action="store_true",
        help="只渲配件、载体不进画面（各 `hide_render`），烘出来的是**配件图集**"
        "（D16 路线 B：客户端逐格把这张叠在本体那张上）。相机、姿态、格子、列约定与本体那批"
        "**完全同一套** ⇒ 同一 (row, col) 就是屏幕上同一块矩形，这是路线 B 的地基。"
        "配套的清单生产者 = `tools/pack.py --accessory`",
    )
    return p.parse_args(argv)


def load_fixture():
    with open(FIXTURE, encoding="utf-8") as fh:
        return json.load(fh)


def wipe_scene():
    for ob in list(bpy.data.objects):
        bpy.data.objects.remove(ob, do_unlink=True)
    for block in (bpy.data.meshes, bpy.data.materials, bpy.data.lights, bpy.data.cameras):
        for item in list(block):
            if item.users == 0:
                block.remove(item)


# ── 外部模型（--model）：装载、归一化、骨骼剪辑采样 ─────────────────────────
def world_of(ob):
    """读**评估后**的世界矩阵 —— 凡是"约束/修改器算完之后长在哪"的判据，只能从这里读。

    事故：CHILD_OF 的 inverse 配方把缩放平方了，帽子在像素里根本不存在，而三条自检全绿。
    根因是 `ob.matrix_world` 在刚加完约束的那一轮更新里可能还是**加约束之前**的旧值 ⇒
    "挂点没漂""跟着骨移动""尺寸恒定"三条量的是那份没被约束改过的矩阵，渲染器画的是另一份。
    这里只取 matrix_world，不调 `to_mesh()` ⇒ 不重建修改器栈（那条路实测 20 分钟）。
    """
    return ob.evaluated_get(bpy.context.evaluated_depsgraph_get()).matrix_world


def mat_resid(a, b):
    """两个矩阵/向量的逐元素最大绝对差。两个坑都踩过：
    `(a - b).length` 在 **Matrix** 上没有这个属性；`tuple(Matrix)` 吐的是**行（Vector）**
    而不是一排浮点数 ⇒ `abs()` 收到 Vector 当场 TypeError。"""
    d = a - b
    vals = [v for row in d for v in row] if isinstance(d, Matrix) else list(d)
    return max(abs(v) for v in vals)


def evaluated_points(ob, limit=4000):
    """评估后的网格顶点（含骨架变形），等步长抽样到最多 limit 个。

    不用 ob.bound_box：绑定骨架的网格那份是 rest pose 的框，摆完姿势它不变，
    于是"姿态把模型甩出格子"这类问题会查不出来。
    """
    ev = ob.evaluated_get(bpy.context.evaluated_depsgraph_get())
    me = ev.to_mesh()
    step = max(1, len(me.vertices) // limit)
    pts = [ev.matrix_world @ me.vertices[i].co for i in range(0, len(me.vertices), step)]
    ev.to_mesh_clear()
    return pts


def rigid_points(ob, limit=3000):
    """刚体配件的取点：**基网格顶点 × matrix_world**，一次都不评估修改器。

    不能用 evaluated_points：xmashat.blend 六件网格全带 SUBSURF，Torus.001 基网格 9,732
    顶点评估后是 **930,224**，而 `to_mesh()` 每次调用都重建整条修改器栈 ⇒ 配件自检要遍历
    全部姿态 × 8 列，实测烧掉 20 分钟而零输出。
    配件不蒙皮，它跟着头骨动的唯一路径是 CHILD_OF，而那份结果只在**评估后的** `matrix_world`
    里（见 world_of 那条事故：裸 `ob.matrix_world` 可能还是加约束前的旧值）。
    且 subsurf 只会把面往控制笼**内**收 ⇒ 基网格的投影是评估后壳的**上界**
    ⇒ 留边读数偏保守（不会放过"真被裁掉"），尺寸读数偏大（不会放过骨缩放漏进帽子）。
    渲染仍走资产自己的修改器，本函数不碰它们。
    """
    verts = ob.data.vertices
    step = max(1, len(verts) // limit)
    mw = world_of(ob)
    return [mw @ verts[i].co for i in range(0, len(verts), step)]


def load_model(path):
    """装载外部模型，返回要烘的网格。

    只保留"有材质槽的 MESH"：外部资产常留着无材质的参考球（太空猫那份就带一个把全身
    包住的 Icosphere），它会被渲成一坨默认灰球，而灰球混在宠物身上不会让任何人警觉。
    """
    ext = os.path.splitext(path)[1].lower()
    if ext == ".blend":
        bpy.ops.wm.open_mainfile(filepath=os.path.abspath(path))
    elif ext in (".glb", ".gltf"):
        bpy.ops.wm.read_factory_settings(use_empty=True)
        bpy.ops.import_scene.gltf(filepath=os.path.abspath(path))
    else:
        sys.exit("--model 只认 .blend/.glb/.gltf：%s" % path)
    for ob in [o for o in bpy.data.objects if o.type in ("LIGHT", "CAMERA")]:
        bpy.data.objects.remove(ob, do_unlink=True)
    keep, drop = [], []
    for ob in [o for o in bpy.data.objects if o.type == "MESH"]:
        (keep if any(m is not None for m in ob.data.materials) else drop).append(ob)
    for ob in drop:
        print("BAKE 丢掉无材质网格 %s" % ob.name)
    if not keep:
        sys.exit("--model %s 里没有带材质的网格" % path)
    print("BAKE 装载 %s → 网格 %s" % (path, " ".join(ob.name for ob in keep)))
    return keep


# ── 配件（--outfit）：装第二份网格、只取白名单、刚性附着到 head 骨、逐帧投影挂点 ──
def _base_name(name):
    """去掉 Blender 的重名后缀：'Torus.002' → 'Torus'。

    append 时若场景里已有同名物体，Blender 会**静默**改名成 .001，所以"拿到的是不是我要
    的那件"不能靠全等比名字：比的是去后缀的基名，收的是数据块身份。
    """
    head, _, tail = name.rpartition(".")
    return head if head and tail.isdigit() and len(tail) == 3 else name


def load_outfit(path, names):
    """把配件网格装进**已经有宠物的**场景，只留 --outfit-objects 白名单里的 MESH。

    不能用 load_model()：它 .blend 那支是 `open_mainfile`（整场景替换），.glb 那支先
    `read_factory_settings(use_empty=True)`（把宠物清掉）—— 两条都会吃掉正在烘的主体。
    .blend 这一支走 `wm.append` 逐件搬：实测 `filepath=blend+"#Object#Torus"` 那种 URI
    写法在 5.2 报"nothing indicated"，只有 directory+filename 能用。
    白名单必须显式给：xmashat.blend 里三块背景板 Plane* 全带材质，load_model 那套
    "按有无材质筛"会把它们一起搬进画面，而没有任何门禁会报警。
    """
    ext = os.path.splitext(path)[1].lower()
    if ext not in (".blend", ".glb", ".gltf"):
        sys.exit("--outfit 只认 .blend/.glb/.gltf：%s" % path)
    abspath = os.path.abspath(path)
    wanted = set(names)
    before = {ob.as_pointer() for ob in bpy.data.objects}
    if ext == ".blend":
        directory = os.path.join(abspath, "Object") + "/"
        for name in names:
            try:
                ret = bpy.ops.wm.append(directory=directory, filename=name, link=False)
            except RuntimeError as e:
                sys.exit("--outfit %s 里 append 不出 %r：%s" % (path, name, e))
            if ret != {"FINISHED"}:
                sys.exit("--outfit %s 里 append %r 返回 %s ⇒ 用 Blender 核对物体名并补 --outfit-objects" % (path, name, ret))
    else:
        bpy.ops.import_scene.gltf(filepath=abspath)  # 不带 factory reset：宠物得留着
    fresh = [ob for ob in bpy.data.objects if ob.as_pointer() not in before]
    keep, drop = [], []
    for ob in fresh:
        (keep if ob.type == "MESH" and _base_name(ob.name) in wanted else drop).append(ob)
    for ob in drop:
        print("BAKE 配件丢掉 %s（type=%s，不在 --outfit-objects 白名单里）" % (ob.name, ob.type))
        bpy.data.objects.remove(ob, do_unlink=True)
    # 比基名的**重数**而不是集合：白名单同时写 Torus 和 Torus.001 时，append 进来的第二件会
    # 被改名成 .001，基名两者都是 Torus ⇒ 用 set 比会漏判"少装了一件"
    need = {}
    for name in names:
        base = _base_name(name)
        need[base] = need.get(base, 0) + 1
    got = {}
    for ob in keep:
        base = _base_name(ob.name)
        got[base] = got.get(base, 0) + 1
    lack = [b for b, k in sorted(need.items()) if got.get(b, 0) < k]
    if lack:
        sys.exit(
            "--outfit %s 里这些网格没装齐：%s（要的重数 / 实到的重数：%s）\n"
            "  这次实际装进来的是：%s\n"
            "  核对办法：blender -b %s --python-expr \"import bpy; print([o.name for o in bpy.data.objects if o.type=='MESH'])\""
            % (
                path,
                ",".join(lack),
                "/".join("%d/%d" % (need[b], got.get(b, 0)) for b in lack),
                " ".join(sorted(ob.name for ob in fresh)) or "（什么都没装进来）",
                path,
            )
        )
    for ob in keep:
        # 白名单外的父级刚被删掉；断开父子链并把世界矩阵钉住，免得残留在 parent_inverse 里
        if ob.parent is not None:
            mw = ob.matrix_world.copy()
            ob.parent = None
            ob.matrix_world = mw
    print("BAKE 配件装载 %s → 网格 %s" % (path, " ".join(ob.name for ob in keep)))
    return keep


SUBSURF_MAX = 6


def set_outfit_subsurf(hat, level):
    """把配件所有细分修改器钉到给定档（level<0 = 沿用配件文件自带的那档，什么都不动）。

    为什么这条要单独开关而不是写死：一份自带 3 档细分的配件能把每格成本从交付档的
    0.895 s 抬到 56.9 s（63×），而"降档之后帽子在 97px 里还圆不圆"是人眼活、不是算术能定的
    ⇒ 每个候选档都得出了图给人看。

    ⚠️ **顶点数不是成本的可靠预测量，别拿它外推 s/格。** 本轮实测四个档（同一格、其余变量全同）：
    求值顶点 10,790 / 62,498 / 249,802 / 998,834 ⇒ s/格 **11.6 / 27.4 / 30.9 / 56.9**
    ⇒ 双对数斜率只有 **≈0.35**（顶点 ×92 而成本只 ×4.9），且**档 0 就有 ≈10 s/格的地板**。
    地板的候选成因 = 这三件各带 1000 根 HAIR 粒子（`render_type=PATH`、`show_render=True`）
    ⇒ 粒子渲进像素而**不进投影包围盒**，所以它既吃成本又可能污染"剪影 vs 挂点"的比对。
    下面那行求值顶点合计仍然要打，它是**换配件文件时的成本量级线索**，不是能换算成秒的数。
    """
    if level < 0:
        print("BAKE 配件细分档：沿用配件文件自带的那档（没传 --outfit-subsurf）")
        return
    if level > SUBSURF_MAX:
        sys.exit("--outfit-subsurf %d 超出 Blender 的细分上限 %d" % (level, SUBSURF_MAX))
    none_at_all = []
    targets = []
    for ob in hat:
        subs = [m for m in ob.modifiers if m.type in ("SUBSURF", "MESH_SUBSURF")]
        if not subs:
            # 沉默降级是本项目明令禁止的形状：这个开关对这份配件是空操作必须说出来
            none_at_all.append(ob.name)
            continue
        for m in subs:
            m.levels = level
            m.render_levels = level
        targets.append(ob)
    # 写完必须让求值图真的重算一遍再读回来 —— 和 "PoseBone.matrix 写完不 update 就读 = 静默 no-op" 同族
    bpy.context.view_layer.update()
    dg = bpy.context.evaluated_depsgraph_get()
    dg.update()
    total = 0
    for ob in targets:
        ev = ob.evaluated_get(dg)
        ev_n = len(ev.data.vertices) if ev and ev.data else -1
        total += max(ev_n, 0)
        print("BAKE 配件细分：%s → 档 %d，基网格 %d 顶点 ⇒ 求值 %d 顶点"
              % (ob.name, level, len(ob.data.vertices), ev_n))
    if none_at_all:
        print("BAKE 配件细分：这些件上一个细分修改器都没有，--outfit-subsurf 对它们是空操作：%s"
              % " ".join(none_at_all))
    print("BAKE 配件求值顶点合计 %d（成本量级线索，不能按正比换算成 s/格：实测斜率 ≈0.35、且有 ≈10 s/格地板）" % total)


def mesh_box(meshes):
    """配件网格的世界包围盒 (x0,x1,y0,y1,z0,z1)（基网格，见 rigid_points）。"""
    pts = [p for ob in meshes for p in rigid_points(ob, limit=3000)]
    return (
        min(p.x for p in pts), max(p.x for p in pts),
        min(p.y for p in pts), max(p.y for p in pts),
        min(p.z for p in pts), max(p.z for p in pts),
    )


def outfit_anchor_point(hat, override):
    """挂点取配件自己哪个点（装载后、任何配件变换前的世界坐标）。返回 (点, 这个点取自哪件)。

    默认量的**不是骨头顶**，而是**帽檐圈心**：渲染器要放的是那一圈，而骨尖不等于帽檐
    落在哪儿（骨头只是"挂在哪根骨"的**附着**依据，不是**坐标**依据）。自动规则取"最低的
    那件网格的包围盒中心"—— 戴在头上的东西，最矮那件通常就是包住头的那一圈；认几何不认
    名字，换一个配件文件不用改代码。不认的时候用 --outfit-anchor 显式给（返回 None 表示
    "这件是我手给的，工具没有意见"）。
    """
    if override:
        return parse_vec(override), None
    lowest = min(hat, key=lambda ob: mesh_box([ob])[4])
    x0, x1, y0, y1, z0, z1 = mesh_box([lowest])
    point = Vector(((x0 + x1) / 2, (y0 + y1) / 2, (z0 + z1) / 2))
    print(
        "BAKE 配件挂点自动取 %s 的包围盒中心 (%+.3f,%+.3f,%+.3f)（该件最低 z=%.3f 是整组最矮）"
        % (lowest.name, point.x, point.y, point.z, z0)
    )
    return point, lowest.name


def bone_world(armature, pb):
    """pose 骨的世界矩阵（原点 = 骨根，局部 +Y 指向骨尖）。"""
    return armature.matrix_world @ pb.matrix


def bone_slot_point(armature, pb, seat):
    """沿这根骨取一点的世界坐标：seat=0 骨根（颈关节）、1 = 骨尖（颅顶）。

    留着只给 neck slot 用（领圈本来就是照着骨头定的）。**帽檐不许用它** —— 用 head_region。
    """
    return armature.matrix_world @ pb.head.lerp(pb.tail, seat)


def neck_band(ws, widen=NECK_WIDEN):
    """从**颅顶往下**的逐带宽度列表里找脖子，返回 (带的下标, 是否真找到)。

    判据 = 三段形状【头变宽】→【收窄】→【肩膀重新变宽（连续三带都宽过颈的 widen 倍）】，取中
    间那段的底。锚点是"头最宽的那一带"而不是第一带：颅顶那一带本身极窄，拿它当运行最窄会让
    任何变宽都立刻误判成找到颈 ⇒ pinch 停在颅顶、头高≈0（写第一版时就撞上过）。
    第四道判据 = **头的最宽带也得比颈粗过 widen 倍**：一根等宽柱子接一个宽底座（rabit 那根
    "木棍"就是这形状）也满足三段，但那是"柱 + 座"不是"头 + 颈"，缺这道它会返回 found=True
    而把整根柱子当头 ⇒ 配件按柱宽定标，静默错（neck_band_test.py 的第 ③ 条抓的就是这个）。
    纯函数、不碰 bpy ⇒ 能被 Blender 外的脚本 AST 抠出来直接测（`~/.local/share/opet/scratch/
    hat/neck_band_test.py`）。
    """
    i = 1
    while i < len(ws) and ws[i] >= ws[i - 1]:
        i += 1  # ① 头在变宽（这一段非降 ⇒ 最宽就是最后进的那一带）
    if i <= 1:
        return 0, False
    head = ws[i - 1]
    kmin = j = i
    while j < len(ws):
        if ws[j] < ws[kmin]:
            kmin = j  # ② 收窄
        elif ws[j] > widen * ws[kmin] and all(
            ws[m] > widen * ws[kmin] for m in range(j, min(j + 3, len(ws)))
        ):
            return kmin, head > widen * ws[kmin]  # ③ 肩膀 且 ④ 头确实比颈粗
        j += 1
    return kmin, False


def head_region(meshes, bone=None, bins=48):
    """从宠物网格量出"看得见的头"：颅顶 z、颈收缩 z、头宽、头区水平中心。

    bone = (骨长, 骨根世界 z, 骨尖世界 z)，只用来把"骨不是头"这条打印出来，不参与计算。

    为什么不用 head 骨：太空猫那份 rig 实测 head 骨在 z 0.712..0.777、骨长 0.069，而整颗头
    在 z ≈0.9..1.83、宽 1.30，且头顶顶点 100% 归 Spine2 管（绕 Head/Neck 骨转 20° 那圈点
    移动 0.0000）⇒ 拿骨位置当帽檐落点会把帽子放进肚子，拿骨长当头颅尺寸会把帽子缩到 6px。
    规则：从脚底到颅顶分 bins 带，**从颅顶往下**找"最窄带之后再持续变宽"的第一次 = 颈收缩，
    它以上的最宽带 = 头宽；中心取颈收缩以上所有点的 xy 均值。两份系数都打印，不认就
    --outfit-scale / --outfit-seat 显式覆盖。rest pose 量一次，逐帧不变（配件跟着骨走）。

    ⚠️ **取点必须是 evaluated_points，不能是 rigid_points**（这条是"帽子戴到腰上"的第二半根因；
    查之前我已把"固定分位窗"换成拓扑判据，仍然没治好）。蒙皮网格的**基顶点 × matrix_world
    是 bind space，不是渲染出来的那一帧**：Armature 修改器还要乘一次骨的逆矩阵把它拉回 rest。
    老虎那份实测 bind 框 **2.313 × 1.242 × 1.825**（X 张开 = A-pose 两条前腿摊开）而渲染/rest
    实际 **1.127 × 1.274 × 1.815** ⇒ 拿 bind 点量"头宽"量成 **2.313**（跨整副肩宽），
    定标 `OUTFIT_SIZE_RATIO × 头宽 / 帽宽` 跟着偏大、"颈以上"把肩膀一起算进头区。
    太空猫那份同样偏（bind 1.500 vs rest 1.339），只是差 12% 没差 105% 那么显眼 ⇒
    **它已交付的 outfit.json / pet.json 那两个挂点也是在 bind 几何上量的，必须重量。**
    rigid_points 那条"评估一次 93 万顶点"的代价只属于**配件**（全 mesh subsurf）；宠物网格在
    rest 上评估一次 = 老虎 4,036 / 太空猫 4,764 点、`to_mesh()` 只调一次，不是热路径。
    """
    pts = [p for ob in meshes for p in evaluated_points(ob, limit=6000)]
    z0 = min(p.z for p in pts)
    z1 = max(p.z for p in pts)
    height = z1 - z0
    if height <= 0:
        sys.exit("head_region：宠物网格的 z 跨度是 %.3f，量不出头在哪" % height)
    bands = {}
    for p in pts:
        k = min(bins - 1, int((p.z - z0) / height * bins))
        row = bands.setdefault(k, [1e9, -1e9, 1e9, -1e9])
        row[0] = min(row[0], p.x)
        row[1] = max(row[1], p.x)
        row[2] = min(row[2], p.y)
        row[3] = max(row[3], p.y)

    def width(k):
        row = bands.get(k)
        if row is None:  # 空格子绝不能当"脖子"，否则 pinch 会挑到一条没顶点的带
            return 1e9
        return max(row[1] - row[0], row[3] - row[2])

    # 颈收缩 = 从颅顶往下【头在变宽】→【收窄到颈】→【重新持续变宽 = 肩膀】那三段里的第二段底。
    # ⚠️ 原先是在 [0.45, 0.72]×身高这段**固定窗口**里找最窄带 —— 那是太空猫的比例，换载体就落在
    # 别处：老虎的脖子在 ≈0.8 身高，固定窗落在**腰**上（pinch_z 0.853），于是"pinch 以上"把整副
    # 肩甲都算成头 ⇒ 头宽量成 2.313（真头 ≈0.79），定标和落座一起偏低偏大，眼验图上"帽子戴到腰上"。
    # ⇒ **任何"总高的固定分位"都是载体相关的**，换成拓扑判据。
    # 锚点必须是"头最宽的那一带"而不是颅顶第一带：颅顶第一带本身极窄（0.55 那种），拿它当运行
    # 最窄会让**任何**变宽都立刻误判成"找到颈"，pinch 直接停在颅顶、头高≈0。
    # 量不到颈时**退回旧固定窗并打印**（宁可留一条可见的退路，不要静默给个假头）。
    order = sorted(bands, reverse=True)  # 从颅顶往下的非空带
    idx, found = neck_band([width(k) for k in order])
    pinch = order[idx]
    if not found:
        pinch = min(range(int(0.45 * bins), int(0.72 * bins) + 1), key=width)
        print(
            "BAKE 头区：**量不到颈收缩**（从颅顶往下没有'变宽→收窄→再变宽'）⇒ 退回固定分位窗 [0.45,0.72]×身高，"
            "那条是载体相关的，请核对下面那个头宽是不是真头的宽度"
        )
    pinch_z = z0 + (pinch + 1) / float(bins) * height
    above = [p for p in pts if p.z >= pinch_z]
    region = {
        "crown_z": z1,
        "pinch_z": pinch_z,
        # 只在**非空带**上取最宽：width() 对空带返回 1e9（那是"别把空带当脖子"的哨兵，不是尺寸），
        # 而 range(pinch, bins) 会走过空带 ⇒ 网格上任何一个洞都会把"头宽"量成 1e9、定标随之爆炸。
        "width": max(width(k) for k in order if k >= pinch),
        "height": z1 - pinch_z,
        "center": Vector((sum(p.x for p in above) / len(above), sum(p.y for p in above) / len(above), pinch_z)),
    }
    print(
        "BAKE 头区（网格几何，颈=%s）：颈收缩 z=%.3f 颅顶 z=%.3f 头高 %.3f 头宽 %.3f 中心 (%+.3f,%+.3f)"
        % (
            "拓扑" if found else "固定窗",
            region["pinch_z"],
            region["crown_z"],
            region["height"],
            region["width"],
            region["center"].x,
            region["center"].y,
        )
    )
    if bone is not None:
        print(
            "BAKE 头区对照 head 骨：骨长 %.3f、世界 z %.3f..%.3f ⇒ 骨既不是头的尺寸也不是头的位置"
            % bone
        )
    return region


def reset_bones(armature, base):
    for pb in armature.pose.bones:
        pb.matrix_basis = base[pb.name]
    bpy.context.view_layer.update()


def update_depsgraph():
    """把整条求值真正跑一遍 —— `view_layer.update()` 单独用**不足以**让刚加上的约束生效。

    实测两条同族事故：① 写完 pose 不更新就读回 = 静默 no-op（`_rot_about_head` 那条）；
    ② 刚 new 出来的对象挂上 CHILD_OF 后 `view_layer.update()` 一次，读到的还是**没带约束的
    旧矩阵**（探针 Empty 实测给出恒等，而同一份约束在已有网格上量到的是骨的世界矩阵）⇒
    上一版那个"探针实测 T_c"就是这么被读成单位阵、把整套附着变成摆设的。
    """
    deps = bpy.context.evaluated_depsgraph_get()
    deps.update()
    bpy.context.view_layer.update()
    deps = bpy.context.evaluated_depsgraph_get()
    return deps


def bind_child_of(armature, bone, hat, placed, tol):
    """把帽刚性绑到骨上，inverse 由**实测 + 验证**定出来，不猜 Blender 的配方。

    约束求值要么形如 `世界 = A @ inverse @ basis`，要么形如 `A' @ basis' @ inverse'`（A 是
    "约束真正拿来当父级的那一份"，它在 `use_scale_*=False` 下被剥成什么样是内部实现）。
    两次手算都把它猜错过（一次平方了缩放 ⇒ 帽小 62×，一次被读成恒等 ⇒ 整套附着是摆设），
    所以这里不推导 A，只**量**：
      1) 先上恒等 inverse，量出 `W_bad`；
      2) 需求"上了约束之后世界矩阵 == 摆放"，按两种可能形状各解一个候选，逐个**装上去验**，
         取残差为零的那一支；两支都不为零就当场拒绝。
    `W_bad` 与摆放**逐位相同** = 约束这一轮没生效 ⇒ 也当场拒绝，绝不静默退化成"绑了但没绑"。

    ⚠️ **验收容差 `tol` 必须由调用方按定标后的足迹给（相对值），不能在这里写死绝对数**：这条量的是
    浮点矩阵复合的噪声，噪声的绝对量随这一档的缩放与那份 rig 的骨矩阵一起漂。实测同一支配方
    scale 0.11 残差 **2.7e-7** 过、scale 0.08 残差 **1.52e-6** 撞在原先那句 `1e-6` 上**误红退出**
    （而它旁边的错误分支是 **2.147** ⇒ 正确支比错误支好 6 个数量级，判"哪一支对"毫无歧义）。
    反过来"约束没生效"那一档**不能用同一条带**去判：真没生效的形状是**恰好 0.0**（矩阵一个位都没动），
    而工作噪声落在 2.7e-7…1.5e-6 ⇒ 两条带之间只剩"逐位相等"这一条分得开，写成任何 `resid < tol`
    都会把"能用的那一档"读成"没生效"。
    """
    con_of = {}
    for ob in hat:
        con = ob.constraints.new("CHILD_OF")
        con.target = armature
        con.subtarget = bone
        con.use_scale_x = con.use_scale_y = con.use_scale_z = False
        con.inverse_matrix = Matrix.Identity(4)
        con_of[ob.name] = con
    update_depsgraph()
    bad_of = {}
    for ob in hat:
        bad = world_of(ob)
        resid = mat_resid(bad, placed[ob.name])
        if bad == placed[ob.name]:
            sys.exit(
                "%s 上完 CHILD_OF（恒等 inverse）后世界矩阵**逐位未变**（残差 %.8f）⇒ 约束没生效，"
                "没法实测父级矩阵。这条就是「绑了个摆设」的形状，不许放行" % (ob.name, resid)
            )
        bad_of[ob.name] = bad
    # 两种候选：inverse = basis @ W_bad⁻¹（形状 A@inv@basis）与 inverse = W_bad⁻¹ @ basis（形状 inv@A@basis）
    for name, cand in (("basis@W⁻¹", lambda b, w: b @ w.inverted()), ("W⁻¹@basis", lambda b, w: w.inverted() @ b)):
        for ob in hat:
            con_of[ob.name].inverse_matrix = cand(placed[ob.name], bad_of[ob.name])
        update_depsgraph()
        resid = max(mat_resid(world_of(ob), placed[ob.name]) for ob in hat)
        print("BAKE 配件 CHILD_OF inverse 候选 %s ⇒ 摆放残差最大 %.8f（容差 %.6f = 1e-3 × 定标后整组足迹）" % (name, resid, tol))
        if resid < tol:
            print("BAKE 配件 CHILD_OF 绑定：采用 %s 这一支（残差 %.8f，逐物体验过）" % (name, resid))
            return
    sys.exit(
        "两种 inverse 配方装上去都没把摆放还原回去 ⇒ 约束的求值形状不是这两种，"
        "挂点读数不能要（继续算下去就是拿一批假坐标写进 outfit.json）"
    )


def attach_outfit(armature, roles, hat, meshes, args, base):
    """把配件定标、刚性对齐到**看得见的头**、上 CHILD_OF 约束。返回挂点读数需要的上下文。

    附着用 CHILD_OF 而不是 `parent_type='BONE'`：后者挂的是骨**尾**且用骨局部轴，实测会把
    --outfit-rot-z 变成世界俯仰而不是偏航；而 `bpy.ops.object.parent_set(keep_transform=True)`
    在无 GUI 下返回 FINISHED 却**静默不做事**。
    `use_scale_*=False`：帽不许随胸腔呼吸涨缩 —— 这是 inherit_scale=FULL 那条坑的第三次现身。
    inverse_matrix 一律由 bind_child_of 实测反解，然后**读评估后的矩阵**验一次摆放没被约束改动。
    """
    head = roles["head"]
    reset_bones(armature, base)  # 定标与摆放必须在 rest 上做，否则跟着呼吸那一档一起偏
    pb = armature.pose.bones[head]
    bone_len = (pb.tail - pb.head).length
    region = head_region(meshes, (bone_len, (armature.matrix_world @ pb.head).z, (armature.matrix_world @ pb.tail).z))
    anchor0, anchor_from = outfit_anchor_point(hat, args.outfit_anchor)
    x0, x1, y0, y1, z0, z1 = mesh_box(hat)
    width = max(x1 - x0, y1 - y0)  # **整组**水平足迹（含歪出去的绒球），不是帽圈直径
    # 帽圈 = 整组最矮那件的直径。定标公式量的不是它，但"挂件看起来多大"读的是它，所以两个都要有出处。
    # 逐件 mesh_box([ob]) 与整组 mesh_box(hat) 取的是同一批点（rigid_points 本来就逐件抽样）⇒ 可直接合。
    object_widths = {}
    ring_name, ring_width, ring_z = "", 0.0, None
    for ob in hat:
        ox0, ox1, oy0, oy1, _, oz0 = mesh_box([ob])
        object_widths[ob.name] = round(max(ox1 - ox0, oy1 - oy0), 5)
        if ring_z is None or oz0 < ring_z:
            ring_name, ring_width, ring_z = ob.name, object_widths[ob.name], oz0
    scale = args.outfit_scale if args.outfit_scale > 0 else round(OUTFIT_SIZE_RATIO * region["width"] / width, 4)
    seat = Vector((region["center"].x, region["center"].y, region["pinch_z"] + args.outfit_seat * region["height"]))
    xform = (
        Matrix.Translation(seat)
        @ Matrix.Rotation(math.radians(args.outfit_rot_z), 4, "Z")
        # 倾角绕帽子**自身**的 Y 轴（链里写在 yaw 的右边 ⇒ 对点而言"先倾后转"）：
        # 放 yaw 左边就变成绕世界前后轴倒，那时 rot_z≠0 的帽子朝一个固定的世界方向倒，
        # 换一个 rot_z / 换一份配件文件就是另一种形状，读数不再有出处。
        @ Matrix.Rotation(math.radians(args.outfit_tilt), 4, "Y")
        @ Matrix.Diagonal((scale, scale, scale, 1.0))
        @ Matrix.Translation(-anchor0)
    )
    for ob in hat:
        ob.matrix_world = xform @ ob.matrix_world
    update_depsgraph()
    placed = {ob.name: world_of(ob).copy() for ob in hat}
    # 一条容差，两处用（绑定验收 + 附着漂移）：绝对量随缩放漂 ⇒ 一律按**定标后的整组足迹**归一。
    tol = 1e-3 * max(width * scale, 1e-9)
    bind_child_of(armature, head, hat, placed, tol)

    ctx = {
        "bone": head,
        "ref": hat[0],
        # 变换后挂点正好落在 seat 上 ⇒ 存进 ref 的局部空间，之后每帧读它的世界位置
        "anchor_local": placed[hat[0].name].inverted() @ seat,
        "scale": scale,
        "rot_z": args.outfit_rot_z,
        "tilt": args.outfit_tilt,
        "seat": args.outfit_seat,
        "anchor_world0": seat.copy(),
        "bone_len": bone_len,
        "head_region": {k: (list(v) if isinstance(v, Vector) else v) for k, v in region.items()},
        "group_width": width,
        "object_widths": object_widths,
        "anchor_object": anchor_from,
        "ring_object": ring_name,
        "ring_width": round(ring_width, 5),
        # 定标后的**整组**世界足迹 = 投影尺寸那条**量级**门禁的期望值（关系检查看不见量级错，见 check_attach）
        "size_ref_world": width * scale,
        "ring_world": ring_width * scale,
        "objects": [ob.name for ob in hat],
        # 读数要能被别人复算 ⇒ 把"这次烘的是哪份模型、转了多少度、配件是哪份文件"一起带上
        "model": args.model,
        "hat_model": args.outfit,
        "face_rot_z": args.face_rot_z,
    }
    # 约束加完先验一次"没把摆放搞飞"：inverse_matrix 算错的话这里就会炸，而不是画面上偏一截
    # ⚠️ 容差**不能写死成绝对世界单位** —— 这条量的是浮点矩阵复合的噪声，而噪声的绝对量正比于
    # 被复合的距离（= 定标后的整组足迹）。实测同一支配方：太空猫 scale 0.25 残差 2.4e-7 过、
    # 老虎自动档（足迹更大）9.1e-7 **撞在 1e-6 上翻红** ⇒ 写死绝对值 = 一条换个载体就随机红的门禁。
    # "配方真的错了"的形状是漂一截（足迹量级 ≈1.3 世界单位），比噪声高 **6 个数量级** ⇒
    # 1e-3 × 足迹 仍在真错下面三个数量级，而浮点噪声再也撞不到它。两个数每轮都打，不做静默边界。
    drift = (outfit_point(ctx) - seat)
    drift_tol = tol  # 与上面 bind_child_of 验收同一条带、同一个出处（别在这里再写一遍算式）
    print("BAKE 附着漂移：残差 %.9f（容差 %.9f = 1e-3 × 定标后整组足迹 %.3f）⇒ %s"
          % (drift.length, drift_tol, ctx["size_ref_world"], "过" if drift.length <= drift_tol else "**不过**"))
    if drift.length > drift_tol:
        sys.exit(
            "上完 CHILD_OF 后挂点漂了 %.6f 世界单位（容差 %.6f）⇒ inverse_matrix 那条配方不对（读的是评估后的矩阵，和渲染器同一份）"
            % (drift.length, drift_tol)
        )
    print(
        "BAKE 配件附着：bone=%s(len %.3f，但**定标/落座用的是网格头区**) 头宽 %.3f 颈收缩 z=%.3f 头高 %.3f ⇒ "
        "挂到 %.0f%% 段 → 世界 (%+.3f,%+.3f,%+.3f)，缩放 %.4f（整组足迹 %.3f → %.3f），绕 Z 转 %g°、绕自身 Y 倾 %g°\n"
        "  定标参考是**整组水平足迹**（%s），不是帽圈：帽圈 %s 实测 %.3f → 定标后 %.3f = **%.2f × 头宽**"
        "（公式那个 %g 只管整组足迹 ⇒ 别把它读成帽檐倍数）\n"
        "  整组各件宽度：%s"
        % (
            head,
            bone_len,
            region["width"],
            region["pinch_z"],
            region["height"],
            100 * args.outfit_seat,
            seat.x,
            seat.y,
            seat.z,
            scale,
            width,
            width * scale,
            args.outfit_rot_z,
            args.outfit_tilt,
            max(object_widths, key=lambda n: object_widths[n]),
            ring_name,
            ring_width,
            ring_width * scale,
            (ring_width * scale) / region["width"],
            OUTFIT_SIZE_RATIO,
            " ".join("%s=%.3f" % (n, w) for n, w in sorted(object_widths.items())),
        )
    )
    return ctx


def outfit_point(ctx):
    """这一帧挂点的世界坐标 —— 读的是被约束算完的物体矩阵，和渲染器画的是同一份。"""
    return world_of(ctx["ref"]) @ ctx["anchor_local"]


def check_attach(armature, roles, ctx, base, cam, cell, spec, pose, meshes):
    """本轮最重要的一条自检："我 parent 了"不等于"它跟着动"，而"跟着动"也不等于"大小对"。

    ① 把头骨在**世界**里平移 +0.05，帽的挂点必须跟着平移同样的量；
    ② idle 全帧内帽的投影尺寸必须恒定 —— 那是 use_scale_*=False 真的生效的证据；
    ③ **量级**门禁：那个恒定尺寸必须落在"帽檐 = 1.25 × 头宽"推算出来的像素附近。

    ③ 是这一轮新加的，因为 ①② 都被实测证伪过其充分性：inverse_matrix 把缩放平方成
    0.00026（要 0.0162，62× 太小）时，① 的位移仍然精确等于 0.050、② 的峰峰仍然 0.000
    —— **关系检查对"标定错了"天然盲**，只有拿一个绝对期望值去比才抓得住。
    """
    pb = armature.pose.bones[ctx["bone"]]
    before = outfit_point(ctx)
    # 顺带记一份"看得见的头"在骨动之前的高度，用于下面那条 rig 事实（头顶顶点归谁管）
    crown_before = crown_top(meshes, ctx["head_region"]["pinch_z"])
    shift = armature.matrix_world.inverted() @ Matrix.Translation((0.0, 0.0, 0.05)) @ armature.matrix_world
    pb.matrix = shift @ pb.matrix
    update_depsgraph()
    after = outfit_point(ctx)
    crown_after = crown_top(meshes, ctx["head_region"]["pinch_z"])
    reset_bones(armature, base)
    delta = after - before
    if delta.length < 0.04 or abs(delta.z - 0.05) > 1e-3:
        sys.exit(
            "配件没跟着 head 骨走：期望位移 (0,0,0.050)，实测 (%+.4f,%+.4f,%+.4f)（|d|=%.4f）\n"
            "  ⇒ CHILD_OF 的 target/subtarget/inverse_matrix 有一条是摆设，挂点读数全部不可信"
            % (*delta, delta.length)
        )
    print(
        "BAKE 附着自检：头骨平移 +0.050 ⇒ 配件挂点位移 (%+.4f,%+.4f,%+.4f) ⇒ 真的接上了；"
        "而颅顶网格只动 %.4f ⇒ %s"
        % (
            *delta,
            abs(crown_after - crown_before),
            "骨驱动的是可见的头"
            if abs(crown_after - crown_before) > 0.04
            else "这根骨**不驱动可见的头**（蒙皮权重在别处），所以它的位置/长度都不能当定标参照 ⇒ 参照必须是网格几何",
        )
    )

    # ② 尺寸恒定：正交相机下刚体的投影尺寸只随缩放变，所以这一条量的就是"有没有漏进骨缩放"
    frames = len(spec["states"]["idle"]["rows"])
    loop = spec["states"]["idle"].get("loop", True)
    place_camera(cam, 0)
    sizes = []
    hat = [bpy.data.objects[n] for n in ctx["objects"]]
    for frame in range(frames):
        pose("idle", frame, frames, loop)
        update_depsgraph()
        co = [world_to_camera_view(bpy.context.scene, cam, p) for ob in hat for p in rigid_points(ob, limit=800)]
        sizes.append(max((max(c.x for c in co) - min(c.x for c in co)) * cell, (max(c.y for c in co) - min(c.y for c in co)) * cell))
    spread = max(sizes) - min(sizes)
    limit = 0.5 * cell / 96.0
    print("BAKE 配件投影尺寸 idle 全帧 %s ⇒ 峰峰 %.3fpx（门禁 ≤%.1fpx，这条量的是骨缩放有没有漏进帽子）" % (" ".join("%.2f" % s for s in sizes), spread, limit))
    if spread > limit:
        sys.exit(
            "配件投影尺寸随 idle 帧变化 %.3fpx —— use_scale_*=False 没生效（胸腔围长 ±3.75%% 传染给帽子）"
            % spread
        )

    # ③ 量级：正交相机下世界尺寸 → px 就是 (extent / ortho_scale) × cell，期望值由定标公式直接给出
    # ⚠️ 这条比的是**整组足迹**（sizes 取的是全组投影 x/y 跨度的较大者，期望用的也是整组世界宽度）
    # ⇒ 它只证明"定标公式接上了"，不证明"帽檐有 1.25 倍头宽"；后者由下面那行打印交人眼判。
    expect = ctx["size_ref_world"] / cam.data.ortho_scale * cell
    got = sum(sizes) / len(sizes)
    if not (0.6 * expect <= got <= 1.6 * expect):
        sys.exit(
            "配件投影尺寸 %.1fpx 落在期望 %.1fpx 的 ±60%% 带外（整组世界足迹 %.4f、ortho_scale %.3f、cell %d）\n"
            "  ⇒ 定标或 CHILD_OF 的缩放档错了；①② 那两条关系检查看不见这种错（62× 太小也照样绿）"
            % (got, expect, ctx["size_ref_world"], cam.data.ortho_scale, cell)
        )
    print(
        "BAKE 配件尺寸量级：实测 %.1fpx vs 期望 %.1fpx（= %.2f 世界单位 / ortho %.3f × %d）⇒ 在带内；"
        "其中帽圈 %.1fpx = %.2f × 头宽 %.1fpx"
        % (
            got,
            expect,
            ctx["size_ref_world"],
            cam.data.ortho_scale,
            cell,
            ctx["ring_world"] / cam.data.ortho_scale * cell,
            ctx["ring_world"] / ctx["head_region"]["width"],
            ctx["head_region"]["width"] / cam.data.ortho_scale * cell,
        )
    )
    reset_bones(armature, base)


def crown_top(meshes, pinch_z):
    """颈收缩以上那圈网格顶点的世界最高 z = 看得见的颅顶（bone 平移后重算，用来验骨是否驱动它）。"""
    top = -1e9
    for ob in meshes:
        for p in evaluated_points(ob, limit=1500):
            if p.z >= pinch_z:
                top = max(top, p.z)
    return top


def check_outfit_framing(cam, cell, columns, spec, states, pose, ctx):
    """配件自己的构图读数：**只打印不判红**。

    不许把帽子掺进 check_framing 的 samples —— 那两行"部件投影最小尺寸 / 格边留边最紧的
    零件"的读数就是四只已签收产物的回归判据（老虎 99.5/11.9、太空猫 118.0/15.0），掺进
    帽子等于当场把它们判废。
    """
    hat = [bpy.data.objects[n] for n in ctx["objects"]]
    tightest = 9e9
    biggest = 0.0
    for state in states:
        frames = len(spec["states"][state]["rows"])
        loop = spec["states"][state].get("loop", True)
        for index in columns:
            place_camera(cam, index)
            for frame in range(frames):
                pose(state, frame, frames, loop)
                update_depsgraph()
                co = [world_to_camera_view(bpy.context.scene, cam, p) for ob in hat for p in rigid_points(ob, limit=500)]
                xs = [c.x for c in co]
                ys = [c.y for c in co]
                biggest = max(biggest, (max(xs) - min(xs)) * cell, (max(ys) - min(ys)) * cell)
                over = max(-min(xs), max(xs) - 1.0, -min(ys), max(ys) - 1.0)
                clearance = (-over * cell) if over > 0.005 else min(min(xs), 1.0 - max(xs), min(ys), 1.0 - max(ys)) * cell
                tightest = min(tightest, clearance)
    print(
        "BAKE 配件构图（不判红）：投影最大尺寸 %.1fpx（格子 %d），格边留边最紧 %.1fpx%s"
        % (biggest, cell, tightest, " ⇒ 已被裁掉，眼验前先看这一行" if tightest < 0 else "")
    )
    return biggest, tightest


def alpha_bbox(path):
    """读回渲染 PNG 里不透明像素的包围盒（px）。Blender 的像素原点在**左下**，
    而 `world_to_camera_view` 的 y 也是从下往上 ⇒ 两边同向，不用翻转。"""
    img = bpy.data.images.load(path, check_existing=False)
    try:
        w, h = img.size[0], img.size[1]
        px = img.pixels[:]
    finally:
        bpy.data.images.remove(img)
    x0, x1, y0, y1 = w, -1, h, -1
    for i in range(w * h):
        if px[i * 4 + 3] > 0.5:
            x = i % w
            y = i // w
            if x < x0:
                x0 = x
            if x > x1:
                x1 = x
            if y < y0:
                y0 = y
            if y > y1:
                y1 = y
    if x1 < 0:
        sys.exit("BAKE 配件像素自检：%s 全透明 ⇒ 这一格什么都没画出来" % path)
    return x0, x1, y0, y1


def projected_box(objects, cam, cell, limit, sampler=rigid_points):
    """一组物体在當前相机下的投影包围盒（px），取的是**采样点**的极值 ⇒ 偏保守（比剪影大）。

    `sampler` 必须按物体分：配件是刚体（用 rigid_points，绝不重建修改器栈），而宠物的剪影
    由骨架蒙皮决定（必须用 evaluated_points，否则拿的是 rest pose 的框，摆完姿势它不变）。
    """
    xs, ys = [], []
    for ob in objects:
        for p in sampler(ob, limit=limit):
            co = world_to_camera_view(bpy.context.scene, cam, p)
            xs.append(co.x * cell)
            ys.append(co.y * cell)
    return min(xs), max(xs), min(ys), max(ys)


def check_hat_in_pixels(path, cam, cell, ctx, meshes):
    """**渲染产物里真的有帽子吗。** 这条是本轮那起事故的门禁化：附着自检①②、构图读数、
    feet_px 忠实性全部绿，而 32 格像素里一个帽子像素都没有。所有已有的探针量的是**几何关系**，
    没有一个去读**最终那张贴图** ⇒ 唯一能挡住"探针全绿而画面空的"的办法就是把画出来的
    alpha 包围盒和投影包围盒对一遍。"""
    hat = [bpy.data.objects[n] for n in ctx["objects"]]
    update_depsgraph()  # 投影盒必须和刚渲染那一格用的是同一份评估矩阵，否则比的是两个时刻
    hb = projected_box(hat, cam, cell, 600)
    pb = projected_box(meshes, cam, cell, 8000, sampler=evaluated_points)
    tol = 6.0 * cell / 96.0  # 采样点极值 vs 真实剪影 + 抗锯齿 + 贴地补偿，留三档宽
    need = []
    if hb[0] < pb[0] - tol:
        need.append(("左", hb[0], "x0"))
    if hb[1] > pb[1] + tol:
        need.append(("右", hb[1], "x1"))
    if hb[2] < pb[2] - tol:
        need.append(("下", hb[2], "y0"))
    if hb[3] > pb[3] + tol:
        need.append(("上", hb[3], "y1"))
    if not need:
        print("BAKE 配件像素自检：帽投影没有超出身体投影（%.1f–%.1f / %.1f–%.1f vs 身体 %.1f–%.1f / %.1f–%.1f）⇒ 这条测不出东西，帽子太小或位置太居中" % (*hb, *pb))
        return False
    x0, x1, y0, y1 = alpha_bbox(path)
    got = {"x0": x0, "x1": x1, "y0": y0, "y1": y1}
    for side, want, key in need:
        # 采样点极值一定**不超出**真实剪影极值，而 alpha 只会因抗锯齿多出零点几 px ⇒
        # 要求 alpha "越过"投影极值 tol 是永远满足不了的假红。正解 = alpha 只要**够近**：
        # 帽在左侧撑出身体 ⇒ 不透明区必须推进到距该极值 tol 以内，差得多就是没画。
        edge = want - tol if key in ("x1", "y1") else want + tol
        reached = got[key] >= edge if key in ("x1", "y1") else got[key] <= edge
        if not reached:
            sys.exit(
                "BAKE 配件像素自检：帽在%s侧投影到 %.1fpx，而渲染 alpha 只到 %.1fpx（要求至少到 %.1fpx，容忍 %.1fpx）"
                "（帽投影盒 %.1f–%.1f / %.1f–%.1f，身体投影盒 %.1f–%.1f / %.1f–%.1f，alpha 盒 %d–%d / %d–%d）\n"
                "  ⇒ 几何自检全绿而像素里没有帽子 —— 约束被静默忽略、或帽被剪掉、或根本没参与渲染"
                % (side, want, got[key], edge, tol, *hb, *pb, x0, x1, y0, y1)
            )
    print(
        "BAKE 配件像素自检：帽撑大了剪影 %s ⇒ alpha 盒 %d–%d / %d–%d 覆盖到投影极值（%s，容忍 %.1fpx）"
        % (" ".join(s for s, _, _ in need), x0, x1, y0, y1, " ".join("%.1f" % v for v in hb), tol)
    )
    return True


def hide_carrier(meshes):
    """`--outfit-only`：把载体从渲染里摘掉，但**不动它的几何与姿态**。

    为什么用 `hide_render` 而不是删掉物体：客户端那条路线的地基是"同一台相机、同一批姿态"，
    而姿态是骨架驱动载体的 ⇒ 删了载体，配件的 `CHILD_OF` 照样跟着 head 骨走（那部分不受影响），
    但 `check_attach` / `check_framing` / `measure_outfit` 这三条读的都是**载体的几何**
    （头区、颅顶、脚锚点），删掉就全都读不到了。`hide_render` 只让成片里没有它。
    """
    for ob in meshes:
        ob.hide_render = True
    return [ob.name for ob in meshes]


def check_hat_only_in_pixels(path, cam, cell, ctx):
    """`--outfit-only` 那一档的像素自检，判的是**另一件事**，所以不能沿用上面那条。

    上面那条比的是"帽有没有把身体的剪影撑大"（载体在画面里才有意义）。这里载体被 `hide_render`
    藏掉了，剪影**只有帽子** ⇒ 那条会走进"没撑大 ⇒ 这条测不出东西"的打印分支而返回 False，
    于是"这一格到底有没有渲出东西"重新变成没人守的东西（这正是 10-30 那起"探针全绿而画面空"
    的事故形状）。这一档要守的两条是：
    1. **非空** —— 不透明像素的包围盒存在（否则整批配件图集是空的，客户端叠上去什么也看不见）；
    2. **不外溢** —— alpha 盒必须落在帽自己的投影盒 + tol 之内。**外溢的唯一来源就是载体没藏干净**
       （身体比帽子大得多，露一点就会把包围盒推到帽盒外面 ⇒ 方向上是安全的判据）。
    反过来"alpha 比投影盒小"是常态（`rigid_points` 的 limit=600 采样取极值必然**不超出**真实剪影，
    见上面那条里同一段注释），所以不许拿它判红。
    """
    hat = [bpy.data.objects[n] for n in ctx["objects"]]
    update_depsgraph()
    hb = projected_box(hat, cam, cell, 600)
    x0, x1, y0, y1 = alpha_bbox(path)
    tol = 6.0 * cell / 96.0
    if x1 < x0 or y1 < y0:
        sys.exit(
            "BAKE 配件图集像素自检：%s 一个不透明像素都没有，而帽的投影盒是 "
            "%.1f–%.1f / %.1f–%.1f ⇒ 载体藏了而配件也没渲出来（约束被静默忽略 / 配件被剪到格外 / "
            "hide_render 连带把配件也关了）" % (path, *hb)
        )
    out = []
    if x0 < hb[0] - tol:
        out.append(("左", x0, hb[0] - tol))
    if x1 > hb[1] + tol:
        out.append(("右", x1, hb[1] + tol))
    if y0 < hb[2] - tol:
        out.append(("下", y0, hb[2] - tol))
    if y1 > hb[3] + tol:
        out.append(("上", y1, hb[3] + tol))
    if out:
        sys.exit(
            "BAKE 配件图集像素自检：alpha 盒 %d–%d / %d–%d 越出帽的投影盒 %.1f–%.1f / %.1f–%.1f（%s 侧 %.1f > %.1f，"
            "容忍 %.1fpx）⇒ 这一格里画了帽之外的东西 —— `--outfit-only` 下唯一的来源就是**载体没藏干净**，"
            "那张图集叠在本体上会把身体重画一遍"
            % (x0, x1, y0, y1, *hb, " ".join(s for s, _, _ in out), *(out[0][1], out[0][2]), tol)
        )
    print(
        "BAKE 配件图集像素自检：这一格只有帽 ⇒ alpha 盒 %d–%d / %d–%d 落在帽投影盒（%s）+%.1fpx 内，载体未参与画面"
        % (x0, x1, y0, y1, " ".join("%.1f" % v for v in hb), tol)
    )
    return True


# 测量轮里"命令行没给 ⇒ 用产物记录的"那批参数。键名 = argparse 的 dest，值 = parse_args 的默认。
RESTORE_DEFAULTS = {
    "model": "",
    "cell": 0,
    "facing": "0,1,2,3,4,5,6,7",
    "face_rot_z": 0.0,
    "ortho_scale": 0.0,
    "target_z": -1.0,
    "bones": "",
    "action_map": "",
    "bone_gain": 1.0,
    "state": "idle",
}

# 配件的那几个旋钮同样直接决定挂点读数（倾 25° 与不倾量出来是两个点），而 RESTORE_DEFAULTS
# 一个都不覆盖 ⇒ 测量轮漏给 --outfit-tilt 就会拿"不倾"的几何去量"倾了 25°"的那批像素。
# ⚠️ 这一列默认值必须与 parse_args 里对应 --outfit-* 的 default 逐字相同（RESTORE_DEFAULTS 同理），
# 因为"命令行没给"唯一的判据就是"当前值等于默认值"。
OUTFIT_RESTORE_DEFAULTS = {
    "outfit_scale": 0.0,
    "outfit_rot_z": 180.0,
    "outfit_tilt": 0.0,
    "outfit_seat": OUTFIT_SEAT,
    "outfit_subsurf": -1,
}


def restore_from_meta(args):
    """--outfit-measure-only：按已烘产物记的参数还原场景，一个像素都不重渲。

    拿不到 meta 就没法证明"我复现的就是产出那些像素的那一批参数"，所以它是硬性输入。
    两条已知缺口：meta 只记 `--bones` 的**覆盖值**（不是解出来的完整角色表 ⇒ outfit.json
    要落全表），而且**没记 target_z**（只能按模型高自动重算，靠下面那条 feet_px 逐位相等挡住）。
    第三条同类缺口（配件旋钮）已在本轮补上：`outfit_scale/rot_z/tilt/seat/subsurf` 现在写进 meta
    并被这里还原 ⇒ 但**补键之前烘的那批仍然没有这些键**，所以那条只能警告、不能假装还原成功。
    """
    path = os.path.join(args.out, "meta.json")
    try:
        with open(path, encoding="utf-8") as fh:
            meta = json.load(fh)
    except OSError:
        sys.exit("--outfit-measure-only 要读 %s：那份产物是怎么烘出来的全在里面" % path)
    from_cli, from_meta = [], []
    for key, default in RESTORE_DEFAULTS.items():
        if getattr(args, key) != default:
            from_cli.append(key)
            continue
        if key == "state":
            if meta.get("states"):
                args.state = ",".join(meta["states"])
                from_meta.append("state=%s" % args.state)
            continue
        if key == "facing":
            if meta.get("columns"):
                args.facing = ",".join(str(c) for c in meta["columns"])
                from_meta.append("facing=%s" % args.facing)
            continue
        if key == "bones":
            if meta.get("bones"):
                args.bones = ",".join("%s=%s" % kv for kv in sorted(meta["bones"].items()))
                from_meta.append("bones=%s" % args.bones)
            continue
        if key in meta:
            value = meta[key]
            if key == "cell" and isinstance(value, dict):
                # meta 记的是 {"w":..,"h":..}，而 --cell 要的是一个整数（方格已由上面的断言保证）
                value = value["w"]
            setattr(args, key, value)
            from_meta.append("%s=%s" % (key, value))
    # plant_feet 是布尔开关，argparse 分不出"没给"和"给了 --no-plant-feet" ⇒ 测量轮一律认记录值
    if "plant_feet" in meta:
        args.plant_feet = bool(meta["plant_feet"])
        from_meta.append("plant_feet=%s（取自记录，布尔开关分不出命令行是否显式给过）" % args.plant_feet)
    if args.outfit:
        # 配件旋钮：命令行没给（== 默认值）⇒ 取记录；给了而记录是另一个数 ⇒ 拒绝而不是覆盖。
        # 后者是"我明明要倾 25° 你偷偷改成 0"和"我在改一批不是这个参数的像素"两种错的共同形状，
        # 只有"以记录为准并打印"会被读成前者，所以选择报错。
        clash, unrecorded = [], []
        for key, default in OUTFIT_RESTORE_DEFAULTS.items():
            if key not in meta:
                unrecorded.append(key)
                continue
            if getattr(args, key) != default:
                if getattr(args, key) != meta[key]:
                    clash.append("%s：命令行 %g vs 记录 %g" % (key, getattr(args, key), meta[key]))
                continue
            setattr(args, key, meta[key])
            from_meta.append("%s=%g" % (key, meta[key]))
        if clash:
            sys.exit(
                "测量轮的配件参数与产出那批像素的记录不符：\n  %s\n"
                "  要么去掉这些命令行参数（认记录），要么用它们重烘一批像素再来量。"
                % "\n  ".join(clash)
            )
        if unrecorded:
            blind = [k for k in unrecorded if getattr(args, k) == OUTFIT_RESTORE_DEFAULTS[k]]
            print(
                "BAKE 测量轮警告：%s 没记 %s（这批烘的时候配件参数还没进 meta）⇒ 只能由命令行给。"
                "%s"
                % (
                    os.path.basename(path),
                    "、".join(unrecorded),
                    ("现在这些还停在默认值，量出来的挂点可能不是那批像素的几何：" + "、".join(blind))
                    if blind
                    else "命令行全都给了 ⇒ 无碍。",
                )
            )
    print(
        "BAKE 测量轮参数还原：\n  meta: %s\n  命令行: %s\n  %s 没记 target_z ⇒ 自动取 %.3f"
        % (
            "、".join(from_meta) or "（没有需要还原的参数）",
            "、".join(from_cli) or "（全部用默认值）",
            os.path.basename(path),
            TARGET_Z,
        )
    )
    if not args.model:
        sys.exit(
            "测量轮没拿到 --model（命令行和 %s 里都没有）⇒ 没有模型就没有骨架，"
            "挂点无从附着。带着产出那份产物时用的同一个模型文件重跑。" % path
        )
    return meta


def measure_outfit(args, cam, cell, spec, states, columns, pose, armature, roles, ctx, base, recorded):
    """逐状态 × 逐帧 × 逐列把挂点投影成格子像素，写 <out>/outfit.json。

    写 **outfit.json** 而不是改 meta.json：main() 末尾那句是无条件覆盖 meta，测量轮要是在那里
    写回去，就把交付产物的出处改成了 `baked:0 skipped:320` —— 一条自我销毁的 provenance。
    """
    place_camera(cam, columns[0])
    got = list(feet_anchor_px(cam, cell))
    want = list(recorded.get("feet_px") or [])
    if got != want:
        sys.exit(
            "忠实性断言失败：还原出来的相机把脚锚投到 %s，而记录值是 %s。\n"
            "  ⇒ 我复现的场景和产出这批像素的那一批不是同一个东西（model / face_rot_z / "
            "ortho_scale / target_z / cell 里至少有一条漂了），此时**任何挂点读数都是假的**。\n"
            "  本次还原用的参数：ortho_scale=%.3f target_z=%.3f cell=%d face_rot_z=%g 模型=%s"
            % (got, want, ORTHO_SCALE, TARGET_Z, cell, ctx["face_rot_z"], ctx["model"])
        )
    print("BAKE 忠实性自检通过：rest pose 下 anchor.feet 逐位 = 记录值 %s ⇒ 相机就是产那批像素的那台" % got)

    def px(point):
        nx, ny = project(cam, point)
        return [round(nx * cell, 3), round((1.0 - ny) * cell, 3)]

    neck = roles.get("neck")
    data = {
        "comment": "挂点坐标的唯一事实源：由 tools/bake.py --outfit-measure-only 投影量出，不许手改。",
        "attach_bone": {"head": roles["head"], "neck": neck},
        "objects": ctx["objects"],
        "scale": ctx["scale"],
        "rot_z": ctx["rot_z"],
        "tilt": ctx["tilt"],
        "seat_fraction": ctx["seat"],
        # 帽檐落座的世界点：只有 fraction 的话，别人拿到这份 json 还要先复算 head_region 才知道帽在哪
        "seat_world": [round(v, 5) for v in ctx["anchor_world0"]],
        "size_ratio": OUTFIT_SIZE_RATIO,
        "bone_len": round(ctx["bone_len"], 5),
        # 定标的参照物是网格量出来的头区，不是骨 ⇒ 这几条数是"帽子为什么这么大、落在哪"的唯一出处
        "head_region": {k: (round(v, 5) if isinstance(v, float) else v) for k, v in ctx["head_region"].items()},
        "group_width": round(ctx["group_width"], 5),
        "object_widths": ctx["object_widths"],
        "anchor_object": ctx["anchor_object"],
        # 定标公式用的是 group_width；帽圈（整组最矮那件 = 渲染器要放的那一圈）另有自己的直径，
        # ring_world / 头宽 才是"帽檐看起来是头的几倍"那个数。两个都存，别拿前者冒充后者。
        "ring_object": ctx["ring_object"],
        "ring_width": round(ctx["ring_width"], 5),
        "size_ref_world": round(ctx["size_ref_world"], 5),
        "ring_world": round(ctx["ring_world"], 5),
        "ring_to_head_width": round(ctx["ring_world"] / ctx["head_region"]["width"], 4),
        "anchor_local": [round(v, 5) for v in ctx["anchor_local"]],
        # meta 只记 --bones 覆盖值 ⇒ 完整角色表落在这里，"挂在哪根骨"这件事才有出处
        "bones": dict(sorted(roles.items())),
        "cell": cell,
        "ortho_scale": ORTHO_SCALE,
        "target_z": TARGET_Z,
        "face_rot_z": ctx["face_rot_z"],
        "model": ctx["model"],
        "hat_model": ctx["hat_model"],
        "fidelity": {"recorded_feet_px": want, "measured_feet_px": got},
        "columns": list(columns),
        "states": {},
        "anchor": {"face": None, "back": None, "hand": None},
    }
    for state in states:
        track = spec["states"][state]
        frames = len(track["rows"])
        loop = track.get("loop", True)
        head_rows, neck_rows = [], []
        for frame in range(frames):
            pose(state, frame, frames, loop)
            update_depsgraph()
            col_head, col_neck = [], []
            for index in columns:
                place_camera(cam, index)
                col_head.append(px(outfit_point(ctx)))
                col_neck.append(
                    px(bone_slot_point(armature, armature.pose.bones[neck], NECK_SLOT_SEAT))
                    if neck
                    else None
                )
            head_rows.append(col_head)
            neck_rows.append(col_neck)
        # 逐列的抖动量：把帽点在这一行所有帧里走过的包围盒对角线打出来（--plant-feet 会把它抬高）
        spread = []
        for i in range(len(columns)):
            xs = [r[i][0] for r in head_rows]
            ys = [r[i][1] for r in head_rows]
            spread.append(round(math.hypot(max(xs) - min(xs), max(ys) - min(ys)), 2))
        data["states"][state] = {
            "rows": track["rows"],
            "loop": loop,
            "head": head_rows,
            "neck": neck_rows,
            "spread_px": spread,
        }
    anchor_state = "idle" if "idle" in data["states"] else states[0]
    data["anchor_from"] = [anchor_state, 0, columns[0]]
    data["anchor"]["head"] = data["states"][anchor_state]["head"][0][0]
    data["anchor"]["neck"] = data["states"][anchor_state]["neck"][0][0]
    reset_bones(armature, base)

    out_path = os.path.join(args.out, "outfit.json")
    os.makedirs(args.out, exist_ok=True)
    with open(out_path, "w", encoding="utf-8") as fh:
        json.dump(data, fh, ensure_ascii=False, indent=1, sort_keys=True)
    total = sum(len(v["head"]) * len(columns) for v in data["states"].values())
    print(
        "BAKE 挂点测量：head=[%s, %s] neck=[%s, %s]（%s 第 0 帧第 %d 列）⇒ 写了 %s，共 %d 个读数 × 2 slot"
        % (*data["anchor"]["head"], *data["anchor"]["neck"], anchor_state, columns[0], out_path, total)
    )
    print(
        "BAKE 仍无事实源的 slot：face / back / hand 都是 null —— 这份 rig 没有手腕骨角色"
        "（BONE_ROLES 只有 arm/forearm），也没有脸部和背部的几何来源，不硬写。"
    )
    return data



def _move_roots(dx=0.0, dy=0.0, dz=0.0, rot_z_deg=0.0):
    """只动世界顶层的网格与骨架：挂在骨架下的网格会自动跟着，不该被搬第二次。"""
    m = Matrix.Translation((dx, dy, dz))
    if rot_z_deg:
        m = m @ Matrix.Rotation(math.radians(rot_z_deg), 4, "Z")
    for ob in [o for o in bpy.data.objects if o.parent is None and o.type in ("MESH", "ARMATURE")]:
        ob.matrix_world = m @ ob.matrix_world
    bpy.context.view_layer.update()


def normalize_model(meshes, rot_deg):
    """脸转到 +X、脚底贴 z=0、xy 按包围盒居中。返回 (高度, 顶点样本, 脚点)。"""
    _move_roots(rot_z_deg=rot_deg)
    pts = [p for ob in meshes for p in evaluated_points(ob)]

    def box(points):
        return (
            min(p.x for p in points), max(p.x for p in points),
            min(p.y for p in points), max(p.y for p in points),
            min(p.z for p in points), max(p.z for p in points),
        )

    x0, x1, y0, y1, z0, z1 = box(pts)
    _move_roots(dx=-0.5 * (x0 + x1), dy=-0.5 * (y0 + y1), dz=-z0)
    pts = [p for ob in meshes for p in evaluated_points(ob)]
    x0, x1, y0, y1, z0, z1 = box(pts)
    height = z1 - z0
    # 脚点取"贴地那一圈顶点的中心"而不是原点：四足动物的脚在身体重心偏后，
    # 用原点当锚点会让内核把宠物横向放偏。
    ground = [p for p in pts if p.z <= 0.05 * height] or pts
    feet = Vector((sum(p.x for p in ground) / len(ground), sum(p.y for p in ground) / len(ground), 0.0))
    print(
        "BAKE 归一化：高 %.3f 宽 %.3f 深 %.3f（脚底 z=0，xy 居中，脚点 %+.3f,%+.3f 由 %d 个贴地顶点得出）"
        % (height, x1 - x0, y1 - y0, feet.x, feet.y, len(ground))
    )
    return height, pts, feet


def auto_front_rear(pts, height):
    """鼻尖/尾点自动取：查朝向只需要"沿前进轴分得开"的两个点，不必是解剖学鼻尖。

    带 |y| 惩罚是为了别选中耳朵或背包侧角，那两点在前进轴上并不靠前。
    """
    head = [p for p in pts if p.z > 0.6 * height] or pts
    nose = max(head, key=lambda p: p.x - 0.5 * abs(p.y))
    rear = min(pts, key=lambda p: p.x + 0.5 * abs(p.y))
    print("BAKE 鼻尖自动取 (%+.3f,%+.3f,%+.3f)，尾点 (%+.3f,%+.3f,%+.3f)" % (*nose, *rear))
    return nose, rear


def root_bone_name(armature):
    if armature is None:
        return None
    tops = [b for b in armature.data.bones if b.parent is None]
    if not tops:
        return None
    return min(tops, key=lambda b: b.head_local.z).name


def setup_clips(armature, mapping, root_bone):
    """校验映射并记下每支剪辑的帧范围与基准脚位；剪辑名写错必须响亮失败。"""
    if armature is None:
        sys.exit("--action-map 需要模型自带骨架，这份没有")
    if armature.animation_data is None:
        armature.animation_data_create()
    # glTF 导入会把其中一支剪辑塞进 NLA，而 NLA 会盖住 animation_data.action
    for track in list(armature.animation_data.nla_tracks):
        armature.animation_data.nla_tracks.remove(track)
    clips = {}
    for state, name in mapping.items():
        act = bpy.data.actions.get(name)
        if act is None:
            sys.exit(
                "状态 %s 要的剪辑 %r 不在模型里；可用：%s"
                % (state, name, sorted(a.name for a in bpy.data.actions))
            )
        start = int(round(act.frame_range[0]))
        length = max(int(round(act.frame_range[1])) - start + 1, 1)
        armature.animation_data.action = act
        bpy.context.scene.frame_set(start)
        bpy.context.view_layer.update()
        base = armature.matrix_world @ armature.pose.bones[root_bone].head
        clips[state] = (act, start, length, base.copy())
        print("BAKE 剪辑 %s ← %s（起 %d，长 %d）" % (state, name, start, length))
    return clips


def apply_clip(armature, clips, root_bone, lock, state, frame, frames, loop):
    act, start, length, base = clips[state]
    armature.animation_data.action = act
    # 循环态取每格中点 (f+0.5)/n，末帧不会又撞回第 0 帧；一次性动画首尾都要取到
    if loop:
        f = start + (frame + 0.5) * length / max(frames, 1)
    else:
        f = start + frame * (length - 1) / max(frames - 1, 1)
    bpy.context.scene.frame_set(int(round(f)))
    if lock and root_bone:
        head = armature.matrix_world @ armature.pose.bones[root_bone].head
        armature.location.x += base.x - head.x
        armature.location.y += base.y - head.y
        bpy.context.view_layer.update()
    return f


def plant_feet(armature, meshes, home_z):
    """把当前姿态的最低点压回 z=0，返回压了多少（世界单位）。

    先把 z 复位再量：这函数每帧给 location.z 加一个增量，不复位就会一帧叠一帧，
    量出来的"补偿量"根本不是姿态本身的下沉。只复位 z —— x/y 是剪辑漂移的锁定量，
    由 apply_clip 负责，不能被这里抹掉。
    """
    armature.location.z = home_z
    zs = [p.z for ob in meshes for p in evaluated_points(ob, limit=1200)]
    dz = -min(zs)
    armature.location.z += dz
    bpy.context.view_layer.update()
    return dz


def parse_map(text, what):
    """把 "k=v,k=v" 解析成字典，供 --action-map / --bones 共用。"""
    out = {}
    for item in text.split(","):
        if not item.strip():
            continue
        if "=" not in item:
            sys.exit("%s 要写成 键=值（逗号分隔）：%s" % (what, item))
        key, val = item.split("=", 1)
        out[key.strip()] = val.strip()
    return out


# 语义角色 → 候选骨名（大小写不敏感）。对不上的角色会打印出来，不静默跳过。
BONE_ROLES = {
    "chest": ("Spine2", "Chest", "Spine1", "Spine"),
    "spine": ("Spine1", "Spine", "Abdomen"),
    "neck": ("Neck",),
    "head": ("Head",),
    "hip": ("Hips", "Pelvis", "Root"),
    "arm_l": ("LeftArm", "ArmL", "ShoulderL"),
    "arm_r": ("RightArm", "ArmR", "ShoulderR"),
    "forearm_l": ("LeftForeArm", "ForeArmL", "LeftArmLower"),
    "forearm_r": ("RightForeArm", "ForeArmR", "RightArmLower"),
    "leg_l": ("LeftUpLeg", "UpLegL", "LeftThigh"),
    "leg_r": ("RightUpLeg", "UpLegR", "RightThigh"),
    "shin_l": ("LeftLeg", "ShinL", "LeftCalf"),
    "shin_r": ("RightLeg", "ShinR", "RightCalf"),
    "foot_l": ("LeftFoot", "FootL", "AnkleL"),
    "foot_r": ("RightFoot", "FootR", "AnkleR"),
    "tail": ("Tail", "tail_01", "tail1", "Tail1"),
}

# 每支动作**非有不可**的角色 ⇒ 缺了就响亮失败，不许"那只腿不动"地混过去。
# 为什么需要这张表：resolve_bones 只强制 chest+head，而"少一根骨"在像素上只是某个部位
# 静止，14 项门禁一项都不会红（太空猫没有尾骨就是这样）。idle 故意不把腿写进来 ——
# 它的门禁（validate 的"idle 底边帧间抖动 ≤2px"）要的正是脚不动。
STATE_BONE_NEEDS = {
    "idle": (),
    "walk": ("leg_l", "leg_r", "hip"),
    "run": ("leg_l", "leg_r", "hip"),
    "stop": ("hip", "shin_l", "shin_r"),
    "jump": ("hip", "leg_l", "leg_r", "shin_l", "shin_r"),
    "climb": ("arm_l", "arm_r", "hip"),
    "sleep": ("hip", "leg_l", "leg_r"),
    "turn": ("hip",),
    "react": ("chest",),
}


def resolve_bones(armature, override):
    """按候选名把语义角色对到骨骼，再用 --bones 显式覆盖。"""
    lower = {b.name.lower(): b.name for b in armature.data.bones}
    roles = {}
    for role, cands in BONE_ROLES.items():
        for cand in cands:
            if cand.lower() in lower:
                roles[role] = lower[cand.lower()]
                break
    for role, name in override.items():
        if name not in armature.data.bones:
            sys.exit("--bones %s=%s：模型里没有这根骨；可用 %s" % (role, name, " ".join(sorted(lower.values()))))
        roles[role] = name
    print("BAKE 骨骼角色：" + " ".join("%s=%s" % kv for kv in sorted(roles.items())))
    missing = [r for r in ("chest", "head") if r not in roles]
    if missing:
        sys.exit("对不上这些必需角色：%s ⇒ 用 --bones chest=某骨,head=某骨 指认" % ",".join(missing))
    skipped = [r for r in BONE_ROLES if r not in roles]
    if skipped:
        print("BAKE 这些角色没对上，动作里会缺：%s" % " ".join(skipped))
    return roles


def rest_pose(armature):
    """清掉导入带来的剪辑，记下每根骨的 matrix_basis 作为每帧的还原基准。

    存 matrix_basis 而不是 matrix：那是骨自己的局部变换，还原它与兄弟/父骨的
    先后顺序无关（写 matrix 会读父骨当前姿态反推，还原顺序一乱就串味）。
    """
    if armature.animation_data is None:
        armature.animation_data_create()
    armature.animation_data.action = None
    for track in list(armature.animation_data.nla_tracks):
        armature.animation_data.nla_tracks.remove(track)
    bpy.context.view_layer.update()
    return {pb.name: pb.matrix_basis.copy() for pb in armature.pose.bones}


def _rot_about_head(pb, axis, deg):
    """绕骨尖在骨架空间里转：与骨骼自身的 roll 无关，换骨架不用重调角度。

    结尾必须 update：`pb.matrix` 是**依赖图算出来的只读结果**，写完不刷新就读回旧值。
    不刷的话，紧接着的第二个 helper 会用"上一次写入之前"的矩阵去复合，把前一次写入
    整个抹掉 —— 实测胸腔的抬升就是这样被紧随的旋转吃掉的，而画面只是"几乎不动"，
    任何像素门禁都不会红。
    """
    bpy.context.view_layer.update()
    head = pb.matrix.to_translation()
    pb.matrix = (
        Matrix.Translation(head)
        @ Matrix.Rotation(math.radians(deg), 4, axis)
        @ Matrix.Translation((-head[0], -head[1], -head[2]))
        @ pb.matrix
    )
    bpy.context.view_layer.update()


def _move_bone(pb, dx=0.0, dy=0.0, dz=0.0):
    pb.matrix = Matrix.Translation((dx, dy, dz)) @ pb.matrix
    bpy.context.view_layer.update()


def _scale_bone(pb, sx=1.0, sy=1.0, sz=1.0):
    """按骨局部轴缩放：X 是骨长（压扁/拉长 = 顿与蹦），Y/Z 是围长方向（呼吸要的是体积，不是平移）。

    子骨会继承（`inherit_scale = FULL`），所以调用方必须紧接着给子骨一个反缩放，
    否则"胸腔鼓"会变成"整只宠物连头盔一起胀大"。
    """
    pb.scale = (pb.scale.x * sx, pb.scale.y * sy, pb.scale.z * sz)
    bpy.context.view_layer.update()


def apply_bone_pose(armature, roles, base, height, gain, state, frame, frames, loop):
    """骨骼驱动的参数化姿态。幅度按模型高度归一 ⇒ 换模型不用重调数字。

    这里是派发入口：idle 单写在下面这一支、其余八支在 apply_bone_action。
    idle 单写在这一支、其余八支在 apply_bone_action，不是偷懒：下面那些系数是用户过眼
    定过档的（“动作有些大，一半效果就可以了”），把它们和未经目视的新动作混在同一段里，
    下次调新动作时很容易顺手改坏已经签收的那一支。

    idle 一律不碰脚部骨：待机时"逐帧最底不透明行"要稳定（validate 的门禁 ≤2px），
    而现成人类剪辑正是靠移动脚来表现重心转移的，所以不能拿来当 idle。

    这些系数怎么定的，两件事缺一不可：
    ① **先修一条静默 no-op**：骨骼版首版头顶只走 0.5 px，看着像没在呼吸，根因不是幅度小，
       是 _rot_about_head 里"写完不刷新就把上一次写入抹掉"（见那个函数的注释）。
    ② **再过眼定档**：修完先按占位小白量到同一档（它 idle 头顶峰峰 6 px、f0→f1 变化
       361–483 px），用户判"动作有些大，一半效果就可以了" ⇒ 下面全是那一档的一半。
       围长这项要特别记：缩放绕骨尖做，会把胸腔以上整段顶高，所以"抬升"必须跟着降，
       不然头顶替胸腔买单（0.045 → 0.008）。以后调观感走 --bone-gain，别改这些数字。
    """
    for bone in armature.pose.bones:
        bone.matrix_basis = base[bone.name]
    bpy.context.view_layer.update()
    if state != "idle":
        apply_bone_action(armature.pose.bones, roles, height, gain, state, frame, frames)
        return
    # 局部变量名别和上面的循环变量同名：pb 曾等于"最后一根骨"，于是 pb[roles['chest']]
    # 变成按 IDProperty 取键 ⇒ KeyError Spine2 not found（这一类错在数字上完全看不出来）
    pb = armature.pose.bones
    wave = gain * math.sin(2.0 * math.pi * frame / max(frames, 1))

    # 胸腔先动：颈/头/手臂是它的子骨，会跟着走，不必像占位小白那样手工传位移
    _move_bone(pb[roles["chest"]], dz=0.008 * height * wave)
    _rot_about_head(pb[roles["chest"]], "Y", 1.5 * wave)
    # 体积：整只平移读起来像"一块东西在飘"，围长一起一伏才是呼吸（占位小白那版是 squash）
    girth = 1.0 + 0.0375 * wave
    _scale_bone(pb[roles["chest"]], sy=girth, sz=girth)
    if "neck" in roles:
        # 子骨继承 FULL scale ⇒ 不反缩的话头盔会跟着胸腔一起胀大
        _scale_bone(pb[roles["neck"]], sy=1.0 / girth, sz=1.0 / girth)
    bpy.context.view_layer.update()
    if "head" in roles:
        # 反向 = 视线保持水平；再叠一点头自己的起伏 = 占位版那种分部件的呼吸感
        _rot_about_head(pb[roles["head"]], "Y", -1.7 * wave)
        _move_bone(pb[roles["head"]], dz=0.006 * height * wave)
    for role, sgn in (("arm_l", 1.0), ("arm_r", -1.0)):
        if role in roles:
            _rot_about_head(pb[roles[role]], "X", sgn * 1.2 * wave)
    if "tail" in roles:
        _rot_about_head(pb[roles["tail"]], "Z", 4.0 * wave)
    bpy.context.view_layer.update()


def apply_bone_action(pb, roles, height, gain, state, frame, frames):
    """除 idle 之外的八支骨骼动作（调用方已把姿态还原成 rest，这里只负责加偏移）。

    骨架空间轴（这份太空猫 rig 实测；和 bone roll 无关，因为 _rot_about_head 用的就是骨架轴）：
      +X = 角色的左手侧，−Y = 脸朝的方向，+Z = 上。
    ⇒ 绕 X **正角**：向上长的骨（脊柱、头颈）前倾；向下垂的骨（大腿、小腿、手臂）往后甩。
      绕 Y：左右侧倾（手臂绕 Y 就是侧举，左骨取负、右骨取正才都朝外）。
      绕 Z：转身。
    `_rot_about_head` 绕**骨尖**转 ⇒ 大腿绕 X 就是髋关节、小腿的是膝关节、脚的是踝关节，
    不用再给关节坐标。

    数字的两条规矩（换模型不用重调）：① 旋转项直接写**度数**（角度与模型大小无关）；
    ② 位移项一律按 height 归一（0.03 = 高的 3%）。`gain`（--bone-gain）只乘**随帧变化的量**，
    静态偏置（走路的常态前倾、睡觉的趴姿）不乘 ⇒ --bone-gain 0 得到的是“该状态的平均姿态
    冻住”，正好当对照组。

    ⚠️ 这一档幅度还没过眼（idle 那一档是用户判过的）。烘完必须目视两张拼图，尤其是
    walk/run/stop 的位移差 —— 设计书 §3.5.2 遗留③ 记的就是“读得出区别但偏弱”。
    """
    phase = frame / max(frames, 1)
    u = frame / max(frames - 1, 1)
    w = gain * math.sin(2.0 * math.pi * phase)
    bob = gain * (0.5 + 0.5 * math.cos(4.0 * math.pi * phase))

    def rot(role, axis, deg):
        """deg 是已经含幅度的角度（要么乘过 w，要么是静态偏置）⇒ 这里不再乘 gain。"""
        if role in roles:
            _rot_about_head(pb[roles[role]], axis, deg)

    def shift(role, dx=0.0, dy=0.0, dz=0.0):
        if role in roles:
            _move_bone(pb[roles[role]], dx=dx * height, dy=dy * height, dz=dz * height)

    def squash(role, tall=1.0, wide=1.0):
        """tall 沿骨长（顿与蹦）、wide 沿围长（炸毛）。会让子骨一起胀，需要保住头时用 girth。"""
        if role in roles:
            _scale_bone(pb[roles[role]], sx=tall, sy=wide, sz=wide)

    def girth(role, k):
        """围长呼吸 + 颈子骨反缩：不反缩就是“一呼一吸连头盔一起胀”（本轮实测踩过）。"""
        if role not in roles:
            return
        _scale_bone(pb[roles[role]], sy=k, sz=k)
        if "neck" in roles:
            _scale_bone(pb[roles["neck"]], sy=1.0 / k, sz=1.0 / k)

    if state in ("walk", "run"):
        # 第一档（18/7/8/0.035/2 与 30/16/14/0.055/6）烘出来 walk 帧间变化 **119 px < idle 的 135 px**，
        # 用户判"加大幅度"。根因不是角度小，是这份 rig 的腿短且被宇航服躯干挡住 ⇒ 绕髋转再多也不改剪影，
        # 真正改剪影的是**头顶高度（lean）+ 整体起伏（lift）**，所以这两项涨得比腿还多。
        # ⚠️ 没走 --bone-gain：gain 乘的是全局随帧量，动它会把已经判过档的另外 7 支一起改掉 ⇒ 只改这两支的系数。
        stride = 32.0 if state == "walk" else 52.0
        knee = 16.0 if state == "walk" else 34.0
        armsw = 18.0 if state == "walk" else 30.0
        lift = 0.060 if state == "walk" else 0.110
        lean = 6.0 if state == "walk" else 14.0
        toe = 9.0 if state == "walk" else 14.0
        rot("chest", "X", lean)
        rot("head", "X", -lean * 0.4)
        shift("hip", dz=lift * bob)  # 只往均值以上走 ⇒ 站立的接触帧不会陷进地里
        for leg, shin, foot, arm, fore, sgn in (
            ("leg_l", "shin_l", "foot_l", "arm_l", "forearm_l", 1.0),
            ("leg_r", "shin_r", "foot_r", "arm_r", "forearm_r", -1.0),
        ):
            swing = sgn * w  # 正 = 这条腿摆到身后
            rot(leg, "X", stride * swing)
            rot(shin, "X", knee * (0.35 + 0.65 * max(0.0, swing)))  # 膝永远微弯，后腿收得多
            rot(foot, "X", toe * max(0.0, swing))  # 蹬地侧脚尖下压
            rot(arm, "X", -armsw * swing)  # 臂与同侧腿反相
            rot(fore, "X", -(6.0 + armsw * 0.4 * swing))  # 肘常态前折 6°

    elif state == "stop":
        # 3 帧一次性 ⇒ 按帧号索引常量元组，不用正弦（用 f/frames 归一会让末帧停在 3/4 处）
        lean = gain * (10.0, -3.5, 0.0)[frame]
        dip = gain * (0.030, 0.006, 0.0)[frame]
        kneea = gain * (14.0, 5.0, 0.0)[frame]
        tall = 1.0 + gain * (-0.05, 0.02, 0.0)[frame]
        rot("chest", "X", lean)
        rot("head", "X", -lean * 0.6)  # 头滞后 = 重量感
        shift("hip", dz=-dip)
        for leg, shin in (("leg_l", "shin_l"), ("leg_r", "shin_r")):
            rot(shin, "X", kneea)  # 膝盖吸收惯性
            rot(leg, "X", -kneea * 0.35)  # 大腿略往前，脚才还留在地上
        for arm in ("arm_l", "arm_r"):
            rot(arm, "X", lean * 0.5)  # 手臂被惯性甩到身后
        squash("chest", tall=tall, wide=1.0 + (1.0 - tall))

    elif state == "jump":
        # 蹲 → 蹬 → 空中收 → 落地预备。⚠️ 动画里离地只有几 px：真正的抛物线由内核移 surface，
        # 这里再叠一条弧就变成两倍弹跳。
        dz = gain * (-0.045, 0.055, 0.020, -0.030)[frame]
        thigh = gain * (-14.0, 10.0, -20.0, -8.0)[frame]
        kneea = gain * (24.0, -4.0, 30.0, 16.0)[frame]
        lean = gain * (12.0, -10.0, -4.0, 8.0)[frame]
        armsw = gain * (16.0, -34.0, -40.0, -20.0)[frame]
        toe = gain * (6.0, -8.0, -2.0, 8.0)[frame]
        tall = 1.0 + gain * (-0.09, 0.12, 0.05, -0.06)[frame]
        shift("hip", dz=dz)
        rot("chest", "X", lean)
        rot("head", "X", -lean * 0.5)
        squash("chest", tall=tall, wide=1.0 + (1.0 - tall))
        for leg, shin, foot in (("leg_l", "shin_l", "foot_l"), ("leg_r", "shin_r", "foot_r")):
            rot(leg, "X", thigh)
            rot(shin, "X", kneea)
            rot(foot, "X", toe)
        for arm, fore in (("arm_l", "forearm_l"), ("arm_r", "forearm_r")):
            rot(arm, "X", armsw)
            rot(fore, "X", -(10.0 + armsw * 0.3))
        rot("tail", "X", gain * (-10.0, 14.0, 18.0, 6.0)[frame])

    elif state == "climb":
        # 贴墙向上：身体后仰 + 两臂交替上够 + 同侧腿跟着提。和 walk 分得开靠的就是
        # “后仰（与走路前倾反号）+ 手臂上够”这两项（第二批那条坑的同一族）
        alt = w  # 4 帧循环 ⇒ 0, 1, 0, −1
        rot("chest", "X", -8.0)
        rot("head", "X", 14.0)  # 抬头看上面
        shift("hip", dz=gain * 0.014 * alt)
        for arm, fore, leg, shin, sgn in (
            ("arm_l", "forearm_l", "leg_l", "shin_l", 1.0),
            ("arm_r", "forearm_r", "leg_r", "shin_r", -1.0),
        ):
            up = 0.5 + 0.5 * sgn * alt  # 这条臂现在在上方吗（0..1）
            rot(arm, "X", -62.0 * up)  # 负角 = 手臂往前上方够
            rot(fore, "X", -14.0 * up)
            rot(leg, "X", -14.0 * up)
            rot(shin, "X", 22.0 * up)
        rot("tail", "Z", 6.0 * w)

    elif state == "sleep":
        # 2 帧 @4fps，而且这是唯一真停帧的状态。⚠️ 循环正弦在 frames=2 时两帧都是 0
        # （sin(πf)）⇒ 必须按帧号给，f0 = 呼、f1 = 吸。没有 IK，躺不下来，
        # 靠“压低 + 蜷 + 头垂 + 减幅呼吸”读“睡了”。
        puff = gain * (0.0, 1.0)[frame % 2]
        shift("hip", dz=-(0.050 + 0.006 * puff))
        rot("chest", "X", 16.0 + 3.0 * puff)  # 趴着，吸气那帧再压一点
        rot("head", "X", 10.0 - 5.0 * puff)
        girth("chest", 1.0 + 0.020 * puff)  # 呼吸幅度取 idle 那一档的一半
        for leg, shin in (("leg_l", "shin_l"), ("leg_r", "shin_r")):
            rot(leg, "X", -18.0)
            rot(shin, "X", 34.0)
        for arm, fore in (("arm_l", "forearm_l"), ("arm_r", "forearm_r")):
            rot(arm, "X", 6.0)
            rot(fore, "X", -30.0)  # 手收在下巴前
        rot("tail", "X", -24.0)  # 尾巴卷到身前（无尾骨则跳过）

    elif state == "turn":
        # 原地枢轴 ±17°：朝向列是离散的 8 列，这四帧负责把“列与列之间”那一下糊过去
        yaw = gain * (-17.0 + 34.0 * u)
        pinch = gain * math.sin(math.pi * u)
        rot("hip", "Z", yaw)
        rot("chest", "Z", yaw * 0.30)  # 上身滞后
        rot("head", "Z", -yaw * 0.25)  # 头最后转回来 = 重量感
        squash("chest", tall=1.0 + 0.05 * pinch, wide=1.0 - 0.06 * pinch)  # 转到侧面就收窄
        for role, sgn in (("arm_l", 1.0), ("arm_r", -1.0)):
            rot(role, "X", sgn * 8.0 * pinch)  # 双臂随转身前后分开
            rot(role, "Y", -sgn * 10.0 * pinch)  # 顺带向外张开
        for role, sgn in (("leg_l", 1.0), ("leg_r", -1.0)):
            rot(role, "X", -sgn * 6.0 * pinch)  # 碎步

    elif state == "react":
        # 被吓到：弹一丁点 + 炸毛。这份 rig 没有 morph target（眨眼、真蓬松都做不了）⇒
        # 和占位小白同款，全靠缩放伪造，胸腔围长取 idle 那一档的两倍多。
        pop = gain * (0.0, 1.0, 0.45)[frame]
        shift("hip", dz=0.020 * pop)
        girth("chest", 1.0 + 0.100 * pop)
        squash("chest", tall=1.0 + 0.05 * pop)
        rot("chest", "X", -6.0 * pop)  # 挺起
        rot("head", "X", -9.0 * pop)  # 仰头
        shift("head", dz=0.012 * pop)
        for role, sgn in (("arm_l", 1.0), ("arm_r", -1.0)):
            rot(role, "Y", -sgn * 14.0 * pop)  # 双臂向外张（左负右正才都朝外）
            rot(role, "X", -10.0 * pop)
        for role in ("forearm_l", "forearm_r"):
            rot(role, "X", -18.0 * pop)
        rot("tail", "X", -14.0 * pop)
        rot("tail", "Z", 10.0 * pop)

    else:
        sys.exit("骨骼参数化姿态没有这一支：%s" % state)
    bpy.context.view_layer.update()


def set_input(node_tree, names, value):
    """按候选名设置 BSDF 输入：5.x 的输入标签和 4.x 不同，逐个试。"""
    for name in names:
        sock = node_tree.nodes.get("Principled BSDF").inputs.get(name)
        if sock is not None:
            sock.default_value = value
            return True
    return False


def flat_material(name, rgb):
    mat = bpy.data.materials.new(name)
    mat.use_nodes = True
    tree = mat.node_tree
    set_input(tree, ("Base Color",), (*rgb, 1.0))
    set_input(tree, ("Roughness",), 1.0)
    set_input(tree, ("Specular IOR Level", "Specular"), 0.0)
    return mat


def add_part(name, kind, loc, scale, mat, parent, rot=(0.0, 0.0, 0.0)):
    ops = {
        "sphere": lambda: bpy.ops.mesh.primitive_uv_sphere_add(radius=1.0, segments=24, ring_count=16),
        # radius2 必须显式给：primitive_cone_add 的 radius2 默认也是 1.0，不写就是个圆柱
        "cone": lambda: bpy.ops.mesh.primitive_cone_add(radius1=1.0, radius2=0.06, depth=2.0, vertices=20),
        "cyl": lambda: bpy.ops.mesh.primitive_cylinder_add(radius=1.0, depth=2.0, vertices=20),
    }
    ops[kind]()
    ob = bpy.context.active_object
    ob.name = name
    ob.location = loc
    ob.scale = scale
    ob.rotation_euler = rot
    ob.data.materials.append(mat)
    if parent is not None:
        ob.parent = parent
    return ob


# 头球中心与半轴：脸上所有部件都由它算出来，不再靠肉眼猜坐标
HEAD_C = (0.06, 0.0, 0.80)
HEAD_R = (0.25, 0.23, 0.22)


def head_surface(dx, dy, dz, push=0.0):
    """头球面上（或向外 push 一点）的点。方向向量任意长度，内部归一。"""
    n = math.sqrt(dx * dx + dy * dy + dz * dz)
    ux, uy, uz = dx / n, dy / n, dz / n
    return (
        HEAD_C[0] + HEAD_R[0] * ux + push * ux,
        HEAD_C[1] + HEAD_R[1] * uy + push * uy,
        HEAD_C[2] + HEAD_R[2] * uz + push * uz,
    )


def build_cat():
    """脚底贴世界 z=0；body/head/tail/ears/legs 挂在 root Empty 上做纯 transform 动画。

    脸部件（眼/鼻/耳内）**一律挂 root 而不是挂头球**：`ob.parent = head` 之后
    `matrix_world = head_matrix @ ob_matrix`，父级的 scale (0.25,0.23,0.22) 会把子级
    的偏移和自身尺寸一起缩掉 —— 上一版的眼球因此只剩 0.0075 世界单位、还被挤回心头，
    耳内缩成一个点，等于整张脸不存在（帧里看不见，validate 也看不见）。改成挂 root
    后局部坐标就是世界坐标，动画里由 HEAD_PARTS/EAR_PARTS 把父级位移显式带上。
    """
    white = flat_material("shiro_body", (0.93, 0.93, 0.96))
    dark = flat_material("shiro_dark", (0.10, 0.10, 0.12))
    pink = flat_material("shiro_inner", (0.85, 0.55, 0.60))

    bpy.ops.object.empty_add(location=(0.0, 0.0, 0.0))
    root = bpy.context.active_object
    root.name = "root"

    parts = {}

    def reg(key, ob, bob=0.0, sway=0.0, squash=0.0):
        parts[key] = {
            "ob": ob,
            "loc": tuple(ob.location),
            "rot": tuple(ob.rotation_euler),
            "scl": tuple(ob.scale),
            "bob": bob,
            "sway": sway,
            "squash": squash,
        }

    reg("root", root)
    reg("body", add_part("body", "sphere", (0.0, 0.0, 0.40), (0.30, 0.26, 0.30), white, root), bob=0.030, squash=0.020)
    reg("head", add_part("head", "sphere", HEAD_C, (0.25, 0.23, 0.22), white, root), bob=0.045)
    reg("ear_l", add_part("ear_l", "cone", (0.10, 0.15, 0.985), (0.075, 0.075, 0.088), white, root), bob=0.050)
    reg("ear_r", add_part("ear_r", "cone", (0.10, -0.15, 0.985), (0.075, 0.075, 0.088), white, root), bob=0.050)
    reg("ear_l_in", add_part("ear_l_in", "cone", (0.145, 0.150, 0.955), (0.040, 0.040, 0.050), pink, root), bob=0.050)
    reg("ear_r_in", add_part("ear_r_in", "cone", (0.145, -0.150, 0.955), (0.040, 0.040, 0.050), pink, root), bob=0.050)
    reg("eye_l", add_part("eye_l", "sphere", head_surface(0.78, 0.44, 0.50, 0.010), (0.030, 0.030, 0.044), dark, root), bob=0.045)
    reg("eye_r", add_part("eye_r", "sphere", head_surface(0.78, -0.44, 0.50, 0.010), (0.030, 0.030, 0.044), dark, root), bob=0.045)
    reg("nose", add_part("nose", "sphere", head_surface(0.95, 0.0, -0.28, 0.012), (0.032, 0.026, 0.022), pink, root), bob=0.045)
    reg("tail", add_part("tail", "cone", (-0.33, 0.0, 0.56), (0.050, 0.050, 0.17), white, root, rot=(0.0, -0.55, 0.0)), sway=0.35, bob=0.015)

    # 腿不参与上下浮：脚底 z=0 是"逐帧最底不透明行"的唯一来源，一动 validate 就报抖动
    legs = {
        "leg_fl": ((0.17, 0.15), 0.0),
        "leg_fr": ((0.17, -0.15), 0.0),
        "leg_bl": ((-0.17, 0.15), 0.5),
        "leg_br": ((-0.17, -0.15), 0.5),
    }
    for key, ((x, y), swing_phase) in legs.items():
        ob = add_part(key, "cyl", (x, y, 0.11), (0.065, 0.065, 0.11), white, root)
        reg(key, ob)
        parts[key]["swing_phase"] = swing_phase
    return parts


# 部件分组：脸部件挂 root（见 build_cat 的注释），所以父级位移要显式带上
HEAD_CORE = ("head", "eye_l", "eye_r", "nose")
EARS_L = ("ear_l", "ear_l_in")
EARS_R = ("ear_r", "ear_r_in")
EAR_ALL = EARS_L + EARS_R
FRONT_LEGS = ("leg_fl", "leg_fr")
REAR_LEGS = ("leg_bl", "leg_br")
LEG_ALL = FRONT_LEGS + REAR_LEGS
AIRBORNE_DZ_MAX = 0.04  # 离地靠内核移 surface，序列帧只给姿态；再高就顶到格子上边


def apply_pose(parts, state, frame, frames, loop=True):
    """参数化动作：9 个状态全靠这里，模型本身不动。

    时间轴分两种：循环态用 `f/frames`（末帧要接得回第 0 帧），一次性动画用
    `f/(frames-1)`（否则末帧停在 3/4 处，看着像被截断）。
    """
    phase = frame / max(frames, 1)
    u = frame / max(frames - 1, 1)
    wave = math.sin(2.0 * math.pi * phase)

    for part in parts.values():
        ob = part["ob"]
        ob.location = part["loc"]
        ob.rotation_euler = part["rot"]
        ob.scale = part["scl"]

    def move(key, dz=0.0, dx=0.0, rot=None, scl=None):
        """增量叠加（base 已在函数开头还原），所以同一部件调多次也安全。"""
        ob = parts[key]["ob"]
        ob.location = (ob.location.x + dx, ob.location.y, ob.location.z + dz)
        if rot is not None:
            ob.rotation_euler = tuple(a + b for a, b in zip(ob.rotation_euler, rot))
        if scl is not None:
            ob.scale = tuple(a * b for a, b in zip(ob.scale, scl))

    def group(keys, **kw):
        for key in keys:
            move(key, **kw)

    if state == "idle":
        for key, part in parts.items():
            if part["bob"]:
                move(key, dz=part["bob"] * wave)
            if part["sway"]:
                move(key, rot=(0.0, part["sway"] * wave, 0.0))
            if part["squash"]:
                move(key, scl=(1.0, 1.0, 1.0 + part["squash"] * wave))

    elif state in ("walk", "run"):
        stride = {"walk": 0.10, "run": 0.20}[state]
        bob_gain = {"walk": 1.0, "run": 1.7}[state]
        move("root", rot=(0.0, {"walk": 0.05, "run": 0.13}[state], 0.0))
        for key, part in parts.items():
            if part["bob"]:
                move(key, dz=part["bob"] * wave * bob_gain)
            if part["sway"]:
                move(key, rot=(0.0, part["sway"] * wave * bob_gain, 0.0))
        # 前后腿反相靠 swing_phase（前 0.0 / 后 0.5），这里只加步幅
        for key in LEG_ALL:
            move(key, rot=(0.0, stride * math.sin(2.0 * math.pi * (phase + parts[key]["swing_phase"])), 0.0))

    elif state == "stop":
        # 惯性前冲 → 回弹 → 站稳：只有 3 帧，靠身体俯仰和压缩读动作
        lean = (0.18, -0.07, 0.0)[frame]
        brace = 1.0 - u
        squash = (0.90, 1.07, 1.0)[frame]
        move("root", rot=(0.0, lean, 0.0))
        move("body", scl=(2.0 - squash, 1.0, squash))
        group(HEAD_CORE, dx=-0.05 * lean * 3.0)  # 头滞后 = 重量感
        move("tail", rot=(0.0, 0.30 * brace, 0.0))
        group(FRONT_LEGS, rot=(0.0, -0.26 * brace, 0.0))  # 前爪撑地
        group(REAR_LEGS, rot=(0.0, 0.22 * brace, 0.0))

    elif state == "jump":
        # 蹲 → 蹬 → 空中收 → 落地预备。离地只有几 px，真正的抛物线由内核移 surface
        body_z = (0.84, 1.16, 1.05, 0.92)[frame]
        bend = (0.30, -0.26, -0.44, 0.22)[frame]
        move("root", dz=(0.0, 0.01, AIRBORNE_DZ_MAX, 0.01)[frame], rot=(0.0, (-0.08, 0.16, 0.05, -0.10)[frame], 0.0))
        move("body", scl=(1.0 + 0.08 * (1.0 - body_z), 1.0, body_z))
        group(HEAD_CORE, dz=(-0.05, 0.07, 0.05, 0.0)[frame])
        group(EAR_ALL, dz=(-0.05, 0.07, 0.05, 0.0)[frame])
        group(FRONT_LEGS, rot=(0.0, bend, 0.0))
        group(REAR_LEGS, rot=(0.0, bend * 0.7, 0.0))
        move("tail", rot=(0.0, (-0.20, 0.35, 0.50, 0.25)[frame], 0.0))

    elif state == "climb":
        # 贴墙向上：整个身体后仰"扒"在墙上，前后爪交替够/蹬。循环态 ⇒ 用 phase
        alt = wave  # 4 帧 → 0, 1, 0, −1
        move("root", dz=0.012 * alt, rot=(0.0, -0.30, 0.0))
        move("body", scl=(0.94, 1.0, 1.10 + 0.03 * alt))
        group(HEAD_CORE, dz=0.055, dx=-0.02)
        group(EAR_ALL, dz=0.055, dx=-0.02)
        group(FRONT_LEGS, rot=(0.0, -0.58 + 0.22 * alt, 0.0))  # 前爪向上够
        group(REAR_LEGS, rot=(0.0, 0.40 - 0.22 * alt, 0.0))  # 后爪蹬墙
        move("tail", rot=(0.0, -0.30, 0.0))

    elif state == "sleep":
        # 趴成一坨 + 呼吸：2 帧 @4fps，只靠 body 的压缩和耳朵的垂度
        breath = (1.00, 1.06)[frame]
        move("body", dz=-0.06, scl=(1.07, 1.07, 0.76 * breath))
        group(HEAD_CORE, dz=-0.14, dx=0.03)
        group(EARS_L, dz=-0.10, rot=(0.35, 0.0, 0.0))  # 耷拉到两边
        group(EARS_R, dz=-0.10, rot=(-0.35, 0.0, 0.0))
        for key in LEG_ALL:
            move(key, dz=-0.02, rot=(0.0, 0.20 if key in FRONT_LEGS else -0.20, 0.0))
        move("tail", dz=-0.06, rot=(0.0, 0.62, 0.0))  # 卷到身前

    elif state == "turn":
        # 原地枢轴：绕 Z 扫过 ±17°，转到接近侧面时身体收窄、尾巴滞后
        yaw = -0.30 + 0.60 * u
        pinch = math.sin(math.pi * u)
        move("root", rot=(0.0, 0.0, yaw))
        move("body", scl=(1.0 - 0.06 * pinch, 1.0, 1.0 + 0.05 * pinch))
        move("tail", rot=(0.0, 0.0, -0.45 * yaw))
        step = (0.10, 0.0, -0.10, 0.0)[frame]
        group(LEG_ALL, rot=(0.0, step, 0.0))

    elif state == "react":
        # 被吓到：弹起一丁点 + 炸毛（尾巴变粗、耳朵外张、瞳孔那对眼睛放大）
        pop = (0.0, 1.0, 0.45)[frame]
        move("root", dz=0.02 * pop)
        move("body", scl=(1.0 + 0.04 * pop, 1.0 + 0.12 * pop, 1.0 + 0.07 * pop))
        group(HEAD_CORE, dz=0.06 * pop)
        group(EARS_L, dz=0.09 * pop, rot=(0.0, 0.0, 0.22 * pop))
        group(EARS_R, dz=0.09 * pop, rot=(0.0, 0.0, -0.22 * pop))
        move("eye_l", scl=(1.0 + 0.15 * pop, 1.0 + 0.15 * pop, 1.0 + 0.15 * pop))
        move("eye_r", scl=(1.0 + 0.15 * pop, 1.0 + 0.15 * pop, 1.0 + 0.15 * pop))
        move("tail", dz=0.03 * pop, rot=(0.0, -0.35 * pop, 0.0), scl=(1.0 + 0.35 * pop, 1.0 + 0.35 * pop, 1.0))
        group(LEG_ALL, rot=(0.0, 0.12 * pop, 0.0))


def setup_render(scene, cell, samples):
    scene.render.engine = "BLENDER_EEVEE"
    scene.render.resolution_x = cell
    scene.render.resolution_y = cell
    scene.render.resolution_percentage = 100
    scene.render.film_transparent = True
    scene.render.image_settings.file_format = "PNG"
    scene.render.image_settings.color_mode = "RGBA"
    eevee = getattr(scene, "eevee", None)
    if eevee is not None and hasattr(eevee, "taa_render_samples"):
        eevee.taa_render_samples = samples
    view = scene.view_settings
    # view_transform 是动态枚举，bl_rna 的 enum_items 只给静态的 NONE ⇒ 只能试赋值
    for cand in ("Standard", "Filmic", "AgX"):
        try:
            view.view_transform = cand
            break
        except TypeError:
            continue
    return view.view_transform


def place_camera(cam, index):
    yaw = math.radians(index * 45.0)
    # 相机在"宠物前进方向右侧 90°"，于是 yaw=0（朝 East）时鼻尖落在屏幕右
    azim = yaw - math.pi / 2.0
    horiz = CAM_RADIUS * math.cos(math.radians(CAM_ELEV_DEG))
    cam.location = (
        horiz * math.cos(azim),
        horiz * math.sin(azim),
        CAM_RADIUS * math.sin(math.radians(CAM_ELEV_DEG)) + TARGET_Z,
    )
    bpy.context.view_layer.update()


def project(cam, point):
    co = world_to_camera_view(bpy.context.scene, cam, point)
    return co.x, co.y


def check_facing(cam):
    """把"朝向左右反掉"从需要人眼看拼图的静默 bug，变成会响亮失败的断言。"""
    problems = []
    for index, expect in FACE_SCREEN_SIDE.items():
        place_camera(cam, index)
        nx, _ = project(cam, NOSE)
        rx, _ = project(cam, REAR)
        delta = nx - rx
        if expect == 0:
            if abs(delta) > 0.06:
                problems.append("col %d 正对/背对却有左右偏移 %.3f" % (index, delta))
        elif delta * expect <= 0.02:
            problems.append("col %d 期望鼻尖在%s侧，实测偏移 %.3f" % (index, "右" if expect > 0 else "左", delta))
    return problems


def part_samples(parts):
    """占位小白的采样：每个图元物体的 bound_box 角点。"""

    def sample():
        out = []
        for key, part in parts.items():
            ob = part["ob"]
            if not isinstance(ob.data, bpy.types.Mesh):
                continue  # root 是 Empty
            out.append((key, [ob.matrix_world @ Vector(c) for c in ob.bound_box]))
        return out

    return sample


def mesh_samples(meshes):
    """外部模型的采样：整个网格按评估后的顶点看，姿态由骨架决定。"""

    def sample():
        return [(ob.name, evaluated_points(ob)) for ob in meshes]

    return sample


def check_framing(cam, cell, spec, states, columns, pose, samples):
    """把两种静默故障变成响亮失败：零件被父级 scale 缩到看不见、姿态超出格子。

    这两种情况在像素里都只是"少了一块"，validate 数不透明像素是查不出来的（上一版的眼球
    实际只剩 0.0075 世界单位、耳内退化成一个点，全套门禁当时全绿）。这里不渲染，只做投影，
    所以每一帧姿态都能查。
    """
    tiny, clipped, tight = {}, {}, {}
    # 下面两条门禁的阈值原本是裸 px，而"1 px 占格子多少"随格子变 ⇒ 提档后同一个构图
    # 会被判得比 @96 更松（1/192 比 1/96 细一倍）。基准档 = 96，阈值按 cell/96 同比走，
    # 于是 cell=96 时逐位等于历史值（--check-only 的旧输出文本仍可当回归判据）。
    unit = cell / 96.0
    for state in states:
        frames = len(spec["states"][state]["rows"])
        loop = spec["states"][state].get("loop", True)
        for index in columns:
            place_camera(cam, index)
            for frame in range(frames):
                pose(state, frame, frames, loop)
                bpy.context.view_layer.update()
                for key, pts in samples():
                    co = [world_to_camera_view(bpy.context.scene, cam, p) for p in pts]
                    xs = [c.x for c in co]
                    ys = [c.y for c in co]
                    size = max((max(xs) - min(xs)) * cell, (max(ys) - min(ys)) * cell)
                    tiny[key] = min(tiny.get(key, 9e9), size)
                    # 留边 = 部件投影到最近格边的距离；负数表示已经裁掉
                    over = max(-min(xs), max(xs) - 1.0, -min(ys), max(ys) - 1.0)
                    clearance = (-over * cell) if over > 0.005 else min(
                        min(xs), 1.0 - max(xs), min(ys), 1.0 - max(ys)
                    ) * cell
                    if clearance < 1.0 * unit:
                        clipped[key] = min(clipped.get(key, 9e9), clearance)
                    tight[key] = min(tight.get(key, 9e9), clearance)
    problems = []
    for key, size in sorted(tiny.items()):
        if size < 2.0 * unit:
            problems.append("%s 整组姿态里投影最大只有 %.1f px ⇒ 基本看不见" % (key, size))
    for key, px in sorted(clipped.items()):
        problems.append("%s 离格边只剩 %.1f px（会被裁或已经裁掉）" % (key, px))
    return problems, tiny, tight


def feet_anchor_px(cam, cell):
    nx, ny = project(cam, FEET)
    return round(nx * cell, 3), round((1.0 - ny) * cell, 3)


def parse_vec(text):
    parts = [t.strip() for t in text.split(",")]
    if len(parts) != 3:
        sys.exit("要写成 x,y,z：%s" % text)
    try:
        return Vector(float(v) for v in parts)
    except ValueError:
        sys.exit("不是三个数字: %s" % text)


def main():
    global ORTHO_SCALE, TARGET_Z, NOSE, REAR, FEET
    args = parse_args()
    if args.outfit_only and not args.outfit:
        # 不是挑剔写法：一个"只在某条路径上有意义"的开关如果允许单独给，它就会静默什么都不做，
        # 而那正是 `--cell` 那条假开关的形状（存在 ≠ 能用）。拒绝比沉默便宜。
        sys.exit("--outfit-only 必须和 --outfit 一起用：它改的是「画面里留不留载体」，"
                 "没有配件就没有可留之物")
    recorded = restore_from_meta(args) if args.outfit_measure_only else None
    spec = load_fixture()
    cell = args.cell or spec["grid"]["w"]
    if spec["grid"]["w"] != spec["grid"]["h"]:
        sys.exit("fixture 的 grid.w/h 不相等（%s/%s），本脚本按方格写" % (spec["grid"]["w"], spec["grid"]["h"]))

    states = [s.strip() for s in args.state.split(",") if s.strip()]
    unknown = [s for s in states if s not in spec["states"]]
    if unknown:
        sys.exit("fixture 里没有这些状态: %s" % unknown)
    columns = [int(c) for c in args.facing.split(",") if c.strip() != ""]

    lift = {}
    base = None  # 只有参数化骨骼那条分支会填；--outfit 要靠它判断附着对象在不在
    armature = None
    roles = {}
    height = 0.0
    cam_note = None
    if args.model:
        meshes = load_model(args.model)
        armature = next((o for o in bpy.data.objects if o.type == "ARMATURE"), None)
        height, pts, feet = normalize_model(meshes, args.face_rot_z)
        FEET = feet
        nose_src = "自动"
        rear_src = "自动"
        NOSE, REAR = auto_front_rear(pts, height)
        if args.nose:
            NOSE, nose_src = parse_vec(args.nose), "显式 --nose"
        if args.rear:
            REAR, rear_src = parse_vec(args.rear), "显式 --rear"
        # 生效值（含显式覆盖）与 Δy 一起打，但**要到 ortho 定完**才打，见下面相机那一行。
        if args.ortho_scale <= 0:
            # 初值按占位小白那一档的比例等比放大（它高 PLACEHOLDER_H 时用 1.55），
            # 真正的边距靠 --check-only 报出来的数收
            ORTHO_SCALE = round(height * (ORTHO_SCALE / PLACEHOLDER_H), 3)
        if args.target_z < 0:
            TARGET_Z = round(0.5 * height, 3)
        bone = root_bone_name(armature)
        mapping = parse_map(args.action_map, "--action-map") if args.action_map else {}

        if mapping:
            clips = setup_clips(armature, mapping, bone)
            missing = [s for s in states if s not in clips]
            if missing:
                sys.exit(
                    "这些状态没给 --action-map 映射：%s\n  模型里可用：%s"
                    % (", ".join(missing), ", ".join(sorted(a.name for a in bpy.data.actions)) or "（没有剪辑）")
                )
            home_z = armature.location.z if armature else 0.0

            def pose(state, frame, frames, loop):
                apply_clip(armature, clips, bone, args.lock_root_motion, state, frame, frames, loop)
                if args.plant_feet:
                    lift[(state, frame)] = plant_feet(armature, meshes, home_z)

            print("BAKE 姿态来源：模型自带剪辑")
        else:
            if armature is None:
                sys.exit(
                    "--model 这份既没骨架也没给 --action-map ⇒ 摆不出姿态。"
                    "有骨架就别带 --action-map 走参数化骨骼姿态，或换一个绑好骨的模型"
                )
            roles = resolve_bones(armature, parse_map(args.bones, "--bones") if args.bones else {})
            # resolve_bones 只强制 chest+head ⇒ 少一根腿骨不会报错，只会"那条腿静止"。
            # 缺的是哪一支的必需骨、怎么补，都在这里点名，别等到拼图阶段才发现某状态没在动。
            lack = {
                s: [r for r in STATE_BONE_NEEDS[s] if r not in roles]
                for s in states
                if STATE_BONE_NEEDS.get(s)
            }
            lack = {s: v for s, v in lack.items() if v}
            if lack:
                sys.exit(
                    "这些状态缺必需的骨骼角色（参数化姿态会静默退化成半个动作，所以直接拒绝）：\n"
                    + "".join("  %s 缺 %s\n" % (s, ", ".join(v)) for s, v in sorted(lack.items()))
                    + "  补法：--bones 角色=骨名（骨名写错会点名列出模型里全部可用骨）；\n"
                    + "  或只烘不缺的状态（--state idle），其余用 --action-map 套自带剪辑。"
                )
            base = rest_pose(armature)
            home_z = armature.location.z

            def pose(state, frame, frames, loop):
                apply_bone_pose(armature, roles, base, height, args.bone_gain, state, frame, frames, loop)
                # 贴地锁定只用于 idle：参数化动作里的垂直位移是**有意写的**（蹲、蹦、趴），
                # 把每帧最低点压回 z=0 等于把它们逐个取消。而且走路一摆腿，"最低点"就换一根骨，
                # 逐帧压回去会让整只宠物跟着上下滑。
                if args.plant_feet and state == "idle":
                    lift[(state, frame)] = plant_feet(armature, meshes, home_z)
                else:
                    armature.location.z = home_z
                    bpy.context.view_layer.update()

            print("BAKE 姿态来源：参数化骨骼（幅度按模型高 %.3f 归一 ×gain %.2f）" % (height, args.bone_gain))

        samples = mesh_samples(meshes)
        # 相机那行**必须在 CLI 覆盖之后**再印：先前它在这里用覆盖前的全局值打，于是命令行
        # 显式给了 --ortho-scale 时读数是一句假话（而还原测量场景时靠的就是这行读数）。
        cam_note = (height, args.face_rot_z)
    else:
        wipe_scene()
        parts = build_cat()

        def pose(state, frame, frames, loop):
            apply_pose(parts, state, frame, frames, loop)

        samples = part_samples(parts)

    if args.ortho_scale > 0:
        ORTHO_SCALE = args.ortho_scale
    if args.target_z >= 0:
        TARGET_Z = args.target_z
    if cam_note is not None:
        print("BAKE 相机：ortho_scale=%.3f target_z=%.3f（模型高 %.3f，脸转 %g°）" % (ORTHO_SCALE, TARGET_Z, cam_note[0], cam_note[1]))
        # 生效点必须在**两处覆盖都打完**之后再打：原先只打"自动取"那一行，于是显式给了
        # --nose/--rear 时日志说的是覆盖前的点而自检量的是覆盖后的点 —— 我拿那行去对"老虎交付
        # 批次朝向自检通过"就撞上了一次假矛盾（那批走的正是显式绕过）。与 --ortho-scale 同族，
        # 只是这里骗的是一条门禁的输入。Δy/ortho 就是 col 2/6 那两个"正对/背对"列的屏幕偏移
        # ⇒ 一并打出来，下次换载体不用靠撞红才知道自动尾点抓到了甩到一侧的尾巴。
        print(
            "BAKE 朝向自检实际用的点：鼻尖 %s (%+.3f,%+.3f,%+.3f) / 尾点 %s (%+.3f,%+.3f,%+.3f)；"
            "Δy %.3f ⇒ col 2/6 屏幕偏移 %.3f（门禁 0.06）"
            % (nose_src, *NOSE, rear_src, *REAR, NOSE.y - REAR.y, abs(NOSE.y - REAR.y) / ORTHO_SCALE)
        )
        # 只打印、不改任何返回值（改 auto_front_rear 本身 = check_facing 那条待用户拍板的账）：
        # 中线带内的最靠后点，给"自动尾点抓到尾巴尖"这种载体一个显式 --rear 候选。带宽按模型高
        # 取（3%），写死世界单位 = 又一条换个载体就换含义的门禁。
        band = 0.03 * cam_note[0]
        mid = [p for p in pts if abs(p.y) <= band]
        if mid:
            cand = min(mid, key=lambda p: p.x)
            print("BAKE 尾点候选（中线带 |y|<=%.3f 内最靠后，仅打印不参与判定）：(%+.3f,%+.3f,%+.3f)"
                  % (band, *cand))

    scene = bpy.context.scene
    used_transform = setup_render(scene, cell, args.samples)

    cam_data = bpy.data.cameras.new("cam")
    cam_data.type = "ORTHO"
    cam_data.ortho_scale = ORTHO_SCALE
    cam_data.clip_start = 0.1
    cam_data.clip_end = 50.0
    cam = bpy.data.objects.new("cam", cam_data)
    scene.collection.objects.link(cam)
    scene.camera = cam

    bpy.ops.object.empty_add(location=(0.0, 0.0, TARGET_Z))
    target = bpy.context.active_object
    target.name = "cam_target"
    con = cam.constraints.new("TRACK_TO")
    con.target = target
    con.track_axis = "TRACK_NEGATIVE_Z"
    con.up_axis = "UP_Y"

    # 主光 + 对侧补光 + 环境光：只有一盏太阳时，背光那几列会拍成全黑剪影
    for key, energy, zdeg in (("sun_key", 2.2, -30.0), ("sun_fill", 1.1, 150.0)):
        data = bpy.data.lights.new(key, "SUN")
        data.energy = energy
        ob = bpy.data.objects.new(key, data)
        ob.rotation_euler = (math.radians(50.0), 0.0, math.radians(zdeg))
        scene.collection.objects.link(ob)

    world = bpy.data.worlds.get("World") or bpy.data.worlds.new("World")
    scene.world = world
    world.use_nodes = True
    bg = world.node_tree.nodes.get("Background")
    if bg is not None:
        bg.inputs[0].default_value = (0.55, 0.55, 0.60, 1.0)
        bg.inputs[1].default_value = 0.55

    if args.outfit:
        if args.outfit_only and args.outfit_measure_only:
            sys.exit("--outfit-only 与 --outfit-measure-only 互斥：测量轮一个像素都不渲（它 return 在渲染之前），"
                     "所以'只渲配件'在那条路径上没有任何作用 ⇒ 两个一起给就是静默吞掉一个开关。"
                     "要挂点就单给 --outfit-measure-only，要配件图集就单给 --outfit-only")
        if not args.model:
            sys.exit("--outfit 必须和 --model 一起用：配件要刚性附着到 head 骨上，"
                     "而程序化占位小白没有骨架（它那些 head/ear 是物体不是骨）")
        if base is None:
            sys.exit("--outfit 走的是参数化骨骼那条路径 ⇒ 带 --action-map 时没有 roles/base 可附着")
        if not args.outfit_objects:
            sys.exit("--outfit 必须给 --outfit-objects 白名单：这份帽文件里三块背景板全都带材质，"
                     "load_model 那套按有无材质筛会把它们一起烘进画面")
        wanted = [n.strip() for n in args.outfit_objects.split(",") if n.strip()]
        hat = load_outfit(args.outfit, wanted)
        # 必须在附着之前：attach / measure / render 三条路径读的是同一份几何，而细分档改的就是那份几何
        set_outfit_subsurf(hat, args.outfit_subsurf)
        outfit_ctx = attach_outfit(armature, roles, hat, meshes, args, base)
        check_attach(armature, roles, outfit_ctx, base, cam, cell, spec, pose, meshes)
        # check_attach 里跑过 idle 若干帧 ⇒ 贴地锁定把 armature.location.z 留在了最后一帧的补偿量上。
        # 测量必须从"什么都没动"开始，否则 rest 那一档的挂点读数会带上上一帧的抬升。
        armature.location.z = home_z
        reset_bones(armature, base)
        if args.outfit_measure_only:
            measure_outfit(args, cam, cell, spec, states, columns, pose, armature, roles, outfit_ctx, base, recorded)
            print("BAKE --outfit-measure-only：只量挂点，没渲染任何像素、没改 meta.json")
            return
        if args.outfit_only:
            outfit_ctx["carrier_hidden"] = hide_carrier(meshes)
            print(
                "BAKE --outfit-only：载体 %d 件已 hide_render（%s）⇒ 这批成片里只有配件，"
                "它是给客户端叠加用的**配件图集**，不是交付给眼睛看的宠物"
                % (len(meshes), ", ".join(outfit_ctx["carrier_hidden"]))
            )
        check_outfit_framing(cam, cell, columns, spec, states, pose, outfit_ctx)
    if not args.skip_facing_check:
        problems = check_facing(cam)
        if problems:
            sys.exit("朝向自检失败：\n  " + "\n  ".join(problems))
        print("BAKE 朝向自检通过（8 列的鼻尖屏幕侧与 Facing::index 一致）")

    if not args.skip_framing_check:
        problems, tiny, tight = check_framing(cam, cell, spec, states, columns, pose, samples)
        # 把留边余量也打出来，换正式模型时重定 ORTHO_SCALE 就靠这两个数
        print("BAKE 部件投影最小尺寸：" + " ".join("%s=%.1f" % (k, v) for k, v in sorted(tiny.items())))
        worst = sorted(tight.items(), key=lambda kv: kv[1])[:4]
        print("BAKE 格边留边最紧的零件：" + " ".join("%s=%.1fpx" % (k, v) for k, v in worst))
        if problems:
            sys.exit("构图自检失败：\n  " + "\n  ".join(problems))
        print("BAKE 构图自检通过（%d 个零件在全部姿态下都可见且没裁到格边）" % len(tiny))

    if lift:
        span = max(lift.values()) - min(lift.values())
        print(
            "BAKE 贴地锁定：把每帧最低点压回 z=0，补偿量 %.3f…%.3f 世界单位（帧间最大差 %.3f ⇒ 占 %dpx 格子的 %.1fpx）"
            % (min(lift.values()), max(lift.values()), span, cell, span / ORTHO_SCALE * cell)
        )

    if args.check_only:
        print("BAKE --check-only：自检跑完，不渲染")
        return

    place_camera(cam, columns[0])
    ax, ay = feet_anchor_px(cam, cell)
    print("BAKE anchor.feet = [%s, %s]（cell=%d, ortho_scale=%.2f, view_transform=%s）" % (ax, ay, cell, ORTHO_SCALE, used_transform))

    baked, skipped = 0, 0
    for state in states:
        track = spec["states"][state]
        frames = len(track["rows"])
        loop = track.get("loop", True)
        out_dir = os.path.join(args.out, state)
        os.makedirs(out_dir, exist_ok=True)
        for index in columns:
            place_camera(cam, index)
            for frame in range(frames):
                pose(state, frame, frames, loop)
                bpy.context.view_layer.update()
                path = os.path.join(out_dir, "f%02d_%02d.png" % (index, frame))
                if os.path.exists(path) and not args.force:
                    skipped += 1
                    continue
                scene.render.filepath = path
                bpy.ops.render.render(write_still=True)
                baked += 1
                print("BAKE %s" % path)
                if args.outfit and index == columns[0] and frame == 0:
                    # 每个状态的第一格都验一次"帽真的在像素里"（读贴图，不是几何推算）。
                    # `--outfit-only` 必须换一条闸门：那条量的判据是"帽让剪影变大了"，而这一批
                    # **本来就没有身体可变大** ⇒ 拿它去判只会两件事之一：载体没藏干净⇒照样绿，
                    # 或藏干净了⇒"没有变大"而报红。新闸门查的是"这一格只有帽、且帽在投影盒里"。
                    if args.outfit_only:
                        check_hat_only_in_pixels(path, cam, cell, outfit_ctx)
                    else:
                        check_hat_in_pixels(path, cam, cell, outfit_ctx, meshes)

    meta = {
        "cell": {"w": cell, "h": cell},
        "feet_px": [ax, ay],
        "ortho_scale": ORTHO_SCALE,
        # 测量轮要靠这几个数还原场景；上一版没记 target_z，只能按模型高重算（那次是个真缺口）
        "target_z": TARGET_Z,
        "cam_elev_deg": CAM_ELEV_DEG,
        "view_transform": used_transform,
        "blender_version": bpy.app.version_string,
        "model": args.model or "procedural:cat_shiro",
        "face_rot_z": args.face_rot_z,
        "action_map": parse_map(args.action_map, "--action-map") if args.action_map else {},
        "bones": (parse_map(args.bones, "--bones") if args.bones else {}),
        "bone_gain": args.bone_gain,
        "plant_feet": bool(args.plant_feet),
        "baked": baked,
        "skipped": skipped,
        "states": {s: len(spec["states"][s]["rows"]) for s in states},
        "columns": columns,
    }
    if args.outfit:
        # 像素里画了帽子的批次必须写明，否则以后没人知道这批图不是"裸宠"
        meta["outfit"] = args.outfit
        meta["outfit_objects"] = [n for n in (args.outfit_objects or "").split(",") if n.strip()]
        meta["outfit_scale"] = outfit_ctx["scale"]
        # 这四个都直接改像素，而`--outfit-scale`之外那三个原先没人记 ⇒ 一批"@192 带帽、倾 25°"的图
        # 在 meta.json 里读起来和"不倾"的那批一模一样。outfit.json 只有测量轮才写，像素批次
        # 的出处只有这一份 ⇒ 补在这里。
        for k in ("rot_z", "tilt", "seat"):
            meta["outfit_" + k] = outfit_ctx[k]
        meta["outfit_subsurf"] = args.outfit_subsurf
        # 这一批是"配件图集"还是"带帽的宠物"，只有这两个数分得开，而它俩渲出来的像素在肉眼看来
        # 是同一类东西（都有帽）。不记 ⇒ 以后没人知道这张 sheet 该单独叠还是直接上屏。
        # 藏了哪几件也一起记：那是"为什么这张图里没有身体"的唯一出处。
        meta["outfit_only"] = bool(args.outfit_only)
        if args.outfit_only:
            meta["outfit_carrier_hidden"] = outfit_ctx.get("carrier_hidden", [])
    with open(os.path.join(args.out, "meta.json"), "w", encoding="utf-8") as fh:
        json.dump(meta, fh, ensure_ascii=False, indent=2)
    print("BAKE 完成 baked=%d skipped=%d → %s" % (baked, skipped, os.path.join(args.out, "meta.json")))


def run():
    """`main()` 的外壳：Blender 会把脚本里未捕获的 Python 异常吞成**退出码 0**（本轮实测：
    `AttributeError: 'Matrix' object has no attribute 'length'` 打在日志里，外面收的却是
    EXIT=0，而烘焙一根帧都没出）。⇒ 在这个工具里"退出码 0"不能当成功读，得自己把异常翻成非零。
    """
    try:
        main()
    except SystemExit as exc:  # 各条门禁的 sys.exit(消息) 走这一支
        code = exc.code
        if isinstance(code, str):
            # 我们自己接住了 SystemExit ⇒ 解释器不会再替我们打这句话，必须补上
            sys.stderr.write(code + "\n")
        sys.stdout.flush()
        sys.stderr.flush()
        os._exit(1)
    except BaseException:
        import traceback

        traceback.print_exc()
        sys.stdout.flush()
        sys.stderr.flush()
        os._exit(1)


if __name__ == "__main__":
    # 探针脚本 import 本模块只为了复用 alpha_bbox/projected_points 这类函数 ⇒ 不能在导入时把整趟烘焙跑掉
    run()
