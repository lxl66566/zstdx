# Measurement noise and criteria
## Anchor sequence-dump walks on executor truth


Reconstructing absolute positions by tiling ll plus ml from block starts and resolving repcodes by hand mis-assigns any sequence whose end coincides with a block end. A greedy `end <= be` pulls the next block's first sequence in. On repetitive data such as json, thousands of mis-resolved matches still byte-verify against the output by coincidence, and the error surfaces megabytes later or never.

Record the match position, match length and offset at the executor and cross-check them. A byte-verify gate alone is not sufficient validation of such a walk.
## Cut analysis pieces at encoder job boundaries


Use `mt_job_size_for`, which `emitframe` prints. A uniform guess drifts whenever the input is not divisible by twice the worker count. A 32 MiB json frame has jobs of 4,194,316 B against a 4 MiB guess.

Every misaligned cut manufactures depth-guarantee violations from legal mid-job shallow reads. It also produces a whole phantom tail piece when the last job is short. Streaming's unpledged growing grid is not recoverable from the frame at all, because it doubles epochs and splits the tail by worker count at finish.
## Treat machine noise as two regimes


The Zen4 32-core box has ±10% noise plus a slow drift phase. Mornings are stable at ±1%, while in the afternoon the same binary can be 25% to 50% slower.

Cross-time-of-day conclusions count only as git-stash back-to-back same-machine A/B. Every single benchmark must be re-run a second time.
## Phantom regressions from sequential A/B


The 2026-09-20 measurement of `prof enc` on json.fast sampled only about 1.4 s per invocation. One byte-identical binary read 419, 454, 455 to 459 and 468 to 474 MiB/s across about 30 minutes of sequential blocks, a ±6% spread. A 2026-10-17 base-by-three then patch-by-three protocol minutes apart produced a phantom 8% regression that nearly reverted a byte-identical-output change.

Sub-10% wall claims on fast solo cells need position-alternating interleaving of the two binaries in one session, or gungraun Ir, which is immune. Same-source forced rebuilds through `cargo clean -p` plus a build are byte-identical by hash, so the spread is machine drift. A relink lottery would require the hashes to differ.
## Re-run long-pass regressions standalone


The 2026-09-27 pass recorded text ST stream fastest and fast at 5240 and 5118 MiB/s, 26% to 28% below the earlier pass, with a normal zstd side. Fresh standalone medians were 6851 and 6571, a 5% residue, at x0.25 and x0.79. Under a concurrent load burst the same cells collapse with our side dropping about 7x harder than zstd's, because the unpledged stream core's staging gate is the sensitive side. A long interleaved pass has burst windows that a minority of cells absorb as their medians.

The 09-28 pass reproduced the collapse with the machine otherwise idle, at 5163 and 5084 in-pass against 6990 and 6702 standalone, and the 2026-09-29 root cause was same-process state with no external load. The minimal reproducer is `matrix --mode enc-stream --shape json,text --level fast,best --budget-ms 1000`, where text.fast reads about 5100 while the solo single-cell run is healthy at 6688 to 6785.

The trigger is a best or L13 row as the pooled ST core's predecessor. json.best is verified, text.best is untestable because the fixed cell order never puts it first, and a fastest or fast predecessor leaves the cell healthy.
## MT cells whose makespan is one job read a single core's placement


The 2026-11-07 pass read dll100.balanced.stream-mt8 at 128 MiB/s against R94's 174 at byte-identical output, which priced as a regression until the same-day A/B. The cell runs 8 jobs of 12.5 MiB on 8 workers posted within 0.4 ms, so its wall is the single slowest job at about 500 ms against a 60 ms fastest, with six workers idle after 470 ms: the cell measures one core's speed. On the two-CCD host the scheduler packs the pool onto the pump's CCD, and the identical binary reads about 160 MiB/s packed against about 200 MiB/s split across both L3 domains, at identical instruction counts and bytes. The same-day A/B of the pre-R94 tree against HEAD read 155 against 159 with HEAD at 1.74 percent fewer instructions, so both passes sampled the same placement band.

A makespan-shaped mt cell needs a same-day A/B plus `perf stat` instruction counts before any regression verdict, and a fixed CPU mask across passes would pin the band.
## Physical-page layout transition


