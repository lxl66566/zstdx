# C FFI

`crates/zstdx-ffi` provides a C-ABI layer over the zstdx codec with drop-in libzstd compatibility as the target. This page answers which libzstd symbols the layer exports, how the ABI behaves at the edges, and where it deliberately deviates from `zstd.h`.

The crate's lib target is named `zstd`, so `cargo build -p zstdx-ffi --release` produces `target/release/libzstd.so` as a cdylib and `libzstd.a` as a staticlib directly. The `.so` carries no SONAME, so binaries linked against a versioned soname need a copy such as `cp libzstd.so libzstd.so.1`. All entry points are `#[no_mangle] extern "C"`. Contexts own the Rust-side state: the streaming encoder over its delivered-cursor sink, and a `FrameDecoder` with staged input, while the one-shot entries call the bulk slice core. The codec core stays untouched, and everything adapts at the boundary.

The claimed version surface is that of the reference tree this repository tracks, 1.6.0 / 10600. It covers `ZSTD_versionNumber`, `ZSTD_versionString`, `ZSTD_versionMajor`, `ZSTD_versionMinor` and `ZSTD_versionRelease`, `ZSTD_MAX_CLEVEL=22`, `ZSTD_minCLevel=-131072`, `ZSTD_defaultCLevel=3`, and `ZSTD_compressBound` verbatim with `bound(0)=64`, `bound(128K)=131584` and a `MAX_INPUT_SIZE` overflow returning 0. The stream size helpers are `CStreamIn=128K`, `CStreamOut=131591`, `DStreamIn=131075` and `DStreamOut=128K`. All of these are verified against the reference `libzstd.a` on this host.

## Symbol coverage

Implemented exports:

- Version and metadata: `ZSTD_versionNumber`, `ZSTD_versionString`, `ZSTD_versionMajor`, `ZSTD_versionMinor`, `ZSTD_versionRelease`, `ZSTD_maxCLevel`, `ZSTD_minCLevel`, `ZSTD_defaultCLevel`, `ZSTD_compressBound`, `ZSTD_getFrameContentSize`, `ZSTD_getDecompressedSize`, `ZSTD_findDecompressedSize`, `ZSTD_findFrameCompressedSize`, `ZSTD_frameHeaderSize`, `ZSTD_getFrameHeader` and `ZSTD_isFrame`.
- One-shot: `ZSTD_compress` and `ZSTD_decompress`.
- Errors: `ZSTD_isError`, `ZSTD_getErrorName` and `ZSTD_getErrorCode`. The enum mirrors `zstd_errors.h` and the names are `ERR_getErrorString`'s strings. Recognition uses `ERR_isError`'s strict bound, where the value `-(maxCode)` itself is not an error, and `getErrorCode` keeps the raw negated number for gap values inside the range but outside the enum, exactly as `ERR_getErrorCode` does.
- Contexts: `ZSTD_createCCtx`, `freeCCtx`, `ZSTD_createDCtx`, `freeDCtx`, `ZSTD_compressCCtx`, `ZSTD_decompressDCtx`, `ZSTD_compress_usingDict`, `ZSTD_decompress_usingDict`, `ZSTD_CCtx_setParameter`, `ZSTD_DCtx_setParameter`, `ZSTD_CCtx_setPledgedSrcSize`, `ZSTD_CCtx_reset`, `ZSTD_DCtx_reset`, `ZSTD_compress2`, `ZSTD_CCtx_loadDictionary` and `ZSTD_DCtx_loadDictionary`.
- Dictionaries: `ZSTD_createCDict`, `freeCDict`, `ZSTD_getDictID_fromCDict`, `ZSTD_compress_usingCDict`, `ZSTD_createDDict`, `freeDDict`, `ZSTD_getDictID_fromDDict`, `ZSTD_decompress_usingDDict`, `ZSTD_getDictID_fromDict` and `ZSTD_getDictID_fromFrame`.
- Streaming: `ZSTD_createCStream`, `freeCStream`, `ZSTD_createDStream`, `freeDStream`, `ZSTD_initCStream`, `ZSTD_initDStream`, `ZSTD_initDStream_usingDict`, `ZSTD_initDStream_usingDDict`, `ZSTD_resetDStream`, `ZSTD_compressStream2`, `ZSTD_compressStream`, `ZSTD_flushStream`, `ZSTD_endStream`, `ZSTD_decompressStream`, `ZSTD_CStreamInSize`, `ZSTD_CStreamOutSize`, `ZSTD_DStreamInSize`, `ZSTD_DStreamOutSize`, `ZSTD_inBuffer` and `ZSTD_outBuffer`.

