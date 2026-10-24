# Block layer review

Scope: `crates/zstdx/src/blocks/` with `block.rs`, `literals_section.rs` and `sequence_section.rs`, `crates/zstdx/src/xxh64.rs`, and `crates/zstdx/src/common/` with `mod.rs` and `simd.rs`. The call-site contracts cover `decoding/block_decoder.rs`, the four frame-decode entries, the ring `decode_blocks` and the flat `decode_blocks_flat`, `decode_frame_flat` and `decode_from_to`, `decoding/mt.rs` stage A and B, and the encode-side checksum stack in `encoding/checksum.rs`, `encoding/async_checksum.rs` and the feeding sites in `levels/fastest.rs`. Reference: RFC 8878 and the libzstd sources under `/root/programs/fork/zstd`, namely `zstd_decompress_block.c` and `lib/common/xxhash.h`.

## Bugs fixed

### Short-input xxh64 seed

`finish()` dropped the seed for inputs shorter than 32 bytes. The short-input branch started at bare `PRIME5`. The reference `XXH64_digest` in `xxhash.h` starts at `v[2] + PRIME5`, and `v[2]` still holds the seed because no round has run. A failing test against twox-hash proved it: seed 1 with length 0 gave `0xEF46DB3751D8E999` instead of `seed+PRIME5`. Every in-tree use passes seed 0, where `v[2] == 0`, so the fix is byte-neutral for all produced frames and needed no ratio or dump differential. The nonzero-seed test stays as the regression.

## Hardening

`write_appending_from`'s resume preconditions existed only in prose. A violated `hash_from` contract, meaning a value above the length, a value that is neither a multiple of 32 nor the length, or an empty buffer, would mis-hash silently in release. The two computable conditions are now `debug_assert`s. All current call sites satisfy them, namely the raw and compressed paths in `fastest.rs` through `FrameHasher::scan_uniform`, and `from > 0` only arrives with `mem_len == 0`.

`serialize_into` accepted a `block_size ≥ 2^21`, which would silently wrap into the discarded fourth byte. A `debug_assert` now pins the field width.

## Header coverage

The header layer was pinned only by accident. No unit tests existed for `LiteralsSection::parse_from_header` or `header_bytes_needed` across the Raw and RLE 1, 2 and 3-byte forms, including the sf-0-versus-2 alias. The Compressed and Treeless 3, 4 and 5-byte forms with 10, 14 and 18-bit size pairs and 1 or 4 streams were untested too. `SequencesHeader::parse_from_header` was untested across all six nbSeq forms, including the two zero-count forms that omit the mode byte, `00` and `80 00`, and so was the mode byte's LL, OF and ML packing with reserved low bits ignored. `read_block_header`'s field layout and its reserved, oversized and truncated rejections were untested, and the 128 KiB maximum boundary was unpinned. The tests added below close these gaps.

## Cleanups

The `BlockType::Reserved` Display implementation had the typo "Reserverd", a `BlockHeader` doc sentence was mangled, and `read_block_header` carried a dead commented-out state check and the typo "follwing". All are fixed. The header docs now describe the parsed fields.

## Verified correct

### Literals headers

The Raw and RLE size_format uses a 1-bit discriminator at bit 2, so sf values 0 and 2 both take the 1-byte, 5-bit form, with bit 3 as the regen LSB there. sf 1 takes 12 bits and sf 3 takes 20 bits. This matches libzstd's `case 0: case 2:` merge in `ZSTD_decodeLiteralsBlock`, and the Rust code has always done it correctly.

The Compressed and Treeless headers use the 3, 4 and 5-byte forms with 10, 14 and 18-bit regen and compressed pairs, and one stream for sf 0 and four streams for sf 1 through 3. The bit packing was recomputed by hand against RFC 8878 section 3.1.1.1.2 and is exact. The new tests pin it in a table.

### Sequences header

The nbSeq forms match RFC 8878 section 3.1.1.3.1.1 and `ZSTD_decodeSeqHeaders`, including the 2-byte form `80 00` that decodes to 0 and omits the mode byte. Bytes after a zero sequence count are rejected as `ExtraBits`, matching libzstd. The reserved low 2 bits of `Symbol_Compression_Modes` are ignored.

### Block header and maximum size

The block header uses the 3-byte layout, rejects the reserved type at parse, and enforces the static 128 KiB cap in `read_block_header`. The window-relative cap `max_block_output = min(window, 128 KiB)` holds on every decode entry. The ring `decode_block_content` checks it for Raw and RLE output, and `parse_sections` checks it for the stored body and the literals regen size before any allocation. The flat entries check it through `check_block_output_size` and the executor budget. The MT stage A scan checks it and falls back to sequential for the typed error, and stage B checks it including the RLE repetition count. `ensure_block_space` always reserves at least the cap plus wildcopy slack. The empty frame's single empty Raw block is handled on encode and decode.

