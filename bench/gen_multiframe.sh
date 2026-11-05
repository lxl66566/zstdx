# Multi-frame decode payloads for the frame-lane cell: concatenated
# streams of independent zstd frames over mixed corpus shapes, the
# grouping-policy probes of docs/src/dev/perf/decoding.md. Requires
# bench/gen_corpus.sh first. Writes <name>.raw and <name>.zst3 (level 3,
# checksums on, one frame per slice) under bench/big/, which is gitignored.
#
#   multi.zst3          32 MiB, six frames, one dominant 16 MiB text frame
#   multi_small.zst3    ~6.2 MiB, 24 ascending small frames (64-512 KiB)
#   multi_balanced.zst3 ~33 MiB, six equal 5.5 MiB frames
#   multi_twodom.zst3   ~26 MiB, two adjacent 12 MiB frames then eight tiny
# Usage: bash bench/gen_multiframe.sh   (output is gitignored)
set -euo pipefail
cd "$(dirname "$0")/.."
for f in json text skewed random; do
  [ -f "bench/corpus/$f.raw" ] || { echo "run bench/gen_corpus.sh first" >&2; exit 1; }
done
mkdir -p bench/big

# emit <name> <"shape offset size">...: one independent zstd frame per
# slice appended to both <name>.raw and <name>.zst3.
emit() {
    local name=$1; shift
    : > "bench/big/$name.raw"
    : > "bench/big/$name.zst3"
    for spec in "$@"; do
        read -r shape off size <<<"$spec"
        dd if="bench/corpus/$shape.raw" of=bench/big/.m_slice bs=4096 \
            skip=$off count=$size iflag=skip_bytes,count_bytes status=none
        cat bench/big/.m_slice >> "bench/big/$name.raw"
        zstd -q -3 -c bench/big/.m_slice >> "bench/big/$name.zst3"
    done
    rm -f bench/big/.m_slice
    echo "$name.raw: $(stat -c%s "bench/big/$name.raw") bytes; $name.zst3: $(stat -c%s "bench/big/$name.zst3") bytes"
}

# One dominant frame: 4 + 2 + 3 + 16 + 3 + 4 = 32 MiB.
emit multi \
  "json 0 4194304" \
  "text 0 2097152" \
  "skewed 0 3145728" \
  "text 8388608 16777216" \
  "json 20971520 3145728" \
  "skewed 16777216 4194304"

# Many small frames, ascending size so consecutive-by-count grouping
# skews: count-equal halves carry unequal bytes.
K=1024
multi_small_specs=()
small_kib=(64 80 88 96 112 128 144 160 176 192 208 224 240 256 288 320 352 384 416 448 480 496 512 512)
shapes=(json text skewed random)
for i in "${!small_kib[@]}"; do
    shape=${shapes[$((i % 4))]}
    off=$(( (i % 4) * 8388608 + i * 131072 ))
    multi_small_specs+=("$shape $off $((small_kib[i] * K))")
done
emit multi_small "${multi_small_specs[@]}"

# Balanced: six equal 5.5 MiB frames, shape pairs at disjoint offsets.
emit multi_balanced \
  "text 0 5767168" \
  "json 0 5767168" \
  "skewed 0 5767168" \
  "text 12582912 5767168" \
  "json 12582912 5767168" \
  "skewed 12582912 5767168"

# Two dominant frames adjacent, then eight tiny frames: consecutive
# grouping must isolate the dominants to balance this shape.
emit multi_twodom \
  "text 0 12582912" \
  "json 0 12582912" \
  "skewed 0 65536" \
  "random 0 98304" \
  "json 6291456 131072" \
  "skewed 6291456 163840" \
  "text 6291456 196608" \
  "random 6291456 229376" \
  "json 12582912 262144" \
  "text 12582912 327680"