The L13 to L3 `resize_tables` transition frees the 36.7 MiB DUBT table and reallocates the 32 MiB L3 hash table onto physically scattered pages. Pagemap read a contiguous-PFN-pair ratio of 0.29 against 0.51 healthy and a PFN span of 333x against 97x. dTLB misses per iteration rose 62%, and the 128 KiB staged-to-win block copies ate 38.8% of cycles against about 10% healthy. The layout stayed frozen for the whole cell because the pooled core was reused with zero per-iteration allocation syscalls, confirmed by strace and gdb breakpoints on `resize_tables` and `apply_level`.

THP=always with the table VMA verified 100% hugepage-backed recovered 11%, to 5628 MiB/s, which is the TLB slice. The residual 15% stays unattributed, because THP defrag=always, `glibc.malloc.mmap_threshold=1024` and compaction at the transition all showed no gain. The box had no movable pages to compact.

The zstd side in the same state drops 2% to 13% on a per-iteration CCtx fault and memset churn that the solo state does not have, with 73% of sampled faults under `ZSTD_resetCCtx_internal → __memset`. That is plausible shared-bandwidth interference, but it is unproven. Mitigations are to keep fast rows ahead of best within a shape, which the default cell order already does, and to use hugepage-backed allocation for the matcher's big buffers. A later audit found the decode and dictionary paths immune, because decode is dispersity-insensitive and dictionary one-shots reallocate per call. See [negative/decoding](../../negative/decoding.md) and [negative/dict](../../negative/dict.md).
## Residual collapse from input-path memmoves


The residual collapse comes from L3 and DRAM stalls inside the input-path memmoves, with zstd, the dTLB and allocator tunables ruled out. A scratch pure-zstdx driver outside the library, on the published API, replayed the cell sequence and read 7171 solo against 5521 collapsed MiB/s, a 23% drop, or 7357 against 6027 under THP=always, a 17% drop. The pure driver reproduced the full delta with zero zstd involvement, so interference is exonerated. Interleaved zstd rounds cost a state-independent 9%, reading 6560 and 7171 against 5026 and 5521, and the matrix medians that session were 6732, 5101 and 5253.

About 87% of the extra per-iteration cycles sit in libc memmove and memcpy, which went from 58% to 64% of samples and from 12.4M to 16.6M cycles per iteration. An LD_PRELOAD per-caller counter shows identical call and byte counts while rates flip from L3 to DRAM. The 128 KiB staged-to-win copies went from 1.23M to 3.62M cycles per iteration, or 0.037 to 0.108 cycles per byte and 2.95x, and the 16 KiB raw-to-staging reads went from 2.62M to 3.71M, or 1.42x. Instructions per iteration dropped in the collapsed state and IPC fell from 1.93 to 1.54, which is stall time with the per-iteration work lower.

dTLB is exonerated for the residual with a 0.28% against 0.41% to 0.52% load-miss rate under THP, while madvise read 4.06% against 7.4% and is the slice THP recovers. Pinning GLIBC_TUNABLES with `glibc.malloc.mmap_threshold=131072`, `trim_threshold=131072` and `dynamic_threshold=0`, settling the pooled core's screen verdict with a text frame first, realloc churn at 109 calls of 16 KiB or more per run, win compaction at 24738 calls per iteration solo against 28 total collapsed, and NUMA on a single node all showed no gain. Restoring the screen verdict adds 14 copies of 2 MiB per iteration at cache rate and changes nothing. Every named buffer is equivalent or better placed in the collapsed state, with the win sharing the 65.5 MiB corpus cluster VMA in both states and live dfast tables 2 MiB THP-covered in both.

