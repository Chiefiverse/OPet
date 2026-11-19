#!/usr/bin/env bash
# 摊平 shell-widget/ 到一个独立插件仓：Omarchy 的 `plugin add` 只认「仓库根 = 插件根」的 git 仓。
# 唯一事实源是本仓的 shell-widget/，产物仓里不许直接改代码。
# 用法：bash scripts/publish_plugin.sh [--verify | --push] [--remote <git-url>]
set -euo pipefail
cd "$(dirname "$0")/.."

SRC=shell-widget
OUT=dist/opet.pet
REMOTE=${OPET_PLUGIN_REMOTE:-git@ssh.github.com:Chiefiverse/OPetPlugIn.git}
MODE=stage

usage() {
  cat <<USAGE
用法：bash scripts/publish_plugin.sh [--verify|--push] [--remote <git-url>]

  （不带开关）  摊平 shell-widget/ 到 $OUT 并跑 omarchy-plugin-validate。不碰网络。
  --verify     同上，再把 $OUT 与已提交内容比对：有漂移或有提交没推 ⇒ exit 1。只读。
  --push       摊平后提交并推送到 --remote（提交身份取本仓的 git config）。
  --remote     覆盖推送地址（默认 \$OPET_PLUGIN_REMOTE 或 OPetPlugIn）。
USAGE
}

while (( $# )); do
  case "$1" in
    --verify) MODE=verify ;;
    --push) MODE=push ;;
    --remote) shift; REMOTE=${1:?--remote 后面要一个 git-url} ;;
    -h|--help) usage; exit 0 ;;
    *) echo "认不出的选项：$1（可用：--verify --push --remote -h）" >&2; usage >&2; exit 1 ;;
  esac
  shift
done

echo "== 1/4 发布清单（读 manifest 的 entryPoints 与 files，不硬编码文件名）"
[[ -f "$SRC/manifest.json" ]] || { echo "缺 $SRC/manifest.json" >&2; exit 1; }
files=(manifest.json)
while IFS= read -r ep; do
  [[ -n $ep ]] && files+=("$ep")
done < <(jq -r '.entryPoints | to_entries[] | .value' "$SRC/manifest.json" | sort -u)
while IFS= read -r extra; do
  [[ -n $extra ]] && files+=("$extra")
