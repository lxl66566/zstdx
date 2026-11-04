# Decoding Path

This page answers where decode time goes, how the output and executor paths are structured, and which residual gaps remain. It is organized by decode stage, with the fused sequence loop first and parallel decode last. Disproven directions live in the [Disproven Directions](../negative.md) page, and the per-stage falsification data is in [negative.md](../negative/decoding.md). Open work is in the [Todo List](../todo.md).

## Output path

The decoder has three output models. A hand-written ring buffer serves dictionary frames only. It implements both discard-from-head and copy-internally-to-tail, where `VecDeque` and `Vec` can each do only one efficiently, with a modulo-free wrap and hand-written `extend_from_within` for four layouts.

Dictionary-free slice decoding writes raw, RLE and compressed blocks directly into the caller's target buffer, bypassing the ring and drain copies, with a block-level budget check and a `TargetTooSmall` error variant. Slice decoding gained 13% to 35% across the board.

The flat paths also peek the sequence section's first byte before decoding literals. A zero-sequence block's literals are its whole output, so they decode straight into the target at the write cursor and the staging buffer with its copy disappear. A target too small, a single-stream section or a 4-stream section outside the interleaved fast gate falls back to staging, so every error keeps the position and type the staging path reports.

The one-shot slice entries also split each compressed block's body off the input slice and parse it in place, where the reader-based entries copy the body into a staging buffer first. The copy was 7% of skewed.zst1's decode and scales with the frame's compressed size, which is why the json and text bulk cells moved too.

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

The flat one-shot path can stage a block's literals inside the caller's target instead of the scratch buffer: when the block carries at least 64 KiB of literals and the target holds the write limit plus the staging region with a 16-byte pad and a 16-byte over-read tail, the literals decode writes `out[written + block_out_max + 16 ..]` and the executor copies from there. Every output line is then cache-present before its write, so the copy's stores commit without write-allocate fills; with scratch staging each 64-byte store pays a DRAM fill, which held skewed.zst19's literal loop at 4.0 B/c against libzstd's 14.2 B/c. The staging block runs through an out-of-line twin of the block body, because a second literal source at the staged call site re-rolls the fused loop's register allocation, which every short-run shape pays per sequence. The streaming HEADROOM paths refuse the arrangement: their previous generation physically sits above the write cursor, where the staging region would, and wrapped match reads must not see staging bytes. The decomposition and the engagement numbers are on the [dec-gap](../bench/dec-gap.md) page.

A single compare of the offset against the active generation's start guards the match source. Inside the active generation the source is `dst - offset`. Behind the compare, the window check rejects out-of-window offsets, and the cold `copy_wrapped_match` serves sources in the wrapped-away previous generation. A previous-generation source physically sits at `dst + seg_a_end - offset`, above the write cursor by at least `MAX_BLOCK`, so one wildcopy serves it.

## Huffman literals

The X2 double-symbol table ports libzstd's `HUF_decompress4X2` and adds an 80% pairing threshold: the pairable fraction of the code space is computed from the weights, and below the threshold the space is left empty and X1 is used. Skewed.zst1 gained 43% and skewed.zst3 15%.

Both tables build through rank-grouped fills. The X1 build in `build_table_from_weights` ports libzstd's `HUF_readDTableX1_wksp` fill: a counting sort buckets the symbols by code length, then each rank's spans are written in symbol order with one constant-length fill per symbol, straight into both the `Entry` view and the packed u16 view. The previous per-symbol scatter interleaved span lengths 1 and 2^10 between adjacent symbols, and its loop boundaries held 0.22M branch misses per dll100.zst3 decode, 6.6% of the total; the rank fill reads 0.03M. Gungraun credits every table-rebuilding shape with fewer instructions, json -0.06 to -0.18% and text -0.24 to -0.64%, against +0.03% on skewed's 16-symbol alphabets where the counting sort outweighs the tiny tables. The scatter stays in the source as a `cfg(test)` reference, and an equivalence test builds both ways over generated weight distributions, so the fast fill cannot drift from the scatter's semantics. The X2 build carries the same structure from its own port, and the weights header's FSE table computes its slice width as a shift over the power-of-two slice count.