### xxh64 exactness

The round, mergeRound, primes, finalize and avalanche functions are identical to `xxhash.h`. The streaming split with 32-byte memory buffering and four chains is identical. The 2x32 unroll is schedule-only, checked by a differential against twox over 24 lengths from 0 to 70000 and 7-byte chunk splits. `finish()` is idempotent, which `get_calculated_checksum` and the double verify rely on. `scan_uniform_absorbing`'s consumed contract, a multiple of 32 on mismatch with `mem` left empty, is what the `FrameHasher` fallback and the `fastest.rs` resume paths rely on, and it holds.

### Unsafe audit for `xxh64.rs`

Every raw read and write in `absorb`, `write_appending_from` and `scan_uniform_absorbing` is bounded by its loop guard. The append path reserves exactly the stored range, writes every byte of it on every path, and calls `set_len` once at the end. The aliasing between `out` and `input` is no worse than `Vec::extend_from_slice`'s own contract. Reads use `read_unaligned`. The code is sound.

### Unsafe audit for `async_checksum.rs`

The SPSC ring confines the producer to its owning thread through `PhantomData<*const ()>`, and nested frames claim from the shared head. The slot payload is published by `kind` with Release, and the slot by `head` with Release under the wake mutex, so a parking worker cannot miss a post. `tail` uses Release and `drain` uses Acquire. The producer's spin covers the `FinishCell` stack lifetime. The reset-before-use ordering guarantees that `states[state_id]` exists. The code is sound.

### Internal invariants

`parse_sections`' asserts are unreachable internal invariants, and input cannot reach them. `decode_literals` provably returns exactly `upper_limit_for_literals` bytes consumed and enforces the regen count through `DecodedLiteralCountMismatch`. `decode_literals`' Raw and RLE direct slicing is pre-sliced by `parse_sections` to exactly the declared length, and the module is `pub(crate)`, so it is not input-reachable.

The `common/mod.rs` constants match the spec: the magic, the 1 KiB minimum window, the 3.75 TB maximum window and the 128 KiB maximum block. `simd.rs` has the clamp tier ordering, env parsing, and a documented and tested panic on a typo.

## Accepted limits

The unreachable `_ => panic!` arms in `header_bytes_needed`, `parse_from_header` and `decode_mode` stay. The inputs are masked to 2 bits, and converting the arms to `unreachable!()` is churn. The messages are accurate.

The unreachable error variants `InvalidBlocktypeNumber` and `IllegalLiteralSectionType` stay as defense in depth. The enums are `non_exhaustive`.

`decode_block_content`'s Reserved panic with the message "How did you even get this" stays as defense in depth. All four decode entries reject Reserved at header parse, and the function is `pub(crate)` only.

SIMD xxh64 stays scalar. The file header documents the vpmullq latency argument, and the direction is already in falsified territory, recorded in [negative/entropy.md](../negative/entropy.md). It is not retried.

## Tests added

- `xxh64::tests::nonzero_seed_short_inputs_match_twox` proves the bug and guards the fix.
- `blocks::literals_section::tests` holds `raw_rle_header_forms`, `compressed_header_forms`, `header_bytes_needed_matches_parse` and `short_header_is_an_error`.
- `blocks::sequence_section::tests` holds `number_of_sequences_forms`, `bytes_read_per_form`, `short_header_is_an_error` and `compression_mode_bit_packing`.
- `decoding::block_decoder::tests` holds `header_field_layout` with the 128 KiB maximum, `reserved_and_oversized_are_rejected` and `truncated_header_is_a_read_error`.
- `serialize_into`'s 21-bit `debug_assert` has no test, because the encoder never violates it.

## Open items

`BlockDecoder`'s state machine is vestigial. `DecoderState::Failed` is never set, which its TODO admits, and `header_buffer` is zeroed after every header for no reason. It is a candidate for deletion the next time the decode paths are touched.

`LiteralsSection::parse_from_header` parses type and size_format through a `BitReader` and then re-derives every field from raw bytes. The reader only serves the empty-input error, so the code could use plain byte arithmetic.

## Commits

- 84511678 fixes the xxh64 short-input seed and adds the nonzero-seed test
- 882a241a adds the header-layer tests
- 902b1d91 fixes the header docs, the Display typo, the dead state check and the `serialize_into` guard
- a10d5900 adds the `write_appending_from` preconditions as debug asserts

## Verification

Full workspace `cargo test` in debug, `--release` and `-p zstdx --no-default-features` was green after the changes. No change affects bytes, because the xxh64 fix is identical at seed 0, which every real frame uses.
