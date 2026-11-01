#!/usr/bin/env bash
# Build the zstdx-ffi cdylib and run the C harness against it, plus the
# interop matrix with the reference libzstd (static) and the zstd CLI.
# Usage: bash tests/run-c.sh [repo-root]
set -euo pipefail

root="${1:-$(cd "$(dirname "$0")/../../.." && pwd)}"
ref="${ZSTD_REF:-/root/programs/fork/zstd}"
work="$(mktemp -d /tmp/zstdx-ffi-c.XXXXXX)"
trap 'rm -rf "$work"' EXIT

# The harness expectations and the ffi size constants embed the reference's
# struct sizes, which move with the build target: a default of x86-64-v3 or
# newer defines __BMI2__, DYNAMIC_BMI2 turns off, the CCtx bmi2 field
# disappears and every compress-side estimate shifts by -8. Build the
# pinned tree from scratch at the x86-64 baseline march so every host and
# runner image produces the same libzstd.a instead of trusting a prebuilt
# one built at whatever the local toolchain defaults to.
reflib="$work/zstd-ref-lib"
mkdir "$reflib"
git -C "$ref" archive HEAD lib | tar -x -C "$reflib"
make -C "$reflib/lib" -j"$(nproc)" libzstd.a MOREFLAGS=-march=x86-64 >/dev/null

cd "$root"
cargo build -p zstdx-ffi --release >/dev/null
lib="$root/target/release/libzstd.so"
test -f "$lib" || { echo "missing $lib"; exit 1; }

exported=$(nm -D "$lib" | grep -c " T ZSTD_")
echo "libzstd.so exports $exported ZSTD_* symbols"

cc_common=(-O1 -Wall -Wextra -DZSTD_STATIC_LINKING_ONLY -I "$reflib/lib")
gcc "${cc_common[@]}" "$root/crates/zstdx-ffi/tests/harness.c" -o "$work/self" \
    -L "$root/target/release" -l:libzstd.so
gcc "${cc_common[@]}" "$root/crates/zstdx-ffi/tests/harness.c" -o "$work/ref" \
    "$reflib/lib/libzstd.a" -lpthread -lm

# The same self-tests against the reference library: the suite doubles as
# a parity check, so expectations that drift from libzstd fail here first.
echo "== self-tests (reference libzstd.a) =="
"$work/ref" || { echo "reference self-tests failed: harness expectations drifted"; exit 1; }

echo "== self-tests (zstdx libzstd.so) =="
LD_LIBRARY_PATH="$root/target/release" "$work/self"

# Interop corpus: json + text slices from the bench corpus, plus a binary mix.
head -c 8388608 "$root/bench/corpus/json.raw" > "$work/json.raw" 2>/dev/null || \
    head -c 8388608 "$root/bench/corpus/text.raw" > "$work/json.raw"
head -c 2097152 "$root/bench/corpus/text.raw" > "$work/text.raw" 2>/dev/null || true
test -s "$work/json.raw" || { echo "no corpus found"; exit 1; }

status=0
for shape in json text; do
    raw="$work/$shape.raw"
    test -s "$raw" || continue
    for level in 1 9 19; do
        echo "== interop $shape level $level: ref -> self =="
        "$work/ref" produce "$raw" "$work/ref.zst" || status=1
        LD_LIBRARY_PATH="$root/target/release" "$work/self" consume "$work/ref.zst" "$raw" || status=1
        echo "== interop $shape level $level: self -> ref =="
        LD_LIBRARY_PATH="$root/target/release" "$work/self" produce-stream "$raw" "$work/self.zst" "$level" || status=1
        "$work/ref" consume "$work/self.zst" "$raw" || status=1
    done
done

# The system zstd CLI decodes a self-produced frame, and vice versa.
if command -v zstd >/dev/null; then
    echo "== interop: self -> zstd CLI =="
    LD_LIBRARY_PATH="$root/target/release" "$work/self" produce-stream "$work/json.raw" "$work/cli.zst" 9
    zstd -d -q -f -o "$work/cli.out" "$work/cli.zst"
    cmp -s "$work/cli.out" "$work/json.raw" && echo "cli decode OK" || { echo "cli decode MISMATCH"; status=1; }
    echo "== interop: zstd CLI -> self =="
    zstd -q -f -19 -o "$work/cli.zst" "$work/json.raw"
    LD_LIBRARY_PATH="$root/target/release" "$work/self" consume "$work/cli.zst" "$work/json.raw" || status=1
else
    echo "zstd CLI not found; skipping CLI interop"
fi

exit $status
