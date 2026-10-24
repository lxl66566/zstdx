# Optimal parser review

This page records the first dedicated audit of the btopt and btultra port in `opt.rs`. It answers how the parser's economics and its search and tree agree with libzstd's `zstd_opt.c`, which single fidelity defect was fixed, and which deviations are deliberate. Earlier reviews covered the entropy cores, the emit chain and the structural invariants, but the opt and ultra rows' parse engine, the ratio flagship, had no line-by-line pass.

The audit had two halves against the C reference v1.6.0 at `/root/programs/fork/zstd`. The economics half covered the weights, rescale, update and price functions, the DP price stewardship and the ultra seeding stats. The search and tree half covered the count kernels, the Finder, hash3, `LdmCursor`, the cursor and fill mechanics, the driver bridge and the ladder geometry. One fidelity defect was fixed. Everything else forms a verified deviation map: the port's constants, BASE tables, weight, price and update formulas, DP bookkeeping and per-block table contracts are field-for-field equal to C.

## The fix

The ULTRA match-plus-one-literal recheck in `run_once` used the guard `pos + cur + 1 < block_end`. C gates the btultra shortcut on `ip + cur < iend` at `zstd_opt.c:1225`, so the guard was one byte stricter than the original port. A DP position whose single priced literal would be the block's last byte never considered the match-plus-one-literal path. The guard now matches C's exact form, and the change applies to ultra rows only.

Output moves exactly where the boundary case fires. The dump grids move 5 cells, all json l19: bulk by +1 B, stream and stream-pledged by +1 B, and stream-mt4 and pledged by +60 B. MT's job-boundary seed parses re-run the DP and amplify the boundary case into the seeded stats. A ratio A/B of revert against fix under the bench lock moves 4 json.ultra cells by at most +0.0013%, leaves 116 cells byte-identical, and leaves the geo-mean unchanged at +8.69%. json.ultra stays ahead of the libzstd reference. The changed frames roundtrip through the system zstd CLI md5-identical to their pre-fix decodes. The debug, release, no-default-features and cli suites are green. Paranoid and Ir A/B were skipped with cause, since the change touches one comparison operand and no structure or instruction count. The fuzz smoke of QUICK, 120 s across 16 workers, is clean.

## Verified equal to C

- The constants and BASE tables are byte-equal: BITCOST_MULTIPLIER 256, OPT_NUM 4096, OPT_SIZE = OPT_NUM+3, LL/ML/OF table sizes 36/53/32, and baseLLfreqs/baseOFCfreqs sums 40/53. `bit_weight`, `frac_weight` and `weight<ULTRA>` match `ZSTD_bitWeight`, `ZSTD_fracWeight` and `ZSTD_WEIGHT`. `new_rep` matches `ZSTD_updateRep`, including the repCode==3 pseudo-slot, pinned by a unit test.
- `rescale_freqs` matches `ZSTD_rescaleFreqs` for every zstdx path, covering the first-block histogram branch, the downscale shifts and the scale-down branch with its factor at most 1 early return. `compressedLiterals`'s absence is equivalent, because C's flag defaults to true and no literal-compression toggle reaches opt. `set_base_prices` matches `ZSTD_setBasePrices` at C's own cadence, on rescale and after every emitted series.
- `update_stats` matches `ZSTD_updateStats`. `lit_price`, `lit_length_price` and `match_price` match `ZSTD_rawLiteralsCost`, `ZSTD_litLengthPrice` and `ZSTD_getMatchPrice`, including the offCode extra bits, the offCode of 20 or more opt-level handicap, the ML extra bits, the +BITCOST/5 match surcharge and the litPriceMax clamp. `ll_inc_price` matches `LL_INCPRICE`, with i64 intermediates making the negative increment arithmetic safe. The price model's LL and ML code ladders and extra bits match `seq_codes` byte for byte, so the prices approximate exactly what block_enc writes.
- The DP stewardship in `run_once` is field-for-field equal to `ZSTD_compressBlock_opt_generic` in `zstd_opt.c`. It covers the opt[0] init, the min_match skip-position init, the first-match coverage, the literal-fix block, the rep rewrite at litlen of 0, the ilimit continue, the 3-way jump, and the downward mlen scan with its store rule and the !ULTRA early break. It also covers the MAX_PRICE gap fill, the `_shortestPath` reverse walk, and the per-series stats and base-price refresh.
- The count kernels cap every read at `block_end`, and there is no extDict surface because the window is one contiguous borrowed slice. `LdmCursor` matches `ZSTD_optLdm_maybeAddMatch` and `getNextMatchAndUpdateSeqStore`. Every `get_all_matches` exit leaves C's next_update and table postcondition.
- The u32 pack and epoch discipline holds. Live entries rebuild exactly under wrap. Cross-frame stale entries and dist-0 self-compares die on the `cand < pos` resolve. `reset`'s origin advance provably kills every prior-frame entry. The cumulative-origin phantom above 4 GiB is the documented accepted class. The repcode wrap arithmetic through `checked_sub` and `saturating_sub` is complete. The DP array bounds are forced by `sufficient_len ≤ 999 < OPT_NUM` before any over-range write.
- Ultra seeding matches the `ZSTD_initStats_ultra` and btultra2 contract: the first-block gate, the throwaway parse with restored reps, table invalidation through the origin advance matching C's window rewind, and the rescale of the seeded stats. The job-boundary strip-tail seed and the dict-tail seed are documented zstdx extensions. In the ladder geometry, every S, TL and ring field of rows 16 to 22 matches `clevels.h` except the three knobs in the [todo](../todo.md), and the C24 to C23 and L17-cap equivalences are the documented forms.

