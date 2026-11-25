# OPet

3D 卡通桌面宠物（Blender 建模 → 预渲染 8 方向序列帧）。目标平台优先级：**Omarchy OS（Hyprland/Wayland）> Windows > macOS**。

## 状态

行为内核 `petcore` 已完成第一个里程碑：状态机 + 物理 + 世界模型（多屏 / 平台 / 墙接触）+ 精灵图集帧调度，固定 60 Hz 步进、带种子确定性随机、纯逻辑无窗口依赖。资产烘焙管线（Blender → WebP 图集）首批落地。Wayland layer-shell 客户端首批已在真机跑通：图集解码 + shm buffer + 逐格点击穿透 + 指针拖拽 + 分数缩放。**Windows 客户端（M5）已能本机交叉打包并在 Wine 上过九轮验收脚本**：独立入口 `opet-overlay-win32.exe`（`WS_EX_LAYERED` + `UpdateLayeredWindow` 单面事件环，穿透 = `WM_NCHITTEST` 逐像素 `HTTRANSPARENT`），菜单 / 换装 / 抽卡卡面 / 收藏面板与 `--db/--lang/--motion/--probe` 四开关都已转真；绘制层与报表层与 Wayland 家共用（`overlay-common` / `overlay-report`，零逻辑分叉）。**真机第一轮（Win11，scale 1.25）已跑**：DPI 路线与内存口径结案，六条反馈全部定性（其中"舞台比屏幕小一圈"= 真机才暴露的 DPI 感知时序 bug，已修：显示器枚举移到感知生效之后；`--probe` 走线程级感知）；测试包改为**整库入库**（本体 + 配件全带，`run.bat` 自带库根与豁免旗，换装/换本体有判卷段）。⚠️ 托盘 / NSIS 安装包 / `opet ctl` / 命中滞回在第三批，★ 目视判词与修复复核格仍在真机回报表上（Wine 读数不等于 Win11 判据）。NSWindow（macOS，M6）待开工。

**开箱 UI（M3-b 首批）已落地并在真机跑通**：`PointerEvent` 现在带按钮维度 ⇒ **右键宠物**开出第二块 layer-shell surface（独立 namespace、独立 buffer 池与帧看门狗，回调按代理对象身份分流）⇒ 菜单里点"抽一次 / 十连"**真的抽一次并写账本**，同块 surface 播稀有度 burst 与结果卡；宠物本体复用既有的双击 react 动画，**没有新增 Command**。三条设计上的老实话：**"点菜单外部关闭"在 layer-shell 的 overlay 层 + `keyboard_interactivity=None` 上做不到**（拿不到失焦），所以关闭只有菜单内 CLOSE 项与再右键；**不带 `--db` 就是内存库并在卡片上打 `DRYRUN`**，连库惰性到第一次点击（跑多久都不会碰你的真存档）；音效 / 概率公示浮层 / `opet.sock` IPC 尚未做。

盲盒与数据（M3）已落地 **headless 全套 + CLI**：全整数抽样引擎（百万分位，无浮点）、软保底 + 硬保底 + 十连必含 SR + 重复折算碎片，存档与审计链在 `crates/petstore`。**概率公示文件由引擎自己算出来**（马尔可夫链精确期望，不掷骰），100 万次真掷骰与它的偏差 worst 345 / 1500 百万分位、χ² 1.9746（限 7.815）——"公示说的数字 == 代码产出的数字"由单测逐字段钉住。抽取记录构成哈希链，第三方**只需导出的 JSONL + seed** 即可重放验证（不碰数据库）。**随机与付费在结构上隔离**：`gacha_banner.currency` 的 DB CHECK 只允许 `companion_time`/`shards`，`Currency` 枚举没有真钱变体，内容包类型没有权重字段，CLI 的 `--currency` 一律 `SCHEMA_ERROR`。概率公示浮层与 `opet.sock` IPC 尚未做（开箱动画与结果卡已由上面的 M3-b 交付）。

