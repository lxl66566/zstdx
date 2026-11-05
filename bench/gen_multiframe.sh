# Multi-frame decode payload: one concatenated stream of six independent
# zstd frames over mixed corpus shapes with one dominant frame, for the
# multi-frame parallel-decode cell (docs/src/dev/research.md, Wildcards).
# Writes bench/big/multi.raw (32 MiB of output) and bench/big/multi.zst3,
# where the fourth frame alone carries half the bytes. Requires
# bench/gen_corpus.sh first.
# Usage: bash bench/gen_multiframe.sh   (output is gitignored)
set -euo pipefail
cd "$(dirname "$0")/.."
[ -f bench/corpus/text.raw ] || { echo "run bench/gen_corpus.sh first" >&2; exit 1; }
mkdir -p bench/big

# Frame table: corpus shape, byte offset into <shape>.raw, frame size.
# 4 + 2 + 3 + 16 + 3 + 4 = 32 MiB, the 16 MiB text frame dominant.
frames=(
  "json 0 4194304"
  "text 0 2097152"
  "skewed 0 3145728"
  "text 8388608 16777216"
  "json 20971520 3145728"
  "skewed 16777216 4194304"
)
: > bench/big/multi.raw
: > bench/big/multi.zst3
for spec in "${frames[@]}"; do
  read -r shape off size <<<"$spec"
  dd if="bench/corpus/$shape.raw" of=bench/big/.multi_slice bs=4096 \
    skip=$off count=$size iflag=skip_bytes,count_bytes status=none
  cat bench/big/.multi_slice >> bench/big/multi.raw
  # one independent frame per slice, checksums on (the CLI default)
  zstd -q -3 -c bench/big/.multi_slice >> bench/big/multi.zst3
done
rm -f bench/big/.multi_slice
echo "multi.raw: $(stat -c%s bench/big/multi.raw) bytes; multi.zst3: $(stat -c%s bench/big/multi.zst3) bytes"