The deprecated init family is implemented. Every entry returns `ZSTD_startingInputLength` of 5. `usingDict` resets and then loads, and its load error is the return value. `usingDDict` attaches the handle's parsed dictionary through an Arc clone, which strengthens the lifetime against the reference's caller-owned reference and avoids UB. `resetDStream` is a bare session reset that keeps the dictionary, where `initDStream` clears it. `ZSTD_compressStream` is a deprecated alias.

The parameters are `ZSTD_c_compressionLevel`, `ZSTD_c_windowLog`, `ZSTD_c_contentSizeFlag`, `ZSTD_c_checksumFlag`, `ZSTD_c_dictIDFlag`, `ZSTD_c_enableLongDistanceMatching`, `ZSTD_c_nbWorkers` and `ZSTD_d_windowLogMax`. `ZSTD_c_dictIDFlag` suppresses the frame header's Dictionary_ID while the dictionary stays in effect. `ZSTD_c_enableLongDistanceMatching` is a `ParamSwitch` tri-state over `EncoderOptions::long_distance_matching`. Everything else returns `ZSTD_error_parameter_unsupported`, which is libzstd's own answer for parameters it does not know.

### Parameter semantics

`ZSTD_CCtx_setParameter` returns the applied value on success. Levels clamp to [-131072, 22], where 0 means and returns the default 3. Boolean flags clamp to 0 or 1. `windowLog` validates 0 or 10 to 31, which is the 64-bit range. `ZSTD_DCtx_setParameter` returns 0 as it does there, with the reference dParam bounds for `ZSTD_d_windowLogMax` of `{0}` plus 10 to 31, and its mid-frame `stage_wrong` gate. `ZSTD_CCtx_setParameter` accepts the compression level mid-frame, the one knob of the reference's authorized update set that this layer implements. The level is stored then and applied to the next frame, while the reference retunes the running frame's blocks, as recorded below as a deviation. Every other parameter, unknown ids included, stays `stage_wrong` mid-frame.

`ZSTD_CCtx_reset` and `ZSTD_DCtx_reset` mirror the directive enum. Session resets never fail, parameter resets fail with `stage_wrong` while a frame is open, and unknown directive values are no-ops returning 0. `ZSTD_compress2` applies sticky settings plus the loaded dictionary over a fresh frame that pledges the source size.

`ZSTD_CCtx_setPledgedSrcSize` applies to the next frame only, where `ZSTD_CONTENTSIZE_UNKNOWN` means no pledge, and it is consumed when that frame completes. A frame opened by an `e_end` call overrides the pledge with that call's input size, which is libzstd's single-round rule and also the reason `ZSTD_compress2` writes the content size automatically.

`ZSTD_getFrameHeader` fills the `ZSTD_FrameHeader` struct for both frame types. Short valid prefixes return the wanted size, mismatching magic prefixes return `prefix_unknown`, the reserved descriptor bit returns `frameParameter_unsupported`, and an oversized window descriptor returns `frameParameter_windowTooLarge`. Its short-input contract matches the reference exactly, pinned by differential probes. Below the 5-byte `ZSTD_FRAMEHEADERSIZE_PREFIX`, the visible magic bytes are validated under the skippable-variant mask, where any first byte from 0x50 to 0x5F qualifies and a complete 4-byte magic of either family still wants 5 bytes rather than the family's own header size, and the struct is left untouched. From 5 bytes on, the struct is zeroed first, whole and including tail padding, because a Rust struct assignment would skip the padding bytes and the entry zeroes memory explicitly. Incomplete headers then want their descriptor-formula size. A null `src` with a positive size answers `generic`, which is the reference entry check, and size 0 still wants 5. `ZSTD_frameHeaderSize` performs the reference's pure descriptor arithmetic with no magic validation, so a skippable frame is measured through its own size field's first byte. `ZSTD_getDictID_fromFrame` returns the skippable magic variant for skippable frames, as the reference's struct field does.

