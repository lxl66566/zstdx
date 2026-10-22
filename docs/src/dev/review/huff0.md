# R64 review — Huff0 + bit I/O

Scope: `crates/zstdx/src/huff0/` (huff0_decoder.rs, huff0_encoder.rs, mod.rs), `crates/zstdx/src/bit_io/` (bit_reader_reverse.rs, bit_reader.rs, bit_writer.rs, mod.rs), plus the call-site contracts in `decoding/literals_section_decoder.rs` and `encoding/block_enc/compressed.rs`. Reference: libzstd `entropy_common.c` (HUF_readStats), `huf_decompress.c` (X1/X2 DTable builds, 4-stream loops), `huf_compress.c` (HUF_writeCTable_wksp, HUF_compressWeights). No byte-affecting change: every fix touches malformed-input handling, unreachable-path hardening, or is a refactor; encoder output is unchanged for all inputs that already produced valid streams.

## Fixed

### F1 — FSE weights decode could exceed 255 weights (bug, malformed input)
`read_weights`' interleaved loop checks `weights.len() > 255` only at iteration end; the two stream-exhaustion `break`s skip it. A crafted stream that exhausts right after `dec1.update_state` at len 254 escapes with 256 weights, and `build_table_from_weights` then fills a 257-entry `bits` array whose `symbol as u8` wraps symbol 256 onto symbol 0 (wrong decode, no UB; libzstd caps at hwSize-1 = 255 and errors). Fixed with a post-loop bound check; the in-loop check stays as the liveness guard against 0-bit FSE states (unbounded push). Commit c812950c.

### F2 — missing libzstd rank-1 tree-validity check (spec deviation, error classes)
C's `HUF_readStats` rejects `(rankStats[1] < 2) || (rankStats[1] & 1)` — the maximum-weight class (weight 1, inferred last weight included) must hold an even count of at least two symbols. Complete-but-non-canonical weight sets (raw `[2,2]` closing with inferred 3, i.e. a code whose max length is below its tableLog) parse Kraft-exact in zstdx and built a working table, so zstdx accepted input libzstd rejects. Note the odd half is mathematically implied by the leftover power-of-two gate (odd weight-1 count ⇒ odd sum ⇒ leftover 1 ⇒ inferred weight 1 ⇒ even total); only the `< 2` half adds rejecting power — both kept for structural parity with the reference. No encoder can emit a violating set (every complete prefix code has ≥ 2 max-depth leaves, and real encoders set tableLog = max code length). Commit c812950c.

### F3 — write_table_desc header-range wrap (latent encoder corruption, unreachable)
The FSE form's acceptance gate for wide alphabets (`n > 128`, unconditional once built) had no `fse_len < 128` bound: a value ≥ 128 would push a header byte the decoder parses as the raw form (silent corrupt stream), and the phantom-weight fallback path would then reach the raw form with `n > 128`, where the `debug_assert!`-only guard lets `n as u8 + 127` wrap in release. Unreachable in practice: a Kraft-valid table's weight stream is bounded by its class entropy, whose maximum over ≤ 255 symbols with mass ≤ 2^12 is ~1.9 bits/symbol (Lagrange optimum ≈ [42,53,47,40,31,16,4,1] over weights 1..8) — ≈ 61 B payload + ≤ ~25 B FSE table/quantization ≪ 128 B. Hardened with an explicit `fse_len < 128` gate and a hard `assert!(n <= 128)` so the invariant fails loudly instead of wrapping. Commit 70ab5f6c.

### F4 — duplicated padding-marker skip + wrong encode4x_only floor (smell)
The skip-zeros-then-one loop was open-coded five times (weights parse, both literals decode paths, both round-trip helpers); the error paths always reported `skipped_bits == 9` (the loop only exits past 8 when the first 8 bits are zero). Folded into `BitReaderReversed::skip_padding_marker()`. Also: `encode4x_only`'s `assert!(data.len() >= 4)` understates the real floor — the quarter split panics at 5..8 (3·ceil(n/4) > n); stated as 6 (three quarters plus the fourth stream's end marker; the caller's one-vs-four gate only passes ≥ 256). Commit 0785ed78.

## Verified correct (no action)

