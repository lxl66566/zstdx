# FSE core review

Scope: the modules under `crates/zstdx/src/fse/`, namely `fse_decoder.rs`, `fse_encoder.rs` and `mod.rs` at 1978 lines, plus the call-site contracts in `decoding/sequence_section_decoder.rs` table modes and packed layout, the huff0 weight FSE path, and the dictionary finalize NCount writes. Reference: libzstd at `/root/programs/fork/zstd`, with `FSE_readNCount`/`FSE_buildDTable` in `entropy_common.c`/`fse_decompress.c` and `FSE_writeNCount`/`normalizeCount`/`normalizeM2`/`optimalTableLog`/`buildCTable`/`compress_usingCTable` in `fse_compress.c`. The format reference is RFC 8878.

## Exactness against libzstd

The audit compared each routine with the reference line by line and found no behavior to change.

### NCount reader

`read_probabilities` matches `FSE_readNCount_body`. The value, threshold and max algebra is identical. The proof that `bits_to_read == nbBits` rests on the C invariant `threshold <= remaining < 2*threshold`. The short, mid and long branches match, including `return_bits(1)`. The zero-run 2-bit markers match the C ctz fast path. `remaining` stays bounded, so `counter <= sum` always holds and `value <= remaining` holds on all three branches. The implementations differ only in error classes. C breaks on `charnum > maxSV` inside the loop and reports corruption or `maxSymbolValue_tooSmall`. This port keeps reading to the sum or to input exhaustion and reports `TooManySymbols` or `GetBitsError`. The work is input-bounded, because every iteration consumes at least one bit or advances the counter.

### Decoding table

`build_decoding_table` matches `FSE_buildDTable`. Symbols with value -1 sit at the top through `highThreshold`. The positive spread uses `position = (position+step) & mask` and skips the -1 region. The per-occurrence baseline and `num_bits` are identical. The `SymbolSpreadInfo` formulas equal C's `newState = (nextState << nbBits) - tableSize` for every occurrence k. For k below R-p the double form holds as `base_d + k*2*sw == (p+k)*2*sw - ts`, and the single form holds as `(k-num_double)*sw == (p+k-R)*sw`. The tests `soa_matches_reference` and `tables_equal` pin both formulas.

### Interleaved encoder

`encode_interleaved` matches `FSE_compress_usingCTable` on the wire. This includes the parity-dependent init swap, where C inits C2 to last for an even srcSize and C1 to last for an odd srcSize plus an extra join step. It also includes the mod-4 join. The final state write order matches: the data[1] chain state goes first and the data[0] chain state goes last, which is the order the decoder reads. Traces on even, odd and mod-4 cases verified this. The single-state `encode` matches the FSE encode semantics, pinned by `round_trip`.

### Normalization

`normalize_count` and `normalize_m2` match `FSE_normalizeCount` and `FSE_normalizeM2`. This includes `RTB_TABLE`, the `-stillToDistribute >= norm[largest]>>1` corner, and the `total==0` and all-poor paths. C's in-loop `count[s]==total` RLE early return is unreachable here, because both callers gate RLE with `most_frequent == nb_seq` before normalizing. The missing C tableLog validation is caller-guaranteed, because `optimal_table_log` clamps to at least 5.

### Table log

`optimal_table_log` and `min_table_log` match C. The wrap of `highbit(src_size-1)-2` is reproduced. C clamps to FSE_MAX_TABLELOG=12 at the end, which matches the Rust `clamp(5, 12)`, and maxTableLog does not apply.

### NCount writer

`write_ncount` matches `FSE_writeNCount`. The threshold, max, value and bit accounting is identical, and so is the zero-run marker order. The `(4+15*nsym)/8+16` bound is proven safe: the worst case is 13 bits per live symbol and 2 bits per zero, and the stores stay inside the reserve.

### Unsafe audit

The unsafe code is sound. `write_ncount` raw-pointer stores are covered by a reserve for `pos+8`, and no realloc happens between pointer derivation and `set_hot_state`. The aliasing is sound. `build_table_body` splits a `tab_len = 448 + ceil(ts/2)` u64 buffer into regions at offsets 0, 2048, 3072 and 3584. The regions are disjoint and u64-aligned. The spread run stores have `ts+8` slack. The `tab_parts`, `state_table`, `probs` and `start` slices are in bounds. No UB was found. Each site documents the contract its callers must uphold: dead-symbol tt entries hold stale pooled data, and `step` on a dead symbol stays in bounds through the dead fill.

