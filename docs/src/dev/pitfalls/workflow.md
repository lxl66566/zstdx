# Workflow pitfalls

This page collects the measurement traps, build hazards and tooling lessons that have cost real time on this project. Each lesson keeps its counterexample, and the sections group the lessons by topic: measurement noise, benchmark tooling, build and cargo, profiling, toolchain, corpus and editing.

## Measurement noise and criteria

### Anchor sequence-dump walks on executor truth

Reconstructing absolute positions by tiling ll plus ml from block starts and resolving repcodes by hand mis-assigns any sequence whose end coincides with a block end. A greedy `end <= be` pulls the next block's first sequence in. On repetitive data such as json, thousands of mis-resolved matches still byte-verify against the output by coincidence, and the error surfaces megabytes later or never.

Record the match position, match length and offset at the executor and cross-check them. A byte-verify gate alone is not sufficient validation of such a walk.

### Cut analysis pieces at encoder job boundaries

Use `mt_job_size_for`, which `emitframe` prints. A uniform guess drifts whenever the input is not divisible by twice the worker count. A 32 MiB json frame has jobs of 4,194,316 B against a 4 MiB guess.

Every misaligned cut manufactures depth-guarantee violations from legal mid-job shallow reads. It also produces a whole phantom tail piece when the last job is short. Streaming's unpledged growing grid is not recoverable from the frame at all, because it doubles epochs and splits the tail by worker count at finish.

### Treat machine noise as two regimes

The Zen4 32-core box has ±10% noise plus a slow drift phase. Mornings are stable at ±1%, while in the afternoon the same binary can be 25% to 50% slower.

Cross-time-of-day conclusions count only as git-stash back-to-back same-machine A/B. Every single benchmark must be re-run a second time.

### Phantom regressions from sequential A/B

The 2026-09-20 measurement of `prof enc` on json.fast sampled only about 1.4 s per invocation. One byte-identical binary read 419, 454, 455 to 459 and 468 to 474 MiB/s across about 30 minutes of sequential blocks, a ±6% spread. A 2026-10-17 base-by-three then patch-by-three protocol minutes apart produced a phantom 8% regression that nearly reverted a byte-identical-output change.

Sub-10% wall claims on fast solo cells need position-alternating interleaving of the two binaries in one session, or gungraun Ir, which is immune. Same-source forced rebuilds through `cargo clean -p` plus a build are byte-identical by hash, so the spread is machine drift. A relink lottery would require the hashes to differ.

### Re-run long-pass regressions standalone

The 2026-09-27 pass recorded text ST stream fastest and fast at 5240 and 5118 MiB/s, 26% to 28% below the earlier pass, with a normal zstd side. Fresh standalone medians were 6851 and 6571, a 5% residue, at x0.25 and x0.79. Under a concurrent load burst the same cells collapse with our side dropping about 7x harder than zstd's, because the unpledged stream core's staging gate is the sensitive side. A long interleaved pass has burst windows that a minority of cells absorb as their medians.

The 09-28 pass reproduced the collapse with the machine otherwise idle, at 5163 and 5084 in-pass against 6990 and 6702 standalone, and the 2026-09-29 root cause was same-process state with no external load. The minimal reproducer is `matrix --mode enc-stream --shape json,text --level fast,best --budget-ms 1000`, where text.fast reads about 5100 while the solo single-cell run is healthy at 6688 to 6785.

The trigger is a best or L13 row as the pooled ST core's predecessor. json.best is verified, text.best is untestable because the fixed cell order never puts it first, and a fastest or fast predecessor leaves the cell healthy.

### Physical-page layout transition

The L13 to L3 `resize_tables` transition frees the 36.7 MiB DUBT table and reallocates the 32 MiB L3 hash table onto physically scattered pages. Pagemap read a contiguous-PFN-pair ratio of 0.29 against 0.51 healthy and a PFN span of 333x against 97x. dTLB misses per iteration rose 62%, and the 128 KiB staged-to-win block copies ate 38.8% of cycles against about 10% healthy. The layout stayed frozen for the whole cell because the pooled core was reused with zero per-iteration allocation syscalls, confirmed by strace and gdb breakpoints on `resize_tables` and `apply_level`.

THP=always with the table VMA verified 100% hugepage-backed recovered 11%, to 5628 MiB/s, which is the TLB slice. The residual 15% stays unattributed, because THP defrag=always, `glibc.malloc.mmap_threshold=1024` and compaction at the transition all showed no gain. The box had no movable pages to compact.

