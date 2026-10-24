# Decoding pitfalls

This page collects the decoding bugs and measurement traps that cost real time, so the next change can avoid the same shape. Each lesson keeps its counterexample, and the sections group the lessons by topic: block and buffer safety, streaming and multi-frame semantics, MT decoding, corpus and test coverage, and profiling.

## Block and buffer safety

### Reserve against a bounded claim

The ring path sized its reserve as literals plus the sum of match lengths with no output bound in front of it. A 129 KB Compressed block that used all-RLE sequence modes with ML code 52 could claim about 3 GB of match output and force a multi-gigabyte ring reallocation. That panicked on allocation failure in the ulimit reproducer and wasted the reserve otherwise.

Libzstd bounds blocks at `blockSizeMax = min(window, 128K)` before executing. The claim is now checked against `common::max_block_output` ahead of the reserve, and an over-large claim reports `ExecuteSequencesError::BlockOutputTooLarge`. The check costs one compare per block on a sum the reserve needed anyway.

### One output cap feeds every site

Past the ring fix, the other paths still accepted out-of-spec blocks. The flat executor's budget check bounded only the caller's target, Raw and RLE blocks were bounded only by the 128K header cap, and literals sections and MT stage A had no bound at all. A small-window frame could therefore silently produce blocks above `min(window, 128K)`, where libzstd reports corruption with "Decompressed Block Size Exceeds Maximum", and an MT-staged claim sized the output buffer before any bound.

`common::max_block_output` now feeds the literals-section size in `parse_sections` before `decode_literals` sizes from it, since RLE literals can claim 1 MiB from one payload byte. It also feeds the Raw and RLE headers in `decode_block_content` and the frame-decoder loops, the flat executor's target clamped to `written + cap`, and MT stage A per block.

Clamping the flat target turns `TargetTooSmall` into the cap's signal, so a cap hit with target room left is reclassified to `BlockOutputTooLarge`. Otherwise the `decode_all_to_vec` retry loop doubles forever chasing an unbounded claim. The HEADROOM instantiation stays sound under the clamp because the wildcopy overshoot of at most 16 B past a sequence end still lands inside `ensure_block_space`'s `window + 2*MAX_BLOCK + slack` allocation. Legal frames never trip any of this, because encoders cap blocks by the declared window per RFC 8878 Block_Maximum_Size and the window formulas match libzstd bit for bit.

### Bound-check arithmetic

A bound check is only as trustworthy as the arithmetic computing its input. The sum of match lengths feeding the cap checks folds with `saturating_add`. On 32-bit targets a plain sum of untrusted match lengths has an upper bound near 1.3e10, which exceeds `u32::MAX` and can wrap to a small value that bypasses the very cap it is checked against.

### Bound the stored side

Libzstd rejects every block type whose stored size exceeds `blockSizeMax` at the header stage, and RLE's `cBlockSize` is its single payload byte. A 1 KiB-window frame carrying a 2 KiB compressed body is corruption there, while zstdx accepted it because the output cap never fired on a body that small.

The check lives in `parse_sections` before the body is read, and every sequential Compressed path funnels through it. The MT scan compares the same `max_block_output(window)` so such frames fall back to the sequential error instead of planning through.

Own frames stay safe because the encoder shrinks blocks to `min(window, 128K)` and keeps a Compressed block only when its stored size is below the input block size. The splitter runs only at window 2^17 and above, where the cap equals the absolute maximum, and its per-partition `csize >= src_bytes` guard restarts unsplit.

### Restart points have two entropy axes

A segment cut at a block with Raw or RLE literals passed the old "not Treeless and no Repeat" check, yet it reset the Huffman table in stage A. A later Treeless-literals block in that segment then failed with `UninitializedHuffmanTable`. Legal frames were rejected on every worker count while sequential decode passed, and libzstd level 3 and above emits Treeless blocks behind incompressible-literal blocks.

Restart candidacy has two independent axes: literals, which rebuild, reuse or ignore the table, and FSE, which is explicit or Repeat. A Raw or RLE-literals cut is legal only while no Treeless block precedes the next rebuild. The frame-start segment needs no check, because a legal frame's first Treeless always follows its first rebuild and illegal frames fail identically in both paths. Weakening the check to "Compressed literals only" kills piece-router engagement on our own encoder's frames whose job boundaries carry Raw literals, so the two-axis check is the fix.

### Clear X2 when X1 is selected

