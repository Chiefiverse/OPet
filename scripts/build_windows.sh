#!/usr/bin/env bash
# M5 第一批（设计书 §3.7 / §6.1.25）—— 在 Linux 上交叉打包 Windows 测试产物。
#
# 一条命令做完五件事，产出可以整个拷到 Windows 11 机器上的目录：
#   1. 宿主门禁（fmt + 全量测试 + clippy）—— 打包脚本不该是第一个发现回归的地方；
#   2. 交叉 clippy + 交叉测试**编译**（--no-run）；
#   3. 交叉 release 构建 `opet-overlay-win32.exe`；
#   4. exe 的 DLL 导入表对白名单核一遍（判据在下面，多一个名字就让构建失败）；
#   5. 摊开 dist/opet-win32/ = exe + 资产三件套 + README（含报告表）+ run.bat（CRLF）；
#   6. 打成单个 zip（带 CRC 自检 + sha256）—— 要跨机器拷的东西，目录树少一个文件不会报错。
#
# 用法：
#   bash scripts/build_windows.sh                 # 完整流程（默认本体 = assets/pets/tiger）
#   PET=spacecat bash scripts/build_windows.sh    # 换一份本体
#   bash scripts/build_windows.sh --quick         # 跳过宿主门禁（只交叉 + 打包）
#   bash scripts/build_windows.sh --clean         # 删掉产物目录与该 target 的构建目录后退出
#
# ⚠️ 三条硬边界：
#   * **本机的 Windows 面 = Wine 11.22**（`bash scripts/test_windows_wine.sh` 九轮全判据化）。
#     交叉这几层证明"能编、能链、导入表干净"，Wine 那一套证明"逻辑在真 Win32 API 上自洽"；
#     但 **Wine 读数不是 Win11 判据**（D5）⇒ 屏上对不对、内存线、DPI 档位只有第 10 步用户
#     那台真机回答，README 里那张报告表就是为这一格写的。
#   * 产物目录**必须**是 git-ignored：里面装的是烘焙产物（sheet.webp 3.4 MB + polygon.json
#     476 KB）。`.gitignore` 现在靠 `/dist/` 挡住它 ⇒ 脚本每次核一遍，挡不住就失败退出，
#     不"只警告"（一次 `git add .` 就是把 3.4 MB 提交进公开仓库）。
#   * exe **链着 `petstore`**（M0 2026-11-22 证伪了"bundled SQLite 编不过"的担心；第一批那句
#     "不带 petstore 依赖"已作废）⇒ `--db` / 抽卡 / 库存 / 面板在 Windows 上有真入口。
#     代价已实测并登记在旁边那张 DLL 白名单（14 → 16 名，出处 = `libsqlite3.a` 的两个 UCRT 符号）。

set -uo pipefail
cd "$(dirname "$0")/.." || exit 1
ROOT="$PWD"

TARGET="x86_64-pc-windows-gnu"
BIN="opet-overlay-win32"
LINKER="x86_64-w64-mingw32-gcc"
OUT="dist/opet-win32"
PET="${PET:-tiger}"
QUICK=0
DO_CLEAN=0

