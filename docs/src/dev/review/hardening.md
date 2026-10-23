# R69 review — encode-path hardening: compile-time invariants, poison walls, paranoid profile

Scope: the structural answer to R68's three-bug postmortem ([encparity](encparity.md)). Every recent memory-safety bug fell into one of three classes, and each class maps to a blind spot of an existing gate: an overread past a buffer's *logical* length that lands inside its *allocation* is invisible to ASAN (the streaming window's slack swallowed the row-tail reads); paired/derived state stored as independently-writable fields desyncs (`tables.len()`/`second` — prose invariants are not re-validated per new path); u64 cursor subtraction wraps under release codegen while debug overflow-checks pass a different execution path. The round erected one wall per blind spot, all zero-cost in the shipped release profile, plus the fuzz-dimension fix for pool-residue bugs (fresh-process replay cannot see them).

## What landed

| Layer | Wall | Commits |
|---|---|---|
| Compile-time (types) | `IdxRange` — wrap-proof spans for same-domain cursor arithmetic; every fill bound, rep-tail scan bar and `insert_max` producer goes through it (or carries a proof comment / loop-bar invariant); per-byte scan loops keep naked arithmetic under their bars by design | 29d8399b |
| Compile-time (structure) | `HeadTables` — the `tables`/`second` pair behind private fields with one mutation entry per resize shape; the R68 lockstep comment rides the single entry; the desync class is unrepresentable | 1b005bdc |
| Compile-time (const proof) | the ladder geometry (`params_for_*`, `adjust_params`) is `const fn`; a `const _` walk of level 0..=22 × src_log 2^10..=2^27 asserts every relation `resize_tables` relies on, and a pair walk pins `LADDER_ROW_COLLISIONS == 132` (same row count at different hash logs — the R68 #3 precondition) cited in the Row arm; a bad ladder edit now fails the build (verified live: a corrupted row and a shifted hash both trip it) | 8a99388c |
| Debug detection | poison margins (`encoding::poison`, cfg(debug_assertions)): the owned streaming window (fill at `block_tail` staging, assert at block commit / call exits), the ST/MT stream cores' `staged`/`output`/per-job/donation slack, LDM split staging batches, pooled `BlockScratch` staging — an intra-allocation touch past a logical length panics with a checkpoint name | 8e9e7e11, 659e465d, 827f77d3, 7ccf6e5e, 97e63721, b323d67b |
| Release-codegen detection | `[profile.paranoid]` (inherits release + `debug-assertions` + `overflow-checks`) — the walls and overflow checks over release codegen, where wrap bugs actually manifest; CI job `.github/workflows/paranoid.yml`; paranoid dump proven byte-equal to release | 5bb703e0 |
| Fuzz dimensions | `encode`/`encode_stream` targets walk a multi-payload ladder ([level, opts, k, chunks...], chunk i at level `(level+i)%23` through one thread — pooled carryover is now a fuzz dimension); artifact walker runs forward + reverse in-process; `pooled_ladder_sweep` pins every level pair at the `adjust_params` clamp boundaries | b75081a6, 16b1209e, 4d73e225 |

The sweep earned its keep immediately: it found two pre-existing MT bugs (below) that every prior gate missed.

## The sweep's two real bugs (pre-existing, fixed)

Both reproduced identically on the pre-hardening base, both in the flush + short-window corner no prior gate drove:

1. **`run_inline_job`'s view anchored at the buffer, not the strip head** (a54737e4): the inline twin sliced `buf[..hi - buf_base]` while its `first` counts from `strip_lo` (every posted job's convention). After a mid-stream flush under a window shorter than the flush offset (forced window_log 9/10), the whole post-flush span re-encoded stale earlier bytes — the frame decodes fine; only a checksum/byte compare catches it. **Silent corruption without checksum.**
2. **A flush-regridded Growing-grid post could cross its epoch end** (c248f074): `post_ready`'s `end = job_start + epoch_size` vs a gate keyed on the epoch boundary; a mid-epoch flush parks `job_start` where the two disagree — release would freeze a view reading never-staged bytes. `post_job`'s debug assert caught it under the walls.

Mechanisms in [pitfalls/encoding](../pitfalls/encoding.md). Fixes are no-ops on untouched grids (dumps byte-identical below).

## Gate numbers

- Tests: debug 416+12+21 · release 411+12+21 · no-default-features 240 · zstdx-cli 99 · **paranoid 416+12+21** — all green with every wall armed.
- Dump byte-differential vs the pre-round base: bulk `--all-levels` and stream `--all-levels` grids **byte-identical** (the two MT fixes included — their corner is off the dump lattice, pinned by tests instead).
- gungraun Ir vs base: fastest json **+0.004%** (flat), balanced json **+0.438%** — decomposed to `HeadTables` accessor placement on the once-per-parse chain path (a semantically identical form of the rep-tail body swung ±3% Ir without executing: register-allocation placement, not algorithmic cost; the landed shape keeps the base loop and clamps post-emit — 68c0d81c). Wall on the memory-bound chain tier expected flat; recovery is todo 24a.
- Fuzz smokes (ASAN, merged tree): `encode` 15,366 runs / `encode_stream` 4,300 runs, zero findings.
- Poison overhead: debug suite 54.0→65.2s (walls' fill/assert inside the heavy MT tests); the sweep itself ~1s (966 cells, option bits rotated one per cell). Release unaffected: `nm` on the release rlib shows zero wall symbols.

## Honest coverage (what the walls do not see)

- The committed-len→staged-end zone of the owned window is **caller-live** by frame_compressor's pre-split carry convention — unpoisonable without a caller-contract change (full-block commits still land the wall directly past the block end).
- The block_enc bitwriter's `hot_push_raw` deliberately writes past the semantic end inside its reserved span (documented discipline); the MT accumulate buffer's spare is the read pump's destination; LDM tables/buckets and the matcher's hash/row/tree tables are exact-sized (no slack); bulk input slices are caller-borrowed (ASAN-visible at the allocation end — the fuzzer's territory).
- `parse_chain`'s `next - 512` wrap is flagged byte-load-bearing with a do-not-fix note (29d8399b).
- Poison asserts fire at checkpoints; a touch whose evidence is legitimately overwritten before the next checkpoint escapes. Checkpoints were placed at every staging boundary to keep the window short.

## Pitfall-scan: which historical bugs each wall would have killed

| Historical bug | Wall |
|---|---|
| reserve baseline (len vs pos) | writer-API consolidation — still open, todo 24b menu |
| HASH_READ tail lockstep / catch-up fill margin | poison (window past logical end) |
| insert_max underflow, `(end-p)&1` peel, rep-tail bars | IdxRange (empty-span floor) |
| dist-0 self-compare | wrapping-resolve helper (landed R4x); NonZero type is todo 24b |
| origin sentinel phantom | coordinate-cap invariant (landed R43 epoch roll) |
| `second`/`tables.len()` desync | HeadTables (unrepresentable) + const collision count |
| head-link induction break | pooled tests (R68 landed); typestate not attempted |
| exit-contract divergence | skill checklist (not type-able cheaply) |
| MT inline view / epoch straddle | poison sweep + pooled sweep (this round) |

## Follow-ups

Todo 24: balanced-Ir placement recovery; the remaining compile-time menu (cross-domain index newtypes, NonZero distances, `Option<Candidate>` sentinels, enum rep-bases); the unpoisonable carry zone; the `-max_len` >4096 campaign (carried from todo 23b — 23a closed here with `encode_stream`).

## Commits

- 8e9e7e11 enc: debug poison-margin helpers
- 29d8399b enc: wrap-proof IdxRange for matcher span arithmetic
- 1b005bdc enc: HeadTables owns the fused head/second table pair
- 659e465d enc: poison margins on the owned streaming window
- 8a99388c enc: const proof of the ladder's table geometry
- 68c0d81c enc: keep the rep-tail loops' base shape under the wrap clamp
- 827f77d3 / 7ccf6e5e / 97e63721 / b323d67b enc: poison walls over the ST/MT stream cores, ldm staging, block scratch
- d641e06c test: poison-wall sweep over both cores and the level ladder
- a54737e4 fix: anchor the mt inline job's view at the strip head
- c248f074 fix: clamp a flush-regridded growing-grid post to its epoch end
- 4f0eef59 fix: link std in the poison helper's no-default-features tests
- 60f37fa5 fmt: zstdx-fuzz targets
- b75081a6 fuzz: encode target walks multi-payload pooled level ladder
- 4d73e225 test: pooled ladder sweep at the adjust_params clamp boundaries
- 16b1209e fuzz: encode_stream target on the multi-payload ladder layout
- 5bb703e0 build: paranoid profile + CI job running it
- merges 4fdc6016 / cb2f1154 / e2e3a536