The zstd side in the same state drops 2% to 13% on a per-iteration CCtx fault and memset churn that the solo state does not have, with 73% of sampled faults under `ZSTD_resetCCtx_internal → __memset`. That is plausible shared-bandwidth interference, but it is unproven. Mitigations are to keep fast rows ahead of best within a shape, which the default cell order already does, and to use hugepage-backed allocation for the matcher's big buffers. A later audit found the decode and dictionary paths immune, because decode is dispersity-insensitive and dictionary one-shots reallocate per call. See [negative/decoding](../negative/decoding.md) and [negative/dict](../negative/dict.md).

### Residual collapse from input-path memmoves

The residual collapse comes from L3 and DRAM stalls inside the input-path memmoves, with zstd, the dTLB and allocator tunables ruled out. A scratch pure-zstdx driver outside the library, on the published API, replayed the cell sequence and read 7171 solo against 5521 collapsed MiB/s, a 23% drop, or 7357 against 6027 under THP=always, a 17% drop. The pure driver reproduced the full delta with zero zstd involvement, so interference is exonerated. Interleaved zstd rounds cost a state-independent 9%, reading 6560 and 7171 against 5026 and 5521, and the matrix medians that session were 6732, 5101 and 5253.

About 87% of the extra per-iteration cycles sit in libc memmove and memcpy, which went from 58% to 64% of samples and from 12.4M to 16.6M cycles per iteration. An LD_PRELOAD per-caller counter shows identical call and byte counts while rates flip from L3 to DRAM. The 128 KiB staged-to-win copies went from 1.23M to 3.62M cycles per iteration, or 0.037 to 0.108 cycles per byte and 2.95x, and the 16 KiB raw-to-staging reads went from 2.62M to 3.71M, or 1.42x. Instructions per iteration dropped in the collapsed state and IPC fell from 1.93 to 1.54, which is stall time with the per-iteration work lower.

dTLB is exonerated for the residual with a 0.28% against 0.41% to 0.52% load-miss rate under THP, while madvise read 4.06% against 7.4% and is the slice THP recovers. Pinning GLIBC_TUNABLES with `glibc.malloc.mmap_threshold=131072`, `trim_threshold=131072` and `dynamic_threshold=0`, settling the pooled core's screen verdict with a text frame first, realloc churn at 109 calls of 16 KiB or more per run, win compaction at 24738 calls per iteration solo against 28 total collapsed, and NUMA on a single node all showed no gain. Restoring the screen verdict adds 14 copies of 2 MiB per iteration at cache rate and changes nothing. Every named buffer is equivalent or better placed in the collapsed state, with the win sharing the 65.5 MiB corpus cluster VMA in both states and live dfast tables 2 MiB THP-covered in both.

What remains live as the mechanism is the physical-frame and L3-set ecology after the transition-time table reallocation. Freed DUBT and chain frames are recycled under the new tables, and the copy destinations stop staying L3-resident. That is invisible to smaps and pagemap, and AMD IBS data_src is mostly invalid on this box, so latency-level proof was not obtainable. The follow-up levers in order are to remove the copies themselves and then to use a persistent hugepage-aligned table slab for the pooled matcher. THP and allocator tuning is exhausted. For tooling, split memmove cost by objdump-ing libc's shared memcpy and memmove entry, which alias at one address, plus per-caller LD_PRELOAD counting, because perf symbol names alone mislead about which loop runs.

### Hugepage-backed matcher buffers

The 2026-09-30 measurement put every matcher buffer of 8 MiB or more into a 2 MiB-aligned anonymous `mmap` advised `MADV_HUGEPAGE` through `encoding::hugepage::HugeBuf`, using `MAP_FIXED_NOREPLACE` at a bump cursor and a size that is a hugepage multiple. That covers the fused tables, `opt_table` and `bt`, the DUBT heads and ring, the LDM table and the owned win.

Below the threshold, on non-Linux, or on any mapping failure it uses a plain `Vec`, which is also what every small table takes, so output bytes depend on allocation strategy only. The `ratio --no-ref` table and the full sweep at geo-mean +8.69% match the base build exactly. smaps during the reproducer showed the 128 MiB win mapping and the 16 plus 32 MiB L13 DUBT tables carrying `AnonHugePages` equal to their full RSS.

Same-machine back-to-back, with the zstd side flat throughout, the reproducer's text.fast recovered 3.8% to 4.7%, from a 5102 to a 5293 median, which is well short of the THP=always slice's 11%. The about 15% in-pass residual stands with every 8 MiB-or-larger buffer of ours fully backed, so our tables' TLB is excluded as the residual cause. The larger win is elsewhere: the DUBT rows gained double digits from the backed heads and ring, with text.best stream solo going from 860 to 994, or +15.6%, json.best from 27 to 30, or +11%, text.fastest in-pass +9.2%, and the json fast rows flat.

### Placement disturbance in text.fast