### Dictionary handle caching

CDict and DDict caching is partial. The handles digest once: `ZSTD_createCDict` fails with NULL for an unloadable dictionary, and an empty dictionary stays a valid level-only handle as libzstd documents. Every compression or decompression reuses the parsed tables. Compression through the handle rides the pooled single-threaded dictionary state, whose seed-image cache restores the filled match state across same-key calls, so the CDict attach economics hold for size-varying records too. A key change in the dictionary, level, LDM mode or a decision-flipping size band re-seeds fully. Decode still builds a fresh decoder state per call.

`ZSTD_compress_usingCDict` takes the level and dictionary from the handle with libzstd's hardcoded frame parameters of contentSize on, checksum off and dictID on. A NULL handle reports `dictionary_wrong`. `ZSTD_decompress_usingDDict` uses the handle's dictionary exclusively, and a NULL handle decodes plain and ignores the context dictionary, as libzstd's `decompressBegin_usingDDict(NULL)` clears.

`ZSTD_CCtx_loadDictionary` defers the digest error to the next frame, which is libzstd's lazy CDict build. `ZSTD_DCtx_loadDictionary` parses eagerly and answers `dictionary_corrupted` at the load, where libzstd answers `memory_allocation` and the closer code wins. A loaded dictionary applies to every frame regardless of the header's dictID field, which is libzstd's rule. Both the one-shot paths through `DecoderOptions` and the streaming decoder register it under the null id, so a `ZSTD_c_dictIDFlag=0` frame decodes with the dictionary attached, covered by `suppressed_dictid_stream_decodes_with_loaded_dict`.

Not implemented, deliberately: the experimental and static-only surface beyond `ZSTD_getFrameHeader`, `frameHeaderSize` and `findDecompressedSize`. That covers `ZSTD_getBlockSize`, `ZSTD_compressBlock`, the deprecated `ZSTD_compress_advanced`, the `zstdmt` entries, inspect, `refPrefix`, `ZSTD_decompressBound`, `decompressionMargin` and the estimate entries. Language bindings, `-l` list mode and the benchmark and train CLI modes also remain open.

## Semantic deviations from zstd.h

### Levels, window and workers

Negative levels clamp to 1, because zstdx has no accelerated levels. `ZSTD_minCLevel` still reports -131072 so consumer-side range checks see the reference range.

`windowLog` above 27 is accepted but the engine caps it at 27. `ZSTD_c_windowLog` validates 10 to 31, the reference's 64-bit range, and the codec's own window ceiling is 2^27, so frames asking for 28 to 31 are produced with the capped window.

`nbWorkers` of 0 or 1 stays single-threaded, and 2 or more engages the MT job paths. A dictionary forces the single-threaded core, which is an engine limit and mirrors libzstd's no-MT-without-`ZSTD_MULTITHREAD` behavior rather than erroring. The reference `libzstd.a` on this host is built without `ZSTD_MULTITHREAD`, so it answers `parameter_unsupported` for `nbWorkers` at 1 or above, where this layer accepts and uses the value.

### Dictionary caching

The CDict caches the parse and, on the compression side, the re-armed match state per seeding key. The handle holds the dictionary digested once, with parse and encoder entropy tables shared by every job, and its compressions ride the pooled single-threaded dictionary state's seed-image cache. A same-key call restores the filled matcher tables, where the key covers the dictionary, level, LDM mode and a payload size inside one seeding projection class. A key change re-seeds. The DDict side still builds a fresh decoder state per call. Libzstd's CDict additionally shares the match state across threads, while the image here rides per-thread pooled states.

An unloadable formatted dictionary answers `dictionary_corrupted`, where the reference answers `memory_allocation`. This holds on the decode-side load through `ZSTD_DCtx_loadDictionary` and on the compression side when the lazily-digested dictionary first meets a frame, after `ZSTD_CCtx_loadDictionary` and then `compress2` or streaming. The reference's code is an allocation artifact of its CDict build path, and the closer code wins.

Sub-8-byte dictionaries mean "no dictionary" on both sides of this layer, following the compress rule in `ZSTD_loadDictionary`. The reference's decode side through `ZSTD_decompressBegin_usingDict` refs fewer than 8 bytes as raw content. That case is reachable only by a frame whose matches land inside a dictionary of 7 bytes or less, so it is recorded here rather than forked per side.

