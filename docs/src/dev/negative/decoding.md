# Falsified directions: decoding

Every entry here is backed by A/B data. If the premises change, such as the table structure, window, corpus or CPU, re-evaluation is allowed, but new decomposition evidence must come first. The differential-testing discipline is in [benchmarking methodology](../bench/methodology.md), and the rules are in the [index](../negative.md).

## MT decode and checksums

### Fixed-offset ramp at job starts

The idea was to suppress matches whose offset sits below a 4-64 KiB floor near MT job starts, so decode pieces never wait on near-boundary reads. The piece-pipeline ceiling stayed at about 1.0-1.14x at every floor. The binding constraint is a crossing read's depth below the cut, computed as `start - src`, and the offset does not matter. A piece can start a copy the moment its source range is published, and a deep source unblocks it regardless of the match's offset size. An offset floor suppresses the wrong set, namely large offsets whose sources are deep and harmless, and leaves shallow-depth reads untouched, where any offset with a source just below the cut qualifies. The depth-semantics ramp, where crossing reads must reach at least D below the job start, is what works. It is landed and env-gated, tracked in the [todo](../todo.md).

### Ramp gate only at probe acceptance

The idea was to emit the ramp gate only at probe-acceptance sites and let backward match extension run to the window floor. The executor-truth guarantee audit found shallow crossing reads at job starts whose probe was in-job, with ml 12-23 at depths 400-2200. Backward extension is a covert leak. It walks source and match back in lockstep at constant offset, so a match probed in-job extends its source across the job start at shallow depth without any probe seeing the crossing. All 9 extension points need the gate's floor, and the floor is piecewise. An in-job source must stop at the job start, because extension is contiguous and passing the boundary drags the source through the whole forbidden band, and an extension that dies inside the band emits an illegal read. An already-deep source extends freely because it only gets deeper. A uniform `start - D` floor both allows the illegal band for in-job sources and disables extension for deep ones.

### Piece watermarks for stage B

The idea was to cut staged output into pieces at block or segment boundaries, track per-segment produced watermarks, and gate pieces per sequence on the exact source range they read. The implementation included exact symbolic reachback analysis, repcode transform chains for piece init states, and an abort and error protocol, and its output was byte-correct. On 64 MiB solo json.zst3 the pieces ran concurrently but completed in a serial chain about 8.5 ms apart, each one piece of unblocked work. mt4 reached 1.23x against the serial-B design's 1.47x, a net regression, and skewed.zst9 reached 0.90x.

The data serializes stage B. A piece's progress at x needs every source of its executed prefix published, and the binding constraint is the shallowest crossing read. The first sequences after any cut read just a few hundred bytes below it, through nearest-match offsets and repcodes, so piece k starts only when piece k-1 is nearly complete. Piece starts stagger by about the minimum offset of 400 B, not by the window. Deepest-read-early cut selection cannot fix this, because shallow reads with offsets around 100-400 co-occur in every early prefix. The deepest read per piece is about the window at p50, json 2 MiB and skewed 4.1 MiB, but shallow first reads always accompany it. B-parallelism for typical match streams is closed unless the encoder guarantees a deep-offset ramp at cut points.

The premise was spent on 2026-09-16. The ramp landed, and the piece-parallel stage B landed with it under the env pair `ZSTDX_MT_RAMP_BYTES` with measured engagement, reaching json.lvl3 mt8 3.2x serial-B as recorded in [mt-stream](../perf/mt-stream.md). This entry's mechanism of arbitrary cuts with live per-sequence gating stays falsified for ramp-less frames. Staging-buffer pooling landed in `3c850d3`, the zero-sequence block fix in `6fdc531`, and wildcopy staged execution in `65e4b98`.

### Checksum on a concurrent hasher thread

The idea was a dedicated hasher thread, with stage B submitting each executed segment range to it through a queue, condvar and lag cap, plus a drain before vector growth reallocs and an unwind guard. On json.zst3 mt4 at 32 MiB solo, the times were 15.4 ms with no hash, 16.9 ms with a serial post-pass and 21-23.5 ms with the concurrent hasher. Instructions were identical, cycles rose 14% and task-clock rose 15%. Skewed went from 46-49 to 43-46, and text was unchanged because the work hides fully. A sync-only variant with queue handoff and no byte reads matched no-hash exactly.