# ── DLL 白名单：这是判据，不是注释 ───────────────────────────────────────
#
# 出处 = 2026-11-22 第 8 步完成后**实测重拿**的 release 产物（`objdump -p`），不是从初值推的。
# 第 5 步那份 11 个名字的初值（`.cargo/config.toml` 旁边留盘的 scratch/win32_dlls_step5_*.txt）
# 到这里已经旧了：`window.rs` / `monitor.rs` 真调起 GDI 与 user32 ⇒ 多 2 个，std 那条随机数线
# 多 1 个（bcryptprimitives）。这正是要重测的原因 —— 拿旧表当判据，第 4 步就白跑了。
#
# ⚠️ **2026-11-23 M5e 重跑到 16 个**（第二批第一次跑全流程时第 5 步真的红了，红的是**结果**而不是
# 病）：M4 把 `petstore` 链进来 ⇒ `rusqlite/bundled` 那份 C 合并文件（`libsqlite3.a`）的两个未定义
# 符号要人回答，实测出处 =
#   `x86_64-w64-mingw32-nm -u …/release/build/libsqlite3-sys-*/out/libsqlite3.a`
#     U _localtime64   ⇒ api-ms-win-crt-time-l1-1-0.dll
#     U qsort          ⇒ api-ms-win-crt-utility-l1-1-0.dll
#   （同一份 objdump 的成员名表里这两个符号各只出现一次，别处的 C 代码没有第三个消费者。）
# 两个都是 **UCRT 的 api set**，Win10 起随系统 ⇒ 与 `libwinpthread` 那一族性质相反（那两个是
# 交叉链的私货，这两个是 C 运行时的分模块）。**换依赖 / 换工具链必须重拿这张表**，见下面第 5 步。
#
# 为什么这张表必须存在：交叉链最容易带进来的私货是 `libwinpthread-1.dll` 与
# `libgcc_s_seh-1.dll`。用户机器上没有这两个名字，缺 DLL 的 PE 在 `main` 之前就被加载器拒掉，
# 症状是一句"找不到 xxx.dll"而没有任何一行我们的输出 —— 那一轮会 entirely 丢掉证据。
ALLOWED_DLLS=(
  "api-ms-win-core-synch-l1-2-0.dll"
  "api-ms-win-crt-environment-l1-1-0.dll"
  "api-ms-win-crt-heap-l1-1-0.dll"
  "api-ms-win-crt-locale-l1-1-0.dll"
  "api-ms-win-crt-math-l1-1-0.dll"
  "api-ms-win-crt-private-l1-1-0.dll"
  "api-ms-win-crt-runtime-l1-1-0.dll"
  "api-ms-win-crt-stdio-l1-1-0.dll"
  "api-ms-win-crt-string-l1-1-0.dll"
  "api-ms-win-crt-time-l1-1-0.dll"    # sqlite3.c 的 _localtime64（M4 接线带来，出处见上面）
  "api-ms-win-crt-utility-l1-1-0.dll" # sqlite3.c 的 qsort（同上）
  "bcryptprimitives.dll" # 种子的随机来源（Windows 8.1 起随系统）
  "gdi32.dll"            # 显示器几何那一族（EnumDisplayMonitors / GetMonitorInfoW 侧）
  "kernel32.dll"
  "ntdll.dll"
  "user32.dll"           # 分层窗口本体：CreateWindowExW / UpdateLayeredWindow / 消息与捕获
)

# 这三个名字**只许缺席**，单列一遍是为了让失败消息直接说出病名，
# 而不是让人去 14 行里找"哪一行多出来了"。
BANNED_DLLS=(
  "libwinpthread-1.dll"
  "libgcc_s_seh-1.dll"
  "libgcc_s_dw2-1.dll"
)

for arg in "$@"; do
  case "$arg" in
    --quick) QUICK=1 ;;
    --clean) DO_CLEAN=1 ;;
    -h | --help)
      sed -n '1,33p' "$0"
      exit 0
      ;;
    *)
      echo "未知参数：$arg（可用：--quick --clean -h）" >&2
      exit 2
      ;;
  esac
done

step() { echo "== $*"; }
fail() {
  echo "❌ $*" >&2
  exit 1
}

if [ "$DO_CLEAN" = 1 ]; then
  step "清理 $OUT 与 target/$TARGET"
  rm -rf "$OUT" "target/$TARGET" || fail "清理失败（路径被谁占着？）"
  echo "✅ 已清理，未构建任何东西"
  exit 0
fi

# ── 0/6 前置条件 ─────────────────────────────────────────────────────────
step "0/6 前置条件"
command -v cargo >/dev/null || fail "PATH 里没有 cargo"
command -v "$LINKER" >/dev/null ||
  fail "找不到 $LINKER ⇒ mingw-w64 没装（.cargo/config.toml 里那条 linker 就是为这个报错点名的）。装法以当时的 pacman -F 为准。"

OBJDUMP=""
for cand in x86_64-w64-mingw32-objdump llvm-objdump objdump; do
  if command -v "$cand" >/dev/null 2>&1; then
    OBJDUMP="$cand"
    break
  fi
done
[ -n "$OBJDUMP" ] || fail "没有 objdump（试过 x86_64-w64-mingw32-objdump / llvm-objdump / objdump）⇒ 白名单那一层没法判"
echo "   objdump = $OBJDUMP"

# petstore 的 C 交叉编译（rusqlite/bundled）是第二批起的硬前置。这里**不真编**（第 2–4 步
# 就是它的真 compiler-invocation 证明），只把"诊断工具在场"提前到第 0 步：白名单红的时候第一
# 问句是"哪个符号是谁带进来的"，拿不到 nm 就只能整张表重猜（M5e 那次的出处行就是这条命令）。
command -v x86_64-w64-mingw32-nm >/dev/null ||
  fail "找不到 x86_64-w64-mingw32-nm ⇒ 第 5 步白名单若红将无法点名新符号的出处（binutils 没装齐）"

rustc --print target-list 2>/dev/null | grep -qx "$TARGET" ||
  fail "rustc 不认识 target $TARGET（工具链太旧？）"
