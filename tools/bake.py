"""在 Blender 内执行的序列帧烘焙：程序化占位小白 → 8 朝向 × N 帧 PNG。

用法（本机 headless 已验证）：
    blender -b --factory-startup --python tools/bake.py -- --state idle

状态行数与格子尺寸都读 `crates/petcore/assets/cat_shiro.pet.json`，不在本文件里复制
一份 0–39 行布局 —— 那份布局被 assets_scheduler.rs 的测试钉着，两处各写必然漂移。

材质注记：5.2 已移除 Shader-to-RGB（实测 `hasattr(bpy.types,'ShaderNodeToRGB')==False`），
所以卡通感走 Principled 平涂 + 定向光，不做色阶分档。正式建模时换 .blend 不改本脚本。
"""

import argparse
import json
import math
import os
import sys

import bpy
from bpy_extras.object_utils import world_to_camera_view
from mathutils import Vector

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


def check_framing(cam, cell, parts, spec, states, columns):
    """把两种静默故障变成响亮失败：零件被父级 scale 缩到看不见、姿态超出格子。

    这两种情况在像素里都只是"少了一块"，validate 数不透明像素是查不出来的（上一版的眼球
    实际只剩 0.0075 世界单位、耳内退化成一个点，全套门禁当时全绿）。这里不渲染，只做投影，
    所以每一帧姿态都能查。
    """
    tiny, clipped, tight = {}, {}, {}
    for state in states:
        frames = len(spec["states"][state]["rows"])
        loop = spec["states"][state].get("loop", True)
        for index in columns:
            place_camera(cam, index)
            for frame in range(frames):
                apply_pose(parts, state, frame, frames, loop)
                bpy.context.view_layer.update()
                for key, part in parts.items():
                    ob = part["ob"]
                    if not isinstance(ob.data, bpy.types.Mesh):
                        continue  # root 是 Empty
                    xs, ys = [], []
                    for corner in ob.bound_box:
                        co = world_to_camera_view(bpy.context.scene, cam, ob.matrix_world @ Vector(corner))
                        xs.append(co.x)
                        ys.append(co.y)
                    size = max((max(xs) - min(xs)) * cell, (max(ys) - min(ys)) * cell)
                    tiny[key] = min(tiny.get(key, 9e9), size)
                    # 留边 = 部件投影到最近格边的距离；负数表示已经裁掉
                    over = max(-min(xs), max(xs) - 1.0, -min(ys), max(ys) - 1.0)
                    clearance = (-over * cell) if over > 0.005 else min(
                        min(xs), 1.0 - max(xs), min(ys), 1.0 - max(ys)
                    ) * cell
                    if clearance < 1.0:
                        clipped[key] = min(clipped.get(key, 9e9), clearance)
                    tight[key] = min(tight.get(key, 9e9), clearance)
    problems = []
    for key, size in sorted(tiny.items()):
        if size < 2.0:
            problems.append("%s 整组姿态里投影最大只有 %.1f px ⇒ 基本看不见" % (key, size))
    for key, px in sorted(clipped.items()):
        problems.append("%s 离格边只剩 %.1f px（会被裁或已经裁掉）" % (key, px))
    return problems, tiny, tight


def feet_anchor_px(cam, cell):
    nx, ny = project(cam, FEET)
    return round(nx * cell, 3), round((1.0 - ny) * cell, 3)


def main():
    args = parse_args()
    spec = load_fixture()
    cell = args.cell or spec["grid"]["w"]
    if cell != spec["grid"]["h"]:
        sys.exit("fixture 的 grid.w/h 不相等（%s/%s），本脚本按方格写" % (spec["grid"]["w"], spec["grid"]["h"]))

    states = [s.strip() for s in args.state.split(",") if s.strip()]
    unknown = [s for s in states if s not in spec["states"]]
    if unknown:
        sys.exit("fixture 里没有这些状态: %s" % unknown)
    columns = [int(c) for c in args.facing.split(",") if c.strip() != ""]

    wipe_scene()
    parts = build_cat()

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

    if not args.skip_facing_check:
        problems = check_facing(cam)
        if problems:
            sys.exit("朝向自检失败：\n  " + "\n  ".join(problems))
        print("BAKE 朝向自检通过（8 列的鼻尖屏幕侧与 Facing::index 一致）")

    if not args.skip_framing_check:
        problems, tiny, tight = check_framing(cam, cell, parts, spec, states, columns)
        # 把留边余量也打出来，换正式模型时重定 ORTHO_SCALE 就靠这两个数
        print("BAKE 部件投影最小尺寸：" + " ".join("%s=%.1f" % (k, v) for k, v in sorted(tiny.items())))
        worst = sorted(tight.items(), key=lambda kv: kv[1])[:4]
        print("BAKE 格边留边最紧的零件：" + " ".join("%s=%.1fpx" % (k, v) for k, v in worst))
        if problems:
            sys.exit("构图自检失败：\n  " + "\n  ".join(problems))
        print("BAKE 构图自检通过（%d 个零件在全部姿态下都可见且没裁到格边）" % len(tiny))

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
                apply_pose(parts, state, frame, frames, loop)
                bpy.context.view_layer.update()
                path = os.path.join(out_dir, "f%02d_%02d.png" % (index, frame))
                if os.path.exists(path) and not args.force:
                    skipped += 1
                    continue
                scene.render.filepath = path
                bpy.ops.render.render(write_still=True)
                baked += 1
                print("BAKE %s" % path)

    meta = {
        "cell": {"w": cell, "h": cell},
        "feet_px": [ax, ay],
        "ortho_scale": ORTHO_SCALE,
        "cam_elev_deg": CAM_ELEV_DEG,
        "view_transform": used_transform,
        "blender_version": bpy.app.version_string,
        "baked": baked,
        "skipped": skipped,
        "states": {s: len(spec["states"][s]["rows"]) for s in states},
        "columns": columns,
    }
    with open(os.path.join(args.out, "meta.json"), "w", encoding="utf-8") as fh:
        json.dump(meta, fh, ensure_ascii=False, indent=2)
    print("BAKE 完成 baked=%d skipped=%d → %s" % (baked, skipped, os.path.join(args.out, "meta.json")))


main()
