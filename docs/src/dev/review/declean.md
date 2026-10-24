# Decode core cleanups

This page records three small decode-core follow-ups from the previous review round, each landed as its own commit. It answers why the Huffman weight alphabet now caps at 11, why `BlockDecoder` no longer carries a state machine, and why `LiteralsSection::parse_from_header` no longer uses a `BitReader`. The parity claims reference libzstd v1.5.7 at `/root/programs/fork/zstd`.

## Weight-table NCount alphabet cap

The [todo list](../todo.md) asked for a weight-table alphabet cap. The earlier review note said libzstd caps the weight alphabet at 12 inside FSE_readNCount, and that number was off by one. `HUF_readStats`' FSE branch calls `FSE_decompress_wksp_bmi2(huffWeight, hwSize-1, ip+1, iSize, 6, ...)`. Every call site hands it a workspace of exactly `HUF_READ_STATS_WORKSPACE_SIZE_U32 = FSE_DECOMPRESS_WKSP_SIZE_U32(6, HUF_TABLELOG_MAX-1)`, sized for maxSymbolValue 11. The call sites are `HUF_readStats`' stack workspace and `HUF_readDTableX1/X2_wksp`'s `statsWksp` and `calleeWksp`.

Inside `FSE_decompress_wksp_body`, `readNCount` writes the actual charnum minus 1 into `maxSVPtr`. The next line rejects `FSE_DECOMPRESS_WKSP_SIZE(tableLog, maxSV) > wkspSize` with `tableLog_tooLarge`. The formula is strictly increasing in maxSV above 11, because each extra symbol costs half a U32. A weights NCount with maxSV at most 11 is accepted, and maxSV at least 12 is rejected with `tableLog_tooLarge` regardless of what the bitstream emits.

The cap is an incidental workspace-sizing artifact, but it is deterministic on every decode path. A C probe against `libzstd.a` calling `HUF_readStats` verified it. Crafted headers declared a never-emitted symbol with probability 1: symbol 11 was accepted with nbSymbols 254 and tableLog 8, and symbols 12 and 13 were rejected with `tableLog_tooLarge`.

The declared-but-never-emitted symbol is the interesting class. Such a symbol never enters the decoded weight set, so no Kraft or tableLog bound on the weights can reject it. zstdx previously accepted these headers whenever the emitted weight set was Kraft-valid, proven by a round trip through zstdx's own encoder pipeline with `norm[13]=1`. The Kraft and tableLog argument covers emitted weights only. Any weight of at least 13 forces tableLog of at least 13, and weight 12 forces tableLog 12, which the max-bits checks kill. The declared alphabet stays outside that argument.

`HuffmanTable::new` now builds its weight `FSETable` at `FSETable::new(11)` instead of 255. Over-range declarations fail at the NCount read with `TooManySymbols`, wrapped as `HuffmanTableError::FSETableError`, which is the same layer where libzstd rejects.

No valid stream regresses. Every libzstd-accepted weights header passes the workspace check, so it declares maxSV at most 11 and still parses under the cap. The flipped inputs are exactly the maxSV at least 12 headers, all of which libzstd rejects. The cap also makes weight value 12 unreachable through the FSE form, matching libzstd, where the same workspace check precludes it. The raw 4-bit form still carries weight-12 values, and its existing build-time checks are untouched.

`weight_fse_alphabet_cap` covers the change through the new `fse_table_with_declared` helper. A declared-never-emitted 11 parses with an exact weight round trip, and 12 reports `TooManySymbols(13)`. The earlier `fse_weights_decode_count_bound`, with its 255 legal weights, stays green because its NCount maxSV is 2.

## BlockDecoder state machine

`BlockDecoder` no longer carries a state machine. `decoding/block_decoder.rs` drops `enum DecoderState`, the `internal_state` field and its transition writes. `Failed` was never set, which its TODO admitted. `ExpectedHeaderOfPreviousBlock` was unreachable in tree: every `decode_block_content` call site, the ring loop and the MT fallback in `frame_decoder.rs`, constructs a fresh decoder and goes through `read_block_header` first, and `decompress_block_flat` on the flat and MT paths never consulted the state at all.

`reset_buffer()`, a 3-byte zeroing of `header_buffer` after every header, was dead. `read_block_header` fully overwrites the buffer through `read_exact` before `is_last`, `block_type` or `block_content_size` read any of it.

`DecodeBlockContentError::{DecoderStateIsFailed, ExpectedHeaderOfPreviousBlock}` is removed, because the removed match held its only construction sites. The enum is `#[non_exhaustive]`, so the removal is compatible.

`BlockDecoder` now holds only `header_buffer`. The invariant it relies on is structural: header-then-body ordering holds at every call site, because each frame gets a fresh decoder and each loop keeps read and decode pairs together. Behavior is identical for every in-tree path. The flat path was already stateless, and the removed match sat at the once-per-block entry of the ring path and never in a symbol loop.

## LiteralsSection header parse

`parse_from_header` read the 2-bit type and the 2-bit size_format through a `BitReader`, then re-derived every field with raw byte arithmetic such as `raw[0] >> 3`. The reader's only real contribution was the empty-input error. The function now uses plain byte arithmetic: `let Some(&first) = raw.first() else`, `section_type(first)`, and `(first >> 2) & 3`. Empty input returns the identical `GetBitsError::NotEnoughRemainingBits { requested: 2, remaining: 0 }` that the 2-bit type read produced. The `BitReader` import is gone from the module.

Field arithmetic, error classes and check ordering are unchanged, including the rule that truncation is rejected before any multi-byte field read. The header tests from the previous round, `raw_rle_header_forms`, `compressed_header_forms`, `header_bytes_needed_matches_parse` and `short_header_is_an_error`, stay green. The empty-input case is now pinned to the exact variant.

## Verification

Full workspace `cargo test` in debug was green after each commit. Final `--release` and `-p zstdx --no-default-features` runs passed as well. `cargo clippy -p zstdx` reported zero warnings. With `--all-targets`, clippy still shows one pre-existing `items_after_statements` in the `reserved_and_oversized_are_rejected` test from the [Block layer review](blocks.md), untouched here.