text.fast solo reads 6% to 8.6% slower, from a 6687 to a 6191 median, across three different code layouts. Instructions are +0.09%, dTLB is flat, the dfast scan and extend loops are objdump-identical, and the wrapper is provably dormant in that cell because a solo fast cell holds no 8 MiB-or-larger buffer. This is the flat-instruction-count wall-swing class, and the same binary's in-pass text.fast is +4%. Standalone citations of text.fast from this build carry that caveat.

A position-alternating 4x interleaved base-versus-merged run put ST solo at 6639, 6783, 6780 and 6719 against merged 6223, 6234, 6192 and 6264, medians 6761 against 6229 for -7.9%, while stream-mt8 was flat at 6182 and 6177. The disturbance was real and confined to ST solo, and later re-measurement kept every pair separated by 320 to 420 MiB/s.

The layout chase stayed open until the 2026-10-01 root cause: cycles were +9.7% at instructions +0.06%, and about 90% of the delta sat inside `start_matching_dfast::<false,false,17,16,false>` at +25% cycles. Dispatch counters showed op delivery to that loop failing, with `de_no_dispatch_per_slot.no_ops_from_frontend` going from 2.71G to 7.31G at equal op-cache delivery, which is a frontend fetch problem. The wrapper's inlined prologue-only deref had shifted the 29-byte dfast extend loop, loop A, by 0x30 so that it straddled a 64-byte icache and op-cache line boundary, while the pre-wrapper build's loop sits fully inside one line.

An earlier 4x protocol on base b0ac11d5 against HEAD 36ae2d24 read base 6669, 6611, 6685 and 6561 against HEAD 6262, 6261, 6250 and 6145, medians 6648 against 6260 for -5.8%, with every pair 320 to 420 MiB/s apart and the zstd side at 5245 to 5292.

### Binary-patch proof of a line straddle

On the same HEAD binary, patched variants with 4x interleaved A/B against the untouched base loop showed that a loop relocated fully inside one 64-byte line restores base speed exactly, at 7125 against 7123 and +0.0%. A copy straddling only a 32-byte fetch-window boundary inside a line is also full speed, at 7160 against 7147 and +0.2%. A copy straddling the 64-byte line boundary is the entire tax, at 6636 against 7224 and -8.1%, with byte-identical outputs in all variants.

The mechanism is the extend loop crossing a 64-byte line on Zen4. The 32-byte fetch window plays no part. The about 8% wall swing is op-cache and frontend delivery to a loop split across two lines. No source-level alignment lever exists, because rustc's inline-asm path silently drops `.p2align` and `.balign` directives while `.byte` and `.nops` survive, so a hot loop's line phase cannot be pinned at the source level.

### Phase-proof extend loop

`match_generator::hash::extend_match` hand-writes the x86-64 extend loop twice as identical 27 to 29-byte copies 32 or 33 bytes apart, or 34 when both load bases take a disp8, which keeps the invariant. A 5-instruction branchless stub with `lea 1f(%rip); mov; and $32; add; jmp *r` selects 0 or 32 from the line-phase bit and enters whichever copy's head sits at line phase 31 or below. One always qualifies, so the executing loop never straddles a 64-byte line whatever address the linker gives the surrounding code.

A per-build objdump scan of all 632 inlined extend sites found copy deltas of 32 at 573 sites and 33 at 59 sites, where a disp8 appears when a load base is rbp or r13, with chosen phases 0 to 32, phase plus len at most 60, and zero invariant violations. The r13 or rbp delta-33 case lands the entry on a pad nop and stays correct.

The copy bodies are register-independent except that disp8, and the loop is `cmp len,lim; ja out; two 8B loads; add $8,len; cmp; je loop; xor; jmp` with a byte tail for sub-8 remainders and a `limit >= 8` gate around the asm. Semantics are identical to the portable body, which stays as the differential test's reference. The test plants mismatches at every byte offset of the first two u64s, on exact-limit runs and on every short-limit tail.

The final 4x protocol closed to parity at a 6718 median against the base 6756, which is -0.6%, and +7.6% over the disturbed state, with a repeat round dead even at 6704. The zstd side ran at 5217 to 5291.

| Run | Values | Median |
|---|---|---|
| base 47d2ffeb | 6828, 6767, 6739, 6745 | 6756 |
| fix | 6692, 6544, 6723, 6718 | 6718 |
| repeat | 6704, 6704 | dead even |

Collateral was text.fast solo +8.0%, which is the fix itself, text.fastest -0.1%, json.fast -0.4%, json.fast mt8 -0.7%, text.best solo -1.1% with the DUBT +15.6% flagship win intact at 1004 to 993, text.best mt8 +1.3%, text.fastest mt8 +2.1%, and text.fast stream-mt8 -1.7% with fully overlapping ranges from 6102-6403 to 5983-6466, which is flat within that cell's ±5% noise and the 6180 class. `ratio --no-ref` is byte-identical to the base build, and the tests, clippy, fmt and no-default-features gates were clean.