When a Compressed block selected X1, the x2 table was not cleared. A later Treeless block could pick up the stale table and go through X2, ending in `BitstreamReadMismatch`. The fix is `x2.clear()` at the top of `build_table_from_weights`.

X2 selection also needs an 80% pairwise threshold, because text's tl=11 table is only 55% to 61% pairable, and lacking the threshold once cost 2.7%.

### Four flat-streaming traps

First, returning after a grow without re-checking space led to an out-of-bounds panic or segfault. Second, the doubling anchor was mistakenly written to slide with `copied`, so an overlap match read unwritten bytes. The anchor must be fixed, and the chunk must stay within readable plus copied bytes.

Third, the `out = buf[end..]` slice pointer was already offset while the match source still used absolute offsets, so the physical base had to be restored. The slice path's `block_start=0` was correct by coincidence, which masked the problem. Fourth, a wrap must use `origin += end`. Writing `origin = end` regresses virtual coordinates. Each of the last two was fixed once across the two FlatView generations.

### Three wildcopy traps

First, the first 4 bytes of `overlap_copy8` must be copied byte by byte, because at offsets below 4 stores feed loads and whole-u32 reads corrupt the file. Second, the wildcopy helper computed distance from two coordinate systems, with an output-relative `dst` and a physical `src`, so `d - s` mixed spaces. It now uses all pointers. The offset-16-and-above branch does not depend on the distance, so most files passed by luck, which is what made this hard to find.

Third, the tests went through `decode_blocks(All)`, where `state.flat` wraps, while `decode_all_to_vec` does not wrap. Reproduction must use the test's real path.

### overlap_copy8 underflow

The adjustment value in `s2 + (8 - dec64)` is negative at spread offsets 5 to 7, and the usize underflows. Debug builds panicked and broke 12 corpus tests, while release wrapping happened to be correct.

The fix uses a signed table plus `offset()`, which aligns with libzstd's `*ip -= dec64table[offset]`.

### Flat wrap safety

A review flagged the wrap condition `end >= window + MAX_BLOCK_SIZE` as insufficient against the 16-byte wildcopy overshoot clobbering the oldest in-window history. The reported scenario, wrapping with `end` only `window + bs` to `window + bs + 16`, is unreachable.

The wrap branch runs only after the cursor gate `end + bs + 16 > buf.len()` fails, and `buf.len() >= window + 2*bs + WRAP_SLACK`, so every real wrap has `end >= window + bs + 49`. That 49-byte margin was a coincidence of two other constants, the gate slack and `WRAP_SLACK >= 32`, so shrinking either silently re-exposes the hazard.

The wrap condition now states its own requirement as `end >= window + bs + WILDCOPY_SLACK`, with a test pinning the wrap point. When a safety property is implied by a coupling of unrelated constants, encode it at the site that needs it.

### HEADROOM keeps the budget check

The headroom instantiation skipped the per-sequence `op + ll + ml <= out_end` budget check along with the wildcopy constant-ization, while `ensure_block_space` guarantees only a 128 KiB plus slack mapping. A corrupted sequence segment whose total output exceeds the block maximum then wrote past the allocation and hit SIGSEGV. Nightly's `copy_nonoverlapping` precondition check flagged it as overlap first, and the artifacts test crashed the test process.

The fix keeps the budget check unconditional, aligning with libzstd's `op + ll + ml > oend` rejection, and headroom decides only the wildcopy strategy. The dec-st matrix re-run showed no regression. A safety check and an optimization switch must not share one boolean, and the optimization should remove the check's cost while keeping the check.

### Zero the literals overshoot

The flat executor's 16-byte literal chunks read up to 15 bytes past the final literal. `literals_buffer.reserve(16)` kept the read inside the allocation, but the bytes were uninitialized capacity, and reading them is UB under the strict Rust model even though the values only land in the output's overshoot and are overwritten before any read. Libzstd's `ZSTD_wildcopy` over-reads the same shape in C and tolerates it there.

The fix zeroes the 16-byte overshoot window per block through `resize(len+16, 0)` and `truncate`. The buffer is pooled and reserved, so it never reallocates, and the cost is a 16-byte memset per block.

Centralizing the creation and consumption surface in `DecoderScratch.literals_buffer` makes the fix cheaper than documenting the tolerance. That buffer is cleared per block, only ever pushed initialized bytes, and is over-read on exactly one path.

### Four-stream quarter split

