# Decoding Path

This page answers where decode time goes, how the output and executor paths are structured, and which residual gaps remain. It is organized by decode stage, with the fused sequence loop first and parallel decode last. Disproven directions live in the [Disproven Directions](../negative.md) page, and the per-stage falsification data is in [negative.md](../negative/decoding.md). Open work is in the [Todo List](../todo.md).

## Output path

The decoder has three output models. A hand-written ring buffer serves dictionary frames only. It implements both discard-from-head and copy-internally-to-tail, where `VecDeque` and `Vec` can each do only one efficiently, with a modulo-free wrap and hand-written `extend_from_within` for four layouts.

Dictionary-free slice decoding writes raw, RLE and compressed blocks directly into the caller's target buffer, bypassing the ring and drain copies, with a block-level budget check and a `TargetTooSmall` error variant. Slice decoding gained 13% to 35% across the board.

The streaming model ports libzstd's outBuff design. Steady-state capacity is `window + 2·block + 64`, and a two-generation virtual-mapping wrap through `FlatView{origin, prev_origin}` maintains a globally monotonic virtual address. There are zero sliding copies and a flush leaves no window behind. Large-window corpora with an 8 MB window gained 85% to 125%, and random and zeros overtake zstd's streaming decoder.

## Fused sequence loop

### Structure

The flat executor fuses decode with execute, so each sequence executes right after it is decoded and never round-trips through a sequence vector. Since 2026-11-02 the loop follows libzstd's `ZSTD_decompressSequences_body` as a whole structure, which closed the long-standing gap described in [dec-gap](../bench/dec-gap.md).

Decoding reads each packed entry field by field through `EntryRef`, in libzstd's `ZSTD_seqSymbol` order of next state, add bits, state bits and base. The offset branches on `ofBits > 1`: offset codes 2 and up are real offsets, and codes 0 and 1 are repcodes resolved right there. The repcode history lives in three locals for the whole block and is written back on success. Each add-bits field is its own read behind a zero-width branch, which predicts well because ml and ll codes rarely carry extra bits. A total of 31 bits or more reloads mid-sequence, so the ll field and the 26 transition bits always fit in the 57 bits a reload leaves. The last sequence skips the transitions and the reload, because they would read past the stream's end.

RLE streams are faked as a single-state table whose entry self-transitions at `acc_log` 0, so the loop has no RLE special cases. `SeqDecoder::new` builds the packed tables once per block when they change. The loop carries only the three FSE states and the pre-shifted bit window, and a reload always rebuilds the window from memory.

### Cost model

The loop is throughput-bound, and out-of-order execution overlaps the decode of sequence N+1 with the execution of sequence N. Branch misses were the deciding cost before the port: a branch on `ll > 0` ahead of the literal copy missed constantly on short-literal shapes, and removing it inside the new structure cut json.zst3 misses from 1.29M to 0.76M per decode. The full counts against libzstd's clang build are in [dec-gap](../bench/dec-gap.md).

The staging paths keep a different decode body. `decode_step` serves `SeqDecoder::next`, which the ring executor and MT stage A drive, and it reads all three add-bits fields with one window read split in parallel, then the three transitions with a second read. It keeps wire offsets, because stage A cannot know the repcode history at a segment start. Its loops carry their own output cursor, so a wider decode body spills there, as recorded in [negative](../negative/decoding.md).

## Sequence decoding

The bitstream is a 64-bit read-back container with a byte pointer and a bits-consumed count, carried as a pre-shifted window `win = bits << consumed`, which removes a shift per read. Three packed tables merge into one base array with constant slot offsets at LL 512, ML 512 and OF 256, so all three streams address through one pointer. Zero-width transition reads are branchless: `wrapping_shr` turns a zero width into a shift by zero, and the field mask zeroes the result. A BMI2 runtime dispatch with a shared `#[inline(always)]` body and `#[target_feature]` wrappers compiles the variable shifts to single-uop `shlx` and `shrx`.

Per-symbol spread precomputation for the FSE table build through inline `SymbolSpreadInfo` arrays moved the table-build profile from 4.9% to 1.9%.

## Executor

The executor ports libzstd's `ZSTD_execSequence`. One combined check sends a sequence to the out-of-line exact path `exec_sequence_end` when its literals run past their end or its end passes the wildcopy limit. That limit is the target end under HEADROOM, where the backing buffer guarantees 16 bytes of slack, and 16 bytes short of it otherwise. The exact path re-checks every bound in libzstd's order and copies without overshoot, so a valid frame reports `TargetTooSmall` exactly when the target is too small.

On the hot path, literals copy 16 bytes unconditionally, and longer runs continue in a 16-byte chunk loop. The literals buffer keeps a zeroed 16-byte tail, so the over-read at a zero length stays initialized, and the garbage written past the literals is overwritten by the match. A match with offset 16 or more uses a 16-byte chunk loop. Offsets from 8 to 15 use an 8-byte loop, and offsets below 8 go through `overlap_copy8`, where libzstd's dec32 and dec64 tables write 8 bytes at a time to lift the effective offset to 8 or more. Before the inline copies landed, an `LD_PRELOAD` measurement on 32 MiB showed about 3M PLT calls taking about 35% of time on json and skewed.

