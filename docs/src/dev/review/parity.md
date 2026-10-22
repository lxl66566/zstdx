# R65 review — decode parity (accepts vs libzstd)

Scope: decode-side sequence execution and frame-level error paths — `decoding/sequence_execution.rs`, `decoding/frame.rs`, `decoding/block_decoder.rs`, `decoding/mt.rs` + `mt_pieces.rs` (error arms), `decoding/errors.rs`, `blocks/sequence_section.rs` (header parse), literals entry paths not covered by R64 ([fse](fse.md), [huff0](huff0.md)). Reference: libzstd v1.5.7 (`zstd -d` CLI, `ZSTD_decompress*` in /root/programs/fork/zstd/lib).

Method (extends R64's): a differential harness — 24 real frames (text/semi-random/RLE-mixed seeds at levels 1-19, checksum and single-thread variants) mutated by exhaustive per-position truncation and single-byte XOR, plus 26 handcrafted header frames and 16 crafted sequence-section frames; every case decoded by zstdx (one-shot `decompress_to_buffer`) and by `zstd -d -c`, comparing accept/reject (and output length on accepts). 15,529 cases total, run twice (serial + 4-thread MT decoder). Any case where one side sets the modes byte's reserved bits was additionally scanned exhaustively (all positions, `| 0x03`).

## Divergences found

| # | Class | Finding | Verdict |
|---|---|---|---|
| 1 | accepts-what-rejects | The Symbol_Compression_Modes byte's low 2 bits (RFC 8878 reserved field) were never validated: every real frame keeps decoding after `modes \|= 0x03` while libzstd's `ZSTD_decodeSeqHeaders` rejects on `*ip & 3`. The MT planner inherited the hole (`probe_restart` parsed the byte too). Spec violation, no memory-safety impact (the bits were ignored). | **Fixed** — `CompressionModes::parse` rejects `ReservedBitsSet`; pinned by `tests/parity.rs::frame_with_reserved_bits_in_mode_byte_is_rejected` + a `SequencesHeader` unit test. 265c4a63 |
| 2 | rejects-what-accepts | `DEFAULT_MAX_WINDOW_SIZE` was exactly 128 MiB with a strict `>` check, rejecting the single window value (1<<27)+1 that libzstd's default (`ZSTD_MAXWINDOWSIZE_DEFAULT`, check `windowSize > maxWindowSize`) accepts. Reachable only via a single-segment frame with FCS = 128 MiB + 1. | **Fixed** — default is now (1<<27)+1, byte-identical to the reference default. 65a5746c |
| 3 | accepts-what-rejects (opt-in) | `MAX_WINDOW_SIZE` was the wire-format maximum (windowLog 41, 3.75 TB), so a caller raising the per-decoder limit could accept windows no libzstd configuration decodes: the header parse rejects windowLog > 31 (`ZSTD_WINDOWLOG_MAX`) outright and `ZSTD_d_windowLogMax`/`ZSTD_DCtx_setMaxWindowSize` cannot be raised past 2^31. Default-path behavior was already identical (both reject > 128 MiB+1). | **Fixed** — absolute ceiling lowered to 1<<31 (inclusive), `set_max_window_size` clamps to it. 65a5746c |
| 4 | error-class-only | NCount tables over a sequence alphabet's cap (LL 35 / ML 52 / OF 31): C breaks inside `FSE_readNCount` on `charnum > maxSV` (corruption / maxSymbolValue_tooSmall); zstdx reads to the sum and rejects at `build_decoding_table` (`TooManySymbols`). Same inputs rejected on both sides (the R64 fse.md finding 7 pattern, now verified for all three sequence tables). | **Accepted** — net accept/reject parity; class unification buys nothing |
| 5 | error-class-only | RLE mode symbol over the alphabet cap uses `MissingByteForRle*LlTable` for both the empty-source and out-of-range cases (C: `srcSize_wrong` vs `corruption_detected`). Both reject the same inputs. | **Accepted** — variant naming is off (it is not about a missing byte for the out-of-range arm), churn > value |
| 6 | dead code | `DecodeSequenceError::UnsupportedOffset` is never constructed: OF_CODES covers codes 0..=31, so the full 32-bit offset space decodes (offset code 31 ≡ libzstd's MaxOff). | **Accepted** — kept as `#[non_exhaustive]` headroom |
| 7 | semantics (not a divergence) | One-shot `ZSTD_decompress` accepts a match offset anywhere in the already-produced output (prefix check `offset > oLitEnd - virtualStart`); only the streaming decoder bounds it by the window (data outside the ring was already flushed). zstdx's flat executor implements exactly the one-shot formula (`offset > virt_base + pos`), its ring path the window formula — so a window-violating frame can accept through `bulk::decompress` while `zstd -d` (streaming) rejects it. This matches the corresponding libzstd API pair. | **Verified by construction** (C-source line-by-line), no action |

## Verified clean (no action)

- Mutation sweep: 15,529 cases (truncations, XOR flips 0xFF/0x01/0x80 rotated, deletes, inserts, handcrafted headers) — zero accept/reject or length divergences, serial and MT (4 threads) alike.
- Crafted sequence frames: offset beyond output, ll over literals, nbSeq 0 two-byte form (valid alone / rejected with a trailing byte), 127-seq stream underflow, leftover bits, zero/empty seq stream, RLE symbols over each alphabet cap, Repeat mode with no prior table, predefined modes, reserved modes — all agree.
- Frame header: reserved descriptor bit rejected, unused bit 4 accepted (RFC: shall not interpret), all FCS field forms incl. the +256 two-byte offset, single-segment FCS = 0 (empty frame accepted by both — `windowSize = 0` is not an error), dictionary id declared but missing (both reject), skippable frames incl. zero-length and truncated payload, trailing garbage rejected as a next-frame parse error.
- NCount over-subscription: a description whose states sum past `1 << tableLog` stops silently at the exact sum in BOTH implementations (C breaks on `remaining <= 1` and the post-loop `remaining == 1` passes; the port stops at `counter == sum`), leaving the unread tail for the next section — same accept, same consumed bytes. Verified after the new fuzz target flagged the shape (its first assertion draft wrongly expected rejection).
- Sequence bitstream end: last-byte-zero rejection ≡ `BIT_initDStream`'s `lastByte == 0` GENERIC; `finish`'s exact-consumption accounting ≡ `BIT_endOfDStream`; the nbSeq varint forms' byte budgets match `ZSTD_decodeSeqHeaders` incl. the 0x80 0x00 zero form.
- Block layer: reserved block type, 21-bit size over MAX_BLOCK_SIZE, Raw/RLE over min(window, 128K), compressed body over the same cap (libzstd checks `cBlockSize > blockSizeMax` for every type), literals regenerated_size over the cap, RLE literals up to 1 MiB from one byte (rejected before allocation).
- Executor paths: every dict-less production entry (bulk one-shot, `StreamingDecoder`, MT) runs the flat executor (`flat_active` defaults true; verified in `frame_decoder.rs`), so the sweep covered the executor actually shipping. The ring-buffer executor (`execute_sequences` + `DecodeBufferError` arms) is reachable only through dictionary frames (`flat_active = false` on dict init); its checks mirror the flat executor's one-for-one (same `do_offset_history`, same literal/offset/block-cap gates) and are pinned at unit level in `tests/block_output_cap.rs` — a dictionary-frame differential is a dict-round item.
- MT decoder: the scan/plan falls back to the sequential path for every structural irregularity (dictionary frames, oversized blocks, reserved types, window over limit, skippable truncation), so its rejections are the sequential path's; stage errors surface in segment order (`take_stage_error`), matching sequential error position.
- Checksum: wrong/missing checksum bytes both reject (flips on `*_chk` seeds included); `verify_content_size` fires at the last block exactly where libzstd reports the size error.

## Fuzz additions

- `fse.rs`: kept `round_trip`; added `ncount_malformed` (arbitrary bytes as NCount under all four sequence-table caps, with truncated / de-prefixed / padded variants) and `ncount_constructed` (R64 property shapes: packed 0xFFFF zero-run markers, full 255-symbol near-max-entropy Kraft-exact table at tableLog 12, 257-symbol alphabet must parse as `TooManySymbols`; every Kraft-exact distribution must serialize and parse back with the parser consuming exactly the written bytes; head-bit flips of the serialized stream must not panic). New `fse::serialize_ncount` (`fuzz_exports`-gated) exposes the serializer to targets without widening `bit_io`.
- `huff0.rs`: kept `round_trip`; added `weight_description_parse` (arbitrary bytes as Huffman weight descriptions — raw and FSE forms of `HUF_readStats` — with truncations, padding, and head-bit flips; must fail cleanly).
- Proof they run: `cargo fuzz run --fuzz-dir crates/zstdx-fuzz <target> -s none -- -runs=300`, both targets complete 300 runs with no findings.

## Commits

- 265c4a63 decoding: reject reserved bits in the Symbol_Compression_Modes byte
- 65a5746c decoding: align window limits with libzstd exactly
- 57de97b7 fuzz: extend fse/huff0 targets with the R64 NCount/weight classes

## Tests

Full `cargo test` (debug), `--release -p zstdx`, `-p zstdx --no-default-features`: green before and after (392 lib tests incl. the 5 new parity pins and the SequencesHeader/mt-probe updates).
