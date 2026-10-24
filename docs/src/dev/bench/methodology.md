# Benchmark Methodology

This page explains how the benchmark suite runs, how to read its numbers, and which comparison rules make a result trustworthy. It is written as a procedure: each rule states what to do and why it matters. The x notation is our time divided by the reference time, so values above 1 mean we are slower, and speeds are MiB/s of raw input unless stated otherwise.

## Environment

The bench machine is an AMD Zen4 part with 32 cores and AVX-512, BMI2 and VAES. Its visible frequency drifts, with `lscpu` showing a scaling factor of about 69% to 73%, and run-to-run noise is ±10% with a slow-drift phase. Rust runs in release with codegen-units set to 1, and the zstd reference side runs on the same baseline instruction set.

## Corpus

`bench/corpus` holds five 32 MiB shapes: json with semi-structured records, text as a flattened source tree, skewed with a 16-letter alphabet, random as incompressible data, and zeros. Decoding uses zst1, zst3 and zst9 files pre-compressed by the zstd CLI, plus zst19 for json, text and skewed. `bench/gen_big.sh` builds the 100 MB system-ELF payload `bench/big/dll100.raw` with its `.zst1/3/9/19` variants. Since the 2026-09-19 release pass, dll is a formal bench shape: six-tier encode rows and four decode levels ride in the `dec-st` and `enc-st` tables through `matrix --file`, and the MT decode scaling row rides through `files --threads`. Any further ad-hoc payload also goes through `--file`, where a `.zst*` path produces decode cells and a `.raw` path produces encode cells.

Four properties of this corpus are load-bearing. The corpus is not comparable across source changes, because `gen_corpus.sh` flattens the repository's source tree into text.raw. Historically the two text shape phase transitions, at ratios 299.6 and 239, both depended on the repeat period landing inside the window range, and once the source grows past 768K that shape will collapse again, so consider pinning a tar snapshot. Corpus generation must compute narrow-integer arithmetic in `usize` and cast afterwards, because `(i as u8 + 1)` truncates at i=255 and then panics on debug overflow. Zeros is a free noise-calibration cell that drifts ±2.6% even with zero code-path overlap. Gungraun's 1 MiB slice under-measures the deep-walk tiers: the chain and opt tiers' 2 MiB window never fills on the slice, so walk depth and the far-candidate memory regime barely appear, and json.balanced measures about 100 instructions per byte on the slice against about 463 per byte on the full 32 MiB file, a 4.6x difference measured 2026-09-13. Instruction deltas on those tiers therefore validate instruction-count changes only, and wall conclusions need the full corpus through `prof` or `matrix`.

Cross-session absolute values drift. Level 6 json read 171 to 179 MiB/s in one session against 177 recorded the round before, and rolling the matcher back to the previous version gave the same readings, which confirms there was no regression. For cross-comparisons, trust only same-session A/B runs.

## Tools

The tools are a standalone crate run as `cargo run --release -p zstdx-bench -- <subcommand>`. Bench and development tools no longer live inside zstdx's examples, which also removes the pitfall of the no-default-features check recompiling examples over the release build. Heavy matrix runs are rare, and day-to-day iteration uses filtered subsets.

