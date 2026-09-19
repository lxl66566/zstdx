# Benchmark Methodology

> Test environment: AMD Zen4 32C (AVX-512/BMI2/VAES), visible frequency drift (lscpu scaling ~69-73%), noise ±10% with a slow-drift phase. Rust release + codegen-units=1; the zstd reference side runs on the same baseline instruction set.

## Corpus

`bench/corpus`, 32MiB × 5 shapes: json (semi-structured records) / text (flattened source tree) / skewed (16-letter alphabet) / random (incompressible) / zeros. Decoding uses zst1/3/9 pre-compressed by the zstd CLI, plus zst19 for json/text/skewed. `bench/gen_big.sh` builds the 100 MB system-ELF payload `bench/big/dll100.raw` (+ `.zst1/3/9/19`) — since the 2026-09-19 release pass it is a **formal bench shape** (dll): six-tier encode rows and four decode levels ride in the dec-st/enc-st tables via `matrix --file`, the MT-decode scaling row via `files --threads`. Any further ad-hoc payload also goes through `--file` (a `.zst*` path → dec-st cells, a raw path → enc-st cells).

Pitfalls:

- **The corpus is not comparable**: `gen_corpus.sh`'s text.raw flattens the repo's src tree — any src change changes the corpus. Historically, the two "text shape phase transitions" (the cross-tile match unlocks at ratios 299.6 and 239) both depended on the repeat period landing inside the window range; once src bloats past 768K that shape will collapse again. Consider pinning a tar snapshot.
- Narrow-integer arithmetic in corpus generation: `(i as u8 + 1)` panics on debug overflow at i=255 (u8 truncates first, then +1); compute in usize and cast afterwards.
- **zeros is a free noise-calibration cell** (drifts ±2.6% even with zero code-path overlap).
- **gungraun's 1MiB slice under-measures deep-walk tiers**: the chain/opt tiers' 2MiB window never fills in 1MiB, so walk depth and the far-candidate memory regime barely appear — json.balanced measures ~100 Ir/B on the slice vs ~463 Ir/B on the full 32MiB file (4.6×, measured 2026-09-13). Ir deltas on those tiers validate instruction-count changes only; wall conclusions need the full corpus (`prof`/`matrix`).
- **Cross-session absolute-value drift**: l6 json read 171-179 one session vs 177 recorded the round before — rolling the matcher back to the previous version gave the same readings, confirming no regression. For cross-comparisons trust only same-session A/B (see Criteria discipline).

## Tools (crates/zstdx-bench)

A standalone crate (`cargo run --release -p zstdx-bench -- <subcommand>`); bench/dev tools no longer live inside zstdx's examples (also eliminating the pitfall of the no-default-features check recompiling examples over the release build). The heavy matrix runs rarely; day-to-day iteration runs filtered subsets.

<!-- prettier-ignore -->
| Subcommand | Purpose |
|---|---|
| `ratio` | Compression-ratio sweep: one deterministic pass per cell over every level × bulk/stream × st/mt (120 cells full corpus), zstdx vs libzstd, roundtrip-gated; cells run concurrently on a rayon pool (`--parallel`, default 8 — sizes only, ST cells still use the ST encoder), geo-mean Δ% summary printed last (tail-friendly); sizes diff cleanly across runs/builds |
| `matrix` | Wide-coverage matrix: `--mode dec-st/dec-mt/enc-st/enc-mt/enc-stream/all` covers the five segments, interleaved A/B + roundtrip gate; `--shape/--level/--workers/--mt-workers` filter cells, `--file` appends payloads outside the corpus to dec-st/enc-st, `--budget-ms` caps the per-side budget; `--full-ladder` switches `enc-st` to the numeric 1-22 axis vs libzstd at the same level — extremely heavy, day-to-day runs never use it, it is a once-before-each-release gate |
| `small` | 1KiB-1MiB small loads; `--size`/`--impl` pin a single size and a single impl (for profiler targeting) |
| `files` | Decoding timing for arbitrary .zst files (budget-based; automatically finds the `.raw`/bare-stem reference for verification via any `zst*` suffix) |
| `prof` | Single-side profiling loops: `prof dec <f> [n]` / `prof enc <lvl> <n> <f...>` / `prof enc-stream <lvl> <n> <f>`; `RUZ_CKSUM` env toggles the checksum path |
| `dump` | Corpus byte snapshots, a deterministic regression probe: dump one directory per build and `diff -r`; `--all-levels` covers all 6 levels (tags l1/l3/l6/l12/l16/l19) |
| `corrupt` | Random-corruption smoke test (0 panics), covering streaming + flat paths |
| `mtcheck` | MT decode cross-check against the zstd CLI (`--workers` selects the level) |
| `seqstats` | Sequence statistics and entropy lower bound |
| `prefill` | prefill_window micro (`--shape/--level` selects) |

