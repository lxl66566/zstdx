#!/usr/bin/env bash
# Parallel ASAN fuzz smoke: all cores on one target, bounded wall time.
#
# A bare `cargo fuzz run` is single-process (one core), its
# `-max_total_time` excludes the seed replay, and with `-jobs=W` every
# child replays the whole corpus itself under contention — a fat corpus
# makes a "2-minute" smoke take 5+. This script splits the work instead:
# replay is distributed over W single-shot processes (crash detection only,
# coverage union irrelevant), fuzzing starts from a rotating 1/W corpus
# slice so every child's INITED is seconds.
#
# Usage:             smoke.sh <target> [seconds=60] [workers<=16]
# QUICK=1 smoke.sh … skip the full replay (fuzz from slice + artifacts
#                    only) — the tight iteration loop; the slice rotates
#                    per run so repeated quick smokes still sweep the
#                    corpus.
set -euo pipefail

target=${1:?usage: smoke.sh <target> [seconds] [workers]}
secs=${2:-60}
workers=${3:-$(nproc)}
# ASAN workers sit at ~200-300 MB RSS each; 13 GB box.
(( workers > 16 )) && workers=16

root=$(cd "$(dirname "$0")/../.." && pwd)
fuzzdir=$root/crates/zstdx-fuzz
corpus=$fuzzdir/corpus/$target
artifacts=$fuzzdir/artifacts/$target
bin=$fuzzdir/target/x86_64-unknown-linux-gnu/release/$target
[[ -d $corpus ]] || { echo "no corpus dir for $target: $corpus" >&2; exit 1; }

cargo fuzz build --fuzz-dir "$fuzzdir" -s address -O >/dev/null

# Hold the bench flock: W ASAN workers saturate the box and would perturb
# any concurrently running matrix/ratio/wall A/B bench.
exec 9>/root/programs/fork/zstd-bench.lock
flock 9

scratch=$(mktemp -d /tmp/zstdx-fuzz-smoke.XXXXXX)
tmp=$fuzzdir/corpus/.smoke-tmp.$$   # same device as the corpus: hardlinks
mkdir -p "$tmp"
finish() {
  mkdir -p "$artifacts"
  for a in "$scratch"/crash-* "$scratch"/leak-* "$scratch"/timeout-* \
           "$tmp"/replay-*/crash-* "$tmp"/replay-*/leak-* "$tmp"/replay-*/timeout-*; do
    [[ -e $a ]] && mv "$a" "$artifacts/"
  done
  rm -rf "$scratch" "$tmp"
  rm -f "$root"/fuzz-*.log
}
trap finish EXIT

# Split the corpus into W same-device hardlink slices once; replay and fuzz
# phases both consume them.
pick=$((RANDOM % workers)) n=0
for f in "$corpus"/*; do
  s=$((n % workers)); n=$((n + 1))
  d=$tmp/slice$s; [[ -d $d ]] || mkdir -p "$d"
  ln "$f" "$d/"
done

t0=$SECONDS
if [[ ${QUICK:-0} != 1 ]]; then
  # Phase 1: W single-shot replays (-runs=0 = load and exit), one slice
  # each — the full corpus executed exactly once, in parallel.
  for i in $(seq 0 $((workers - 1))); do
    d=$tmp/replay$i; mkdir -p "$d"
    (cd "$d" && "$bin" -runs=0 -rss_limit_mb=8192 \
       -artifact_prefix="$d/" "$tmp/slice$i" > "$d/log" 2>&1) &
  done
  wait
  replayed=$n
else
  replayed=0
fi

# Phase 2: W parallel fuzzers from one rotating slice (+ artifacts), the
# -jobs children write fuzz-<n>.log to the CWD, hence the scratch dir.
seed=$tmp/slice$pick
[[ -d $artifacts ]] && seed_args=("$seed" "$artifacts") || seed_args=("$seed")
cd "$scratch"
"$bin" -jobs="$workers" -workers="$workers" \
  -max_total_time="$secs" -rss_limit_mb=8192 \
  -artifact_prefix="$scratch/" -print_final_stats=1 \
  "${seed_args[@]}"

echo "smoke wall: $((SECONDS - t0))s = $replayed seeds replayed in parallel + ${secs}s x $workers fuzz workers"