if ! rustup target list --installed 2>/dev/null | grep -qx "$TARGET"; then
  step "target $TARGET 未安装 ⇒ 装（带 rsproxy 镜像；官方源实测 11–26 KB/s）"
  RUSTUP_DIST_SERVER=https://rsproxy.cn RUSTUP_UPDATE_ROOT=https://rsproxy.cn/rustup \
    rustup target add "$TARGET" || fail "rustup target add $TARGET 失败"
fi

for f in pet.json sheet.webp polygon.json; do
  [ -f "assets/pets/$PET/$f" ] || fail "assets/pets/$PET/$f 不存在 ⇒ 用 PET=<id> 换一份本体"
done

# 门禁日志一律留在这里（不在 /tmp、不删）：报出来的总数必须能被二次核对，见下面那一行注释。
LOG_DIR="$HOME/.local/share/opet/scratch"
mkdir -p "$LOG_DIR" || fail "建不出 $LOG_DIR"

# ── 1/6 宿主门禁 ────────────────────────────────────────────────────────
if [ "$QUICK" = 0 ]; then
  step "1/6 宿主门禁：fmt / test / clippy"
  cargo fmt --check || fail "fmt 不干净（先 cargo fmt）"

  # 测试日志**留盘**、不删：本轮报的是 `637 passed` 这种"逐行相加"的总数，而总数没有
  # 二次核对能力——只有留着 `Running <target>` 与 `test result:` 成对出现的原文，
  # 下一轮才能核实那些数是不是同一轮量出来的（AGENTS 现场事实第 5 条同一条纪律）。
  LOG="$LOG_DIR/host_test_$(date -Is).log"
  cargo test --workspace >"$LOG" 2>&1
  [ $? -eq 0 ] || {
    tail -40 "$LOG" >&2
    fail "宿主测试红了（上面是尾部；完整日志 = $LOG）"
  }
  grep '^test result:' "$LOG" | sed 's/^/   /'
  echo "   合计 passed=$(grep '^test result:' "$LOG" | awk '{s+=$4} END {print s}') / failed=$(grep '^test result:' "$LOG" | awk '{s+=$6} END {print s}') / ignored=$(grep '^test result:' "$LOG" | awk '{s+=$8} END {print s}')（逐行相加；原文 = $LOG）"
  CLOG="$LOG_DIR/host_clippy_$(date -Is).log"
  cargo clippy --all-targets >"$CLOG" 2>&1
  [ $? -eq 0 ] || {
    tail -20 "$CLOG" >&2
    fail "宿主 clippy 构建失败（完整日志 = $CLOG）"
  }
  if grep -q '^warning' "$CLOG"; then
    grep '^warning' "$CLOG" | head -5
    fail "宿主 clippy 有警告 ⇒ 零警告口径破了（完整日志 = $CLOG。⚠️ 判据是 `^warning` 的行数，**不是** cargo 末行那句 `Finished`：重新检查时警告不落屏，Finished 照样打）"
  fi
  echo "   ✅ 宿主 clippy 0 warning（原文 = $CLOG）"
else
  step "1/6 跳过宿主门禁（--quick）"
fi

# ── 2/6 交叉构建 ────────────────────────────────────────────────────────
step "2/6 交叉 clippy（$TARGET）"
# ⚠️ 退出码不算判据：clippy **带着警告也退 0**，而且 cargo 缓存命中时末行照打
# `Finished` ⇒ "看到 Finished 就当干净"是这条门禁最容易犯的假绿。判据 = `^warning` 行数。
XLOG="$LOG_DIR/cross_clippy_$(date -Is).log"
cargo clippy -p overlay-win32 --target "$TARGET" --all-targets >"$XLOG" 2>&1
[ $? -eq 0 ] || {
  tail -30 "$XLOG" >&2
  fail "交叉 clippy 构建失败（完整日志 = $XLOG）"
}
if grep -q '^warning' "$XLOG"; then
  grep '^warning' "$XLOG" | head -5
  fail "交叉 clippy 有警告 ⇒ 零警告口径破了（完整日志 = $XLOG）"
fi
echo "   ✅ 交叉 clippy 0 warning（原文 = $XLOG）"

step "3/6 交叉测试**编译**（--no-run）"
# 跑不了：本机没有 wine，也没有 Windows。所以这一层能证的只有"这些测例在 Windows
# 目标上建得起来"——这已经比"只在 Linux 上编过"多一格：测例里的 cfg 与链接错误都抓得住。
cargo test --no-run -p overlay-win32 -p overlay-common -p overlay-report -p petstore --target "$TARGET" ||
  fail "交叉测试构建失败"

