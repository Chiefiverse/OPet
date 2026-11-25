#!/usr/bin/env bash
# M8（设计书 §6.1.26）—— Windows 客户端的 Wine 验收脚本 = M7-g 那五轮手工命令的固化。
#
# 为什么值得落成脚本：M7-g 的读数是真的，但它不可重放——下一轮改动了 win32 面之后，
# "Wine 上也绿过"这句话必须能由一条命令重新证出来，而不是靠 daily 里的表格。
# 判据全部落在**退出码与日志 token** 上，不落在"跑完没炸"上。
#
# 用法：
#   bash scripts/test_windows_wine.sh                # 全轮（含一次测试 exe 编译）
#   SKIP_BUILD=1 bash scripts/test_windows_wine.sh   # 直接用已编好的测试 exe
#   LIVE_SECONDS=30 bash scripts/test_windows_wine.sh  # 加长两轮 live（默认 10/20）
#
# ⚠️ 三条边界（每条都对应回报表里"要真机"的那几格）：
#   * **Wine 读数不是 Win11 判据**（D5）。本脚本判得动的只有：类型与逻辑在真 Win32 API
#     上自洽（轮 3）、启动链与摘要 token 齐（轮 6/7）、拒绝路径给理由（轮 2/8/9）。
#     PrivateWorkingSet 恒 0 ⇒ ≤200 MiB 那条在这里**无量**；PER_MONITOR_AWARE_V2 标签
#     判的是 SetProcessDpiAwarenessContext 的返回值，Wine 的 TRUE ≠ Win11 的 TRUE。
#   * **交互四件（菜单/换装/抽卡/面板）不在此脚本内** —— HTTRANSPARENT 窗外不投递事件，
#     合成点击进不了分层窗口那条命中链，那四件是"要用户本人点"的目视格（真机或桌面 Wine）。
#   * 默认 `WINEDEBUG=-all` 会**吃掉 fixme 行**：这一轮看不见 `DPI context 0x22` 不等于
#     Wine 修好了。要判 Wine 的 DPI 行为就 WINEDEBUG=fixme-all 重跑轮 6 并自己读日志。

set -u
cd "$(dirname "$0")/.." || exit 2
ROOT="$PWD"

export WINEPREFIX="${WINEPREFIX:-$HOME/.opet-win32}"
export WINEDLLOVERRIDES="mscoree,mshtml="
export WINEDEBUG="${WINEDEBUG:--all}"
TARGET="x86_64-pc-windows-gnu"
RECT_SECONDS="${RECT_SECONDS:-20}"
LIVE_SECONDS="${LIVE_SECONDS:-10}"
SKIP_BUILD="${SKIP_BUILD:-0}"

LOG_DIR="$HOME/.local/share/opet/scratch"
mkdir -p "$LOG_DIR" || { echo "建不出 $LOG_DIR" >&2; exit 2; }
LOG="$LOG_DIR/wine_m8_$(date +%FT%H-%M-%S%:z).log"
FAILURES=0
PASS_COUNT=0

note() { echo "== $*"; }
ok()   { PASS_COUNT=$((PASS_COUNT + 1)); echo "   ✅ $*" | tee -a "$LOG"; }
bad()  { FAILURES=$((FAILURES + 1)); echo "   ❌ $*" | tee -a "$LOG"; }

# 一条 Wine 调用：$1 = 日志段名，其余 = argv。stdout+stderr 全进 $LOG，退出码单独拿。
run_wine() {
  local seg="$1"; shift
  echo "--- $seg ---" >>"$LOG"
  wine "$@" >>"$LOG" 2>&1
  local code=$?
  echo "[$seg exit=$code]" >>"$LOG"
  return $code
}

# token 判据：从日志里固定串匹配（这些行含反斜杠与引号，一律 -F，不碰正则）。
check() {
  local seg="$1" pat="$2" want="$3"   # want: yes|no
  if grep -qF -- "$pat" "$LOG"; then
    [ "$want" = "yes" ] && ok "$seg：命中「$pat」" || bad "$seg：本不该出现「$pat」"
  else
    [ "$want" = "yes" ] && bad "$seg：缺「$pat」" || ok "$seg：无「$pat」（按预期）"
  fi
}