done < <(jq -r '(.files // [])[]' "$SRC/manifest.json")
for f in "${files[@]}"; do
  [[ $f != /* && $f != *..* ]] || { echo "入口必须是不含 .. 的相对路径，manifest 却写了：$f" >&2; exit 1; }
  [[ -f "$SRC/$f" ]] || { echo "manifest 指到 $SRC/$f，但它不存在" >&2; exit 1; }
done
printf '  %s\n' "${files[@]}"

echo "== 2/4 工作副本 $OUT"
mkdir -p dist
if [[ -e $OUT && ! -d $OUT/.git ]]; then
  [[ -z $(find "$OUT" -mindepth 1 -maxdepth 1 -print -quit) ]] || {
    echo "$OUT 已存在但不是 git 仓 ⇒ 自己看一眼再处理（脚本不代删别人的东西）" >&2; exit 1; }
fi
if [[ ! -d $OUT/.git ]]; then
  git init -q -b main "$OUT"
  git -C "$OUT" remote add origin "$REMOTE"
elif [[ $(git -C "$OUT" remote get-url origin 2>/dev/null || echo) != "$REMOTE" ]]; then
  echo "  改 origin：$(git -C "$OUT" remote get-url origin 2>/dev/null || echo '<无>') → $REMOTE"
  git -C "$OUT" remote set-url origin "$REMOTE"
fi
BRANCH=$(git -C "$OUT" symbolic-ref --quiet --short HEAD || echo main)

if [[ $MODE == push ]]; then
  # 先对齐远端再提交：`omarchy plugin update` 只做 `merge --ff-only`，分叉历史会让已装的人更新不了。
  refs=$(git ls-remote --heads "$REMOTE" 2>/dev/null || true)
  if [[ -n $refs ]]; then
    grep -q 'refs/heads/main$' <<<"$refs" || BRANCH=$(sed -n 's|.*refs/heads/||p' <<<"$refs" | head -1)
    git -C "$OUT" fetch -q origin "$BRANCH"
    if ! git -C "$OUT" rev-parse --verify -q HEAD >/dev/null; then
      git -C "$OUT" checkout -q -B "$BRANCH" FETCH_HEAD    # 本地还没有提交：直接站到远端那一档
    elif ! git -C "$OUT" merge --ff-only -q FETCH_HEAD 2>/dev/null; then
      echo "本地 $OUT 与 origin/$BRANCH 已分叉 ⇒ 产物仓里没有不可丢的东西：rm -rf $OUT 后重跑 --push" >&2
      exit 1
    fi
  fi
fi

echo "== 3/4 摊平"
while IFS= read -r -d '' f; do rm -f "$OUT/$f"; done < <(git -C "$OUT" ls-files -z)
for f in "${files[@]}"; do
  mkdir -p "$(dirname "$OUT/$f")"
  cp "$SRC/$f" "$OUT/$f"
done
[[ -f LICENSE ]] || { echo "缺 LICENSE（产物仓要带许可）" >&2; exit 1; }
cp LICENSE "$OUT/LICENSE"

case $REMOTE in
  git@ssh.github.com:*|git@github.com:*) PUB=https://github.com/"${REMOTE#*:}" ;;
  https://*|http://*) PUB=$REMOTE ;;
  *) PUB=$REMOTE; echo "  ⚠️ --remote 不是 https 形式，产物仓 README 里的安装命令会照抄它" ;;
esac
cat > "$OUT/README.md" <<'PUBDOC'
# opet.pet (Omarchy entry plugin · release build)

A bar icon that controls the [OPet](https://github.com/Chiefiverse/OPet) desktop pet:
start / stop / pause / resume / pull x1 / pull x10, without leaving the bar.
The pet is its own layer-shell process, launched and supervised by this plugin's
Service.qml (a pet crash never touches the desktop; 3 consecutive failed restarts
stops the supervisor).

Text is English by default; set **Language** to `zh` in the plugin settings for
Chinese (takes effect immediately, no restart).

⚠️ **This is a build artifact, not the source.** Single source of truth = the
`shell-widget/` directory of the OPet repository; edits here are overwritten by
the next flatten.

Install (requires Omarchy 4+):

    omarchy plugin add __PUB__ --enable

Update: `omarchy plugin update opet.pet`.

After installing, two settings are required: the **opet executable** and the
**body manifest (pet.json)** — on the Plugins page of Setup, or by editing the
layout entry in `~/.config/omarchy/shell.json`. Both deliberately have no
defaults: the plugin never guesses paths on your machine, and it refuses to
start while either is missing, naming the one that is. Without a `db` the pet
runs in demo mode (in-memory save, the card shows DRYRUN, nothing is written).

License: AGPL-3.0-only, same as OPet code (see LICENSE).
PUBDOC
# ⚠️ 这段必须是带引号的 heredoc：内容里有反引号（markdown 行内码）与 >，
#    不带引号会被当命令替换与重定向执行。URL 由标记替换，不靠 heredoc 展开。
sed -i "s|__PUB__|$PUB|g" "$OUT/README.md"

link=$(find "$OUT" -name .git -prune -o -type l -print -quit)
[[ -z $link ]] || { echo "摊平结果里有 symlink，Omarchy 会拒装：$link" >&2; exit 1; }
omarchy-plugin-validate "$OUT"
echo "  omarchy-plugin-validate：通过"

echo "== 4/4 结果"
# 漂移判据一律用 `diff HEAD` + `ls-files --others`，不用 `status --porcelain`：
# 后者读的是**暂存区**，而 `--push` 会 `git add -A` ⇒ 一次跑完就把脏内容留在索引里，
# 下一轮 --verify 会拿着上一轮的残留报红（假红）。这两条只读，不碰索引、不碰工作树。
publish_set=" ${files[*]} LICENSE README.md "
if git -C "$OUT" rev-parse --verify -q HEAD >/dev/null; then
  dirty=$( { git -C "$OUT" diff --name-status HEAD
             git -C "$OUT" ls-files --others
             for t in $(git -C "$OUT" ls-files); do
               [[ $publish_set == *" $t "* ]] || echo "陈旧 $t"
             done; } | sed '/^$/d' )
  ahead=0
  if git -C "$OUT" rev-parse --verify -q "origin/$BRANCH" >/dev/null; then
    ahead=$(git -C "$OUT" rev-list --count "origin/$BRANCH..HEAD")
    published=1
  else
    published=0
  fi
else
  dirty="(还没有任何提交 = 从未发布)"
  ahead=0
  published=0
fi
printf '  发布件（%s）：\n' "$OUT"
find "$OUT" -type f -not -path "$OUT/.git/*" | sort | sed 's|^|    |'
printf '  体积：%s\n' "$(du -sh "$OUT" | cut -f1)"

case $MODE in
  stage)
    if [[ -n $dirty || $ahead -gt 0 || $published == 0 ]]; then
      echo "  与已发布内容有差异（这是默认档，只报不改）：跑 --verify 会红，跑 --push 会发"
    else
      echo "  与已发布内容一致"
    fi
    ;;
  verify)
    if [[ -n $dirty ]]; then
      echo "  漂移文件：" >&2
      printf '%s\n' "$dirty" >&2
      echo "VERIFY result=FAIL ⇒ $OUT 与 shell-widget/ 摊出来的结果不一致（或还没发布过）" >&2
      exit 1
    fi
    if (( published == 0 )); then
      echo "VERIFY result=FAIL ⇒ origin/$BRANCH 上还没有任何东西（从没 --push 过）" >&2
      exit 1
    fi
    if (( ahead > 0 )); then
      echo "VERIFY result=FAIL ⇒ 有 $ahead 个提交没推到 origin/$BRANCH" >&2
      exit 1
    fi
    echo "VERIFY result=PASS ⇒ 产物仓与 shell-widget/ 逐字一致，且都已推送"
    ;;
  push)
    ver=$(jq -r '.version' "$OUT/manifest.json")
    git -C "$OUT" add -A
    if [[ -n $(git -C "$OUT" status --porcelain) ]]; then
      aname=$(git config user.name || true)
      amail=$(git config user.email || true)
      [[ -n $aname && -n $amail ]] || { echo "本仓没配 git user.name/user.email ⇒ 提交无处署名" >&2; exit 1; }
      git -C "$OUT" -c user.name="$aname" -c user.email="$amail" commit -q \
        -m "摊平自 OPet/shell-widget @$(git rev-parse --short HEAD)（插件 v$ver）"
      echo "  已提交：$aname <$amail>"
    else
      echo "  内容无变化，不产生新提交"
    fi
    if (( published == 0 )) || [[ $(git -C "$OUT" rev-list --count "origin/$BRANCH..HEAD") -gt 0 ]]; then
      git -C "$OUT" push -q origin "HEAD:refs/heads/$BRANCH"
      echo "  已推送 origin/$BRANCH：$REMOTE"
    else
      echo "  无提交可推（已经与 origin/$BRANCH 一致）"
    fi
    echo "别人现在可以装：omarchy plugin add $PUB --enable"
    # 实测：`plugin add` 克隆的是远端 HEAD 指向的分支（不写死 main）。HEAD 指错 = 装的人拿到空目录，
    # validate 报 "missing manifest.json" ⇒ 拒装。这条只有远端能答，所以推完现问一次。
    head_ref=$(git ls-remote --symref "$REMOTE" HEAD 2>/dev/null | sed -n 's|^ref: refs/heads/\([^[:space:]]*\).*|\1|p')
    if [[ -n $head_ref && $head_ref != "$BRANCH" ]]; then
      echo "  ⚠️ 远端 HEAD 指 '$head_ref'，内容在 '$BRANCH' ⇒ omarchy plugin add 会拒装。" >&2
      echo "     去仓库 Settings → Branches 把默认分支改成 $BRANCH，别人（和 plugin update）才找得到。" >&2
    fi
    ;;
esac
