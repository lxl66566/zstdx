# C FFI (`crates/zstdx-ffi`)

A C-ABI layer over the zstdx codec targeting drop-in libzstd compatibility. The crate's lib target is named `zstd`, so `cargo build -p zstdx-ffi --release` produces `target/release/libzstd.so` (cdylib) and `libzstd.a` (staticlib) directly. The `.so` carries no SONAME; to satisfy binaries linked against a versioned soname, copy it (`cp libzstd.so libzstd.so.1`). All entry points are `#[no_mangle] extern "C"`; contexts own the Rust-side state (an encoder over a staging sink, a `FrameDecoder` with staged input) and the codec core is untouched — everything adapts at the boundary.

The claimed version surface is that of the reference tree this repo tracks (1.6.0 / 10600): `ZSTD_versionNumber/String/Major/Minor/Release`, `ZSTD_MAX_CLEVEL=22`, `ZSTD_minCLevel=-131072`, `ZSTD_defaultCLevel=3`, `ZSTD_compressBound` verbatim (`bound(0)=64`, `bound(128K)=131584`, `MAX_INPUT_SIZE` overflow → 0), stream size helpers (`CStreamIn=128K`, `CStreamOut=131591`, `DStreamIn=131075`, `DStreamOut=128K`) — all verified against the reference `libzstd.a` on this host.

## Symbol coverage

Implemented (42 exports):

