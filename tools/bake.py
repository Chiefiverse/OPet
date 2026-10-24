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
ORTHO_SCALE = 1.45
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
        "cone": lambda: bpy.ops.mesh.primitive_cone_add(radius1=1.0, depth=2.0, vertices=20),
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


def build_cat():
    """脚底贴世界 z=0；body/head/tail/ears/legs 挂在 root Empty 上做纯 transform 动画。"""
    white = flat_material("shiro_body", (0.93, 0.93, 0.96))
    dark = flat_material("shiro_dark", (0.12, 0.12, 0.14))
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

    reg("body", add_part("body", "sphere", (0.0, 0.0, 0.40), (0.30, 0.26, 0.30), white, root), bob=0.030, squash=0.020)
    reg("head", add_part("head", "sphere", (0.06, 0.0, 0.80), (0.25, 0.23, 0.22), white, root), bob=0.045)
    reg("ear_l", add_part("ear_l", "cone", (0.14, 0.14, 1.00), (0.07, 0.07, 0.08), white, root), bob=0.050)
    reg("ear_r", add_part("ear_r", "cone", (0.14, -0.14, 1.00), (0.07, 0.07, 0.08), white, root), bob=0.050)
    reg("ear_l_in", add_part("ear_l_in", "cone", (0.15, 0.14, 0.99), (0.04, 0.04, 0.05), pink, parts["ear_l"]["ob"]))
    reg("ear_r_in", add_part("ear_r_in", "cone", (0.15, -0.14, 0.99), (0.04, 0.04, 0.05), pink, parts["ear_r"]["ob"]))
    reg("eye_l", add_part("eye_l", "sphere", (0.30, 0.10, 0.84), (0.030, 0.030, 0.042), dark, parts["head"]["ob"]))
    reg("eye_r", add_part("eye_r", "sphere", (0.30, -0.10, 0.84), (0.030, 0.030, 0.042), dark, parts["head"]["ob"]))
    reg("nose", add_part("nose", "sphere", (0.33, 0.0, 0.77), (0.028, 0.028, 0.022), pink, parts["head"]["ob"]))
    reg("tail", add_part("tail", "cyl", (-0.30, 0.0, 0.52), (0.045, 0.045, 0.22), white, root), sway=0.35, bob=0.015)

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


def apply_pose(parts, state, frame, frames):
    """参数化动作。循环状态第 frames 帧回到第 0 帧，所以用 f/frames 而非 f/(frames-1)。"""
    phase = frame / max(frames, 1)
    wave = math.sin(2.0 * math.pi * phase)
    for part in parts.values():
        ob = part["ob"]
        ob.location = part["loc"]
        ob.rotation_euler = part["rot"]
        ob.scale = part["scl"]
        if state == "idle":
            ob.location.z += part["bob"] * wave
            sy = 1.0 + part["squash"] * wave
            ob.scale = (part["scl"][0], part["scl"][1], part["scl"][2] * sy)
            if part["sway"]:
                ob.rotation_euler = (
                    part["rot"][0],
                    part["rot"][1] + part["sway"] * wave,
                    part["rot"][2],
                )
        elif state in ("walk", "run"):
            gain = 0.06 if state == "walk" else 0.10
            ob.location.z += part["bob"] * wave
            if part["sway"]:
                ob.rotation_euler = (part["rot"][0], part["rot"][1] + part["sway"] * wave, part["rot"][2])
            if part.get("swing_phase") is not None:
                ob.rotation_euler = (
                    0.0,
                    gain * math.sin(2.0 * math.pi * (phase + part["swing_phase"])),
                    0.0,
                )


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

    place_camera(cam, columns[0])
    ax, ay = feet_anchor_px(cam, cell)
    print("BAKE anchor.feet = [%s, %s]（cell=%d, ortho_scale=%.2f, view_transform=%s）" % (ax, ay, cell, ORTHO_SCALE, used_transform))

    baked, skipped = 0, 0
    for state in states:
        frames = len(spec["states"][state]["rows"])
        out_dir = os.path.join(args.out, state)
        os.makedirs(out_dir, exist_ok=True)
        for index in columns:
            place_camera(cam, index)
            for frame in range(frames):
                apply_pose(parts, state, frame, frames)
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
