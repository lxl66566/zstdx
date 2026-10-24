# Decode-path walls

This page records the decode-side hardening round that followed the encode-path [hardening](hardening.md) model. An audit confirmed no reachable memory-safety bug on the decode paths, but five load-bearing invariants were held up by comments alone, so a refactor could silently degrade them into undefined behavior. The round turns each of them into something debug-detectable, and the state-writing ones into structure that cannot be violated sideways. Every wall lives under `cfg(debug_assertions)`; release codegen and semantics stay untouched.

## Blind spots

The sequence decode loop reads its three FSE tables through raw `*tbl.add(slot + state)` loads whose safety rests entirely on a theorem about the table builder's output, with nothing checking the builder. The literals staging publishes uninitialized heap with `set_len` and relies on a cross-function write-before-read discipline; an unwritten byte is an uninitialized read that no sanitizer sees, because it lands inside the allocation. The FlatOut generation fields and the decode ring's cursors are paired state written field by field at multiple sites, so prose invariants about them are re-proven only by reading every path. The mt stage's output accounting sums plain `usize`, which wraps consistently on 32-bit targets.

## Walls

### FSE table factory proofs

Every raw table load descends from one theorem: `build_decoding_table`'s spread math gives each entry `base_line + (1 << num_bits) <= 1 << accuracy_log`, so any state transition (base plus the bits it reads) lands inside the table, whatever the bitstream says. Two `cfg(debug_assertions)` scans pin it at the factories. The raw scan runs over the finished `decode` table in `fse_decoder`, covering every producer (sequence tables, predefined tables, Huffman weight tables, dictionary tables). The packed scan runs inside `pack_seq_table` over the packed entries in the packed index domain: the decode loop computes its next state as the packed `next_state_base + add_bits` and indexes the slot with it directly, and the stale slot tail beyond the real table length stays unreachable exactly while every packed entry's transition stays below that length. The u16 next-state field also only survives the packing while the theorem holds, since the theorem bounds the base below the table size. A builder regression is now a debug panic at build time instead of a heap overread at decode time.

### Literals staging poison

The 4-stream fast path in `literals_section_decoder` reserves the section's size and publishes the region with `set_len`, then the interleave plus `finish_streams` overwrite every byte before anything reads it. The wall mirrors `encoding::poison` on the decode side: before `set_len`, the to-be-published region plus a 64-byte margin behind it is filled with the pattern through the spare capacity (a debug-only extra reserve backs the margin), and at the interleave exit — on success and on error, before the length is restored — the margin is asserted intact. A byte the interleave never writes now reads the pattern instead of uninitialized memory, and any write past the published region panics with the checkpoint name.

The margin assert must run inside `decompress_literals`, not at the next call's entry: the flat block path legitimately writes zeros over the first 16 slack bytes right after (`block_decoder`'s wildcopy overread guard), so an entry-keyed wall would false-positive on it.

### FlatOut generation chain

The eight coupled fields carry three invariants. The chain identity `origin == prev_origin + seg_a_end` is what `copy_wrapped_match`'s fast path folds into a single pointer add. A `true` return from `ensure_block_space` promises `block_max + WILDCOPY_SLACK` bytes of target room beyond the cursor. And a block never straddles a wrap, because wraps only fire on a fully handed-out buffer inside `ensure_block_space`.

All generation-field writes now go through `set_generation_chain`, which checks the chain identity at the single write point (the same shape as the encoder's `HeadTables`). `ensure_block_space` became a checked entry over the unchanged room logic, asserting the target-room promise and the cursor invariants on every `true` return. `advance` asserts its cursor stays inside the buffer, and `view` runs the state check per block. The HEADROOM chain gained its consumer-side link: `decompress_block_flat` asserts that `headroom` implies the block maximum plus slack inside the target slice, the one place that knows the block maximum — before this round the chain from `ensure_block_space` through `block_target` to the executor's unconditional wildcopy was connected by comments only. `copy_wrapped_match` additionally pins its segment mapping with `src_abs < seg_end`, guarding the chunk-length subtraction. The `seg_a_end` field docs were wrong (they claimed a virtual end); it stores the previous generation's run length.

### Decode ring write points

The ring's invariant 3 (`head < cap` and `tail < cap`) is exactly `wrap_add`'s precondition, and none of the 29 existing debug asserts pinned it, while `tail` had eight scattered write points, `head` three, and the realloc block wrote the `buf`/`cap`/`head`/`tail` quartet field by field. Three named entries collect the writes in the `HeadTables` spirit: `advance_tail` and `advance_head` (bounds and `wrap_add` preconditions checked before and after) take every append and drop, and `install_allocation` owns the realloc quartet with the sentinel bound asserted. The two raw-pointer factories `data_slice_parts` and `free_slice_parts` check the cursor bounds before computing addresses. All are `#[inline(always)]` with debug-only checks.

### mt stage accounting saturation

Stage A's `out_size` accumulation (Raw, RLE and Compressed arms) and stage B's `cap` fold now use saturating arithmetic, in the same style as the existing saturated Σml comments. On 32-bit targets a segment with enough capped blocks wrapped both consistently; today that is memory-safe through the per-block caps, but the accounting that sizes the stage-B region degraded silently. Saturation fails the region sizing loudly instead.

## Gate numbers

- Tests with every wall armed: debug 432+12+21, release 427+12+21, no-default-features 253+21, paranoid 432+12+21. Zero failures anywhere.
- `nm` over the release rlib shows no wall symbols; the only `poison` matches are std's own mutex namespace.
- Decode output is unchanged by construction (asserts and encapsulation only), so no dump gate was required.

### Codegen and wall A/B

A full per-function disassembly diff of the release bench binary against the pre-round base shows four changed functions out of 3237: the renamed `ensure_block_space` body (the checked wrapper compiles away; the body is instruction-identical), `reserve_amortized` (the merged install block, a cold once-per-growth tail), and the two mt stage functions carrying the saturating arithmetic. Every decode hot-path function — the fused decode step, both interleave loops, `finish_streams`, the table builders, the flat executor — is instruction-for-instruction identical.

The interleaved wall A/B (`prof dec`, 12 iters per round, both prebuilt binaries alternating under the bench lock, on a machine with two other agents compiling) reads, as medians of three rounds: json.zst3 +0.2%, text.zst9 0.0%, skewed.zst1 −1.7%. Same-binary round-to-round swings reached 5% on skewed and 10% on text during the session, so all deltas sit inside the noise band, which the instruction-identical codegen already predicted.

## Known unwallable zones

The literals buffer's first 16 slack bytes are the executor's documented wildcopy overread zone; `block_decoder` zeroes them per block, and the staging margin assert runs before that by design. The ring's free zone is every append's write destination, so only its cursor bounds are assertable, not its bytes. The flat buffer's handed-out previous generation is live match history until the next wrap overwrites it. Caller-borrowed output slices are ASAN's territory at the allocation end, as on the encode side. `FlatView::one_shot`'s `seg_a_end` filler is never read (the never-wrapped instantiation never calls `copy_wrapped_match`), so the chain identity is asserted on `FlatOut` only, where the real writes live.

## Commits

- dec: factory-proof asserts over built and packed FSE tables
- dec: poison wall over the literals staging's published region
- dec: single write entry and invariant asserts for the FlatOut generations
- dec: named cursor write points and bound asserts for the decode ring
- dec: saturate the mt stage's output-size accounting
