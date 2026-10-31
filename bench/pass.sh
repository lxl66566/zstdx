#!/usr/bin/env bash
# Full bench pass driver: build once, then run every section against the
# prebuilt binary with per-section wall times and a summary table. The
# whole pass runs under the shared machine lock, one session, so co-tenant
# runs cannot contaminate cells (see docs/src/dev/bench/methodology.md).
#
# Usage:
#   bench/pass.sh [--out DIR] [section ...]
#
# Sections default to the daily set: dec-st dec-mt files-dll enc-st enc-mt8
# enc-mt16 enc-stream small ratio. The name "all" replaces the list with
# the daily set plus the full numeric ladder per shape, the release gate.
# Each section's output lands in <out>/<section>.log; a summary table with
# per-section walls prints at the end. dll100 payloads join automatically
# when bench/big carries them; generate with bench/gen_big.sh.
#
# The output directory defaults under bench/scratch/, which is gitignored;
# pass --out to place it elsewhere.

set -euo pipefail
cd "$(dirname "$0")/.."

BIN=target/release/zstdx-bench
LOCK=/root/programs/fork/zstd-bench.lock

out=bench/scratch/pass-$(date +%Y%m%d-%H%M%S)
sections=()
while [ $# -gt 0 ]; do
    case "$1" in
        --out) out=$2; shift 2 ;;
        -h|--help) grep '^#' "$0" | sed '1d'; exit 0 ;;
        *) sections+=("$1"); shift ;;
    esac
done
DAILY=(dec-st dec-mt files-dll enc-st enc-mt8 enc-mt16 enc-stream small ratio)
if [ ${#sections[@]} -eq 0 ]; then
    sections=("${DAILY[@]}")
elif [ "${sections[0]}" = "all" ]; then
    sections=("${DAILY[@]}" ladder-json ladder-text ladder-skewed ladder-random ladder-zeros)
fi

# payloads outside the corpus join when present
DEC_FILES=()
ENC_FILES=()
DLL_ZST3=""
for f in bench/big/dll100.zst1 bench/big/dll100.zst3 bench/big/dll100.zst9 bench/big/dll100.zst19; do
    [ -f "$f" ] && DEC_FILES+=(--file "$f")
done
[ -f bench/big/dll100.raw ] && ENC_FILES+=(--file bench/big/dll100.raw)
[ -f bench/big/dll100.zst3 ] && DLL_ZST3=bench/big/dll100.zst3

mkdir -p "$out"
echo "== building (release, one build for every section) =="
CARGO_BUILD_JOBS=${CARGO_BUILD_JOBS:-8} cargo build --release -p zstdx-bench

# One section's run: dispatches by name, stdout to $1, stderr to $2.
run_section() {
    local sec=$1 stdout=$2 stderr=$3
    case "$sec" in
        dec-st)     "$BIN" matrix --mode dec-st "${DEC_FILES[@]}" ;;
        dec-mt)     "$BIN" matrix --mode dec-mt ;;
        files-dll)
            [ -n "$DLL_ZST3" ] || return 2
            "$BIN" files --threads 2,4,8,16 "$DLL_ZST3" ;;
        enc-st)     "$BIN" matrix --mode enc-st "${ENC_FILES[@]}" ;;
        enc-mt8)    "$BIN" matrix --mode enc-mt --workers 8 --mt-workers 8 ;;
        enc-mt16)   "$BIN" matrix --mode enc-mt --workers 16 --mt-workers 16 ;;
        enc-stream) "$BIN" matrix --mode enc-stream --mt-workers 8 "${ENC_FILES[@]}" ;;
        small)      "$BIN" small --shape json,text --level 1,3,9 ;;
        ratio)      "$BIN" ratio ;;
        ladder-*)   "$BIN" matrix --mode enc-st --full-ladder --shape "${sec#ladder-}" ;;
        *)          echo "unknown section: $sec" >&2; return 1 ;;
    esac >"$stdout" 2>"$stderr"
}

# one lock hold for the whole pass: a bench session must not interleave
# with another multi-core run
exec 9>"$LOCK"
flock 9
echo "== pass start: ${sections[*]} -> $out =="

names=()
walls=()
total=0
for sec in "${sections[@]}"; do
    echo "== $sec"
    t0=${EPOCHREALTIME/.}
    rc=0
    run_section "$sec" "$out/$sec.log" "$out/$sec.err" || rc=$?
    if [ "$rc" -eq 2 ]; then
        echo "   skipped (payload absent)"
        continue
    fi
    [ "$rc" -ne 0 ] && exit "$rc"
    t1=${EPOCHREALTIME/.}
    ms=$(((10#$t1 - 10#$t0) / 1000))
    names+=("$sec")
    walls+=("$ms")
    total=$((total + ms))
    echo "   wall ${ms} ms"
done

echo "== summary =="
for i in "${!names[@]}"; do
    printf '%-14s %8d ms  %s
' "${names[$i]}" "${walls[$i]}" "$out/${names[$i]}.log"
done
printf '%-14s %8d ms\n' "total" "$total"
