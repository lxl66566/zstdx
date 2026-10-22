# R64 review — Blocks + xxh64

Scope: `crates/zstdx/src/blocks/` (block.rs, literals_section.rs, sequence_section.rs), `crates/zstdx/src/xxh64.rs`, `crates/zstdx/src/common/` (mod.rs, simd.rs), plus the call-site contracts: `decoding/block_decoder.rs`, the four frame-decode entries (ring `decode_blocks`, flat `decode_blocks_flat`/`decode_frame_flat`/`decode_from_to`), `decoding/mt.rs` stage A/B, and the encode-side checksum stack (`encoding/checksum.rs`, `encoding/async_checksum.rs`, `levels/fastest.rs` feeding sites).

Reference: RFC 8878 + libzstd sources under `/root/programs/fork/zstd` (`zstd_decompress_block.c`, `lib/common/xxhash.h`).

## Findings

### Fixed

1. **bug, medium — xxh64 drops the seed for inputs < 32 bytes.** `finish()`'s short-input branch started at bare `PRIME5`; the reference (`xxhash.h` `XXH64_digest`) starts at `v[2] + PRIME5`, where `v[2]` still holds the seed because no round has run. Proven with a failing test vs twox-hash (seed 1, len 0 → `0xEF46DB3751D8E999` instead of `seed+PRIME5`). Every in-tree use is seed 0 (where `v[2] == 0`), so the fix is **byte-neutral for all produced frames**; no ratio/dump differential needed. Fix + nonzero-seed test: 84511678.
2. **test gap, high — the entire header layer was pinned only by accident.** No unit tests existed for `LiteralsSection::parse_from_header`/`header_bytes_needed` (all Raw/RLE 1/2/3-byte forms incl. the sf-0-vs-2 alias, Compressed/Treeless 3/4/5-byte forms with 10/14/18-bit size pairs and 1/4 streams), `SequencesHeader::parse_from_header` (all six nbSeq forms incl. the two zero-count forms that omit the mode byte: `00` and `80 00`), the mode byte's LL/OF/ML packing with reserved low bits ignored, or `read_block_header`'s field layout/reserved/oversized/truncated rejections. The 128 KiB max boundary was unpinned. Tests: 882a241a.
3. **smell — header docs and guards.** `BlockType::Reserved` Display typo ("Reserverd"); mangled `BlockHeader` doc sentence; `serialize_into` accepted a `block_size ≥ 2^21` that would silently wrap into the discarded fourth byte (now `debug_assert`); dead commented-out state check in `read_block_header`; "follwing" typo. 902b1d91.
4. **hardening — `write_appending_from` resume preconditions were prose only.** A violated `hash_from` contract (≤ len, multiple of 32 or len, empty buffer) would mis-hash silently in release; the two computable ones are now `debug_assert`s. All current call sites (`fastest.rs` raw/compressed paths via `FrameHasher::scan_uniform`) verified to satisfy them: `from > 0` only arrives with `mem_len == 0`. a10d5900.

### Verified correct, no change