**菜单与卡片的文字换成了真字体，并有四套语言**（UI 视觉重做 V2）：`ab_glyph` + **入库的最小字体子集**（`assets/fonts/ui.otf`，35 KB，覆盖 ASCII + 简中 + 假名 + 谚文实际用到的 237 个码位）⇒ 抽到「蓝帽」时卡上打的是**中文名**而不是 `cap_blue`。**界面语言用户自己能设**：`--lang zh|en|ja|ko` > `~/.config/opet/config.toml` 的 `[ui] lang` > 环境变量 `LC_ALL`/`LANG` > 默认中文，**菜单底部那一行"语言 中文"点一下就循环**（只在话内切换，客户端不回写你的配置文件）。**动画强度可关**（前庭敏感）：`--motion full|reduce|off`（或 `[ui] motion`），`off` 关掉全部 burst 与闪烁，SSR 那一帧白闪的上限也从"整卡过白"降到 85% 亮度。**文字可读性不再是观感而是数字**：任何会被当成文字底色的色块都要有一支 ≥ 4.5:1 的墨，由单测逐档 × 逐语言 × 逐底色遍历（含一条"白字压金底必须不合格"的反面对照）。⚠️ **旧的 5×7 ASCII 点阵留着只作兜底**（字体子集万一解析失败才走它，非 ASCII 会画 `?`）。

## 仓库结构

