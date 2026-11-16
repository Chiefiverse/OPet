#!/usr/bin/env bash
# 打一个可直接运行的便携测试版：release 二进制 + 6 件 starter 资产 + run.sh。
# 用法：bash scripts/make_portable.sh
set -euo pipefail
cd "$(dirname "$0")/.."

OUT=dist/opet-portable
PETS=(rabbit bear spacecat)
ACCESSORIES=(skicap ballcap dragonfly)

echo "== 1/4 cargo build --release -p opet-cli"
cargo build --release -p opet-cli

echo "== 2/4 组装 $OUT"
rm -rf "$OUT"
mkdir -p "$OUT/assets/pets" "$OUT/assets/accessories"
cp target/release/opet "$OUT/opet"

# 只拷运行时三件套（清单 + sheet.webp + polygon.json），不拷 frames/（仅烘焙用）。
for p in "${PETS[@]}"; do
  src=assets/pets/$p
  for f in pet.json sheet.webp polygon.json; do
    [[ -f "$src/$f" ]] || { echo "缺资产：$src/$f" >&2; exit 1; }
  done
  mkdir -p "$OUT/assets/pets/$p"
  cp "$src"/{pet.json,sheet.webp,polygon.json} "$OUT/assets/pets/$p/"
done
for a in "${ACCESSORIES[@]}"; do
  src=assets/accessories/$a
  for f in accessory.json sheet.webp polygon.json; do
    [[ -f "$src/$f" ]] || { echo "缺资产：$src/$f" >&2; exit 1; }
  done
  mkdir -p "$OUT/assets/accessories/$a"
  cp "$src"/{accessory.json,sheet.webp,polygon.json} "$OUT/assets/accessories/$a/"
done

echo "== 3/4 生成 run.sh"
cat > "$OUT/run.sh" <<'RUN'
#!/usr/bin/env bash
set -euo pipefail
HERE=$(cd "$(dirname "$0")" && pwd)
export OPET_PETS="$HERE/assets/pets"
export OPET_ACCESSORIES="$HERE/assets/accessories"
DB="$HERE/opet.db"
"$HERE/opet" starter --db "$DB"
exec "$HERE/opet" overlay \
  --manifest "$HERE/assets/pets/bear/pet.json" \
  --db "$DB" --panel-top "${OPET_PANEL_TOP:-26}" \
  --seconds "${OPET_SECONDS:-0}"
RUN
chmod +x "$OUT/run.sh"

echo "== 4/4 结果"
du -sh "$OUT"
find "$OUT" -type f | sort
echo "便携包就绪：$OUT（运行 bash $OUT/run.sh）"
