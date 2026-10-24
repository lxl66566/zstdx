# Huffman coder review

Scope: the modules under `crates/zstdx/src/huff0/`, namely `huff0_decoder.rs`, `huff0_encoder.rs` and `mod.rs`, and under `crates/zstdx/src/bit_io/`, namely `bit_reader_reverse.rs`, `bit_reader.rs`, `bit_writer.rs` and `mod.rs`, plus the call-site contracts in `decoding/literals_section_decoder.rs` and `encoding/block_enc/compressed.rs`. Reference: libzstd `entropy_common.c` with HUF_readStats, `huf_decompress.c` with the X1 and X2 DTable builds and the 4-stream loops, and `huf_compress.c` with HUF_writeCTable_wksp and HUF_compressWeights. No change here affects bytes: every fix touches malformed-input handling or unreachable-path hardening, and encoder output stays the same for every input that already produced a valid stream.

## Bugs fixed

### Weight count bound

`read_weights`' interleaved loop checks `weights.len() > 255` only at the end of an iteration. The two stream-exhaustion `break`s skip the check. A crafted stream that exhausts right after `dec1.update_state` at len 254 escapes with 256 weights. `build_table_from_weights` then fills a 257-entry `bits` array, and `symbol as u8` wraps symbol 256 onto symbol 0. The decode is wrong and there is no UB. libzstd caps the count at hwSize-1, which is 255, and reports an error. A post-loop bound check now rejects the stream. The in-loop check stays as the liveness guard against 0-bit FSE states, which would otherwise push without bound.

### Rank-1 validity

C's `HUF_readStats` rejects `(rankStats[1] < 2) || (rankStats[1] & 1)`. The maximum-weight class, weight 1 with the inferred last weight included, must hold an even count of at least two symbols. Complete but non-canonical weight sets parsed Kraft-exact in zstdx and built a working table. An example is the raw set `[2,2]` closing with an inferred 3, a code whose maximum length is below its tableLog. zstdx therefore accepted input that libzstd rejects. The odd half is mathematically implied by the leftover power-of-two gate: an odd weight-1 count gives an odd sum, an odd sum leaves a leftover of 1, an inferred weight of 1 makes the total even. Only the `< 2` half adds rejecting power, and both checks stay for structural parity with the reference. No encoder can emit a violating set, because every complete prefix code has at least two max-depth leaves and real encoders set tableLog to the maximum code length.

## Hardening

### Table descriptor header range

The FSE form's acceptance gate for wide alphabets at `n > 128`, unconditional once built, had no `fse_len < 128` bound. A value of at least 128 would push a header byte that the decoder parses as the raw form, which silently corrupts the stream. The phantom-weight fallback path could then reach the raw form with `n > 128`, where the `debug_assert!`-only guard lets `n as u8 + 127` wrap in release. The case is unreachable in practice. A Kraft-valid table's weight stream is bounded by its class entropy, whose maximum over at most 255 symbols with mass at most 2^12 is about 1.9 bits per symbol. The Lagrange optimum is about `[42,53,47,40,31,16,4,1]` over weights 1 to 8, which gives about 61 B of payload plus at most about 25 B of FSE table and quantization, far below 128 B. An explicit `fse_len < 128` gate and a hard `assert!(n <= 128)` now make the invariant fail loudly instead of wrapping.

## Refactors

The loop that skips zero bits and then consumes one was open-coded five times: in the weights parse, in both literals decode paths, and in both round-trip helpers. Its error paths always reported `skipped_bits == 9`, because the loop only exits past 8 when the first 8 bits are zero. The code now calls `BitReaderReversed::skip_padding_marker()`. `encode4x_only`'s `assert!(data.len() >= 4)` also understated the real floor. The quarter split panics at 5 to 8 because `3*ceil(n/4) > n`, and the assertion now states 6, which is three quarters plus the fourth stream's end marker. The caller's one-versus-four gate only passes at 256 or more.

## Verified correct

### X2 table build

The X2 table build is a line-by-line port of `HUF_readDTableX2_wksp` and `HUF_fillDTableX2`. It uses `rescale = targetLog-tableLog-1` and `scaleLog = nbBitsBaseline-targetLog`. It clamps `minWeight` to 1 and fills a single symbol across the span below `rankVal[minWeight]`. Level-2 spans use `1 << (targetLog - totalBits)` at `start + rankVal[w2]`, with a single-symbol else branch. The `eligible_slots` 80% cutoff is a documented zstdx-specific perf gate and is not part of the format.

### X1 table fill