command -v wine >/dev/null || { echo "PATH 里没有 wine" >&2; exit 2; }
[ -d "$WINEPREFIX" ] || { echo "WINEPREFIX=$WINEPREFIX 不存在 ⇒ 先 wineboot -u 初始化" >&2; exit 2; }

EXE="target/$TARGET/release/opet-overlay-win32.exe"
[ -f "$EXE" ] || { echo "$EXE 不存在 ⇒ 先跑 bash scripts/build_windows.sh（或 -p overlay-win32 --target $TARGET --release）" >&2; exit 2; }
[ -f "assets/pets/tiger/pet.json" ] || { echo "缺 assets/pets/tiger/pet.json" >&2; exit 2; }

# 会写 target/ 的命令不许并发（§6.1.25 ⑨-6）：默认让 cargo 自己串行，但抢一个 flock 兜底。
BUILD_LOCK="$ROOT/target/.opet-build.lock"
TEST_EXES=()
if [ "$SKIP_BUILD" != "1" ]; then
  exec 9>"$BUILD_LOCK"
  flock -n 9 || { echo "另一个构建正持有 $BUILD_LOCK ⇒ 等它结束再跑（并发写 target/ 会踩坏两边的留盘日志）" >&2; exit 2; }
  note "编交叉测试 exe（overlay-win32 + petstore，--no-run；JSON 清单留档）"
  JSON_LOG="$LOG_DIR/wine_m8_testjson_$(date +%FT%H-%M-%S%:z).log"
  cargo test --no-run -p overlay-win32 -p petstore --target "$TARGET" --message-format=json \
    >"$JSON_LOG" 2>>"$LOG"
  [ $? = 0 ] || { tail -20 "$LOG" >&2; echo "交叉测试编译失败（stderr 日志 = $LOG）" >&2; exit 1; }
  # 族名单取自这次编译的产物清单：新加一个 tests/*.rs 不需要改这个脚本
  #（手抄名单 = 下一轮悄悄漏测的形状）。
  mapfile -t TEST_EXES < <(grep -o '"executable":"[^"]*\.exe"' "$JSON_LOG" | sed 's/.*:"//; s/"$//' | grep -v '/examples/' | sort -u)
fi
if [ "${#TEST_EXES[@]}" = 0 ]; then
  # SKIP_BUILD=1 或 JSON 读不出 ⇒ 退回 glob，但只收本脚本两个包的族
  #（debug/deps 里躺着别的包的历史 exe，全收 = 把不相干的包也算进本轮判据）。
  shopt -s nullglob
  TEST_EXES=(target/$TARGET/debug/deps/overlay_win32-*.exe
             target/$TARGET/debug/deps/opet_overlay_win32-*.exe
             target/$TARGET/debug/deps/petstore-*.exe
             target/$TARGET/debug/deps/audit_verify-*.exe
             target/$TARGET/debug/deps/equipped_roundtrip-*.exe
             target/$TARGET/debug/deps/ledger_roundtrip-*.exe
             target/$TARGET/debug/deps/pack_install-*.exe
             target/$TARGET/debug/deps/pack_inventory-*.exe
             target/$TARGET/debug/deps/paid_randomness-*.exe
             target/$TARGET/debug/deps/schema_redlines-*.exe)
  shopt -u nullglob
fi

# ── 1/9 --help：参数面文本 ────────────────────────────────────────────────
note "1/9 --help"
run_wine "help" "$EXE" --help || bad "--help 退出码非 0"
check "help" "USAGE: opet-overlay-win32" yes
check "help" "no --ipc" yes
check "help" "no --no-cursor" yes

# ── 2/9 三条 REFUSED 点名拒绝：拒绝必须有理由、有 USAGE、退 2 ─────────────
note "2/9 REFUSED 开关（--ipc / --ipc-path / --no-cursor）"
for flag in --ipc --ipc-path --no-cursor; do
  run_wine "refused:$flag" "$EXE" "$flag"
  code=$?
  [ "$code" = 2 ] && ok "$flag：exit 2" || bad "$flag：exit $code（拒绝没落地）"
  check "refused:$flag" "USAGE: opet-overlay-win32" yes