The slow path decoded each stream to bit exhaustion and checked only the symbol total, so a stream that encoded past its quarter, with another stream short by the same amount, was accepted with an output layout that no fast path or libzstd would produce.

The slow path now enforces the same segment contract as `finish_streams`: each stream stops at its segment end and must hit both the exact symbol count and exact bit exhaustion. The segment math also forced adopting libzstd's `MIN_LITERALS_FOR_4_STREAMS`, so 4-stream sections below 6 regenerated bytes are rejected up front because their quarter split underflows stream 4, which libzstd rejects at the literals header.

When crafting hand-made Huffman streams for tests, direct weights are 4 bits each with the last weight implicit and the sum must leave a power-of-two leftover. A reversed bit stream byte is laid out as padding zeros, then the end marker 1, then the symbol bits, read MSB-first.

### Dictionary registration semantics

`FrameDecoder::add_dict` and `add_shared_dict` register id-keyed, so the frame's dictID must match. The options-based entries use `add_options_dict`, which also registers under the null id, because libzstd applies a loaded dictionary to every frame regardless of the header's dictID field.

The FFI streaming decoder kept the id-keyed form, so a `ZSTD_c_dictIDFlag=0` frame decoded with the formatted dictionary loaded hit "Data corruption detected" from sequences referencing history the decoder never installed. The one-shot FFI paths that route through `DecoderOptions` worked. Any new decode surface that takes a dictionary follows the options rule unless its contract is id-keyed by name.

### Dictionary repcode load check

`Dictionary::decode_dict` stored the three repeat offsets without libzstd's `ZSTD_loadDEntropy` check of `rep == 0 || rep > dictContentSize`, with the content size excluding the 12 rep bytes. A corrupt dictionary then failed later or mid-frame instead of at load.

The fix reports `DictionaryDecodeError::InvalidRepeatOffset`. The real `--train` fixtures all satisfy the check, and only the fabricated unit-test dictionary with rep 0x00abcdef over 25 content bytes was affected, which had been corrupt all along.

### Reset restores the flat default

`FrameDecoderState::reset` left `flat_active` cleared. After a dictionary frame cleared it through the dictionary branches or `force_dict`, every later dictionary-free frame on the same decoder stayed on the ring path for no reason.

`reset` now restores `flat_active = true`, and `FrameDecoder::reset` re-disables it right after when the next frame carries a dictionary, so the ordering keeps both paths correct.

### Retired bits on no-reload paths

Refactoring the bitstream reader so the read's shift count absorbs the consumed offset split "read n bits" from "advance the accounting". The paths that skip the reload, which are the block's last sequence in both the plain and wide forms, then forgot the advance, and every frame failed `finish` with `ExtraBits{n}`.

The old `consumed += n` lived inside `read`, so no-reload paths were accounted automatically. Any reader shape that moves the accounting out of the read must repeat it on every path. The corpus roundtrip tests caught this, not review.

## Streaming and multi-frame semantics

### Streaming window semantics use a generation model

Libzstd streaming never compares an offset against the declared window. The per-sequence rule is `offset > oLitEnd - virtualStart`, which tests the generation, and the ring geometry is tuned so the two almost always coincide. The cap is the window floored at 1 KiB plus 2*blockMax plus 2*WILDCOPY, capped by the FCS, and the ring wraps when `outStart + blockMax > cap`.

The two rules differ at the edges in both directions. `fa_w16_o100000`, with an offset past the 65536 window but inside the 180009-byte generation, accepts under streaming, while an in-window offset flips to reject once the origin wraps and the previous generation is dropped. A single-origin simplification of the model wrongly rejects `ctrl_w10_o900`, whose offset 900 is below the 1024 window.

Never collapse the generation bookkeeping into a window-distance check, and pin decode tests per path. One-shot decode is whole-history by design, and C's `ZSTD_decompress` accepts any in-history offset, while streaming is generational, so the two disagree by design on the same frame.

A raw dictionary is a virtual generation-0 prefix that expires at the first wrap, with reach equal to dictSize plus the bytes produced until then. The semantics map and probe table are in [winsem](../review/winsem.md).

### Folded bases and virtual addresses

Passing the generation floor as a live value rolled the fused executor's register allocation by 2% Ir, so the floor was folded into the pre-computed `vbase_op`. One helper still reconstructed `virt_base` as `vbase_op + out_base`, which after the folding equals `virt_base - prev_origin`. From the second wrap on, every previous-generation address was shifted, producing wrong bytes, a bogus `OffsetTooBig`, or a debug underflow, with four different test failures from one root cause.