X1 and X2 stream-state pointerization folds the base into `ip` and `op` and reduces live values from 16 to 14, cutting inner-loop instructions 10%. The X2 interleaved loop dispatches through the BMI2 tier with the sequence executor, so the per-lookup window shift compiles to one shlx without the `%cl` staging move.

The BMI2 tier's X2 hot stretch runs as inline assembly (`x2_inner_asm`), a port of libzstd's amd64 `HUF_decompress4X2_usingDTable_internal_fast_asm_loop` shape: the entry's three fields load separately so the count byte feeds shlx straight off its load, and the stretch carries no spill traffic, with the `ip[]` pointers in xmm slots that only the reloads touch. The Rust body the tier replaced executed 243 instructions per iteration against the asm block's 202, at about 4.5 IPC for both, so the loop was issue-bound and the instruction count was the lever; the asm's three loads per lookup also trade IPC for count, mirroring the reference's memory-port-bound profile. The outer bounds, the corruption checks and the scalar tail stay in Rust, and the scalar tier keeps the pure-Rust body, so `ZSTDX_DEC_SIMD_TIER=scalar` measures the old loop. On 2026-11-04 this moved skewed l1 Ir -14.5 percent and skewed.zst19 bulk from x1.25 to x1.137 against libzstd, with the json and text cells unchanged. The X1 loop keeps its Rust form: it holds 1.42 cycles per symbol at the theoretical limit, and its BMI2 conversion is falsified below.

## Parallel decode

MT decode is our exclusive dimension because libzstd has no MT decode API. On the 2026-11-02 pass after the stage-B port, json.zst3 reads 1.61 times its own single-thread throughput at mt16, dll100.zst3 1.92 times and skewed.zst9 1.06 times, at absolute cells of 3222, 3086 and 1043 MiB/s against single-thread controls of 2003, 1612 and 982. On the 2026-10-18 pass, json.zst3 had reached 1.65 times our single-thread throughput and 1.23 times the zstd stream reference at 16 workers, dll100.zst3 1.90 times single-thread and 1.36 times the reference, and skewed.zst9 1.25 times single-thread against a ±15 percent absolute band across passes.

The scaling comes from decoding independent restart points in parallel. Stage A decodes each segment's sequences through the staging `decode_step` into a vector of wire values, and stage B executes them in order with its own executor, because only stage B knows the repcode history. Stage A runs on a global pool of parked worker threads leased per call (see `mt_pool`, the mirror of the streaming encoder's pool), so a repeat decode pays a slot handoff instead of a worker spawn+join.

Stage B follows the ST loop's libzstd shape (see [dec-gap](../bench/dec-gap.md)). Each staged sequence resolves its offset in locals off the wire value, through the same selector mapping `do_offset_history` pins for its other callers: values above 3 are real offsets minus the repcode bias, and 1 through 3 pick rep0, rep1, rep2 or the rep0-minus-1 pseudo slot from the wire bits and the literal-length-0 flag. The region's write limit and wild cap fold into one end compare, and one combined check against the literals end and that compare sends every edge case to the out-of-line exact path `exec_staged_end`. The hot path copies literals unconditionally in 16-byte chunks and the match through `wildcopy_match`; stage A appends a 16-byte zero tail to the staged literals after the last block, and the block plans' literal ranges index positions recorded before the extension, so the copy's source overread stays initialized.

The port cut stage B's execute time per segment to 43 percent on skewed.zst9, at 18.0 to 7.8 ms per 16 MiB segment, and to 65 percent on json.zst3 and 70 on dll100.zst3. One locked before/after pair on the same day moved json.zst3 mt16 from 2197 to 3222 MiB/s, dll100.zst3 from 2291 to 3086, and skewed.zst9 from 776 to 1043, the last above its single-thread control for the first time.

