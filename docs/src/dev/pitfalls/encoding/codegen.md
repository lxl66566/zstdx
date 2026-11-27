# Guard and codegen follow-ups
## Sentinels through contracted helpers


Btlazy's depth-0 rep probe priced the incumbent `best` even when the tree search returned nothing. `lazy_value`'s `debug_assert!(len >= MIN_MATCH)` fired on the `{off: 0, len: 0}` sentinel, and `full_ladder_roundtrip` failed in every debug run. Release passed because `s*0/4 - lazy_price(0) == 0`, which is accidentally the correct no-candidate baseline.

The sentinel's semantics must be explicit at the call site, with `incumbent = 0` when `best.len == 0`, instead of relying on len-0 arithmetic degenerating to the right value. The release behavior was never wrong, but nothing tied the two together.
## Keep the btlazy2 rep off-base


Collapsing the longest-rep candidate to `off_base 1` because it is a rep emits the wrong offset whenever the winner is the rep1 or rep2 form. The roundtrip fails only on rep-dense data and far into the stream, found at about 4 MiB on json. Off-base 1, 2 and 3 each name a different decoder action at ll==0, which are swap, rep2 and rep3, and a different plain repcode at ll > 0.
## opt.rs port traps


A debug `eprintln!` under `#[cfg(feature="std")]` is compiled into release builds with default features and once silently flooded stderr. `ZSTD_count` returns a pointer difference, so continuing a count must replace the value and never accumulate it.

Rep history is updated once per series by the path walk, and emitter-side per-sequence updates are the decoder-equivalent semantics, so pick one, because duplicate updates always blow up. The insert-into-tree counting cap belongs in the DP window of 4096 slots, or regions skipped by the parser get re-counted from stale heads, which is the lesson behind text moving from 45 to 313 MiB/s.
## Gain gate meaning


C's +7 is the margin for whether a later candidate replaces the already-held match, not a store gate. Putting the gate on the store decision is stricter than C and deliberate, because our tables are denser, but the cost is that short-distance matches in text get falsely killed.
## Wrap-dependent fused checks


The DUBT diet's fused checks `cand - (floor + 1) >= span` rely on the unsigned wrap of a below-floor candidate resolving to huge, which is the intended out-of-range verdict. The same wrap trips the debug profile's overflow checks, and the whole library suite had been red since that commit with 16 tests while every release gate stayed green.

Wrap-by-design arithmetic must spell `wrapping_sub` even where the release semantics are the point, and a red debug suite on a landing branch is a gate failure, not noise.
## Cost table -1 markers


`ZSTD_crossEntropyCost` maps `norm == -1`, the low-probability NCount wire form, to probability 1 before the `kInverseProbabilityLog256` lookup. The port read the raw i32, and `(u32::MAX << shift)` indexed 4 GiB past the 256-entry table.

It is reachable only through the exact-cost arm for DictEntropy frames, where a literal run of 2051 bytes or more hits the LL default distribution's -1 tail at codes 32 to 35. One holdout file in the rebuilt fixture carried it, and the earlier fixture had that file in train, so the roundtrip gate never compressed it. A dictionary-frame-only panic at a table-indexing site is a wire-form artifact, not memory unsafety.
## Gate dict-cursor updates


The catch-up insert cursor initialized to `u64::MAX` on no-dict frames but advanced `next_ins = pos + 1` after every probe unconditionally. One probe in, the cursor went live, the `pos > next_ins` guard fired on no-dict frames, and json and skewed l9 bytes changed.

The full-ladder dump caught it, and the dictionary-fixture sweeps cannot see the leak by construction. Per-frame feature state that decides fills belongs in the guard itself.
## Full-width mask shifts


The row search's head rotation masked its rotated candidate mask with `(1u64 << entries) - 1`, which is correct for the 16-entry rows it shipped with. At rowLog 6, with `entries` at 64, the shift equals the register width. Debug builds panic, while release x86 masks the count to 6 bits and shifts by 0, so the mask collapses to 0 and every search silently returns no match. No assert fires, and the encoder just emits literals, while the tests that passed with rowLog 4 did not cover the wider instantiation.

The full-row arm now uses `rotate_right`, which is total for any count. `(1 << N) - 1` and `x << N` are total only for N strictly below the type width, so a const-generic width needs its full-width arm written explicitly, and every new instantiation of a width-parameterized path needs at least one debug-mode roundtrip at that width.

A companion edge from the same widening is that an array length cannot key off a const generic through arithmetic, because `[0u32; 1 << ROW_LOG]` wants `generic_const_exprs` and a `const` item in the function cannot see outer generics. Materialize the shipped widths through a compile-time dispatch onto a second const-generic function, `row_search_cap::<ROW_LOG, 16/32/64, _>`, which also keeps the pre-existing width's stack layout byte for byte.
## Live window length is not source size