When a base carries a folded term, every consumer that rebuilds an address from it must un-fold through the same view. Structural values such as the 5-word `FlatView` never enter the fused loop by value, and per-block scalars are derived in the prologue instead.

### Multi-frame agreement

After the first frame's `is_finished`, `StreamingDecoder::read` returned `Ok(0)` while `decode_all` looped over all frames, so the same multi-frame input gave different results on the two paths. A new cross-check assertion in the fuzz decode target caught it, because the old target only called `read_to_end`.

The semantics follow libzstd and the zstd crate: both paths decode all frames, skip skippable frames transparently, error on any trailing garbage of 1 byte or more including a 1 to 3-byte partial magic, and never treat an empty content frame as EOF.

The fix pushes the frame-tail continuation primitive, which pre-reads 4 magic bytes to distinguish clean EOF from a partial tail, into `decoding::frame_source`, shared by `stream::read::Decoder` and `StreamingDecoder`. The continuation check must live inside the read fill loop, because when an empty frame ends the current read, `can_collect == 0` falsely reports EOF.

That crash's second frame had FHD=0x38 setting a reserved bit, and the header-strictness divergence it exposed is closed under Divergences.

### Next-frame hunt keeps peeked bytes

`start_frame` built a fresh `PrefixedReader` per attempt, so any `Err` after the magic was consumed, most naturally `WouldBlock` on a non-blocking socket, dropped the staged bytes with the local. The retry then read the payload as a magic number, the stream was permanently misaligned with `BadMagicNumber`, and a transient error became permanent corruption.

The hunt state now rides on the `FrameDecoder` through `NextFrameStaging`, so staged header bytes replay on retry. A failed skippable-frame skip keeps the remaining-content count and resumes the discard, and skippable content is never staged because it is unbounded. The reader pulls at most the magic into the stage, because anything over-read past the header would be lost when the hunt ends, since block decoding reads from the raw source.

Follow-up: staging is stream-scoped state on a reusable decoder, so a decoder abandoned mid-hunt and rebound to a new source replayed the stale magic into the new stream's head and was misparsed as "Reserved block occured". `init_first_frame` and `FrameDecoder::reset` now drop the staging, and the hunt itself is unaffected because `start_frame` lifts the staging out for each attempt before any reset runs.

## MT decoding

### Piece grids must be fully cut

The incremental piece-cutting loop had no `n_pieces` bound, so after the last piece, where `end` is clamped to the FCS, `end` stayed at the FCS forever. The loop spun pushing empty pieces while holding the driver mutex, and every thread then contended at 0% CPU.

`try_settle` also treated "validated equals currently-cut pieces" as a complete round, so a grid settled mid-staging with its uncut tail's `max_top` all zero. The watermark gate opened, and bytes came out wrong with a checksum mismatch. The fix bounds the loop and requires a grid to be fully cut before it can win.

- Verdict states other than `winner`, such as all-dead and poison or abort, never notified the caller condvar. A plain frame, where all grids fail the lag gate, deadlocked with every thread parked. The fix notifies for each state that satisfies the caller's predicate.
- A `MutexGuard` in a let-else scrutinee lives through the else branch, so re-locking the same mutex there self-deadlocks. Bind the copied value to a plain local first.

### Track the doubled value

`decode_to_vec_mt`'s parallel path grows segment by segment, while the serial fallback reports `TargetTooSmall` on an empty Vec. The doubling retry must track the doubled value, because `reserve(cap)` is a no-op when the spare capacity is at least `cap` and the loop otherwise runs forever.

During debugging this was once misdiagnosed as the flat decoder looping, when the retry loop was re-running the full decode every round.

### MT literals count the delta

ST clears per block, so nothing happened there, but MT stage A accumulates per segment, and every second or later Huffman literals block within a segment was falsely reported. The final validation used the buffer's total length instead of the delta.

Seven of 11 corpus files failed MT decode while the repo tests stayed green, because the textish corpus passed by luck with Raw and RLE literals. The fix uses the delta. The shape of the repo test corpora can systematically mask a class of bugs, and a semi-structured regression test now covers it.

- A symbol with power-of-two probability can yield `nb_d = acc+1` while having no double state and never being used. The assert should read `num_double == 0 || nb_d <= acc`, and the first version was reverted and redone because of this.

