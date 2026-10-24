#!/usr/bin/env bash
# Phase B — Omarchy / Hyprland 真机验收清单（设计书 §6.2）
#
# 用途：切换到 Omarchy OS 后的头两天跑这个脚本，把只能在真机上验证的风险点逐条判定。
# 现在（Windows / 无 Hyprland）也能安全运行：探测不到的项会标 SKIP，不会报错退出。
#
# 用法：
#   bash scripts/phase_b_checklist.sh              # 跑环境部分（B1–B3），宠物相关项自动 SKIP
#   bash scripts/phase_b_checklist.sh --with-pet   # 额外跑 B4–B6（需要 opet/opet-overlay-wayland 在 PATH 或 $OPET_BIN）
#   bash scripts/phase_b_checklist.sh --soak 30    # B4 的 RSS/CPU 采样改为 30 分钟
#
# 判定标准来源：设计书 §1.3（性能目标）与 §6.1 验收门禁。
# ⚠️ B0–B3 已于 2026-10-24 在 Omarchy 真机（Hyprland 0.56.2）实跑：9 PASS / 1 WARN / 0 FAIL / 4 SKIP。
#    B4–B6 **仍未跑**：B4 的 `--with-pet` 会在用户正在用的桌面上起 surface（M4 的边界是**必须带 `--seconds` 限时自毁**，
#    且"点身体/点空白/连打 30s 键盘"三项只能人工判 ⇒ 跑之前先问用户），B5 需要 `opet.service`（M7 才装），B6 需要 Quickshell widget。

set -uo pipefail

WITH_PET=0
SOAK_MIN=1
for arg in "$@"; do
  case "$arg" in
    --with-pet) WITH_PET=1 ;;
    --soak) shift_next=1 ;;
    --soak=*) SOAK_MIN="${arg#*=}" ;;
    -h|--help) sed -n '1,22p' "$0"; exit 0 ;;
  esac
done
# 支持 `--soak 30` 这种空格写法
prev=""
for arg in "$@"; do
  if [ "$prev" = "--soak" ]; then SOAK_MIN="$arg"; fi
  prev="$arg"
done
[ -z "${SOAK_MIN//[0-9]/}" ] || SOAK_MIN=1

PASS=0; FAIL=0; SKIP=0; WARN=0
REPORT="${PHASE_B_REPORT:-phase_b_report.txt}"
: > "$REPORT"

say()  { printf '%s\n' "$*" | tee -a "$REPORT"; }
hdr()  { say ""; say "=== $* ==="; }
ok()   { say "  [PASS] $*"; PASS=$((PASS+1)); }
bad()  { say "  [FAIL] $*"; FAIL=$((FAIL+1)); }
warn() { say "  [WARN] $*"; WARN=$((WARN+1)); }
skip() { say "  [SKIP] $*"; SKIP=$((SKIP+1)); }
need() { command -v "$1" >/dev/null 2>&1; }

say "OPet Phase B 真机验收"
say "时间: $(date -Is)  主机: $(uname -sr)  用户: ${USER:-$(id -un)}"

# ---------------------------------------------------------------- B0 会话环境
hdr "B0 会话环境（必须确认在 Hyprland/Wayland 原生会话里）"

sess="${XDG_SESSION_TYPE:-unknown}"
say "  XDG_SESSION_TYPE=$sess  XDG_CURRENT_DESKTOP=${XDG_CURRENT_DESKTOP:-unset}"

if [ "$sess" != "wayland" ]; then
  bad "当前不是 Wayland 会话。layer-shell 全部检查无效 —— 请在 Hyprland 会话里重跑（X11/Xwayland 下结果不可信）"
else
  ok "Wayland 会话"
fi

