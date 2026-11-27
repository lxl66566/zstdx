# Block and buffer safety
## Reserve against a bounded claim


The ring path sized its reserve as literals plus the sum of match lengths with no output bound in front of it. A 129 KB Compressed block that used all-RLE sequence modes with ML code 52 could claim about 3 GB of match output and force a multi-gigabyte ring reallocation. That panicked on allocation failure in the ulimit reproducer and wasted the reserve otherwise.

Libzstd bounds blocks at `blockSizeMax = min(window, 128K)` before executing. The claim is now checked against `common::max_block_output` ahead of the reserve, and an over-large claim reports `ExecuteSequencesError::BlockOutputTooLarge`. The check costs one compare per block on a sum the reserve needed anyway.
## One output cap feeds every site


Past the ring fix, the other paths still accepted out-of-spec blocks. The flat executor's budget check bounded only the caller's target, Raw and RLE blocks were bounded only by the 128K header cap, and literals sections and MT stage A had no bound at all. A small-window frame could therefore silently produce blocks above `min(window, 128K)`, where libzstd reports corruption with "Decompressed Block Size Exceeds Maximum", and an MT-staged claim sized the output buffer before any bound.

`common::max_block_output` now feeds the literals-section size in `parse_sections` before `decode_literals` sizes from it, since RLE literals can claim 1 MiB from one payload byte. It also feeds the Raw and RLE headers in `decode_block_content` and the frame-decoder loops, the flat executor's target clamped to `written + cap`, and MT stage A per block.

Clamping the flat target turns `TargetTooSmall` into the cap's signal, so a cap hit with target room left is reclassified to `BlockOutputTooLarge`. Otherwise the `decode_all_to_vec` retry loop doubles forever chasing an unbounded claim. The HEADROOM instantiation stays sound under the clamp because the wildcopy overshoot of at most 16 B past a sequence end still lands inside `ensure_block_space`'s `window + 2*MAX_BLOCK + slack` allocation. Legal frames never trip any of this, because encoders cap blocks by the declared window per RFC 8878 Block_Maximum_Size and the window formulas match libzstd bit for bit.
## Bound-check arithmetic


A bound check is only as trustworthy as the arithmetic computing its input. The sum of match lengths feeding the cap checks folds with `saturating_add`. On 32-bit targets a plain sum of untrusted match lengths has an upper bound near 1.3e10, which exceeds `u32::MAX` and can wrap to a small value that bypasses the very cap it is checked against.
## Bound the stored side


Libzstd rejects every block type whose stored size exceeds `blockSizeMax` at the header stage, and RLE's `cBlockSize` is its single payload byte. A 1 KiB-window frame carrying a 2 KiB compressed body is corruption there, while zstdx accepted it because the output cap never fired on a body that small.

The check lives in `parse_sections` before the body is read, and every sequential Compressed path funnels through it. The MT scan compares the same `max_block_output(window)` so such frames fall back to the sequential error instead of planning through.

Own frames stay safe because the encoder shrinks blocks to `min(window, 128K)` and keeps a Compressed block only when its stored size is below the input block size. The splitter runs only at window 2^17 and above, where the cap equals the absolute maximum, and its per-partition `csize >= src_bytes` guard restarts unsplit.
## Restart points have two entropy axes


A segment cut at a block with Raw or RLE literals passed the old "not Treeless and no Repeat" check, yet it reset the Huffman table in stage A. A later Treeless-literals block in that segment then failed with `UninitializedHuffmanTable`. Legal frames were rejected on every worker count while sequential decode passed, and libzstd level 3 and above emits Treeless blocks behind incompressible-literal blocks.

Restart candidacy has two independent axes: literals, which rebuild, reuse or ignore the table, and FSE, which is explicit or Repeat. A Raw or RLE-literals cut is legal only while no Treeless block precedes the next rebuild. The frame-start segment needs no check, because a legal frame's first Treeless always follows its first rebuild and illegal frames fail identically in both paths. Weakening the check to "Compressed literals only" kills piece-router engagement on our own encoder's frames whose job boundaries carry Raw literals, so the two-axis check is the fix.
## Clear X2 when X1 is selected


When a Compressed block selected X1, the x2 table was not cleared. A later Treeless block could pick up the stale table and go through X2, ending in `BitstreamReadMismatch`. The fix is `x2.clear()` at the top of `build_table_from_weights`.

X2 selection also needs an 80% pairwise threshold, because text's tl=11 table is only 55% to 61% pairable, and lacking the threshold once cost 2.7%.
## Four flat-streaming traps


First, returning after a grow without re-checking space led to an out-of-bounds panic or segfault. Second, the doubling anchor was mistakenly written to slide with `copied`, so an overlap match read unwritten bytes. The anchor must be fixed, and the chunk must stay within readable plus copied bytes.

