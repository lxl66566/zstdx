# R70 review — optimal parser (opt.rs) vs libzstd zstd_opt.c

Scope: the btopt/btultra port's first dedicated audit — R64 covered the entropy cores, R68 the emit chain, R69 the structural invariants; the optimal parser itself (the opt/ultra rows' parse engine, the ratio flagship) had no line-by-line pass. Two halves against the C reference (v1.6.0, `/root/programs/fork/zstd`): the economics (weights, rescale/update/prices, DP price stewardship, ultra seeding stats) and the search/tree (count kernels, Finder, hash3, LdmCursor, cursor/fill mechanics, the driver bridge, ladder geometry). One fidelity defect fixed; everything else is a verified deviation map — the port's constants, BASE tables, weight/price/update formulas, DP bookkeeping and per-block table contracts are field-for-field equal to C.

## The fix

| Site | Finding | Fix |
|---|---|---|
| `run_once` ULTRA match+1-literal recheck | The guard was `pos + cur + 1 < block_end` where C gates the btultra shortcut on `ip + cur < iend` (zstd_opt.c:1225) — one byte stricter since the original port, so a DP position whose single priced literal would be the block's last byte never considered the match+1-literal path. | Relaxed to C's exact form. `7d030838`, ultra rows only. |

Output moves exactly where the boundary case fires: the dump grids move 5 cells, all json l19 (bulk +1 B, stream/stream-pledged +1 B, stream-mt4/pledged +60 B — MT's job-boundary seed parses re-run the DP and amplify the boundary case into the seeded stats). Ratio A/B (revert vs fix under the bench lock): 4 json.ultra cells move ≤ +0.0013%, 116 cells byte-identical, geo-mean unchanged at +8.69%, json.ultra stays ahead of the libzstd ref. Changed frames roundtrip through the system zstd CLI md5-identical to their pre-fix decodes. Gates: debug/release/no-default-features/cli suites green; paranoid and Ir A/B skipped with cause (one comparison operand, no structural or instruction-count change); fuzz smoke (QUICK, 120 s × 16 workers) clean.

## Verified equal to C (ledger highlights)

- Constants and BASE tables byte-equal (BITCOST_MULTIPLIER 256, OPT_NUM 4096, OPT_SIZE = OPT_NUM+3, LL/ML/OF table sizes 36/53/32, baseLLfreqs/baseOFCfreqs sums 40/53); `bit_weight`/`frac_weight`/`weight<ULTRA>` == ZSTD_bitWeight/fracWeight/WEIGHT; `new_rep` == ZSTD_updateRep including the repCode==3 pseudo-slot (pinned by unit test).
- `rescale_freqs` (first-block histogram branch, downscale shifts, scale-down branch with the factor≤1 early return) == ZSTD_rescaleFreqs for every zstdx path; `compressedLiterals`'s absence is equivalent (C's flag is default-true and no literal-compression toggle reaches opt). `set_base_prices` == ZSTD_setBasePrices at C's own cadence (rescale + after every emitted series).
- `update_stats` == ZSTD_updateStats; `lit_price`/`lit_length_price`/`match_price` == ZSTD_rawLiteralsCost/ZSTD_litLengthPrice/ZSTD_getMatchPrice including offCode extra bits, the offCode≥20 opt-level handicap, ML extra bits, the +BITCOST/5 match surcharge and the litPriceMax clamp; `ll_inc_price` == LL_INCPRICE with i64 intermediates making the negative-inc arithmetic safe; the price model's LL/ML code ladders and extra-bits match `seq_codes` byte-for-byte (prices approximate exactly what block_enc writes).
- The DP stewardship in `run_once` — opt[0] init, min_match skip-position init, first-match coverage, literal-fix block, rep rewrite at litlen==0, ilimit continue, the 3-way jump, the downward mlen scan with its store rule and !ULTRA early break, MAX_PRICE gap fill, `_shortestPath` reverse walk, per-series stats/base-price refresh — field-for-field equal to zstd_opt.c's ZSTD_compressBlock_opt_generic.
- Count kernels cap every read at `block_end` (no extDict surface: the window is one contiguous borrowed slice); `LdmCursor` == ZSTD_optLdm_maybeAddMatch/getNextMatchAndUpdateSeqStore semantics; every `get_all_matches` exit leaves C's next_update/table postcondition.
- u32 pack/epoch discipline: live entries rebuild exactly under wrap; cross-frame stale entries and dist-0 self-compares die on the `cand < pos` resolve; `reset`'s origin advance provably kills every prior-frame entry; the >4-GiB cumulative-origin phantom is the documented accepted class. Repcode wrap arithmetic (`checked_sub`/`saturating_sub`) complete; DP array bounds forced by `sufficient_len ≤ 999 < OPT_NUM` before any over-range write.
- Ultra seeding == ZSTD_initStats_ultra/btultra2's contract (first-block gate, throwaway parse with restored reps, table invalidation via origin advance ≡ C's window rewind, rescale of seeded stats); the job-boundary strip-tail seed and the dict-tail seed are documented zstdx extensions. Ladder geometry: every S/TL/ring field of rows 16-22 matches clevels.h except the three knobs in todo 25a; the C24≡C23 and L17-cap equivalences are the documented forms.