The X1 fill computes `tableLog = highbit32(weightTotal)+1`, which matches `highest_bit_set`. `rank_indexes` holds the prefix sums. The `rank_idx[0] == table.len` assert pins Kraft exactness. Rejecting a weight above 11 has the same net effect as libzstd's weight at most 12 plus tableLog at most 11, because a weight of 12 forces tableLog to at least 12, which the max-bits check kills.

### Weight parsing

The raw-weights parse bounds match C: `iSize+1 > srcSize` against `bytes_needed`. The FSE branch bounds match as well: `header ≤ fse_stream.len()`, the FSE descriptor at most the header, and the stream length equal to the header minus the descriptor. The byte accounting `1 + header` matches C's `iSize+1`.

### Four-stream loops

The 4-stream interleaved fast loops for X1 and X2 are sound. The design uses the `| 1` marker and a `trailing_zeros` reload. `iters` is precomputed from the input-byte bound divided by 7 and the output bounds divided by 5 or 10. The loops check ip-crossing corruption, and `finish_streams` accounts foreign bits correctly, where the window may extend at most 8 bytes below the stream start and the `ext` and `foreign` math was checked. The X2 u16 write cannot spill into the next segment, because the iteration bound keeps lookups at least 1 byte from `seg_end` and the final byte goes through the scalar finish.

### BitWriter

The BitWriter's cold-path unaligned u64 store is reserve-covered. The `length == bit_idx/8` invariant holds across `write_bits_64_cold`, `write_packed_codes_dual_impl`, `write_uniform4_bulk` and `set_hot_state`, each of which reserves `pos` plus a bound derived from the semantic position. `flush`'s byte-alignment assert holds at every call site by protocol, because the `change_bits` callers are the aligned jump-table and size-field writes. The uniform4 AVX2 and AVX512 packers store within the bulk reserve and are bit-identical to the scalar group loop. A dedicated randomized AVX2-versus-scalar test and the tier-forced dump gate cover them.

### BitReaderReversed

The `BitReaderReversed` refill handles the short-window cases, where the window is a partial last window or a past-end zero fill. It keeps `bits_consumed < 8` after the refill and accounts `extra_bits`. The `from_parts` contract `index + 8 <= len` is upheld by `finish_streams`, through the `ip <= end-8` invariant of the reload discipline.

### Call sites

`block_decoder/mt.rs` pre-validate literal-section sizes before slicing, so Raw and RLE indexing cannot panic. A treeless block cannot reuse a stale X2 table across fresh builds, because `build_table_from_weights` clears x2 first, and reuse within a treeless run is correct because the weights stay the same. `reinit_from` copies x2 together with the weights it was built from.

## Accepted limits

`MAX_MAX_NUM_BITS` in the decoder and `MAX_CODE_LENGTH` in the encoder are two constants for the same 11. Unifying them would couple the two modules' hot code for no behavioral value.

The X1 and X2 entry-packing magic numbers `& 0x3f`, `>> 53`, `<< 16` and `<< 24` stay as documented literals. Turning them into constants risks codegen churn in the hottest decode loop for readability only.

`HuffmanDecoder::init_state` returns `num_bits`, which is constant per table, and every caller ignores it. The return value is a harmless API vestige.

`GetBitsError::TooManyBits` mixes `usize` and `u8` fields. It is an internal error type, and the churn would exceed the value.

The `n > 128` FSE acceptance is looser than libzstd's `hSize < maxSymbolValue/2` gate by deliberate choice. The raw form is impossible there, and the FSE form is valid per spec for any conforming decoder.

`BitReaderReversed::get_bits(n > 56)` would underflow the peek shift. The internal contract documents at most 56 bits, and all callers stay at 22 or below.

## Tests added

- `rejects_weight_sets_without_rank_one_symbols` and `accepts_canonical_weight_sets` cover raw-weight forms including the single-weight and header-128 edges in `huff0_decoder`.
- `fse_weights_decode_count_bound` feeds FSE-crafted weight streams through the encoder pipeline: 255, the legal maximum, decodes, and 256 rejects with `TooManyWeights(256)`.
- `wide_alphabet_table_desc_stays_fse_addressable` uses a shuffled near-max-entropy Kraft-exact 234-weight multiset, which must take the FSE form with a header below 128 and parse back into the same weights.
- `padding_marker_skips_zeros_then_one` covers the folded reader helper.

## Commits

- c812950c fixes the weight-count bound and the rank-1 validity check
- 70ab5f6c adds the table-descriptor header range gate
- 0785ed78 folds the padding-marker skip and states the `encode4x_only` floor

## Verification

The baseline and post-change suites were green with `cargo test` in debug, `--release` and `--no-default-features`, with 16 result lines each. Clippy reported zero warnings.