Concurrent placement is shape-dependent. On json the decode working set of input, staging and output is about 75 MiB and already thrashes the 32 MiB L3. The concurrent 32 MiB output read stream then runs at per-core DRAM speed of about 6.6 GB/s, about 5 ms of CPU instead of about 1.6 ms L3-warm, and it degrades the decoder's own miss profile with decode CPU up 6%. The hash lands in the wall one for one. The protocol itself is free. The landed form is an executor-side inline absorb of the hot just-written ranges, giving json 2029 against 1871 at mt4, skewed 756 against 695, and text flat.

### Parallel chunked xxh64

Parallel chunked xxh64 with a merge is impossible by construction. XXH64's spec merge through `mergeAccumulator` only folds the four lanes of one state during finalization. The round function `round(acc, lane) = rotl31(acc + lane·P2)·P1` is not affine in `acc`, because rotl and mod-2^64 addition do not commute, so a stream's state after concatenating chunks A and B cannot be reconstructed from independently hashed chunks. There is no exact parallel XXH64 merge. XXH3 has one, but frame checksums are XXH64.

## Fused loop structure

### Batch processing split

Splitting the fused loop into decode-batch and exec-batch dual loops, in 3 variants, lost 15 to 21%. The intermediate Vec round-trip is not the bottleneck. Batch buffering breaks LLVM register residency and loses the fused out-of-order overlap.

### Two-stage batch pipeline

A two-stage batch pipeline with batch 16 and codegen fully on target gave cycles +9-15% and misses +14-30%. The interleaved decode and exec branch stream inside the fused loop carries the TAGE and BTB load, and splitting it into two pure streams destroys history correlation. The fused structure is a local optimum.

### Streaming decode_step rework

A structural rework of streaming `decode_step`, aimed at the json and skewed residue of 1.05-1.3x, hit the register wall. About 20 live values exceed the 15 GPRs, and the three batch reworks above are the structural attempts. There is no headroom unless the live values shrink first. The next order of magnitude needs an algorithm-level change, such as libzstd's 8-deep sequence ring-buffer pipeline, and its benefit and risk need assessment first.

### Eight-deep sequence ring pipeline

The idea was libzstd's `ZSTD_decompressSequencesLong` model, with decode-ahead into an in-memory ring, match-source prefetch through a decode-side output cursor, and offset resolution moved to decode time. Two variants were built, an 8-slot store-after-exec and a 16-slot store-first so no decoded value crosses the exec body. The results were json.zst3 1380 to 1105-1231, or -11 to -20%, skewed.zst3 -4 to -9%, text.zst3 -1 to -4%, dll100.zst19 1350 to 1162-1233, and dll100.zst9 1536 to 1366-1433. The prefetch alone lost 5% on every shape, including far-match W19 dll streams.

The fused loop is throughput-bound and not latency-bound. Whole-decode IPC is 3.45 with a flat per-instruction cycle profile, nothing above about 3%, so the out-of-order engine already overlaps decode N+1 with exec N for free. The ring's added work converts about 1:1 into lost wall: three stack round-trips for the FSE states, the ring store and load, and the cursor add chain. Software prefetch of match sources hurts uniformly, because Zen4's hardware prefetchers and memory-level parallelism already hide the source-load latency, and 2 exact-address prefetches per sequence cost about 5% even 8 deep. Ring and prefetch structures on this loop are closed, and the residue is instruction-count attrition.

### Copy issue scheduling in the fused loop

The idea was to port libzstd's `ZSTD_wildcopy` issue structure onto the existing wildcopy kernels without a new copy kernel. It used a head-first unconditional copy16 carrying the `ml <= 16` test, a 2x-unrolled tail landing on one exact final chunk inside the 16-byte overshoot budget, unlike libzstd's 32-byte WILDCOPY_OVERLENGTH, and it removed the statically dead `ml > 0` branch, since every ML code base is at least 3. Three builds were measured: the full landing schedule, head-peel only, and ml0-removal only.

