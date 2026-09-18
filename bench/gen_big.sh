# Large-file corpus for the ">32MB inputs" scenario (see docs/src/dev/perf/matchers.md):
# concatenates real system ELF binaries (code+data+strings mix, ~DLL-like shape)
# into bench/big/dll100.raw (~100MB) and bench/big/dll32.raw (32MiB subset).
# Usage: bash bench/gen_big.sh   (output is gitignored; rerun after system updates)
set -euo pipefail
cd "$(dirname "$0")/.."
mkdir -p bench/big
: > bench/big/dll100.raw
total=0
while read -r f; do
  s=$(stat -Lc%s "$f" 2>/dev/null || echo 0)
  if [ $((total + s)) -le $((100 * 1024 * 1024)) ]; then
    cat "$f" >> bench/big/dll100.raw
    total=$((total + s))
  else
    head -c $((100 * 1024 * 1024 - total)) "$f" >> bench/big/dll100.raw
    total=$((100 * 1024 * 1024))
    break
  fi
done < <(find -L /run/current-system/sw/bin -type f -size +100k 2>/dev/null \
  | while read -r f; do head -c4 "$f" 2>/dev/null | grep -q ELF && echo "$f"; done)
head -c 33554432 bench/big/dll100.raw > bench/big/dll32.raw
# compressed variants so the decode A/B can run on the same payloads
# (matrix --mode dec-st --file bench/big/dll100.zstN)
for lvl in 1 3 9 19; do
  if [ ! -f bench/big/dll100.zst$lvl ]; then
    zstd -q -T0 -$lvl -k -o bench/big/dll100.zst$lvl bench/big/dll100.raw
  fi
done
echo "dll100.raw: $(stat -c%s bench/big/dll100.raw) bytes; dll32.raw: $(stat -c%s bench/big/dll32.raw) bytes"