The transferable lessons are three. A 30-byte hot loop straddling a 64-byte line can cost about 8% wall at identical instruction counts, so check loop phase against 64-byte lines when a flat-Ir wall swing appears after a code-shape change. A check against 32-byte windows misses it. Rustc drops alignment directives in inline asm, so phase robustness must be built into the emitted code, and the two-copy-plus-stub pattern is the reusable form. Entering mid-instruction-stream through nop padding is safe only with 1-byte 0x90 nops, because multi-byte long-nop encodings fault at a wrong phase, and the stub's indirect jump must target an early-out register with a lateout register avoided to keep a live input intact.

The pattern's second application confirms the lesson's generality: the 2026-10-25 dll100.best minus 10 percent regression was the same straddle arriving through an unrelated caller's code growth, and porting the two-copy stub to the dubt count loop in `count_run` restored the cell at zero stub cost on long compares. That walk is in the [matchers notes](../perf/matchers.md).

### Copy-chain deletion is closed

The probe gathered the whole stream into a reused thread-local buffer and used the bulk direct-window core, env-flipped inside one binary with position-balanced alternating matrix rounds. It held every healthy stream cell at stock speed: text.fast 6644 to 6742 against 6712 to 6903 MiB/s, text.fastest 7083 to 7114 against 7094, text.best 1022 against 1021, json.fast 432 to 446 against 448 to 464, and json.best 29 to 30 on both sides, with zstd sides flat at 5248 to 5355. The zero-copy bulk core measured 10973 MiB/s on the same shape, +63%.

One serial input copy per byte already saturates the resource the scan needs, so deleting two of three buys nothing. Every owned-window streaming mechanism is bounded at stock speed, because it needs at least one fill copy by construction plus the compaction schedule whose `win_base` timeline is byte-load-bearing through every parse's `reach = min(pos - win_base, max_window)` floor. The 60% headroom exists only behind a zero-copy borrowed window, which is bulk-only because the Read-spare-capacity and non-contiguous-history vetoes stand.

The collapsed state recurred 5 of 5 reproducer rounds in one four-minute window, with in-pass text.fast at 4960 to 5030 and the zstd side down 5% with it, so the event was machine-wide, and then 0 of 36 across the interleaved series. No collapsed-phase ceiling sample exists, so the falsification rests on the healthy-state bound. The full row is in [negative](../negative/mt-stream.md).

### Cells with known bimodality

random reads 1388 or 2027 MiB/s depending on frequency drift at 73% scaling and SMT sibling scheduling. skewed.Fastest readings are heavily affected by the sibling core and settle back after one re-run round. The `bench_matrix` x factor drifts ±10% across runs even for unchanged cells, and the canonical criteria list is in [methodology](../bench/methodology.md).

- Position bias favors whichever binary runs later. Within one test, the later binary consistently gets a 4% to 5% advantage from cache and frequency phase, so A/B must be position-aligned, alternating first and second and comparing by position.

### Allocator cross-contamination

When serially running multiple implementations or sizes in one process, zstd-side CCtx allocation churn shreds the malloc arena, and subsequent timed segments read 3x off, with random-64K at about 3000 against 9200 in a single-shape process. Small-load criteria must pin implementation and size to a single-shape, single-size process.

glibc's dynamic mmap and trim thresholds also drift with the allocation sequence, and pinning `MALLOC_TRIM_THRESHOLD_` disables zstd's adaptivity so its random-64K collapses 3x, so pin it or run isolated. The first run after a build disturbance can read 30% low, so take final numbers from multiple stable rounds. In solo benches, in-process cell order is a source of allocation-layout artifacts, because a table's size change moves where later allocations land, so decide A/B with fresh processes or cross-validate across two environments. BMI2 dispatch overhead is Zen4-only data, measured neutral at zero cost, and needs re-measurement on Intel before generalizing.

## Benchmark tooling

### Instrumentation must be opt-in

The `seq_dump` decoder hook, a TLS `Vec::push` per sequence inside `decode_step`, was enabled unconditionally in zstdx-bench's zstdx dependency for the `seqstats` tool. Every measured decode and every ratio-sweep roundtrip gate silently ran with it, and json stream fell from 1733 to 1224 MiB/s, 29% slower and x1.26 to x1.79 against libzstd, while the zstd side was unchanged, so the A/B harness reported a phantom regression.

The interleaved A/B protects against clock drift, but it cannot protect against a one-sided build difference. Instrumenting features in the measured crate's hot loop are bench-crate opt-in features behind `--features seq_dump`, and after landing any instrumentation hook, re-run one timing cell at HEAD as a canary before trusting later numbers.

