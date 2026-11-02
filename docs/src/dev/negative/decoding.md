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

The batch and pipeline entries below were measured on the batched-read loop that the libzstd-shaped loop replaced on 2026-11-02. They split decode from execution, which the new loop keeps fused like libzstd, so they stay closed until a decomposition of the new loop says otherwise.

### Single pieces of libzstd's loop

From 2026-09 to 2026-10, four pieces of libzstd's sequence loop were landed one at a time on the batched-read loop, and each was recorded as falsified. The unconditional literal copy16 cut skewed misses by 62% and cost text 9.3%. libzstd's `ZSTD_seqSymbol` field-per-load entry cut 15.1 instructions per sequence while IPC fell about 1:1. Register-resident repcodes read json -2% and skewed +8% to 11%. libzstd's wildcopy issue order shrank the copy half and grew the decode half by spills. Every piece re-rolled register allocation inside one saturated loop and lost to the text-stream placement gate.

The whole structure, ported at once on 2026-11-02, won every dense shape by 14% to 51% with text ahead as well, as recorded in [dec-gap](../bench/dec-gap.md). The piece verdicts are withdrawn. A piece's loss inside a register-saturated loop says nothing about a structure that changes the live set as a whole.

### Batch processing split

Splitting the fused loop into decode-batch and exec-batch dual loops, in 3 variants, lost 15 to 21%. The intermediate Vec round-trip is not the bottleneck. Batch buffering breaks LLVM register residency and loses the fused out-of-order overlap.

### Two-stage batch pipeline

A two-stage batch pipeline with batch 16 and codegen fully on target gave cycles +9-15% and misses +14-30%. The interleaved decode and exec branch stream inside the fused loop carries the TAGE and BTB load, and splitting it into two pure streams destroys history correlation.

### Eight-deep sequence ring pipeline

The idea was libzstd's `ZSTD_decompressSequencesLong` model, with decode-ahead into an in-memory ring, match-source prefetch through a decode-side output cursor, and offset resolution moved to decode time. Two variants were built, an 8-slot store-after-exec and a 16-slot store-first so no decoded value crosses the exec body. The results were json.zst3 1380 to 1105-1231, or -11 to -20%, skewed.zst3 -4 to -9%, text.zst3 -1 to -4%, dll100.zst19 1350 to 1162-1233, and dll100.zst9 1536 to 1366-1433. The prefetch alone lost 5% on every shape, including far-match W19 dll streams. Zen4's hardware prefetchers and memory-level parallelism already hide the source-load latency, so two exact-address prefetches per sequence cost about 5% even 8 deep.

### Wide decode body in the staging loops

The idea was one window read serving the add-bits and the transitions, shared between the flat executor and the staging pass that `SeqDecoder::next` drives. Three shapes were built: a two-phase split body, an out-pointer result with a cursor-derived last-sequence flag, and a loop-local state direct call. The staging-loop codegen showed mid-loop pushes rising from 0 to 11, stack traffic from 35 to 102 operations, and bzhi gone. json mt2 on pinned dedicated cores read 1786 to 1195, or -33%, and SMT siblings 1491 to 628, or -58%. The staging loops carry their own output cursor, so a six-widths-six-bases live set spills there, and `decode_step` keeps the two-read split body.

## Branching and state layout

### Three FSE states in one word

Packing 3 FSE states into one u32 or u64 regresses. The pack and unpack land on the serial FSE chain and cost more than the spills they save.

### Bit-container forms

Two forms were measured on the batched-read loop. Carrying the raw container, so the read count folds the consumed offset and the reload is a pointer retreat plus a plain load, cut json instructions 2.1% and won json, dll and skewed by 1% to 4%, but lost text 3% to 5% wall reproducibly. A single bit cursor `u = N−57` replacing the `(ip, consumed)` pair read flat instructions and lost text about 3%. The raw-container form is libzstd's own reader, so on the libzstd-shaped loop the premise changed and the form is reopenable.

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

The long-match copy loop is bandwidth-bound at its ceiling, not latency or issue bound. No kernel converts into decode time, and the copy-tail family stays closed under the long-match premise.

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

AVX2 constant threaded dispatch with 8 instantiations is feasible but regresses. The nested `target_feature` pattern works, and the codegen risk is high.

### AVX2 intermediate tier for decode dispatch

The idea was an AVX2 intermediate tier for the three BMI2 gates: the flat executor, the two-pass sequence decode, and the MT segment decode. The premise was that mid-tier AVX2-only CPUs fall to scalar and lose the decode lead. Forced-scalar against BMI2 on Zen4, same binary, position-balanced over 8 rounds through `ZSTDX_DEC_SIMD_TIER`, read json.zst3/19 -6.7/-7.3%, dll100.zst3/9 -5.6/-6.3% and skewed.zst3 -3.9% stream wall, while text.zst3/19 read -1.9/-1.3% and skewed.zst19 -0.4% because the copy-bound cells are flat. Perf stat on json.zst3 read scalar +18.0% instructions at IPC 3.83 against 3.49, or +7.4% cycles, and the gap is pure variable-shift instruction volume. The bmi2 instantiations emit shlx, shrx and bzhi only, with zero pdep or pext, so they are Zen1 and Zen2 microcode-safe. The default-path canary of base against override build read plus or minus 0.0-0.7%, with no codegen interference.

No such CPU exists. The decode wide gate is BMI2, and AVX2 and BMI2 shipped in the same silicon generations, Haswell in 2013 and Zen in 2017. No later x86 part separates them, and Intel's AVX-cut Celeron and Pentium lines drop AVX2 as well. Every AVX2-capable machine already runs the BMI2 tier, and the scalar fallback population of pre-Haswell and pre-Zen parts has no AVX2 to exploit. AVX2 also has no scalar variable-shift instruction, and batching through vpsrlvq and vpsllvq is the falsified batch and packing class above. Even a synthetic AVX2-without-BMI2 CPUID from QEMU surgery has nothing to select. The decode lead on mid-tier CPUs is protected by the existing BMI2 tier, and wide-copy AVX2 kernels for the decode copies were falsified separately. Tiers are forceable through `ZSTDX_DEC_SIMD_TIER=scalar|bmi2`, where unset means detect and an invalid value panics, and the files, mtcheck and corrupt gates pass under both forced tiers.