| Subcommand | Purpose |
|---|---|
| `ratio` | Compression-ratio sweep with one deterministic pass per cell over every level, bulk and stream modes and single-thread and MT modes, 120 cells on the full corpus, zstdx against libzstd, roundtrip-gated. Cells run concurrently on a rayon pool through `--parallel`, default 8, because sizes only are compared and single-thread cells still use the single-thread encoder. The geo-mean delta is printed last for tail-friendly output, and sizes diff cleanly across runs and builds |
| `matrix` | Wide-coverage matrix. `--mode dec-st/dec-mt/enc-st/enc-mt/enc-stream/all` covers the five segments with interleaved A/B and a roundtrip gate. `--shape/--level/--workers/--mt-workers` filter cells, `--file` appends payloads outside the corpus to the `dec-st` and `enc-st` cells, and `--budget-ms` caps the per-side budget. `--full-ladder` switches encode single-thread to the numeric 1-22 axis against libzstd at the same level. It is extremely heavy, day-to-day runs never use it, and it is a once-before-each-release gate |
| `small` | Small loads from 1 KiB to 1 MiB. `--size` and `--impl` pin a single size and implementation for profiler targeting |
| `files` | Decoding timing for arbitrary `.zst` files, budget-based, with automatic reference discovery for verification through any `zst*` suffix |
| `prof` | Single-side profiling loops: `prof dec <f> [n]`, `prof enc <lvl> <n> <f...>` and `prof enc-stream <lvl> <n> <f>`. The `RUZ_CKSUM` environment variable toggles the checksum path |
| `dump` | Corpus byte snapshots as a deterministic regression probe. Dump one directory per build and run `diff -r`. `--all-levels` covers all six levels tagged l1, l3, l6, l12, l16 and l19, and `--stream` swaps in the streaming cores as the one-command stream differential gate |
| `corrupt` | Random-corruption smoke test that must produce zero panics, covering streaming and flat paths |
| `mtcheck` | MT decode cross-check against the zstd CLI, with `--workers` selecting the level |
| `seqstats` | Sequence statistics and entropy lower bound |
| `prefill` | `prefill_window` micro-benchmark, with `--shape` and `--level` selecting |

The common harness in `src/common.rs` provides per-round interleaving, warmup, a time budget through `--budget-ms` with a default of 500 ms per side, and median and MAD statistics. `--budget-ms` is equivalent to the older `BENCH_BUDGET_MS` environment variable, which still works. The crate's README documents every subcommand's usage.

## Procedure for a pass

1. Lock the machine and run the whole pass in one session. Use the shared lock file so a co-tenant run cannot contaminate a cell, because cross-session absolutes drift.
2. Confirm the workspace tests are green and build in release with codegen-units set to 1. The measurement is only meaningful on the shipped profile.
3. Pick the smallest subcommand that answers the question. Use `ratio` for output sizes, `matrix` with a mode filter for speed, `small` for payloads from 1 KiB to 1 MiB, and `prof` for single-side attribution.
4. Gate output changes with sizes first. Run `ratio` after every encoder-touching change because it is single-pass, parallel and deterministic, taking about a minute for the full sweep, and diff two captures to attribute size changes. For changes that must not alter output at all, `dump` plus `cmp -r` is the byte-exact gate.
5. Gate stream-path changes with the stream differential dump. Run `dump <dir> --all-levels --stream` per build and `diff -r` the two directories, which covers 120 frames from 5 shapes by 6 levels by four stream modes. Every frame is roundtrip and interop-gated in-tool. Use this instead of scratch emit-frame loops.
6. Measure speed with interleaving, warmup and medians, never with a single run. Repeat the measurement a second time before drawing a conclusion, and across times of day trust only a back-to-back same-machine A/B.
7. Archive the raw table when a pass completes, and record the commit, date, machine, flags and per-side budget beside it.

The stream differential gate has four sanctioned divergences from the bulk dump. Unpledged MT rides the Growing grid keyed on `--workers`, so never diff across worker counts. Pledged MT differs only on the Balanced row, which covers skewed and text level 9, because the reach probe's stream and bulk verdicts split. Pledged single-thread differs by the 4-byte FCS header field. The open cells have no byte-equal bulk counterpart.

Several one-off tools were deleted because the remaining subcommands cover them. `bench_compare` and `bench_encode` map to `matrix` decode and encode modes with filters, `bench_corpus` maps to `files`, `ab_fast` and `ab_mt` map to `matrix` with filters, `compression_ratio` maps to `small`, `stream_cmp` maps to `small` with small-block streaming reads for fuzz decode, and criterion's `decode_all` maps to `files`.

## Criteria discipline

