# Encode-path hardening

This page records the structural answer to the three bugs found in the [encoder edges](encparity.md) review. Each recent memory-safety bug fell into one of three classes, and each class exposed a blind spot of an existing gate. The round built one wall per blind spot, all zero-cost in the shipped release profile, and added the fuzz dimension that catches pool-residue bugs, which fresh-process replay cannot see.

## Blind spots

An overread past a buffer's logical length that lands inside its allocation is invisible to ASAN. The streaming window's slack swallowed the row-tail reads. Paired or derived state stored as independently writable fields can desync, as `tables.len()` and `second` did, because prose invariants are not re-validated per new path. A u64 cursor subtraction wraps under release codegen, while debug overflow checks pass through a different execution path.

## Walls

### Compile-time types

`IdxRange` provides wrap-proof spans for same-domain cursor arithmetic. Every fill bound, rep-tail scan bar and `insert_max` producer goes through it, or carries a proof comment or a loop-bar invariant. The per-byte scan loops keep naked arithmetic under their bars by design.

The same pattern now pins the merged sequence add width in `seq_codes`: a `const _` grid over every LL/ML base pair that fits one block asserts `ln + mn <= 31`, so the block encoder's per-field split push (`push_add`) is provably sufficient under the w27 offset cap, and `hot_push_raw` debug-asserts the 56-bit payload cap at its entry.

The btlazy selection's absent candidates are unrepresentable since 2026-10-24. `Found`'s two argmaxes are `Option`s, with `Scored` pairing the literal-offset candidate and its score so the score cannot outlive the candidate, and the driver's incumbent is an `Option<Scored>` destructured at the lazy bar. Every construction site guarantees `len >= MIN_MATCH`, which `scale_value`'s assert now pins exactly. Gungraun reads best json and text at -0.7% and -0.6% Ir at byte-identical dumps.

### Compile-time structure

`HeadTables` puts the `tables` and `second` pair behind private fields with one mutation entry per resize shape. The lockstep comment from the encoder-edge review rides that single entry, and the desync class becomes unrepresentable.

### Compile-time constant proof

The ladder geometry in `params_for_*` and `adjust_params` is `const fn`. A `const _` walk over level 0..=22 and src_log 2^10..=2^27 asserts every relation that `resize_tables` relies on. A pair walk pins `LADDER_ROW_COLLISIONS == 132`, which is the same row count at different hash logs, the precondition of the third defect in the [encoder edges](encparity.md) review, and the Row arm cites it. A bad ladder edit now fails the build. This was verified live with a corrupted row and a shifted hash, and both trip it.

### Debug detection

Poison margins live in `encoding::poison` under `cfg(debug_assertions)`. They cover the owned streaming window, with a fill at `block_tail` staging and asserts at block commit and call exits. They also cover the ST and MT stream cores' `staged`, `output`, per-job and donation slack, the LDM split staging batches, and the pooled `BlockScratch` staging. An intra-allocation touch past a logical length panics with a checkpoint name.

### Release-codegen detection

`[profile.paranoid]` inherits release and enables `debug-assertions` and `overflow-checks`, so the walls and overflow checks run over release codegen, where wrap bugs actually manifest. The CI job `.github/workflows/paranoid.yml` runs it, and the paranoid dump is proven byte-equal to release.

### Fuzz dimensions

The `encode` and `encode_stream` targets walk a multi-payload ladder of `[level, opts, k, chunks...]`, with chunk i at level `(level+i)%23` through one thread, so pooled carryover is now a fuzz dimension. The artifact walker runs forward and in reverse in-process. `pooled_ladder_sweep` pins every level pair at the `adjust_params` clamp boundaries.

The sweep earned its keep immediately, because it found two pre-existing MT bugs that every prior gate missed.

## Bugs fixed

Both bugs reproduced identically on the pre-hardening base, and both sat in the flush and short-window corner that no prior gate drove.

`run_inline_job`'s view was anchored at the buffer instead of the strip head. The inline twin sliced `buf[..hi - buf_base]` while its `first` counts from `strip_lo`, which is every posted job's convention. After a mid-stream flush under a window shorter than the flush offset, with a forced window_log of 9 or 10, the whole post-flush span re-encoded stale earlier bytes. The frame still decodes, and only a checksum or byte compare catches it, so this was silent corruption without a checksum.