step "4/6 交叉 release 构建"
cargo build --release -p overlay-win32 --target "$TARGET" || fail "交叉 release 构建失败"
EXE="target/$TARGET/release/$BIN.exe"
[ -f "$EXE" ] || fail "构建说成功了但 $EXE 不在（不要把「没有」读成「没问题」）"

# ── 5/6 DLL 导入表 ─────────────────────────────────────────────────────
step "5/6 DLL 导入表核对（判据 = 本脚本顶部那两张表）"
PE="$(mktemp)" || fail "mktemp 失败"
# 先落盘再看结果：写成 `objdump | sed || fail` 的话 `$?` 是 sed 的，那道门等于没立。
if ! "$OBJDUMP" -p "$EXE" >"$PE" 2>&1; then
  cat "$PE" >&2
  fail "$OBJDUMP -p $EXE 没跑成"
fi
DLLS="$(sed -n 's/^[[:space:]]*DLL Name: //p' "$PE" | tr '[:upper:]' '[:lower:]' | sort -u)"
rm -f "$PE"
[ -n "$DLLS" ] || fail "导入表读出来是空的 ⇒ 判据本身失效，先修仪器再谈构建"
printf '%s\n' "$DLLS" | sed 's/^/   /'

while read -r dll; do
  [ -n "$dll" ] || continue
  for banned in "${BANNED_DLLS[@]}"; do
    if [ "$dll" = "$banned" ]; then
      fail "导入了 $dll ⇒ 用户机器上没有这个名字，加载器会在 main 之前拒掉这个 exe。
     先核对 .cargo/config.toml 里那条「crt-static 消不掉任何依赖」的实测结论是否仍成立；
     真需要它就在该文件加 rustflags = [\"-C\", \"target-feature=+crt-static\"] 后重跑本脚本。"
    fi
  done
  hit=0
  for ok in "${ALLOWED_DLLS[@]}"; do
    [ "$dll" = "$ok" ] && hit=1
  done
  [ "$hit" = 1 ] ||
    fail "导入了白名单之外的 $dll ⇒ 先回答「它是谁带进来的」（cargo tree -i），再决定是修依赖还是把实测出处写进这张表旁边。不许直接加名字了事。"
done <<<"$DLLS"

# 反向那一半：窗口四件套少一个都算"这不是本轮那条路径的产物"，
# 否则一份根本不含窗口代码的 exe 也能通过白名单（白名单是子集判据，天然拦不住"少东西"）。
for required in kernel32.dll user32.dll gdi32.dll ntdll.dll; do
  printf '%s\n' "$DLLS" | grep -qxF "$required" ||
    fail "导入表里没有 $required ⇒ 这个 exe 不是窗口那条路径的产物"
done
echo "   ✅ $(printf '%s\n' "$DLLS" | wc -l) 个名字全在白名单内，且窗口四件套齐全"

# ── 6/6 摊开产物目录 ───────────────────────────────────────────────────
step "6/6 摊开 $OUT"
rm -rf "$OUT"
mkdir -p "$OUT/assets/pets" "$OUT/assets/accessories" || fail "建不出 $OUT"
cp "$EXE" "$OUT/$BIN.exe" || fail "拷 exe 失败"
# 只拷运行时三件套；frames/ 是每档 ~11 MB 的烘焙中间产物，pack.py 之后没人读它。
# ⚠️ **M8 真机第一轮（2026-11-26）反馈 1/2 的根因在这一格**：zip 原来只带一只 tiger、
# 零配件，而收藏册与换装读的是**库根**（`$OPET_PETS` → `%LOCALAPPDATA%\opet\pets`）⇒
# 真机上两条 `打不开（os error 3）`，换装/换本体的行点了根本没有东西可点。
# 所以整库入库：十只本体 + 全部在册配件，run.bat 再用环境变量把库根指到包内。
for d in assets/pets/*/; do
  id="$(basename "$d")"
  [ -f "$d/pet.json" ] && [ -f "$d/sheet.webp" ] || continue
  mkdir -p "$OUT/assets/pets/$id" || fail "建不出 $OUT"
  cp "$d"/{pet.json,sheet.webp,polygon.json} "$OUT/assets/pets/$id/" ||
    fail "拷本体 $id 失败"
done
for d in assets/accessories/*/; do
  id="$(basename "$d")"
  [ -f "$d/accessory.json" ] && [ -f "$d/sheet.webp" ] || continue
  mkdir -p "$OUT/assets/accessories/$id" || fail "建不出 $OUT"
  cp "$d"/{accessory.json,sheet.webp,polygon.json} "$OUT/assets/accessories/$id/" ||
    fail "拷配件 $id 失败"
done
[ -f "$OUT/assets/pets/$PET/pet.json" ] ||
  fail "开机本体 $PET 不在产物里 ⇒ run.bat 的 --manifest 指向虚空"

# run.bat / help.bat 必须 CRLF：cmd.exe 读 LF-only 批处理会在一行中间断句，症状是
# "说不认识某条命令"，而文件在 Linux 上看着完全正确 ⇒ 判据只能落在字节上（下面核 CR）。
# ⚠️ 批处理正文**只写 ASCII**：cmd.exe 按当前 ANSI 代码页逐字节解析 .bat，而中文 Windows
# 默认是 936（GBK）⇒ 一份 UTF-8 的中文批处理里，某个双字节的后字节可能落在 `%`/引号上被当成
# 引导字节吃掉下一格。exe 自己的中文输出没这个问题（Rust 走 WriteConsoleW，不经代码页），
# 所以要区分的是"我们写的脚本"与"脚本打印的东西"。
cat >"$OUT/run.bat" <<'BAT'
@echo off
REM Windows test slice, second+third batch: pet, menu, dress-up, gacha card,
REM collection panel. Four runs. Every run ends by itself - no pet is allowed to
REM linger on your desktop. Report form: README-win32-test.txt
REM
REM Point the two asset libraries at THIS folder. Without these two lines the
REM client looks in %LOCALAPPDATA%\opet\pets, that folder does not exist, and
REM every collection / dress-up row is a read-only "would do nothing" row
REM (real-machine round 1, 2026-11-26, reports 1 and 2 - packaging gap).
set "OPET_PETS=%~dp0assets\pets"
set "OPET_ACCESSORIES=%~dp0assets\accessories"

echo ==== 1/4  test rect (reads no asset bytes, 20s) ====
opet-overlay-win32 --test-rect --seconds 20

echo.
echo ==== 2/4  the pet, demo store (nothing is written to disk, 300s) ====
echo Right-click the silhouette: menu. Try one pull, ten pulls, the collection panel.
echo Dress-up and body-swap HERE are expected refusals (demo store owns nothing,
echo and the exemption needs a real store) - the live version is run 3.
opet-overlay-win32 --manifest assets\pets\__PET__\pet.json --seconds 300 --seed 1

echo.
echo ==== 3/4  real archive for one session (writes opet-session.db here, 300s) ====
echo Same checks, but the DB line, the gacha rows and the panel rows must be live.
echo The two unowned flags are ON in THIS run only: dress-up and body-swap
echo must visibly work here (hat page, "put on" / "take off" / "pick another",
echo and the pet rows on the collection panel).
opet-overlay-win32 --manifest assets\pets\__PET__\pet.json --seconds 300 --seed 1 --db opet-session.db --allow-unowned-accessories --allow-unowned-pets

echo.
echo ==== 4/4  language and motion from the command line (60s) ====
opet-overlay-win32 --manifest assets\pets\__PET__\pet.json --seconds 60 --lang ja --motion off

echo.
echo One more read-only run worth doing by hand (no window, prints diagnostics):
echo     opet-overlay-win32 --probe ^> probe.log 2^>^&1
echo If a run printed no summary block, it was ended from outside - that run is
echo not evidence. Please also run  --probe  and attach probe.log.
pause
BAT
cat >"$OUT/help.bat" <<'BAT'
@echo off
opet-overlay-win32 --help
pause
BAT
sed -i 's/$/\r/' "$OUT/run.bat" "$OUT/help.bat"

README="$OUT/README-win32-test.txt"
{
  cat <<'TXT'
OPet for Windows —— M5/M6/M7 测试包（宠物 + 菜单 + 换装 + 抽卡卡面 + 收藏面板）

这个目录里
  opet-overlay-win32.exe   交叉编译出的 64 位程序。不装、不要管理员权限
  assets/pets/             **整册本体库**（每只一个目录：pet.json + sheet.webp + polygon.json）
  assets/accessories/      **整册配件库**（每件一个目录：accessory.json + sheet.webp + polygon.json）
  run.bat                  双击 = 四段：矩形档 20s → 本体演示态 300s → 带 --db 与豁免旗 300s → 语言/动效 60s，
                           各自到点自己退。开头两行 set 把 OPET_PETS / OPET_ACCESSORIES
                           指到本目录 ⇒ 收藏册与换装行是**活的**（第一轮 zip 不带库根，
                           那两族行全是只读，反馈 1/2 就是它）
  help.bat                 把 --help 打到控制台（参数面 + 被拒绝的开关与理由）

这一片**有**：最上层会动、能拖、点会反应的宠物；右键菜单；换装（含未拥有豁免旗）；
抽卡与结果卡面（含动效三档）；收藏面板与收藏册点名；换本体；--db/--lang/--motion/--probe 转真。
这一片**没有**（第三批）：托盘图标、NSIS 安装包、opet ctl 控制套接字、命中滞回。
上面那些开关会被**点名拒绝并给理由**（--ipc / --ipc-path / --no-cursor），
不会出现"接受了但没有任何作用"。
⚠️ 一条**在册限制**（真机第一轮反馈 3 定性，不是本轮回归）：**物品名不随 --lang 切换**。
菜单、卡片、摘要这些 UI 文案有四套语言，但宠物与配件的名字来自资产数据，
清单 schema 里每个物品只有一个中文 `display_name` ⇒ 切 en/ja/ko 后物品行照旧是
「牛仔帽」「旗」。要让物品名跟着语言走，得先给资产格式加多语言名字段（产品决定，已登记）。

怎么跑
  1) 双击 run.bat；或者
  2) 在本目录开命令行（Shift+右键 → 在此处打开终端），逐条：
       opet-overlay-win32 --test-rect --seconds 20
       opet-overlay-win32 --manifest assets\pets\__PET__\pet.json --seconds 300 --seed 1
       set "OPET_PETS=%CD%\assets\pets" & set "OPET_ACCESSORIES=%CD%\assets\accessories"
       opet-overlay-win32 --manifest assets\pets\__PET__\pet.json --seconds 300 --db opet-session.db --allow-unowned-accessories --allow-unowned-pets
       opet-overlay-win32 --probe > probe.log 2>&1
     想一直跑：--seconds 0。**没有托盘图标**，结束它 = 任务管理器，或
       taskkill /IM opet-overlay-win32.exe
     不带 --seconds = 3 秒自毁（手工敲错也不许有一只宠物赖在你桌面上）。
     不带 --db = 演示态：屏上右下角有 DRYRUN，盘上一个字节都不写（与 Wayland 同口径）。