What remains live as the mechanism is the physical-frame and L3-set ecology after the transition-time table reallocation. Freed DUBT and chain frames are recycled under the new tables, and the copy destinations stop staying L3-resident. That is invisible to smaps and pagemap, and AMD IBS data_src is mostly invalid on this box, so latency-level proof was not obtainable. The first follow-up lever landed: the single-threaded streaming core writes each block straight into the matcher window's tail (`feed_blocks`, `Matcher::block_tail_open`) for every row without a pre-split detector, which is the fast, dfast, btlazy and opt levels, so the staged-to-win copy is gone and the read pump's bytes reach the window in one copy instead of two. Head-gated frames hand the drained head over once and feed directly from there, and the row rows (levels 5 to 8, 11 and 12) and the chain row (level 9) keep the staging path, whose carry is what a cut block needs. Output does not move: the full-ladder bulk dump (30 frames), the full-ladder streaming dump (120 frames) and the 120-cell ratio sweep are byte-identical to 22fa6d3e, and the corrupt, mtcheck, parity and four test suites are clean. A position-alternating 6x A/B on the out-of-tree cell-sequence reproducer read the fast row at 7762 against 7085 MiB/s in the same-row layout and 5614 against 5252 after a best/L13 frame, so the row gains 7 to 10 percent and none of it comes from the transition: the same-row control gains the same, because the fast row takes the new feed on both arms. The layout penalty itself is unchanged, -25.9 percent against -27.7, and the within-process order effect widens from -8.5 to -11.0 percent, so the residual collapse is outside the input copies. The recorded -23 percent in-pass collapse does not reproduce on this tree either: the installed cell order reads text.fast at 6174 to 6300 MiB/s in-pass against 5692 to 6260 solo, and the out-of-tree reproducer's -8.0 percent same-process reading is the current one. The second lever, a persistent hugepage-aligned table slab for the pooled matcher, was built and falsified on 2026-11-18: with the transition churn gone, the parked pages contiguous and the instruction and dTLB counts flat, the post-best fast cell keeps its full penalty, and the residue tracks the pooled core's persistent window-and-parked-LDM geometry instead of any transition event (the full record is in [negative/mt-stream](../../negative/mt-stream.md)). THP and allocator tuning is exhausted. The geometry line closed the same day: the window-capacity half is the mechanism, and the landed level-down shrink takes the settled penalty to parity, while the parked-LDM half is falsified by a direct A/B ([encoding](../../perf/encoding.md) carries the lever). For tooling, split memmove cost by objdump-ing libc's shared memcpy and memmove entry, which alias at one address, plus per-caller LD_PRELOAD counting, because perf symbol names alone mislead about which loop runs.
## Hugepage-backed matcher buffers


The 2026-09-30 measurement put every matcher buffer of 8 MiB or more into a 2 MiB-aligned anonymous `mmap` advised `MADV_HUGEPAGE` through `encoding::hugepage::HugeBuf`, using `MAP_FIXED_NOREPLACE` at a bump cursor and a size that is a hugepage multiple. That covers the fused tables, `opt_table` and `bt`, the DUBT heads and ring, the LDM table and the owned win.

Below the threshold, on non-Linux, or on any mapping failure it uses a plain `Vec`, which is also what every small table takes, so output bytes depend on allocation strategy only. The `ratio --no-ref` table and the full sweep at geo-mean +8.69% match the base build exactly. smaps during the reproducer showed the 128 MiB win mapping and the 16 plus 32 MiB L13 DUBT tables carrying `AnonHugePages` equal to their full RSS.

Same-machine back-to-back, with the zstd side flat throughout, the reproducer's text.fast recovered 3.8% to 4.7%, from a 5102 to a 5293 median, which is well short of the THP=always slice's 11%. The about 15% in-pass residual stands with every 8 MiB-or-larger buffer of ours fully backed, so our tables' TLB is excluded as the residual cause. The larger win is elsewhere: the DUBT rows gained double digits from the backed heads and ring, with text.best stream solo going from 860 to 994, or +15.6%, json.best from 27 to 30, or +11%, text.fastest in-pass +9.2%, and the json fast rows flat.
## Placement disturbance in text.fast


text.fast solo reads 6% to 8.6% slower, from a 6687 to a 6191 median, across three different code layouts. Instructions are +0.09%, dTLB is flat, the dfast scan and extend loops are objdump-identical, and the wrapper is provably dormant in that cell because a solo fast cell holds no 8 MiB-or-larger buffer. This is the flat-instruction-count wall-swing class, and the same binary's in-pass text.fast is +4%. Standalone citations of text.fast from this build carry that caveat.