1. Re-run every single-run conclusion a second time. Across times of day, trust only a git-stash back-to-back same-machine A/B, because the machine drifts between sessions.
2. Treat deterministic output sizes as the only free and trustworthy A/B signal for changes that should not alter output. Speed needs multi-round medians or in-process toggles, and a single-round gain within ±10% is untrustworthy.
3. In a full bench run, the encode segment that follows the decode segment reads uniformly low in absolute terms, and zstd's side is also about 18% slower there. Use dedicated A/B tools for cross-comparisons, and from a full run trust only cells stable within ±MAD.
4. Judge late-stage micro changes by `perf stat` instruction counts, because wall-clock swings of ±5% to ±25% are usually layout or scheduling noise. Judge end-to-end changes by interleaved wall-clock. Gungraun instruction comparisons are valid only within one build lineage, because rebuilding the same source in a fresh worktree shifts crate hashes and inlining by up to about ±0.9% of instructions. The measurement on 2026-09-23 read json fastest at 30.98M against 31.34M for identical source. Re-baseline the comparison column in the same worktree before reading a delta.
5. Run small-load criteria as a single-shape, single-size process. Allocator cross-contamination can distort the result by 3x.
6. For loads in the 10 GB/s class such as text.zst3 and text.zst9, look at cycles rather than wall-clock, or trust only cells below 9 GB/s.

## Measurement conventions

The `xslow` ratio is our time divided by zstd time, so a value above 1 means we are slower, and speeds are always MiB/s of raw input. Zstd's bulk slice decode API is a slow wrapper that re-enters per block, so its real speed lives on the streaming path: the streaming column is the real gap and the bulk column is only an API-convention reference.

`ZSTDX_SIMD_FORCE=auto|avx512|avx2|scalar` clamps every runtime-dispatched encode kernel to a narrower tier through `common::simd`, which is parsed once per process, and an unset value clamps nothing. This is the direct way to measure the intermediate SIMD tiers on an AVX-512 box, and the dump differential gate re-runs under each tier. BMI2-only sites sit outside the clamp because there is no vector width to tier and every AVX2-era CPU carries them. Tier A/Bs run as same-binary environment flips through `prof` solo with interleaved position-rotating rounds, so the binary is bit-identical and only kernel selection moves, which makes this the drift-controlled form of the comparison.

The encode side runs with frame checksums disabled on both sides. Our xxh64 overhead is measured separately, with json.Fast on versus off at 1.036. Final numbers after the sidecar offload include checksums, so mind the convention when comparing. MT encoding uses a cold thread pool per call on both sides, because zstd has no per-call equivalent API, and the warm-pool column is listed for reference only. MT decode has no public libzstd API, so it is a dimension unique to us and measured as a solo scalability table.

Decode measurements need layout-lottery discipline. Any machine-code change inside a fused decode symbol re-rolls register allocation and whole-binary placement, and the roll is a lottery: the same source shapes drew text-stream +2.8% to +5% on the 2026-09-20 rolls and plus or minus 0% on the 2026-10-07 ones, which included the fused bit-read re-landing and a controlled same-day re-roll through an entry-load order swap. Every such change must clear a text-stream A/B/B/A gate, and solo `perf stat` separates the intrinsic effect in instructions from the placement draw in cycles at equal instructions, with text solo cycles at ±0.02% unaligned and ±0.16% aligned across the controlled re-roll.

`-C llvm-args=-align-all-functions=5` was evaluated on 2026-10-07 and rejected as a workspace default. It pins function phases but is itself a layout draw with its own cost, reading text +2.6%, json +1.0% and skewed +0.4% solo cycles against the same-source default build. It also shifts every published x and breaks snapshot comparability, and it cannot pin layout order, which is still re-rolled per change. No drift reduction was demonstrable in-session because no adverse draw occurred under either mode.

## Coverage gaps

The matrix has no dictionary cells. Dictionary A/Bs run on the systemd fixture, whose recipe and gates are on the [encoding page](../perf/encoding.md), and they do not go through `matrix` or `ratio`.