Against the pre-change baseline, gungraun stream Ir read full json +2.14%, text +0.79% and skewed +0.10%, peel json +0.24%, text -0.39% and skewed -0.30%, and ml0-only +0.28 to +0.43% on all three. The wall with the peel build under `dec-st` interleaving read json 1.263 to 1.277, 1.355 to 1.362 and 1.290 to 1.306, text 1.213 to 1.217, 1.066 to 1.077 and 1.045 to 1.054, and skewed 1.186 to 1.225 and 1.268 to 1.319. Text got worse even where Ir improved, because the spilled decode half costs more per instruction than the copies it displaced. The exec half shrank as designed, with json exec-file attribution -608K/-522K Ir, skewed -141K and text -98K, but the decode half of the same inlined loop grew +624 to +812K on every shape, through the `offset_hist` base pointer demoted to a per-sequence stack reload and an extra spill store.

The fused loop is one register-allocation domain. Any exec-half shape change re-rolls allocation across the whole loop, and the saturated register budget returns the copy-side saving as decode-half spills at worse than 1:1. skewed.zst9, the 6M-sequence shape, loses 3-4% wall. Even the provably dead `ml > 0` branch removal nets +0.3-0.4% Ir. Copy issue scheduling is closed as a gap-closer. The copy structure is at parity with libzstd's, and the whole residue is the decode half's fixed cost under allocation fragility.

### Unconditional ll above zero

Making ll above zero unconditional, with a copy16 even at ll 0 and garbage overwritten, cut skewed misses by 62% but cost text 9.3%. The reordering effect of removing the branch itself hurts critical files through TAGE and BTB history.

### Inlining the fast path

Inlining the fast path into the fused loop cost json 1.2%. The hot-loop layout is disturbed, so new code goes into an existing out-of-line callee.

### Instantiation slimming

Instantiation slimming, by shrinking the `HEADROOM=false` build, has no mechanism to help. The streaming hot path only instantiates `HEADROOM=true`, and outlining error paths risks homing.

## Branching and state layout

### Repcode registerization

Repcode registerization with fully branchless parsing gave json -2% and misses -22% but skewed +8-11%. The register budget is saturated, and the 3 added live values necessarily evict hotter ones, so the branch-elimination gains stay below the spill cost.

### Purely branchless repcode

A purely branchless repcode, with a cmov chain and a `%3` fold pseudo-slot, regresses json by 5-6%. The repcode branch predicts well enough that removing branches is not a speedup. The final version uses 1 cmov on the parse side and a single branch on the update side.

### Three FSE states in one word

Packing 3 FSE states into one u32 or u64 regresses. The pack and unpack land on the serial FSE chain and cost more than the spills they save.

### Select-ifying the wrapped branch

Select-ifying the wrapped branch left misses unmoved and cost json 2.9%. The branch is 74% taken and drifts with `pos`, and it already predicts well. Stub-attribute the branch before trying a select.

### Read-side fold

The idea was to carry the raw bit container and fold the reload's window rebuild into the read. The count becomes `bits >> (64 - consumed - n)`, and the reload becomes a pointer retreat plus a plain load, which removes the read-tail `win <<= n` and the reload's `bits << consumed`, or 2 variable shifts per sequence, exactly as designed. gungraun stream Ir read json -2.1%, text -1.9% and skewed -1.2%. Solo cycles read json.zst3 -1.8% at IPC 3.608 to 3.632, dll100.zst3 -1.1%, text.zst1 +1.4 to +1.8%, and text.zst3 +3.4%, reproduced twice. The dec-st stream xslow read json -1.4 to -2.2%, dll100 -0.8 to -1.6%, skewed.zst3/9 -3.1/-4.1%, text.zst1 +1.3 to +1.5%, and text.zst3/9/19 +3.0/+4.5/+5.0%, with the ours side at -1.6 to -4.8% and the zstd side flat within 0.3%.