Common harness (`src/common.rs` in the crate): per-round interleaving, warmup, time budget (`--budget-ms`, default 500ms/side; equivalent to the old BENCH_BUDGET_MS env, which still works), median/mad statistics. The crate's README documents every subcommand's usage.

Ratio-bench workflow note: run `ratio` after every encoder-touching change; it is single-pass and parallel (~1 min full sweep), and its output is fully deterministic — `diff` two captures to attribute size changes. For changes that must not alter output at all, `dump` + `cmp -r` remains the byte-exact gate.

Deleted one-off tools (all covered): bench_compare / bench_encode (matrix `dec-st`/`enc-st` + filters), bench_corpus (files), ab_fast / ab_mt (matrix + filters), compression_ratio (small), stream_cmp (small-block streaming reads for fuzz decode), criterion decode_all (files).

## Criteria discipline

1. **Every single-run conclusion must be re-run a second time**; across time-of-day trust only git-stash back-to-back same-machine A/B.
2. **Deterministic output sizes are the only free, trustworthy A/B signal** (for changes that should not alter output); speed needs multi-round medians or in-process toggles; "gains" within ±10% in a single round are untrustworthy.
3. In a full bench run, the encode segment that comes after the decode segment reads uniformly low in absolute terms (zstd's side also 18% slower) — for cross-comparisons trust dedicated A/B tools; from bench trust only cells stable within ±mad.
4. Late-stage micro changes are judged by `perf stat` **instruction counts** (wall-clock ±5-25% swings are usually layout/scheduling noise); end-to-end changes use interleaved wall-clock.
5. Small-load criteria always use a single-shape, single-size process (allocator cross-contamination can distort 3×).
6. Wall-clock is untrustworthy for 10GB/s-class loads (text.zst3/z9): look at cycles or trust only <9GB/s cells.

## Measurement conventions

- **xslow = our time ÷ zstd time**, >1 = we are slower; speeds are always MiB/s of raw.
- zstd's bulk(slice) decode API is a slow wrapper (per-block re-entry); its real speed lives on the streaming path — **the streaming column is the real gap**; the bulk column is only an API-convention reference.
- The encode side runs with frame checksums disabled on both sides (our xxh64 overhead measured separately: json.Fast on/off = 1.036); final numbers after the sidecar offload include checksums — mind the convention when comparing.
- MT encoding uses "cold thread pool per call" on both sides (zstd has no per-call equivalent API; the warm-pool column is listed for reference only).
- MT decode has no public libzstd API; it is a dimension unique to us (solo scalability table).

## Matrix coverage gaps (to fill later)

Dictionary encode/decode, streaming encoding with a pledged known size, MT scalability on >32MB inputs, zstd CLI multi-file, how our MT decode performs on our own MT-encoded output (restart density is controllable; higher theoretical ceiling), and the **full-matrix re-run** after the tangled-chain fix + opt parser + ratio preservation.