COMPOSITOR="none"
if [ -n "${HYPRLAND_INSTANCE_SIGNATURE:-}" ]; then COMPOSITOR="hyprland"; fi
if [ -n "${SWAYSOCK:-}" ]; then COMPOSITOR="sway"; fi
case "$COMPOSITOR" in
  hyprland) ok "合成器: Hyprland ($(hyprctl version 2>/dev/null | sed -n 's/^hyprland/, v/p' | tr -d '\n'))" ;;
  sway)     ok "合成器: Sway（layer-shell 同样支持；本脚本 Hyprland 专属步骤会 SKIP）" ;;
  none)     bad "检测不到 Hyprland/Sway 实例（HYPRLAND_INSTANCE_SIGNATURE 与 SWAYSOCK 均为空）" ;;
esac
say "  WAYLAND_DISPLAY=${WAYLAND_DISPLAY:-unset}  XDG_RUNTIME_DIR=${XDG_RUNTIME_DIR:-unset}"

# --------------------------------------------------- B1 layer-shell 与协议能力
hdr "B1 协议能力探测（zwlr_layer_shell_v1 / viewporter / fractional scale）"
say "  预期：Hyprland 自 0.2 起实现 wlr-layer-shell，正常情况下三者都在"

# 探测优先级：**客户端自带的 --probe 优先**，外部工具只当兜底。
# 理由（2026-10-24 真机实测）：Arch/Omarchy 上 `wayland-info` 与 `weston-info` 都不存在，
# 靠外部工具的这条路本来就是死的；而 `opet overlay --probe` 只读 registry、
# 不映射任何 surface，所以在用户正在用的 live 会话上跑也安全。
probe_bin=""
for c in "${OPET_BIN:-}" opet-overlay-wayland ./target/release/opet-overlay-wayland opet ./target/release/opet; do
  [ -n "$c" ] || continue
  if command -v "$c" >/dev/null 2>&1 || [ -x "$c" ]; then probe_bin="$c"; break; fi
done

if [ -n "$probe_bin" ]; then
  case "$probe_bin" in
    *opet) probe_out="$("$probe_bin" overlay --probe 2>&1)" ;;
    *)     probe_out="$("$probe_bin" --probe 2>&1)" ;;
  esac
  say "  用客户端自带诊断：$probe_bin（输出已存报告）"
  printf '%s\n' "$probe_out" >> "$REPORT"
  for proto in zwlr_layer_shell_v1 wp_viewporter wp_fractional_scale_manager_v1 wl_shm wl_seat; do
    if printf '%s\n' "$probe_out" | grep -q "  $proto  *v"; then
      ok "$proto 可用（$(printf '%s\n' "$probe_out" | grep -m1 "  $proto" | tr -s ' ')）"
    else
      bad "$proto 未出现在 registry（客户端必须走降级路径，见设计书 §4）"
    fi
  done
  # `zwp_linux_dmabuf_v1` 是 GPU 零拷贝路径的前提，M4 的 CPU blit 不用它，但 R5 要
  if printf '%s\n' "$probe_out" | grep -q "zwp_linux_dmabuf_v1"; then
    ok "zwp_linux_dmabuf_v1 可用（未来 GPU 零拷贝路径的前提）"
  else
    warn "无 zwp_linux_dmabuf_v1：只能走 shm"
  fi
elif need wayland-info; then
  wi="$(wayland-info 2>/dev/null)"
  for proto in zwlr_layer_shell_v1 wp_viewporter wp_fractional_scale_manager_v1 zwp_linux_dmabuf_v1; do
    if printf '%s\n' "$wi" | grep -q "$proto"; then ok "$proto 可用"; else bad "$proto 未出现在 registry（客户端必须走降级路径，见设计书 §4）"; fi
  done
  printf '%s\n' "$wi" | grep -i "subcompositor\|layer" >> "$REPORT" 2>/dev/null || true
else
  skip "既没有编译好的客户端（`cargo build --release` 后有 opet / opet-overlay-wayland），也没有 wayland-info/weston-info ⇒ 本步无法判定协议能力。客户端的 \`overlay --probe\` 就是为此存在的交付项"
fi

# --------------------------------------------------------- B2 GPU / 驱动（R5）
hdr "B2 GPU 与驱动（R5：NVIDIA + Wayland 透明闪烁只能在真机复现）"