The fold moves the consumed offset off the window, where the pre-shift kept it off the read's count chain, and into the count, in series with `n`. Throughput-bound shapes at IPC 3.6 convert the uop cut into cycles, and the long-sequence text mix pays the longer serial count chain more than the removed shifts buy. The per-shape bar of text staying flat rejects the trade.

### Folding consumed into ip

The idea was a single bit cursor `u = N−57` replacing the `(ip, consumed)` pair. The reload degenerates to loading the container at `src + (u'>>3)`, the tail regime pins the head container, and cold paths run on a local pre-shifted window. Ir stayed flat, with json -0.1% and skewed +0.1%, because the removed retreat and mask operations return as the regime test and sar/cmov. Solo cycles read json -0.2%, dll100 +0.1%, text.zst1 +2.3% and text.zst3 +3.5%. The dec-st stream read json -0.3 to -0.5%, skewed.zst1 +2.6% while zst3/9/19 read -2.1 to -2.6%, and text +2.7 to +3.2%.

One fewer live value and one fewer branch do not survive the re-roll. The raw-bits base's json win vanishes while the text loss persists at the same magnitude. Two independent source shapes losing about 3% text cycles at flat-or-better Ir isolate the mechanism to the count-carrying read itself, and not to allocation luck. The read-side fold family is closed against the per-shape bar.

### Shared fused bit-read body

The idea was one window read serving add-bits and transitions, shared between the flat executor and the staging pass. Three shapes were built: a two-phase split body, an out-pointer result with a cursor-derived last-sequence flag, and a loop-local state direct call. The staging-loop codegen showed mid-loop pushes rising from 0 to 11, stack traffic from 35 to 102 operations, and bzhi gone. json mt2 on pinned dedicated cores read 1786 to 1195, or -33%, and SMT siblings 1491 to 628, or -58%. Unpinned runs read -16 to -25% for every rework, while each rework beat the pre-fusion build on controlled dedicated-core placements by +4 to +12%. The ST flat executor itself improved +1.7-2.8% stream.

The six-widths-six-bases live set fits only a caller without its own cursor state. The staging pass carries one, so every shared fused variant spills there. This was resolved on 2026-10-07 by stopping the sharing. `decode_step<const FUSED>` monomorphizes per call site, with the flat executor fused and the staging split, the staging symbols are instruction-identical to the pre-fusion build, and the ST gains are recovered with no MT exposure as recorded in [perf/decoding](../perf/decoding.md). The shared-body family stays falsified. The early-materialization sub-lever inside the fused body also lost standalone, at solo perf cycles +5-6% on json.zst3 with +0.3% Ir. Scheduling inside the fused loop is part of the optimization, and cycles are the metric when it is reshuffled.

### Real-offset constant rotation

The idea was a fast path in `do_offset_history` for `of > 3` that rotates `hist[2]=hist[1]; hist[1]=hist[0]; hist[0]=of-3` and leaves the repcode slot machinery untouched. It was motivated by a 2026-09-20 differential: libzstd pays 3 moves, while our select rotation ran on every real offset, costing +12 Ir per sequence at the 87-100% real-offset share of json, dll and skewed. Callgrind on json read the fused loop 737.6 to 700.7 M Ir, or -8.7 Ir per sequence and 174.1 to 165.4, with the whole decode at -3.8%. The wall A/B/B/A matrix read json.zst1/3/9/19 -1.0 to -1.8%, skewed.zst3 -1.3%, skewed.zst9 -3.5% and dll100.zst3 -0.4%, but text.zst3/9/19 read +2.8/+4.2/+5.0% on both B runs with more than 300 rounds each and mad of 0.001-0.002, a net loss across the cell set. Solo perf stat on text read instructions -1.0%, cycles +2.5%, and misses and L1d unchanged.