控制台那一页是什么
  win:  开头 = 这一轮的几何：舞台显示器、scale 与 DPI 路线、窗口/buffer 像素、
        顶部与底部留白是"量出来的"还是"旗压的"、多屏跨 DPI 的那句提示
  摘要： 开头 = tick 数、五档省电归属与逐档归属、surface/buffer 尺寸、内核状态与
        指纹、内存与 CPU 时间
  ⚠️ 那一页要**整段贴回来**（从第一行 win: 到摘要最后一行）。最稳妥的是同时落一份文件，
     然后把**那个文件**发回来（屏上照旧显示）：
       opet-overlay-win32 --manifest assets\pets\__PET__\pet.json --seconds 300 > out.log 2>&1
     别把 `type out.log` 的屏上内容抄进报告：cmd 的 type 按当前 ANSI 代码页读字节，
     UTF-8 的中文会变方块（想看得懂先 `chcp 65001`）。exe 自己打上屏幕那一页不走代码页
     ⇒ 屏上是好的，会坏的只有 type 那一步。

要看的五件事
  A. 宠物在不在：脚应该站在地面上，不悬空、不埋进任务栏
  B. 边缘：方块/宠物四周有没有一圈黑边或白边（有 = 预乘 alpha 在本平台这条路不对）
  C. 点击穿透：宠物身上点得到、拖得动；它旁边空白处点下去应当落到后面的窗口
     （记事本光标还在闪 / 按钮按得动），宠物不许"黏"在指针上
  D. 缩放：设置 → 显示 → 缩放改成 125% 或 150%，宠物应当跟着变大小
  E. 拖动：拖着甩出宠物剪影再松手，宠物应当停在松手处

