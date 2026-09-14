# zstdx-bench

Benchmark and dev-tool harness for the `zstdx` crate: wall-clock A/B against
the `zstd` crate (libzstd bindings), plus correctness/validation tools. Kept
outside the library so dev-only dependencies never leak into it.

- Build/run: `cargo run --release -p zstdx-bench -- <subcommand> [flags]`
  (always `--release`; debug builds measure nothing).
- Corpus prerequisite: `bash bench/gen_corpus.sh` once (32 MiB × 5 shapes:
  `json` / `text` / `skewed` / `random` / `zeros`, plus `.zst1/3/9` variants).
- Timing subcommands share an interleaved A/B harness (`src/common.rs`):
  both sides alternate round by round and the per-round **ratio** is the
  primary verdict (robust to clock drift). Per-side time budget:
  `--budget-ms` (default 500 ms), or the `BENCH_BUDGET_MS` env var.
- Deterministic, instruction-count based benchmarks (valgrind) live in the
  sibling crate [`zstdx-gungraun`](../zstdx-gungraun/Readme.md).

Level vocabulary everywhere: the six ladder levels `fastest/fast/balanced/
best/opt/ultra`, bench-paired with libzstd at the same numeric levels
1/3/9/13/17/19. Corpus shapes are selected with `--shape`, levels with
`--level` (comma-separated lists; omit to mean "all"; `matrix --level` also
accepts numeric levels 1-22).

## Subcommands

### `ratio` — compression-ratio sweep (fast regression gate)

One deterministic pass per cell over **every level × {bulk, streaming} ×
{single-threaded, multi-threaded}** — 120 cells on the full corpus, both
sides (zstdx and libzstd) per cell, no timing loops. Sizes are deterministic,
so `diff` two runs (or two builds) to spot regressions; a geo-mean summary
block is printed last, so `tail -n 40 <log>` sees the verdict. Every zstdx
cell is roundtrip-gated through both decoders before being reported.

Δ% is scale-free: `zstdx_ratio / zstd_ratio - 1`, positive = denser than
libzstd.

```bash
cargo run --release -p zstdx-bench -- ratio                  # full sweep (~min, parallel)
cargo run --release -p zstdx-bench -- ratio --shape json,text --level ultra
cargo run --release -p zstdx-bench -- ratio --mode bulk-st,bulk-mt
cargo run --release -p zstdx-bench -- ratio --no-ref         # our sizes only
```

Flags: `--shape/--level/--mode` filter cells; `--mt-workers` (default 4) is
the worker count both sides use in the mt modes; `--parallel` (default 8) is
the number of cells compressed concurrently. Cells only measure sizes, so
single-threaded cells still run the single-threaded encoder — the pool
merely runs many of them at once. Keep `--parallel` within RAM: each cell
transiently holds a few 32 MiB-scale buffers (higher parallelism is fine on
big machines). Progress goes to stderr (`[n/total] …`), rows and summary to
stdout.

Run this after every encoder change that may alter output sizes; use `dump`
below when output must stay **byte-identical**.

### `matrix` — wide wall-clock A/B vs the zstd crate

The heavyweight tool; narrow it with `--mode` and filters instead of running
`all` while iterating. Sections: `dec-st` (bulk/stream decode),
`dec-mt` (our parallel decode scaling; libzstd has no MT decode),
`enc-st` (all ladder levels, sizes+ratio per cell), `enc-mt` (equal worker
counts, plus a zstd warm-pool reference), `enc-stream` (streaming encode ST
and MT vs libzstd, bulk-MT ceiling). Every cell is roundtrip-gated first.

```bash
cargo run --release -p zstdx-bench -- matrix --mode enc-st --shape json
cargo run --release -p zstdx-bench -- matrix --mode dec-st --budget-ms 2000
cargo run --release -p zstdx-bench -- matrix --mode enc-mt --workers 8 --mt-workers 8
cargo run --release -p zstdx-bench -- matrix --mode enc-st --full-ladder --shape json --level 1,7,22
```