A flush-regridded Growing-grid post could cross its epoch end. `post_ready` computed `end = job_start + epoch_size`, against a gate keyed on the epoch boundary, and a mid-epoch flush parks `job_start` where the two disagree. Release would freeze a view reading bytes that were never staged. `post_job`'s debug assert caught it under the walls.

The mechanisms are recorded in [pitfalls/encoding](../pitfalls/encoding.md). The fixes are no-ops on untouched grids, and the dumps below are byte-identical.

## Gate numbers

- The tests pass with every wall armed: debug 416+12+21, release 411+12+21, no-default-features 240, zstdx-cli 99 and paranoid 416+12+21.
- The dump byte differential against the pre-round base is byte-identical for the bulk `--all-levels` and stream `--all-levels` grids. The two MT fixes are included, and their corner sits off the dump lattice, so tests pin it.
- gungraun Ir against the base reads +0.004% for fastest json, which is flat, and +0.438% for balanced json. The second figure decomposes to `HeadTables` accessor placement on the once-per-parse chain path. A semantically identical form of the rep-tail body swung by about 3% Ir either way without executing, which is register-allocation placement and not algorithmic cost. The landed shape keeps the base loop and clamps post-emit in 68c0d81c. A wall A/B closed the residue on 2026-10-24. Four order-swapped interleaved `prof enc 9` runs on json gave a base median of 155.5 against 159 MiB/s for HEAD, and every pair favored HEAD. The memory-bound chain tier never pays the cost, and no recovery was spent.
- Fuzz smokes under ASAN on the merged tree ran `encode` for 15,366 runs and `encode_stream` for 4,300 runs with zero findings.
- Poison overhead took the debug suite from 54.0 to 65.2 s, because the walls' fill and assert work runs inside the heavy MT tests. The sweep itself takes about 1 s for 966 cells with the option bits rotated one per cell. Release is unaffected, and `nm` on the release rlib shows zero wall symbols.

## Coverage gaps

The committed-length to staged-end zone of the owned window is caller-live by `frame_compressor`'s pre-split carry convention. It cannot be poisoned without a caller-contract change, although full-block commits still land the wall directly past the block end.

The block_enc bitwriter's `hot_push_raw` deliberately writes past the semantic end inside its reserved span, which is documented discipline. The MT accumulate buffer's spare is the read pump's destination. The LDM tables and buckets and the matcher's hash, row and tree tables are exact-sized with no slack. Bulk input slices are caller-borrowed, so they are ASAN-visible at the allocation end, which is the fuzzer's territory.

`parse_chain`'s `next - 512` wrap is flagged as byte-load-bearing with a do-not-fix note in 29d8399b.

Poison asserts fire at checkpoints, so a touch whose evidence is legitimately overwritten before the next checkpoint escapes. The checkpoints sit at every staging boundary to keep that window short.

## Historical bugs and their walls

| Historical bug | Wall |
|---|---|
| reserve baseline, len against pos | writer-API consolidation, open in the [todo](../todo.md) |
| HASH_READ tail lockstep and catch-up fill margin | poison, window past logical end |
| insert_max underflow, `(end-p)&1` peel, rep-tail bars | IdxRange, empty-span floor |
| dist-0 self-compare | wrapping-resolve helper, landed |
| origin sentinel phantom | coordinate-cap invariant, landed with the epoch roll |
| `second` and `tables.len()` desync | HeadTables, unrepresentable, plus the const collision count |
| head-link induction break | pooled tests, landed with the encoder-edge round, no typestate attempted |
| exit-contract divergence | skill checklist, not cheaply typeable |
| MT inline view and epoch straddle | poison sweep and pooled sweep, this round |
| merged add width past `hot_push_raw`'s 56-bit cap | per-field split push, `seq_codes` const grid, entry debug assert |
| finish's last-block patch over drained bytes | `out_read` guard on the patch arm, empty-block fallback |

## Follow-ups

The balanced-Ir placement residue is closed, as the wall A/B shows. The remaining compile-time menu in the [todo](../todo.md) holds cross-domain index newtypes, NonZero distances and enum rep-bases. It also holds the unpoisonable carry zone and the `-max_len` campaign above 4096, where the `encode_stream` part is closed here.

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