TXT
  cat <<'TXT'
──── 报告表（填这一页，别凭印象口述）──────────────────────────────────
★ = 这一格 Wine 判不了、只有真 Win11 的回答算数（D5 口径：Wine 的内存口径无量、
DPI 档位只到返回值、叠帧行为与真合成器不同。本机 Wine 九轮绿的都不是这些格）。

 1. Windows 版本与构建号            ：        （winver 回车，抄那一行）
 2. 分辨率与缩放百分比              ：        （设置 → 显示 → 缩放与布局，两个都抄）
 3. 显示器块数                      ：
 4. 第 2/3 块屏的缩放               ：        （只有一块屏就写"无"）
 5. 任务栏位置                      ：        （上/下/左/右）
 6. 任务栏高度或宽度（像素）        ：        （截图里量，或抄摘要里"顶部留白/底部留白"）
 7. 有没有把宠物拖到过别的屏        ：
 8. 摘要里的 scale 与 DPI 路线      ：★       （抄 win: 那一行）
 9. 宠物是否可缩放（是/否 + 档位）  ：★       （缩放改成 125%/150% 各看一次）
10. 宠物是否可移动（是/否）         ：
11. 截图：宠物骑在记事本窗口上      ：        （要能看出穿透边界在哪）
12. 截图：宠物在屏幕边缘（左/右/下）：
13. 控制台整段输出                  ：        （从第一行 win: 到摘要最后一行；
                                                长的那一页直接附 out.log 文件）
