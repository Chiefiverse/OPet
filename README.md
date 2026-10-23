# OPet

3D 卡通桌面宠物（Blender 建模 → 预渲染 8 方向序列帧）。目标平台优先级：**Omarchy OS（Hyprland/Wayland）> Windows > macOS**。

## 状态

行为内核 `petcore` 已完成第一版里程碑：状态机 + 物理 + 世界模型（多屏 / 平台 / 墙接触）+ 精灵图集帧调度，固定 60 Hz 步进、带种子确定性随机、纯逻辑无窗口依赖。平台 overlay 客户端与资产烘焙管线进行中。

## 仓库结构

```
crates/petcore/       行为内核（平台无关，无窗口依赖）
crates/opet-cli/      命令行入口：headless 模拟、吞吐基准、迁移表生成
scripts/              真机验收清单
```

## 构建与验证

```sh
cargo test --workspace
cargo clippy --all-targets
cargo build --release

./target/release/opet sim headless --hours 8     # 长时模拟，逐 tick 校验不变量
./target/release/opet sim bench --ticks 600000   # 纯逻辑吞吐
./target/release/opet sim table                  # 行为状态机迁移表
```

## 许可

- 代码：**AGPL-3.0-only**（见 [`LICENSE`](LICENSE)）。
- 官方美术资产与付费内容包：**专有**，以 `.petpack` 资产包分发，不在本仓库内。