## Corpus and test coverage

- Use the test's real decode path. The flat streaming tests went through `decode_blocks(All)`, where `state.flat` wraps, while `decode_all_to_vec` does not wrap. A reproduction that takes the wrong path can pass while the real caller fails.
- A mixed corpus can hide a whole bug class. Seven of 11 corpus files failed MT decode while the repo tests passed, because the test corpus happened to carry Raw and RLE literals on the failing paths and a textish shape passed by luck. The fix added a semi-structured regression test.
- A swapped assertion breaks debug tests. `debug_assert_eq!(written, before)` had its arguments swapped and broke three debug tests.
- Corruption smokes must accept legitimate decode errors. `corruption_smoke` panicked when corruption hit the frame magic, where `BadMagicNumber` is a legitimate error. The example should return through let-else instead of unwrap.
- Strip any zst suffix when locating a reference file. `bench_files`'s `strip_suffix` recognized only `.zst`, so with `.zstN` it used the compressed file as the reference and reported a checksum mismatch. The `zstdx-bench files` command now strips any `zst*` suffix and tries the bare stem and `.raw`.
- The fuzz target needed a cross-check, not only `read_to_end`. The old decode target called `read_to_end` and never compared streaming against flat decode, which is why the single-frame versus all-frames divergence survived. A cross-check assertion now compares the two paths on the same input.

## Profiling and attribution

### Rebuilds replace the profiled binary

Recording `prof dec` on the plain release build and then running a `--features seq_dump` tool rebuilds `target/release/zstdx-bench` in place, so every recorded address goes stale. `perf annotate` then attributes the fused loop's samples to `bulk::compress_with`, a neighboring symbol's range, without any warning.

Record and annotate without an intervening cargo invocation, or re-record after any build.

### Cycle counts drift across sessions

The same binary and input measured 713M against 679M cycles per 40 runs of 32 MiB for text.zst3 across sessions, about ±5%. Cycles equal time times frequency per run, but cross-run frequency and interference move the work's cycle count through cache and power state.

For cycle-level A/B on text-class cells, only back-to-back runs inside one script, or the interleaved xslow ratio, are valid. That is exactly what caught the re-roll tax below.

### Machine-code placement in the fused symbol

A strict cut of 8.7 Ir per sequence from the real-offset rotation fold cost text.zst3, text.zst9 and text.zst19 2.8%, 4.2% and 5.0% reproducibly while winning json and skewed. On text the per-sequence budget is about 1% of the decode, so the source is displaced non-loop code such as the xxh64-absorb and copy-tail layout.

Gate every fused-loop change on a text-stream cell, not only on the shape it targets, and expect the tax to dwarf cuts below 10 Ir per sequence.

### Removed uops become lost IPC

Swapping the packed FSE entry for libzstd's per-field layout removed 15 Ir per sequence, which was 8.6% of loop Ir and the largest cut measured in this loop, and bought 0.1% cycles on json. IPC fell from 3.61 to 3.35 while instructions fell 7.4%, uniformly across shapes, with branch-miss and L1d profiles unchanged.

The loop is issue and latency saturated, so ALU uops that look removable are already hidden behind the serial state to load to read chain. Replacing 3 wide loads with 12 narrow ones consumes issue and AGU bandwidth and adds a second loop-carried table pointer. Instruction-count attribution alone over-values a lever that only shaves parallel uops.

A candidate must shorten the serial chain or the issue structure, and solo cycles are the first gate, ahead of Ir.

### AMD annotate percentages

A single instruction, `add $0x10,%r8` as the copy loop's cursor bump, showed 25.4% of the whole profile inside a symbol holding 23.2%, and the reference's xxh64 region showed line shares summing above its own symbol total. Annotate locates hot regions and does not measure them. Quantify with callgrind Ir plus `perf stat` totals and per-symbol `perf report` shares only.

### Checksum-less pair differential

`zstd -3` and `zstd -3 --no-check` produce the same parse on the same raw, because only the trailer differs. The difference in cycles between the checked and unchecked runs on one side is that side's pure xxh64 absorb.

On text.zst3 it measured ours at 346.0 M against zstd's 345.5 M per 40 runs of 32 MiB, exact parity, while the profile annotation had suggested a 25% difference. Without the split, the checked-wall ratio of x1.071 hides the real residual of x1.15 unchecked.