The Ir cut is real, but the fused symbol's codegen re-roll taxes text reproducibly. On text the per-sequence budget is about 1% of the decode, with 57 K sequences at an average ml of 311, so +2.5-5% cannot be the loop's own instructions. It is non-loop code displaced by the relayout, through xxh64-absorb placement or copy-tail layout. Any machine-code change inside the fused symbol, even a strict Ir cut, must clear a text-stream A/B/B/A gate. The rotation was relanded on 2026-10-07: on the reworked codebase it re-rolled text-benign, with solo text.zst3 Ir -0.98% at flat cycles, dec-st text cells within 0.8% noise, json stream -1.1 to -1.6%, skewed.zst3/9 -2.4/-3.0%, dll100 +1 to +1.4%, and mt2/8 spot ahead as recorded in [perf/decoding](../perf/decoding.md). The tax is a per-roll placement lottery, not a property of the cut, and the mandatory text-stream gate arbitrates every roll.

### Five-bit nb fields

The idea was 5-bit nb fields in the packed FSE entry, using `& 0x1f` instead of `& 0xff` at extraction, since every storable nb is at most 31 and the six `and $0x3f` guards LLVM emits before the bzhi counts in the two split phases should die. Callgrind on json read the fused loop 737.6 to 749.4 M Ir, +1.6%, and the whole decode +1.2%. The change lost deterministically at the instruction level before any wall run, because the mask change re-rolled the loop's register allocation and the added spills outweigh the removed guards. This was a second same-day confirmation that source-level micro-cuts do not survive translation into this loop's codegen.

### FSE entry layout swap

The idea was libzstd's exact `ZSTD_seqSymbol` layout of `{u16 next_state, u8 nb_add_bits, u8 nb_bits, u32 base}`, so the loop's unpack becomes 12 single-field loads with no serial shift chain off 3 packed u64 loads, and slot addressing stays unchanged. Callgrind on json read the fused loop 737.6 to 673.8 M Ir, or -15.1 Ir per sequence and 174.1 to 159.0, with the whole decode at -6.8%. gungraun stream Ir read json -3.45%, text -3.37% and skewed -1.38%. Solo perf A/B/A/B under lock read instructions json -7.4%, skewed -4.8% and dll100 -6.0% at uniformly dropped IPC, json 3.61 to 3.35, skewed 3.77 to 3.63 and dll100 3.62 to 3.41, which gave cycles json -0.1%, skewed -1.4%, dll100 -0.4% and text.zst1/3 +0.45/+0.80%. The dec-st matrix A/B/B/A read text.zst1/3/9/19 stream +0.6/+2.0/+1.1/+2.2% on all four cells and both B runs, json.zst3.stream +1.0%, json bulk +1 to +3%, dll100 stream +0.1%, json zst1/9/19 stream -0.8 to -1.2%, and skewed stream -0.2 to -2.0%.

The removed unpack uops were ALU-port filler that the out-of-order engine already hid behind the loop's serial and miss latency. The 15 fewer Ir per sequence buy about 0 cycles, because the 12 field loads consume issue and AGU bandwidth and a second loop-carried pointer makes the table base reload twice per sequence in this codegen. IPC falls about 1:1 with the instruction cut on every shape. The loop is issue and latency saturated, not uop-count saturated. On text the same machine-code change additionally re-rolls the placement tax. The text gate failed and the primary dense cell, json.zst3, regressed, so the change is judged negative despite the largest single-loop Ir cut measured here.

### FSE entry layout swap, two-u32 variant

A 2xu32 field-group variant, with `{u32 [nb_add | nb_bits<<8 | next<<16], u32 base}`, 6 loads, one mask between the load and the read-count sum, and a direct base load, read the fused loop at 737.6 to 756.9 M Ir on json, +2.6% at about 178.6 Ir per sequence. gungraun stream Ir read json +0.28% and skewed +1.11%, and the change lost deterministically before any wall run. Halving the load count re-introduces transition-extraction shifts, and the allocation re-roll spills the value bases back to the stack, visible as the `0x78/0x88/0x90/0xe0` store cluster, so Ir rises. The entry-layout family is closed. The packed u64 with in-loop unpack is locally optimal on this machine, and with this result dies the offset-history fold's only possible partner for a bundle of 5% or more.

## Copy and hash kernels

### BMI2 conversion of the X1 4-stream loop

BMI2 conversion of the 4-stream X1 loop cut instructions 12.7% but cost json and skewed 2-3%. Front-end and code-layout effects eat the gains, and this loop at 1.42 cycles per symbol is at the theoretical limit.