Where the MT wall sits after the port: stage B's execute is memory-bound on the staging round-trip, reading every staged literal and sequence while writing the whole output, and stage A produces in parallel behind it, so json and dll100 split their walls between the two. Moving the literal half of that round-trip into stage A by placing runs at their final output positions was measured on 2026-11-03 and falsified: the staged literal copy doubles as the wildcopy repair of the previous match's overshoot, so placement pays a per-run repair tax plus a parallel re-copy of the same bytes, with the A/B in [negative](../negative/decoding.md). Keeping the executor's copy and moving only the staging buffer's location, as per-segment in-dst zones with a sound collision contract, was measured on 2026-11-04 and falsified as well: the serial executor pays a code-shape tax, the zone warmth crosses cores where the ST arrangement kept it same-core, json's round-trip is sequence-dominated and skewed cannot fit its zones, with the A/B and the split attribution in [negative](../negative/decoding.md). Skewed.zst9 carries only the two 16 MiB restart points its encoder emitted, so its wall was the first segment's staging latency plus two executes, and stage A's per-worker staging rate held every restart-poor frame near single-thread speed. The restart points belong to the frame and the stage-A body widening is falsified, so the floor stood until the R96 planner started staging inside epochs, the next paragraph. Current cells and their spreads are in the [snapshot](../bench/snapshot.md).

R94 (2026-11-04) validated the sequence-read half of that attribution with a traffic model and a padding probe. The model counts stage B's round-trip as the staged sequences read at 12 B each plus the staged literals read plus the whole output written; per-frame counts came from the `seq_dump` recording decoder, and the sum of ll plus ml equals the output on every shape, so raw and RLE blocks are absent and the staged literals are the ll bytes.

| shape | sequences | staged sequences | staged literals | output | sequence share of stage-B traffic |
|---|---:|---:|---:|---:|---:|
| json.zst3 | 2.12M | 25.4 MB | 1.3 MB | 33.6 MB | 42% |
| json.zst9 | 1.71M | 20.6 MB | 1.1 MB | 33.6 MB | 37% |
| skewed.zst9 | 6.09M | 73.1 MB | 2.0 MB | 33.6 MB | 67% |
| dll100.zst3 | 6.01M | 72.1 MB | 21.2 MB | 104.9 MB | 36% |

The probe padded the staged element to 24 B per sequence, a zeroed field the executor never reads, byte-identical under mtcheck, and ran it against base 91191a7d in two locked interleaved rounds per side:

| cell | base | padded | wall loss |
|---|---:|---:|---:|
| json.zst3 mt4, mt8, mt16 | 2998, 3172, 3190 | 2688, 2848, 2788 | 11.5, 11.4, 14.4% |
| json.zst9 mt4, mt8, mt16 | 2904, 3282, 3365 | 2686, 3001, 3036 | 8.1, 9.4, 10.8% |
| skewed.zst9 mt4, mt8, mt16 | 1043, 1063, 1063 | 968, 968, 970 | 7.7, 9.8, 9.6% |
| dll100.zst3 mt4, mt8, mt16 | 2794, 3039, 3076 | 2594, 2657, 2731 | 7.7, 14.4, 12.7% |

The single-thread controls held within 0.8 percent on json and dll100; skewed's controls read 981 and 884 against 970 to 978, one noisy round in four with its mt cells consistent across rounds. Causality holds on every mt cell. Dividing each wall loss by the sequence share of stage-B traffic gives the bandwidth-bound stage-B share of the mt wall: 0.21 to 0.40 on json and dll100, and 0.12 to 0.15 on restart-poor skewed, whose staging latency owns the wall. The staged Sequence vector is therefore causal, and shrinking the record to 8 B per sequence removes a third of the sequence stream, which prices that lever at about 3 to 4 percent on the same shares. A tail prefetch of the next segment's staging measured flat the same day and is falsified in [negative](../negative/decoding.md).