- X2 table build is a line-by-line port of `HUF_readDTableX2_wksp`/`HUF_fillDTableX2`: rescale = targetLog-tableLog-1, scaleLog = nbBitsBaseline-targetLog, minWeight clamp to 1, the skip-span single-symbol fill below `rankVal[minWeight]`, level-2 spans `1 << (targetLog - totalBits)` at `start + rankVal[w2]`, single-symbol else branch. The `eligible_slots` 80% cutoff is a documented zstdx-specific perf gate, not part of the format.
- X1 table fill: tableLog = highbit32(weightTotal)+1 matches `highest_bit_set`; rank_indexes prefix sums; the `rank_idx[0] == table.len]` assert pins Kraft exactness; weight > 11 rejection ≡ libzstd's weight ≤ 12 + tableLog ≤ 11 net effect (a weight of 12 forces tableLog ≥ 12, which the max-bits check kills).
- Raw-weights parse bounds match C (`iSize+1 > srcSize` vs bytes_needed); FSE branch bounds (`header ≤ fse_stream.len()`, FSE-desc ≤ header, stream = header - desc) match; byte accounting `1 + header` matches C's `iSize+1`.
- 4-stream interleaved fast loops (X1/X2): the `| 1` marker + trailing_zeros reload design, `iters` precomputed from input-bytes/7 and output/5-or/10 bounds, ip-crossing corruption checks, and `finish_streams`' foreign-bit accounting (window may extend ≤ 8 bytes below the stream start; `ext`/`foreign` math checked) are sound. The X2 u16 write cannot spill into the next segment (iteration bound keeps lookups ≥ 1 byte from seg_end; final byte via the scalar finish).
- BitWriter: cold-path unaligned u64 store is reserve-covered and the length == bit_idx/8 invariant is maintained across write_bits_64_cold / write_packed_codes_dual_impl / write_uniform4_bulk / set_hot_state (each reserves pos + bound from the semantic position); `flush`'s byte-alignment assert holds at every call site by protocol (change_bits callers are the aligned jump-table/size-field writes); uniform4 AVX2/AVX512 packers' stores are within the bulk reserve and bit-identical to the scalar group loop (plus the dedicated AVX2-vs-scalar randomized test and the tier-forced dump gate).
- BitReaderReversed refill: the three short-window cases (partial last window, past-end zero fill) keep `bits_consumed < 8` post-refill and account `extra_bits`; `from_parts`' `index + 8 <= len` contract is upheld by `finish_streams` (ip ≤ end-8 invariant of the reload discipline).
- Call sites: block_decoder/mt.rs pre-validate literal-section sizes before slicing (Raw/RLE indexing cannot panic); treeless blocks reusing a stale X2 table is impossible across fresh builds (build_table_from_weights clears x2 first) and correct within treeless runs (same weights); `reinit_from` copies x2 together with the weights it was built from.

## Accepted (with reason)

- `MAX_MAX_NUM_BITS` (decoder) and `MAX_CODE_LENGTH` (encoder) are two consts for the same 11 — unifying them couples the two modules' hot code for zero behavioral value.
- X1/X2 entry-packing magic numbers (`& 0x3f`, `>> 53`, `<< 16`/`<< 24`) stay as documented literals; const-ifying risks codegen churn in the hottest decode loop for readability only.
- `HuffmanDecoder::init_state` returns `num_bits` (constant per table); every caller ignores it — harmless API vestige.
- `GetBitsError::TooManyBits` mixes usize/u8 fields — internal error type, churn > value.
- The `n > 128` FSE acceptance being looser than libzstd's `hSize < maxSymbolValue/2` gate is a documented deliberate choice (the raw form is impossible there; the FSE form is valid per spec for any conforming decoder).
- `BitReaderReversed::get_bits(n > 56)` would underflow the peek shift — internal contract (documented "at most 56 bits"), all callers ≤ 22.

## Tests added

- `rejects_weight_sets_without_rank_one_symbols`, `accepts_canonical_weight_sets` (raw-weight forms incl. single-weight and header-128 edges) — huff0_decoder.
- `fse_weights_decode_count_bound` — FSE-crafted weight streams via the encoder pipeline: 255 (legal max) decodes, 256 rejects `TooManyWeights(256)`.
- `wide_alphabet_table_desc_stays_fse_addressable` — near-max-entropy Kraft-exact 234-weight multiset (shuffled) must take the FSE form with header < 128 and parse back into the same weights.
- `padding_marker_skips_zeros_then_one` — the folded reader helper.

Baseline and post-change suites: `cargo test` (debug), `--release`, `--no-default-features` all green (16 result lines each); clippy zero warnings.
