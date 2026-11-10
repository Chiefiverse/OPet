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
import shutil
import subprocess
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

# 脚上那一档（--outfit-scale-ref foot_box）的参照：靴子的**整组水平足迹** = 该倍数 × 该侧腿在
# [鞋底, 踝] 这段里量出来的水平尺寸（脚趾→脚跟通常就是这段里最长的一维）。1.15 = "包住脚但不
# 多出一截"的一个初值，和 OUTFIT_SIZE_RATIO 一样**只能由眼验定档**，不是算出来的。
OUTFIT_FOOT_SIZE_RATIO = 1.15
# 鞋底带的厚度占配件自身 z 跨度的比例：自动挂点取这一段顶点的中心 ⇒ 拿到的是"鞋底中心"，
# 而不是整只靴子的包围盒中心（后者在靴筒中段，拿它当挂点会把整只靴子抬到踝以上）。
OUTFIT_SOLE_BAND = 0.15
# 手上那一档（--outfit-scale-ref bone，旗子 / 拳套）的参照物 = 以 attach-role 那根**手的骨根为球心**、
# 半径 OUTFIT_HAND_REGION × 载体高 的那块载体网格。
# ⚠️ 半径为什么挂在**载体高**上而不是骨长上（2026-11-09 现量，`scratch/boot_probe/hand_probe.log`）：
# bear 的 LeftHand 骨长 0.0207 而 RightHand 0.0992 —— 同一份 rig 的两只手骨差 4.8×，按"3×骨长"取半径
# 时左手那一档读到 **0 个点**（右手同档 1805 点）⇒ 半径随骨长漂 = 参照物在骨短的那一侧直接消失。
# 0.06 × 载体高在两支都落在手上：bear 0.109 ⇒ 218 点 / 水平跨 0.172；robot 0.111 ⇒ 615 点 / 0.212。
OUTFIT_HAND_REGION = 0.06
# 与 OUTFIT_FOOT_SIZE_RATIO 同族：一个"包住这只手但不多出一截"的初值，**只能由眼验定档**，不是算出来的。
OUTFIT_HAND_SIZE_RATIO = 1.15
# 附着自检第②条（"骨缩放有没有漏进配件"）的容差，单位是**相对量**不是 px（2026-11-09 定）。
# 两端都有现量：病 = 胸腔呼吸那档 ±3.75%；纯转动的噪声 = 外接半径逐帧 2×10⁻⁴%。
# 0.5% 落在中间（比病松 7.5 倍、比噪声紧 2.5 万倍），不是"先写一个数再找理由"。
OUTFIT_SCALE_LEAK_TOL = 0.005
# 镜像面的两条实测门禁：两根踝骨骨长之差 / 连线与载体左右轴之差。后者放宽到 35° 是因为
# **rig 的静止姿态本来就不是逐位对称的**（bear 实测两踝 x = +0.2816 / −0.2830 而 z 差 0.021），
# 拿"对称"当输入要求会让所有真实外部模型都过不了；这条判的是"这两根骨是不是一对左右脚"。
MIRROR_BONE_LEN_TOL = 0.02
# 骨长那条门禁的**绝对地板** = 该倍数 × 载体高（2026-11-09 被 robot 逼出来的）：它两手骨长
# 0.0382 / 0.0352 差 7.9%，可这两根骨只有 3.5 世界厘米 —— 差值 0.0030 = 载体高的 0.16%，@192
# 不到 1 px。原比例档是为踝骨（0.261 长）定的，照它判一根 0.035 的骨会把一份画面完全对称的用例
# 当场拒掉。地板只放宽"差的绝对量"，不放宽这条门禁的本意（"这对骨是不是左右一对"的真错形状是
# leg/foot、arm/hand 那种 3–5× 的差，任何一档都拦得住）。
MIRROR_BONE_LEN_FLOOR = 0.005
MIRROR_AXIS_DEG_MAX = 35.0

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
        "--facing-confirmed",
        metavar="判词",
        help="人眼过 2×2 出图后对朝向环的判词（例如 'col0=侧身鼻尖右 col2=正脸 col4=侧身鼻尖左 "
        "col6=背面'）。烘宠物本体时**必填**：朝向环的相位没有任何自动判据（两条实测都被否证，"
        "见 check_facing_render），这一句会进 meta.json 随产物一起留档。",
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
        "--outfit-color",
        default="",
        help="把配件的材质换成一块平涂漫反（0–255 的 R,G,B，例：232,214,170 = 米黄）；留空 = 原样保留源材质。"
        "存在的理由 = 深色配件（一顶近黑的礼帽）在屏幕上看不见，而这不是缩放能解决的事",
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
        "--outfit-lean",
        type=float,
        default=0.0,
        help="配件整体绕**载体的左右轴**俯仰多少度：正数 = 顶端朝**背离载体**那一侧仰出去"
        "（背挂件的「往外仰」就是这个数，而它与 --outfit-tilt 是两根不同的轴：tilt 转的是配件自己的"
        "坐标系、盘面相对背是斜的；lean 转的是「已经摆好的那一整件」相对载体，盘面仍正对背）。"
        "轴按载体骨架与鼻尖向量现量（与 --outfit-offset 同一套水平坐标系，**不是世界轴** ⇒ 本体带 "
        "--face-rot-z 90 时不会把前后读成左右）。默认 0 = 一字不变。",
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
        help="配件落座在参照物区间的哪一段：0 = 区间下缘（帽子=颈收缩处、靴子=鞋底、手=手区下缘）、"
        "1 = 上缘（颅顶 / 踝 / 手区上缘）；默认 %g" % OUTFIT_SEAT,
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
        "--outfit-attach-role",
        default="head",
        help="配件刚性附着到哪个**语义骨角色**（默认 head = 帽子）。脚上的配件写 foot_l，手上的写 hand_l。"
        "角色没对上当场点名退出，**不静默退回头部** ⇒ 那是'绑了个摆设'的同族",
    )
    p.add_argument(
        "--outfit-mirror-to",
        default="",
        help="第二个骨角色（如 foot_r / hand_r）：给了就把配件**再复制一份、镜像烘进网格数据**并绑到那根骨上，"
        "于是一份源网格穿在左右两只脚（或两只手）上。默认不给 = 单实例（帽子那一支逐字节不变）。"
        "镜像必须烘进数据而不是留在矩阵上：`bind_child_of` 的约束显式不吃缩放，而负缩放留在 "
        "matrix_world 会让后续每一条包围盒/投影读数读反（法向与绕序随后统一重算）",
    )
    p.add_argument(
        "--outfit-scale-ref",
        default="head_width",
        choices=["head_width", "foot_box", "hand_bone"],
        help="定标与落座的参照物：head_width = 看得见的头（head_region，帽子那一支的原路）；"
        "foot_box = 该侧腿在 [鞋底, 踝] 这一段自己的网格包围盒（头宽对靴子无意义）；"
        "hand_bone = 以那根手骨的骨根为球心、0.06 × 载体高 为半径的那块载体网格（旗杆、拳筒都落在这上面）。"
        "后两档同时把配件挂点的自动规则从'整组最矮那件的包围盒中心'换成'最低带中心'（鞋底中心 / 旗杆底、拳口）",
    )
    p.add_argument(
        "--outfit-offset",
        default="",
        metavar="前后,左右,上下",
        help="在载体自己的水平坐标系里把配件再平移这么多个**世界单位**（左右 = 骨架局部 X，"
        "不是世界 X —— 本体普遍带 --face-rot-z 90 烘）。上下沿世界 Z。"
        "给了镜像时这一位移只加在第一件的挂点上，镜像面会把左右分量自己翻号 ⇒ 写 0,+0.12,0 "
        "就是两只都朝**外**挪，不是两只朝同一个方向挪。"
        "默认不给 = 帽子那一支逐字节不变",
    )
    p.add_argument(
        "--outfit-scale-z",
        type=float,
        default=1.0,
        help="在 --outfit-scale 之外**只**沿配件自己的 Z 再乘这一倍（压靴筒用）。"
        "1.0 = 各向同性 = 帽子那一支逐字节不变。"
        "为什么要单独一个轴：靴子这份源文件筒高≈筒宽（1.77 vs 1.79），而载体是小腿粗、踝以下短的腿，"
        "'要矮'和'要细'在 --outfit-scale 上是同一个数 ⇒ 只用它压高度必然同时把筒径压到套不住腿"
        "（boot 首轮的 0.22/0.168 两支正是这样把'没包住'换成了'太高'的反面）。"
        "它写在 rot_z/tilt 之前作用于配件自身坐标 ⇒ 压的是'这只靴子自己的竖直方向'，不随 yaw 漂",
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
    p.add_argument(
        "--outfit-occlusion",
        action="store_true",
        default=False,
        help="每格多渲一趟『载体 + 品红自发光配件』，把被载体挡在身后的配件像素 alpha 抹掉 ⇒ 配件图集"
        "自己就带遮挡（远侧那只手套不再浮在身体前面）。默认关 = 逐字节沿用旧行为。\n"
        "为什么是『颜色当身份』而不是渲染通道：本机 Blender 5.2.1 上 pass 在 Python 侧**没有读出口**"
        "（实测 `scene.render.use_pass_object_index` 不存在、合成器 RenderLayers 节点只剩 Image/Alpha 两个"
        "插座、`CompositorNodeComposite` 已删、`Image.layers` 没了而 `Image.channels` 变成 int、"
        "`OPEN_EXR_MULTILAYER` 不在枚举里、引擎表只有 BLENDER_EEVEE ⇒ 连『物体纯色渲遮罩』的 Workbench 都没有）。"
        "代价 = 每格多一趟渲染；本体那批不重渲（它本来就是另一批产物）",
    )
    p.add_argument(
        "--outfit-occ-samples",
        type=int,
        default=1,
        help="遮罩那一趟的 TAA 采样数（默认 1 = 旧行为逐字节不变）。**1 采样对薄网格是有代价的**："
        "配件里那些只占一两个像素宽的边缘（羽毛、披风边）在遮罩图里落成'半覆盖'像素，"
        "`occ_front` 读到的既不是纯品红也不是纯载体，判据按非品红处理 ⇒ 成品里那些边缘像素被抹掉，"
        "看上去就是配件自己在丢像素。把它提到 4 左右让遮罩图的边缘收敛，代价只有那一趟变慢"
        "（成品那一趟仍用 `--samples`），不影响两趟的相机/姿态/尺寸一致性",
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


def evaluated_z_span(meshes, limit=3000):
    """载体网格评估后的世界 z 跨度 = "载体高"。

    不能用 `mesh_box`：那份读的是基网格 × matrix_world = **bind space**（head_region 的
    docstring 记了老虎 A-pose 摊腿把 X 撑到 2.313 的那件事），而这里要的绝对尺度要跟着
    画面上的身体走。手区半径与镜像骨长地板都读它，两处必须同一个来源。
    """
    zs = [p.z for ob in meshes for p in evaluated_points(ob, limit=limit)]
    return max(zs) - min(zs)


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
    # ⚠️ 白名单按**基名**比，与 :506 那处保持同一口径：在 Blender 界面里看到的往往是带重名后缀的
    # `mesh_uv.obj.001`（同场景已有同名网格时它就是这么显示/导出的），照抄进 --outfit-objects
    # 原本会一个都不匹配 ⇒ 全部丢弃 ⇒ 报"没装齐"。重数由下面 need/got 那一对把守，不会因此漏判。
    wanted = {_base_name(n) for n in names}
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
    # ⚠️ `wm.append` 把**父级**当依赖一起搬。白名单同时点名父与子（牛仔帽 `Cube,Cube.001`）时，
    # 追加子级那一步会把父级**再复制一份**（实测 2026-11-09：`Cube` + `Cube.001` + `Cube.002`，
    # 其中 `Cube.002` 与 `Cube` 网格基名相同、矩阵逐位相同、mesh.users=1 ⇒ 是副本不是链接）。
    # 像素看不出来（同材质同位置 ⇒ 重叠面渲成同一个面），但**每件重合件都把每格成本抬一档**
    # （帽檐 subsurf3+solidify 求值 9,794 顶点）而装载那行报的件数从此是假的 ⇒ 当场按
    # (网格基名, matrix_basis, dimensions) 去重，只留创建顺序里的第一件。
    # 判据用"逐位相同"而不是"看着差不多"：不重合的两件（例如同一网格的两个不同挂点）必须留下，
    # 那正是 `Torus`/`Torus.001` 这种写法要表达的东西。
    dupes, first_of = [], {}
    for ob in fresh:
        key = (
            _base_name(ob.data.name) if ob.type == "MESH" else "type:%s" % ob.type,
            tuple(round(v, 6) for row in ob.matrix_basis for v in row),
            tuple(round(d, 6) for d in ob.dimensions),
        )
        if key in first_of:
            dupes.append((ob, first_of[key]))
        else:
            first_of[key] = ob
    doomed = {ob.as_pointer(): src for ob, src in dupes}
    if doomed:
        # ⚠️ 顺序是硬要求：**先把要留的算出来，再删**。删掉之后那个 StructRNA 立即失效，
        # 之后任何一次对它求值（连 `ob not in doomed` 这种比较都算）都会炸 ReferenceError，
        # 而 Blender 会把脚本里未捕获的异常吞成退出码 0（见 `run()` 的文档）⇒ 一声不响地什么都不烘。
        keep_fresh = [ob for ob in fresh if ob.as_pointer() not in doomed]
        for ob in fresh:
            # 隐式父级正是子级此刻的 parent ⇒ 删之前先把子级的世界矩阵钉住，否则 parent 一变 None，
            # 子级会按自己的 local 矩阵跳位（而那正是"绑了但位置不对"的形状）。
            if ob.parent is not None and ob.parent.as_pointer() in doomed:
                mw = ob.matrix_world.copy()
                ob.parent = None
                ob.matrix_world = mw
        for ob, src in dupes:
            print(
                "BAKE 配件去重：删掉 %s（与 %s 的网格基名、matrix_basis、尺寸逐位相同 ⇒ append 子级时"
                "连带搬进来的父级副本，不是第二件配件）" % (ob.name, src.name)
            )
            bpy.data.objects.remove(ob, do_unlink=True)
        fresh = keep_fresh
    keep, drop = [], []
    for ob in fresh:
        (keep if ob.type == "MESH" and _base_name(ob.name) in wanted else drop).append(ob)
    # 报错要用的名字必须在**删之前**取：下面 drop 循环一 remove，`fresh` 里那些 StructRNA 立即失效，
    # 那句"这次实际装进来的是…"就会炸成 ReferenceError 而把真正的点名信息吃掉（:486 那条注释
    # 讲的是同一族事故，这一支当时没顾上）。
    arrived = sorted(ob.name for ob in fresh)
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
                " ".join(arrived) or "（什么都没装进来）",
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


def foot_region(meshes, ankle, side, axis, plane_pt):
    """脚上那一档的参照物 = **该侧腿自己的网格**，键名与 `head_region` 逐一对齐。

    为什么不能沿用头区：头宽对靴子毫无意义（bull 的头宽 1.317 里含两根角），而靴子要包的是
    那只脚。这里的"头宽"换成 [鞋底, 踝] 这段里该侧点的水平最大尺寸（脚趾→脚跟通常就是它），
    "颈收缩 z"换成鞋底 z，"颅顶"换成踝 z ⇒ `attach_outfit` 里那条
    `seat = pinch_z + outfit_seat × height` 一个字不用改，seat=0 落在鞋底、1 落在踝。
    ⚠️ 分侧靠**量的**正中面（`plane_pt` + 法向 `axis`），不是假设的 x=0：真实 rig 的静止姿态
    不是逐位对称的，把"左右对称"当输入要求会让每一份外部模型都过不了门禁。
    ⚠️ `axis` 必须是**载体的左右轴**而不是世界 X：本体烘出来普遍带 `--face-rot-z 90`，那时两踝
    只差在世界 Y 上、x 几乎相同 ⇒ 拿世界 x 分侧会把"左右"切成"前后"，量到的 0.889 是两只脚
    合起来的前后深度而不是单只脚的尺寸（bear 首轮实测就是这么错的）。水平尺寸同理要在
    (axis, axis×Z) 这套**随载体转**的坐标里取范围，世界 x/y 的包围盒在转过的载体上系统性偏大。
    """
    pts = [p for p in (q for ob in meshes for q in evaluated_points(ob, limit=6000))]
    sel = [p for p in pts if (p - plane_pt).dot(axis) * side > 0.0 and p.z <= ankle.z + 1e-6]
    if len(sel) < 50:
        sys.exit(
            "foot_region：过点 (%+.3f,%+.3f,%+.3f)、法向 (%+.3f,%+.3f,%+.3f) 那个正中面的 %s 侧、"
            "z ≤ 踝 %.4f 这段里只量到 %d 个顶点 ⇒ 这只脚在网格上没有自己的体积（分侧判据失效），"
            "定标参照不能要"
            % (*plane_pt, *axis, "左" if side > 0 else "右", ankle.z, len(sel))
        )
    fwd = axis.cross(Vector((0.0, 0.0, 1.0)))
    u = [(p - plane_pt).dot(axis) for p in sel]
    v = [(p - plane_pt).dot(fwd) for p in sel]
    zs = [p.z for p in sel]
    width = max(max(u) - min(u), max(v) - min(v))
    height = ankle.z - min(zs)
    if width <= 0 or height <= 0:
        sys.exit("foot_region：该侧脚的水平尺寸 %.3f 或鞋底→踝高度 %.3f 非正，量不出靴子该多大" % (width, height))
    region = {
        "crown_z": ankle.z,
        "pinch_z": min(zs),
        "width": width,
        "height": height,
        "center": Vector((sum(p.x for p in sel) / len(sel), sum(p.y for p in sel) / len(sel), min(zs))),
        "band_points": len(sel),
        "extent": (min(u), max(u), min(v), max(v), min(zs), max(zs)),
    }
    print(
        "BAKE 脚区（网格几何，载体左右轴 (%+.3f,%+.3f,%+.3f) 的 %s 侧）："
        "鞋底 z=%.3f 踝 z=%.3f 脚高 %.3f 脚尺寸 %.3f 中心 (%+.3f,%+.3f) 用了 %d 个顶点"
        % (
            *axis,
            "左" if side > 0 else "右",
            region["pinch_z"],
            region["crown_z"],
            region["height"],
            region["width"],
            region["center"].x,
            region["center"].y,
            region["band_points"],
        )
    )
    return region


def hand_region(meshes, center, radius, axis):
    """手上那一档（旗子 / 拳套）的参照物 = **以手的骨根为球心那块载体网格**，键名与 `head_region` 逐一对齐。

    为什么不沿用头区或脚区：头宽对手上挂件无意义，而脚区那条"z ≤ 踝"的带在手上是空的 —— 手在
    身体侧面而不是底下。这里的"颈收缩 z"换成**球区最低 z**、"颅顶"换成球区最高 z ⇒
    `attach_outfit` 里那条 `seat = pinch_z + outfit_seat × height` 一个字不用改，seat=0 落在手的
    下缘、1 落在上缘。水平尺寸在 (axis, axis×Z) 这套**随载体转**的坐标里取，理由同 `foot_region`。
    ⚠️ 半径按**载体高**算而不是按骨长（`OUTFIT_HAND_REGION` 那里记了 bear 两只手骨差 4.8× 的实测）：
    骨长一档，短的那侧会读到 0 个点，参照物直接消失。
    """
    r2 = radius * radius
    sel = []
    for ob in meshes:
        for p in evaluated_points(ob, limit=6000):
            if (p - center).length_squared <= r2:
                sel.append(p)
    if len(sel) < 50:
        sys.exit(
            "hand_region：以 (%+.3f,%+.3f,%+.3f) 为球心、半径 %.4f（= %.0f%% × 载体高）那个球里只量到 "
            "%d 个顶点 ⇒ 这只手在网格上没有自己的体积（多半是载体不是四足/双足 rigs，或半径档不对），"
            "定标参照不能要" % (*center, radius, 100 * OUTFIT_HAND_REGION, len(sel))
        )
    fwd = axis.cross(Vector((0.0, 0.0, 1.0)))
    u = [(p - center).dot(axis) for p in sel]
    v = [(p - center).dot(fwd) for p in sel]
    zs = [p.z for p in sel]
    width = max(max(u) - min(u), max(v) - min(v))
    height = max(zs) - min(zs)
    if width <= 0 or height <= 0:
        sys.exit("hand_region：该手区水平尺寸 %.3f 或竖直跨度 %.3f 非正，量不出手上配件该多大" % (width, height))
    region = {
        "crown_z": max(zs),
        "pinch_z": min(zs),
        "width": width,
        "height": height,
        "center": Vector((sum(p.x for p in sel) / len(sel), sum(p.y for p in sel) / len(sel), min(zs))),
        "band_points": len(sel),
        "radius": radius,
        "extent": (min(u), max(u), min(v), max(v), min(zs), max(zs)),
    }
    print(
        "BAKE 手区（网格几何，球心 (%+.3f,%+.3f,%+.3f) 半径 %.4f，载体左右轴 (%+.3f,%+.3f,%+.3f)）："
        "下缘 z=%.3f 上缘 z=%.3f 手区高 %.3f 手区尺寸 %.3f 中心 (%+.3f,%+.3f) 用了 %d 个顶点"
        % (
            *center,
            radius,
            *axis,
            region["pinch_z"],
            region["crown_z"],
            region["height"],
            region["width"],
            region["center"].x,
            region["center"].y,
            region["band_points"],
        )
    )
    return region


def outfit_sole_point(hat, override, band_label="鞋底带"):
    """靴子的挂点取**鞋底带中心**，不是整只靴子的包围盒中心。

    `outfit_anchor_point` 那条自动规则（整组最矮那件的包围盒中心）对帽子是对的 —— 帽圈本来就
    是帽子的最低件、它的中心就是要落座的那一圈。靴子只有一件，同一条规则会落在靴筒中段 ⇒
    seat=0 把靴筒中段按在鞋底上，整只靴子飞起来。取最低 OUTFIT_SOLE_BAND 这段顶点的中心，
    拿到的就是"鞋底中心"。认几何不认名字，和帽圈那条同一族。

    ⚠️ 同一条规则在手上那一档读出来的是**握点**，而且这是量出来的不是借用的（2026-11-09 现量）：
    flag.glb 的 z 跨 0..1.081，最低 15% 那段（z ≤ 0.162）里只有旗杆的 7,603 个点、旗面一个都没有
    ⇒ 中心 = 旗杆底 = 该握的地方；boxing.glb 的 z 跨 −0.9..0.9，最低那段是拳口（X 跨 0.770，
    往上到拳部 1.241）⇒ 中心 = 口 = 手该套进去的地方。两档都落在正确的点上，所以不再加第三个
    挂点旋钮。`band_label` 只改措辞不改算法：那条打印是"这批像素的挂点从哪块几何来"的唯一出处。
    """
    if override:
        return parse_vec(override), None
    lowest = min(hat, key=lambda ob: mesh_box([ob])[4])
    x0, x1, y0, y1, z0, z1 = mesh_box([lowest])
    top = z0 + OUTFIT_SOLE_BAND * (z1 - z0)
    band = [p for p in rigid_points(lowest, limit=6000) if p.z <= top]
    if not band:
        sys.exit("outfit_sole_point：%s 在最低 %.1f%% 那段（z ≤ %.4f）里一个顶点都没有" % (lowest.name, 100 * OUTFIT_SOLE_BAND, top))
    point = Vector(
        (
            sum(p.x for p in band) / len(band),
            sum(p.y for p in band) / len(band),
            sum(p.z for p in band) / len(band),
        )
    )
    print(
        "BAKE 配件挂点自动取 %s 的**%s**中心 (%+.3f,%+.3f,%+.3f)（带 = z ≤ %.4f，即最低 %.0f%% 那 %d 点；"
        "整件 z %.4f..%.4f）"
        % (lowest.name, band_label, point.x, point.y, point.z, top, 100 * OUTFIT_SOLE_BAND, len(band), z0, z1)
    )
    return point, lowest.name


def mirror_frame(armature, roles, a_role, b_role, height):
    """量出"把 a 骨根映到 b 骨根"的那个镜面，返回 (4x4 反射矩阵, 中点, 法向)。

    两点定一个镜面是**恒成立的**（法向 = 连线方向，过中点），所以这里没有"猜平面"的余地；
    真正要验的是**这两根骨是不是一对左右骨**，三条实测：
      ① 骨长相等（差过门禁 ⇒ 那不是左右对称的两根骨，镜像出来必然一边大一边小）。门禁是
         `max(比例档, MIRROR_BONE_LEN_FLOOR × 载体高)`：纯比例档是为踝骨（0.261 长）定的，照它
         判 robot 那对手骨（0.0382 / 0.0352，差 7.9% 但绝对差只有载体高的 0.16% = @192 不到 1 px）
         会把一份画面完全对称的用例当场拒掉。地板只放宽绝对量，拦不住的真错形状仍是
         leg/foot、arm/hand 那种 3–5× 的差。
      ② 连线与载体的左右轴夹角（**折到 0–90°**：镜面法向是无向线）< MIRROR_AXIS_DEG_MAX（连线要是接近竖直，说明这两根骨是上下串
         的，比如 leg 和 foot，把它们当左右一对镜像 = 一只配件贴在腿中间）；
      ③ 两骨都存在（缺哪个就点名哪个，不许退回头部那一支）。
    ⚠️ **镜面取载体的矢状面（过两踝中点、法向 = 左右轴在水平面上的投影），不取两踝连线的垂直平分面**
    （horse 首轮实测暴露的那件事）：垂直平分面的法向**继承 rig 自己的不对称**。马那份两踝 z 差 0.0407、
    连线与左右轴夹 10.89° ⇒ 那样定出来的镜面从竖直歪 9.0°，镜像那只靴子跟着歪：靴口相对腿偏到 −0.212、
    鞋底沉进地面下 0.123。歪掉的不是"对称"而是"竖"——而靴子的内侧面在真实载体上本来就应当竖直。
    矢状面把 rig 的不对称**只当读数**（下面那行照打两踝的 前后 / z 差），不参与定平面 ⇒ 两只靴子等高、
    都竖直；代价是"镜像后的骨根"与真实那根骨差开 rig 自己的那点不对称，那是美术改源 rig 的事，不是这里
    悄悄把镜面斜过去迁就它的事（与上面①②两条门禁同一个理由）。
    """
    for role in (a_role, b_role):
        if role not in roles:
            sys.exit(
                "--outfit-mirror-to 要 %s 和 %s 两个骨角色，模型里解出来的是：%s ⇒ 用 --bones 指认"
                % (a_role, b_role, " ".join(sorted(roles)) or "（一个都没有）")
            )
    pa = armature.pose.bones[roles[a_role]]
    pb = armature.pose.bones[roles[b_role]]
    la = (pa.tail - pa.head).length
    lb = (pb.tail - pb.head).length
    tol_len = max(MIRROR_BONE_LEN_TOL * max(la, lb), MIRROR_BONE_LEN_FLOOR * height)
    if la <= 0 or lb <= 0 or abs(la - lb) > tol_len:
        sys.exit(
            "%s 骨长 %.5f 与 %s 骨长 %.5f 差过 %.5f（= max(%.0f%% × 骨长, %.1f%% × 载体高 %.3f)）⇒ 这不是一对"
            "左右对称的骨，镜像出来的两只必然一只大一只小，不能要"
            % (
                roles[a_role],
                la,
                roles[b_role],
                lb,
                tol_len,
                100 * MIRROR_BONE_LEN_TOL,
                100 * MIRROR_BONE_LEN_FLOOR,
                height,
            )
        )
    a = armature.matrix_world @ pa.head
    b = armature.matrix_world @ pb.head
    ln = b - a
    if ln.length < 1e-6:
        sys.exit("%s 与 %s 的骨根重合（%s）⇒ 定不出镜面，两只脚是同一根骨？" % (roles[a_role], roles[b_role], tuple(a)))
    ln = ln.normalized()
    # 载体的左右轴 = 骨架世界矩阵的第一列（局部 X），**不是**世界 X：本体普遍带 --face-rot-z 90 烘，
    # 那时左右脚在世界上差 Y 而 x 几乎相同。bear 首轮实测：连线与局部 X 折后夹角 **2.22°**（过），
    # 而那份的镜面法向读出来是 (+0.012,−0.999,−0.037) ≈ 世界 −Y —— 两个数同时成立才叫对上。
    # 折叠是必须的：镜面是**无向**的（法向反号是同一个镜面），不折就会把这条能力唯一的目标用例
    # （bear，探针里读到的原值是 177.78°）当场拒掉。
    axis_x = armature.matrix_world.to_3x3() @ Vector((1, 0, 0))
    if axis_x.length < 1e-6:
        sys.exit("载体骨架的局部 X 是零向量 ⇒ 定不出它的左右轴，也就定不出镜面")
    axis_x.normalize()
    deg = math.degrees(ln.angle(axis_x, math.pi))
    # `abs` 不是多余的保险：`Vector.angle` 的第二个参数是**反平行时的 fallback**，传 π 就会得到
    # 180.0000…，`min(180+ε, 180-(180+ε))` 于是给一个**负的**读数（打印出来像"夹角 -0.00°"）。
    deg = min(deg, abs(180.0 - deg))
    if deg > MIRROR_AXIS_DEG_MAX:
        sys.exit(
            "两骨根连线 (%+.4f,%+.4f,%+.4f) 与载体左右轴夹角 %.1f° > %.1f° ⇒ 这两根骨不是左右一对"
            "（接近竖直 = 上下串的两根，比如 leg/foot），镜像会把一只靴子贴进腿中间"
            % (*ln, deg, MIRROR_AXIS_DEG_MAX)
        )
    mid = (a + b) / 2
    # 镜面 = 矢状面：法向 = 左右轴**投到水平面**再归一（去掉它自己的 z 分量，也去掉连线的 z 分量），
    # 符号取向让 `refl @ a` 落在 b 那一侧（反射是无向的，但下面那条镜像残差门禁要一个确定方向）。
    n = Vector((axis_x.x, axis_x.y, 0.0))
    if n.length < 1e-6:
        sys.exit(
            "载体左右轴 (%+.4f,%+.4f,%+.4f) 指着正上/正下 ⇒ 这份 rig 没有水平意义上的左右，"
            "矢状面定不出来；换一对真正左右对称的骨角色（--outfit-attach-role / --outfit-mirror-to）"
            % tuple(axis_x)
        )
    n.normalize()
    if n.dot(ln) < 0.0:
        n = -n
    r3 = Matrix.Identity(3)
    for i in range(3):
        for j in range(3):
            r3[i][j] -= 2.0 * n[i] * n[j]
    refl = Matrix.Translation(mid) @ r3.to_4x4() @ Matrix.Translation(-mid)
    # rig 自身不对称的三项读数：沿左右轴的差（对称面这一维）、前后差、上下差。后两项**不进镜面**，
    # 只打印，因为"两只靴子一高一低"如果显眼是源 rig 的事。
    fw = Vector((NOSE[0] - REAR[0], NOSE[1] - REAR[1], 0.0))
    d = b - a
    print(
        "BAKE 镜像面（实测）：矢状面法向 (%+.5f,%+.5f,%+.5f) 过中点 (%+.4f,%+.4f,%+.4f)，"
        "与水平面夹角 %.4f°（按构造恒 0）；两骨骨长均为 %.5f、连线与左右轴夹角 %.2f°（门禁 ≤%.0f°）；"
        "rig 自身不对称 左右=%.5f 前后=%.5f 上下=%.5f ⇒ %s"
        % (
            *n,
            *mid,
            math.degrees(n.angle(Vector((0, 0, 1)))) - 90.0,
            la,
            deg,
            MIRROR_AXIS_DEG_MAX,
            abs(d.dot(n)),
            abs(d.dot(fw.normalized())) if fw.length > 1e-6 else 0.0,
            abs(a.z - b.z),
            "照它烘，只登记不判红"
            if max(abs(d.dot(n)), abs(a.z - b.z)) > 1e-4
            else "两踝在矢状面两侧等高",
        )
    )
    return refl, mid, n


def mirror_objects(hat, placed, refl, suffix=".mirror"):
    """复制一份配件网格，把**反射烘进顶点数据**（不是留在物体矩阵上）。

    为什么必须烘进数据：`bind_child_of` 的约束显式 `use_scale_*=False`，带不动负缩放；而把
    det=-1 留在 matrix_world 上，之后每一条 `mesh_box` / `rigid_points` / `world_to_camera_view`
    读数读的都是反的（构图与像素门禁会一起变成摆设）。
    数据侧的变换 = `M⁻¹ @ refl @ M`（M 是该件摆放后的世界矩阵），这样"物体矩阵不变、顶点动"
    之后世界位置正好等于 refl 作用在原顶点上。
    反射是负定向 ⇒ 顶点搬完之后**绕序推出的法向正好是外法向的反**：`(A x)×(A y) = det(A)·A⁻ᵀ(x×y)`，
    而反射的 det(A) = −1。所以处置是**无条件 `reverse_faces`**（它同时改绕序和法向），再由下面那条
    逐面门禁对着**解析镜像值**验 —— 期望值在数据空间算，理由见下面那段注释。
    ⚠️ 这里原来写的是"先 `recalc_face_normals` 再按多数投票决定翻不翻"，**已被 boot.glb 实测否证**：
    那份 500,000 面的网格 recalc 后只有 2336/4000 面与解析值同号（不是一张可定向的封闭壳，逐岛各挑
    朝向），"多数符号"本身就不可选；而它翻完仍然逐面 180° 红，因为比较用错了空间（世界 vs 数据）。
    """
    import bmesh

    dupes = []
    for ob in hat:
        me = ob.data.copy()
        me.name = ob.data.name + suffix
        dup = bpy.data.objects.new(ob.name + suffix, me)
        dup.matrix_world = ob.matrix_world.copy()
        dup.visible_camera = ob.visible_camera
        # 修改器挂在**物体**上而不是网格上 ⇒ 只 copy data 会让镜像件丢掉 SUBSURF/SOLIDIFY，
        # 渲出来一只光滑一只带帽檐细分（boot 那份没修改器所以看不见这个坑，帽子那份一旦镜像就会中）
        for md in ob.modifiers:
            dup.modifiers.append(md.copy())
        for col in ob.users_collection:
            col.objects.link(dup)
        m = ob.matrix_world
        a = m.inverted() @ refl @ m
        # 顶点：直接按索引搬（复制件与原件顶点序逐位相同 ⇒ 残差门禁能一一对应）
        for v in me.vertices:
            v.co = a @ v.co
        # ⚠️ 期望法向必须在**数据空间**里算：me.polygons[i].normal 读的是 dup 的**局部**法向，而
        # refl 是**世界**反射 ⇒ 正确的对照是共轭过的那一个。直接拿 refl.to_3x3() 乘局部法向 =
        # 拿世界的方向比局部的方向：bear 首轮（物体带 --outfit-rot-z 180）实测整体差 176–180°，
        # 看上去像"绕序没翻对"，其实是这里比错了空间 —— 一条会把自己那条修复判成失败的假红。
        # ⚠️ 但**法向和顶点不是同一个矩阵**：共轭一个对合反射仍是对合（a⁻¹ = a），而法向按逆转置走，
        # 所以法向用的是 aᵀ = (a⁻¹)ᵀ。各向同性缩放时 a 正交 ⇒ aᵀ = a⁻¹ = a，两者逐位相同（帽子那一支、
        # boot 首轮 scale_z=1 都在这格上，所以旧代码是对的）；一旦 --outfit-scale-z≠1，a 不再正交，
        # 用 a 会让这条门禁自己红：boot@sz0.72 实测最大夹角 2.930°（门禁 2°）而渲出来其实没里外翻。
        lin = a.to_3x3().transposed()
        want_normals = [(lin @ p.normal).normalized() for p in ob.data.polygons]
        if me.has_custom_normals:
            # 有拆面法向（custom split normals）时 polygon.normal 不跟绕序走 ⇒ 翻绕序等于什么都没改，
            # 而下面的门禁照样会绿。boot.glb 实测无 NORMAL 属性，所以这一支现在是"能红"而不是"要修"。
            sys.exit(
                "镜像件 %s 带拆面法向 ⇒ 绕序改了而法向不会跟着改，这条能力在这里会静默失效。"
                "先烘掉 custom normals（或在源文件里去掉法向）再来" % dup.name
            )
        bm = bmesh.new()
        bm.from_mesh(me)
        faces = bm.faces[:]
        if len(faces) != len(want_normals):
            sys.exit(
                "镜像件 %s 的面数 %d ≠ 原件 %d ⇒ 复制不是逐面搬的，法向门禁失去对应关系"
                % (dup.name, len(faces), len(want_normals))
            )
        # 下面逐面对应关系靠"bmesh 读回来的面序 == 网格多边形序" ⇒ 先量它，不假设它
        stride = max(1, len(faces) // 4000)
        sample = list(range(0, len(faces), stride))
        for i in sample:
            if {v.index for v in faces[i].verts} != set(ob.data.polygons[i].vertices):
                sys.exit(
                    "镜像件 %s 第 %d 面的顶点与原件对不上（%s vs %s）⇒ 面序对应关系不成立，"
                    "法向门禁读的不是同一批面"
                    % (dup.name, i, sorted(v.index for v in faces[i].verts), ob.data.polygons[i].vertices)
                )
        # **无条件**翻绕序，不投票：反射 det=−1 ⇒ 同一索引序从外面看由逆时针变成顺时针，
        # 而 `polygon.normal` 就是从绕序导出的 ⇒ 翻一次正好补回来。
        # 原先这里先 `recalc_face_normals` 再按多数投票决定翻不翻，实测被 boot 否证：那份网格
        # recalc 后只有 2336/4000 面与解析值同号（它不是一张可定向的封闭壳，逐岛各挑朝向），
        # 于是"多数符号"这个开关本身就不可靠，而它放行后逐面门禁仍然 180° 红。
        bmesh.ops.reverse_faces(bm, faces=faces)
        bm.to_mesh(me)
        bm.free()
        me.update()
        # polygon.normal 是从绕序**惰性导出**的量 ⇒ 不点名更新一次就会读到 to_mesh 之前的旧值，
        # 而下面那条门禁读旧值时照样能给出一个"看起来很稳"的数（0° 或 180° 都可能是缓存）。
        bpy.context.view_layer.update()
        worst = max(
            (me.polygons[i].normal.angle(want_normals[i], math.pi) for i in range(len(me.polygons))),
            default=0.0,
        )
        print(
            "BAKE 镜像法向：翻绕序后与解析镜像法向的最大夹角 %.3f°（面数 %d）⇒ %s"
            % (
                math.degrees(worst),
                len(me.polygons),
                "绕序/法向烘对了"
                if worst < math.radians(2.0)
                else "**法向没翻对**（这一份渲出来是里外翻的）",
            )
        )
        if worst >= math.radians(2.0):
            sys.exit(
                "镜像件 %s 的法向与解析镜像值差 %.3f°（门禁 2°）⇒ 这一份渲出来是里外翻的，"
                "不许上屏" % (dup.name, math.degrees(worst))
            )
        # ⚠️ 镜像件的**世界矩阵不是镜像过的矩阵**，而是和原件同一个矩阵：反射被烘进顶点数据了，
        # `dup.matrix_world` 从头到尾没动过 ⇒ `refl @ placed[ob]` 是一个"物体矩阵镜像、数据也镜像"
        # = 镜像两次的矩阵，bind_child_of 按它去反解 inverse 必然对不上（boot 首轮实测残差 0.584，
        # 而容差 0.0005 ⇒ 两种 inverse 配方都红）。真正镜像的是**挂点**（下面 instances[1] 的
        # anchor_local 用 `refl @ seat`），而"同一个矩阵 × 镜像的数据"给出的世界顶点正好 = refl 作用
        # 在原顶点上 —— 那才是这条能力的定义。
        placed[dup.name] = placed[ob.name].copy()
        dupes.append(dup)
    return dupes


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


def slot_labels(scale_ref):
    """三档参照各自的一族措辞，**只在这一处**分叉。

    为什么值得一个函数：这几行标签原先是 5 个 `head_width ? : foot_box` 的二元三目，加第三档
    时它们会**静默把"手"读成"脚"**（"脚尺寸/鞋底 z/靴筒"出现在一张旗子的日志里，而日志是
    "这批像素怎么定出来的"唯一出处）。加一档就改一处的形状，才不会被下一个人漏掉。
    """
    if scale_ref == "foot_box":
        return "脚尺寸", "鞋底 z", "鞋底", "脚高", "网格脚区", "别把它读成靴筒高度倍数"
    if scale_ref == "hand_bone":
        return "手区尺寸", "手区下缘 z", "握持", "手区高", "网格手区", "别把它读成旗面高度倍数"
    return "头宽", "颈收缩 z", "帽圈", "头高", "网格头区", "别把它读成帽檐倍数"


def attach_outfit(armature, roles, hat, meshes, args, base):
    """把配件定标、刚性对齐到**看得见的头**、上 CHILD_OF 约束。返回挂点读数需要的上下文。

    附着用 CHILD_OF 而不是 `parent_type='BONE'`：后者挂的是骨**尾**且用骨局部轴，实测会把
    --outfit-rot-z 变成世界俯仰而不是偏航；而 `bpy.ops.object.parent_set(keep_transform=True)`
    在无 GUI 下返回 FINISHED 却**静默不做事**。
    `use_scale_*=False`：帽不许随胸腔呼吸涨缩 —— 这是 inherit_scale=FULL 那条坑的第三次现身。
    inverse_matrix 一律由 bind_child_of 实测反解，然后**读评估后的矩阵**验一次摆放没被约束改动。
    """
    role = args.outfit_attach_role
    if role not in roles:
        sys.exit(
            "--outfit-attach-role %r 在这份模型里没解出来 ⇒ 可用角色：%s。"
            "**不许退回头部那一支**：那会把靴子戴在头上，而六道门禁都会绿"
            % (role, " ".join(sorted(roles)) or "（一个都没有）")
        )
    mirror_role = args.outfit_mirror_to
    if mirror_role and mirror_role == role:
        sys.exit("--outfit-mirror-to 不能和 --outfit-attach-role 是同一个角色 %r ⇒ 镜像出来的第二件"
                 "绑回同一根骨，两只脚会穿同一只靴子（正是这条能力要防的形状）" % role)
    if mirror_role and mirror_role not in roles:
        sys.exit(
            "--outfit-mirror-to %r 在这份模型里没解出来 ⇒ 可用角色：%s"
            % (mirror_role, " ".join(sorted(roles)))
        )
    head = roles[role]
    reset_bones(armature, base)  # 定标与摆放必须在 rest 上做，否则跟着呼吸那一档一起偏
    pb = armature.pose.bones[head]
    bone_len = (pb.tail - pb.head).length
    bone_world = armature.matrix_world @ pb.head
    ref_side = ref_axis = ref_plane = ref_center = None
    # 载体左右轴 = 骨架世界矩阵的第一列（局部 X）：本体普遍带 --face-rot-z 90 烘，
    # 那时"左右"落在世界 Y 上，拿世界 x 分侧会把左右切成前后（bear 首轮实测的错法）。
    left_axis = armature.matrix_world.to_3x3() @ Vector((1.0, 0.0, 0.0))
    if left_axis.length < 1e-6:
        sys.exit("载体骨架的局部 X 是零向量 ⇒ 定不出它的左右轴")
    left_axis.normalize()
    hand_radius = None
    if args.outfit_scale_ref == "foot_box":
        # 参照物换成该侧腿自己的网格；正中面由**两根踝骨**量出，法向是载体的左右轴而不是世界 X
        if mirror_role:
            # 正中面由**两根踝骨**量出：过其中点、法向 = 两踝连线（那条无向线就是左右轴）
            other = armature.matrix_world @ armature.pose.bones[roles[mirror_role]].head
            plane_pt = (bone_world + other) / 2.0
            line = other - bone_world
            if line.length < 1e-6:
                sys.exit("--outfit-mirror-to %s：两根踝骨重合（连线长 %.3e）⇒ 量不出左右轴，"
                         "分侧与镜像面都不能要" % (mirror_role, line.length))
            axis = line.normalized()
            # 定个符号：让 axis 指向载体局部 +X 那侧，于是下面打印的"左"= 局部 X 侧，
            # 不会因"--outfit-attach-role 给的是哪只脚"而翻转含义（mirror_frame 那条门禁
            # 已经验过这根轴与两踝连线是同一根无向线）
            if axis.dot(left_axis) < 0.0:
                axis = -axis
        else:
            plane_pt = armature.matrix_world @ Vector((0.0, 0.0, 0.0))
            axis = left_axis
        side = 1.0 if (bone_world - plane_pt).dot(axis) >= 0.0 else -1.0
        ref_side, ref_axis, ref_plane = side, axis, plane_pt
        region = foot_region(meshes, bone_world, side, axis, plane_pt)
    elif args.outfit_scale_ref == "hand_bone":
        # 参照物 = 手上那圈载体网格（球心 = attach-role 那根手的骨根）。这里**不**按正中面分侧：
        # 球半径只有载体高的 6%，两只手离得比它远得多（bear 实测 0.109 半径 / 218 点，另一只手
        # 一个点都不落进来）⇒ 分侧只会多一个会失败的判据，换不到任何东西。
        body_h = evaluated_z_span(meshes)
        hand_radius = OUTFIT_HAND_REGION * body_h
        region = hand_region(meshes, bone_world, hand_radius, left_axis)
        ref_axis = left_axis
        ref_center = bone_world
    else:
        region = head_region(
            meshes,
            (bone_len, (armature.matrix_world @ pb.head).z, (armature.matrix_world @ pb.tail).z),
        )
    anchor0, anchor_from = (
        outfit_sole_point(hat, args.outfit_anchor, band_label=slot_labels(args.outfit_scale_ref)[2] + "带")
        if args.outfit_scale_ref in ("foot_box", "hand_bone")
        else outfit_anchor_point(hat, args.outfit_anchor)
    )
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
    # 三个参照各有自己的比例常数：头宽对靴子无意义，小腿宽对手上挂件同样无意义 ⇒ 不是一条公式两个系数。
    size_ratio = {
        "foot_box": OUTFIT_FOOT_SIZE_RATIO,
        "hand_bone": OUTFIT_HAND_SIZE_RATIO,
    }.get(args.outfit_scale_ref, OUTFIT_SIZE_RATIO)
    scale = args.outfit_scale if args.outfit_scale > 0 else round(size_ratio * region["width"] / width, 4)
    # 量级门禁 ③ 的期望必须跟着"sizes 取的是投影 x/y 的较大者"这句话走：**细长**配件（旗杆、靴筒）
    # 占优的那一维是自身高度而不是水平足迹。相机俯 CAM_ELEV_DEG=15° ⇒ 屏幕 y 轴与世界 Z 夹 15°，
    # 一根竖直杆在屏幕上的长度 = 杆长 × cos15°（flag × bear 实测：杆 0.361 世界 × 0.9659 = 0.3487
    # ÷ ortho 2.640 × 192 = 25.3px，与那一行的读数逐位对上）。
    # 帽子那一支足迹 ≥ 身高 ⇒ max 取的是足迹 ⇒ 期望与旧值**逐字节相同**：这条修正只对细长形状生效。
    tall_world = (z1 - z0) * scale * args.outfit_scale_z * math.cos(math.radians(CAM_ELEV_DEG))
    seat = Vector((region["center"].x, region["center"].y, region["pinch_z"] + args.outfit_seat * region["height"]))
    offset = Vector((0.0, 0.0, 0.0))
    lean_axis = None
    if args.outfit_offset or args.outfit_lean:
        # 位移写在**载体自己的水平坐标系**里，不是世界轴：本体普遍带 --face-rot-z 90 烘，
        # 那时"前后"落在世界 Y 上、"左右"落在世界 X 上 ⇒ 拿世界轴写这个数会把前后当左右（脚区那一维
        # 首轮已经栽过一次，同一个坐标系错误不该在第二个旋钮上重犯）。
        la = armature.matrix_world.to_3x3() @ Vector((1.0, 0.0, 0.0))
        la.z = 0.0
        if la.length < 1e-6:
            sys.exit("--outfit-offset / --outfit-lean 要用载体的左右轴，而这份骨架的局部 X 投到水平面是零向量（它指着正上/正下）")
        la.normalize()
        fw = Vector((NOSE[0] - REAR[0], NOSE[1] - REAR[1], 0.0))
        if fw.length < 1e-6:
            sys.exit("--outfit-offset / --outfit-lean 要用鼻尖向量当'前后'，而这份载体的鼻尖在正上/正下")
        fw.normalize()
        if args.outfit_offset:
            ox, oy, oz = parse_vec(args.outfit_offset)
            # 左右那一项按"这一件在哪一侧"对齐成**向外**：attach-role 落在 -la 那一侧时，同一个 +oy
            # 必须读成"更右"而不是"更左"，否则换一根骨当 attach-role 就把这个旋钮的含义整个翻反。
            # 镜像件不在这里管：镜面法向就是左右轴，反射自己会把 la 分量翻号 ⇒ 写一次 0,+0.12,0 是两只都朝外。
            side = 1.0 if bone_world.dot(la) >= 0.0 else -1.0
            offset = fw * ox + la * (oy * side) + Vector((0.0, 0.0, oz))
        if args.outfit_lean:
            # 正数要读成"顶端朝外仰"，所以轴的符号由鼻尖向量现量决定而不是照抄 la：
            # 挂在 +Z 的那一点在绕 u 转时的瞬时速度 ∝ u × ẑ，要它指向 −fw（背离身体）⇒ 挑那个点积为正的符号。
            lean_axis = la if la.cross(Vector((0.0, 0.0, 1.0))).dot(-fw) > 0.0 else -la
    seat = seat + offset
    xform = (
        Matrix.Translation(seat)
        # 俯仰写在 yaw 的**左边** = 它转的是"已经被 rot_z/tilt 摆好的那一整件"，且绕的是载体的左右轴
        # 而不是配件自己的轴 ⇒ 这正是 tilt 给不出的那个自由度（背挂的盾：tilt 管盘面斜不斜，lean 管整件仰不仰）。
        @ (
            Matrix.Rotation(math.radians(args.outfit_lean), 4, lean_axis)
            if lean_axis is not None
            else Matrix.Identity(4)
        )
        @ Matrix.Rotation(math.radians(args.outfit_rot_z), 4, "Z")
        # 倾角绕帽子**自身**的 Y 轴（链里写在 yaw 的右边 ⇒ 对点而言"先倾后转"）：
        # 放 yaw 左边就变成绕世界前后轴倒，那时 rot_z≠0 的帽子朝一个固定的世界方向倒，
        # 换一个 rot_z / 换一份配件文件就是另一种形状，读数不再有出处。
        @ Matrix.Rotation(math.radians(args.outfit_tilt), 4, "Y")
        @ Matrix.Diagonal((scale, scale, scale * args.outfit_scale_z, 1.0))
        @ Matrix.Translation(-anchor0)
    )
    for ob in hat:
        ob.matrix_world = xform @ ob.matrix_world
    update_depsgraph()
    placed = {ob.name: world_of(ob).copy() for ob in hat}
    # 一条容差，两处用（绑定验收 + 附着漂移）：绝对量随缩放漂 ⇒ 一律按**定标后的整组足迹**归一。
    tol = 1e-3 * max(width * scale, 1e-9)
    dupes = []
    refl = None
    if mirror_role:
        # 镜像必须在**摆放之后、绑定之前**烘：refl 是由两根骨的 rest 位置量出来的，而摆放把
        # 原件放到了这一侧的脚上 —— 先烘再摆会让"镜像面"作用在未摆放的局部数据上。
        refl, mirror_mid, mirror_n = mirror_frame(armature, roles, role, mirror_role, evaluated_z_span(meshes))
        dupes = mirror_objects(hat, placed, refl)
    bind_child_of(armature, head, hat, placed, tol)
    if dupes:
        # 第二份绑到**另一根**骨上：这条调用就是"两只脚不是同一只靴子贴两遍"的全部实现，
        # 而它能不能被证伪由下面那条逐实例门禁负责（扰动单根骨，另一只必须不动）。
        bind_child_of(armature, roles[mirror_role], dupes, placed, tol)
    instances = [
        {
            "bone": head,
            "role": role,
            "objects": [ob.name for ob in hat],
            "ref": hat[0].name,
            # 变换后挂点正好落在 seat 上 ⇒ 存进 ref 的局部空间，之后每帧读它的世界位置
            "anchor_local": placed[hat[0].name].inverted() @ seat,
            "expect": seat.copy(),
        }
    ]
    if dupes:
        # 镜像件的挂点由 refl 作用在第一件的挂点上得到（同一份局部量在两具骨上不再同值）
        instances.append(
            {
                "bone": roles[mirror_role],
                "role": mirror_role,
                "objects": [d.name for d in dupes],
                "ref": dupes[0].name,
                "anchor_local": placed[dupes[0].name].inverted() @ (refl @ seat),
                "expect": (refl @ seat).copy(),
            }
        )

    ctx = {
        "bone": head,
        "instances": instances,
        "attach_role": role,
        "mirror_role": mirror_role,
        "scale_ref": args.outfit_scale_ref,
        "size_ratio": size_ratio,
        "scale": scale,
        "scale_z": args.outfit_scale_z,
        "rot_z": args.outfit_rot_z,
        "tilt": args.outfit_tilt,
        "lean": args.outfit_lean,
        "seat": args.outfit_seat,
        "offset": args.outfit_offset,
        "anchor_world0": seat.copy(),
        "bone_len": bone_len,
        "head_region": {k: (list(v) if isinstance(v, Vector) else v) for k, v in region.items()},
        # 脚区那一支的分侧判据要能被 check_attach 复用（"这根骨驱动的可见网格动没动"得按同一侧量）
        "ref_side": ref_side,
        "ref_axis": tuple(ref_axis) if ref_axis is not None else None,
        "ref_plane": tuple(ref_plane) if ref_plane is not None else None,
        # 手区那一支的参照物是一个**球**，region_top 要按同一个球复算 ⇒ 球心与半径都得进 ctx
        "ref_center": tuple(ref_center) if ref_center is not None else None,
        "ref_radius": hand_radius,
        "group_width": width,
        "object_widths": object_widths,
        "anchor_object": anchor_from,
        "ring_object": ring_name,
        "ring_width": round(ring_width, 5),
        # 定标后的**整组**世界足迹 = 投影尺寸那条**量级**门禁的期望值（关系检查看不见量级错，见 check_attach）
        "size_ref_world": width * scale,
        # 定标后的**世界身高**（已按俯角投影到屏幕 y）：旗杆这类"高 ≫ 宽"的配件，屏幕上量的那一维是它，
        # 拿足迹去期望它 = 一条只会假红的门禁（flag 首轮实测 25.3px vs 14.0px = 1.81×）。
        "tall_ref_world": tall_world,
        "ring_world": ring_width * scale,
        "objects": [ob.name for ob in hat] + [d.name for d in dupes],
        # 镜像门禁要拿 refl 逐顶点比，而它在 attach 之后没人再认识 ⇒ 跟着 ctx 走
        "mirror_refl": None if refl is None else [list(r) for r in refl],
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
    drift_tol = tol  # 与上面 bind_child_of 验收同一条带、同一个出处（别在这里再写一遍算式）
    for i, inst in enumerate(instances):
        drift = outfit_point(ctx, i) - inst["expect"]
        # 每一件各自验一次：只验第一件的话，"镜像件绑回同一根骨 / inverse 解错"这类形状会全绿过去，
        # 而那正是这条能力唯一要防的事。
        if drift.length > drift_tol:
            sys.exit(
                "上完 CHILD_OF 后 %s 骨那一件（%s）的挂点漂了 %.6f 世界单位（容差 %.6f）"
                "⇒ 那一份的 inverse_matrix 配方不对（读的是评估后的矩阵，和渲染器同一份）"
                % (inst["bone"], "+".join(inst["objects"]), drift.length, drift_tol)
            )
        if i == 0:
            print("BAKE 附着漂移：残差 %.9f（容差 %.9f = 1e-3 × 定标后整组足迹 %.3f）⇒ 过"
                  % (drift.length, drift_tol, ctx["size_ref_world"]))
        else:
            print("BAKE 镜像件附着漂移：残差 %.9f（同一容差 %.9f）⇒ 过" % (drift.length, drift_tol))
    # 措辞跟着参照物走：这条打印是"这批像素怎么定出来的"的唯一出处，靴子那一支要是照打
    # "头宽/颈收缩/帽圈"，读的人就会拿 0.446 去查 bull 的头宽（bear 首轮实测正是这样错的）
    ref_label, pinch_label, ring_label, height_label, region_label, ratio_note = slot_labels(
        args.outfit_scale_ref
    )
    # 帽子那一支（scale_z=1）连这行打印都逐字节不变：日志比对是"新开关没碰到旧那支"的免费证据
    zs = "" if args.outfit_scale_z == 1.0 else "、自身 Z 另压 %.2f" % args.outfit_scale_z
    print(
        "BAKE 配件附着：bone=%s(len %.3f，但**定标/落座用的是%s**) %s %.3f %s=%.3f %s %.3f ⇒ "
        "挂到 %.0f%% 段 → 世界 (%+.3f,%+.3f,%+.3f)，缩放 %.4f（整组足迹 %.3f → %.3f）%s，绕 Z 转 %g°、绕自身 Y 倾 %g°\n"
        "  定标参考是**整组水平足迹**（%s），不是%s：%s %s 实测 %.3f → 定标后 %.3f = **%.2f × %s**"
        "（公式那个 %g 只管整组足迹 ⇒ %s）\n"
        "  整组各件宽度：%s"
        % (
            head,
            bone_len,
            region_label,
            ref_label,
            region["width"],
            pinch_label,
            region["pinch_z"],
            height_label,
            region["height"],
            100 * args.outfit_seat,
            seat.x,
            seat.y,
            seat.z,
            scale,
            width,
            width * scale,
            zs,
            args.outfit_rot_z,
            args.outfit_tilt,
            max(object_widths, key=lambda n: object_widths[n]),
            ring_label,
            ring_label,
            ring_name,
            ring_width,
            ring_width * scale,
            (ring_width * scale) / region["width"],
            ref_label,
            size_ratio,
            ratio_note,
            " ".join("%s=%.3f" % (n, w) for n, w in sorted(object_widths.items())),
        )
    )
    if args.outfit_lean:
        # 单独一行而不是补进上面那行：那行的字节是"帽子那一支没被新开关碰到"的免费证据，
        # 而这一维只有背挂件才用得到。打的是**轴向量**，因为"正号到底朝哪倒"是这个符号选择的结果，
        # 换一份脸不朝 +X 的载体就得重新量，不能靠读 help 记住。
        print(
            "BAKE 配件整体俯仰：绕载体左右轴 %+.3f°，轴 = (%+.3f,%+.3f,0) ⇒ 正号 = 顶端朝背离载体那一侧仰"
            % (args.outfit_lean, lean_axis.x, lean_axis.y)
        )
    if args.outfit_offset:
        # 上面那行世界坐标读不出"这一件往哪边挪了多少"：位移写在载体坐标里，而镜像件的左右分量
        # 由镜面自己翻号 ⇒ "筒中心对没对上腿中心"这个数必须由这一行给出出处，不能靠人反推世界坐标。
        print(
            "BAKE 配件平移：载体坐标 前后=%+.3f 左右=%+.3f（attach-role 这一侧向外）上下=%+.3f "
            "⇒ 世界 (%+.3f,%+.3f,%+.3f)" % (ox, oy, oz, offset.x, offset.y, offset.z)
        )
    return ctx


def outfit_point(ctx, index=0):
    """这一帧挂点的世界坐标 —— 读的是被约束算完的物体矩阵，和渲染器画的是同一份。

    `index=0` = `--outfit-attach-role` 那一件；给了 `--outfit-mirror-to` 时第二件在 `index=1`。
    逐实例读，不合并成一个"整组质心" —— 合并会把"两件绑在同一根骨上"读成正常。
    """
    inst = ctx["instances"][index]
    return world_of(bpy.data.objects[inst["ref"]]) @ inst["anchor_local"]


def check_attach(armature, roles, ctx, base, cam, cell, spec, pose, meshes):
    """本轮最重要的一条自检："我 parent 了"不等于"它跟着动"，而"跟着动"也不等于"大小对"。

    ① 逐实例：把**这一件自己那根骨**在世界里平移 +0.05 ⇒ 这一件必须跟着走同样的量，而**其余每一件
       必须几乎不动**。镜像那一支的整条能力就立在这一条上：扰动左踝而两只靴子一起动 = 两件绑在
       同一根骨上 = "看起来对称、其实是一只靴子贴两遍"，而这正是这条门禁唯一抓得住的形状。
    ② idle 全帧内每一件配件的**外接半径**必须恒定 —— 那是 use_scale_*=False 真的生效的证据。
       ⚠️ 这里刻意不量投影 px：投影随缩放**和朝向**两者变，量它等于假设附着骨不转（帽子碰巧成，
       手骨在 idle 里摆 2.3° 就把一条完全健康的旗子判成"骨缩放漏进配件"，2026-11-09 现量判别见
       `scratch/gate2_cause_2026-11-09.py`）。外接半径只随缩放变，正是这条门禁想读的那一件事。
    ③ **量级**门禁：那个恒定尺寸必须落在"定标公式 × 参照宽"推算出来的像素附近（**逐件**比，
       两件并在一起的跨度是"两只靴子之间的距离"，跟任何一份定标公式都无关 ⇒ 拿它比必然假红）。

    ③ 是那一轮加进来的，因为 ①② 都被实测证伪过其充分性：inverse_matrix 把缩放平方成
    0.00026（要 0.0162，62× 太小）时，① 的位移仍然精确等于 0.050、② 的峰峰仍然 0.000
    —— **关系检查对"标定错了"天然盲**，只有拿一个绝对期望值去比才抓得住。
    """
    shift = armature.matrix_world.inverted() @ Matrix.Translation((0.0, 0.0, 0.05)) @ armature.matrix_world
    for k, inst in enumerate(ctx["instances"]):
        before = [outfit_point(ctx, i) for i in range(len(ctx["instances"]))]
        # 顺带记一份"看得见的头"在骨动之前的高度，用于下面那条 rig 事实（头顶顶点归谁管）
        crown_before = region_top(meshes, ctx)
        armature.pose.bones[inst["bone"]].matrix = shift @ armature.pose.bones[inst["bone"]].matrix
        update_depsgraph()
        after = [outfit_point(ctx, i) for i in range(len(ctx["instances"]))]
        crown_after = region_top(meshes, ctx)
        reset_bones(armature, base)
        for i, (b0, a0) in enumerate(zip(before, after)):
            delta = a0 - b0
            want = 0.05 if i == k else 0.0
            # "别的实例纹丝不动"这一档**必须自己红**：原先那条只看"被扰动的那件动不动"，
            # 于是两件绑同一根骨（都动 0.05）在旧口径下是满分通过。
            bad = (delta.length < 0.04 or abs(delta.z - 0.05) > 1e-3) if i == k else (delta.length > 1e-4)
            if bad:
                why = (
                    "该动的没动 ⇒ CHILD_OF 的 target/subtarget/inverse_matrix 有一条是摆设，或绑错了骨"
                    if i == k
                    else "**不该动的动了 ⇒ 两件绑在同一根骨上，镜像那件事根本没发生**"
                )
                sys.exit(
                    "扰动 %s 骨（+0.050 世界 Z）⇒ 第 %d 件（绑在 %s）实测位移 (%+.4f,%+.4f,%+.4f)，"
                    "期望 %.3f ⇒ %s"
                    % (inst["bone"], i, ctx["instances"][i]["bone"], *delta, delta.length, want, why)
                )
            if i == k:
                print(
                    "BAKE 附着自检：扰动 %s 骨 +0.050 ⇒ 该件挂点位移 (%+.4f,%+.4f,%+.4f) ⇒ 真的接上了；"
                    "而参照网格只动 %.4f ⇒ %s"
                    % (
                        inst["bone"],
                        *delta,
                        abs(crown_after - crown_before),
                        region_drive_note(ctx, abs(crown_after - crown_before)),
                    )
                )
            else:
                print(
                    "BAKE 镜像逐实例自检：扰动 %s 骨时，另一件（%s）位移 %.6f ⇒ 没跟着动，两件各绑一根骨"
                    % (inst["bone"], ctx["instances"][i]["bone"], delta.length)
                )

    # ② 尺寸恒定：这条要抓的**只有**"骨缩放漏进配件"（胸腔一呼吸就把帽子吹大 ±3.75%）。
    # ⚠️ 判据量的是**外接半径**而不是投影 px：投影同时随缩放**和朝向**变，拿它当判据等于隐含
    # "配件朝向恒定"这个前提 —— 头骨在 idle 里几乎不转，帽子那一支实测峰峰 0.000px，前提是**碰巧**成立；
    # 手骨在 idle 里摆 2.3°，flag × bear 三档 --outfit-scale 的 峰峰/尺寸 恒为 2.02%（0.511/25.29、
    # 0.918/45.46、1.377/68.19），而那个比值**缩放漏和转动都造得出** ⇒ 只能现量判别
    # （`scratch/gate2_cause_2026-11-09.py`）：matrix 缩放逐帧 0.900000、外接半径逐帧 0.706137，
    # 而杆（glTF 导入后是局部 Y）离铅垂 0.000°/2.183°/0.000°/2.328° ⇒ 那 2% 是转动，门禁当时在报假红。
    # 外接半径 = 点到质心的最大距离，刚体下只随缩放变、与朝向完全无关 ⇒ 换成它，病照样抓得住而噪声少四个量级。
    # ⚠️ 仍要**逐实例量，不能拿两件并在一起的跨度**：并起来读的是"两只靴子之间的距离"，
    # 它随两条腿的摆动而变 ⇒ 一份完全健康的镜像会被这条判成"骨缩放漏进配件"。
    frames = len(spec["states"]["idle"]["rows"])
    loop = spec["states"]["idle"].get("loop", True)
    place_camera(cam, 0)
    limit = 0.5 * cell / 96.0
    # 期望值取**水平足迹与投影身高的较大者**，不是只取足迹：`sizes` 量的本来就是投影 x/y 跨度的
    # 较大那一维，而旗杆这类"高 ≫ 宽"的配件屏幕上占优的是高 ⇒ 只按足迹期望必然假红
    # （flag × bear 首轮实测 25.3px vs 14.0px，比值正是身高 0.361 × cos15° / 足迹 0.193）。
    # 帽子与靴子那两支足迹 ≥ 身高，取大者之后与旧值**逐字节相同**，这条改动的形状只对"细长"生效。
    expect_world = max(ctx["size_ref_world"], ctx["tall_ref_world"])
    expect = expect_world / cam.data.ortho_scale * cell
    ref_label, _, ring_label, _, _, _ = slot_labels(ctx["scale_ref"])
    for k, inst in enumerate(ctx["instances"]):
        hat = [bpy.data.objects[n] for n in inst["objects"]]
        sizes = []
        radius = []
        for frame in range(frames):
            pose("idle", frame, frames, loop)
            update_depsgraph()
            pts = [p for ob in hat for p in rigid_points(ob, limit=800)]
            co = [world_to_camera_view(bpy.context.scene, cam, p) for p in pts]
            sizes.append(max((max(c.x for c in co) - min(c.x for c in co)) * cell, (max(c.y for c in co) - min(c.y for c in co)) * cell))
            mid = sum(pts, Vector()) / len(pts)
            radius.append(max((p - mid).length for p in pts))
        spread = max(sizes) - min(sizes)
        r_mean = sum(radius) / len(radius)
        rel = (max(radius) - min(radius)) / r_mean
        tag = "" if k == 0 else "（镜像件）"
        print(
            "BAKE 配件外接半径%s idle 全帧 %s ⇒ 相对峰峰 %.4f%%（门禁 ≤%.2f%%，这条量的才是骨缩放有没有漏进配件）"
            % (tag, " ".join("%.6f" % r for r in radius), 100 * rel, 100 * OUTFIT_SCALE_LEAK_TOL)
        )
        print(
            "BAKE 配件投影尺寸%s idle 全帧 %s ⇒ 峰峰 %.3fpx（≤%.1fpx 这一档**只打印不判红**：投影随缩放和朝向两者变，"
            "附着骨一转它必然非零）" % (tag, " ".join("%.2f" % s for s in sizes), spread, limit)
        )
        if rel > OUTFIT_SCALE_LEAK_TOL:
            sys.exit(
                "配件（%s 骨那一件）外接半径随 idle 帧变化 %.4f%%（门禁 ≤%.2f%%）—— use_scale_*=False 没生效"
                "（胸腔围长 ±3.75%% 传染给配件）；外接半径与朝向无关，所以这一条不再是假红"
                % (inst["bone"], 100 * rel, 100 * OUTFIT_SCALE_LEAK_TOL)
            )

        # ③ 量级：正交相机下世界尺寸 → px 就是 (extent / ortho_scale) × cell，期望值由定标公式直接给出
        # ⚠️ 这条比的是**整组足迹**（sizes 取的是全组投影 x/y 跨度的较大者，期望用的也是整组世界宽度）
        # ⇒ 它只证明"定标公式接上了"，不证明"帽檐有 1.25 倍头宽"；后者由下面那行打印交人眼判。
        got = sum(sizes) / len(sizes)
        if not (0.6 * expect <= got <= 1.6 * expect):
            sys.exit(
                "配件（%s 骨那一件）投影尺寸 %.1fpx 落在期望 %.1fpx 的 ±60%% 带外（期望用的那一维 = %.4f 世界单位、"
                "足迹 %.4f / 投影身高 %.4f、ortho_scale %.3f、cell %d）\n"
                "  ⇒ 定标或 CHILD_OF 的缩放档错了；①② 那两条关系检查看不见这种错（62× 太小也照样绿）"
                % (
                    inst["bone"],
                    got,
                    expect,
                    expect_world,
                    ctx["size_ref_world"],
                    ctx["tall_ref_world"],
                    cam.data.ortho_scale,
                    cell,
                )
            )
        print(
            "BAKE 配件尺寸量级%s：实测 %.1fpx vs 期望 %.1fpx（= %.2f 世界单位【%s】 / ortho %.3f × %d）⇒ 在带内；"
            "其中%s %.1fpx = %.2f × %s %.1fpx"
            % (
                tag,
                got,
                expect,
                expect_world,
                "投影身高" if ctx["tall_ref_world"] > ctx["size_ref_world"] else "整组足迹",
                cam.data.ortho_scale,
                cell,
                ring_label,
                ctx["ring_world"] / cam.data.ortho_scale * cell,
                ctx["ring_world"] / ctx["head_region"]["width"],
                ref_label,
                ctx["head_region"]["width"] / cam.data.ortho_scale * cell,
            )
        )
    reset_bones(armature, base)


def check_mirror_residual(armature, ctx, base):
    """镜像残差门禁：把左件逐顶点按镜面搬过去，和右件的世界位置比，取最大偏差。

    这条量的是**"整只靴子对称"而不只是"挂点对"**：`attach_outfit` 末尾那条漂移自检只看一个点，
    而 CHILD_OF 的 inverse 是逐物体的矩阵 —— 一件的 inverse 带着一点旋转，挂点照样落在 seat 上、
    靴筒却歪了。逐顶点比才看得见。取点走 `rigid_points` 那条同一口径（基网格 × **评估后的**世界矩阵，
    不 `to_mesh()`：靴子那份 28.7 万顶点，重建修改器栈是白烧）。
    对照关系是**逐索引**的（镜像件由 `mirror_objects` 从同一份 mesh 复制、只搬顶点，顺序逐位相同），
    所以这里必须按同一个步长取样，不能各抽各的。
    """
    if ctx["mirror_refl"] is None:
        return
    refl = Matrix(ctx["mirror_refl"])
    reset_bones(armature, base)
    update_depsgraph()
    worst, worst_at, pairs = 0.0, None, 0
    for name_a, name_b in zip(ctx["instances"][0]["objects"], ctx["instances"][1]["objects"]):
        ob_a, ob_b = bpy.data.objects[name_a], bpy.data.objects[name_b]
        va, vb = ob_a.data.vertices, ob_b.data.vertices
        if len(va) != len(vb):
            sys.exit(
                "镜像件 %s 有 %d 个顶点而原件 %s 有 %d 个 ⇒ 逐索引对应关系断了，这条残差门禁失去意义"
                % (name_b, len(vb), name_a, len(va))
            )
        ma, mb = world_of(ob_a), world_of(ob_b)
        step = max(1, len(va) // 4000)
        for i in range(0, len(va), step):
            want = refl @ (ma @ va[i].co)
            got = mb @ vb[i].co
            d = (want - got).length
            if d > worst:
                worst, worst_at = d, "%s[%d]" % (name_b, i)
            pairs += 1
    tol = 1e-3 * max(ctx["size_ref_world"], 1e-9)  # 与 bind/漂移同一条相对带，别在这里再写一遍算式
    print(
        "BAKE 镜像残差：比了 %d 对顶点（原件 %s ↔ 镜像件 %s）⇒ 最大偏差 %.7f 世界单位（容差 %.7f = 1e-3 × 定标后足迹）%s"
        % (
            pairs,
            " ".join(ctx["instances"][0]["objects"]),
            " ".join(ctx["instances"][1]["objects"]),
            worst,
            tol,
            "⇒ 两只真的互为镜像" if worst <= tol else "（最大在 %s）⇒ **不对**" % worst_at,
        )
    )
    if worst > tol:
        sys.exit(
            "镜像残差 %.7f 超容差 %.7f ⇒ 那两只靴子不是同一个东西的镜像（多半是某一件的 "
            "inverse_matrix 带了点旋转/缩放，而单点挂点自检看不见）" % (worst, tol)
        )


def region_top(meshes, ctx):
    """把"定标参照那块网格"的世界最高 z 量出来，用来验那根骨到底驱不驱动它。"""
    if ctx["scale_ref"] == "foot_box":
        return foot_top(
            meshes,
            ctx["head_region"]["crown_z"],
            ctx["ref_side"],
            Vector(ctx["ref_axis"]),
            Vector(ctx["ref_plane"]),
        )
    if ctx["scale_ref"] == "hand_bone":
        return hand_top(meshes, Vector(ctx["ref_center"]), ctx["ref_radius"])
    return crown_top(meshes, ctx["head_region"]["pinch_z"])


def region_drive_note(ctx, moved):
    """那条 rig 事实的一句话读数：骨动了，可见的参照网格跟了多少 ⇒ 骨能不能当参照物。"""
    subject = {
        "foot_box": "该侧脚的网格",
        "hand_bone": "该侧手的网格",
    }.get(ctx["scale_ref"], "颅顶网格")
    return (
        "骨驱动的是可见的%s" % subject
        if moved > 0.04
        else "这根骨**不驱动可见的%s**（蒙皮权重在别处），所以它的位置/长度都不能当定标参照 ⇒ 参照必须是网格几何" % subject
    )


def hand_top(meshes, center, radius):
    """该手球区内的网格最高点 —— 与 `hand_region` 用同一批点，是"这根骨驱动的是不是这只手"的读数。"""
    r2 = radius * radius
    top = -1e9
    for ob in meshes:
        for p in evaluated_points(ob, limit=1500):
            if (p - center).length_squared <= r2:
                top = max(top, p.z)
    return top


def foot_top(meshes, ankle_z, side, axis, plane_pt):
    """该侧、踝以下的网格最高点 —— 与 `foot_region` 用同一批点，是"这根骨驱动的是不是这只脚"的读数。"""
    top = -1e9
    for ob in meshes:
        for p in evaluated_points(ob, limit=1500):
            if p.z <= ankle_z + 1e-6 and (p - plane_pt).dot(axis) * side > 0.0:
                top = max(top, p.z)
    return top


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


# 遮挡那一趟给配件挂的临时颜色。选品红不是审美：判据读的是"R、B 都明显高于 G"这个**关系**，
# 它对 view transform 的具体曲线不敏感（AgX/Filmic 会把高饱和往灰里拉，但拉的幅度对 R/B 对称、
# 对 G 不对称 ⇒ 关系保留），而载体自然色里没有一个落在这个锥形里（白/灰：R+B 追不上 2.4G；
# 红：B 太小；蓝：R 太小）。第一次在新模型上开这一档时看那行 `遮罩判据实测` 读数，别看直觉。
OCC_MASK_EMISSION = (1.0, 0.0, 1.0)


def occ_front(r, g, b):
    """遮罩那一趟里"这个像素最前面是配件吗"。"""
    return min(r, b) > 0.55 * max(r, b) and (r + b) > 2.4 * g + 0.06


def occ_mask_material():
    mat = bpy.data.materials.get("OPET_OCC_MASK")
    if mat is None:
        mat = bpy.data.materials.new("OPET_OCC_MASK")
    mat.use_nodes = True
    nt = mat.node_tree
    nt.nodes.clear()
    out = nt.nodes.new("ShaderNodeOutputMaterial")
    em = nt.nodes.new("ShaderNodeEmission")
    em.inputs["Color"].default_value = (OCC_MASK_EMISSION[0], OCC_MASK_EMISSION[1], OCC_MASK_EMISSION[2], 1.0)
    em.inputs["Strength"].default_value = 1.0
    nt.links.new(em.outputs["Emission"], out.inputs["Surface"])
    return mat


def paint_occ_mask(hat):
    """整组配件（含镜像那件）的材质槽临时换成品红自发光，返回"网格名 → 原材质表"用于逐格还原。

    按**网格**存而不是按物体：镜像件是从原件 `copy()` 来的，而 `copy()` 复制的是物体不是 mesh 数据块
    ⇒ 两件共用一份 `materials` 列表，按物体存会在还原时把同一份列表填两遍。
    """
    mat = occ_mask_material()
    saved = {}
    for ob in hat:
        me = ob.data
        if me.name in saved:
            continue
        saved[me.name] = list(me.materials)
        me.materials.clear()
        me.materials.append(mat)
    return saved


def wipe_occ_paint(saved):
    for name, mats in saved.items():
        me = bpy.data.meshes.get(name)
        if me is None:
            continue
        me.materials.clear()
        for m in mats:
            me.materials.append(m)


def apply_outfit_occlusion(path, mask_path, hat, meshes, totals, spp):
    """把这一格配件里**被载体挡在身后**的像素抹掉（就地改写 `path` 那张 PNG 的 alpha）。

    只在 `--outfit-only` 下调用：那时载体是被 `hide_render` 藏着的，这一趟临时把它放回来、渲完再藏回去。
    为什么能这么换：`hide_render` 只管成片里画不画，不动几何也不动姿态（见 `hide_carrier`），
    而遮罩要的正是"同一台相机、同一批姿态下，谁在最前面"。
    """
    scene = bpy.context.scene
    for ob in meshes:
        ob.hide_render = False
    saved = paint_occ_mask(hat)
    eevee = getattr(scene, "eevee", None)
    keep_spp = getattr(eevee, "taa_render_samples", None) if eevee is not None else None
    if eevee is not None and hasattr(eevee, "taa_render_samples"):
        # 遮罩要的是边界清楚，不是画面好看：默认 1 采样既省掉一半以上的时间，也不给判据留半透明的边。
        # ⚠️ 但"不给判据留半透明的边"这一句对**薄网格不成立**：一根一两个像素宽的羽毛，它在遮罩图里
        # 本来就是半覆盖的，1 采样把这种像素判成"不是品红"⇒ 成品里那根羽毛被抹掉。`--outfit-occ-samples`
        # 就是给这一族留的口子（默认 1 ⇒ 旧行为逐字节不变）。
        eevee.taa_render_samples = max(1, spp)
    keep_path = scene.render.filepath
    scene.render.filepath = mask_path
    try:
        bpy.ops.render.render(write_still=True)
    finally:
        scene.render.filepath = keep_path
        wipe_occ_paint(saved)
        for ob in meshes:
            ob.hide_render = True
        if eevee is not None and keep_spp is not None:
            eevee.taa_render_samples = keep_spp

    prod = bpy.data.images.load(path, check_existing=False)
    mask = bpy.data.images.load(mask_path, check_existing=False)
    try:
        w, h = prod.size[0], prod.size[1]
        if (mask.size[0], mask.size[1]) != (w, h):
            sys.exit("BAKE 遮挡：遮罩图 %dx%d 与成品 %dx%d 不同尺寸 ⇒ 两趟不是同一个渲染设置" % (mask.size[0], mask.size[1], w, h))
        pp = list(prod.pixels[:])
        mp = mask.pixels[:]
        kept = erased = 0
        rg_sum = [0.0, 0.0, 0.0]
        for i in range(w * h):
            if pp[i * 4 + 3] <= 0.5:
                continue
            o = i * 4
            if mp[o + 3] > 0.5 and not occ_front(mp[o], mp[o + 1], mp[o + 2]):
                pp[o + 3] = 0.0
                erased += 1
            else:
                kept += 1
                rg_sum[0] += mp[o]
                rg_sum[1] += mp[o + 1]
                rg_sum[2] += mp[o + 2]
        # kept==0 在这一格**未必是病**：位移小的那一档里旗在侧身整格躲到载体身后，正确的遮挡产物
        # 就是全透明。上一版把它当场 sys.exit，于是探针梯被自己的门禁打断，而它要的恰恰是这个数。
        # 真病是"整批一件都没留下"（材质没挂上 / 品红被 view transform 拉出锥形），那由批末那条
        # 汇总判据负责，且那批的 empty 计数会等于 cells，读数一样清楚。
        if kept == 0 and erased:
            totals["empty"].append(path)
        prod.pixels[:] = pp
        prod.filepath_raw = path
        prod.file_format = "PNG"
        prod.save()
    finally:
        bpy.data.images.remove(mask)
        bpy.data.images.remove(prod)
    totals["cells"] += 1
    totals["kept"] += kept
    totals["erased"] += erased
    if kept:
        totals["rgb"] = [c + s / kept for c, s in zip(totals["rgb"], rg_sum)]
    return kept, erased


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
    "outfit_lean": 0.0,
    "outfit_seat": OUTFIT_SEAT,
    "outfit_subsurf": -1,
    # 这三条是"靴子那一支"新加的开关，而它们直接决定像素：绑到 foot_l 还是 head、烘不烘第二件、
    # 按头宽还是按脚尺寸定标 ⇒ 少记一条，测量轮就会拿帽子的几何去量靴子那批图（同族缺口见上面注释）。
    # 默认值必须与 parse_args 逐字相同（"命令行没给"唯一的判据就是"当前值 == 默认值"）。
    "outfit_attach_role": "head",
    "outfit_mirror_to": "",
    "outfit_scale_ref": "head_width",
    # 这两条同样直接改像素，而且是 boot 这一支被眼判否之后才补的：**平移**决定筒套不套在腿上、
    # **单轴压高**决定靴筒到哪 ⇒ 漏记一条就会拿"没挪、没压"的几何去量挪过、压过的那批图。
    "outfit_offset": "",
    "outfit_scale_z": 1.0,
}


def _kv_text(key, value):
    """参数还原那两行打印的统一格式：数值键 %g，字符串键原样（`--outfit-attach-role` 是字符串）。"""
    return "%s=%s" % (key, ("%g" % value) if isinstance(value, (int, float)) else value)


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
                    clash.append("%s：命令行 %s vs 记录 %s" % (key, getattr(args, key), meta[key]))
                continue
            setattr(args, key, meta[key])
            from_meta.append(_kv_text(key, meta[key]))
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
        # 镜像那一支有第二件、第二根骨 ⇒ 每件各自是谁绑的、期望挂点在哪，都得有出处（逐实例那条门禁读它）
        "instances": [
            {
                "role": inst["role"],
                "bone": inst["bone"],
                "objects": inst["objects"],
                "expect_world": [round(v, 5) for v in inst["expect"]],
            }
            for inst in ctx["instances"]
        ],
        "objects": ctx["objects"],
        "scale": ctx["scale"],
        "scale_z": ctx["scale_z"],
        "offset": ctx["offset"],
        "rot_z": ctx["rot_z"],
        "tilt": ctx["tilt"],
        "seat_fraction": ctx["seat"],
        # 帽檐落座的世界点：只有 fraction 的话，别人拿到这份 json 还要先复算 head_region 才知道帽在哪
        "seat_world": [round(v, 5) for v in ctx["anchor_world0"]],
        # 存**这次真的用了的那个系数**，不存模块常数：head_width 与 foot_box 两条公式系数不同，
        # 写死 OUTFIT_SIZE_RATIO 会让靴子那份 json 自称按头宽定过标。
        "size_ratio": ctx["size_ratio"],
        "scale_ref": ctx["scale_ref"],
        "attach_role": ctx["attach_role"],
        "mirror_role": ctx["mirror_role"],
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
        "tall_ref_world": round(ctx["tall_ref_world"], 5),
        "ring_world": round(ctx["ring_world"], 5),
        "ring_to_head_width": round(ctx["ring_world"] / ctx["head_region"]["width"], 4),
        "anchor_local": [round(v, 5) for v in ctx["instances"][0]["anchor_local"]],
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
        # 逐实例各记一条（形状与 head_rows 同为 帧×列）：靴子那两件绑的是两根不同的骨，
        # 而"两件的读数逐位相同"本身就是绑错骨的证据 ⇒ 不能只记第一件
        extra_rows = [[] for _ in ctx["instances"][1:]]
        for frame in range(frames):
            pose(state, frame, frames, loop)
            update_depsgraph()
            col_head, col_neck = [], []
            col_extra = [[] for _ in extra_rows]
            for index in columns:
                place_camera(cam, index)
                col_head.append(px(outfit_point(ctx)))
                col_neck.append(
                    px(bone_slot_point(armature, armature.pose.bones[neck], NECK_SLOT_SEAT))
                    if neck
                    else None
                )
                for k, rows in enumerate(col_extra):
                    rows.append(px(outfit_point(ctx, k + 1)))
            head_rows.append(col_head)
            neck_rows.append(col_neck)
            for rows, col in zip(extra_rows, col_extra):
                rows.append(col)
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
        for k, rows in enumerate(extra_rows):
            data["states"][state]["instance_%d" % (k + 1)] = rows
    anchor_state = "idle" if "idle" in data["states"] else states[0]
    data["anchor_from"] = [anchor_state, 0, columns[0]]
    # 键名 = 这份配件真的占的那个 slot（`--outfit-attach-role foot_l` 量出来的点写进 "head"
    # 就是一份自称帽点的靴子读数，而 pack.py 会照原样写进本体清单的 outfit_anchors）。
    anchor_slot = {
        "foot_box": "feet",
        "hand_bone": "hand",
    }.get(ctx["scale_ref"], "head")
    data["anchor"][anchor_slot] = data["states"][anchor_state]["head"][0][0]
    data["anchor"]["neck"] = data["states"][anchor_state]["neck"][0][0]
    if extra_rows:
        data["anchor"][anchor_slot + "_2"] = data["states"][anchor_state]["instance_1"][0][0]
    reset_bones(armature, base)

    out_path = os.path.join(args.out, "outfit.json")
    os.makedirs(args.out, exist_ok=True)
    with open(out_path, "w", encoding="utf-8") as fh:
        json.dump(data, fh, ensure_ascii=False, indent=1, sort_keys=True)
    total = sum(len(v["head"]) * len(columns) for v in data["states"].values())
    inst_note = "" if not extra_rows else " + 镜像件 [%s]" % ", ".join(
        "%.1f" % v for v in data["anchor"][anchor_slot + "_2"]
    )
    print(
        "BAKE 挂点测量：%s=[%s, %s]%s neck=[%s, %s]（%s 第 0 帧第 %d 列）⇒ 写了 %s，共 %d 个读数 × %d 件"
        % (
            anchor_slot,
            *data["anchor"][anchor_slot],
            inst_note,
            *data["anchor"]["neck"],
            anchor_state,
            columns[0],
            out_path,
            total,
            1 + len(extra_rows),
        )
    )
    missing = [s for s, v in sorted(data["anchor"].items()) if v is None]
    print(
        "BAKE 仍无事实源的 slot：%s —— 这份 rig 没有手腕骨角色（BONE_ROLES 只有 arm/forearm），"
        "也没有那些部位的几何来源，不硬写。" % (" / ".join(missing) or "（无）")
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
    # 手上那一档（旗子 / 拳套）的挂点。⚠️ 这四档**没有任何一支动作读它们**（STATE_BONE_NEEDS 里没有
    # hand_*，apply_bone_pose / apply_bone_action 也不点名手骨）⇒ 加进来对已签收的十份本体像素逐字节
    # 无影响；唯一可见的差别是 meta.json 的 `bones` 字典多出两行、以及"角色没对上"那行少列两项。
    # 手指骨（LeftHandIndex1…4 / Thumb / Middle / Ring / Pinky）故意不进候选表：配件要挂在手掌根，
    # 指骨一根 0.008 世界单位，拿它当参照物就是拿噪声定标。
    "hand_l": ("LeftHand", "HandL", "LeftWrist", "LeftPaw"),
    "hand_r": ("RightHand", "HandR", "RightWrist", "RightPaw"),
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


def paint_outfit_flat(hat, spec):
    """`--outfit-color` 那一档：整组配件网格换成一块平涂漫反（含镜像件，它与原件共用 mesh 数据）。

    换材质而不是调亮度：深色礼帽的源材质是近黑漫反 + 贴图，逐像素提亮会把贴图细节糊成一片，
    而"在屏幕上看不见"这件事的根因是反照率，不是曝光。返回建出来的那块材质（挡住渲染循环里
    `paint_occ_mask` 的存/取，它按网格存原材质表 ⇒ 还原时会还原回这一块）。
    """
    if not spec:
        return None
    parts = [x.strip() for x in spec.split(",") if x.strip()]
    if len(parts) != 3:
        sys.exit("--outfit-color 要三段 0–255 的 R,G,B（例：232,214,170）；给了 %r" % spec)
    try:
        rgb = [int(x) for x in parts]
    except ValueError:
        sys.exit("--outfit-color 三段必须是 0–255 的整数，收到 %r" % (parts,))
    if any(not 0 <= v <= 255 for v in rgb):
        sys.exit("--outfit-color 三段必须在 0–255 内，收到 %r" % (rgb,))
    mat = flat_material("outfit_flat", tuple(v / 255.0 for v in rgb))
    done = set()
    for ob in hat:
        me = ob.data
        if me.name in done:
            continue
        me.materials.clear()
        me.materials.append(mat)
        done.add(me.name)
    print("BAKE --outfit-color = %s ⇒ 配件 %d 块网格换成平涂（源材质与贴图不再参与渲染）" % (rgb, len(done)))
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


FACING_PROBE_PX = 192
FACING_PROBE_SAMPLES = 16
# 探针档从 96/4 提到 192/16：96 档会把 dinosaur 正确档的 fb 余量压到 +0.0117（192 档 +0.0268），
# 而这两条读数现在都只是线索、不判生死 ⇒ 没有理由省这四张图的钱。


def _probe_image_metrics(path):
    """一张透明底渲染图里的候选朝向测量（全部纯像素，不猜鼻尖）。

    包围盒中心做原点、半宽归一 ⇒ 与宠物在格子里摆在哪无关。
      `asym`  = 剪影左右不对称度（实测**不能**当判据：镜像对称性是环旋转的不变量）
      `wide`  = 剪影宽 / 高
      `headc` = 头部带（剪影上沿 35%）色度方差加权质心的横向偏移，±1 = 贴左右边
      `headv` = 头部带的色度标准差（脸有眼/鼻/嘴 ⇒ 应当比后脑勺花）
      `edge`  = 头部带的亮度梯度能量均值（格外的邻居按 0 算 ⇒ 剪影边界也算进去）
      `allv`  = 全图色度标准差
    """
    img = bpy.data.images.load(path, check_existing=False)
    try:
        w, h = img.size[0], img.size[1]
        px = img.pixels[:]
    finally:
        bpy.data.images.remove(img)
    alpha = [px[i * 4 + 3] for i in range(w * h)]
    xs = [i % w for i, a in enumerate(alpha) if a > 0.5]
    ys = [i // w for i, a in enumerate(alpha) if a > 0.5]
    if not xs:
        return None
    x0, x1 = min(xs), max(xs)
    y0, y1 = min(ys), max(ys)
    span = x1 - x0
    half = max(span / 2.0, 1.0)
    head_from = y1 - int(round(0.35 * (y1 - y0)))
    covered = 0
    asym = 0.0
    head_wsum = 0.0
    head_xsum = 0.0
    head_n = 0
    hs = [0.0, 0.0, 0.0]
    hq = [0.0, 0.0, 0.0]
    hlum = {}
    a_s = [0.0, 0.0, 0.0]
    a_q = [0.0, 0.0, 0.0]
    for y in range(y0, y1 + 1):
        base = y * w
        in_head = y >= head_from
        for x in range(x0, x1 + 1):
            i = base + x
            a = alpha[i]
            if a <= 0.5:
                continue
            covered += 1
            asym += abs(a - alpha[base + x0 + span - x])
            k = i * 4
            rgb = (px[k], px[k + 1], px[k + 2])
            for c in range(3):
                a_s[c] += rgb[c]
                a_q[c] += rgb[c] * rgb[c]
            if in_head:
                head_n += 1
                v = 0.0
                for c in range(3):
                    hs[c] += rgb[c]
                    hq[c] += rgb[c] * rgb[c]
                v = sum(rgb) / 3.0
                head_wsum += v * (x - x0 - half)
                head_xsum += v
                hlum[(x, y)] = 0.2126 * rgb[0] + 0.7152 * rgb[1] + 0.0722 * rgb[2]
    if covered == 0 or head_n == 0 or head_xsum <= 0.0:
        return None
    var = 0.0
    hvar = 0.0
    for c in range(3):
        m = a_s[c] / covered
        var += max(a_q[c] / covered - m * m, 0.0)
        hm = hs[c] / head_n
        hvar += max(hq[c] / head_n - hm * hm, 0.0)
    edge = 0.0
    for (x, y), l in hlum.items():
        d = 0.0
        for nx, ny in ((x - 1, y), (x + 1, y), (x, y - 1), (x, y + 1)):
            d += abs(l - hlum.get((nx, ny), 0.0))
        edge += d
    edge /= len(hlum)
    return {
        "asym": asym / covered,
        "wide": (span + 1) / float(y1 - y0 + 1),
        "headc": head_wsum / head_xsum / half,
        "headv": hvar**0.5,
        "edge": edge,
        "allv": var**0.5,
    }


def check_facing_render(cam, scene, pose, states, spec, cell, out_dir):
    """朝向环的**像素测量**：渲四张正交小图，打印线索，出那张必须眼过的 2×2。

    上面那条几何自检沿 +X 取极值、再断言这些极值在 col 0 落在屏幕右 —— 那是拿结论当前提，
    对"脸其实朝 ±Y"的模型永远读不出来（实测放走了 2026-11-08 那八支，整圈差 90°，而两道
    自检 + 14 项 validate + 十份指纹 + 546 条测试全绿）。

    本函数**不再是判据**，这是同日第二批实测出来的结论，四条候选全被否证：
      · 剪影左右不对称度 —— 镜像对称性是环旋转的不变量 ⇒ 原理上就钉不死。
      · 头部带色度方差 —— 护甲/背包/鬃毛比脸更花，五个模型里三到五个反号。
      · swing = headc 之差（亮度加权质心）—— 深色脸 / 亮后脑壳的模型符号整个反：spacecat
        −0.2386、robot@+90 −0.1155 都是**正确产物**，而它给 robot@−90（整圈差 180°）判了
        +0.0578 = 放行。
      · fb = 头部带梯度能量 col2/col6 之差 —— 四档相位判决矩阵 8 档判错 3 档：tiger
        −0.0083 与 horse −0.0005 把已签收的正确产物判红，dino@rot180（差 270°）+0.0086
        判绿。⇒ 它也不配当判据。
    ⇒ 朝向环的相位在本管线里**没有自动判据**，闸门改成人判词：`--facing-confirmed`（见
    main() 里那道"渲染之前先要判词"），它进 meta.json 随产物留档。这里只负责把四张图渲出来、
    把两条线索和那张 2×2 打到 out_dir。
    """
    probe = min(cell, FACING_PROBE_PX)
    keep = (scene.render.resolution_x, scene.render.resolution_y, scene.render.filepath)
    eevee = getattr(scene, "eevee", None)
    keep_samples = getattr(eevee, "taa_render_samples", None) if eevee is not None else None
    if eevee is not None and hasattr(eevee, "taa_render_samples"):
        eevee.taa_render_samples = FACING_PROBE_SAMPLES
    scene.render.resolution_x = probe
    scene.render.resolution_y = probe
    tmp = os.path.join("/tmp", "opet_facing_probe_%d" % os.getpid())
    os.makedirs(tmp, exist_ok=True)
    state = "idle" if "idle" in states else states[0]
    track = spec["states"][state]
    frames = len(track["rows"])
    pose(state, 0, frames, track.get("loop", True))
    bpy.context.view_layer.update()
    got = {}
    pngs = {}
    for index in (0, 2, 4, 6):
        place_camera(cam, index)
        path = os.path.join(tmp, "probe_%d.png" % index)
        scene.render.filepath = path
        bpy.ops.render.render(write_still=True)
        pngs[index] = path
        got[index] = _probe_image_metrics(path)
    scene.render.resolution_x, scene.render.resolution_y, scene.render.filepath = keep
    if eevee is not None and keep_samples is not None:
        eevee.taa_render_samples = keep_samples

    problems = []
    if any(v is None for v in got.values()):
        missing = [str(i) for i, v in got.items() if v is None]
        problems.append("朝向像素自检：col %s 的探针图全透明 ⇒ 判不了" % ",".join(missing))
        shutil.rmtree(tmp, ignore_errors=True)
        return problems, got
    for key in ("headc", "asym", "wide", "edge"):
        print(
            "BAKE   探针 %-5s = " % key
            + " ".join("col%d:%+.4f" % (i, got[i][key]) for i in (0, 2, 4, 6))
        )
    # 两条自动读数都只打印、都不判生死。它们各自被实测否证过：
    #   swing（headc 之差，按亮度加权）—— spacecat −0.2386、robot@+90 −0.1155 ⇒ 深色脸 /
    #     亮后脑壳的模型符号整个反号，它既假绿（robot@−90 +0.0578 放行过一次整圈差 180° 的产物）
    #     又假红（正确的 spacecat）。
    #   fb（头部带梯度能量 col2/col6 之差）—— 四档相位判决矩阵 8 档里判错 3 档：
    #     tiger −0.0083 与 horse −0.0005 是**已签收的正确产物**被判红，
    #     dino@rot180（= 差 270°）+0.0086 被判绿。
    # ⇒ 本管线里没有任何自动判据能定朝向环，2026-11-08 之前那句"fb 四档全对"是错的。
    # 真正的闸门 = 下面那张 2×2 出图 + `--facing-confirmed` 那句人判词（它进 meta）。
    fb = (got[2]["edge"] - got[6]["edge"]) / max(got[2]["edge"] + got[6]["edge"], 1e-9)
    swing = got[0]["headc"] - got[4]["headc"]
    print(
        "BAKE 朝向线索：fb(梯度能量 col2/col6) = %+.4f · swing(headc col0/col4) = %+.4f"
        " ⇒ 两条都只是线索，放行靠 --facing-confirmed" % (fb, swing)
    )
    got["fb"] = fb
    got["swing"] = swing

    # 这张是给人看的，不是装饰：四档相位没有任何一条自动判据抓得住（上面四条候选都被实测
    # 否证），而这四张探针图本来就已经渲了 ⇒ 拼一张 2×2 留在产物旁边，眼过它 + 一句
    # --facing-confirmed 判词 = 当前唯一的闸门。
    sheet = os.path.join(out_dir, "facing_probe_2x2.png")
    rc = -1
    if shutil.which("magick"):
        os.makedirs(out_dir, exist_ok=True)
        rc = subprocess.call(
            ["magick", "montage"]
            + [pngs[i] for i in (0, 2, 4, 6)]
            + ["-tile", "2x2", "-geometry", "+2+2", "-background", "#14161a", sheet]
        )
    shutil.rmtree(tmp, ignore_errors=True)
    if rc == 0:
        print(
            "BAKE 朝向 2×2 出图（col 0/2/4/6）= %s ⇒ 必须眼过：col 2 那张要正好是**脸正对镜头**"
            % sheet
        )
    else:
        print(
            "BAKE 朝向 2×2 出图生成失败（rc=%s，magick 不在 PATH？）⇒ 眼过不了 ⇒ 判词也无从给出" % rc
        )
    got["sheet"] = sheet if rc == 0 else None
    return problems, got


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
    if args.outfit_occlusion and not args.outfit_only:
        # 同一条理由第二次用：这一档改的是"配件图集里哪些像素留不留"，
        # 而载体本来就在画面里时那张图集不是配件图集，抹它没有意义（静默 no-op = 假开关）。
        sys.exit("--outfit-occlusion 必须和 --outfit --outfit-only 一起用：它抹的是**配件图集**里被载体挡住的像素，"
                 "不带 --outfit-only 时载体就在画面里，那一趟遮罩什么都改不了")
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
    nose_src = rear_src = "内置默认点（占位小白）"
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
            sys.exit("--outfit 必须和 --model 一起用：配件要刚性附着到 --outfit-attach-role 那根骨上（默认 head），"
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
        paint_outfit_flat(hat, args.outfit_color)
        check_attach(armature, roles, outfit_ctx, base, cam, cell, spec, pose, meshes)
        check_mirror_residual(armature, outfit_ctx, base)  # 没给 --outfit-mirror-to 时它自己早退
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
        geo = check_facing(cam)
        auto_points = not nose_src.startswith("显式") or not rear_src.startswith("显式")
        if geo and not auto_points:
            sys.exit("朝向几何自检失败：\n  " + "\n  ".join(geo))
        if geo:
            # 点是自动取的 ⇒ 这条测量本身是循环的（沿 +X 取极值再断言 +X 方向的屏幕偏移），
            # 实测它既假绿八支、又假红 dinosaur 的正确档 ⇒ 降级成线索，判红交给下面的像素自检。
            print("BAKE 朝向几何自检（自动取点 ⇒ 仅线索，不判红）：" + " / ".join(geo))
        else:
            print(
                "BAKE 朝向几何自检通过（点来源 %s/%s ⇒ %s）"
                % (
                    nose_src,
                    rear_src,
                    "显式给点 ⇒ 对前进轴有效"
                    if not auto_points
                    else "自动取点 ⇒ 对前进轴无效，以像素自检为准",
                )
            )
        problems, probe = check_facing_render(cam, scene, pose, states, spec, cell, args.out)
        if problems and args.outfit_only:
            # 配件图集那一档渲出来的**只有帽**，没有脸可当 landmark ⇒ 这条测量对它没有定义。
            # 不是"豁免就算过"：那一档的前进轴仍由显式 --nose/--rear 那条几何自检负责钉。
            print("BAKE 朝向像素自检跳过（--outfit-only 的探针图里没有脸，headc 无定义）：" + " / ".join(problems))
            problems = []
        if problems:
            sys.exit("朝向像素自检失败：\n  " + "\n  ".join(problems))
        if (
            not args.check_only
            and not args.outfit_only
            and not args.facing_confirmed
        ):
            # 这一步故意放在渲染 320 格**之前**：它不要钱，而它拦下的是"整批产物朝向错"这种
            # 六道门禁全看不见、只有眼睛能看见的事故（2026-11-08 那八支就是这么上池的）。
            sys.exit(
                "朝向环的相位**没有任何自动判据**（swing 在 spacecat/robot 上反号、fb 在四档相位"
                "矩阵上 8 档判错 3 档 ⇒ 两条现在都只是线索）。\n"
                "  眼过这张 2×2（col 0/2/4/6 应当 = 侧身鼻尖右 / 正脸 / 侧身鼻尖左 / 背面）：\n"
                "    %s\n"
                "  再带 --facing-confirmed '你的判词' 重跑；本轮线索：fb=%+.4f swing=%+.4f"
                % (
                    probe.get("sheet") or "<2×2 未生成（magick 不在 PATH？）>",
                    probe.get("fb", float("nan")),
                    probe.get("swing", float("nan")),
                )
            )
        if args.facing_confirmed and not args.outfit_only:
            print("BAKE 朝向人判词（进 meta）：%s" % args.facing_confirmed)

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

    occ_mask_path = os.path.join(args.out, "occ_mask.png")
    occ_totals = {"cells": 0, "kept": 0, "erased": 0, "rgb": [0.0, 0.0, 0.0], "empty": []}
    if args.outfit_occlusion:
        print(
            "BAKE --outfit-occlusion：每格多渲一趟『载体 + 品红配件』当遮罩（载体临时 hide_render=False，"
            "渲完藏回去；配件材质临时换掉，渲完还原；这一趟 taa=1 要的是边界不是好看）⇒ 成本约 ×2"
        )
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
                if args.outfit_occlusion:
                    apply_outfit_occlusion(path, occ_mask_path, [bpy.data.objects[n] for n in outfit_ctx["objects"]], meshes, occ_totals, args.outfit_occ_samples)

    if args.outfit_occlusion:
        if skipped:
            # 半批遮罩比整批没遮罩更坏：同一份图集里有些格挡过、有些没挡，而产物面读不出这件事
            sys.exit(
                "BAKE --outfit-occlusion：%d 格因为帧已存在而被跳过（没走遮挡那一趟）⇒ 这批图集会混着"
                "'挡过的'与'没挡过的'两种格。带 --force 重跑整批" % skipped
            )
        n = max(occ_totals["cells"], 1)
        r, g, b = (c / n for c in occ_totals["rgb"])
        if occ_totals["kept"] == 0 and occ_totals["erased"]:
            sys.exit(
                "BAKE 遮挡：%d 格配件像素，遮罩那一趟里**整批一个像素都没留下** ⇒ 这才是真病"
                "（临时材质没挂上，或这台机器的 view transform 把品红拉出了 `occ_front` 的锥形：R、B 都高于 G）。"
                "逐格为零不判红，因为位移小的档位里'配件整格躲到载体身后'是正确结果，而它正是档位梯要量的数"
                % occ_totals["erased"]
            )
        print(
            "BAKE 遮挡汇总：%d 格，保留 %d px、抹掉 %d px（= 全部配件像素的 %.1f%%）；"
            "遮罩判据实测：被认成'配件在最前'的像素平均 RGB = %.3f/%.3f/%.3f（品红应当 R≈B≫G，"
            "若这个关系不成立说明这台机器的 view transform 把品红拉出了 `occ_front` 的锥形）\n"
            "BAKE 遮挡档位读数：配件被整格挡没的格数 = %d/%d%s"
            % (
                occ_totals["cells"],
                occ_totals["kept"],
                occ_totals["erased"],
                100.0 * occ_totals["erased"] / max(occ_totals["kept"] + occ_totals["erased"], 1),
                r,
                g,
                b,
                len(occ_totals["empty"]),
                occ_totals["cells"],
                ("，前几个 = " + " ".join(os.path.relpath(p, args.out) for p in occ_totals["empty"][:4])) if occ_totals["empty"] else "",
            )
        )

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
        # 朝向环的相位没有自动判据 ⇒ 这批像素的朝向由这句人判词负责，它必须随产物留档
        "facing_verdict": args.facing_confirmed,
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
        if args.outfit_occlusion:
            # 一句出处：这批图集里"被身体挡住的那部分配件像素"是被抹掉的，不是没烘出来。
            # 它不进参数还原（遮挡不改几何、也不改相机），但没有它的话，以后看到"侧身那一格
            # 手套只剩一只"会先怀疑挂点，而正确答案是"那是遮挡"。
            meta["outfit_occlusion"] = True
            # 整格挡没的格名也留档：产物面读到"run/f00_06 那一格配件是全透明"时，唯一能把它和
            # "挂点漂了 / 那格没烘"分开的就是这张名单（批末那条读数只打进 stdout，而 scratch 会被清）
            meta["outfit_occlusion_empty"] = [
                os.path.relpath(p, args.out) for p in occ_totals["empty"]
            ]
        meta["outfit_objects"] = [n for n in (args.outfit_objects or "").split(",") if n.strip()]
        meta["outfit_scale"] = outfit_ctx["scale"]
        # 这四个都直接改像素，而`--outfit-scale`之外那三个原先没人记 ⇒ 一批"@192 带帽、倾 25°"的图
        # 在 meta.json 里读起来和"不倾"的那批一模一样。outfit.json 只有测量轮才写，像素批次
        # 的出处只有这一份 ⇒ 补在这里。
        for k in ("rot_z", "tilt", "lean", "seat"):
            meta["outfit_" + k] = outfit_ctx[k]
        # 这三条决定"这批像素上配件绑在哪根骨、有几件、按什么定标"，而 `--outfit-only` 的产物
        # 只有这一份出处（outfit.json 仅测量轮写）⇒ 不记就等于以后没法解释"两只靴子从哪来"。
        meta["outfit_attach_role"] = outfit_ctx["attach_role"]
        meta["outfit_mirror_to"] = outfit_ctx["mirror_role"]
        meta["outfit_scale_ref"] = outfit_ctx["scale_ref"]
        # 平移和定标一样直接改像素（靴筒压在小腿上是这个数、不是缩放），而它同样只在 meta 里有出处
        meta["outfit_offset"] = outfit_ctx["offset"]
        meta["outfit_scale_z"] = outfit_ctx["scale_z"]
        meta["outfit_instances"] = [
            {"bone": i["bone"], "role": i["role"], "objects": i["objects"]} for i in outfit_ctx["instances"]
        ]
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
