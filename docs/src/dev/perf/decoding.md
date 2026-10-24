# Decoding Path

This page answers where decode time goes, how the output and executor paths are structured, and which residual gaps remain measured floors. It is organized by decode stage, with the fused sequence loop first and parallel decode last. Disproven directions live in the [Disproven Directions](../negative.md) page, and the per-stage falsification data is in [negative.md](../negative/decoding.md). Open work is in the [Todo List](../todo.md).

## Output path

The decoder has three output models. A hand-written ring buffer serves dictionary frames only. It implements both discard-from-head and copy-internally-to-tail, where `VecDeque` and `Vec` can each do only one efficiently, with a modulo-free wrap and hand-written `extend_from_within` for four layouts.

Dictionary-free slice decoding writes raw, RLE and compressed blocks directly into the caller's target buffer, bypassing the ring and drain copies, with a block-level budget check and a `TargetTooSmall` error variant. Slice decoding gained 13% to 35% across the board.

The streaming model ports libzstd's outBuff design. Steady-state capacity is `window + 2·block + 64`, and a two-generation virtual-mapping wrap through `FlatView{origin, prev_origin}` maintains a globally monotonic virtual address. There are zero sliding copies and a flush leaves no window behind. Large-window corpora with an 8 MB window gained 85% to 125%, and random and zeros overtake zstd's streaming decoder.

## Fused sequence loop

### Structure

The loop fuses decode with execute, so sequences execute immediately after decoding and never round-trip through a sequence vector. Pure fusion was initially -1.8%. It turned positive only once RLE was faked as a single-state table, with a fake entry that self-transitions at `acc_log` 0, which eliminated three `Option` registers and six branches per sequence and deleted four variant loops. Json.zst1 gained 10% and overtook the zstd crate's slice decoder, and json.zst3 gained 7%.

`decode_step` carries state, the executor uses a pointer cursor, and a `HEADROOM` const specializes the copy strategy: when streaming buffers guarantee block size plus 16 bytes of slack, the wildcopy gate disappears entirely. The original landing also removed the per-sequence budget gate of `w+ll+ml+16 <= out_len`, which was re-added unconditionally after the SIGSEGV it enabled, documented in the [decoding pitfalls](../pitfalls/decoding.md), so HEADROOM now only decides the copy strategy. Streaming gained 4% to 7.4%.

The loop carries only the three FSE states rather than packed entries, and `bits` and `src_len` leave the stream state because a reload can always rebuild the bit window from memory. The executor cursor becomes raw pointers, which folds away the base. `SeqDecoder::new` builds the three state tables once per block.

The loop has a register wall. With 15 or more live values LLVM stack-spills the bit window, and on the annotated assembly a `shlx` with a memory operand is the tell. All structural rework was disproven, including splitting out batching, two-stage pipelines and register-resident repcodes, so the fused structure is locally optimal.

### Cost model

Whole-decode IPC is 3.45 on json.zst3 with a flat cycle profile where no instruction exceeds about 3%. The loop is throughput-bound rather than latency-bound, so decode of sequence N+1 already overlaps execute of sequence N through out-of-order execution for free. Added bookkeeping converts about 1:1 into wall, including the three FSE states' stack round-trips, the ring store and load, and the prefetch-cursor add chain. The loop's L1 miss clusters, the wildcopy match source at about 8%, its address math at about 4.5%, bitstream reload at about 4% and FSE entry loads at about 3%, all hide behind that overlap, which is also why match-source prefetch buys nothing at -5% uniformly. The remaining gap to libzstd is instruction quality, and ring and prefetch restructures are falsified.

Porting libzstd's `ZSTD_wildcopy` issue structure onto the existing kernels shrank the execute half by exactly the predicted 0.5M to 0.6M instructions on json yet lost net instructions and wall on every shape. The loop is one register-allocation domain, and the saturated budget returns the copy-side saving as decode-half spills of 0.6M to 0.8M instructions. The copy structure is at parity with libzstd's, and the residue is decode-half instruction quality under allocation fragility, where every source change to either half re-rolls the whole loop's allocation by ±600K to 800K instructions on json.

### Fused bit read

The add-bits group and the transition group are consecutive in the bitstream and jointly bounded: a sum of 31 or less implies the total is 57 or less, which is exactly the coverage the post-reload window guarantees. One window read therefore serves both groups. The last sequence has no transitions and reads add bits alone, and the wide path above 31 is outlined cold. The spill's store-to-load forwarding round trip sat on the serial bit path twice per sequence, and fusing leaves one.

