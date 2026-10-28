# Peripheral review

This page records the 2026-10-25 review round over the modules without a dedicated review record: the streaming cores and `bulk.rs`, the encoder's non-matcher modules (`frame_compressor`, `block_enc`, entropy emission), and the compat, dictionary-trainer and option surfaces. The method was a three-way read with each finding re-verified against the live code paths before landing. Performance, decoder entropy and matcher behavior were out of scope; those live in their own pages.

## Landed fixes

### nbSeq long-form emission

`encode_seqnum` carried two defects in the counts-above-32511 domain, reachable whenever one block packs more than 0x7F00 sequences (a 128 KiB block admits up to about 43690 at three bytes per sequence). The two-byte arm extended to `0x7FFF`, but its high byte `(n >> 8) | 0x80` becomes `0xFF` — the long-form marker — at `0x7F00`, so counts in [0x7F00, 0x7FFF] serialized a two-byte header that every parser reads as four bytes. The three-byte arm's payload went out high byte first, while the format (RFC 8878, and this crate's own `SequencesHeader` parser) reads it little-endian; only palindromic payloads survived. Both were silent stream corruption. The two-byte arm now ends at `0x7EFF`, the long form starts at `0x7F00` with the payload low byte first, and `seqnum_header_boundary_roundtrip` pins every arm edge through the parser.

### FrameCompressor reuse resets the frame entropy state

`FrameCompressor::compress`'s no-dictionary reset branch cleared the matcher, the Huffman table and `dict_entropy`, but left `fse_tables.*_previous`, `state.split` and the Huffman table's pool recycling unhandled, all three of which `reset_slice_state` owns on the slice path. A second frame on a reused compressor could then pass frame one's adopted tables into its first block's selection and emit `Repeat_Mode` with no established table, which both this decoder (`TableIsUninitialized`) and libzstd reject as corruption. The shared tail is now `reset_entropy_state`, which both resets call, and `entropy_state_two_frames_reused_compressor` pins the reuse at a compressed level against decode and fresh-compressor bytes.

### Sequential bulk decode honors the window bound

The sequential and dictionary arms of `decompress_with` and `decompress_to_buffer_with` built a bare `FrameDecoder`, so `DecoderOptions::max_window_size` bound only the multithreaded arm. `decoder_from_options` now configures the window bound and the dictionary for every arm, and the three copies of the doubling retry loop (`bulk::decompress`, the dictionary arms, compat `Decompressor::decompress`) collapsed into `decode_all_to_vec_growing`, which stops at half of `isize::MAX` and reports `TargetTooSmall` instead of wrapping the doubling on 32-bit. `sequential_decode_honors_max_window_size` pins the rejection and the still-decoding bound.

### Width-independent size bounds

Two 32-bit overflows closed. `upper_bound`'s skippable-frame guard compared `8 + length` in usize, which wraps on i386 for a length near `u32::MAX` and would admit a truncated skippable frame as complete; the comparison runs in u64 now. The MT encoder's `initial_reserve` multiplied its job-size terms in usize, wrapping before the `BUF_CAP_MAX` clamp on 32-bit worker counts; the sum runs in u64 and clamps in u64.

### window_log_max validates; workers clamps

Both compat stream sides enforce libzstd's `ZSTD_d_windowLogMax` bounds (10..=31) through `ParameterError::WindowLogMaxOutOfRange`; the write side also gained the `started` flag, so its setter fails with `AlreadyStreaming` after the first write, matching the read side's lazy start. `EncoderOptions::workers` clamps to `MAX_WORKERS = 256`, `ZSTDMT_NBWORKERS_MAX` parity.

### The finished guard covers both pump arms

`pump_from` checked `is_finished`, but `pump`'s MT dispatch bypassed the check, so a finished MT core could re-enter `pump_direct` and trip its entry assert. The guard sits in `pump` ahead of both arms, and `pump_from` (whose only caller is `pump`) dropped its own copy.

### compat bulk parameter methods

The bulk `Compressor` and `Decompressor` carry the zstd 0.13 parameter methods (todo 27). `include_checksum`, `include_contentsize`, `window_log` and `multithread` live on the compressor, `window_log_max` on the decompressor, all routing into the existing option structs. A bulk context is job-shaped, so each setter applies to the next job with no started-state refusal, and the parameters survive a dictionary re-register like the dictIDFlag already did. `include_contentsize(false)` degrades the always-on bulk pledge to an input-shape hint, matching the stream encoders' materialize mapping, so the header keeps no content-size field while the length still sizes the frame. `window_log` feeds `InputShape::window_log` under the engine's 10..=27 clamp, and `multithread` above one rides the native MT bulk entry, where the small-input and dict-id-suppressed engagement demotions apply as on the native path. The `window_log_max` range check moved from `stream.rs` into the shared compat module and serves the stream decoders and the bulk setter. A re-audit against the vendored zstd 0.13.3 found no other non-experimental gap: the remaining exclusions are `set_target_cblock_size` (todo 18) and the zstd_safe-passthrough family (`set_parameter`, `context_mut`, `with_context`, `with_writer`, `with_encoder`, `with_decoder`, the borrowing dictionary constructors).

## Open findings

- The compat write-side encoder's `flush()` before the first write is a no-op and later parameter setters keep succeeding, while the zstd crate's `flush()` engages the context and fails them with `stage_wrong`, and the compat read side's own `flush(out)` also engages. Parameter-timing divergence only; no output bytes differ.
- Compat encode-side dictionary validation stays deferred to the first compress call while the decode side validates at set time (documented at the setter, pinned by test).
- `set_dictionary` on the compat decode side is additive through the id-keyed map rather than replacing (documented divergence).
- The trainer entry points panic on output-write failures via `expect` while sibling read errors return `Result` (native API surface, not compat).
- `walk()` in the trainer swallows directory-listing errors into an empty sample set.