- Version/metadata: `ZSTD_versionNumber/versionString/versionMajor/versionMinor/versionRelease`, `ZSTD_maxCLevel/minCLevel/defaultCLevel`, `ZSTD_compressBound`, `ZSTD_getFrameContentSize`, `ZSTD_findFrameCompressedSize`, `ZSTD_isFrame`
- One-shot: `ZSTD_compress`, `ZSTD_decompress`
- Errors: `ZSTD_isError`, `ZSTD_getErrorName`, `ZSTD_getErrorCode` (enum mirrors `zstd_errors.h`; names are `ERR_getErrorString`'s strings)
- Contexts: `ZSTD_createCCtx/freeCCtx`, `ZSTD_createDCtx/freeDCtx`, `ZSTD_compressCCtx`, `ZSTD_decompressDCtx`, `ZSTD_compress_usingDict`, `ZSTD_decompress_usingDict`, `ZSTD_CCtx_setParameter`, `ZSTD_DCtx_setParameter`
- Streaming: `ZSTD_createCStream/freeCStream`, `ZSTD_createDStream/freeDStream`, `ZSTD_initCStream/initDStream`, `ZSTD_compressStream2`, `ZSTD_compressStream` (deprecated alias), `ZSTD_flushStream`, `ZSTD_endStream`, `ZSTD_decompressStream`, `ZSTD_CStreamInSize/OutSize`, `ZSTD_DStreamInSize/OutSize`, `ZSTD_inBuffer`/`ZSTD_outBuffer`

Parameters: `ZSTD_c_compressionLevel`, `ZSTD_c_windowLog`, `ZSTD_c_contentSizeFlag`, `ZSTD_c_checksumFlag`, `ZSTD_c_nbWorkers`, `ZSTD_d_windowLogMax`. Everything else returns `ZSTD_error_parameter_unsupported` (libzstd's own answer for parameters it does not know).

Deferred (deliberately out of scope this round): `ZSTD_compress2`, `ZSTD_CCtx/DCtx_reset`, `ZSTD_CCtx_setPledgedSrcSize`, the `CDict/DDict` digest family, `ZSTD_{C,D}Ctx_loadDictionary`, `ZSTD_getDictID_*`, `ZSTD_getFrameHeader` and the inspect/`zstdmt`/experimental surfaces, `ZSTD_getDecompressedSize`/`findDecompressedSize` (multi-frame size queries), `ZSTD_compress_advanced` (deprecated).

## Semantic deviations from zstd.h

- **Negative levels clamp to 1.** zstdx has no accelerated levels; `ZSTD_minCLevel` still reports -131072 so consumer-side range checks see the reference range.
- **windowLog above 27 is accepted but engine-capped at 27** (`ZSTD_c_windowLog` validates 10..=30 like libzstd; the codec's own window ceiling is 2^27). Frames asking 28..=30 are produced with the capped window.
- **nbWorkers: 0/1 single-threaded, ≥2 engages the MT job paths**; a dictionary forces the single-threaded core (engine limit — mirrors libzstd's no-mt-without-ZSTD_MULTITHREAD behavior rather than erroring).
- **One-shot compression rides the streaming encoder core with a pledged size** (the only public path that writes the frame content size into the header, as libzstd does by default): `ZSTD_compress` output is byte-identical to `ZSTD_compressStream2(…, e_end)` over the same bytes, not to zstdx's own headerless bulk frames. Cost: the stream core's staging instead of the zero-copy slice path — the known stream-encode speed class; a bulk entry with a content-size flag is the follow-up if the one-shot path needs the slice core's speed.
- **Bad-magic error codes match libzstd per entry point**: one-shot reports `srcSize_wrong` (verified against the reference library), the streaming path reports `prefix_unknown`. Truncated input anywhere → `srcSize_wrong`; checksum mismatch → `checksum_wrong`; undersized destination → `dstSize_tooSmall`.
- **A failed context is sticky, not undefined**: libzstd documents a failed `ZSTD_decompressStream` context as UB-until-reset; this layer replays the recorded error code until `ZSTD_initCStream`/`ZSTD_initDStream` reset it.
- **`ZSTD_CCtx_setParameter` mid-frame returns `stage_wrong`** (libzstd rejects most parameters once a frame is open; parameters unlock again after the frame closes or a reset).
- **`ZSTD_initCStream`/`ZSTD_initDStream` clear the context dictionary** per their doc contract; `ZSTD_decompress_usingDict` leaves its dictionary loaded on the DCtx until then.
- **NULL hygiene is defensive, not UB**: null `src` with positive length → `srcBuffer_wrong`, null `dst` with positive capacity → `dstBuffer_null` (libzstd would read/write them); `free*(NULL)` → 0; empty one-shot input decodes to 0 bytes (libzstd's multi-frame loop treats it as no frames).
- **`ZSTD_isFrame` is the reference's pure magic check** (≥4 bytes, either magic family; no descriptor validation).
- **Streaming frames carry no pledged content size** (`setPledgedSrcSize` is not implemented), so `ZSTD_getFrameContentSize` on them reports `CONTENTSIZE_UNKNOWN` — the same as libzstd without a pledge.
- **Streaming decode is single-threaded**, as in libzstd's stable API (decode threads are an engine capability without a stable libzstd knob).

Buffer-advance and flush contracts match zstd.h exactly and are pinned by tests: both `pos` fields advance monotonically, input may be left unconsumed when the output fills, `ZSTD_e_flush`/`ZSTD_e_end` return 0 only when everything is flushed (driven through 1-byte output buffers in both the Rust and C suites), `ZSTD_decompressStream` returns 0 only when a frame is fully decoded and flushed with all input consumed, and a hint of one block otherwise. Streaming output is deterministic across buffer chunkings (the encoder never sees them); multi-frame streams with skippable frames decode transparently.

## Harness

`crates/zstdx-ffi/tests/ffi.rs` drives the exported ABI from Rust (levels 0..22, tiny-buffer streaming, flush-to-zero, multi-frame + skippable decode, error-code mapping, dictionary roundtrip, parameter bounds). `tests/run-c.sh` compiles `tests/harness.c` twice — once against the reference `libzstd.a`, once against the built `libzstd.so` — runs the self-tests on ours, then the interop matrix (ref→self and self→ref at levels 1/9/19 on json/text corpus slices, plus the zstd CLI decoding a self-produced frame and vice versa). Both suites were green on landing (2026-10-01).