### Bench harness self-wounds

The MiB/s formula once forgot to multiply by rounds, so `raw.len()/elapsed` was used where `raw.len()*n/elapsed` belonged. A filter-argument substring mismatch silently runs an empty process, with utime near 0 as the signal.

The x factor's `%.3f` format is unreadable at extreme values, so look at the MiB/s column. zeros is a free noise-calibration cell. seqstats hardcodes `Level::Fastest`, which makes it useless for non-Fastest work, and a `--level` flag is the real fix.

### Parameter sweeps must print back

A regex replacement dropped one comma so a subpattern never matched, and one "unchanged" run yielded a "changed" conclusion masked by another configuration's data. A sed margin sweep `s/+A/+B/` silently no-ops when the pattern is absent, so an entire sweep step, and any test rerunning the same value, measures the unchanged file.

Verify with grep or Python asserts, as set_lvl.py does with its echoes and asserts. Two bogus identical-output conclusions were drawn before detection.

### Pin both sides of a ref-frame A/B

The seqstats "ours" parse always comes from the current build, so two runs in the 2026-09-18 A/B whose reference side was rebuilt from the same tree once proved a bit-identical parse against what was believed to be the base. Freeze one side as a frame file with `--ref-frame`, plus `--dict` and `--level`, and diff parses against that file while rebuilding only the moving side.

After a base build through `git checkout --`, re-verify that the patch is applied before the next new measurement, since a stale binary silently reverts the experiment.

### Long locked benches run detached

A follow-up command in the same shell bucket blocks until the locked bench finishes, and the queued command's client has long timed out, so its output is lost while the command still executes. A queued `rm` once deleted a finished matrix run's only output copy.

Launch long locked benches detached with `nohup flock ... &`, poll from a separate session, and never queue cleanup commands into a bucket that might be blocked.

### Tier-only gates hide off-tier rows

`ratio` and `dump --all-levels` walk levels 1, 3, 9, 13, 17 and 19, so every off-tier row at 16, 18, 20 to 22 and the numeric-only rows generally is invisible to both gates. Row-16 and row-22 knob A/Bs initially looked byte-identical because the grids never compress those levels.

Off-tier experiments must gate on `matrix --mode enc-st --full-ladder --level <n>` for sizes and ratio per cell, or on `emitframe` per level. A knob change that reports no cell moved under the standard gates has not been measured.

### Fuzz oracles and reference APIs

The decode target's inherited `streamed == flat` assertion was false by construction, because the paths mirror different libzstd APIs, streaming generation model against one-shot whole history, and they diverge by design on wrapped-ring aliasing frames. Each path must be pinned against its own reference counterpart, never against the sibling.

Even the per-path byte compare is ill-defined on window-invalid aliased frames, because libzstd's accepted bytes then depend on the caller's buffer geometry. The zstd crate's `copy_decode` resolved a stale-ring tail `28 cb d5 fa` where the CLI and zstdx streaming both produce `00 00 00 00`, byte-identical over the whole 360 KB output, so a cross-implementation byte assert can fire on a defect-free decoder.

This happened only inside a long-lived worker process, because the reference's aliased read returns its own buffer's stale heap bytes. Fresh-process single runs by 20 and a full one-process corpus replay of the same artifact were all clean.

The adjudicator that keeps the oracle sound runs the streamed byte compare only when the reference's own streaming and one-shot outputs agree, which every valid frame does because validity is exactly layout-independence. Accept and reject agreement stays unconditional. The campaign report is `/root/scratch-r73-fuzz`.

### MT fuzzing and router floors

The parallel decoder engages only at 512 KiB or more of compressed input, or a pledged single frame in the 2 to 64 MiB piece regime. libFuzzer's default 4 KiB `max_len` therefore only fuzzes the sequential fallback behind the MT entry points.

Run with `-max_len` around 2 MiB and seed both regimes. Ramp frames from the CLI through `ZSTDX_MT_RAMP_BYTES=2M zstdx -T4`, whose MT headers carry the pledge, exercise the piece executor. Plain `-T4` frames above the floor exercise segment-parallel decode, and a sample of the decode corpus fills in. The piece-router override is safe to flip inside a fuzz target because it runs in a dedicated process, the same reason the piece tests live in their own binary.

The strongest property is directional parity: MT must agree with sequential byte for byte and never accept what sequential rejects. Large inputs cut execution to about 30 per second per worker, so budget by coverage, with exec count as a secondary signal.

### Cross-surface fuzz empty cases

