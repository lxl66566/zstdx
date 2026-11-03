# FFI parity

This page records the C ABI comparison between `crates/zstdx-ffi` and libzstd 1.6.0. It answers which symbols the layer covers, which reported differences are fixed, which are deliberate, and which areas remain unprobed. The comparison covers the whole exported C ABI against the tracked reference tree at `/root/programs/fork/zstd`, with `libzstd.a` built on this host at the x86-64 baseline march.

## Method

Differential C probes are compiled once against the reference and once against the built `libzstd.so`, and their outputs are byte-diffed. Every pinned expectation is first verified on the reference. `tests/run-c.sh` runs the same harness against `libzstd.a` as a parity gate.

The streaming decode family is pinned by a transcript grid: embedded reference-built frames (pledged, unknown-size, checksummed, multi-block, raw, RLE, multi-frame, skippable-only and truncated or corrupt edits) driven through fixed input-chunk and output-capacity shapes, with every call's (return, input.pos, output.pos, error) compared cell by cell against the reference record, 41 cells and 2248 calls at close. The bounds, cParams, estimate and frame-walk families were pinned by a golden grid: a C probe printing every cell (the full bounds matrices, the level rows over 31 levels by 16 size hints by 9 dictionary sizes, the adjust and check matrices, the estimates over levels, cParams grids, dictionary sizes and load methods, and the frame-walk answers over real, truncated and garbage frames) run against both libraries and byte-diffed. The grid is byte-identical except where a cell embeds this codec's own compressed output as input, where the derived answers were compared field by field. The estimate constants were calibrated against the same probe: the struct sizes were read from the reference headers directly and the private `CDict`/`DDict` sizes solved from reference estimate answers over rows whose every other term is hand-computable.

## Symbol coverage

The layer covers 106 of the 152 `ZSTDLIB_API` and `ZSTDLIB_STATIC_API` symbols that the reference header declares. Coverage was probed with `nm` on both builds and diffed against the `ZSTDLIB_*_API` declarations in `zstd.h`.

- Version and metadata: complete for what consumers call, with `versionNumber/String/Major/Minor/Release`, `maxCLevel/minCLevel/defaultCLevel`, `compressBound`, `getFrameContentSize`, `getDecompressedSize`, `findDecompressedSize`, `findFrameCompressedSize`, `frameHeaderSize`, `getFrameHeader` and `isFrame`.
- One-shot: `compress`, `decompress`, and the context and dictionary variants `compressCCtx`, `decompressDCtx`, `compress_usingDict/decompress_usingDict`, `compress_usingCDict/decompress_usingDDict` and `compress2`.
- Errors: `isError`, `getErrorName`, `getErrorCode`. Error-code fidelity is reference-exact: the enum mirrors `zstd_errors.h`, `isError` is `ERR_isError`'s strict bound where `-(maxCode)` itself is not an error, and `getErrorCode` keeps raw negated gap codes.
- Contexts and parameters: create, free and reset on both sides, `setParameter`/`loadDictionary`/`setPledgedSrcSize` on both sides, and the `ZSTD_dParameter` bounds.
- Dictionaries: CDict and DDict create and free, the four `getDictID_from*` entries, `createCDict` and `createDDict`.
- Streaming: create, free and init on both sides, including the deprecated init family, `compressStream2`, `compressStream`/`flushStream`/`endStream`, `decompressStream`, and the four size helpers.
- Bounds and readback: `cParam_getBounds`/`dParam_getBounds` over the full id space, `CCtx_getParameter`/`DCtx_getParameter`, `DCtx_setMaxWindowSize` and `getBlockSize`.
- cParams helpers: `getCParams`/`getParams`/`adjustCParams`/`checkCParams`, ported formula-exact from the reference level table.
- Estimates: the ten `estimate*` entries over the level rows and raw cParams, plus `decodingBufferSize_min` and `sequenceBound`.
- Frame walk and skippable payload: `decompressBound`, `decompressionMargin`, `writeSkippableFrame`, `readSkippableFrame`, `isSkippableFrame`, `isDeterministicBuild` and `getFrameHeader_advanced` for the `zstd1` format.
- Prefix and dictionary references: `CCtx_refPrefix`/`_advanced`, `DCtx_refPrefix`/`_advanced`, `DCtx_refDDict`, the `loadDictionary_byReference`/`_advanced` variants on both sides and the `createCDict_byReference`/`createDDict_byReference`/`createDDict_advanced` constructors.

