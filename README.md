# OPet

3D 卡通桌面宠物（Blender 建模 → 预渲染 8 方向序列帧）。目标平台优先级：**Omarchy OS（Hyprland/Wayland）> Windows > macOS**。

## 状态

行为内核 `petcore` 已完成第一个里程碑：状态机 + 物理 + 世界模型（多屏 / 平台 / 墙接触）+ 精灵图集帧调度，固定 60 Hz 步进、带种子确定性随机、纯逻辑无窗口依赖。资产烘焙管线（Blender → WebP 图集）首批落地。Wayland layer-shell 客户端首批已在真机跑通：图集解码 + shm buffer + 逐格点击穿透 + 指针拖拽 + 1.5× 分数缩放。Win32 / NSWindow 客户端待开工。

**开箱 UI（M3-b 首批）已落地并在真机跑通**：`PointerEvent` 现在带按钮维度 ⇒ **右键宠物**开出第二块 layer-shell surface（独立 namespace、独立 buffer 池与帧看门狗，回调按代理对象身份分流）⇒ 菜单里点"抽一次 / 十连"**真的抽一次并写账本**，同块 surface 播稀有度 burst 与结果卡；宠物本体复用既有的双击 react 动画，**没有新增 Command**。三条设计上的老实话：**"点菜单外部关闭"在 layer-shell 的 overlay 层 + `keyboard_interactivity=None` 上做不到**（拿不到失焦），所以关闭只有菜单内 CLOSE 项与再右键；**不带 `--db` 就是内存库并在卡片上打 `DRYRUN`**，连库惰性到第一次点击（跑多久都不会碰你的真存档）；音效 / 概率公示浮层 / `opet.sock` IPC 尚未做。

盲盒与数据（M3）已落地 **headless 全套 + CLI**：全整数抽样引擎（百万分位，无浮点）、软保底 + 硬保底 + 十连必含 SR + 重复折算碎片，存档与审计链在 `crates/petstore`。**概率公示文件由引擎自己算出来**（马尔可夫链精确期望，不掷骰），100 万次真掷骰与它的偏差 worst 345 / 1500 百万分位、χ² 1.9746（限 7.815）——"公示说的数字 == 代码产出的数字"由单测逐字段钉住。抽取记录构成哈希链，第三方**只需导出的 JSONL + seed** 即可重放验证（不碰数据库）。**随机与付费在结构上隔离**：`gacha_banner.currency` 的 DB CHECK 只允许 `companion_time`/`shards`，`Currency` 枚举没有真钱变体，内容包类型没有权重字段，CLI 的 `--currency` 一律 `SCHEMA_ERROR`。概率公示浮层与 `opet.sock` IPC 尚未做（开箱动画与结果卡已由上面的 M3-b 交付）。

## 仓库结构

```
crates/petcore/            行为内核 + 抽样引擎（平台无关，无窗口依赖、无 IO、无墙钟）
  src/gacha/               盲盒：参数/骰子流/保底/目录/审计链/公示生成（纯函数）
  assets/gacha/rates.json  概率公示产物（`opet catalog rates` 生成，不手写）
crates/petstore/           存档：SQLite schema + 迁移 + 账本 + 墙钟 + JSONL 导出
crates/opet-cli/           命令行入口：headless 模拟、吞吐基准、迁移表、抽样门禁、抽取与审计
crates/overlay-wayland/    Wayland layer-shell 客户端（宠物上屏 + 点击穿透 + 右键菜单/结果卡）
  menu.rs / font.rs / card.rs  菜单布局与命中 / 5×7 ASCII 位图 / 卡片像素（三者都不碰 wayland 类型）
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

./target/release/opet overlay --manifest FILE --db /path/opet.db --seconds 120 --panel-top 20
                                                 # 右键宠物 → 菜单（抽一次/十连/关闭 + 余额行）。
                                                 # 不给 --db 就走内存库并在卡片上打 DRYRUN；连库惰性
                                                 # 到第一次点击，所以不点击就不建文件、不写盘
./target/release/opet audit verify --db /path/opet.db --account local
                                                 # 抽完自己复核账本链（第三方走 --file + --seed）

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
