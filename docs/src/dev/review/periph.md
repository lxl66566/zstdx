# Peripheral review

This page records the 2026-10-25 review round over the modules without a dedicated review record: the streaming cores and `bulk.rs`, the encoder's non-matcher modules (`frame_compressor`, `block_enc`, entropy emission), and the compat, dictionary-trainer and option surfaces. The method was a three-way read with each finding re-verified against the live code paths before landing. Performance, decoder entropy and matcher behavior were out of scope; those live in their own pages.

## Landed fixes

### nbSeq long-form emission

`encode_seqnum` carried two defects in the counts-above-32511 domain, reachable whenever one block packs more than 0x7F00 sequences (a 128 KiB block admits up to about 43690 at three bytes per sequence). The two-byte arm extended to `0x7FFF`, but its high byte `(n >> 8) | 0x80` becomes `0xFF` — the long-form marker — at `0x7F00`, so counts in [0x7F00, 0x7FFF] serialized a two-byte header that every parser reads as four bytes. The three-byte arm's payload went out high byte first, while the format (RFC 8878, and this crate's own `SequencesHeader` parser) reads it little-endian; only palindromic payloads survived. Both were silent stream corruption. The two-byte arm now ends at `0x7EFF`, the long form starts at `0x7F00` with the payload low byte first, and `seqnum_header_boundary_roundtrip` pins every arm edge through the parser.

### FrameCompressor reuse resets the frame entropy state

`FrameCompressor::compress`'s no-dictionary reset branch cleared the matcher, the Huffman table and `dict_entropy`, but left `fse_tables.*_previous`, `state.split` and the Huffman table's pool recycling unhandled, all three of which `reset_slice_state` owns on the slice path. A second frame on a reused compressor could then pass frame one's adopted tables into its first block's selection and emit `Repeat_Mode` with no established table, which both this decoder (`TableIsUninitialized`) and libzstd reject as corruption. The shared tail is now one function both resets call.

## Open findings

- The compat layer's bulk `Compressor` and `Decompressor` lack the parameter methods the zstd 0.13 crate gives them (`include_checksum`, `include_contentsize`, `window_log`, `multithread` on the encoder, `window_log_max` on the decoder); the compat stream types have them. The capability exists in `EncoderOptions`; only the bulk shapes are missing. Tracked in the [todo list](../todo.md).
- The compat write-side encoder's `flush()` before the first write is a no-op and later parameter setters keep succeeding, while the zstd crate's `flush()` engages the context and fails them with `stage_wrong`, and the compat read side's own `flush(out)` also engages. Parameter-timing divergence only; no output bytes differ.
- Compat encode-side dictionary validation stays deferred to the first compress call while the decode side validates at set time (documented at the setter, pinned by test).
- `set_dictionary` on the compat decode side is additive through the id-keyed map rather than replacing (documented divergence).
- The trainer entry points panic on output-write failures via `expect` while sibling read errors return `Result` (native API surface, not compat).
- `walk()` in the trainer swallows directory-listing errors into an empty sample set.