Third, the `out = buf[end..]` slice pointer was already offset while the match source still used absolute offsets, so the physical base had to be restored. The slice path's `block_start=0` was correct by coincidence, which masked the problem. Fourth, a wrap must use `origin += end`. Writing `origin = end` regresses virtual coordinates. Each of the last two was fixed once across the two FlatView generations.
## Three wildcopy traps


First, the first 4 bytes of `overlap_copy8` must be copied byte by byte, because at offsets below 4 stores feed loads and whole-u32 reads corrupt the file. Second, the wildcopy helper computed distance from two coordinate systems, with an output-relative `dst` and a physical `src`, so `d - s` mixed spaces. It now uses all pointers. The offset-16-and-above branch does not depend on the distance, so most files passed by luck, which is what made this hard to find.

Third, the tests went through `decode_blocks(All)`, where `state.flat` wraps, while `decode_all_to_vec` does not wrap. Reproduction must use the test's real path.
## overlap_copy8 underflow


The adjustment value in `s2 + (8 - dec64)` is negative at spread offsets 5 to 7, and the usize underflows. Debug builds panicked and broke 12 corpus tests, while release wrapping happened to be correct.

The fix uses a signed table plus `offset()`, which aligns with libzstd's `*ip -= dec64table[offset]`.
## Flat wrap safety


A review flagged the wrap condition `end >= window + MAX_BLOCK_SIZE` as insufficient against the 16-byte wildcopy overshoot clobbering the oldest in-window history. The reported scenario, wrapping with `end` only `window + bs` to `window + bs + 16`, is unreachable.

The wrap branch runs only after the cursor gate `end + bs + 16 > buf.len()` fails, and `buf.len() >= window + 2*bs + WRAP_SLACK`, so every real wrap has `end >= window + bs + 49`. That 49-byte margin was a coincidence of two other constants, the gate slack and `WRAP_SLACK >= 32`, so shrinking either silently re-exposes the hazard.

The wrap condition now states its own requirement as `end >= window + bs + WILDCOPY_SLACK`, with a test pinning the wrap point. When a safety property is implied by a coupling of unrelated constants, encode it at the site that needs it.
## Ring wrap copies cannot overlap


A review suspected an overlap-cut asymmetry between the read-side and write-side split branches of `RingBuffer::extend_from_within_unchecked`. The suspicion is unfounded: with `start + len <= len()` the source stays inside the wrapped readable set, the destination fills the free region, and neither destination chunk can re-enter the source's physical span, so every split copy is non-overlapping. LZ-style overlap never reaches this core either, because `repeat_in_chunks` anchors its doubling chunks so each source ends exactly where its destination begins. `ringbuffer::tests::extend_from_within_matches_model_across_wrap_edges` sweeps every 16-byte-ring geometry against a flat `Vec` model to keep it that way.
## HEADROOM keeps the budget check


The headroom instantiation skipped the per-sequence `op + ll + ml <= out_end` budget check along with the wildcopy constant-ization, while `ensure_block_space` guarantees only a 128 KiB plus slack mapping. A corrupted sequence segment whose total output exceeds the block maximum then wrote past the allocation and hit SIGSEGV. Nightly's `copy_nonoverlapping` precondition check flagged it as overlap first, and the artifacts test crashed the test process.

The fix keeps the budget check unconditional, aligning with libzstd's `op + ll + ml > oend` rejection, and headroom decides only the wildcopy strategy. The dec-st matrix re-run showed no regression. A safety check and an optimization switch must not share one boolean, and the optimization should remove the check's cost while keeping the check.
## Zero the literals overshoot


The flat executor's 16-byte literal chunks read up to 15 bytes past the final literal. `literals_buffer.reserve(16)` kept the read inside the allocation, but the bytes were uninitialized capacity, and reading them is UB under the strict Rust model even though the values only land in the output's overshoot and are overwritten before any read. Libzstd's `ZSTD_wildcopy` over-reads the same shape in C and tolerates it there.

The fix zeroes the 16-byte overshoot window per block through `resize(len+16, 0)` and `truncate`. The buffer is pooled and reserved, so it never reallocates, and the cost is a 16-byte memset per block.

Centralizing the creation and consumption surface in `DecoderScratch.literals_buffer` makes the fix cheaper than documenting the tolerance. That buffer is cleared per block, only ever pushed initialized bytes, and is over-read on exactly one path.
## Four-stream quarter split


The slow path decoded each stream to bit exhaustion and checked only the symbol total, so a stream that encoded past its quarter, with another stream short by the same amount, was accepted with an output layout that no fast path or libzstd would produce.

The slow path now enforces the same segment contract as `finish_streams`: each stream stops at its segment end and must hit both the exact symbol count and exact bit exhaustion. The segment math also forced adopting libzstd's `MIN_LITERALS_FOR_4_STREAMS`, so 4-stream sections below 6 regenerated bytes are rejected up front because their quarter split underflows stream 4, which libzstd rejects at the literals header.