gpu_line="unknown"
if need lspci; then
  gpu_line="$(lspci -d '::0300' 2>/dev/null | head -1 | cut -d: -f3- | sed 's/^ //')"
  [ -n "$gpu_line" ] || gpu_line="$(lspci 2>/dev/null | grep -i -E 'vga|3d|display' | head -1 | cut -d: -f3- | sed 's/^ //')"
fi
say "  GPU: ${gpu_line:-（lspci 不可用）}"

nvidia_loaded=0
if need lsmod && lsmod 2>/dev/null | grep -q '^nvidia'; then nvidia_loaded=1; fi
if [ "$nvidia_loaded" = 1 ]; then
  v="(未知)"
  need nvidia-smi && v="$(nvidia-smi --query-gpu=driver_version --format=csv,noheader 2>/dev/null | head -1)"
  warn "检测到 NVIDIA 专有驱动（版本 $v）—— 本步是 R5 的关键现场：必须亲眼确认宠物 surface 在 1.0/1.5/2.0 缩放下无闪烁、无残影、无黑边"
  say "       复现条件请记进 issue：驱动版本 / 缩放 / 是否开了 blur / Hyprland 版本"
  say "       若失败：用 --fallback-rgb 开关（不透明描边卡片模式）作为文档化降级路径"
elif [ -d /sys/module/nvidia ]; then
  warn "检测到 nvidia 内核模块（lsmod 不可用时的兜底判定）—— 按 NVIDIA 路径处理，见上"
elif [ "${gpu_line}" = "unknown" ]; then
  skip "无法确定 GPU/驱动（lspci 与 lsmod 不可用）—— 在 Omarchy 上应安装 pciutils 后重跑本步"
else
  ok "非 NVIDIA 专有驱动路径（${gpu_line}）—— R5 风险较低"
fi
if need glxinfo; then say "  渲染器: $(glxinfo -B 2>/dev/null | grep -i 'opengl renderer' | cut -d: -f2- | sed 's/^ //')"; else skip "glxinfo 未安装（Arch: mesa-demos；Fedora/Debian: mesa-utils）"; fi
if [ "${WLR_BACKENDS:-}" = "headless" ] || printf '%s' "$gpu_line" | grep -qi 'llvmpipe\|virgl'; then
  warn "看起来是软件渲染/虚拟机环境：本环境只能验证协议正确性，GPU 相关判定（R5）必须在实体机重做（D5）"
fi

# ------------------------------------------------------- B3 缩放与多屏（HiDPI）
hdr "B3 显示器与缩放（宠物尺寸不糊、坐标不偏）"

if [ "$COMPOSITOR" = hyprland ] && need hyprctl; then
  hyprctl monitors 2>/dev/null | sed -n 's/^Monitor /  Monitor /p;s/^  description: /    desc: /p;s/^  fps: /    fps: /p' >> "$REPORT"
  say "$(hyprctl monitors 2>/dev/null | grep -E '^Monitor|scale:' | sed 's/^/  /')"
  n=$(hyprctl monitors 2>/dev/null | grep -c '^Monitor')
  [ "${n:-0}" -ge 1 ] && ok "检测到 $n 个输出" || bad "hyprctl monitors 无输出"
  say "  → 人工核对：每个输出的 scale 值；非整数缩放（1.25/1.5）下宠物边缘不应发虚，位置不应偏移"
elif [ "$COMPOSITOR" = sway ] && need swaymsg; then
  swaymsg -t get_outputs 2>/dev/null | grep -E '"name"|"scale"' | tr -d ' ,' | sed 's/^/  /' >> "$REPORT"
else
  skip "无 hyprctl/swaymsg，跳过（VM 里跑 Sway 亦可完成本步）"
fi

say "  → 热插拔测试（手动）：拔掉副屏，宠物应在 wl_output remove 事件后自动迁到主屏，不崩溃、不留在幽灵坐标"

# ------------------------------------------------- B4 最小 overlay + 穿透 + 焦点
hdr "B4 宠物 surface：穿透、无焦点抢占、内存/CPU"