14. 边缘有没有黑边                  ：★       （Wine 有已知的 ULW 叠帧"累加老虎"，
                                                那边的边缘读数不能外推）
15. 穿透：试过哪几种判定方法        ：        （记事本光标 / 画布画点 / 按钮能不能按）
16. 穿透：结论                      ：        （穿 / 不穿 / 部分不穿 = 哪一块）
17. CPU 占用                        ：        （任务管理器那一列，另抄摘要里的 CPU 时间）
18. 内存（PrivateWorkingSet）       ：★       （任务管理器"专用工作集"或
                                                Get-Process 的 PrivateMemory；
                                                摘要里那行在 Wine 恒 0 = 无量）
19. 这一轮怎么结束的                ：        （到点自退 / 手杀 / taskkill / 关控制台）
20. 崩过吗（摘要打完 = 没崩）        ：
21. 其它异常（闪烁、残影、卡顿）    ：
22. --probe 的 probe.log            ：        （整份附回来；退出码 0/1 也要说）
23. 右键菜单：出不出、贴没贴身      ：        （run.bat 第 2 段里点）
24. 菜单项：换装页点"穿上"外观立变？：        （判卷段 = run.bat 第 3 段；第 2 段是
                                                演示库，点了应当是**点名拒绝**而不是变装）
25. 帽子页的未拥有行                ：        （第 2 段不带豁免 = 灰的点名拒绝；第 3 段
                                                自带 --allow-unowned-accessories，能穿）
26. 抽卡：单抽/十连点得动吗          ：        （余额行随之变吗）
27. 结果卡面：相位看得见吗          ：        （ burst 展开 → 结果 → 点掉）
28. --motion reduce/off：卡面变了吗  ：★off    （off 该**根本不进** burst 相位；
                                                Wine 只有静态断言，没有点击证据）
29. 余额为 0 时抽卡                  ：        （点名拒绝的文案出不出）
30. 收藏面板：开面板菜单是不是先关   ：        （两块面永不同时在场）
31. 面板行：没抽到的本体点了有效吗   ：        （第 2 段 = 只读行点名拒绝；第 3 段自带
                                                --allow-unowned-pets，应换得动）
32. 换本体后帽子去哪了                ：        （新本体没有那件 ⇒ 应脱下并写库，
                                                重启后还是脱着的状态；判卷段 = 第 3 段）
33. 带 --db 跑完后的 opet-session.db ：        （把文件本身附回来；Linux 侧要复验审计链）
34. 宠物几何：脚贴地、活动范围铺满屏 ：★       （第一轮抓到"舞台被 scale 除了两次"的真 bug
                                                ⇒ 宠物只占屏幕一角、脚不落地。本版已修，
                                                这一格是修复复核项：屏幕四边与地面都走到）
35. --lang 切语言后的物品名           ：        （UI 文案与物品名都应随语言变（2026-11-26
                                                §7-R12 翻向：抽卡卡面/收藏册/还缺/最近四块
                                                面都走 name_for）；名仍中文 = 病）
36. 这一版你最想让我先修的           ：

填不出的格子写"没测"，不要留空 —— 留空我会当成"你测过、没问题"，
"没测"我才知道下一轮该补哪一格。
TXT
  printf '本次本体：%s\n' "$PET"
  printf 'exe 字节数：%s\n' "$(stat -c %s "$OUT/$BIN.exe")"
  printf '构建时间：%s\n' "$(date -Is)"
  printf '构建机器：%s / rustc %s\n' "$(uname -sr)" "$(rustc -V)"
  printf 'DLL 导入表（本机实测）：\n'
  printf '%s\n' "$DLLS" | sed 's/^/  /'
} >"$README" || fail "写 README 失败"
# 正文里那两处 __PET__ 是占位符（用带引号的 heredoc 写正文 ⇒ 反斜杠与 % 都是字面量，
# 而 $ 展开会咬人，所以路径不插值、事后统一替换）。
sed -i "s|__PET__|$PET|g" "$README" "$OUT/run.bat"

# ── 自检：目录里该有的都有，且 git 不认识这里 ────────────────────────
step "产物目录自检"
for f in "$BIN.exe" "run.bat" "help.bat" "README-win32-test.txt" "assets/pets/$PET/pet.json" "assets/pets/$PET/sheet.webp" "assets/pets/$PET/polygon.json"; do
  [ -f "$OUT/$f" ] || fail "产物目录缺 $f"