The trade won as designed on the single-threaded flat executor. Instructions rose at json +4.6%, text +4.4% and skewed +2.0%, IPC rose to match at json 3.462 to 3.565, and wall improved on every sequence-dense shape at json stream +1.7% to +2.0%, dll100 +2.0% to +2.8% and text.zst1 +1.9%. Trading off-chain spill uops for fewer serial chain segments converts into IPC even at higher instruction count.

The first re-land was reverted because of a caller register-budget finding, not because the conversion was wrong. The fused body keeps six widths, six bases and the window value live across one extraction, a shape that fits the flat executor's loop but not the sequence-staging pass. There it spilled 11 mid-loop pushes, tripled stack traffic in the 2026-09-28 measurement, and cost the piece-parallel path json mt2 -27% on dedicated cores, -58% on SMT siblings and -33% unpinned. Three shared-body reworks, a two-phase split body, an out-pointer result and a cursor-derived last-sequence flag, each beat the pre-fusion build on controlled dedicated-core placements at +4% to +12%, yet still lost the unpinned cross-CCD mt2 cell at -16% to -25%, with the residual not fully attributed.

The resolution was to stop sharing the body. `decode_step<const FUSED>` monomorphizes per call site, the flat executor inlines the fused read, and the staging loops that call `next` keep the split-read body. The staging path's machine code is instruction-identical to the pre-change build by an address-normalized `objdiff` of `decode_sequences*`.

### Read-side folds

Both remaining levers of the fused bit-read family were falsified as a class. Carrying the raw container so the read count folds the consumed offset and reloads shift-free, and folding `consumed` into a single bit cursor `u = N-57` so the reload is a plain load at `src + (u'>>3)`, both deliver their predicted uop cuts at json -2.1% instructions or flat. They win the throughput-bound shapes at json -1.8% cycles, dll -1.1% and skewed stream -2% to -4%.

They lose text reproducibly at +1.4% to +3.5% cycles and +2.7% to +5.0% stream wall across two independent allocation re-rolls. Moving the consumed offset from the window into the count puts the loop-carried cursor in series with `n`, the sequence count, on the read's serial chain, and the latency-leaning long-sequence mix pays that beyond what the removed shifts buy. The pre-shifted window's count derived from `n` is load-bearing.

The same profiling round attributed text.zst3 and zst9 cycles: the untouched xxh64 absorb is 52% to 60% of `prof dec` cycles and the fused loop 11% to 21%, from low-sample profiles, so text-stream wall barely moves for loop-side wins while loop-side losses reproduce at full size.

The real-offset constant rotation landed after the placement lottery made it text-benign. The fast path `of > 3` does the plain three-move rotation `hist[2]=hist[1]; hist[1]=hist[0]; hist[0]=of-3`, off the select chain that the repcode domain keeps through `do_offset_history`. The 87% to 100% real-offset share on json, dll and skewed had been paying the full select quartet for a constant rotation.

Solo perf on pinned interleaved rounds measured text.zst3 instructions -0.98% at cycles +0.05% over four repetitions, json.zst3 instructions -4.2% with cycles -1.2% to -1.7%, and skewed.zst9 instructions -4.7% with cycles -3.4% to -3.9%. Stream A/B/B/A moved json -1.1% to -1.6% at all four levels with bulk at -0.3% to -1.5%, skewed.zst3 and zst9 stream -2.4% and -3.0% with zst19 at -0.9% to -1.0%, and dll100 +1.0% and +1.4%. Text stayed inside ±0.8% in both directions while the zstd reference drifted alongside, and mt2 and mt8 spot checks read +7% and +3.8% because stage B executes the same selects.

The earlier negative record stands as layout-lottery proof rather than a mechanism veto. Identical source shapes drew +2.8% to +5% text on one roll and ±0% on another, so the text-stream A/B/B/A gate stays mandatory for every fused-symbol machine-code change. A same-build hotspot profile put text.zst3 at 51.5% xxh64 absorb, 22.7% fused executor, 15.4% drain-copy memmove, 5.0% buffer-init memset and 2.2% wrapped path, with the drain copy and init being API-shaped costs both sides pay, as zeros.zst9 with no sequences shows the same memmove and memset pair. Json.zst3 read 77.9% fused, 6.9% absorb, 4.1% wrapped and 3.1% FSE build, and skewed.zst9 read 82.9% fused and 8.2% wrapped. Text's fused share is why loop-side wins barely move its wall while collateral placements of the 2026-09-20 class reproduced at full size.