The first crash in the `dict_pooled` target triaged to a target bug. The engine behaved correctly. A derived empty raw dictionary parses fine through `EncoderOptions::dictionary` and gives a windowed dictionary frame, while compat `set_dictionary(_, &[])` means detach and gives a no-dictionary single-segment frame, so the cross-surface byte-identity assertion was vacuously wrong at the boundary.

Pin every surface's documented behavior on the degenerate inputs the fuzzer can derive: empty, 1-byte and all-same. The same campaign's engine find was that a reused pooled state only reproduces in-process with the exact call order and a warm pool, because fresh-process reruns of the artifact came back clean and libFuzzer carries state across execs from the empty init input. Keep the reproducer as a scripted call sequence as well as the artifact.

### The pooled-state fuzz targets

The `dict_pooled` target checks pooled dictionary encode identity across bulk raw and parsed attachment, compat re-set, and `FrameCompressor` displacement across interleaved frames. It asserts no panic, a dictionary roundtrip, second-call bytes equal to first-call bytes, and the cached path decoding as the fresh parse.

The `ldm_mt_parity` target checks LdmMode by workers decode parity over {Auto, Enable, Disable} by {0, 1, 4} by pledged and open by bulk and stream, with three decoders each: sequential, parallel and reference, as a window-declaration proxy. A campaign of 4 jobs of 241 s per target under the bench flock ran 41.5K execs for `dict_pooled`, with its corpus growing from 141 to 3677 units at about 48 exec/s/worker and 388 MiB RSS, and a clean `ldm_mt_parity` at 3290 units, with zero crashes after the empty-dictionary target fix.

The campaign's one engine bug was that `FrameCompressor` with a dictionary and no input shape folded the unknown length into a dictionary-sized span. That clamped the window to the dictionary while the source streamed arbitrary input past it. The blocks then exceeded `min(window, 128 KiB)`, both decoders rejected the frame, and the fuzzer minimized it to a 30-byte reproducer. It was fixed by the stream core's rule that an unpledged stream keeps the unknown length, gated by `unpledged_frame_compressor_dict_keeps_full_window`.

### Port fidelity against the reference binary

The `-i0` scheduler shape looked tested, because a zero budget still measures one round, yet `zstd -b1 -i0` against our binary showed 10 ms against 2.2 s. That exposed the missed `run_ms > total_ms` clamp in `BMK_resetTimedFnState`. Timing the two CLIs side by side on an edge flag is a cheap probe that no unit shape replaces.

Conversely, `--patch-from` under `-b` is silently ignored by the reference itself, because zstdcli.c aliases `dictFileName = patchFromDictFileName` after the bench dispatch and the `-D` conflict and multi-file checks sit after it too. A port that ignores it too is at parity, so verify the quirk against the binary before fixing it into a rejection or a dictionary bench.

### Fuzz campaign mechanics

With nightly as the default toolchain, `cargo fuzz run --fuzz-dir crates/zstdx-fuzz -s address -O <target>` works directly. libFuzzer's `-jobs` and `-workers` mode writes `fuzz-N.log` into the current directory, so clean them after the run or launch from a scratch directory. `-max_total_time` is per job, so a 4 by 4 wave takes wall time close to one job's budget.

The campaign must hold the bench flock, four workers is plenty, and per-worker RSS is about 430 MiB with ASAN on decode targets, so set `-rss_limit_mb` below the free-RAM floor. Seed corpora from `crates/zstdx/decodecorpus_files` using every third `.zst`, from `dict_tests` using slices of the formatted dictionary at many offsets and widths for headers, truncated tails and table regions, and from the raw corpus for interop, where the target compresses seeds with the reference crate itself.

The current smokes run through `crates/zstdx-fuzz/smoke.sh`. It drives the built binary directly because `cargo fuzz run` silently appends `corpus/<target>` to the arguments, which collapses chunked parallel `-merge=1` into N full merges. It splits the seed replay over W single-shot `-runs=0` processes, because letting every `-jobs` child replay the whole corpus under contention drops per-worker execution 3x at 16 workers and `-max_total_time` never sees the replay at all. It then fuzzes W-wide from a rotating 1/W slice, holds the bench flock, and archives crashes to `artifacts/`.

Corpora stay merged after campaigns, with encode going from 11653 to 4807 and encode_stream from 2518 to 1667. The recipe in the fuzz Readme needs hardlink splits on the corpus device, because a tmpfs `/tmp` cannot hardlink to it.

### fsmonitor swallows tool writes

After a Python script or cargo fmt modifies files, the fsmonitor daemon does not report it, so git believes the files are unchanged while the blobs differ. Status shows clean, and a pathspec commit silently commits old content. This once lost all source changes in a fix commit, exposed only when `git show --stat`'s file count did not add up.

