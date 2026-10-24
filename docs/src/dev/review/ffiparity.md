# FFI parity

This page records the C ABI comparison between `crates/zstdx-ffi` and libzstd 1.6.0. It answers which symbols the layer covers, which reported differences are fixed, which are deliberate, and which areas remain unprobed. The comparison covers the whole exported C ABI against the tracked reference tree at `/root/programs/fork/zstd`, with `libzstd.a` built on this host.

## Method

Differential C probes are compiled once against the reference and once against the built `libzstd.so`, and their outputs are byte-diffed. Every pinned expectation is first verified on the reference. `tests/run-c.sh` runs the same harness against `libzstd.a` as a parity gate.

## Symbol coverage

The layer covers 65 of the 152 `ZSTDLIB_API` and `ZSTDLIB_STATIC_API` symbols that the reference header declares. Coverage was probed with `nm` on both builds and diffed against the `ZSTDLIB_*_API` declarations in `zstd.h`.

- Version and metadata: complete for what consumers call, with `versionNumber/String/Major/Minor/Release`, `maxCLevel/minCLevel/defaultCLevel`, `compressBound`, `getFrameContentSize`, `getDecompressedSize`, `findDecompressedSize`, `findFrameCompressedSize`, `frameHeaderSize`, `getFrameHeader` and `isFrame`.
- One-shot: `compress`, `decompress`, and the context and dictionary variants `compressCCtx`, `decompressDCtx`, `compress_usingDict/decompress_usingDict`, `compress_usingCDict/decompress_usingDDict` and `compress2`.
- Errors: `isError`, `getErrorName`, `getErrorCode`. Error-code fidelity is reference-exact: the enum mirrors `zstd_errors.h`, `isError` is `ERR_isError`'s strict bound where `-(maxCode)` itself is not an error, and `getErrorCode` keeps raw negated gap codes.
- Contexts and parameters: create, free and reset on both sides, `setParameter`/`loadDictionary`/`setPledgedSrcSize` on both sides, and the `ZSTD_dParameter` bounds.
- Dictionaries: CDict and DDict create and free, the four `getDictID_from*` entries, `createCDict` and `createDDict`.
- Streaming: create, free and init on both sides, including the deprecated init family, `compressStream2`, `compressStream`/`flushStream`/`endStream`, `decompressStream`, and the four size helpers.

The remainder is deliberately uncovered. It holds `estimate*` with 14 entries, `initStatic*` with 6 entries, the `*_advanced` and `*_byReference` context and dictionary constructors, `refPrefix`/`refCDict`/`refDDict`/`refThreadPool`, the block-level API of `compressBlock`/`decompressBlock`/`insertBlock`/`getcBlockSize`, the legacy push API of `decompressContinue`/`decompressBegin`, `copyCCtx`/`copyDCtx`, `writeSkippableFrame`/`readSkippableFrame`, `decompressStream_simpleArgs`/`compressStream2_simpleArgs`, the thread-pool and inspect entries, and `ZSTD_getCParams`/`getParams`/`adjustCParams`/`checkCParams`. Each family needs its own parity round if a consumer calls it, and none is reachable through the stable `zstd` crate bindings.

## Divergences fixed

The one-shot bad-magic gating is now reference-exact. The reference's `ZSTD_decompressMultiFrame` enters `ZSTD_decompressFrame` only above the 5-byte prefix. It believes a garbage magic only after the precondition of descriptor-formula header size plus block header, which is 9 bytes bare and 17 with an 8-byte FCS. `ZSTD_frameHeaderSize_internal` does not validate the magic, which is judged in `ZSTD_decodeFrameHeader` after both gates. A real decoded frame in front promotes even a passing tail to `srcSize_wrong` through the `moreThan1Frame` rule, and skippable frames `continue` past that flag and never promote. The full matrix covered garbage-only, frame plus garbage and skippable plus garbage, two descriptor shapes and tail lengths from 0 to 19, and it is probe-identical. The behavior is pinned in `harness.c`, which runs on `libzstd.a` first, and in `tests/ffi.rs`.

Null and truncated buffers on the decode entries are fixed. A null `dst` reads as zero capacity, which gives `dstSize_tooSmall` for real output and success for an empty frame. The reference was probed and never touches `dst` on those paths. A null or short `src` below the prefix gives the no-deref `srcSize_wrong`. The engine's `MagicNumberReadError` on a walk tail under 5 bytes follows the same leftover-input rule.

`ZSTD_initDStream` now returns `ZSTD_startingInputLength`, which is 5. An existing stale pin asserted `ZSTD_DStreamInSize` and was corrected.

The deprecated DStream init family now behaves like the reference. `initDStream_usingDict` resets and loads, and the load error is the return. `initDStream_usingDDict` attaches the parsed handle dictionary. `resetDStream` performs a bare session reset, and the dictionary survives. All three return 5.