### Call-site contracts

The sequence table modes cover RLE through `pack_rle_entry` self-transition, predefined rebuild gating, and the RFC max_log values 9, 9 and 8. `LL_CODES`, `ML_CODES` and `OF_CODES` are sized to the MAX_* codes, so `pack_seq_table`'s `codes[sym]` lookup is bounds-safe. The huff0 weights call `build_decoder(.., 6)`, the RFC weight max log. Dictionary finalize writes NCount with logs of at least 5.

## Bugs fixed

The NCount reader rejected valid streams. The final symbol of a short-form NCount ends its read one bit past the last byte. libzstd accepts this through its zero-padded read window, because net consumption stays within the stream. The Rust bounds check errored before the branch could call `return_bits(1)`. Any encoder can emit this form, including libzstd's own writeNCount, so the bug was reachable in production, and a random distribution found it within 44 cases. The fix adds `BitReader::get_bits_zero_padded` with slack 1 and a post-branch truncation guard, which truncates a read that keeps the padding bit. The change affects the decoder only and no encoder bytes change.

## Hardening

`write_ncount`'s accumulator could overflow. Entering with 47 pending bits plus 4 header bits gives 51, and a 13-bit first symbol at acc_log 12 reached nb=64, so the flush shifted `acc >> 64`. That panics in debug and wraps silently in release. Current callers cannot reach it because acc_log stays at most 9. A post-header `flush!()` now prevents it, the bit sequence is preserved, and the byte-identity suites stay green.

`write_ncount`'s `acc_log - 5` also underflowed for acc_log below 5. `rle_table` builds acc_log-0 tables, which are legitimately never serialized because RLE writes a raw byte, but nothing guarded the serializer. The codec halves now share `ACC_LOG_OFFSET`, hoisted to `fse/mod.rs`, and a `debug_assert!((ACC_LOG_OFFSET..=12).contains(&acc_log))` guards the serializer.

## Shared definitions

The predefined distributions existed twice, as the encoder's `LL/ML/OF_DIST` and the decoder's `*_DEFAULT_DISTRIBUTION`, and no test pinned their equality. Drift would have broken predefined-mode interop silently. They now live in `blocks::sequence_section` next to the MAX_*_CODE constants. The copy-pasted match-length doc on the OF distribution is fixed too. The change is byte-neutral.

## Accepted limits

`ProbabilityCounterMismatch` is unreachable, because the loop only exits with counter equal to sum and values are bounded by `remaining`. It stays as defense in depth. It is the analog of C's post-loop `remaining != 1` check, which is reachable in C through its in-loop maxSV break. If the bound proof ever regresses, the error prevents `build_decoding_table` from walking an over-subscribed spread, which would underflow `negative_idx`. The site documents this.

The weight FSE table was built with `FSETable::new(255)`. libzstd's real cap is HUF_TABLELOG_MAX-1, which is 11, reached through FSE workspace sizing in HUF_readStats. FSE_readNCount itself has no such check. A declared-but-never-emitted maxSV of at least 12 is a class no weight-set bound can reject, and zstdx accepted it. This item was accepted in this round and closed on 2026-10-23 in the [decode cleanup review](declean.md). `FSETable::new(11)` now rejects the class at the NCount read, and every libzstd-accepted header stays unchanged, verified with the C probe.

`mod.rs`'s `round_trip` returns early when `data.len() < 2`, which the later `< 64` check subsumes. The case is test-only and harmless.

## Tests added

`ncount_roundtrip` covers 300 cases over acc 5..=12, -1 weights, interior runs past 24 that produce packed 0xFFFF markers, trailing dead symbols, byte-count equality, and decode-table identity against `build_from_probabilities`. The lookahead and truncation cases are pinned too. `Entry` gained `PartialEq, Eq` for the comparison.

## Commits

- 8d8541f fse: share the predefined sequence distributions between encoder and decoder
- c0ce8bc fse: harden write_ncount's accumulator invariant
- 5061157 fse: accept the NCount final short-form one-bit lookahead

## Verification

Full `cargo test` with debug, `--release` and `--no-default-features` was green before and after each change. The run had 368 lib tests, including the 3 new ones.