OPET_BIN="${OPET_BIN:-}"
if [ -z "$OPET_BIN" ]; then
  for c in opet-overlay-wayland opet; do need "$c" && { OPET_BIN="$(command -v "$c")"; break; }; done
fi
[ -z "$OPET_BIN" ] && need cargo && [ -x "target/release/opet" ] && OPET_BIN="$PWD/target/release/opet"

if [ "$WITH_PET" = 0 ]; then
  skip "--with-pet 未指定（M4 之前正常）。二进制就绪后用: bash scripts/phase_b_checklist.sh --with-pet"
elif [ -z "$OPET_BIN" ]; then
  bad "--with-pet 已指定但找不到 opet-overlay-wayland / opet；请设置 OPET_BIN=/path/to/binary"
else
  say "  使用二进制: $OPET_BIN"

  # (1) 焦点不被抢占：启动前后活动窗口必须一致
  # 起宠物**必须给参数**：没 `--manifest` 客户端直接拒绝（内置 fixture 没有像素），
  # 没 `--seconds` 就会赖在用户正在用的桌面上 —— 两条都是红线，所以这里显式拼参数，
  # 而不是裸跑二进制（原先正是那样，B4 必然报"3s 内已退出"）。
  # `--panel-top` 是 layer-shell 的可用区补偿（本机顶栏 20 逻辑像素），别的机器可覆盖。
  pet_secs=$(( SOAK_MIN * 60 + 30 ))
  case "$OPET_BIN" in
    *opet) pet_cmd=("$OPET_BIN" overlay) ;;
    *)     pet_cmd=("$OPET_BIN") ;;
  esac
  pet_cmd+=(--manifest "${OPET_MANIFEST:-assets/pets/cat_shiro/pet.json}"
            --seconds "$pet_secs"
            ${OPET_OVERLAY_ARGS:---panel-top 20})
  say "  启动命令: ${pet_cmd[*]}"
  before=""
  [ "$COMPOSITOR" = hyprland ] && before="$(hyprctl activewindow 2>/dev/null | sed -n 's/^.*ID: \([0-9A-Fa-f]*\).*/\1/p' | head -1)"
  "${pet_cmd[@]}" >/tmp/opet.log 2>&1 &
  petpid=$!
  sleep 3
  if ! kill -0 "$petpid" 2>/dev/null; then
    bad "宠物进程启动后 3s 内已退出，见 /tmp/opet.log"
  else
    say "  宠物 PID=$petpid"
    if [ -n "$before" ]; then
      after="$(hyprctl activewindow 2>/dev/null | sed -n 's/^.*ID: \([0-9A-Fa-f]*\).*/\1/p' | head -1)"
      [ "$before" = "$after" ] && ok "键盘焦点未被抢占（layer-shell keyboard_interactivity=none 生效）" \
                               || bad "焦点被抢走：$before → $after（违反 Omarchy 用户底线，必须修）"
      # (2) layer surface 不应出现在普通窗口列表里
      if hyprctl clients 2>/dev/null | grep -qi 'opet'; then
        warn "宠物出现在 hyprctl clients 里 —— 若它是普通 xdg-toplevel（降级路径），预期如此；若是 layer surface 则配置有误"
      else
        ok "宠物不在 clients 列表（符合 overlay layer surface 行为）"
      fi
    else
      skip "无法读取 activewindow，跳过焦点/客户端判定"
    fi

    # (3) RSS + CPU 采样（门禁：RSS ≤ 120MB，空闲 CPU ≤ 1.5%）
    say "  采样 ${SOAK_MIN} 分钟（宠物置于 idle/静止）..."
    peak_rss=0; cpu_total=0; samples=0
    end=$(( $(date +%s) + SOAK_MIN * 60 ))
    while [ "$(date +%s)" -lt "$end" ]; do
      if ! kill -0 "$petpid" 2>/dev/null; then bad "采样期间宠物进程退出（崩溃！）"; break; fi
      rss=$(awk '/VmRSS/{print $2}' /proc/$petpid/status 2>/dev/null | head -1)
      if [ -n "${rss:-}" ]; then
        [ "$rss" -gt "$peak_rss" ] && peak_rss=$rss
        c=$(ps -o %cpu= -p "$petpid" 2>/dev/null | tr -d ' ')
        [ -n "${c:-}" ] && { cpu_total=$(awk -v a="$cpu_total" -v b="$c" 'BEGIN{print a+b}'); samples=$((samples+1)); }
      fi
      sleep 10
    done
    if [ "$peak_rss" -gt 0 ]; then
      peak_mb=$(( peak_rss / 1024 ))
      [ "$peak_mb" -le 120 ] && ok "峰值 RSS = ${peak_mb} MB（门禁 ≤120）" || bad "峰值 RSS = ${peak_mb} MB，超门禁"
      avg_cpu=$(awk -v a="$cpu_total" -v n="$samples" 'BEGIN{ if(n>0) printf "%.2f", a/n; else print "n/a"}')
      say "  平均 CPU = ${avg_cpu}%（门禁：空闲 ≤1.5%，活跃 ≤5%）"
      [ "$avg_cpu" = "n/a" ] || awk -v c="$avg_cpu" 'BEGIN{exit !(c>1.5)}' && warn "空闲 CPU 超过 1.5% —— 检查是否在 Idle 状态下仍提交帧（应停帧）"
    else
      skip "读不到 /proc RSS（非 Linux？），用 systemd-cgtop/htop 人工测"
    fi

    # (4) 穿透（无法自动判定，必须人工）
    say "  → 人工穿透测试（三项全过才算 B4 PASS）："
    say "     a) 点宠物身体：能被抓住/拖拽"
    say "     b) 点宠物四周（透明区）：点击落到背后终端，光标位置正确"
    say "     c) 让宠物爬到正在打字的光标上，连续打字 30s：不掉字符、不丢焦点"
    kill "$petpid" 2>/dev/null; wait "$petpid" 2>/dev/null || true
  fi