done
# --ipc-path 带值也必须被点名拒（不是"值吃了旗就放行"）
run_wine "refused:--ipc-path X" "$EXE" --ipc-path /tmp/x.sock
[ $? = 2 ] && ok "--ipc-path 带值同样 exit 2" || bad "--ipc-path 带值退出码非 2"

# ── 3/9 交叉测试 exe 在 Wine 里真跑（M7-g 轮①的固化）────────────────────
note "3/9 交叉 lib 测试 exe（overlay-win32；petstore 族一并）"
# 同名多份 = 历史 hash 残留，一律取最新那份，否则测的是旧代码。
for base in $(printf '%s\n' "${TEST_EXES[@]}" | sed 's|.*/||; s|-[^-]*\.exe$||' | sort -u); do
  newest=$(ls -t target/$TARGET/debug/deps/${base}-*.exe 2>/dev/null | head -1)
  [ -n "$newest" ] || continue
  run_wine "libtest:$base" "$newest"
  code=$?
  if [ "$code" = 0 ]; then
    ok "$base.exe：exit 0"
    check "libtest:$base" "test result: ok" yes
  else
    bad "$base.exe：exit $code（test result 行没打 ok ⇒ 见日志尾部）"
    tail -5 "$LOG" >&2
  fi
done

# ── 4/9 --probe：四节全有数、exit 0（M7-g 轮②）──────────────────────────
note "4/9 --probe（期望 PASS / exit 0）"
run_wine "probe" "$EXE" --probe
[ $? = 0 ] && ok "--probe：exit 0" || bad "--probe：exit 非 0 ⇒ 这台 Wine 的分层窗口符号或监视器枚举坏了"
check "probe" "UpdateLayeredWindow 解析得到" yes
check "probe" "⇒ PASS" yes
check "probe" "GetDpiForSystem = 96" yes
# 这条是 M7 那轮假阴性的反面教材：符号查找必须点名 "user32.dll"，
# 若哪天有人改回 GetModuleHandleW(null)，这里会红 —— 红的是病，不许放宽。
check "probe" "解析不到" no

# ── 5/9 --probe --monitor DISPLAY9：拒绝要有理由（M7-g 轮③）──────────────
note "5/9 --probe --monitor DISPLAY9（期望 FAIL / exit 1）"
run_wine "probe-miss" "$EXE" --probe --monitor DISPLAY9
[ $? = 1 ] && ok "--probe --monitor DISPLAY9：exit 1" || bad "点名不存在的监视器居然退 0"
check "probe-miss" "点名未命中" yes
check "probe-miss" "⇒ FAIL" yes
# 裸旗拒收值（与 Wayland 那家行为不同，同名不同物 = §6.1.26 登记的偏离）
run_wine "probe-inline" "$EXE" --probe=0
[ $? = 2 ] && ok "--probe=0：exit 2（本平台是裸旗）" || bad "--probe=0 退出码非 2"

# ── 6/9 --test-rect：第一批冻结行为逐字不变 ──────────────────────────────
note "6/9 --test-rect --seconds $RECT_SECONDS"
run_wine "test-rect" "$EXE" --test-rect --seconds "$RECT_SECONDS"
[ $? = 0 ] && ok "--test-rect：exit 0（自己到点退出）" || bad "--test-rect 退出码非 0"
check "test-rect" "矩形档=是" yes
check "test-rect" "windows overlay 摘要：" yes
check "test-rect" "scale 1.00×（PER_MONITOR_AWARE_V2）" yes
check "test-rect" "buffer 错误 0" yes

# ── 7/9 live：本体 + --db + OPET_PETS/OPET_ACCESSORIES + --allow-unowned-pets（M7-g 轮④）
note "7/9 live --seconds $LIVE_SECONDS（tiger + 新库 + 库根注入）"
mkdir -p "$ROOT/scratch"
DB_WIN=$(winepath -w "$ROOT/scratch/wine_m8.db" 2>/dev/null)
rm -f "$ROOT/scratch/wine_m8.db"
PETS_WIN=$(winepath -w "$ROOT/assets/pets" 2>/dev/null)
ACC_WIN=$(winepath -w "$ROOT/assets/accessories" 2>/dev/null)
[ -n "$DB_WIN" ] || { bad "winepath 转不出绝对路径"; }
OPET_PETS="$PETS_WIN" OPET_ACCESSORIES="$ACC_WIN" \
  run_wine "live" "$EXE" --manifest assets/pets/tiger/pet.json \
  --seconds "$LIVE_SECONDS" --db "$DB_WIN" --allow-unowned-pets