In this repository run all git commands as `git -c core.fsmonitor=false`, and always check the stat file count after committing. A knock-on effect is that a pathspec commit resets the index to HEAD plus the committed paths, so a previously staged `git mv` rename falls out of the index entirely. Files ignored by .gitignore but historically force-tracked, such as fuzz artifacts, must be re-staged with `git add -f` or the rename is committed as a pure deletion.

## Build and cargo

- A/B always runs back-to-back on the same machine through `git stash`. After a stash round-trip, `target/` is not rebuilt automatically, and one session spent half a day objdump-ing the stale BASE binary while analyzing why BMI2 was not generated.

### Shared stash stack

All worktrees of this repository push onto one `refs/stash`, so a `git stash pop` after any delay can restore and drop another session's work in progress. This was observed live, where a pop restored a foreign match_generator and opt.rs diff and dropped its entry.

Before popping, verify the top entry's content with `git stash show --stat`. On a wrong pop the dropped commit usually still exists, and `git stash store <sha>` re-registers it exactly. For worktree-local parking, prefer scratch commits on the branch with `git add -A && git commit -m wip` and reset later.

### Per-package cargo binaries

`cargo build -p zstdx` does not refresh `target/release/zstdx-bench`, so rebuild the bench binary after every edit. A ratio run once showed byte-identical output because it was a stale binary.

### No-default caching illusion

Touch sources to confirm recompilation before counting warnings. `cargo build --no-default-features` also recompiles examples as no-std and overwrites the default build under `target/release`, with AVX-512 kernels cfg'd out and xxhash gone, which invalidates every subsequent measurement.

Before measuring, rebuild the default build and verify with `nm ... | grep <kernel symbol>`. The no-default check must carry `-p zstdx`, because a root build lets the CLI bring the hash feature back. The examples have since moved out of zstdx, so that last risk is gone.

### Cargo test's piped exit code

With `cargo test | tail`, the cargo failure is swallowed by the `&&` chain while the success marker still prints. The test gate must be `cargo test > log; echo $?`, persisting to disk and checking both the exit code and the FAILED count.

`2>&1 | tail -N` also truncates the output file to its last N lines and wastes a re-run round.

### The debug test suite is mandatory

A green release suite does not mean a bug-free tree. If the suite seems hung, first suspect that a panic poisoned the thread pool, because an MT worker panic makes the join wait forever, which once meant 17 minutes of zero output. Localize with `--test-threads 1` and by running the test binary directly.

- Running the test binary directly requires the crate root, because 13 corpus tests use relative paths.
- The bench corpus is not in git, being an untracked generated artifact, so copy it before benching in a worktree.
- Delete debug panic and eprintln instrumentation before committing. One was once left behind, and an eprintln under `#[cfg(feature="std")]` compiles into release with the default feature.
- On this repository's git version, `git stash push --staged` only copies without restoring the scene. For splitting atomic commits, use `git restore --staged` plus `git commit <path>`.
- `cargo fmt` touches historically unformatted files. Before committing, run `git checkout --` on the unrelated files, because it nearly dragged six of them into an atomic commit.

### Un-CI'd configuration drifts

`--no-default-features` drifted uncompilable across two full optimization rounds, through a std-gated re-export and std-only float methods in newly no_std-path code, while every agent verified default features only. A one-line `cargo check --no-default-features` in the pre-commit ritual catches this class for free, and the same applies to `--features std`-minus combinations on crates that promise them.

`--all-features` is the same trap on the lint side, where `#[cfg(any(test, feature = "fuzz_exports"))]` huff0 helpers sat as `dead_code`-invisible under default features for several rounds and the bench's `seq_dump`-only helpers were only unused-but-compiled without the feature. The lint gate must be `cargo clippy --workspace --all-targets --all-features`. The trap fires in reverse too, where a `#[cfg(feature = "dict_builder")]`-only import in compat/dict.rs warned on every default-feature build but was invisible under the all-features gate for a full round, so a default-features clippy pass belongs in the ritual alongside the all-features one.

The no-default-features pass also carried 11 pre-existing warnings, 4 mechanical in `io_nostd.rs`, 5 `unused_self` on the checksum-free implementation's symmetry API and 2 feature-gated dead-code functions in opt.rs and match_generator, and none blocked the gate's purpose of catching new drift. Every profile now builds and lints warning-free, and the gate is CI-enforced through `.github/workflows/lint.yml` with `cargo fmt --all -- --check`, `cargo clippy --workspace --all-targets` and `cargo clippy -p zstdx --no-default-features --all-targets`. The manual ritual had drifted again when poison-test files landed with pedantic lints, which motivated wiring it in.