fi

# -------------------------------------------------- B5 Quickshell widget 与崩溃隔离
hdr "B5 桌面壳联动（D3 + R1：宠物崩溃绝不能拖垮桌面）"

if [ "$COMPOSITOR" = hyprland ]; then
  if need systemctl && systemctl --user is-active --quiet opet.service 2>/dev/null; then
    ok "opet.service（user）处于 active"
    pid="$(systemctl --user show -p MainPID --value opet.service 2>/dev/null)"
    say "  主进程 PID=$pid；执行 kill -9 验证自动拉起…"
    kill -9 "$pid" 2>/dev/null || true
    sleep 6
    if systemctl --user is-active --quiet opet.service; then
      if hyprctl monitors >/dev/null 2>&1; then ok "宠物被 kill -9 后已自动恢复，且 Hyprland 与顶栏完全正常（R1 验收通过）"
      else bad "合成器异常 —— 这是致命问题，立刻停止并重新评估进程隔离"; fi
    else
      warn "宠物未在 6s 内恢复：检查 Restart=on-failure 与 RestartSec"
    fi
  else
    skip "opet.service(user) 未安装/未启用（M7 打包后再验本步）"
  fi
  skip "Quickshell widget IPC 200ms 超时降级：需 shell-widget/ 就绪后人工验证（顶栏图标点三下无响应时应退化为静态图标，且不影响其他 shell 功能）"
else
  skip "非 Hyprland 会话，跳过"
fi

# ------------------------------------------------------------------ 汇总
hdr "汇总"
say "  PASS=$PASS  WARN=$WARN  FAIL=$FAIL  SKIP=$SKIP"
say "  完整输出: $REPORT"
say ""
say "  B 阶段结论判定：B0/B1/B2/B3 全 PASS 且 B4 的三项人工测试通过 → R2/R5 不确定性关闭，可进 M7。"
say "  任何一项 FAIL：先按设计书 §4 的降级路径（layer-shell → xdg-toplevel + windowrule → 不透明卡片）确认是否可绕过，再决定是否改架构。"
say "  注意：软件渲染/VM 下的 PASS 不能替代真机 PASS（R5 只在实体机可判）。"

[ "$FAIL" -eq 0 ] || exit 1
exit 0