`--full-ladder` swaps the `enc-st` level axis to every numeric level 1-22
with both sides at the same number (`--level` accepts tier names too, which
then select their number; the mt/stream sections keep their tier subsets).
The full 1-22 ladder is extremely heavy: never run it during daily
iteration — run it once before a release. Combine with `--shape`/`--level`/
`--budget-ms` to narrow smoke passes.

### `small` — small-payload encode

Per-call encode throughput for 1 KiB–1 MiB payloads (allocator effects
included), zstdx level `fastest` vs libzstd level 1. `--size` and
`--impl zstdx|zstd` pin one size/implementation for profiler attribution.

```bash
cargo run --release -p zstdx-bench -- small --size 4096
```

### `files` — decode timing on explicit files

Decodes given `.zst` files with a time budget; verifies against the raw
counterpart found next to the file (both `z000033.zst` and `json.zst3`
naming conventions).

```bash
cargo run --release -p zstdx-bench -- files bench/corpus/*.zst3
```

### `prof` — solo profiling loops

Tight single-side loops for `perf`: no reference side, so samples land in
one code path. `prof dec <file.zst> [iters]`, `prof enc <zstd-level> <iters>
<file...>` (`RUZ_CKSUM=1` switches to the checksummed bulk path),
`prof enc-stream <zstd-level> <iters> <file>`.

```bash
cargo run --release -p zstdx-bench -- prof enc 3 20 bench/corpus/text.raw
```

### `dump` — byte-exact output snapshots

Compresses every corpus `.raw` into a directory so two builds can be
compared byte for byte: `dump <dir>` per build, then `cmp -r dir_a dir_b`.
The regression gate for optimizations that must not change the encoder's
output at all. `--all-levels` covers the whole ladder (files tagged
`.l1/.l3/.l6/.l12/.l16/.l19`).

```bash
cargo run --release -p zstdx-bench -- dump /tmp/dump-a
cargo run --release -p zstdx-bench -- dump /tmp/dump-a --all-levels
```

### `corrupt` — corruption smoke test

Flips bytes in a valid frame (`--rounds` copies) and asserts the decoder
errors or succeeds without panicking/hanging, on both the streaming and flat
paths.

```bash
cargo run --release -p zstdx-bench -- corrupt bench/corpus/json.zst3
```

### `mtcheck` — MT decode validation

Decodes every `.zst*` file in a directory with several worker counts
(`--workers`, default 2,4,8,16) and byte-compares against the `zstd` CLI.

```bash
cargo run --release -p zstdx-bench -- mtcheck bench/corpus
```

### `seqstats` — matcher sequence statistics

Sequence statistics and entropy lower bound of our matcher on one corpus
file; `--level` selects. `--ref-frame <file.zst>` adds the differential mode:
the reference frame (e.g. `bench/corpus/text.zst9`) is decoded through the
`seq_dump` decoder hook (enabled in this crate's zstdx dependency) and its
(ll, ml, of-wire) triples diffed against our encoder's parse of the same raw
data — same-position divergences, matches only the reference found (bucketed
by ml and offset-log, with our covering sequence as context), per-side
literal/match/repcode aggregates and entropy bounds, and the cold-start
literal split (first 768 KiB). `--divergences N` caps the printed events
(they also count toward the missed-match context lines).

### `prefill` — `prefill_window` micro

Times the matcher's window prefill per strategy on window-sized corpus
strips (`--shape random,text,json` defaults; `--level` selects strategies).

### `train` — dictionary trainer tooling

Trains a raw-content dictionary with the in-tree trainer from files or
directories (`--size` caps the output), or dumps the content section of a
formatted dict (`--content-of <dict>`) so content selection and
entropy-table seeding can be A/B'd in isolation against `zstd --train`.

```bash
cargo run --release -p zstdx-bench -- train bench/dict_files --out dict.bin --size 16384
```