A position-alternating 4x interleaved base-versus-merged run put ST solo at 6639, 6783, 6780 and 6719 against merged 6223, 6234, 6192 and 6264, medians 6761 against 6229 for -7.9%, while stream-mt8 was flat at 6182 and 6177. The disturbance was real and confined to ST solo, and later re-measurement kept every pair separated by 320 to 420 MiB/s.

The layout chase stayed open until the 2026-10-01 root cause: cycles were +9.7% at instructions +0.06%, and about 90% of the delta sat inside `start_matching_dfast::<false,false,17,16,false>` at +25% cycles. Dispatch counters showed op delivery to that loop failing, with `de_no_dispatch_per_slot.no_ops_from_frontend` going from 2.71G to 7.31G at equal op-cache delivery, which is a frontend fetch problem. The wrapper's inlined prologue-only deref had shifted the 29-byte dfast extend loop, loop A, by 0x30 so that it straddled a 64-byte icache and op-cache line boundary, while the pre-wrapper build's loop sits fully inside one line.

An earlier 4x protocol on base b0ac11d5 against HEAD 36ae2d24 read base 6669, 6611, 6685 and 6561 against HEAD 6262, 6261, 6250 and 6145, medians 6648 against 6260 for -5.8%, with every pair 320 to 420 MiB/s apart and the zstd side at 5245 to 5292.
## Binary-patch proof of a line straddle


On the same HEAD binary, patched variants with 4x interleaved A/B against the untouched base loop showed that a loop relocated fully inside one 64-byte line restores base speed exactly, at 7125 against 7123 and +0.0%. A copy straddling only a 32-byte fetch-window boundary inside a line is also full speed, at 7160 against 7147 and +0.2%. A copy straddling the 64-byte line boundary is the entire tax, at 6636 against 7224 and -8.1%, with byte-identical outputs in all variants.

The mechanism is the extend loop crossing a 64-byte line on Zen4. The 32-byte fetch window plays no part. The about 8% wall swing is op-cache and frontend delivery to a loop split across two lines. No source-level alignment lever exists, because rustc's inline-asm path silently drops `.p2align` and `.balign` directives while `.byte` and `.nops` survive, so a hot loop's line phase cannot be pinned at the source level.
## Phase-proof extend loop


`match_generator::hash::extend_match` hand-writes the x86-64 extend loop twice as identical 27 to 29-byte copies 32 or 33 bytes apart, or 34 when both load bases take a disp8, which keeps the invariant. A 5-instruction branchless stub with `lea 1f(%rip); mov; and $32; add; jmp *r` selects 0 or 32 from the line-phase bit and enters whichever copy's head sits at line phase 31 or below. One always qualifies, so the executing loop never straddles a 64-byte line whatever address the linker gives the surrounding code.

A per-build objdump scan of all 632 inlined extend sites found copy deltas of 32 at 573 sites and 33 at 59 sites, where a disp8 appears when a load base is rbp or r13, with chosen phases 0 to 32, phase plus len at most 60, and zero invariant violations. The r13 or rbp delta-33 case lands the entry on a pad nop and stays correct.

The copy bodies are register-independent except that disp8, and the loop is `cmp len,lim; ja out; two 8B loads; add $8,len; cmp; je loop; xor; jmp` with a byte tail for sub-8 remainders and a `limit >= 8` gate around the asm. Semantics are identical to the portable body, which stays as the differential test's reference. The test plants mismatches at every byte offset of the first two u64s, on exact-limit runs and on every short-limit tail.

The final 4x protocol closed to parity at a 6718 median against the base 6756, which is -0.6%, and +7.6% over the disturbed state, with a repeat round dead even at 6704. The zstd side ran at 5217 to 5291.

| Run           | Values                 | Median    |
| ------------- | ---------------------- | --------- |
| base 47d2ffeb | 6828, 6767, 6739, 6745 | 6756      |
| fix           | 6692, 6544, 6723, 6718 | 6718      |
| repeat        | 6704, 6704             | dead even |