- **Raw/RLE size_format**: 1-bit discriminator — bit 2 — so sf values 0 *and* 2 are the 1-byte/5-bit form (bit 3 is the regen LSB there), sf 1 → 12 bits, sf 3 → 20 bits. Matches libzstd's `case 0: case 2:` merge in `ZSTD_decodeLiteralsBlock`; the Rust code has always done this correctly.
- **Compressed/Treeless headers**: 3/4/5-byte forms, 10/14/18-bit regen/compressed pairs, streams = 1 (sf 0) / 4 (sf 1-3). Bit packing recomputed by hand against RFC 8878 §3.1.1.1.2 — exact; now also table-pinned by tests.
- **Sequences header**: nbSeq forms match RFC 8878 §3.1.1.3.1.1 and `ZSTD_decodeSeqHeaders`, including the 2-byte form decoding to 0 (`80 00`) omitting the mode byte. Bytes-after-zero-sequences rejected as `ExtraBits` (libzstd parity). Reserved low 2 bits of `Symbol_Compression_Modes` ignored.
- **Block header dispatch and Block_Maximum_Size**: 3-byte layout, reserved type rejected at parse, static 128 KiB cap in `read_block_header`; the window-relative cap `max_block_output = min(window, 128 KiB)` is enforced on every decode entry — ring `decode_block_content` (Raw/RLE output), `parse_sections` (stored body *and* literals regen size, before any allocation), flat entries (`check_block_output_size` + executor budget), MT stage A scan (falls back to sequential for the typed error) and stage B (incl. the RLE repetition count). `ensure_block_space` always reserves ≥ cap + wildcopy slack. Empty-frame single empty Raw block handled on encode and decode.
- **xxh64 exactness**: round/mergeRound/primes/finalize/avalanche identical to `xxhash.h`; streaming split (32-byte mem buffering, four chains) identical; the 2×32 unroll is schedule-only (differential vs twox over 24 lengths 0..70000 and 7-byte chunk splits). `finish()` is idempotent, which `get_calculated_checksum`/double-verify rely on. `scan_uniform_absorbing`'s consumed contract (multiple of 32 on mismatch, mem left empty) is what the `FrameHasher` fallback and `fastest.rs` resume paths rely on — holds.
- **unsafe audit (xxh64.rs)**: every raw read/write in `absorb`/`write_appending_from`/`scan_uniform_absorbing` is bounded by its loop guard; the append path reserves exactly the stored range, writes every byte of it on every path, and `set_len`s once at the end. `out`/`input` aliasing is no worse than `Vec::extend_from_slice`'s own contract. Reads are `read_unaligned`. Sound.
- **unsafe audit (async_checksum.rs)**: SPSC ring with the producer confined to its owning thread (`PhantomData<*const ()>`, nested frames claim from the shared head); slot payload published by `kind`-Release, slot by `head`-Release under the wake mutex so a parking worker cannot miss a post; `tail`-Release/`drain`-Acquire; `FinishCell` stack lifetime covered by the producer's spin; RESET-before-use ordering guarantees `states[state_id]` exists. Sound.
- **`parse_sections` asserts**: `decode_literals` provably returns exactly `upper_limit_for_literals` bytes consumed and enforces the regen count (`DecodedLiteralCountMismatch`), so the asserts are unreachable internal invariants, not input-reachable panics.
- **`decode_literals` Raw/RLE direct slicing**: pre-sliced by `parse_sections` to exactly the declared length; module is `pub(crate)`. Not input-reachable.
- **common/mod.rs constants**: magic, 1 KiB min window, 3.75 TB max window, 128 KiB block max — all match spec. **simd.rs**: clamp tier ordering, env parsing, panic-on-typo documented and tested.

### Dismissed

- Unreachable `_ => panic!` arms in `header_bytes_needed`/`parse_from_header`/`decode_mode` (inputs masked to 2 bits): converting to `unreachable!()` is churn; messages are honest.
- Unreachable error variants (`InvalidBlocktypeNumber`, `IllegalLiteralSectionType`): defensive, enums `non_exhaustive`. Keep.
- `decode_block_content`'s Reserved panic ("How did you even get this"): all four decode entries reject Reserved at header parse; `pub(crate)` only. Defense-in-depth, keep.
- SIMD xxh64: intentionally scalar (file header documents the vpmullq-latency argument); known falsified direction territory (see negative/entropy.md). Not retried.

## Tests added

- `xxh64::tests::nonzero_seed_short_inputs_match_twox` (bug 1 proof + regression).
- `blocks::literals_section::tests`: `raw_rle_header_forms`, `compressed_header_forms`, `header_bytes_needed_matches_parse`, `short_header_is_an_error`.
- `blocks::sequence_section::tests`: `number_of_sequences_forms`, `bytes_read_per_form`, `short_header_is_an_error`, `compression_mode_bit_packing`.
- `decoding::block_decoder::tests`: `header_field_layout` (incl. 128 KiB max), `reserved_and_oversized_are_rejected`, `truncated_header_is_a_read_error`.
- `serialize_into`: 21-bit `debug_assert` (no test — encoder never violates it).

## Results

Full workspace `cargo test` (debug), `--release`, and `-p zstdx --no-default-features` all green after the changes. No byte-affecting change: the xxh64 fix is identical for seed 0 (all real frames).

## For later rounds

- `BlockDecoder`'s state machine is vestigial: `DecoderState::Failed` is never set (its TODO admits it) and `header_buffer` is zeroed after every header for no reason. Candidate for deletion next time the decode paths are touched.
- `LiteralsSection::parse_from_header` parses type/size_format through a `BitReader` but then re-derives every field from raw bytes; the reader only serves the empty-input error. Could be simplified to plain byte arithmetic.
