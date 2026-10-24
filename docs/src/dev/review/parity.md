# Decode error paths

Scope: the decode-side sequence execution and frame-level error paths in `decoding/sequence_execution.rs`, `decoding/frame.rs`, `decoding/block_decoder.rs`, `decoding/mt.rs` and `decoding/mt_pieces.rs` error arms, `decoding/errors.rs`, and `blocks/sequence_section.rs` header parse, plus the literals entry paths not covered in the [FSE core review](fse.md) and the [Huffman coder review](huff0.md). Reference: libzstd v1.5.7, namely the `zstd -d` CLI and `ZSTD_decompress*` under `/root/programs/fork/zstd/lib`.

## Method

The method extends the differential harness from the previous review round. It uses 24 real frames from text, semi-random and RLE-mixed seeds at levels 1 to 19, with checksum and single-thread variants, mutated by exhaustive per-position truncation and single-byte XOR. It also uses 26 handcrafted header frames and 16 crafted sequence-section frames. Every case is decoded by zstdx through the one-shot `decompress_to_buffer` and by `zstd -d -c`, and the harness compares accept or reject plus the output length on accepts. The total is 15,529 cases, run twice, once serially and once with the 4-thread MT decoder. Any case where one side sets the modes byte's reserved bits was scanned exhaustively, at all positions with `| 0x03`.

## Divergences fixed

The reserved low 2 bits of the `Symbol_Compression_Modes` byte were never validated. Every real frame kept decoding after `modes |= 0x03`, while libzstd's `ZSTD_decodeSeqHeaders` rejects on `*ip & 3`. The MT planner inherited the hole, because `probe_restart` parsed the byte too. This was a spec violation with no memory-safety impact, since the bits were ignored. `CompressionModes::parse` now rejects `ReservedBitsSet`, pinned by `tests/parity.rs::frame_with_reserved_bits_in_mode_byte_is_rejected` and a `SequencesHeader` unit test.

`DEFAULT_MAX_WINDOW_SIZE` was exactly 128 MiB with a strict `>` check. That rejected the single window value (1<<27)+1, which libzstd's default accepts through `ZSTD_MAXWINDOWSIZE_DEFAULT` with the check `windowSize > maxWindowSize`. The case is reachable only through a single-segment frame with an FCS of 128 MiB + 1. The default is now (1<<27)+1, byte-identical to the reference default.

`MAX_WINDOW_SIZE` was the wire-format maximum of windowLog 41, or 3.75 TB. A caller that raised the per-decoder limit could therefore accept windows that no libzstd configuration decodes. The header parse rejects windowLog above 31, which is `ZSTD_WINDOWLOG_MAX`, and `ZSTD_d_windowLogMax` and `ZSTD_DCtx_setMaxWindowSize` cannot be raised past 2^31. Default-path behavior was already identical, because both sides rejected values above 128 MiB + 1. The absolute ceiling is now 1<<31 inclusive, and `set_max_window_size` clamps to it.

## Accepted differences

NCount tables over a sequence alphabet's cap, which is LL 35, ML 52 and OF 31, differ only in error class. C breaks inside `FSE_readNCount` on `charnum > maxSV` and reports corruption or `maxSymbolValue_tooSmall`. zstdx reads to the sum and rejects at `build_decoding_table` with `TooManySymbols`. Both sides reject the same inputs. This is the same pattern recorded in the [FSE core review](fse.md), now verified for all three sequence tables, and unifying the class buys nothing.

An RLE mode symbol over the alphabet cap reports `MissingByteForRle*LlTable` for both the empty-source case and the out-of-range case, while C reports `srcSize_wrong` for one and `corruption_detected` for the other. Both sides reject the same inputs. The variant naming is off, because it is not about a missing byte for the out-of-range arm, and the churn would exceed the value.

`DecodeSequenceError::UnsupportedOffset` is never constructed. `OF_CODES` covers codes 0 through 31, so the full 32-bit offset space decodes, and offset code 31 matches libzstd's MaxOff. The variant stays as `#[non_exhaustive]` headroom.

## Offset scope

The one-shot `ZSTD_decompress` accepts a match offset anywhere in the already-produced output, through the prefix check `offset > oLitEnd - virtualStart`. Only the streaming decoder bounds the offset by the window, because data outside the ring was already flushed. zstdx's flat executor implements exactly the one-shot formula, `offset > virt_base + pos`, and its ring path implements the window formula. A window-violating frame can therefore accept through `bulk::decompress` while `zstd -d`, a streaming decoder, rejects it. This matches the corresponding libzstd API pair and was verified by construction against the C source. No action.

## Verified clean