The packed-record lever that pricing pointed at was measured the same day and closed. An 8 B record cannot hold the staged triple, because the staged offset needs the whole u32 under position-bounded offset validity, and the 9 B record that fits lost 2.4 to 6.2 percent on every mt cell: the per-sequence unpack on the serial executor costs more than the quarter-cut sequence stream buys back, the pack taxes stage A's staging rate, and the flat 12 B aligned triple stays. The reopen form the entry named, a batched SIMD expansion off the per-sequence path, was built the same night in three shapes and also lost 9 to 15 percent on every mt cell at +3.8 percent instructions, a cycle-efficiency loss the traffic model's bandwidth term never pays back. The width derivation and both A/B rounds are in [negative](../negative/decoding.md).

R96 (2026-11-04) lifted that floor by unchaining stage-A starts from restart points. A restart point is where every table re-describes, the sufficient condition for a cold scratch, but staging only needs the tables the boundary carries, so the pre-scan now folds the table epoch per block, recording which block last described the Huffman table and each FSE stream since the last restart point, with the axes independent: a Compressed-literals block re-establishes Huffman while its FSE modes may stay Repeat, Treeless chains across Raw and RLE literals, and zero-sequence blocks touch no FSE stream. The planner keeps its target multiples, `input/(workers*2)` floored at 256 KiB, and snaps a cut to the first restart point within 1.5x of its target, so restart-dense frames keep their segmentation and only restart-poor epochs split at the nearest block boundary; skewed.zst9's plan reads 15 segments with the 16 MiB restart itself snapped as one cut. A lease starting mid-epoch primes its scratch by replaying the recorded descriptions, at most one Huffman build plus one explicit FSE description per stream, header-parse-rate work: the whole scan, fold included, reads 35 to 42 us over 256 blocks, and every mid-epoch lease's staging keeps ahead of the serial consumer, so the priming stays inside the staging slack on all 13 primed segments of the probe walk. Stage B stays segment-serial and the staged representation is untouched, so output stays byte-identical by construction. The locked interleaved A/B against the pre-change build ran eight rounds per side at a 1000 ms per-cell budget; round-sides whose cells an unlocked lane phase collapsed are excluded per cell, and the medians below read off the three to six clean pairs.

| cell (32 MiB unless noted) | pre-change | mid-epoch | delta |
|---|---:|---:|---:|
| skewed.zst9 st | 980 | 982 | +0.2% |
| skewed.zst9 mt4, mt8, mt16 | 1058, 1061, 1062 | 1628, 1725, 1746 | +54, +63, +64% |
| skewed.zst19 st | 2936 | 2936 | +0.0% |
| skewed.zst19 mt4, mt8, mt16 | 2911, 2914, 2914 | 6021, 6374, 6500 | +107, +119, +123% |
| json.zst3 st, mt4, mt8, mt16 | 2011, 2968, 3146, 3198 | 2015, 3011, 3132, 3181 | +0.2, +1.4, -0.4, -0.5% |
| dll100.zst3 (100 MB) st, mt4, mt8, mt16 | 1675, 2861, 3006, 3088 | 1678, 2807, 2959, 3064 | +0.2, -1.9, -1.6, -0.7% |
| text.zst3 st, mt4, mt8, mt16 | 11968, 11694, 11713, 11678 | 11941, 11686, 11646, 11668 | -0.2, -0.1, -0.6, -0.1% |

The probe splits the new skewed.zst9 mt8 wall: 15 segments, the first 2 MiB lease waits 1.9 to 4.5 ms behind its staging plus handoff, every later wait sits near zero except the restart-snapped 6 MiB segment whose 2.4 to 2.8 ms lands after the consumer drained the first five executes, and the serial stage-B executes sum to 13.7 ms warm. The 19.5 ms wall at 1725 MiB/s is therefore 70 percent of the 2450 MiB/s execute-only bound, x2.5 single-thread, so the ceiling is now the serial execute plus the first-batch staging tail, the floor this page predicted. skewed.zst19 executes at memcpy rate off its staged records, a 2.3 ms sum against its entropy-bound 2936 MiB/s single-thread control, so its mt cells read x2.05 to 2.21 with the remaining wall in the 0.9 ms first lease and about 2 ms of per-call serial work the probe does not split, checksum absorb and staged-buffer teardown. The restart-dense controls stay flat, so the finer cut costs nothing where restarts already price the grid; dll100's mt4 and mt8 medians read -1.9 and -1.6 percent with their cleanest pairs inside half a percent, inside the run-to-run band. mt16 does not beat mt8 because the 256 KiB input floor prices the grid at 15 segments either way and the residue is serial.