## Kept deviations (verified, recorded)

| Deviation | Class | Why kept |
|---|---|---|
| Ultra seeding's both passes see the LDM candidates (C's shared rawSeqStore loses them in pass 2) | lenient superset | Strictly more parse options; matching C would drop candidates, any change is a ratio campaign. |
| No C dictionary entropy-table branch in `rescale_freqs` (formatted-dict CTables/FSE seeding of the price tables) | fidelity, superseded | Non-ultra dict frames shape prices via the R58 seed-parse replay instead; the dict-row family measured ahead of C's rows on the fixture (perf/encoding.md's tuning campaign). Porting C's branch is a campaign, not a fix. |
| `bt_low = pos - ring/2` vs C's `curr - (ring-1)` | lenient by one position | Threads one extra below-ring hop; aliasing-safe in both. |
| insert-side per-position floor vs C's fill-target-based windowLow | lenient superset | Admits candidates in `[pos-max_window, target-max_window)` during fills; search-side floors match C. |
| Dict frames probe the first payload position (C's `ip += (ip==prefixStart)` skips it) | lenient, one position | Undocumented but negligible; recorded here. |
| POS_BIAS makes window position 0 a representable candidate (C's matchLow=1 excludes it) | lenient | Documented in the POS_BIAS comment. |
| Insert-side count cap at `idx + OPT_NUM`, clamp_lag gating, `cand_floor`'s stock search domain on dict content | documented deliberate | Recorded in-code and in pitfalls/encoding (the count cap is a measured speed/ratio trade; the dict domain is managed by the skip-shape guard). |

## Verified sound, no finding

Read bounds at tails (hash/count/byte reads bounded by `idx+HASH_READ ≤ block_end`, debug-asserted at both entries), `lit_price` underflow (unreachable — needs a zero-length first block the grid never emits; C has the same shape behind an assert), `predef`-every-block vs C's first-block-only (dead in both — prices are consulted only when the parse loop runs, which needs ≥9/10 bytes), `frac_weight`'s `hb < 23` guard (widest realistic table far below), `lit_length_price(MAX_BLOCK_SIZE)`'s recursion arm (dead in practice — DP stretches end before the cap; todo 25c).

## Pitfall recorded

The shared-cursor hoist trap (hash3's fill cursor rides `next_update`; the R58 eager seed moved one without the other) — pitfalls/encoding.md. The gap is unreachable on the current ladder (every hash3 user row has mls==3, so the 22-bit main tree shadows the 17-bit slot — probed byte-identical on natural and adversarial rep-suppressed fixtures), and the prefill that closes it is todo 25b.

## Follow-ups resolved (R71, 2026-10-24)

- row 16 mls 5 and row 22 search_log 9: both falsified with data ([negative](../negative/matchers.md)) — the unlisted tuned-ladder deviations are measured-optimal as they stand.
- hash3_log clamped to the final window log (`MIN(17, windowLog)`, C zstd_compress.c:2027) — corpus byte-identical, saves the 512 KiB table on forced-small-window opt states.
- hash3 dict-seed prefill landed: the eager seed now fills the 3-byte table over `[0, keep-OPT_NUM)` (the pitfall below closes in code, output-neutral everywhere).
- the dead `lit_length_price(MAX_BLOCK_SIZE)` arm carries a debug_assert.
- The R71 experiments also exposed the forced-window interop bug (unpledged streams at tree rows emit offsets past the declared window; libzstd rejects) — todo 26.

## Commits

- 7d030838 fix(opt): match C's guard on the ultra match+1-literal recheck