[ $? = 0 ] && ok "live：exit 0（自己到点退出 = report() 跑过了）" || bad "live 退出码非 0 ⇒ 摘要没落盘，这一轮不算数"
check "live" "未拥有本体放行=是" yes
check "live" "← cli --db（schema 已建/验" yes
check "live" "gacha: 库已就绪" yes
check "live" "⇒ 10 只可换" yes
check "live" "⇒ 2 件可用" yes
check "live" "五档合计" yes
check "live" "⇒ OK（互斥且齐）" yes
check "live" "延后 0" yes
check "live" "buffer 错误 0" yes
check "live" "帧回调看门狗 0" yes
check "live" "windows overlay 摘要：" yes
# 配置链来源行：存在 = 「已读取」，首轮不存在 = 「不存在」—— 两种都必须明说，不许沉默。
if grep -qF "config.toml（已读取）" "$LOG"; then
  ok "live：配置链走到「已读取」档"
elif grep -qF "config.toml 不存在" "$LOG"; then
  ok "live：配置链如实打「不存在」（首轮形状）"
else
  bad "live：配置来源行两档都没打 ⇒ M7-e 那条链断了"
fi

# ── 8/9 --db 相对路径：落笔前拒绝（源级门禁之外的运行时面）───────────────
note "8/9 --db 相对路径（期望 exit 2 且点名绝对路径）"
run_wine "db-relative" "$EXE" --manifest assets/pets/tiger/pet.json --seconds 2 --db scratch/relative.db
code=$?
[ "$code" = 2 ] && ok "相对 --db：exit 2" || bad "相对 --db 没被拒（退出码 $code）"
check "db-relative" "存档路径必须是绝对路径" yes

# ── 9/9 跨平台账本回归：Linux 的 opet 读 Windows 二进制写下的库（M6 那格的固化）
# 样本 = m6 那轮 Wine 真跑写下的 m6.db（12 条抽取）。⚠️ 第 7 轮的新库**不能**当样本：
# banner 惰性到开菜单那一刻才装，无人点击的库上 audit verify 必然报「库里没有 banner」——
# 那是正确结果，不是病（11-25 实测）。head 哈希钉死 = 整条重放链不许悄悄漂移。
note "9/9 opet audit verify --db <Wine 写下的 m6.db>"
M6_DB="$WINEPREFIX/drive_c/users/jun/AppData/Local/opet/m6.db"
HOST_BIN="target/release/opet"
if [ ! -f "$M6_DB" ]; then
  note "9/9 跳过（$M6_DB 不在 ⇒ M6 那轮的账本样本被清过）"
elif [ ! -f "$HOST_BIN" ]; then
  note "9/9 跳过（没有 target/release/opet ⇒ cargo build --release 后重跑）"
else
  "$HOST_BIN" audit verify --db "$M6_DB" >>"$LOG" 2>&1
  code=$?
  [ "$code" = 0 ] && ok "audit verify：exit 0" || bad "Windows 写下的账本在 Linux 读不回来（退出码 $code，见日志尾部）"
  check "audit" "len=12" yes
  check "audit" "head=9913a0648dbd26985a1cbbf3b64660d3b51f865e08d05e2966f73c2f64ff819f" yes
fi

# ── 汇总 ──────────────────────────────────────────────────────────────────
echo
echo "════ 汇总：$PASS_COUNT 项判据，$FAILURES 项失败 ════"
echo "日志 = $LOG"
if [ "$FAILURES" = 0 ]; then
  echo "✅ Wine 全轮通过。⚠️ 这不等于 Win11 通过（D5）：内存线在 Wine 无量、DPI 标签只到返回值、"
  echo "   目视四件（菜单/换装/抽卡/面板）与真机回报表仍在用户手上。"
  exit 0
else
  echo "❌ 有 $FAILURES 项判据红 ⇒ 先问「它红的是病还是一个正确的结果」，别先放宽。"
  exit 1
fi