## Kept deviations

| Deviation | Class | Why kept |
|---|---|---|
| Ultra seeding's both passes see the LDM candidates, where C's shared rawSeqStore loses them in pass 2 | lenient superset | Strictly more parse options. Matching C would drop candidates, and any change is a ratio campaign. |
| No C dictionary entropy-table branch in `rescale_freqs`, which seeds the price tables from the formatted-dict CTables and FSE tables | fidelity, superseded | Non-ultra dictionary frames shape prices through the seed-parse replay instead. The dict-row family measured ahead of C's rows on the fixture in the [encoding tuning campaign](../perf/encoding.md). Porting C's branch is a campaign, not a fix. |
| `bt_low = pos - ring/2` against C's `curr - (ring-1)` | lenient by one position | Threads one extra below-ring hop, and aliasing is safe in both. |
| Insert-side per-position floor against C's fill-target-based windowLow | lenient superset | Admits candidates in `[pos-max_window, target-max_window)` during fills, and the search-side floors match C. |
| Dictionary frames probe the first payload position, where C's `ip += (ip==prefixStart)` skips it | lenient, one position | Negligible and recorded here. |
| POS_BIAS makes window position 0 a representable candidate, where C's matchLow=1 excludes it | lenient | Documented in the POS_BIAS comment. |
| Insert-side count cap at `idx + OPT_NUM`, clamp_lag gating, and `cand_floor`'s stock search domain on dictionary content | documented deliberate | Recorded in code and in [pitfalls/encoding](../pitfalls/encoding.md). The count cap is a measured speed and ratio trade, and the dictionary domain is managed by the skip-shape guard. |

## Verified sound, no finding

Read bounds at tails hold, because the hash, count and byte reads are bounded by `idx+HASH_READ ≤ block_end` and debug-asserted at both entries. `lit_price` underflow is unreachable, because it needs a zero-length first block that the grid never emits, and C has the same shape behind an assert. `predef` on every block against C's first-block-only is dead in both, because prices are consulted only when the parse loop runs and that needs at least 9 or 10 bytes. `frac_weight`'s `hb < 23` guard holds, because the widest realistic table stays far below. `lit_length_price(MAX_BLOCK_SIZE)`'s recursion arm is dead in practice, because DP stretches end before the cap, recorded in the [todo](../todo.md).

## Pitfall

The shared-cursor hoist trap is recorded in [pitfalls/encoding](../pitfalls/encoding.md). hash3's fill cursor rides `next_update`, and the eager seed moved one without the other. The gap is unreachable on the current ladder, because every hash3 user row has mls of 3, so the 22-bit main tree shadows the 17-bit slot. This was probed byte-identical on plain and adversarial rep-suppressed fixtures. The prefill that closes the gap is in the [todo](../todo.md).

## Follow-ups resolved

These follow-ups were resolved on 2026-10-24 as part of the optimal-parser review's second pass.

- Row 16 with mls 5 and row 22 with search_log 9 were both falsified with data in the [matcher review](../negative/matchers.md), so the unlisted tuned-ladder deviations are measured-optimal as they stand.
- `hash3_log` is clamped to the final window log through `MIN(17, windowLog)`, from `zstd_compress.c:2027` in C. The corpus is byte-identical, and the clamp saves the 512 KiB table on forced-small-window opt states.
- The hash3 dictionary-seed prefill landed. The eager seed now fills the 3-byte table over `[0, keep-OPT_NUM)`, which closes the pitfall in code and is output-neutral everywhere.
- The dead `lit_length_price(MAX_BLOCK_SIZE)` arm carries a `debug_assert`.
- These experiments also exposed the forced-window interop bug, where unpledged streams at tree rows emit offsets past the declared window and libzstd rejects them. It is covered in [window semantics](winsem.md).

## Commits

- 7d030838 fix(opt): match C's guard on the ultra match+1-literal recheck