```
crates/petcore/            行为内核 + 抽样引擎（平台无关，无窗口依赖、无 IO、无墙钟）
  src/gacha/               盲盒：参数/骰子流/保底/目录/审计链/公示生成（纯函数）
  assets/gacha/rates.json  概率公示产物（`opet catalog rates` 生成，不手写）
crates/petstore/           存档：SQLite schema + 迁移 + 账本 + 墙钟 + JSONL 导出
                           + config.rs：自写的平坦 `[ui]` reader（零新依赖，只读不回写）
crates/opet-cli/           命令行入口：headless 模拟、吞吐基准、迁移表、抽样门禁、抽取与审计
crates/overlay-common/     两家客户端共用的绘制层（零窗口、零 IO、零平台类型）
  ui/menu.rs / ui/card.rs  菜单布局·命中·相位 / 卡片像素（都不碰窗口类型 ⇒ 可 headless 出图）
  ui/panel.rs              收藏面板行 / 分页 / 命中（与菜单互斥只在客户端那一处门）
  ui/text.rs / ui/l10n.rs  真字体光栅与字号档 / zh·en·ja·ko 四套文案表
  ui/font.rs               5×7 ASCII 位图，**仅当字体子集解析失败时的兜底路径**
  ui/settings.rs           `--lang`/`--motion` > config.toml > LC_ALL/LANG > 默认 的唯一裁决链
  assets/fonts/            ui.otf 子集 + ui-codepoints.txt（码位单一事实源）+ OFL 许可证与声明
crates/overlay-report/     两家共用的报表层（连库 + 目录 + 收藏册 + 概率表，零像素 / 零墙钟）
crates/overlay-wayland/    Wayland layer-shell 客户端（宠物上屏 + 点击穿透 + 右键菜单/结果卡）
  settings.rs               unix 链装载面（`$XDG_CONFIG_HOME` → `~/.config`）
crates/overlay-win32/      Win32 分层窗口客户端（独立 exe；`opet` 总入口为 windows-gnu 编不过）
  probe.rs / settings.rs   `--probe` 四节 / Windows 三档阶梯 `%LOCALAPPDATA%` → `%APPDATA%` → `%USERPROFILE%`
scripts/                   真机验收清单（`test_windows_wine.sh` = Windows 的 Wine 九轮）
shell-widget/              Omarchy 入口插件（manifest + Service.qml 拥有宠物进程生命周期 + 顶栏图标与四档弹卡，走 `--ipc`）
                           ⚠️ 这里是唯一事实源；发布 = 摊平到独立仓（Omarchy 只认"仓库根 = 插件根"），见下面 publish_plugin.sh
tools/                     bake.py / pack.py / validate.py / subset_font.py（子集重建与自检）
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

./target/release/opet overlay --manifest FILE --seconds 120 --panel-top 20 --lang en --motion off
                                                 # 语言与动画强度也可给配置文件（优先级如下）：
                                                 #   --lang/--motion > ~/.config/opet/config.toml
                                                 #   > 环境变量 LC_ALL/LANG > 默认 zh / full
                                                 # config.toml：
                                                 #   [ui]
                                                 #   lang = "ja"        # zh|en|ja|ko
                                                 #   motion = "reduce"  # full|reduce|off
                                                 # 菜单底部那行语言点一下就地循环（不写回文件）

OPET_CARD_DUMP=1 cargo test -p overlay-common --lib -- card::tests::dump_each_phase_for_eyeball --ignored --nocapture
                                                 # 把菜单与结果卡的每个相位出成 PPM 给人逐张过眼
                                                 # （208 张：相位 × 4 语言 × 3 动画档 × 两档缩放，
                                                 #   文件名 /tmp/opet_card_<相位>_<locale>_<motion>_s<scale>.ppm，
                                                 #   原生物理像素，放大请在转换时做）
python3 tools/subset_font.py --verify            # 自检入库子集与码位表是否仍然互相覆盖（缺字即红）

./target/release/opet gacha-sim --pulls 1000000 --seed 1
                                                 # 概率门禁：实测 vs 公示，末行 GATE … result=PASS/FAIL
./target/release/opet catalog rates              # 由引擎生成概率公示（含逐件概率）
./target/release/opet pull --count 10 --dry-run  # 打印概率与 pity，不写盘、不扣币
./target/release/opet audit export --out audit.jsonl
./target/release/opet audit verify --file audit.jsonl --seed S
                                                 # 第三方重放：只吃这个文件 + seed
                                                 # 要证明"没被删尾行"必须加 --expect-head HEX

bash scripts/build_windows.sh                    # 本机交叉编译出 Windows 包（预检 + 六步 + zip/CRC 自校），
                                                 # 产物摊在 dist/opet-win32/（run.bat 4 条 run + 34 行回报表）
bash scripts/test_windows_wine.sh                # Windows 客户端的 Wine 验收九轮（62 项判据；
                                                 # ⚠️ Wine 读数 ≠ Win11 判据，目视格在回报表的 ★ 列）

bash scripts/publish_plugin.sh --verify          # Omarchy 插件仓防漂移门禁：产物与 shell-widget/
                                                 # 不一致、或有提交没推 ⇒ exit 1（默认档只摊平，不碰网络）
omarchy plugin add https://github.com/Chiefiverse/OPetPlugIn.git --enable
                                                 # 别人怎么装入口插件（要 Omarchy 4+）；装完仍需在
                                                 # Setup → Plugins 填 opet 可执行文件与 pet.json
```

## 许可

- 代码：**AGPL-3.0-only**（见 [`LICENSE`](LICENSE)）。
- 官方美术资产与付费内容包：**专有**，以 `.petpack` 资产包分发，不在本仓库内。
- UI 字体子集（`crates/overlay-common/assets/fonts/ui.otf`）：**SIL Open Font License 1.1**，随包带 [`LICENSE-ui-font.txt`](crates/overlay-common/assets/fonts/LICENSE-ui-font.txt) 与 [`NOTICE-ui-font.txt`](crates/overlay-common/assets/fonts/NOTICE-ui-font.txt)。它由 Noto Sans CJK 子集化而来，按随包许可证文本**第 3 条**（"No Modified Version … may use the Reserved Font Name"）的要求**家族名已改为 `OPet UI`**（不以上游名义分发），许可条款不与 AGPL 合并。