Clippy itself drifted a third way on 2026-10-24: a newer toolchain fired `manual_div_ceil` and `manual_ilog2` on frame-crafting helpers older than those lints, and `opt.rs`'s `prefill_hash3` turned dead-code under no-default-features behind its `std`-gated caller. Both CI commands run clean again after the sweep. A clippy bump can reopen old code, so re-read the gate output after toolchain updates.

## Profiling and attribution

### Record with ASLR off

Use `setarch -R perf record` plus `perf script` to extract an IP histogram, then objdump the corresponding address ranges and read them manually. `perf annotate --stdio` output mixes in kernel disassembly and is untrustworthy.

### Keep annotate windows small

A +0x1000 window once made `FSETable::clone` look like instructions inside `emit_match`. Get exact addresses with `nm` first, then use a small window.

### The instruction-count criterion

Late in optimization, ±5% to ±25% wall-clock swings from single-line changes are usually layout or scheduling noise, and only a drop in `perf stat` instruction count is a real gain. Flat instructions with a wall swing count as ineffective disturbance. Use instruction counts for micro changes and interleaved wall-clock medians for end-to-end changes.

- AMD branch-miss event sampling has a large skid. Attribute through stub bisection plus `perf stat` totals, with the details on the decoding pitfall page.
- LD_PRELOAD counting of memcpy and memmove is the sharp tool for small-copy disasters, which is how json 32 MiB showed about 3M PLT calls.

### Register-pressure law

At 15 or more live values LLVM keeps the bit window on the stack, while 12 values can stay in registers, and a `shlx` with a memory operand in annotate means you are hit.

Removing a single memory operand is nearly useless at +0.2%, because the out-of-order engine already hides memory latency, and shortening dependency chains gives only +1%. Cutting serial dependencies through grouped batch extraction pays the most.

### Per-job spans and caller phases

jobdecomp's spans are job-side, and the stream spf build traced only its final snapshot clone while the fill loop ran through `chain_grid_fill` spanless. The 2026-09-23 decomposition booked "grid_fill 66% of summed job time" against the below-median jobs' fills, while the wall's actual whale was the caller's built-then-dropped prefix build at 18 to 33 ms on text, invisible to every span.

Before booking a per-job share as the cell's lever, time the caller phases of one finish or encode call: build, probe settle, post and drain. A summed-job-time share is a wall attribution only when no serial caller phase dominates and the phase's own output is kept.

## Toolchain and environment

### Python frame parsing

In `(b1<<8)|b2+0x7f00`, `+` binds tighter than `|`. The literals-header size-field bit width is easy to misremember. `sort -rn` mixed in kernel symbols. Check an analysis tool's field definitions against a known-correct decoder before writing it.

- Background task output should not go through tail or head in pipes. Persist the full output to disk, then grep.
- Gungraun comparisons need explicit baselines. Pass `--save-baseline=X` and `--baseline=X`, because the default "No change" compares the run against itself. Baseline names reject `-`, so use `_`.

### Lane counts in gated tests

The 2026-10-24 first run of the paranoid profile caught `parallel_fill_matches_serial` invoking `fill_parallel`, whose entry `debug_assert!(lanes > 1)` gated on `parallel_fill_lanes()`, which is 3 only at `available_parallelism() >= 6` and 1 otherwise. The 4-vCPU GitHub runner failed the assert because the paranoid profile keeps debug assertions, while every 6-core-or-larger dev box passed.

`std::thread::scope` needs runnable threads, so a core count alone is insufficient for the parallel path. The lane count parameter makes the path fully exercisable on a small box, and machine thresholds stay in the production dispatch only.

## Corpus and reproducibility

- Corpus comparability and cross-session drift pitfalls live in [benchmark methodology](../bench/methodology.md). That covers the text.raw shape transitions and the same-session A/B rule.

### Published-crate test portability

Every fixture access goes through `fixture_entries` and `fixture_bytes`, for corpus and dictionary directories, single corpus files and fuzz artifacts in `tests/`. They skip with a note on `NotFound` but still panic on real IO errors, so the standalone `.crate` `cargo test` passes with no untracked corpus present while genuine breakage still fails. Bypassing them with a direct `Path` read reintroduces the corpus-not-in-git dependency at publish time.

## Editing tools

- The Edit tool matches by text. Identical tail context in the X1 and X2 functions once caused edits to the wrong function, so when editing similar functions the old string must carry an identifier unique to each.
- Match-arm edits should contain only the target arm. One edit copied the next arm wholesale into the new string and produced a duplicate arm exposed only by clippy's unreachable-pattern lint, with behavior that happened to stay correct but redundant.

### Verify batch restores with git diff

A Python replace missed one site because `replace(pat, 2)` hits only the first two, and json.fastest size drifted by 101 B, caught only through deterministic size comparison. Diff first after every change, because deterministic sizes are a free regression probe.
