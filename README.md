# OPet

3D 卡通桌面宠物（Blender 建模 → 预渲染 8 方向序列帧）。目标平台优先级：**Omarchy OS（Hyprland/Wayland）> Windows > macOS**。

## 状态

行为内核 `petcore` 已完成第一个里程碑：状态机 + 物理 + 世界模型（多屏 / 平台 / 墙接触）+ 精灵图集帧调度，固定 60 Hz 步进、带种子确定性随机、纯逻辑无窗口依赖。资产烘焙管线（Blender → WebP 图集）首批落地。Wayland layer-shell 客户端首批已在真机跑通：图集解码 + shm buffer + 逐格点击穿透 + 指针拖拽 + 1.5× 分数缩放；右键菜单、Win32 / NSWindow 客户端待开工。

盲盒与数据（M3）已落地 **headless 全套 + CLI**：全整数抽样引擎（百万分位，无浮点）、软保底 + 硬保底 + 十连必含 SR + 重复折算碎片，存档与审计链在 `crates/petstore`。**概率公示文件由引擎自己算出来**（马尔可夫链精确期望，不掷骰），100 万次真掷骰与它的偏差 worst 345 / 1500 百万分位、χ² 1.9746（限 7.815）——"公示说的数字 == 代码产出的数字"由单测逐字段钉住。抽取记录构成哈希链，第三方**只需导出的 JSONL + seed** 即可重放验证（不碰数据库）。**随机与付费在结构上隔离**：`gacha_banner.currency` 的 DB CHECK 只允许 `companion_time`/`shards`，`Currency` 枚举没有真钱变体，内容包类型没有权重字段，CLI 的 `--currency` 一律 `SCHEMA_ERROR`。开箱动画 / 概率公示浮层 / IPC 尚未做。

## 仓库结构

```
crates/petcore/            行为内核 + 抽样引擎（平台无关，无窗口依赖、无 IO、无墙钟）
  src/gacha/               盲盒：参数/骰子流/保底/目录/审计链/公示生成（纯函数）
  assets/gacha/rates.json  概率公示产物（`opet catalog rates` 生成，不手写）
crates/petstore/           存档：SQLite schema + 迁移 + 账本 + 墙钟 + JSONL 导出
crates/opet-cli/           命令行入口：headless 模拟、吞吐基准、迁移表、抽样门禁、抽取与审计
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

./target/release/opet gacha-sim --pulls 1000000 --seed 1
                                                 # 概率门禁：实测 vs 公示，末行 GATE … result=PASS/FAIL
./target/release/opet catalog rates              # 由引擎生成概率公示（含逐件概率）
./target/release/opet pull --count 10 --dry-run  # 打印概率与 pity，不写盘、不扣币
./target/release/opet audit export --out audit.jsonl
./target/release/opet audit verify --file audit.jsonl --seed S
                                                 # 第三方重放：只吃这个文件 + seed
                                                 # 要证明"没被删尾行"必须加 --expect-head HEX
```

## 许可

- 代码：**AGPL-3.0-only**（见 [`LICENSE`](LICENSE)）。
- 官方美术资产与付费内容包：**专有**，以 `.petpack` 资产包分发，不在本仓库内。