The remainder, 46 symbols, is deliberately uncovered:
- The `initStatic*` family (6): the contexts are heap objects; there is no caller-provided workspace to init into.
- The legacy push API (11: `compressBegin` and kin, `decompressBegin`/`decompressContinue`, `nextInputType`, `nextSrcSizeToDecompress`): a second streaming engine around the same core.
- The `ZSTD_CCtx_params` bag and its users (12: `createCCtxParams`, `freeCCtxParams`, the `CCtxParams_*` setters, `CCtx_setParams`/`setCParams`/`setFParams`/`setParametersUsingCCtxParams`, `estimateCCtxSize_usingCCtxParams`, `estimateCStreamSize_usingCCtxParams`): the bag accepts engine knobs (hashLog through strategy, the LDM set) that the codec does not expose, so adopting one could only silently ignore it and change the frame bytes against the reference.
- Constructors with explicit cParams, params or custom allocators (6: `createCCtx_advanced`, `createCDict_advanced`, `createCDict_advanced2`, the `*_advanced` stream constructors): the same knob problem; `createDDict_advanced` is in and refuses a non-default `customMem`, since caller allocation has no hook here.
- `CCtx_refCDict` and the thread pool (4): the attach-dict parameter precedence and worker-pool plumbing have no engine counterpart.
- Block-level decode (2: `decompressBlock`, `mergeBlockDelimiters`) and introspection (8: `getFrameProgression`, `toFlushNow`, the six `sizeof_*`): the first need sequence-store level access; the introspection entries report the reference's object sizes, which are not this implementation's.
- `copyDCtx` (1): a deprecated field-range copy between reference-layout objects.

Each family needs its own parity round if a consumer calls it, and none is reachable through the stable `zstd` crate bindings.

## Divergences fixed

The one-shot bad-magic gating is now reference-exact. The reference's `ZSTD_decompressMultiFrame` enters `ZSTD_decompressFrame` only above the 5-byte prefix. It believes a garbage magic only after the precondition of descriptor-formula header size plus block header, which is 9 bytes bare and 17 with an 8-byte FCS. `ZSTD_frameHeaderSize_internal` does not validate the magic, which is judged in `ZSTD_decodeFrameHeader` after both gates. A real decoded frame in front promotes even a passing tail to `srcSize_wrong` through the `moreThan1Frame` rule, and skippable frames `continue` past that flag and never promote. The full matrix covered garbage-only, frame plus garbage and skippable plus garbage, two descriptor shapes and tail lengths from 0 to 19, and it is probe-identical. The behavior is pinned in `harness.c`, which runs on `libzstd.a` first, and in `tests/ffi.rs`.

Null and truncated buffers on the decode entries are fixed. A null `dst` reads as zero capacity, which gives `dstSize_tooSmall` for real output and success for an empty frame. The reference was probed and never touches `dst` on those paths. A null or short `src` below the prefix gives the no-deref `srcSize_wrong`. The engine's `MagicNumberReadError` on a walk tail under 5 bytes follows the same leftover-input rule.

`ZSTD_initDStream` now returns `ZSTD_startingInputLength`, which is 5. An existing stale pin asserted `ZSTD_DStreamInSize` and was corrected.

The deprecated DStream init family now behaves like the reference. `initDStream_usingDict` resets and loads, and the load error is the return. `initDStream_usingDDict` attaches the parsed handle dictionary. `resetDStream` performs a bare session reset, and the dictionary survives. All three return 5.

The no-forward-progress guard matches the reference. Sixteen consecutive no-progress `decompressStream` calls answer `noForwardProgress_destFull` (80) with a full output, and otherwise `noForwardProgress_inputEmpty` (82). Progress or any session reset restarts the count. A `ZSTD_DStream` is its `ZSTD_DCtx`, and the reference's `ZSTD_decompressBegin` under session resets clears the counter, which the earlier implementation missed on `initDStream`. The between-frames state is exempt exactly as the reference's `loadHeader` shortcut, whose hint return sits before the guard. After the completion 0 went out, further calls ask 9, which is `max(FRAMEHEADERSIZE_MIN, hSize) - staged + blockHeaderSize`. `compressStream2` has no guard on either side, probed on both.

The streaming decode machine is now a port of the reference's `ZSTD_decompressStream` stage engine, and the `destFull` and hint deviations are closed. Input is claimed piece by piece (frame header chunks, 3-byte block headers, whole compressed or RLE bodies, 4-byte checksum trailers, skippable content), every completed piece is processed at once, and decoded output is released one flush round per block: a round that cannot empty the pending buffer ends the call, exactly where the reference stops. Raw bodies stream piecewise, releasing every byte that arrives. A frame whose output is still pending at its end holds back one input byte (the reference's hostage) until the flush completes, a pledged frame that fits the caller's buffer and arrives whole takes the single-pass shortcut, and every frame ends its own call with the answer 0, skippable frames included. The return value is the reference's exact `nextSrcSizeHint` arithmetic, including the block-header preload on non-last bodies, the staged-prefix subtraction and the `max(6, hSize) - staged + 3` header formula for partial next-frame prefixes. With an unwritable output the first call still consumes and decodes one block, so the no-forward-progress guard fires `destFull` on call 17, the reference's count. Error paths leave the caller's cursors untouched, the one exception being the partial-header magic rejection, which sits behind the reference's `input->pos = input->size` commit. The whole surface is pinned by the transcript grid above, and the post-error replay after a bad magic matches the reference's staged-garbage replay; the reference's mid-machine continuation after body and checksum errors stays out of the record as UB, with this layer's sticky replay unchanged.