### Copy-loop 32-byte pairing

A copy-loop 32-byte pairing rework with AVX2 copy32 dropped misses hard, json by 22%, but wall clock regressed. Halving misses is not a time benefit, and json-class workloads are not miss-dominated.

### Hand-rolled copy_inline

A hand-rolled `copy_inline` on the large-copy corpus moved memmove from 33% to 1.15% with zero end-to-end gain. Large copies are bandwidth-bound necessary work, which does not conflict with wildcopy succeeding on small-copy corpora.

### AVX-512 xxhash core

An AVX-512 vpmullq vector core for xxhash came out worse. The vpmullq latency of about 4 cycles breaks the serial chain, and twox's 4 chains already hug the scalar machine limit. The right answer is 8-chain scalar interleaving.

### Long-match wide-copy kernels

The long-match wide-copy kernels were re-evaluated under the long-match premise from [dec-gap](../bench/dec-gap.md). The earlier AVX2 copy32 and copy-loop pairing rows used json premises, and the new premise is text.zst3's 151 average 112 KB tile matches at offsets 256K-2M, which are 94% of match bytes. The candidates were libzstd's first-16-peel plus 2x16B per iteration overshoot schedule, 2x16B per iteration and 4x16B per iteration exact-backward-tail, AVX2 256-bit chunks, AVX-512 512-bit chunks, libc memcpy per match, and NT streaming stores at 16, 32 and 64 B with aligned peel. The kernels were replayed on the recorded executed-match trace over a frame-sized buffer, one kernel per process, in 4 interleaved rounds measured in ps/B over the offset-at-least-32 domain. The current 16B loop read 23.0, the libzstd schedule 22.2, or -3.5% copy-only and about -0.8% decode, the exact-tail variants 30.2-30.5, AVX2 48.3, AVX-512 32.0, memcpy 27.7, and NT 157-162, about 7x, where movntdq also needs a 16B-aligned destination and faults when unaligned. In situ the loop holds IPC 3.57 at about 10 B/cycle, the per-core L3 ceiling for the 2.3 MiB streaming window, while the issue ceiling would be about 21 B/cycle.

The long-match copy loop is bandwidth-bound at its ceiling, not latency or issue bound. No kernel converts into decode time, and the one schedule that wins on copy time alone stays below the documented fused-symbol re-roll tax of +3-5% on text-stream. The copy-tail family stays closed under the new premise. Text's unchecked gap of x1.15 belongs to the dense shapes' instruction-volume family described in [dec-gap](../bench/dec-gap.md).

## Table builds and entropy

### Further X2 and X1 Huffman loop work

Further X2 and X1 Huffman loop optimization is closed. The loop runs at about 0.75 cycles per byte, the same order as libzstd's hand-written asm, and there is no conventional path for SIMD bit-serial Huffman.

### Further FSE table-build work

Further FSE table-build optimization is closed. After the build moved from 4.9% to 1.9%, every individual item is below 4%.

## Memory layout and allocation

### Writing literals directly into the flat output

Writing literals directly into the flat output fails its premise. Literals and matches interleave in the output, so a contiguous layout cannot no-op. Direct writing only saves the staging, not the number of writes.

### MADV_HUGEPAGE

`madvise(MADV_HUGEPAGE)` was within noise on THP=always. It was re-evaluated and landed on 2026-09-12 on the THP=madvise bench machine, scoped to the stream-mt accumulate buffer, saving about 3 ms per 32 MiB of 4 KiB first-touch faults and moving solo text.fastest from 2464 to 3706. The original falsification's premise was THP=always, where the kernel maps huge pages regardless. On THP=madvise machines the advice is load-bearing for densely touched burst-scale buffers, so it stays scoped per buffer and is not a global policy.

### Page dispersity on the decode side

The question was whether the same-process page dispersity mechanism seen on 2026-09-29 also taxes decode. In that mechanism a pooled core's big buffers are reallocated onto scattered PFNs by a large alloc and free churn predecessor, and the layout then stays frozen for the whole cell. The churn was one Best or L13 plus one Fast or L3 bulk pass on the same 32 MiB raw in-process, reproducing the pooled slice core's L13 to L3 `resize_tables` transition. Cells ran matrix-identical in fresh processes, with 3 interleaved reps and per-rep order alternation, and the harness's own in-pass state was mined separately through full dec-st runs against single-shape runs.