### One-shot entries

The one-shot variants run on fresh level-only settings. `ZSTD_compressCCtx` and `ZSTD_compress_usingDict` ignore the context's sticky parameters and dictionaries, as `zstd.h` documents, and `ZSTD_compress2` and the streaming entries are where stickiness applies. `ZSTD_decompressDCtx` does see the context dictionary, which is sticky per `zstd.h`.

One-shot compression rides the zero-copy bulk slice core with the source size declared. `ZSTD_compress` output stays byte-identical to `ZSTD_compressStream2` with `e_end` over the same bytes, and the equivalence is structural: `bulk::compress_with` threads the pledged size into every slice-path header with the streaming core's exact shape, pinned by `pledged_stream_matches_declared_bulk_st` across both header forms, the checksum and window-log variants, the reach-probe donated path and dictionary frames. A C-harness A/B/A measured 2026-10-16 covers json, text and dll32 at 32 MiB and 4K to 1M payloads at levels 1, 3 and 9 plus `compress2` at w8. Every output byte is identical and ST cells are wall-neutral, because a single whole-source `write_all` through the stream core already runs at the slice core's speed. The stream tax that the pull-shaped bench cells carry, where the encode streaming table's text.fastest stream runs at half of bulk, is a property of the chunked feed shape and not a cost the one-shot path shares. json `compress2` at w8 l1 is 5% to 8% faster, the one reproduced win. The single cell the slice core cannot express is `nbWorkers >= 2` with the content-size flag off, which keeps the streaming core, because the MT slice path always declares its size for the decoder's piece-partition policy. `ZSTD_compress2` rides the same entry with sticky settings and a pledged single round.

### Error and edge semantics

One-shot bad-magic answers follow the reference's decompressFrame gating exactly, pinned by differential probes. A garbage tail is judged only after the precondition of a visible header plus a block header, and the gate is descriptor-formula header size plus 3, which is 9 bare and 17 with an 8-byte FCS. Below the gate, and for any input shorter than the 5-byte prefix, the answer is the leftover-input `srcSize_wrong`. Past the gate, a garbage-only series answers `prefix_unknown`, while a decoded frame in front promotes even a passing tail to `srcSize_wrong` through the moreThan1Frame rule. Skippable frames skip past the promotion. Truncated input anywhere reports `srcSize_wrong`, a checksum mismatch reports `checksum_wrong`, and an undersized destination reports `dstSize_tooSmall`.

A failed context is sticky. Libzstd documents a failed `ZSTD_decompressStream` context as UB until reset, while this layer replays the recorded error code until `ZSTD_initCStream` or `ZSTD_initDStream` resets it.

`ZSTD_CCtx_setParameter` returns `stage_wrong` mid-frame except for the level. Libzstd authorizes block and level knobs for mid-frame updates through `ZSTD_isUpdateAuthorized`, and of that set this layer implements only `ZSTD_c_compressionLevel`, which is accepted and stored mid-frame. The level change applies to the next frame here, where the reference retunes the running frame's remaining blocks, which is an output-byte difference in the change-level-mid-frame corner only. Parameters unlock again after the frame closes or a reset.

Stream entries reject wild cursors like the reference. `compressStream2` answers `dstSize_tooSmall` for `output.pos > output.size`, then `srcSize_wrong` for `input.pos > input.size`, and `decompressStream` checks input first. The previous silent clamp is gone. A NULL context or buffer remains a defensive `generic` where the reference would crash, because it dereferences before any check.

`ZSTD_initCStream` and `ZSTD_initDStream` clear the context dictionary per their doc contract. `ZSTD_decompress_usingDict` leaves its dictionary loaded on the DCtx until then, and libzstd's decompose-segment does not retain it across calls.

