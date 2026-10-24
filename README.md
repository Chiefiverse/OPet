# OPet

3D 卡通桌面宠物（Blender 建模 → 预渲染 8 方向序列帧）。目标平台优先级：**Omarchy OS（Hyprland/Wayland）> Windows > macOS**。

## 状态

行为内核 `petcore` 已完成第一个里程碑：状态机 + 物理 + 世界模型（多屏 / 平台 / 墙接触）+ 精灵图集帧调度，固定 60 Hz 步进、带种子确定性随机、纯逻辑无窗口依赖。资产烘焙管线（Blender → WebP 图集）首批落地。Wayland layer-shell 客户端首批已在真机跑通：图集解码 + shm buffer + 逐格点击穿透 + 指针拖拽 + 1.5× 分数缩放；右键菜单、Win32 / NSWindow 客户端待开工。

## 仓库结构

```
crates/petcore/            行为内核（平台无关，无窗口依赖）
crates/opet-cli/           命令行入口：headless 模拟、吞吐基准、迁移表生成
crates/overlay-wayland/    Wayland layer-shell 客户端（宠物上屏 + 点击穿透）
scripts/                   真机验收清单
```

## 构建与验证

```sh
cargo test --workspace
cargo clippy --all-targets
cargo build --release

./target/release/opet sim headless --hours 8     # 长时模拟，逐 tick 校验不变量
./target/release/opet sim bench --ticks 600000   # 纯逻辑吞吐
./target/release/opet sim table                  # 行为状态机迁移表

./target/release/opet overlay --probe            # 只读打印 registry 与显示器几何，不映射 surface
./target/release/opet overlay --manifest FILE --seconds 30 --panel-top 20
                                                 # 前台起宠物；顶部有栏时必须给 --panel-top，
                                                 # 否则 margin 从可用区量起会把宠物整体下推
```

## 许可

- 代码：**AGPL-3.0-only**（见 [`LICENSE`](LICENSE)）。
- 官方美术资产与付费内容包：**专有**，以 `.petpack` 资产包分发，不在本仓库内。