A single compare of the offset against the active generation's start guards the match source. Inside the active generation the source is `dst - offset`. Behind the compare, the window check rejects out-of-window offsets, and the cold `copy_wrapped_match` serves sources in the wrapped-away previous generation. A previous-generation source physically sits at `dst + seg_a_end - offset`, above the write cursor by at least `MAX_BLOCK`, so one wildcopy serves it.

## Huffman literals

The X2 double-symbol table ports libzstd's `HUF_decompress4X2` and adds an 80% pairing threshold: the pairable fraction of the code space is computed from the weights, and below the threshold the space is left empty and X1 is used. Skewed.zst1 gained 43% and skewed.zst3 15%.

X1 and X2 stream-state pointerization folds the base into `ip` and `op` and reduces live values from 16 to 14, cutting inner-loop instructions 10%. The conclusion is that this sits at the practical floor: libzstd's hand-written assembly does three decoding-table loads, which is memory-port-bound at about 0.7 cycles per byte, while our one load plus two shifts is ALU-bound at about 0.75 cycles per byte, the same magnitude.

## Parallel decode

MT decode is our exclusive dimension because libzstd has no MT decode API. On the 2026-11-02 pass after the stage-B port, json.zst3 reads 1.61 times its own single-thread throughput at mt16, dll100.zst3 1.92 times and skewed.zst9 1.06 times, at absolute cells of 3222, 3086 and 1043 MiB/s against single-thread controls of 2003, 1612 and 982. On the 2026-10-18 pass, json.zst3 had reached 1.65 times our single-thread throughput and 1.23 times the zstd stream reference at 16 workers, dll100.zst3 1.90 times single-thread and 1.36 times the reference, and skewed.zst9 1.25 times single-thread against a ±15 percent absolute band across passes.

The scaling comes from decoding independent restart points in parallel. Stage A decodes each segment's sequences through the staging `decode_step` into a vector of wire values, and stage B executes them in order with its own executor, because only stage B knows the repcode history.

Stage B follows the ST loop's libzstd shape (see [dec-gap](../bench/dec-gap.md)). Each staged sequence resolves its offset in locals off the wire value, through the same selector mapping `do_offset_history` pins for its other callers: values above 3 are real offsets minus the repcode bias, and 1 through 3 pick rep0, rep1, rep2 or the rep0-minus-1 pseudo slot from the wire bits and the literal-length-0 flag. The region's write limit and wild cap fold into one end compare, and one combined check against the literals end and that compare sends every edge case to the out-of-line exact path `exec_staged_end`. The hot path copies literals unconditionally in 16-byte chunks and the match through `wildcopy_match`; stage A appends a 16-byte zero tail to the staged literals after the last block, and the block plans' literal ranges index positions recorded before the extension, so the copy's source overread stays initialized.

The port cut stage B's execute time per segment to 43 percent on skewed.zst9, at 18.0 to 7.8 ms per 16 MiB segment, and to 65 percent on json.zst3 and 70 on dll100.zst3. One locked before/after pair on the same day moved json.zst3 mt16 from 2197 to 3222 MiB/s, dll100.zst3 from 2291 to 3086, and skewed.zst9 from 776 to 1043, the last above its single-thread control for the first time.

Where the MT wall sits after the port: stage B's execute is memory-bound on the staging round-trip, reading every staged literal and sequence while writing the whole output, and stage A produces in parallel behind it, so json and dll100 split their walls between the two. Skewed.zst9 carries only the two 16 MiB restart points its encoder emitted, so its wall is the first segment's staging latency plus two executes, and stage A's per-worker staging rate holds every restart-poor frame near single-thread speed. The restart points belong to the frame and the stage-A body widening is falsified, so restart-poor frames stay at this floor. Current cells and their spreads are in the [snapshot](../bench/snapshot.md).

## Miscellaneous

Frames without a content checksum skip xxh64 init and update, gaining 2.7%, and the shared in-tree eight-chain implementation lives on the [encoding page](encoding.md). Consecutive blocks in predefined mode do not rebuild FSE tables, which eliminates per-block allocation and gains 2.5%. The `StreamingDecoder::read` path decodes until the caller's buffer is full, which removed the one-block granularity and gained 2%.

Fixed-width copies with allowed overshoot go through `copy_bytes_overshooting`, using u128 and usize whole-word loops that bypass memcpy's length dispatch. Overlapping copies that must be repeated use `repeat_in_chunks`, which performs log2 memcpys instead of `ml/offset`. Interleaved Huffman table access and output are unchecked, a change that measured about zero because branch prediction already absorbed the bounds check but was kept.

## Residual

After the libzstd-shaped loop, decode reads x0.91 to 1.03 bulk and x0.96 to 1.05 stream on json, text and skewed.zst3/9, and x1.03 to 1.13 on dll100. skewed.zst1 and skewed.zst19 bulk stay at x1.32, where our bulk path runs slower than our own stream path, so that residue sits on the literals side of the block decode. The dll100 branch-miss excess is attributed to the per-block table builds, led by the Huffman X1 fill's per-symbol scatter loop, with the measured decomposition in [dec-gap](../bench/dec-gap.md). The per-cell numbers and the remaining items are in [dec-gap](../bench/dec-gap.md) too.