done
# 库不空是本轮反馈 1/2 的**打包面判据**：zip 里只有一只 tiger 时，换装与换本体在真机上
# 根本没有测试面（`打不开` 那两行日志就是证据），所以这里断言的是"库存在且非空"。
N_PETS=$(ls -d "$OUT"/assets/pets/*/ 2>/dev/null | wc -l)
N_ACCS=$(ls -d "$OUT"/assets/accessories/*/ 2>/dev/null | wc -l)
[ "$N_PETS" -ge 2 ] || fail "dist 只有 $N_PETS 只本体 ⇒ 换本体没有测试面"
[ "$N_ACCS" -ge 1 ] || fail "dist 没有配件库 ⇒ 换装没有测试面"
echo "   ✅ 库账：本体 $N_PETS 只、配件 $N_ACCS 件（run.bat 的 set 指向它们）"
grep -q $'\r' "$OUT/run.bat" || fail "run.bat 里没有 CR ⇒ 不是 CRLF，cmd.exe 会读坏"
grep -q $'\r' "$OUT/help.bat" || fail "help.bat 里没有 CR ⇒ 不是 CRLF，cmd.exe 会读坏"

if git check-ignore -q "$OUT"; then
  echo "   ✅ $OUT 被 .gitignore 挡住（烘焙产物 + 二进制，不许提交）"
else
  fail "$OUT 不在 .gitignore 覆盖范围内 ⇒ 任何一次 git add . 都会把 3.4 MB 的 sheet 提交进公开的 AGPL 仓库。先修 .gitignore（现在靠的是 /dist/）。"
fi

# ── zip：给"拷到另一台机器"这一步留一份可校验的单文件 ────────────────
# 为什么值得打：真机那一轮在 Windows 上进行，中间要过 U 盘/网盘/邮件。目录树拷过去
# 少一个文件不会报错 —— 它会在 run.bat 那一步才炸，而那时人已经离开这台机器了。
# 单文件 + sha256 + zip 自带的 CRC 校验，让"传丢了"能在开机前而不是测试中途发现。
step "打 zip"
ZIP="$ROOT/dist/$(basename "$OUT")_$(date +%F).zip"
rm -f "$ZIP"                       # 同名旧包会让 zip 往里追加（--filesync 语义不清），先清掉
( cd "$ROOT/dist" && zip -q -X -9 -r "$(basename "$ZIP")" "$(basename "$OUT")" ) \
  || fail "zip 失败（缺 zip 包？Arch: pacman -S zip）"
zip -qT "$ZIP" || fail "zip CRC 自检没过 ⇒ 这份别拷过去，压缩或磁盘有问题"
ZIP_ENTRIES=$(unzip -Z1 "$ZIP" 2>/dev/null | grep -vc '/$')
EXE_IN_ZIP=$(unzip -Z1 "$ZIP" 2>/dev/null | grep -c "$BIN.exe$")
[ "$EXE_IN_ZIP" = "1" ] || fail "zip 里没有（或有多个）$BIN.exe ⇒ 打包路径不对"
# 计数从"目录里真有几个文件"来，不再写死 7（写死的那版随整库入库必然红，而它红的
# 是"计划变了"不是"包坏了"—— 提前换成动态，判据留给"zip 与目录不一致"这件事本身）。
DIR_FILES=$(find "$OUT" -type f | wc -l)
printf '   zip 内文件数 = %s（目录实测 %s：exe + 本体 %s 只×3 + 配件 %s 件×3 + 两 .bat + README）\n' \
  "$ZIP_ENTRIES" "$DIR_FILES" "$N_PETS" "$N_ACCS"
[ "$ZIP_ENTRIES" = "$DIR_FILES" ] || fail "zip 内容物数量与产物目录不一致，别拷这份"

echo
du -sh "$OUT"
find "$OUT" -type f | sort | sed "s|^$OUT/|   |"
echo "✅ 打包完成：$ROOT/$OUT"
echo "   单文件包：$ZIP（$(du -h "$ZIP" | cut -f1)）"
echo "   sha256 ：$(sha256sum "$ZIP" | cut -d' ' -f1)"
echo "   ⇒ 到 Windows 上先 certutil -hashfile <包名> SHA256 对一眼，再解压、双击 run.bat。"
echo "   整个目录拷到 Windows 11 x64，双击 run.bat，把 README 末尾那张表填回来。"
echo "   交叉层只证明\"能编/能链/导入表干净\"；Wine 九轮（scripts/test_windows_wine.sh）只证明逻辑与启动链；屏上对不对要真机。"