## Sequence decoding

The sequence decode path uses a 64-bit read-back bitstream with a pointer and a bits-consumed count plus an inlined reload, and a packed u64 FSE entry whose base and add bits are folded in at table build, so all fields arrive in one load. That combination accounted for about 15% of sequence decoding. Three packed tables merge into one base array with constant slot offsets at LL 512, ML 512 and OF 256, which kills the per-iteration table pointer reload. A pre-shifted bit window `win = bits << consumed` removes two shifts per read.

Grouped batch extraction handles add bits three at a time and state transitions three at a time from one serial window read split in parallel, with a sum of 31 or less avoiding a mid-sequence reload, which gained 7% to 8% on skewed.zst3 and 3% to 5% on json.zst3. Zero-width branchless reads through `wrapping_shr` and bzhi masks, plus repcode resolution rotations through select, gained 4.5% on skewed.zst3 streaming. A BMI2 runtime dispatch with a shared `#[inline(always)]` implementation and `#[target_feature]` wrappers is neutral on Zen4 and benefits Intel.

Per-symbol spread precomputation for the FSE table build through inline `SymbolSpreadInfo` arrays moved the table-build profile from 4.9% to 1.9%. `do_offset_history` is slot-indexed with single-value slot selection and rotation for code-1 plus zero-literal cases, backed by exhaustive equivalence tests, which reduces instructions per sequence.

## Executor

The executor ports libzstd's wildcopy copy stack. Literals of 16 bytes or less use inline copy16, with `literals_buffer.reserve(16)` guaranteeing the over-read stays in bounds, and larger runs use a 16-byte chunk loop. A match with offset 16 or more uses copy16 in a 16-byte loop, an offset from 8 to 16 uses an 8-byte loop, and an offset below 8 uses `overlapCopy8`, where dec32 and dec64 tables write 8 bytes at a time to lift the effective offset to 8 or more. The first 4 bytes are written byte by byte so stores feed loads.

A budget gate of `w+ll+ml+16 <= out_len` guarantees the overshoot lands in a legal region, and the tail takes the exact slow path. The motivation was json and skewed being masses of 5 to 6 byte small copies: an `LD_PRELOAD` measurement on 32 MiB showed about 3M PLT calls taking about 35% of time. After landing, memmove fell from 14.3% to 1.6% and json, skewed and text decoding gained 23% to 45%.

The active-segment match source simplifies algebraically to `dst - offset`, so virtual distance equals physical distance and base indirection disappears. Wrapped sources are outlined cold to shed four view live values, and boundary checks merge three into one. A previous-segment linear fast path uses the fact that a wrapped previous-segment source physically sits at `dst + seg_a_end - offset`, above the write cursor by at least `MAX_BLOCK`, so wildcopy's double-sided overshoot is safe and one wildcopy replaces generic segment walking. It is placed in the existing out-of-line callee to preserve hot-loop codegen, and gained 4.8% on skewed.zst9.

## Huffman literals

The X2 double-symbol table ports libzstd's `HUF_decompress4X2` and adds an 80% pairing threshold: the pairable fraction of the code space is computed from the weights, and below the threshold the space is left empty and X1 is used. Skewed.zst1 gained 43% and skewed.zst3 15%.

X1 and X2 stream-state pointerization folds the base into `ip` and `op` and reduces live values from 16 to 14, cutting inner-loop instructions 10%. The conclusion is that this sits at the practical floor: libzstd's hand-written assembly does three decoding-table loads, which is memory-port-bound at about 0.7 cycles per byte, while our one load plus two shifts is ALU-bound at about 0.75 cycles per byte, the same magnitude.

## Parallel decode

MT decode is our exclusive dimension because libzstd has no MT decode API. On the 2026-10-18 pass, json.zst3 reached 1.65 times our single-thread throughput and 1.23 times the zstd stream reference at 16 workers, and dll100.zst3 reached 1.90 times single-thread and 1.36 times the reference. Skewed.zst9 read 1.25 times single-thread and 0.96 times the reference, with its absolute band wandering ±15% across passes, and text stayed flat at 0.97.