The no-forward-progress guard matches the reference. Sixteen consecutive no-progress `decompressStream` calls answer `noForwardProgress_destFull` (80) with a full output, and otherwise `noForwardProgress_inputEmpty` (82). Progress or any session reset restarts the count. A `ZSTD_DStream` is its `ZSTD_DCtx`, and the reference's `ZSTD_decompressBegin` under session resets clears the counter, which the earlier implementation missed on `initDStream`. The between-frames state is exempt exactly as the reference's `loadHeader` shortcut, whose hint return sits before the guard. After the completion 0 went out, further calls ask 9, which is `max(FRAMEHEADERSIZE_MIN, hSize) - staged + blockHeaderSize`. `compressStream2` has no guard on either side, probed on both.

The earlier commits fixed several contracts. Commit 4fdc4872 fixed `getFrameHeader`'s short-input and struct-write contract, including the 48-byte struct's tail padding. Commit 68395088 enforced the strict maxCode bound and kept the raw gap codes. Commit 136a2758 fixed the parameter bounds, where `d_windowLogMax` is the union of `{0}` and 10..=31 and 32..=41 answers `parameter_outOfBound`, probed identical on both libs with no deviation remaining. It also fixed the mid-frame `stage_wrong` gates, the wild-cursor checks and the mid-frame cParam whitelist.

## Recorded deviations

- A corrupt formatted dictionary at load answers `dictionary_corrupted` (30), where the reference answers `memory_allocation` (64). This was probed on `ZSTD_DCtx_loadDictionary` and on `ZSTD_CCtx_loadDictionary` with a first frame. The reference's code is an allocation artifact of its CDict and DDict build path, so the code that names the actual cause wins.
- NULL-pointer defense covers cases where the reference has UB. A null `src` with at least 5 claimed bytes on a decode entry answers `srcSize_wrong`, where the reference derefs. Null buffers on compress entries answer `srcBuffer_wrong` and `dstBuffer_null`, where the reference writes. Both are documented in `ffi.md`.
- One-shot compress accepts an exact-fit capacity. The reference's `ZSTD_compress` refuses a `dstCapacity` in `[used, used+8)` with `dstSize_tooSmall` even though the output fits. A probe with used=276 accepted the first capacity at 284, an internal workspace-margin artifact. This layer accepts the exact fit. The earlier "noise-cap anomaly" is this case, now boundary-pinned.
- The wild-cursor clamp is fixed. `compressStream2` and `decompressStream` now answer `dstSize_tooSmall` and `srcSize_wrong` for `pos > size` in the reference's check order. The earlier behavior was a silent clamp, and the fix is in 136a2758.
- Streaming staging is block-granular, and the reference decodes ahead. With an unwritable output of zero capacity, this layer consumes no input and the guard fires on call 16. The reference keeps consuming into internal buffers, hostage byte included at `inpos=272/273`, and fires on 17. Mid-frame with partial input, the reference can deliver a small first block's output where this layer waits for the block it can decode whole, as in `feed60: outpos=50` against 0. Error codes, final output and the completion contract are identical, and the divergence is confined to transient progress and hint shapes. Matching the reference here needs an internal decode-ahead buffer, which is a redesign.
- Mid-frame hints are advisory and not byte-exact. The reference returns its exact `nextSrcSizeHint`, and this layer suggests one block with `DSTREAM_OUT_SIZE`. Both are documented hints, and no caller contract distinguishes them.
- `initDStream_usingDDict` has a strengthened lifetime. The handle's dictionary is Arc-cloned into the context, where the reference keeps a caller-owned pointer that dangles if freed early. This layer cannot dangle.
- Pre-existing behavior, documented in `ffi.md`: negative levels clamp to 1, windowLog above 27 is engine-capped, nbWorkers semantics apply, and one-shot variants use fresh settings.
- Also pre-existing and documented in `ffi.md`: a failed stream replays its error where the reference has UB, `setParameter` level mid-frame applies to the next frame where the reference retunes the running one, and sub-8-byte dictionaries mean no dictionary on both sides.

## Unprobed remainder

- The deliberately uncovered symbol families above, namely estimate, static, advanced, ref, block-level and push-API entries, have no behavior probes. Their coverage is a named gap, not a verified one.
- `ZSTD_decompressContinue`'s own no-forward-progress counter, `dctx->noForwardProgress`, is moot while the push API is unimplemented.
- Legacy-format frames on the `ZSTD_isLegacy` paths are unsupported by this codec, and no probe exercised legacy magic on the decode entries.
- The exact between-frames hint when a partial next-frame prefix under 5 bytes is staged is unprobed. The reference computes `max(6, hSize) - lhSize + 3`, and this layer returns the same formula only in the nothing-staged shape of 9, falling back to the block suggestion once 5 bytes arrive.

## Commits

- 4fdc4872 fixes `getFrameHeader`'s short-input and struct-write contract, including the 48-byte struct's tail padding
- 68395088 enforces the strict maxCode bound and keeps the raw gap codes
- 136a2758 fixes the parameter bounds, the mid-frame `stage_wrong` gates, the wild-cursor checks and the mid-frame cParam whitelist
- b28e8b68 fixes the one-shot bad-magic gating, null and truncated decode buffers, the `initDStream` return, the deprecated DStream init family and the no-forward-progress guard

## Verification

Every pinned expectation was first verified on the reference. `tests/run-c.sh` runs the same harness against `libzstd.a` as a parity gate, and the probe outputs are byte-diffed between the reference build and the built `libzstd.so`.