NULL hygiene is defensive rather than UB, and decode-side reference-exact where the reference has an answer. On the decode entries `ZSTD_decompress`, `decompressDCtx`, `decompress_usingDict` and `decompress_usingDDict`, a null `dst` reads as zero capacity, so real output answers `dstSize_tooSmall` and an empty frame succeeds. A null or short `src` answers `srcSize_wrong` without dereferencing, because the reference's multi-frame loop needs 5 bytes before it touches the source. A null `src` with 5 or more claimed bytes, where the reference would dereference and hit UB, answers `srcSize_wrong` as the defense. The compress side keeps `srcBuffer_wrong` and `dstBuffer_null` for null buffers, where no reference answer exists because it writes. `free*(NULL)` returns 0, and empty one-shot input decodes to 0 bytes.

`ZSTD_isFrame` is the reference's pure magic check. It wants at least 4 bytes and either magic family, with no descriptor validation.

### Streaming contract

Streaming frames carry no pledged content size unless one is pledged. `setPledgedSrcSize` is honored for the next frame. Frames opened by chunked `e_continue` calls declare nothing, and frames opened by an `e_end` call carry their input size, which is the same split as libzstd.

Streaming decode is single-threaded, as in libzstd's stable API, where decode threads are an engine capability without a stable libzstd knob.

The no-forward-progress guard is the reference's. It answers `noForwardProgress_destFull` with code 80 when 16 consecutive calls move neither cursor and the output is full, and `noForwardProgress_inputEmpty` with code 82 otherwise. Any progress, a session reset or `initDStream` restarts the count, and a `ZSTD_DStream` is its `ZSTD_DCtx`, so the reference's `ZSTD_decompressBegin` under every session reset clears the counter. The between-frames state is exempt, because the reference's `loadHeader` shortcut returns its hint before the guard. After the completion answer 0 went out, further calls ask for the next frame's header with 9, which is `max(FRAMEHEADERSIZE_MIN, hSize) - staged + blockHeaderSize`, and never count toward the guard. `compressStream2` has no guard on either side.

Streaming staging is block-granular where the reference decodes ahead, a recorded deviation. With an unwritable output of zero capacity, this layer consumes no input and the guard fires on call 16, where the reference keeps consuming into its internal buffers, hostage byte included, and fires on 17. Mid-frame with partial block input, the reference may already deliver output from a small first block where this layer waits for the block it can decode whole. Error codes, final output and the completion contract are identical, and the divergence is confined to transient hint and progress shapes.

## Buffer and flush contracts

Buffer-advance and flush contracts match `zstd.h` exactly and are pinned by tests. Both `pos` fields advance monotonically, input may be left unconsumed when the output fills, and `ZSTD_e_flush` and `ZSTD_e_end` return 0 only when everything is flushed, driven through 1-byte output buffers in both the Rust and C suites. `ZSTD_decompressStream` returns 0 only when a frame is fully decoded and flushed with all input consumed, once per finished frame. The next call then asks 9 for the next frame's header, as the reference does, and a hint of one block is returned otherwise. Streaming output is deterministic across buffer chunkings because the encoder never sees them, and multi-frame streams with skippable frames decode transparently.

## Harness

`crates/zstdx-ffi/tests/ffi.rs` drives the exported ABI from Rust. It covers levels 0 to 22, tiny-buffer streaming, flush-to-zero, multi-frame and skippable decode, error-code mapping, dictionary roundtrip, parameter bounds, the reset family, `compress2`, pledged sizes, CDict and DDict reuse and equivalence with `usingDict`, loadDictionary stickiness, getDictID flows over a real trained dictionary when the tree carries one, and the metadata size entries. `tests/run-c.sh` compiles `tests/harness.c` twice, once against the reference `libzstd.a` and once against the built `libzstd.so`, and runs the self-tests on both. The reference run is a parity gate, so harness expectations that drift from libzstd fail there first. The script then runs the interop matrix, reference to self and self to reference at levels 1, 9 and 19 on json and text corpus slices, plus the zstd CLI decoding a self-produced frame and the reverse.

The suites are green with 65 symbols. The second review round added the bad-magic gating matrix, the null-buffer family, the deprecated init entries and the no-forward-progress guard to both suites.

## Drop-in smoke

As a stretch check, the dynamically linked system `zstd` CLI at 1.5.7 loads our preloaded `libzstd.so` cleanly, then refuses to operate because its build-time version guard rejects any library that does not match its own version string, with the message "expecting 1.5.7; actual 1.6.0". The claimed 1.6.0 version is the tracked reference tree's, and the drop-in target is a consumer without that guard.