The scaling comes from decoding independent restart points in parallel, with each worker executing the same fused loop over its segment. The memory-bound verdict and execute walls stand, and the remaining no-lever verdict is that overlapping execute with the verdict is a falsified class. Current cells and their spreads are in the [snapshot](../bench/snapshot.md).

## Miscellaneous

Frames without a content checksum skip xxh64 init and update, gaining 2.7%, and the shared in-tree eight-chain implementation lives on the [encoding page](encoding.md). Consecutive blocks in predefined mode do not rebuild FSE tables, which eliminates per-block allocation and gains 2.5%. The `StreamingDecoder::read` path decodes until the caller's buffer is full, which removed the one-block granularity and gained 2%.

Fixed-width copies with allowed overshoot go through `copy_bytes_overshooting`, using u128 and usize whole-word loops that bypass memcpy's length dispatch. Overlapping copies that must be repeated use `repeat_in_chunks`, which performs log2 memcpys instead of `ml/offset`. Interleaved Huffman table access and output are unchecked, a change that measured about zero because branch prediction already absorbed the bounds check but was kept.

## Residual

Against libzstd 1.5.7 on the same files, streaming decode runs at 1.18 to 1.32 times the time on json and dll, 1.10 to 1.27 on skewed and 1.04 to 1.21 on text. We execute 1.34 to 1.41 times the instructions at equal or better IPC with equal L1 data misses, so the loop is throughput-bound on both sides and the entire cycle gap is instruction volume. Json reads 174 against 126 instructions per sequence, a difference of 48, and the loop alone is 82% of the cycle gap while non-loop fixed costs are at parity. The differential attribution is in [dec-gap](../bench/dec-gap.md), and the falsified variants are in the [decode negative results](../negative/decoding.md), also reachable as [negative.md](../negative.md).

The 48 instructions split five ways. Entry unpacking costs +10, from three packed loads and 24 extract shifts against their 12 field loads. Offset-history machinery costs +12 on real offsets. Stack round-trips cost +15, from the window store and load, `consumed` bookkeeping, double state stores and `rem` and `op` spills. Read and splits cost +11, from the fused read plus six field splits against per-field two-shift reads. Literals cost +2.

Text is not a latency exception. In its reference frames, 94% of match bytes are about 150 tile-copy matches at an average of about 112 KB and offsets from 256K to 2M, and their copy loop runs at IPC 3.57 at the per-core L3 ceiling. The xxh64 absorb is exact parity at 346M against 345.5M cycles per 40 passes of 32 MiB and merely dilutes the ratio: x1.071 checked and x1.15 unchecked. The unchecked residual is the same instruction-volume family as the dense shapes.

The register wall is proven and every structural rework, including the ring pipeline, is falsified. The read-side serial-chain folds win the dense shapes about 2% yet reproducibly cost text 3% to 5% wall, because folding the consumed offset into the count puts the loop cursor on the read's serial chain while the pre-shifted window's count is load-bearing for the long-sequence mix. Of the two surgical instruction cuts from that round, the 5-bit nb fields lost deterministically at +1.6% instructions, and the real-offset rotation re-landed once the codebase re-rolled its placement text-benign. The placement tax is a per-roll lottery rather than a property of the cut, and the text-stream gate arbitrates each roll.

The entry-struct layout swap is also falsified. Both the field-per-load form and a 2×u32 field-group variant cut 15 instructions per sequence and bought no cycles, because IPC fell about 1:1 with the instruction cut on every shape. The packed u64 entry is locally optimal on Zen4. Text's copy-tail latency is latency-class with no instruction lever. With the read-side folds and both entry layouts gone, the fused loop's residual against libzstd has no open instruction-volume lever.

Issue and latency saturation was measured directly at the front end on 2026-09-21 with Zen4 op-cache, loop-buffer and issue-stall events plus IBS fetch sampling. Both loops dispatch about 99.5% of ops from the op cache and 0% from the loop buffer, and the front-end stall differential is 2.4 cycle-equivalent percentage points or less with no block concentration. The fused loop's own IBS fetch latency is about zero with at most 0.24% misses on every shape. We dispatch more ops per cycle than libzstd on the dense shapes, 4.22 against 4.11 on json, while every issue resource reads at or below theirs. The residual is op volume through execution ports, and no front-end or layout lever remains.