The small-frame tree-hash screen keyed on `win.len() <= 128 KiB`, but the window slice is ring or buffer capped near one block, so the condition held on every block of every large text frame. It switched the DUBT tree to 4-byte hashing frame-wide, costing 32 MiB text 30% at the Best tier, and only the full 120-cell sweep caught it while every small-payload gate stayed green. Size-keyed policy must read the declared input shape through `params.small_src`, never the accumulated window.

The dual bug is that a row replacement through `return adjust_params(btlazy(...))` silently resets constructor-defaulted flags, so `small_src` came back false and disarmed the same screen for the frames it was built for, costing 115 B on text-16K l9 with all gates green. The symptom class in both cases is per-cell ratio drift with no test failure, and only the sweep's byte accounting catches it.
## Screens are O(samples)


At the fastest tier a 1 KiB to 128 KiB call is 6 to 40 µs, and one extra window pass costs 20% to 50% of it, measured at 47% slower on skewed-16K. The same lesson bit twice inside the screen.

A `[u64; 4]` bitmap indexed as `bitmap[b >> 6] |= ...` compiles to a store-forwarding chain at about 2.8 cycles per sample, and keeping the four words in registers through a `match` fixes it. A runtime-variable bar in the scan's hot loop, with `ml >= min_ml` and `miss >> shift`, costs 22% wall, where the same value as an instantiation constant folds away.

The frame-parser tooling for section diffs has its own map: literals size formats `00`, `01`, `10` and `11` mean 10-bit one-stream, 10-bit four-stream, 14-bit and 18-bit. `read_ncount`'s `nbBits` and `threshold` shrink dynamically with `remaining`, and the reader prefetches, so each table needs a fresh reader before its walk. The NCount header's accuracy field stores `log - 5`.
## Hot loop body shape


Restructuring `rep1_chain`'s tail into the clamp-first form cost balanced json +1.39% Ir while the restructured body is never executed by the bench. A semantically identical revert measured +3.0%, and the landed post-emit `pos.min(block_end)` clamp that kept the base loop shape recovered to +0.44%, with all of it register-allocation placement around the once-per-parse accessor chain.

This is the walk-shape entry's class, and semantic gates such as dumps and tests are blind to it while only gungraun Ir sees it. When hardening edits share a file with a hot loop, keep the loop's instruction shape untouched and attach the guard before or after the loop. Bisect with gungraun per hunk rather than per round, because placement effects do not compose linearly.
## Clamp search domains to the final window


The tree rows' domain was `chain_reach`, the ring's reach, not `window`. A forced `window_log` on an unpledged stream updated `p.window` while `adjust_params`' unknown-length early return left `chain_reach` at the stock 1<<22 or 1<<23, so rows 9, 13-15 and 16-19 emitted offsets past the declared window. Libzstd rejects those, and at w16 its ring silently decodes wrong bytes, gated only by the checksum.

C's legality invariant is `ZSTD_getLowestMatchIndex` at the final windowLog, unconditionally in every search loop, while `adjustCParams` log clamps run only under a known srcSize because they are memory clamps, not legality. The zstdx mirror is the unconditional `clamp_geometry_to_window` with chain, row and bt at most wlog, hash and dfast short at most wlog+1, and the bt ring's two-link-slot layout makes `bt_log` exactly C's cycleLog, with the length-gated downsize kept separate.

Repro shapes for beyond-window redundancy need no near twins, and the `farfrags` and `twinhalves` classes in `encode_edges` cover them, while near-periodic shapes with a period at or below the window never catch the class. The audit is in [winsem](../../review/winsem.md).
## Hoist every cursor of a moved field


C derives `nextToUpdate3 = ms->nextToUpdate` at block entry. The eager dictionary seed moves `next_update` to `keep - OPT_NUM` without moving hash3's coverage, so the frame's first block inherits a hash3 fill hole over `[0, keep-OPT_NUM)` where the lazy flow fills contiguously from 0.

It was probed unreachable on the current ladder, because every hash3 user row from 18 to 22 has `mls == 3` and the 22-bit main tree hashes the same 3-grams and re-finds any candidate the 17-bit slot would offer. Cached-versus-fresh output is byte-identical on ordinary and adversarial rep-suppressed fixtures. The unreachability is a property of the ladder, not of the code.

A seed-time hoist of a field that other cursors derive from owes a fill, or a reachability proof, for every derived cursor. The proof must cite what makes the gap unreachable, here the mls shadowing, rather than a byte-exact by construction argument over the tables that happened to be audited.