When crafting hand-made Huffman streams for tests, direct weights are 4 bits each with the last weight implicit and the sum must leave a power-of-two leftover. A reversed bit stream byte is laid out as padding zeros, then the end marker 1, then the symbol bits, read MSB-first.
## Dictionary registration semantics


`FrameDecoder::add_dict` and `add_shared_dict` register id-keyed, so the frame's dictID must match. The options-based entries use `add_options_dict`, which also registers under the null id, because libzstd applies a loaded dictionary to every frame regardless of the header's dictID field.

The FFI streaming decoder kept the id-keyed form, so a `ZSTD_c_dictIDFlag=0` frame decoded with the formatted dictionary loaded hit "Data corruption detected" from sequences referencing history the decoder never installed. The one-shot FFI paths that route through `DecoderOptions` worked. Any new decode surface that takes a dictionary follows the options rule unless its contract is id-keyed by name.
## Dictionary repcode load check


`Dictionary::decode_dict` stored the three repeat offsets without libzstd's `ZSTD_loadDEntropy` check of `rep == 0 || rep > dictContentSize`, with the content size excluding the 12 rep bytes. A corrupt dictionary then failed later or mid-frame instead of at load.

The fix reports `DictionaryDecodeError::InvalidRepeatOffset`. The real `--train` fixtures all satisfy the check, and only the fabricated unit-test dictionary with rep 0x00abcdef over 25 content bytes was affected, which had been corrupt all along.
## Reset restores the flat default


`FrameDecoderState::reset` left `flat_active` cleared. After a dictionary frame cleared it through the dictionary branches or `force_dict`, every later dictionary-free frame on the same decoder stayed on the ring path for no reason.

`reset` now restores `flat_active = true`, and `FrameDecoder::reset` re-disables it right after when the next frame carries a dictionary, so the ordering keeps both paths correct.
## The ring reserves at activation


The same reset eagerly reserved the ring for the frame's declared window. With the flat path owning every dictionary-free decode, a reused decoder peaked at the window size before the decode even started: a 9-byte frame with a 2^27 window descriptor made the second frame on the decoder allocate ~134 MB. The reserve belongs where the ring actually activates, the `force_dict` takeover branch; every append reserves per block anyway, so the ring's behavior is unchanged once active.
## Retired bits on no-reload paths


Refactoring the bitstream reader so the read's shift count absorbs the consumed offset split "read n bits" from "advance the accounting". The paths that skip the reload, which are the block's last sequence in both the plain and wide forms, then forgot the advance, and every frame failed `finish` with `ExtraBits{n}`.

The old `consumed += n` lived inside `read`, so no-reload paths were accounted automatically. Any reader shape that moves the accounting out of the read must repeat it on every path. The corpus roundtrip tests caught this, not review.
## Reload loads follow the stream's byte order


The Huffman and FSE reload paths filled their bit registers with the target's native word order. On s390x every backward bitstream then consumed its bytes reversed, and all frames failed `ChecksumMismatch` even though the same frames decoded correctly through the reference CLI with the checksum suppressed. The frame carries the bytes in the stream's own order, so the register refill goes through `bit_io`'s little-endian load on every target, exactly like the bitstream writers' stores on the encoding side.
## The X2 tail needs the pair table


The X2 interleaved loop handed its window state to `finish_streams`, the single-symbol X1 mirror. On an under-running stream both tails decode rotations of the frozen floor window, and a pair lookup consumes both symbols' bits in one step while selecting its second symbol from the slot itself, so the rotation garbage differs from two single-symbol lookups. Both decoders accepted the frame, with different bytes from position 245 on a corrupted jump table. The X2 loop now finishes through `finish_streams_x2`, an exact mirror of `HUF_decodeStreamX2`'s three phases including `HUF_decodeLastSymbolX2`'s consumption clamp, and the phase shapes must stay separate: matching verdicts is not enough when the reference accepts observable garbage.
## Asm templates are Intel operand order


The `x2_inner_asm` template reads as Intel syntax, `dst, src` on every line, even though the emitted listing prints AT&T-style `%reg` names. Writing the new stretch exit as AT&T-style `cmp {olimit}, {op3}` therefore tested `olimit < op3` and looped the stretch away from its bound: the loop only ever grew op[3] by entry lengths, so an Intel-order reading of that line ran until an ip retreated far below the region and segfaulted. The exit is `cmp {op3}, {olimit}` with `jb`, op[3] minus olimit with a below branch, and any new template line must be written in Intel order. The round-bound removal that exposed this was sound on its own: op[3] advances at least 5 per round, five lookups of length at least 1, so the `olimit` compare terminates the stretch within the caller's `iters` envelope without a counter.
