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
    if args.model:
        meshes = load_model(args.model)
        armature = next((o for o in bpy.data.objects if o.type == "ARMATURE"), None)
        height, pts, feet = normalize_model(meshes, args.face_rot_z)
        FEET = feet
        NOSE, REAR = auto_front_rear(pts, height)
        if args.nose:
            NOSE = parse_vec(args.nose)
        if args.rear:
            REAR = parse_vec(args.rear)
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
        print("BAKE 相机：ortho_scale=%.3f target_z=%.3f（模型高 %.3f，脸转 %g°）" % (ORTHO_SCALE, TARGET_Z, height, args.face_rot_z))
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

    meta = {
        "cell": {"w": cell, "h": cell},
        "feet_px": [ax, ay],
        "ortho_scale": ORTHO_SCALE,
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
    with open(os.path.join(args.out, "meta.json"), "w", encoding="utf-8") as fh:
        json.dump(meta, fh, ensure_ascii=False, indent=2)
    print("BAKE 完成 baked=%d skipped=%d → %s" % (baked, skipped, os.path.join(args.out, "meta.json")))


main()