- The mutation sweep covered 15,529 cases of truncations, XOR flips with 0xFF, 0x01 and 0x80 rotated, deletes, inserts and handcrafted headers, with zero accept-or-reject or length divergences, serially and with the 4-thread MT decoder.
- The crafted sequence frames agree. They cover an offset beyond the output, LL over the literals, the nbSeq 0 two-byte form both alone and with a trailing byte, a 127-sequence stream underflow, and leftover bits. They also cover a zero or empty sequence stream, RLE symbols over each alphabet cap, Repeat mode with no prior table, predefined modes and reserved modes.
- The frame header checks agree. The reserved descriptor bit is rejected, and unused bit 4 is accepted because the RFC says it shall not be interpreted. Every FCS field form parses, including the +256 two-byte offset. Single-segment FCS = 0 is an empty frame accepted by both, because `windowSize = 0` is not an error. A declared but missing dictionary id is rejected by both, skippable frames work including the zero-length and truncated payload cases, and trailing garbage is rejected as a next-frame parse error.
- An NCount description whose states sum past `1 << tableLog` stops silently at the exact sum in both implementations. C breaks on `remaining <= 1` and the post-loop `remaining == 1` passes, and the port stops at `counter == sum`. The unread tail is left for the next section, with the same accept and the same consumed bytes. A new fuzz target flagged the shape, and its first assertion draft wrongly expected rejection.
- At the end of the sequence bitstream, the last-byte-zero rejection matches `BIT_initDStream`'s `lastByte == 0` GENERIC. `finish`'s exact-consumption accounting matches `BIT_endOfDStream`. The nbSeq varint forms' byte budgets match `ZSTD_decodeSeqHeaders`, including the 0x80 0x00 zero form.
- The block layer agrees on the reserved block type. A 21-bit size over MAX_BLOCK_SIZE is rejected, and Raw and RLE output over min(window, 128K) is rejected too. A compressed body over the same cap is rejected, because libzstd checks `cBlockSize > blockSizeMax` for every type. Literals `regenerated_size` over the cap is rejected, and RLE literals of up to 1 MiB from one byte are rejected before allocation.
- Every dict-less production entry, namely bulk one-shot, `StreamingDecoder` and MT, runs the flat executor, since `flat_active` defaults true as verified in `frame_decoder.rs`, so the sweep covered the executor that ships. The ring-buffer executor, `execute_sequences` plus the `DecodeBufferError` arms, is reachable only through dictionary frames because `flat_active = false` on dict init. Its checks mirror the flat executor one for one, with the same `do_offset_history` and the same literal, offset and block-cap gates, and `tests/block_output_cap.rs` pins them at unit level. A dictionary-frame differential belongs to a dictionary round.
- The MT decoder's scan and plan fall back to the sequential path for every structural irregularity, namely dictionary frames, oversized blocks, reserved types, a window over the limit and skippable truncation, so its rejections are the sequential path's. Stage errors surface in segment order through `take_stage_error`, matching the sequential error position.
- A wrong or missing checksum is rejected, including flips on the `*_chk` seeds, and `verify_content_size` fires at the last block exactly where libzstd reports the size error.

## Fuzz additions

`fse.rs` keeps `round_trip` and adds two targets. `ncount_malformed` feeds arbitrary bytes as NCount under all four sequence-table caps, with truncated, de-prefixed and padded variants. `ncount_constructed` uses the property shapes from the previous round: packed 0xFFFF zero-run markers, a full 255-symbol near-max-entropy Kraft-exact table at tableLog 12, and a 257-symbol alphabet that must parse as `TooManySymbols`. Every Kraft-exact distribution must serialize and parse back with the parser consuming exactly the written bytes, and head-bit flips of the serialized stream must not panic. The new `fse::serialize_ncount`, gated behind `fuzz_exports`, exposes the serializer to targets without widening `bit_io`.

`huff0.rs` keeps `round_trip` and adds `weight_description_parse`, which feeds arbitrary bytes as Huffman weight descriptions in both the raw and FSE forms of `HUF_readStats`, with truncations, padding and head-bit flips, and requires a clean failure.

Both targets were run with `cargo fuzz run --fuzz-dir crates/zstdx-fuzz <target> -s none -- -runs=300` and completed 300 runs with no findings.

## Commits

- 265c4a63 decoding: reject reserved bits in the Symbol_Compression_Modes byte
- 65a5746c decoding: align window limits with libzstd exactly
- 57de97b7 fuzz: extend fse/huff0 targets with the R64 NCount/weight classes

## Verification

Full `cargo test` with debug, `--release -p zstdx` and `-p zstdx --no-default-features` was green before and after. The run had 392 lib tests, including the 5 new parity pins and the `SequencesHeader` and MT probe updates.