### Replay harness biases

Overshoot kernels cannot be value-verified on a bare trace, because the executor's correctness invariant is that every read stays behind the write cursor only when the next sequence's literals or match rewrite the overshoot region in address order. A bare match list has no such writes, so a correct kernel mismatches and a mismatching kernel can look fine. Verify exact-tail kernels only.

Interleaving several kernels inside one process also carries order bias, because an NT-store kernel evicts the working set and the first kernel of the next round reads 31.7 ps/B where isolated runs read 23.0. Run one kernel per process and interleave at the process level.

- AMD `ex_ret_brn_misp` samples land in the middle of basic blocks, within about 10 instructions on either side, and cannot be attributed, and `ibs_op` has no miss filtering. Attribution relies on stub bisection plus `perf stat` totals, and annotate or script miss attribution is untrustworthy.

### Stub bisection checks

After stubbing, verify the exit code and output length, because a run once took the half-run leftover values of an out-of-bounds-read segfault as numbers and cycles falsely dropped 13x. Stubs must preserve real offsets, because a `%4096` small offset changes the whole memory-access pattern and makes the wrapped cases vanish entirely, which invalidates the mix.

- Empty-copy execution owns most of the misses. Stubbing the copy loops out one by one decomposes the miss sources, and the decode side holds only 10% while the executor with its copy loops and gating accounts for 88%. That overturns the old assumption that decode-side branches are the bulk.
- Wall clock is untrustworthy for 10 GB/s-class loads. text.zst3 and text.zst9 vary by 0.82x to 1.25x within a run. Look at cycles, or trust only interleaved medians of loads below 9 GB/s.

### The WRAP instantiation

Streaming wraps after 2 MB and carries the full set of view values, about 12 stack round-trips per sequence. Even after cold outlining there can be residual spills, with about 20 live values against 15 general-purpose registers. Before annotating, confirm which instantiation is hot.

### Known hotspots can be limits

After perf showed xxh64 absorb at 8.15%, investigation ruled it out, because the 8-lane version at about 20 GB/s is already faster than zstd's scalar 4-lane and is not the gap source. Compare against known limits before taking a hotspot as work to do.

## Divergences from libzstd

### Header reserved bits are strict

The FHD reserved bit at bit 3 is rejected at header parse with `FrameDescriptorError::ReservedBitSet`, matching libzstd's "unsupported frame parameter" and its `fhdByte & 0x08` check. The spec's Unused_bit at bit 4 stays accepted, because a compliant decoder shall not interpret it and libzstd ignores it too. The old "bits 4-3" framing was imprecise.

The interop gate covers 1300 reference frames from the fuzz interop, decode and decode_dict payloads crossed with the zstd CLI at -1, -3, -9 and -19, all of which still decode, and the raw decode-corpus pass set is unchanged at 20 files before and after. The two b5593b58 replay tests flipped to reference-parity errors.

### Frame content size is a contract

zstdx once decoded frames without comparing produced output against the header's FCS, so a frame declaring 57 bytes whose blocks produced 5 decoded successfully on every path. Inside a larger stream it gave the misleading downstream error "Reserved block occured", while libzstd reports a size error at the frame tail.

Every frame-finish point now verifies its output count against the declared size and reports `FrameDecoderError::ContentSizeMismatch`, and frames without an FCS stay unchecked. The ring path's count came from `DecodeBuffer`'s `total_output_counter`, which missed raw blocks, RLE blocks and full-dictionary repeats. A per-path counter is only trustworthy once a consumer forces every writer to feed it.

The MT executor never sees a frame tail, so `scan` records every frame's pledge in `ScanPlan::frame_pledges`, and both executors verify each frame's executed span after execution. The piece winner path is exact by construction, because its pieces tile the pledge and a mismatched pledge kills every grid before the fallback. The pledge check runs before the checksum comparison to preserve the sequential paths' error precedence.

### Streaming generation shortcut

With a known FCS, the whole frame already in the input and enough output room, libzstd's streaming call becomes a one-shot pass through `zdss_loadHeader` and `ZSTD_decompress_usingDDict`, and it accepts out-of-window offsets. `pledged_w10_f60015_o50000` accepts under a 64 KiB-out streaming call and rejects under a 4 KiB-out call.

zstdx streaming always applies the generation model and rejects. The divergence covers invalid frames only and is pinned in `decode_window_semantics::window_semantics_pledged`.