Collateral was text.fast solo +8.0%, which is the fix itself, text.fastest -0.1%, json.fast -0.4%, json.fast mt8 -0.7%, text.best solo -1.1% with the DUBT +15.6% flagship win intact at 1004 to 993, text.best mt8 +1.3%, text.fastest mt8 +2.1%, and text.fast stream-mt8 -1.7% with fully overlapping ranges from 6102-6403 to 5983-6466, which is flat within that cell's ±5% noise and the 6180 class. `ratio --no-ref` is byte-identical to the base build, and the tests, clippy, fmt and no-default-features gates were clean.

The transferable lessons are three. A 30-byte hot loop straddling a 64-byte line can cost about 8% wall at identical instruction counts, so check loop phase against 64-byte lines when a flat-Ir wall swing appears after a code-shape change. A check against 32-byte windows misses it. Rustc drops alignment directives in inline asm, so phase robustness must be built into the emitted code, and the two-copy-plus-stub pattern is the reusable form. Entering mid-instruction-stream through nop padding is safe only with 1-byte 0x90 nops, because multi-byte long-nop encodings fault at a wrong phase, and the stub's indirect jump must target an early-out register with a lateout register avoided to keep a live input intact.

The pattern's second application confirms the lesson's generality: the 2026-10-25 dll100.best minus 10 percent regression was the same straddle arriving through an unrelated caller's code growth, and porting the two-copy stub to the dubt count loop in `count_run` restored the cell at zero stub cost on long compares. That walk is in the [matchers notes](../../perf/matchers.md).
## Copy-chain deletion is closed


The probe gathered the whole stream into a reused thread-local buffer and used the bulk direct-window core, env-flipped inside one binary with position-balanced alternating matrix rounds. It held every healthy stream cell at stock speed: text.fast 6644 to 6742 against 6712 to 6903 MiB/s, text.fastest 7083 to 7114 against 7094, text.best 1022 against 1021, json.fast 432 to 446 against 448 to 464, and json.best 29 to 30 on both sides, with zstd sides flat at 5248 to 5355. The zero-copy bulk core measured 10973 MiB/s on the same shape, +63%.

One serial input copy per byte already saturates the resource the scan needs, so deleting two of three buys nothing. Every owned-window streaming mechanism is bounded at stock speed, because it needs at least one fill copy by construction plus the compaction schedule whose `win_base` timeline is byte-load-bearing through every parse's `reach = min(pos - win_base, max_window)` floor. The 60% headroom exists only behind a zero-copy borrowed window, which is bulk-only because the Read-spare-capacity and non-contiguous-history vetoes stand.

The collapsed state recurred 5 of 5 reproducer rounds in one four-minute window, with in-pass text.fast at 4960 to 5030 and the zstd side down 5% with it, so the event was machine-wide, and then 0 of 36 across the interleaved series. No collapsed-phase ceiling sample exists, so the falsification rests on the healthy-state bound. The full row is in [negative](../../negative/mt-stream.md).
## Cells with known bimodality


random reads 1388 or 2027 MiB/s depending on frequency drift at 73% scaling and SMT sibling scheduling. skewed.Fastest readings are heavily affected by the sibling core and settle back after one re-run round. The `bench_matrix` x factor drifts ±10% across runs even for unchanged cells, and the canonical criteria list is in [methodology](../../bench/methodology.md).

- Position bias favors whichever binary runs later. Within one test, the later binary consistently gets a 4% to 5% advantage from cache and frequency phase, so A/B must be position-aligned, alternating first and second and comparing by position.
## Allocator cross-contamination


When serially running multiple implementations or sizes in one process, zstd-side CCtx allocation churn shreds the malloc arena, and subsequent timed segments read 3x off, with random-64K at about 3000 against 9200 in a single-shape process. Small-load criteria must pin implementation and size to a single-shape, single-size process.

glibc's dynamic mmap and trim thresholds also drift with the allocation sequence, and pinning `MALLOC_TRIM_THRESHOLD_` disables zstd's adaptivity so its random-64K collapses 3x, so pin it or run isolated. The first run after a build disturbance can read 30% low, so take final numbers from multiple stable rounds. In solo benches, in-process cell order is a source of allocation-layout artifacts, because a table's size change moves where later allocations land, so decide A/B with fresh processes or cross-validate across two environments. BMI2 dispatch overhead is Zen4-only data, measured neutral at zero cost, and needs re-measurement on Intel before generalizing.