The driver read bulk json.zst19 -0.6%, json.zst3 +0.2%, skewed.zst19 +0.5% and text.zst19 +0.8%, with stream cells within plus or minus 0.4%, against an encoder reference effect of -25%. The harness in-pass state against single-shape runs read zst19 bulk -1.0/-2.4/-0.5% for json, text and skewed, with stream within plus or minus 0.4%. The out buffer, the only big pooled hot buffer at 32 MiB, had a contiguous-PFN-pair ratio of 0.04-0.36 across runs in both states with no speed correlation, at 2035-2188 MiB/s. Steady-state dTLB load misses were 46-51 ppm of instructions in both states at identical 287M instructions per iteration, against the encoder reference's +62% misses per iteration.

Decode is insensitive to dispersity, not merely unexposed. The flat path's output writes are a sequential stream and match sources sit in the cache-resident recent window, so no loop's cost scales with the out buffer's PFN layout. The encoder differs because its 128 KiB staged-to-win copies run over table and window regions and take 38.8% of cycles in its collapsed state. The pooled decoder's window-scale ring reserve is never touched on the dict-free flat path, with 2 of 513 pages present in the VMA scan. The streaming cell and the one-shot APIs re-allocate per call, so the frozen-layout mechanism cannot attach there at all, and `matrix --mode all` runs dec-st first, so no encoder-churn-then-decode sequencing exists in the harness. No decode-side hugepage wiring is warranted.

## Dispatch and CPU tiers

### AVX2 constant threaded dispatch

AVX2 constant threaded dispatch with 8 instantiations is feasible but regresses. The nested `target_feature` pattern works, and the codegen risk is high. It stays on the intermediate-tier todo.

### AVX2 intermediate tier for decode dispatch

The idea was an AVX2 intermediate tier for the three BMI2 gates: the fused flat executor, the two-pass sequence decode, and the MT segment decode. The premise was that mid-tier AVX2-only CPUs fall to scalar and lose the decode lead. Forced-scalar against BMI2 on Zen4, same binary, position-balanced over 8 rounds through `ZSTDX_DEC_SIMD_TIER`, read json.zst3/19 -6.7/-7.3%, dll100.zst3/9 -5.6/-6.3% and skewed.zst3 -3.9% stream wall, while text.zst3/19 read -1.9/-1.3% and skewed.zst19 -0.4% because the copy-bound cells are flat. Perf stat on json.zst3 read scalar +18.0% instructions at IPC 3.83 against 3.49, or +7.4% cycles, and the gap is pure variable-shift instruction volume. The bmi2 instantiations emit shlx, shrx and bzhi only, with 32/36/32 sites in the fused symbol and zero pdep or pext, so they are Zen1 and Zen2 microcode-safe. The default-path canary of base against override build read plus or minus 0.0-0.7%, with no codegen interference.

No such CPU exists. The decode wide gate is BMI2, and AVX2 and BMI2 shipped in the same silicon generations, Haswell in 2013 and Zen in 2017. No later x86 part separates them, and Intel's AVX-cut Celeron and Pentium lines drop AVX2 as well. Every AVX2-capable machine already runs the BMI2 tier, and the scalar fallback population of pre-Haswell and pre-Zen parts has no AVX2 to exploit. AVX2 also has no scalar variable-shift instruction, and batching through vpsrlvq and vpsllvq is the falsified batch and packing class above. Even a synthetic AVX2-without-BMI2 CPUID from QEMU surgery has nothing to select. The decode lead on mid-tier CPUs is protected by the existing BMI2 tier, and wide-copy AVX2 kernels for the decode copies were falsified separately. Tiers are forceable through `ZSTDX_DEC_SIMD_TIER=scalar|bmi2`, where unset means detect and an invalid value panics, and the files, mtcheck and corrupt gates pass under both forced tiers.