The earlier commits fixed several contracts. Commit 4fdc4872 fixed `getFrameHeader`'s short-input and struct-write contract, including the 48-byte struct's tail padding. Commit 68395088 enforced the strict maxCode bound and kept the raw gap codes. Commit 136a2758 fixed the parameter bounds, where `d_windowLogMax` is the union of `{0}` and 10..=31 and 32..=41 answers `parameter_outOfBound`, probed identical on both libs with no deviation remaining. It also fixed the mid-frame `stage_wrong` gates, the wild-cursor checks and the mid-frame cParam whitelist.

## Recorded deviations

- A corrupt formatted dictionary at load answers `dictionary_corrupted` (30), where the reference answers `memory_allocation` (64). This was probed on `ZSTD_DCtx_loadDictionary` and on `ZSTD_CCtx_loadDictionary` with a first frame. The reference's code is an allocation artifact of its CDict and DDict build path, so the code that names the actual cause wins.
- NULL-pointer defense covers cases where the reference has UB. A null `src` with at least 5 claimed bytes on a decode entry answers `srcSize_wrong`, where the reference derefs. Null buffers on compress entries answer `srcBuffer_wrong` and `dstBuffer_null`, where the reference writes. Both are documented in `ffi.md`.
- One-shot compress accepts an exact-fit capacity. The reference's `ZSTD_compress` refuses a `dstCapacity` in `[used, used+8)` with `dstSize_tooSmall` even though the output fits. A probe with used=276 accepted the first capacity at 284, an internal workspace-margin artifact. This layer accepts the exact fit. The earlier "noise-cap anomaly" is this case, now boundary-pinned.
- The wild-cursor clamp is fixed. `compressStream2` and `decompressStream` now answer `dstSize_tooSmall` and `srcSize_wrong` for `pos > size` in the reference's check order. The earlier behavior was a silent clamp, and the fix is in 136a2758.
- `initDStream_usingDDict` has a strengthened lifetime. The handle's dictionary is Arc-cloned into the context, where the reference keeps a caller-owned pointer that dangles if freed early. This layer cannot dangle.
- Pre-existing behavior, documented in `ffi.md`: negative levels clamp to 1, windowLog above 27 is engine-capped, nbWorkers semantics apply, and one-shot variants use fresh settings.
- Also pre-existing and documented in `ffi.md`: a failed stream replays its error where the reference has UB, `setParameter` level mid-frame applies to the next frame where the reference retunes the running one, and sub-8-byte dictionaries mean no dictionary on both sides.

## Unprobed remainder

- The deliberately uncovered symbol families above, namely estimate, static, advanced, ref, block-level and push-API entries, have no behavior probes. Their coverage is a named gap, not a verified one.
- `ZSTD_decompressContinue`'s own no-forward-progress counter, `dctx->noForwardProgress`, is moot while the push API is unimplemented.
- Legacy-format frames on the `ZSTD_isLegacy` paths are unsupported by this codec, and no probe exercised legacy magic on the decode entries.

- The estimate family, the cParams helpers, the bounds matrices, the readback entries, the frame-walk entries, the skippable payload I/O and the prefix family landed reference-exact, pinned by the golden grid and the extended harness. The decode-side dictionary re-arm at frame boundaries (a prefix, a load or a `refDDict` between frames now applies to the next frame of a live stream) came out of that round; the earlier behavior ignored dictionary changes on a live stream.

## Commits

- 4fdc4872 fixes `getFrameHeader`'s short-input and struct-write contract, including the 48-byte struct's tail padding
- 68395088 enforces the strict maxCode bound and keeps the raw gap codes
- 136a2758 fixes the parameter bounds, the mid-frame `stage_wrong` gates, the wild-cursor checks and the mid-frame cParam whitelist
- b28e8b68 fixes the one-shot bad-magic gating, null and truncated decode buffers, the `initDStream` return, the deprecated DStream init family and the no-forward-progress guard
- dd4df0ac ports the reference's `ZSTD_decompressStream` stage engine into the FFI streaming shim, closing the `destFull` call-count and hint deviations
- (this commit) pins the streaming surface with the transcript grid: 41 cells over embedded reference-built frames, 2248 calls compared cell by cell on both libraries

## Verification

Every pinned expectation was first verified on the reference. `tests/run-c.sh` runs the same harness against `libzstd.a` as a parity gate, and the probe outputs are byte-diffed between the reference build and the built `libzstd.so`.