The worker pool and what it removed: a scope-based decode spawned and joined every stage-A worker per call, which costs 57, 93 and 192 us at four, eight and sixteen threads on this machine, against 8, 16 and 42 us for a slot handoff. The pool keeps each worker's entropy scratch alive as well, so a call stops re-allocating the FSE and Huffman tables and the zeroed `seq_packed` vector per worker. The spawn share alone sits near 0.5 to 2 percent of the call walls, but the measured interleaved A/B moved more than that because the retained scratch carries the same steady-thread warmth the encoder's pooled states carry; the single-thread controls stayed flat in the same runs.

| File (32 MiB unless noted) | mt4 | mt8 | mt16 |
|---|---|---|---|
| json.zst3 | 2571 to 2969 | 3093 to 3236 | 3221 to 3245 |
| json.zst9 | 2405 to 2969 | 2771 to 3374 | 3128 to 3452 |
| skewed.zst9 | 1031 to 1066 | 1007 to 1066 | 1040 to 1066 |
| dll100.zst3 (100 MB) | 2790 to 2928 | 3056 to 3067 | 3123 to 3139 |
| json cut to 20 MiB, level 3 | 2600 to 2862 | 2995 to 3122 | 2924 to 2956 |

The lease mechanics: each call builds one `StageAJob` holding frozen input and plan pointers plus the slot array, leases `min(workers, segments)` slots from the pool, and a drop guard aborts and waits every lease out on all exit paths, so the frozen borrows always outlive the workers a scope used to guarantee. A worker panic publishes into the job's poison cell, the consumer runs to completion, and the payload resumes on the caller after the guard's wait, exactly the scope's behavior; the panicked thread retires instead of reparking. The backpressure bound, the ordered consumer and the staging pool are unchanged. Parked threads wake on a five-second idle timeout and exit, and the pool is depth-capped at 32, so a one-off decode leaves no threads behind.

## Miscellaneous

Frames without a content checksum skip xxh64 init and update, gaining 2.7%, and the shared in-tree eight-chain implementation lives on the [encoding page](encoding.md). Consecutive blocks in predefined mode do not rebuild FSE tables, which eliminates per-block allocation and gains 2.5%. The `StreamingDecoder::read` path decodes until the caller's buffer is full, which removed the one-block granularity and gained 2%.

Fixed-width copies with allowed overshoot go through `copy_bytes_overshooting`, using u128 and usize whole-word loops that bypass memcpy's length dispatch. Overlapping copies that must be repeated use `repeat_in_chunks`, which performs log2 memcpys instead of `ml/offset`. Interleaved Huffman table access and output are unchecked, a change that measured about zero because branch prediction already absorbed the bounds check but was kept.

## Residual

After the libzstd-shaped loop, the 2026-11-02 literals round, the 2026-11-03 table-build fills, the 2026-11-04 X2 asm stretch and the 2026-11-04 in-dst literals staging, decode reads x0.91 to x1.05 bulk on json, text and skewed, with skewed.zst19 bulk at x1.045 and skewed's other bulk cells ahead of libzstd. Stream cells read x0.93 to x1.04, with skewed.zst19 stream at x0.98, and dll100 reads x1.02 to x1.05 on both paths. The dll100 branch-miss excess sits at 1.14x the gcc build's, with the per-block table builds' Huffman half closed; the measured decomposition is in [dec-gap](../bench/dec-gap.md). The per-cell numbers and the remaining items are in [dec-gap](../bench/dec-gap.md) too.
