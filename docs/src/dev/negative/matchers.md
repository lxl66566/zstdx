# Falsified directions: encoding matchers

Every entry here is backed by A/B data. If the premises change, such as the table structure, window, corpus or CPU, re-evaluation is allowed, but new decomposition evidence must come first. The differential-testing discipline is in [benchmarking methodology](../bench/methodology.md), and the rules are in the [index](../negative.md). Where an entry uses the x notation, x is our time divided by the reference time, and values below 1 mean we are faster.

## Fast scan loop

### Folding the two-regime scan entry

Folding the two-regime scan-entry arithmetic re-anchored the window in `fast_epoch_advance` so `fast_shift ≤ win_base` always, then deleted the `off` slice and the saturating rebase from both parse entries and the advance's `max` floor. The fold was byte-identical by construction, held the full-ladder dump gate, passed the 6-GiB replay, the translational-invariance check and all tests on 2026-10-17. gungraun fold-against-base read fastest json -1.57%, text +1.91% and skewed -0.13%, fast json/text/skewed -0.09/-0.24/-0.21%, and random and zeros flat, for a net -0.4% aggregate Ir with one real loss cell. Wall was unadjudicable at this effect size, because sequential prof blocks read json.fast anywhere in 419-474 MiB/s with about 6% spread even across byte-identical base binaries from two forced recompiles, and each profile invocation samples only about 1.4 s, so the fold's 431-447 readings proved nothing, while text.fastest stayed flat at its 13 GiB/s memory-bound ceiling.

The entry cost was a one-time register-allocation roll and not a live cost, because the folded arithmetic is a per-block prologue at about 0.001% of executions. Deleting it re-rolls both scan bodies' allocation without buying anything reproducible, so the two-regime entry form is the closed price of the earlier change. Two facts stand for future readers. The giant-window regime of `shift > win_base` is defensive only, because every reachable caller adopts per-block sliding windows in bulk `emit_slice_blocks`, MT `compress_job_blocks_inner` and the reach-probe and donation loops, all sliding at `window_size` or `overlap`, and owned windows cap at window plus block of about 128 MiB, far below the 2-GiB span. Its dormant `catch_up_insertions` u64 underflow is debug-only and unreachable, so it needs no fix unless a single adoption above 2 GiB ever appears. Reopening the fold needs a reproducible register-negative case.

### u32 candidate resolve

The idea was a u32 idx-space candidate resolve for the fastest tier's dense body, replacing the u64 `dist = pos−entry+1`, `reach` and `(pos−win_base)−dist` chain with wrapping u32 arithmetic off the already-live idx and pos halves. It is provably byte-identical below 4 GiB, since `pack_pos` truncates identically and true distances are window-bounded, and it was hosted as a 4-argument `#[inline(always)]` helper and as bare per-site expressions reusing the plain arm's `reach`. The helper moved dense-fn Ir from 62.43M to 64.45M, or +3.2%, on json 4 MiB, and gungraun read json +0.31%, text +1.05%, skewed +0.03%, random +0.02% and zeros +0.08%, while the manual form read 63.99M at +2.5%. The above-4-GiB rejection is closed by rebasing the coordinates instead of the arithmetic, through the `fast_shift` epoch rebase at the parse entry recorded in [matchers](../perf/matchers.md), while the u32-mechanism falsification stands.

The arithmetic saving is real, but the loop's register budget re-rolls on any structural edit to the resolve region. Block-level attribution of the manual form shows the resolve-adjacent blocks shrinking by 2.4M, because the old 44-instruction resolve block's rejoin, the miss-ramp constants and a dead bar block went away. The pair-head block grew from 4.99M to 6.93M at +7 instructions per pair, through spill reloads displaced into the loop head, plus a new resolve merge block at +0.54M and the extend entry at +0.39M. The merged const-`if` select extends entry and reach live ranges, and LLVM moves the spills from the post-probe rejoin into the per-pair head, for a net loss in both hostings. This is the same register famine as the dfast position-u64 carry and the gate-extension falsifications: only register-negative changes convert. One residual observation matters. The u64 resolve rejects every candidate once absolute pos reaches 2^32, because entry truncation makes `dist >= 2^32` for in-window entries, a latent defect for streams above 4 GiB that the u32 form would have fixed.

### Incremental idx and pos32 cursor

The idx and pos32 cursor conversion was the register-removal candidate left open by the u32-resolve falsification. It made the window index the maintained cursor so every u64 absolute position leaves the per-pair path. In v1, pos32 was carried as a second incrementally maintained IV and emit landings converted through u64 as `win_base + anchor_idx` in and `(new_pos − win_base)` back. In v2, anchor, emit and loop bounds were all kept in idx space through new `emit_at` and `rep1_chain_at` helpers, pos32 was re-derived per pair from a constant `win_base32`, and every u64 operation between loop head and back edge was removed. Both were byte-identical under the full-ladder dump gate. v1 read json +0.79% and text +1.28% Ir, with L1 hits up 2.0-2.3% and estimated cycles up 1.9-2.1%, while v2 read json +0.25%, text +1.62%, skewed +0.06%, and random and zeros flat, with cycles up 0.85% and 2.0%.

Codegen attribution of v2's plain body shows why. The base keeps `pos` as the IV and gets pos32 free in the register's low half at both table stores through `lea r8d, [rbx+1]`, paying one `sub` for idx at the addressing sites. The idx-IV form inverts the exchange: `win_base32` spills to `[rsp+96]` and reloads every pair, three 3-operand LEAs rebuild pv0 and the store values, and the hash constant de-hoists into an in-loop `movabs`. `prev0` and `prev1` spill per pair in both forms because their live ranges span the rep-probe block by design. The loop sits at a cursor-domain optimum, where pos-as-IV is strictly cheaper because three use sites want pos-derived values and only load addressing wants idx, and the u64 IV donates its low half as pos32 for free. The u32-resolve family is therefore falsified in all four hostings: pos-IV helper, pos-IV manual, pos32-IV and idx-IV. The above-4-GiB candidate-rejection regime itself is closed by the `fast_shift` coordinate-epoch rebase at the parse entry, which keeps this loop's arithmetic untouched as recorded in [matchers](../perf/matchers.md).

### Steady and tail scan-loop split

The steady and tail split folded `pair_len` to a constant 2 with a full-pairs steady loop and a single-position tail epilogue. It cut fastest json and text Ir by 3.3% and 2.7%, but text.fastest wall fell 7%, from 12201 to 11347 MiB/s, reproducibly in both prof and the interleaved matrix. The third macro instantiation displaces the hot blocks, and at 12 GiB/s the loop pays pure layout noise, so instruction-count wins that do not survive the layout lottery are not wins.

### dfast position-u64 carry

The dfast position-u64 carry rode `v0 = read8(ip)` across the inner-loop back edge, feeding the next short hash, the long and short probes' current-side compares and the rep pre-probe, saving 4 loads per position. fast json, text, skewed and random Ir rose 3.6 to 6%, with json at 31.80M to 33.12M, while wall stayed flat. The removed reloads were L1 hits, and the pinned register displaces hotter pipeline state into spills. Operand-supply savings must not be bought with a live value inside a register-saturated loop. In contrast, deriving `cur` from the hash load within one iteration, where no new live value crosses the back edge, is a clean win.

### Rep-probe halving

The fast-loop rep-probe halving dropped the first-position probe and imported libzstd's ip2 one-compare-per-pair policy. json.fastest grew from 5459976 to 5719320 bytes, +4.75%, with r moving from 6.146 to 5.867, below zstd-1's 6.11, for +3.6% wall from 530 to 549 MiB/s, while text, skewed and random ratios were unchanged. libzstd's fast covers the ml 4 to 6 band with its hash4 probe, so halving its rep compares is free. Our fast is hash5 with an ml-at-least-6 bar, since hash4 was falsified, which makes the every-position rep probe the only detector of that band, and json's short structural-token repeats live in it. The first-position probe's `of_value-1` emit also keeps rep history for the chains. The rep probe is load-bearing, so cut its branch cost through the OR-fold and never its count.

### Rep pair prefilter

Folding the fast loop's rep pair prefilter as `(a | b) == 0` changed the output, adding 1238 B on json. The expression tests that both xor-flags are zero, not the OR of the two zero tests, so the probe fires less often and the parse changes. When folding two zero tests into one branch, the boolean fold must be `(a == 0) | (b == 0)` on the comparison results, because folding the operands silently changes the predicate's meaning.

### Fast miss-run stepping

Fast miss-run stepping doubled the pair advance after N consecutive misses, on the miss tail only, with `insert_covered` untouched. Three rescue and gate variants were tried. With no insert, N=4 moved json from 5,466,381 to 5,292,466, or -3.2%, and wall from 567 to 617 MiB/s, x1.510 to x1.389, but text grew from 108,531 to 116,519, +7.4%, at -9% speed. N=8 read json -1.25% and text +2.9%. Dense insertion still cost text +2.3% and moved skewed.fastest wall from 2786 to 1270, -54%, at the 2^15 random-store floor. The span-EMA gate cost text +7.2% and the ml-EMA gate text +3.6%.

json's weak mid-run matches are net-negative, so skipping them wins size and speed, but text's short matches are its payload, and text's damaged regions are locally json-shaped, with dense short-match stretches between giant matches. No parse statistic separates the classes, which generalizes the chain-tier cold-gate lesson. The residue worth keeping is that json.fastest's parse is weak-match-polluted, so a shape-safe acceptance-bar or fragmentation lever on match-dense data is the open follow-up.

### Batched probing of the miss tail

Search-side batched probing of the fast miss tail placed pair-of-pairs lanes exactly at the scalar pairs' positions and ramp trajectory, with pre-store gathers, one exit branch per four positions, and the first-hit lane handing back to the pair loop. Output returned to baseline once the lane layout reproduced the scalar pairs, reading json +0.05%, text -0.3% and skewed identical, while a strided-spread lane layout lost json +1.7%, at r 6.04 under the zstd-1 line, and text +10.6%, so lane placement and not probe density is load-bearing. gungraun zx_fastest json Ir rose from 29,369,831 to 30,484,521, or +3.8%, and interleaved wall read json.fastest 567 to 500-511, -10 to -12%, text.fastest 13,368 to 11,522, -14%, and skewed 2786 to 2875, +3%.

The mispredict budget is not concentrated where batching amortizes. json's miss runs are 1-2 pairs at an average ll of 2.67, so each run pays batch entry, one iteration and the dropout re-probe instead of two cheap pair iterations, which raises instructions and lowers wall. Only long-run shapes gain. Batched and AVX-512 probing of the search side is closed for the fast tier's short-run regime, and the vector flavor adds gather and pack cost on the same losing structure.

### rep1_armed in the OR-fold

Folding `rep1_armed` into the hash-compare OR-fold's condition used `(m0 == 0) | (m1 == 0) | rep1_armed` as a single branch in v1. In the json-64K LBR, the armed flag's spilled byte reload sat on the hot branch's input path, at `cmpb 0xb8(%rsp)` plus `je` in 4.3% of samples. The restructured v2 keeps the fold register-only as `m0 | m1` and enters the armed-but-no-hash-hit case through its own rarely-taken branch. An OR term folded into a hot condition must be register-resident, because a spilled bool puts its reload on the critical path of every iteration, armed or not.

### Static-price acceptance bar

A static-price acceptance bar on the fast tier's hash path declined a hash match when the displaced literals at a flat price could not cover the offset bits, using `ml*3 >= ilog2(dist)+7`, with declines falling through to the second-position probe. json.fastest moved from 5,466,385 to 5,367,850, -1.80%, but text.fastest grew from 108,535 to 110,158, +1.50%, and the 4-bit static form declined nothing new because ml=6 only appears past 256 KiB. The shapes' displaced-literal prices straddle every static constant, with json at about 3.3-3.6 bits/B and text at 4.3 or more because its short far matches are payload. This repeats the chain store gate's flat-margin lesson, and only per-shape pricing fed back from the Huffman lengths separates the classes.

### Fed-back literal-price bar

A fed-back-literal-price acceptance bar on the fast tier's hash path ran `pays_for_offset_lit` after backward extension, gated the gather on a `dist >= 1024` compare, and fed declines into the miss ramp, hosted with the gather inlined and with the gather outlined as `#[cold]` and the bar state carried as a raw pointer. The mechanism was shape-safe: json.fastest read -1.24 to -1.27% in all four modes, bulk-ST from 5,466,381 to 5,392,573, text -14 B, skewed -55 to -384 B, and the other 108 cells byte-identical. Wall regressed to json.fastest -4.5% and text.fastest -12 to -13% over 3-4 interleaved rounds in both hostings, and even a DORMANT build with the gate at `dist < 1<<48`, where the bar code never fires and output is byte-identical to base, lost the same text 12% and json 4%.

The fast scan loop cannot host even a never-taken bar, because the compare and branch, the bar state crossing the back edge and the decline paths re-roll the register and layout dice of a saturated loop. text.fastest at 12-13 GiB/s pays pure code presence. The pollution itself is recoverable and the size win is real and shape-safe, so the direction was reopened and landed the same day through the per-block dense-mode scan instantiation in [matchers](../perf/matchers.md), which reproduced json -1.35% bar-only exactly, kept text output byte-identical, and cut the plain cell's residual cost by 1.8% wall.

### insert_covered loop widening

Widening the `insert_covered` dense loop from 2 to 4 scalar lanes while preserving ascending store order moved dll100 fastest in-function samples from 413.7M to 512.6M, +24%, with wall flat over 11 interleaved rounds at 352 to 338-350. The post-change profile flattens, with the top instruction at 3.3% against the store's 9.9%. The dense fill is uop-volume-bound, at load, and, imul, shr and store per position and about 36M positions per 100 MB at l1, and never latency-starved, so extra lanes only widen the working set. The only lever is indexing fewer positions through a density-coupled policy change.

### dfast miss advance counter

Moving the dfast miss advance to libzstd's `nextStep` counter, with `step++` per crossed 256-byte grid boundary and a `while` catch-up for multi-boundary jumps, is trajectory-identical to our `1 + (run>>8)` recompute by construction. callgrind on dll4 moved from 138.0M to 147.8M, +7%, because scalar evolution turns the catch-up `while` into a cold division block and `step` and `next_step` occupy two back-edge registers, while the window pointer's liveness dies with them. The register budget and not instruction count is this loop's binding constraint, and C's counter form pays off for gcc, which spills both to the stack, while here every added live value re-rolls the whole allocation.

### dfast rep-probe delta hoisting

The dfast rep-probe delta hoisting followed libzstd's `1 - offset_1`, folding the candidate address into the probe load's displacement, clamping per pass and using a self-compare delta for frame heads. Two veto shapes were tried, an eager `(delta != 1 | probe >= rep0)` and a single per-pass `rep_guard` value folding the veto to one compare. Both regress on dll4 callgrind, by +14M and +21M Ir over the 138M const-log baseline. LLVM hoists the veto flags as `setne` and `setae` per iteration, and the delta, guard and rep0 live set pushes the window and table pointers back to the stack. Byte-exact C-parity restructures that add live values lose regardless of nominal savings. C's cheap form also relies on its per-block rep clamp, while our per-position bound, at the frame-head transition of `ip = rep0-1` that is load-bearing on zeros-class heads, forces the extra state, so C's exact form is not reachable byte-identically at all.

### Stride-2 covered fill

Applying stride-2 covered fill to every block without a density gate grew dll100 from 48,825,383 to 48,845,668, +0.042%, but json from 5,466,385 to 5,515,496, +0.90%, and text from 108,535 to 110,055, +1.40%. Pair probing makes stride 2 phase-complete only where repeated content has many copies, as dll's mass twins do dozens of times, while a unique json or text tile must be hit at the shifted phase itself. The second parity's value is 20x shape-skewed, so the thinning ships density-gated through the per-block `avg_ll` and `nseq` gate in [matchers](../perf/matchers.md).

### Dual-anchor covered fill

The dual-anchor covered fill followed C's fast policy of `start+2` and `end-2` only. Ungated, it moved dll100 from 48,825,383 to 49,050,988, +0.46%, at 356 to 381.5 MiB/s, +7.3% wall over 7 interleaved rounds, but json read +1.58% and text +3.04%. Gated on the same 77.7% fill coverage it read 49,088,233, +0.54%, at +3.7% wall. The wall is real, but the dll100 size axis is the gate, following the fill-lag clamp precedent where a -0.65% dll100 loss blocked an unconditional opt win. Stride 2 keeps the phase coverage at 1/12 of the size cost and even wins 0.043% back on churn, so it is recorded as the speed-priority knob if the tier's target flips.

### Stride-2 peel-then-pairs

Stride-2 unrolled peel-then-pairs inserted pairs stepping by 4, peeled the odd tail and caught the trailing single slot. dll100 moved from 359 to 355 MiB/s, -1.2% over 9 interleaved rounds, with output byte-identical to the rolled form. The modal gated match holds 3-4 stride-2 inserts, so the unroll's peel branch and tail check execute on nearly every call and cost more than the pair iteration saves. This repeats the dense loop's counted-form lesson.

### Parse-statistic gates for dense mode

The idea was to select dense mode from parse statistics, with an nseq floor and an `avg_ll` ceiling recalibrated from the CoveredFill recipe. text's match-dense blocks sit at nseq 2133-9420 inside json's 6560-6799 band, and their `avg_ll` of 1.37-2.64 overlaps json's 2.60-2.83. At nseq of at least 512 alone, text's four dense blocks, holding 99.5% of its sequences and 99.8% of its literals, enter the dense body, which changes output and brings back the stepping damage, and `avg_ll` ceilings cannot exclude them without excluding json. No parse statistic separates the classes, because text's damaged short-match stretches are locally json-shaped up to block-level aggregates. The separator is content-side, the fed-back literal table's covered-symbol count, which reads json 11-25, text 78-83 and dll at a minimum of 43 at the stats bar.

### Dense-mode miss thresholds

Dense-mode miss-run stepping thresholds were swept off the winning point on json.fastest bulk-ST with deterministic sizes and a baseline of 5,466,381, with the bar always on. `miss_count > 1` read 5,653,733, +3.6%, `> 4` read 5,821,308, +6.5%, `> 6` read 5,303,760, -3.0%, and `> 8` read 5,398,206, -1.25%, while the winner `> 3`, equal to >2 except for tail pairs, read 5,252,610, -3.91%. The doubled advance re-rolls the scan trajectory of which positions insert and probe, and the parse cascade amplifies. The size response to the threshold is non-monotone with catastrophic neighbors on both sides of the plateau, so the winner is a corpus-tuned constant and should be treated as calibration and not as mechanism.

### DenseFlat covered fill

`CoveredFill::DenseFlat` made the 16-or-fewer fill a clamped 16 or 8-slot straight-line unroll whose tail slots re-store the final position, table-state-identical by construction and byte-equal over a 30-file full-ladder dump, killing the pairs loop's exit and the parity peel, the tier's top mispredict lump. Branch misses fell from 88.9M to 81.9M per 40 iterations, -7.9% or -175K per encode, and branches fell 250K per encode. Instructions rose from 38.70G to 41.77G, +7.9% or +76.6M per encode at about +73 per call over the 1.045M at-most-16 calls. Solo cycles rose from 11.25G to 12.07G, +7.3% on json 32 MiB perf stat, with interleaved wall at 594.5 to 580 MiB/s in one drift-band round. The unrolled slot costs about 10 instructions, with a clamp cmov, window-index re-derivation and pack-position increment per slot, where the rolled pairs loop pays about 6 per covered position by amortizing address math and position increments across the 2x unroll. About 2.6M cycles of harvested mispredicts cannot fund +76M instructions at any waste level, even the ml 13-16 mode band at 80% of calls. The covered fill's branch structure is protected by the same store floor that falsified the AVX-512 and lane-widening variants, and only indexing-volume changes can move it, but those are ratio-falsified.

### Covered-symbol bar at 48

Running the dense-scan covered-symbol bar at 48, the `HEAD_SYMS_MIN` value, instead of 32 grew dll100 l1 from 48,804,295 to 48,804,754, +459 B. One ELF block passes the stats arms at covered 43 because its literal price is 6.16 bits/B and far matches are payload there, and the bar plus stepping changes its parse. The 48 bar leaves a dll leak inside the json-at-most-25 to dll-at-least-43 gap, while 32 sits mid-gap and restores dll100 l1 byte-identity at zero json cost, since in-blocks max out at 25.

### Const-generic hash log on the plain body

Instantiating the const-generic hash log for the fastest loop's PLAIN body too, with all four RAMPED and DENSE instantiations at H15, grew the dll4 fastest scan from 80.82M to 81.94M Ir, +1.4%, at wall parity, while json4's dense body with the same mechanism read -3.4%. Removing a provably-live register, the `shr %cl` shift count, is still a per-body regalloc re-roll, and the plain body's roll consumed the register elsewhere and added spill traffic. The landed cut scopes the const row to `ScanDensity::Dense` blocks only, where the roll measured a win on 2026-09-18.

### Head-slot prefetch in fills

Software prefetch of the head slot in the chain fill loops, with a lookahead-8 pipeline on `catch_up_insertions`' dense Chain arm and `chain_grid_fill`'s strided arm through x86 T0, produced byte-identical output but flat wall everywhere. text.balanced ST solo read 1140-1156 against 1149 MiB/s, which is noise, the `catch_up_insertions` profile share moved from 7.2 to 7.5%, and text.balanced stream-mt8 read x2.054 to x2.056. The fill loops' per-iteration table loads are independent across iterations with no pointer chase, since the head value feeds only that iteration's own stores, so the out-of-order engine already overlaps them through the load queue. Hash-table prefetch pays on dependent walks such as the chain walk's link chase, not on bulk fills.

### Fast accept bar for small frames

Lowering the fast row's accept bar alone from `ml >= 6` to 4, with no ramp change, worsened the text-4K l1 ratio because short noisy matches were priced as payload. The bar and the miss ramp are one policy, and they landed jointly as `ml >= 4` with step `1 + miss>>7` behind the alphabet screen, accepting short matches only where the scan also covers every position.

### Insert-skip acceleration

Insert-skip acceleration for the small-band fast row had no effect and was reverted, because the miss-step ramp already owns that axis.

### min_match retuning

Retuning `min_match` at the fast row for small inputs moved the ratio by about 2 B of noise, and the accept bar and not the row's `min_match` is the small-band knob.

### Dense bars below 2 KiB

Dense bars for the fast row below 2 KiB unconditionally, with no alphabet screen because sampled counts cannot separate the classes there, cost skewed-1K l1 15.6% wall with byte-identical output, and cost json-1K 5.5% for -7 B. Even at 1 KiB the sampled screen separates, with json at 38 and text at 46-62 over 512 samples at stride 2, so the always-tier is pure cost on the narrow classes.

### Full-window distinct-byte scan

A full-window distinct-byte scan as the alphabet screen cost skewed-16K fastest 47% wall. The scan is O(n) with a store-forwarding bitmap chain and takes about half the whole call. Small-band screens must be O(samples), and 128 or 512 strided reads decide the same verdicts at 3% or less of the call.

### Runtime-variable parse bars

Runtime-variable parse bars, with `ml >= min_ml` and `miss >> ramp_shift` locals in the fast row's hot loop, cost text-16K fastest 22% wall against the same values as instantiation constants. LLVM folds `if CONST {4} else {6}` but keeps a runtime bar live in the loop, so small-source bars ship as the `SMALL_WIDE` const generic.

## Fast-tier hash and table geometry

### hash4 revivals

Four hash4 revivals were tried: a short-table side table at 2^13, a 16 KB distance cap, a single table at 2^14, and a rep0 fallback. All failed, with json at 4.18-4.93 and all worse. Table pollution from high-frequency 4-byte patterns is irreversible, and hash5 is settled.

### hash4 near-distance side table

A hash4 near-distance side table with an ml-at-least-6 gate, intended to cure pollution, cost 7 to 14% speed across the board with a flat ratio. The ml gate does cure the pollution, but the gains cannot cover the dual-table cost.

### Probing rep1 and rep2

Probing rep1 and rep2 as well raised the hit rate from 7.6% to 26%, but json, text and skewed ratios all worsened. Weak rep matches steal positions from longer hash matches, and libzstd-fast probing only rep0 is right, while a trailing rep2 probe also degrades slightly.

### rep0 threshold

Raising the rep0 threshold from 4 to 5 is neutral, because ml=4 rep matches are too few.

### HASH_LOG 17

HASH_LOG 17 grew json +0.6% but cost 8% speed, and the skewed ratio worsened.

### Tiered MIN_MATCH

A tiered `MIN_MATCH` that requires ml of at least 5 for large offsets had zero effect, because far ml=4 matches are rare anyway.

### Fastest window

A Fastest window below 448K or at 2 MB was measured. 256K and 384K are catastrophic, with text ratio moving from 301 to 7.88, and 2 MB is worse. Cutting the tile period is catastrophic, and 512K is workable but 0x70000 is more cache-friendly, with 768K later unlocking cross-tile matches.

### dfast window

A dfast window of 2 MB was re-evaluated and landed on 2026-09-12. The earlier -1.1% to -4% json speed loss did not reproduce under the u32-table matcher, and json now reads parity ratio at +4% speed with text at +11%. The original falsification was corpus-only, on 32 MiB with no large-binary shape and before `327bc99`, and the 100 MB ELF corpus showed the 1 MiB window itself cost 26-28% ratio there, with libzstd-3 at wlog 20 reproducing our old bytes within 0.03%. Check [matchers](../perf/matchers.md) before retesting windows on new shapes.

### Chain window of 4 MB

A chain window of 4 MB was a double loss on every shape, with skewed at 14 MiB/s. It finds farther but not longer, because the offset code-position loss exceeds the length gain, so window expansion must come with LDM. That conclusion holds for the 32 MiB corpus shape, and W21 at 2 MiB landed on 2026-09-12 as a different point on the trade, with dll -30% size and json.balanced -14.6% for -14% solo speed, once the corpus included large binaries. Window A/Bs are corpus-set-sensitive, so re-run both extremes before citing either direction.

### C's anchor-distance miss formula

Porting C's anchor-distance miss formula to fast and chain regressed across the board, with the skewed scan description about 80x slower. We use stride-1 dense insertion and newest-wins single-candidate tables, where per-byte probing overwrites good far entries with garbage, and the C formula's premises of a sparse table and a large hLog do not hold. It holds for dfast only, in `0b46f21`.

### Dual-anchor table insertion

Pure dual-anchor table insertion, libzstd fast's default, read json 5.71-5.90 and text 275-290. The ratio loss is severe, and dense insertion for 16-or-fewer matches must be kept.

### In-match insertion step

Raising the in-match insertion step from 1 to 2 moved the json ratio from 6.02 to 5.97, which is not worth it for json's 2% speed.

### Zero-slope probing

Zero-slope full-density probing, or a slope relaxed to `mc>>3`, collapses json, skewed and random. Insertion volume rises, which ages the 2^15 table faster, so far matches die, and the dynamic miss slope is essential.

### hLog16

hLog16, a 512 KB table from the 2^15 era, cost 8 to 14% speed with no ratio gain, because it is beyond L2 locality.

### Fastest hash_log sweep

The fastest-tier `hash_log` sweep of 13, 14 and 16 against the landed 2^15 was the last open knob. Each build was identical except for the `HASH_LOG` const, so the row, the initial allocation and the dense-body const-log instantiation moved together. At 2^14, json moved from 5,250,281 to 5,241,622, -0.16%, with solo cycles -5.6%, interleaved wall 602 to 637 MiB/s at x1.424 to 1.337, mt8 3727 to 3936, and small json +1 to +2%. Text grew from 108,512 to 110,863, +2.16%, with r 309.2 to 302.7, flipping the +0.09% zstd-1 parity to -2.03%, and dll100 grew from 48,815,803 to 50,962,327, +4.41%. At 2^13, json read -0.35% at cycles -8.7%, wall 662 at x1.297, and mt8 +9.1%, text +5.99% and dll100 +6.85%. At 2^16, text read -1.06% and dll100 -2.40%, json +0.07%, but json cycles rose +6.5%, dll100 +7.0%, wall -6.1% and mt8 -5.0%.

The size axis is a monotone json-speed to far-match-ratio dial. The smaller table's newest-wins proximity bias wins json's near-field repeats outright, denser and faster, while text's unique tiles and dll's far twins die to overwrites. The gain and loss split is not gateable by parse statistics, since json and dll sit in the same dense band and only text's alphabet separates through the covered-fill gate's separator. The size axis vetoes every alternative point, because dll100's +4.41% is 10 times the dual-anchor precedent's rejected +0.46%. 2^15 is the ratio-chosen optimum and the 1.5x per-event instruction volume is its price.

### Full u64 hashing

Full u64 hashing without 40-bit truncation wins json both ways but costs text 7% ratio. The 40-bit mask keeps mathematical equivalence, and global ratio takes priority.

### u32 table entries at Fastest

Changing table entries from u64 to u32 at Fastest 2^15 with a 2-bit epoch scheme gave identical output at +0.09% instructions. Entry width is not a lever while the table lives in L2, which contrasts with Balanced's 16 MiB working set, where u32 is a big win in `327bc99`.

## Chain and lazy walks

### lazySkipping port

A full lazySkipping port for chain was flat over 11 interleaved rounds, with skewed +3.1% and text -0.7%. The C mechanism's two levers of dense to sparse insertion and slow to fast stepping are already spent in our design, so miss-segment strategy directions are closed.

### Lazy-walk cheapening

Four lazy-walk cheapening variants were tried: depth-1-only margins, a cap of 4 steps, skipping the second-chance search after an empty probe, and a rep probe when the incumbent is already a rep. Depth 1 collapses the ratio, with json 6.55 to 6.18 and text back to 361.5, while the cap and empty skip change nothing or trade 0.05 ratio for 2.5% speed, and the rep-versus-rep probe never alters selection, with byte-identical output without it. The alternating depth-1 and depth-2 margin structure is the gain. The walk is not deep, and the two-searches-per-failure baseline costs the about 30% json instructions.

### Lazy walk pre-reads

Post-walk issue of the lazy walk's pre-reads, gated on the walk actually running so long matches waste nothing, moved dll from +4.5% to +0.5%, json from +2% to -2%, and text not at all. The head loads must issue before the incumbent walk, because after its loop they decode behind the walk's poorly predicted exit branch and the shadow evaporates. Out-of-order overlap does not rescue bad issue placement across a data-dependent loop exit.

### Dual lazy pre-read

A dual pre-read for the lazy walk, covering pos+1 and pos+2 for the depth-0 rep probe's advance, cost dll +4.5% and json +2% but text -2.5% wall reproducibly. The scan body's added straight-line code alone re-rolls text's layout badly, and gating the pre-reads off on long matches did not recover text, only removing the code did. Halving to a single pre-read returned text to +1.4%, because adding code to a hot loop is never free, executed or not.

### Chain-scan spill relief

Chain-scan spill relief through a seeded and steady split removed `seed_offset`, `seed_hits`, `seed_budget` and `rep_pending` live values so the walk's stack-operand parameters could win registers. The split landed on 2026-09-13 with balanced Ir json/text/skewed at -0.38/-0.78/-0.44% and wall flat on every shape, at json 86-88 and dll100 106-107. The four live values were not the spill driver. Cycles and L1-miss samples concentrate in the same two steady walk copies, at 82% and 93% on json 32 MiB, at about 66 cycles per step, which is one serialized L3-class load. The walk is a latency-bound chain-link chase over a 4 MiB chain table and 1-2 MiB candidate distances, and the per-step stack operands hide under the load latency. After inlining, the instruction profile is flat with a maximum of 2.5%, so Ir-side edits to this loop no longer convert to wall. The split stays for structure and deterministic Ir, but further register-budget work on the chain loop should not be expected to move wall.

### Chain miss-step grid

Moving the chain miss-step to libzstd's slow grid of `1 + miss>>8` with kSearchStrength moved text.balanced from -2.78% to -2.12%, +0.66 points all in the cold-start tile, but json fell from 90 to 76 MiB/s and skewed from 2094 to 19 MiB/s. Dense probing implies dense insertion because every probed position inserts, and dense insertion over skewed's literal half dies on the 12 MiB head and chain random-store floor at about 2 stores per byte, the same floor as the fast-tier insertion falsification. The chain tier's sparse speed is the skip, and the fast tier's `>>2` ramp is load-bearing here too.

### Dense back-fill of miss gaps

Dense back-fill insertion of chain miss-skip gaps followed libzstd's insertAndFindFirstIndex model, with sparse probing, dense insertion and the `>>2` ramp kept. It moved text from -2.78% to -2.42%, +0.36 points, but skewed fell from 2094 to 80 MiB/s. The back-fill pays the store floor per skipped byte, and the ratio gain is smaller than the probing-side variant, because insertion without probing only lets later probes find the skipped content while the reference matches start at the skipped positions.

### Cold-gated miss-step variants

Cold-gated variants of the same change froze miss-step growth when the head slot is empty, expecting cold tables to probe at one-load price, in branchy and branchless forms. text moved from -2.78% to -2.12% and json stayed flat at 89, but skewed fell from 2094 to 1309 MiB/s. The empty-slot signal does not separate a cold table from a sparse table, because under sparse probing skewed's literal-run 5-grams are mostly novel in the table, so the freeze keeps the step at 1 and every literal byte is probed and inserted again. No cheap discriminator between cold-start misses and garbage misses was found inside the scan loop.

### insert_covered guard removal

Removing the `insert_covered` parity guard in counted forms used a plain `for k in 0..count` and a hand-unrolled `while k + 2 <= count` with `start + k` addressing. The plain counted form cost +3.3% Ir on json and text, with LLVM keeping it rolled and paying full loop overhead per insert. The hand-unrolled form cost +4.3% through per-iteration u64 re-derivation of the absolute position. The mod-2 unroll with its mispredicting parity guard was already the cheap form, and the landed peel-then-pairs variant is the only shape that keeps the 2x unroll without the guard at about flat Ir.

## DUBT and optimal parser

### Armed-dll residue hypotheses

Three hypotheses for the dll100 fastest w8 gap in 2026-09-26 were all falsified on counters, `seq_dump` and perf stat, with no variant build needed: more segments, probes or emits than libzstd's `ZSTD_compressBlock_fast` and `ZSTD_ldm`, the armed path's own machinery, and table or window miss traffic. On dll32 armed l1, positions probed read ours about 2.66M against C's 2.77M, and sequences decoded through the hook read ours 549,287 at an average ml of 51.6 against C's 567,316 at 49.9, so ours is 0.31% denser at fewer events. The armed machinery is cold per encode, with clamp-at-split 160, coverage drops 0 and invalid wholesale candidates 0. L1 misses read ours 9.8M against C's 18.3M per run, while whole-process callgrind Ir read 634.7M against 589.7M, 1.08x, with the ST wall at 1.53x from 787 against 1204 MiB/s.

The gap is per-event Ir of about 98 against 71 per probed position, and cycles per Ir inside the fast scan loop. That is the known register-saturated codegen deficit whose structural levers are all closed in this table: u32 resolve, idx and pos32 cursors, geometry and batched probing. Do not re-measure event volume or memory on the armed dll cells, and do not read whole-process Ir parity as wall parity, because the wall gap exceeds the Ir gap and the scan's cycles per Ir is the expensive side.

### Opt ring widening

Widening the opt family's ring past libzstd's own geometry put rows 16-19 at bt_log 24, or 16 MiB positions and a 128 MiB u32 ring, on top of the landed far-class floor lift. C's own -19 `--long=26` ring is btLog 23 at 8 MiB positions with clog 24, which the lift already matches, and this went one beyond. dll100 st l17 moved from 17,361,153 to 17,303,439, -0.33%, and l19 from 16,587,043 to 16,560,891, -0.16%, for an 8x ring from 16 to 128 MiB, with a reproducible l19 slowdown of 13.9-15.5 s against the lifted-stock 12.9 s because the ring working set doubles and the deep 8-16 MiB descent adds count work. The dll100 far-class value beyond 8 MiB is terminal-candidate material, priced by the landed floor lift at one compare per descent branch, and giving it a full 128-compare descent buys tenths of a percent at 8x memory. C stops its own ring at 8 MiB positions for the same reason, so widening past windowLow parity is closed.

### Far-class floor lift on Best

The far-class floor lift on the Best tier for btlazy2 rows 13-15 used the same `max_window -> window` lift the opt family landed, and C's `ZSTD_DUBT_findBestMatch` is windowLow-bounded too, so it is C-parity. dll100 st l13/l14/l15 moved from 19,589,827/19,531,220/19,511,172 to 18,675,489/18,593,373/18,531,732, or -4.7/-4.8/-5.0%, with l15 4.5% denser than `zstd -15 --long=26` at 19,398,347, and dll32 l15 read -1.4%. Instructions rose from 44.4G to 88.8G, 2.0x under perf stat ST, and bulk-mt4 wall from 8.8 to 36.2 s, 4.1x, with user time from 25 to 71 s because every per-job table restart re-pays it.

The DUBT's lazy parse re-creates the falsified far-win-resume cost class. After a far win the scan resumes at the twin's divergence point where near twins still agree for megabytes, and every post-resume search pays near-block-cap `count_from` extensions. The opt parser dodges this by visiting every position regardless, and its instruction count went down under the same lift, from 133.8G to 132.7G. Best's far class needs a search-side count cap or C's wholesale LDM lazy emission through `ZSTD_ldm_blockCompress` before it can land, and the bare lift should not be retried.

### NT-store clear at frame-start heads

An NT-store clear through `clear_table` at the btlazy2 frame-start heads site, the `dubt_stale` clear and a same-day variant of the 2026-09-21 ring-clear diet, was measured over 7 interleaved rounds of text.best bulk. The base read a 769 median against 736 for NT, -4.3%, while the plain-fill variant of the same change measured +3% instead. The MT job-start site's documented NT win does not transfer to a frame start. What follows the job-start clear is a scan with its own warm working set that NT protects, while what follows the frame-start clear is the tree fill and first searches whose working set is the heads table, so plain fill leaves the zeroed lines L3-resident to serve that pass while NT streams them to DRAM. Clear placement is a cache-locality decision and not a size threshold.

### Fused count and order

A fused count and order in the DUBT descent through `count_order` read one u64 pair per tree compare, took the common-prefix length from `tzcnt`, and extracted the ordering byte from the registers instead of two byte loads. The variant was byte-exact and dump-gated, but callgrind on json L13 moved Ir from 8.83G to 9.03G, +2.4%, with solo wall json +1% and skewed -1%, which is noise. The two byte loads were L1-resident and off the dependency chain, issuing in parallel with the next tree step, while the register extraction sits on the xor to tzcnt chain. The first-differing-bit shortcut is also not correct, because byte order is MSB-first within the byte and `tzcnt` finds the LSB, and the wrong-logic version changed bytes and the dump gate caught it. C does it with a 4, 2 and 1-byte compare ladder at similar Ir, and register-side reuse of loaded words pays only when the byte loads miss, which L1-hot window bytes do not.

### Inlining find_best and insert_dubt1

`#[inline(always)]` on `find_best` and `insert_dubt1`, folding the per-search and per-sorted-node call overhead into the block loop, moved callgrind json L13 Ir from 9.03G to 9.34G, +3.4%. The call sites' frames merge into one mega-frame, so the loop's about 20 live values, covering finder fields, window bounds and pricing state, contend for one register set and spill harder than the outlined calls did. The call and ret at 6.8M to 19.4M calls per pass was never the cost, and the spill traffic is. `#[inline]` stays outlined, which LLVM's cost model gets right, and the noalias-argument trick in `fill_range` is the working variant of the same idea.

### btlazy2 direct port

The first direct DUBT port of btlazy2 on 2026-09-13 grafted an unsorted-mark unwind onto the opt tree's threading. json roundtrip corrupted at about 4.2 MiB at the first ring wrap, because tree-threading writes passed through still-marked chain nodes and a later sort's inherited common-prefix claims went false, with a match trusting a 15-byte prefix that was never compared. The hole was never pinned after about 6 instrumentation rounds. It was resolved on 2026-09-14 by a fresh port in `encoding/dubt.rs`, because the failure was the grafting and not the scheme. A standalone finder keeping C's exact slot discipline, with the chain in `[0]`, the mark and larger child in `[1]`, oldest-first batch sort and a nullified boundary, makes a descent never cross an unsorted node hold by construction, and it landed clean as recorded in [matchers](../perf/matchers.md). Port the architecture whole, and never patch one tree's threading toward another's invariants.

### Ultra tree ring C24

The Ultra tree ring C24, libzstd L19's cLog-beyond-wLog slack, produced dll100 output byte-identical to C23. Ring slots beyond the window floor can never resolve as candidates, because the walk's `resolve` and floor checks reject them before any ring read matters. libzstd's C24 is dead weight, and C23 carries the full-linking invariant at half the footprint, so a ring above the window is pointless without a window-floor change.

### Opt tier at W22 and C22

The Opt tier at W22 and C22, for libzstd L16 exactness and ladder differentiation, read dll Opt 21.43 MB against Best's 21.57 MB, collapsing the tier gap to 0.7%. The Opt tier's dll value is the W23 reach at 20.91 MB, 3.0% under Best, and a cheaper json speed of -22% is not worth deleting the tier's reason to exist.

### Ultra ring C22

The Ultra ring C22, applying the Opt-side C22 relief to rows 18 and 19, was measured on 2026-09-12 and read json -18.6%, from 13.35 to 10.88 s, for +0.22% dll, from 20.03 to 20.07 MB, so it was not taken. This is a deliberate tier-policy rejection, not a falsification. Opt pays the trade because its product is the reach at speed and json.opt came out denser anyway, while Ultra is the density flagship and its 7.7% zstd-19 margin is the tier's entire identity. Revisit only if Ultra's speed deficit becomes the product question.

### DUBT early-out at targetLength

A DUBT search-descent early-out at the row's `targetLength` of 32, the `sufficient_len` value, transplanted from the opt walk, read the tiny-vocabulary ladder synthetic at 28110 B, worse than L9's 24142 and far worse than C's own btlazy2 at 19336, with json at 5472046 against 5364906 after the fix. The collection walk's break stops the tree threading with it, so on match-rich data the tree starves within a few KiB. The walk cap must be block-scale at `OPT_NUM`, and the 25 to 21 MiB/s the early-out bought was the starvation itself.

### DUBT walk and batch cap

A 4x DUBT walk and batch cap, sorting deeper backlogs per search, read json 5473898, slightly worse, and speed fell from 25 to 20 MiB/s. The far-candidate loss against the old Keep tree is not chain-cap starvation, because more sorting only re-threads and prunes more. The old tree's json edge was link retention, a different mechanism.

### Row 13 at S5

DUBT row 13 at S5, with a search descent of 32 and C's L14 knob, read json 5472469, flat against S4, at -24% speed. On this corpus the json candidates sit within the first about 16 tree steps, and deepening buys text +0.3% only. Keep C's S4, S5 and S6 ladder split across rows instead of paying S5 on row 13.

### DUBT as the balanced matcher

Using DUBT as the balanced tier's whole-row matcher, moving row 9 to `btlazy(...)` with S1-S5, ring C20-C22 and heads H20-H21 swept, moved text from -2.49% to +0.45% at S3/C22/H21, but json from +19.46% to +5.23% at 29 MiB/s against the chain's 82. The S1 floor read 54 MiB/s with a worse json ratio than S3, and any row placed at 10-12 would invert against its own chain rows, whose json is about 7.1 against the tree's about 6.3. The json balanced density is the chain's literal-aware lazy selection at +19.5% over zstd-9 and not the strategy class, and the best tier's own S4 tops out at 6.25 on json against the chain row's 7.11, so no tree knob buys it back. A DUBT-balanced row is a best-tier replica on speed or a json regression on ratio. The cold-start slice survives, with the landed head taking the tree only for the span where it measurably wins in [matchers](../perf/matchers.md), and the S1 result adds that a too-shallow head is actively harmful at -4.36%, worse than no head, because it parses worse and hands the chain a degraded rep history.

### DUBT frontier retention

Retaining the DUBT frontier on budget-exhaustion breaks, by nulling the trailing links only when the ordering claim broke on an equal tail and keeping them on a pure budget stop, read json 5469237, -0.05%, at 22 against 25 MiB/s. Keeping unvisited continuation links preserves little reach at this shape, because the pruned candidates are the worst-ordered ones, and it is not worth -12% speed. The full Keep and STALE class stays retired.

### DUBT data shapes

Rewriting the DUBT descent and sort loops into C's data shapes to cut per-node instructions was tried in two attempts. Attempt 1 used a full pointer form, with a `count_ptr(a,b,len,end)` pointer-walking count, u32 node values with wrapping `delta = curr32 - v` bounds replacing the 64-bit unpack, and candidate and end pointers hoisted into the loops. Attempt 2 used a minimal delta form with the same u32-delta bounds but kept the index-based count and window reads and added no pointers. Both were byte-identical under the full-ladder dump and both regressed, with attempt 1 moving json whole-encode Ir from 8.85G to 9.55G, +8%, and the finder symbol from 7.22G to 7.92G, and attempt 2 moving it to 9.04G, +2.2%.

The index form LLVM lowers today is already near a local optimum. The 64-bit unpack chain and stack operands are the register-allocation slack of loops whose live set of fused selection state and finder state exceeds the GPR budget, so every arithmetic reshuffle re-rolls regalloc, and both rolls lost. Per-node instruction reduction needs fewer live values or fewer events, not different shapes.

### Range-pair folding

Folding the DUBT walk and descent range pairs of `floor < cand < pos` into one unsigned compare through a precomputed `cand−lo < span`, and dropping the redundant null test from the loop heads, raised `find_all_matches` Ir +2.5%, from 11.06G to 11.34G, with wall flat. The two plain compares codegen cheaper than the wrapping-sub chain, and loop-head branch count is not the cost on this walk. The null test itself was free to drop, and the u32 rebuild of the null link always fails the range checks, so the landed port keeps it.

### dll sequence-count lever

The premise that a same-density parse with half the sequences would halve emit and entropy cost was falsified on the regenerated dll100 on 2026-09-16. `seqstats --ref-frame` read nseq ours against libzstd at 4.87M against 5.00M for l1, 5.51M against 6.01M for l3, and 3.39M against 4.76M for l9, where ours is 29% fewer at an average ml of 26.4. The average ml of 10.9 shape was the pre-2026-09-15 corpus. No sequence-density lever exists on this shape, and emit and entropy are per-byte at parity or ahead, with fastest at about 27% of cycles against C's about 34% and fast at 3.03G against 3.45G Ir. The differential is matcher-side instruction volume.

### Opt fill-lag clamp

Running the opt fill-lag clamp in C's unconditional form, from `ZSTD_buildSeqStore`'s limited update after a very long match at every block on rows 16-19, moved dll100 opt st from 18,780,148 to 18,901,786, -0.65%, and ultra from 17,932,355 to 18,045,646, -0.63%, with dll32 at -0.17/-0.14% and text and json corpus byte-identical. Where a far class is in play, as in LDM-armed dll and sub-32 MiB dll16 at -0.28% where LDM never arms, the lagged region's in-domain candidates measurably pay. The landed clamp runs only on the alphabet-gated population in [matchers](../perf/matchers.md).

### Clamp on job strips

Extending the clamp to job and dictionary strip parses on streaming-MT text jobs moved text.opt stream-mt from 82,124 to 82,140, +16 B, and text.ultra stream-mt from 81,314 to 81,329, +15 B. A job-restart parse, with tables, stats and price state reset per job, measurably selects differently with the lagged candidates present, unlike the frame-continuous parse where the clamp was output-neutral on every shape. Jobs stay unclamped through `strip_parse`.

### Row 16 at mls 5

Opt row 16 at mls 5, following C L16's searchLength 5, where ours runs mls equal to `min_match` at 4, grew l16 bulk-ST sizes by json +0.81%, text +0.91% and dll32 +1.88%, with json from 4,692,114 to 4,729,891, text from 81,852 to 82,595 and dll32 from 3,887,241 to 3,960,398. mls 5 even drops below libzstd's own L16 on json, against a 5-10% wall win with json l16 at x1.327 to 1.190. The row's identity is ratio, because the 4-byte hash feeds the DP shorter candidates at more positions and the price model prefers them. The wall trade is real, but the row is not for sale. This was measured on 2026-10-24 through `matrix --mode enc-st --full-ladder --level 16`, since the ratio and dump grids are tier-number-only and blind to off-tier rows.

### Row 22 at search_log 9

Opt row 22 at search_log 9, following C L22's S9 where ours runs 8, read l22 sizes json -4 B, text +0 B and dll32 +33 B, a net +29 B worse, while json l22 wall ran about 38% slower at x0.555 to 0.766 and text +5.7%. Our geometry saturates below 2^8 compares, with the row's ring capped at C24/H22 and the about 144 MiB budget, where C's S9 feeds its C27/H25. One more search bit re-walks the same candidates for the same bytes. This was measured on 2026-10-24.

### DUBT descent prefetch

`prefetch_children` issued an early read of the child pair's ring line and the candidate's window line per descent step, before the ordering branch resolved, with a const-generic `PF` instantiation and a `ZSTDX_DUBT_PF` env override for single-binary A/B, on both `find_best`'s sorted walk and `insert_dubt1`'s batch sort. The result was byte-identical under the dump gate and a consistent loss: skewed l13 from 26.3 to 27.1 s per iteration, +3% over 3 rounds, json +0.9% and dll100 +4.7%. Perf stat on skewed read instructions +26%, from 29.6G to 37.2G per 2 iterations, while cycles moved only +3.8%, from 46.6G to 48.4G, so the 7.6G added instructions are nearly free and the machine has real issue slack on this walk.

The walk's memory-level parallelism is already harvested by out-of-order speculation. The next step's pair load depends only on the current candidate value and not on the compare outcome, so speculative issue loads it in parallel with the count bytes, and one memory latency per step is the hardware floor. Every address a software prefetch could compute early is one speculation already issues, so latency slack on a dependent walk does not imply prefetchability. The harvestable slack was ROB-order decode blocking of the entry load behind the rep probes' count loops, which the head-load hoist converts instead in [matchers](../perf/matchers.md).

## LDM integration

### Dense dfast strip fill at MT job heads

A dense or back-filled dfast strip fill at MT job heads, in the FillGrid dictionary form or at full stride-1 density, was floated for the json.fast MT-versus-ST differential and is analytically dead with no build run. A window-length strip's stride-3 grid already saturates both tables, with l3 at about 700K inserts into 2^17 long slots leaving about 0.4% empty, and the small table's exponent is twice that. Per slot the newest-wins holder under stride 3 differs from the dense holder by at most 2 bytes of position, so candidate quality is identical while the fill triples.

The json.fast MT differential is not a strip-fill artifact. Our MT bytes are monotone in boundary count, with json l3 at 32 MiB reading ST 6,336,067, mt2 6,335,591, mt4 6,333,597 and mt8 6,331,112, because job-local entropy restarts and job-scaled block splitting win over the boundary costs. The -0.17 to -0.19% versus ST residue decomposes into our ST beating libzstd's ST at +0.05% while libzstd's MT recovers 0.23% over its own ST through per-job entropy restarts. Our ST already re-adapts per block, so the win cannot land twice, and the gap against libzstd-mt is 6.2 KB at mt8 and 8.7 KB at mt4, global and not boundary-localized, measured on 2026-09-18.

### Depth-0 price competition

Depth-0 price competition as the btlazy2 LDM integration injected each far candidate into the lazy selection like the chain row and added a miss-step clamp so the scan lands exactly on splits. dll32 best ST read 4,269,832, or 4,236,108 in the gap-parse form, at 16.0 s per iteration against chain-only 7.6 s, 2.1x slower. Instructions moved from 245.7G to 544.6G while `find_best` calls dropped from 4.93M to 3.97M, per-call from 50K to 137K, and perf put about 98% of cycles in `count_from`'s 8-byte equal-run scanner, with whole-encode scanned bytes moving from 241G to 540G.

The tree's parse after a far win resumes at the far twin's divergence point, where near twins still agree for megabytes, so every post-resume search pays near-block-cap extensions against them. The stock parse only searches at its own tree-divergence points, where the first scan is short by construction, and the clamp compounds the cost by capping miss steps to split granularity. C's lazy-family LDM emits candidates wholesale through `ZSTD_ldm_blockCompress` and parses only the literal gaps, precisely so the tree never touches the duplicated mass. Price competition stays in the opt parser, which visits every position anyway.

### C-parity gap-parse seam

A C-parity gap-parse seam on the chain row, emitting exactly as stored with no backward extension at the split, C's `curr > nextToUpdate + 1024` catch-up bar and no offset-2 chain after the emission, was compared with the plain seam at dll100 st 19,853,210 B. Backward extension into the pending gap literals restored +26.5 KB, because the gear split lands mid-match for misaligned twins. Dropping the 1024 bar restored +44 KB, because interior catch-up of at most 512 B is never behind the split and the mid-size population's 64-300 B twins need their tails indexed for later in-reach chain matches. The offset-2 chain restored +39 KB. dll32 tracks the first two at -1.8 KB and -853 B and is flat on the third. The btlazy seam's C parity is not the chain row's optimum, because the row's own emissions backward-extend and ride rep1 and its chain, unlike the tree, can use interior-tail entries at in-reach distances. Wholesale emission changes what the seam owes, and each deviation pays for itself in bytes.

### Miss-step clamp on the gap-parse scan

The injection model's miss-step clamp on the gap-parse scan never stepped `pos` over the next split, keeping the scan from overshooting a candidate. It was built and A/B'd, and the dll32 and dll100 ST frames were byte-identical to the unclamped build. In the gap-parse model the split is never probed, and the overshoot only skips literals of the candidate's own literal run, which the wholesale emission takes regardless. The clamp existed to land the scan on the split for injection, a consumer that no longer reads it.

### Indexing the LDM match interior

Indexing the LDM match interior into the lazy tables, with no `ZSTD_ldm_limitTableUpdate` cap at the gap-parse seam, moved dll32 best ST whole-encode scanned bytes from 6.7G to 1,592G at 26.5 s per iteration, worse than the falsified depth-0 form, and C's cap restoring it to 6.7G and 0.61 s with tables catching up at most 512 bytes. Freshly filled interior entries sit exactly at the near-twin period behind the resume point, so the first bucket candidates scan to the segment cap. The interior is already far-matchable history through the LDM table itself, and the lazy ring gains nothing but the worst candidates.

### Always-on balanced LDM

LDM as an always-on balanced candidate source probed every generated split at a minMatchLength of 64 and window W22. Corpus speed read json -15%, text -31% and skewed -43% in an interleaved manual A/B over 3 rounds, for zero ratio change, because json has no 64 B repeats and its far class is 32-63 B fragments, text's far tiles ride rep0 chains within reach, and skewed has none. The split pass costs about 9 cycles per byte measured, through the gear chain, a 64 B xxh64 per 64 B and two random-write arrays. A candidate source may not tax shapes that never win, so LDM ships size-gated at inputs of 64 MiB or more with a quiet latch and canary revival in [matchers](../perf/matchers.md).

### LDM inside the chain reach

Probing LDM candidates inside the chain's reach with no offset filter cost text 30% at parity ratio. Every split's far read plus the miss-step clamp stopping the scan at each candidate lost to chain and rep incumbents. LDM's exclusive class is offsets beyond the chain reach, because in-window candidates are duplication, and the modal repeat distance, dll's 1-2 MiB, is already served by the newest-wins heads.

### LDM minMatchLength 32

Lowering the LDM minMatchLength from 64 to 32 to carry json's 32-63 B far class grew json by 13 KB at equal reach, because the far fragments are net-negative and supplying more of them loses size. dll100 at small reach was also worse. json's far class fails its own offset price, and the 64 B window is the right bar for this tier's far candidates.

### Content latch for the split pass

Two content-latch variants for the split pass were tried, K=16 with no horizon and a canary every 8 MiB. dll100 moved from -11.4% to -3.5%, because the first far class is only reachable at least 4 MiB in, and sparse canaries miss the 4-8 MiB class entirely on dll32, with the ratio back to baseline. A latch that fires before the class can exist, or a canary too sparse to find it, deletes the feature. The horizon that counts quiet only past the reach plus 1 MiB canaries keeps dll100 at -9.75%.

### Pre-horizon fill-only generation

Pre-horizon fill-only generation for the mid-size gate extension indexed splits without matching below the reach horizon, on the premise that no candidate can survive the filter there so fill must be byte-exact. dll100-ST moved -41 B, because generate is not fill plus emission. A raw in-span candidate advances the anchor and triggers the overlapping-pattern re-arm, so the gear hash samples different positions downstream and table content diverges even when every sequence dies in the filter. The anchor and re-arm path is part of the split-sampling semantics, not the emission side, so any change touches the W26 population's bytes, which are frozen. The pre-horizon tax stays and is bounded by the alphabet gate instead.

### Win-keyed canary revival

Win-keyed canary revival for the mid-size population revived only when a canary candidate actually out-lengths the incumbent instead of on candidate existence. dll32 moved from -19.7% to -13.5%, with ramping far regions reviving late, while skewed stayed at about -35%. Low-alphabet candidates transiently win the injection with `ml > best_len` and are displaced by the lazy walk without ever emitting, so quiet resets either way. `ldm_won` is an injection-time flag and not an emission flag, and revival keys cannot separate a candidate that exists from one that pays on low alphabets. The alphabet gate is the discriminator that works, and both directions were measured the same day.

### LDM mid-size arming at W24

LDM mid-size arming at W24 with 16 MiB sources, using dll16 as the first 16 MiB of dll32, moved dll16 from 2,930,674 to 2,914,632, -0.55%, at -6% speed. The win concentrates where the source spans its far class, because dll16's 4-16 MiB twins are too sparse inside a 16 MiB window and the pairs that pay sit at 16-32 MiB distances present only in dll32. The bar stays at 32 MiB.

### Chain reach shrink on the json premise

Shrinking chain reach on the json premise that LDM carries the tiles produced json sizes byte-identical with LDM off at every reach, from 4720677 to 4661039, and text identical at all reaches because its tiles ride rep0. The json shrink curve is a pure chain-reach effect, since the far chain candidates were net-negative, and LDM coverage is not involved, so the LDM-carried-tiles premise is falsified. The shrink itself is a real chain-only lever, at json -1.07% size and +60% solo speed at W18, blocked only by dll needing reach W22 at +4% at W20.

### Near-twin displacement latch

A near-twin displacement latch for the chain's far candidates instrumented `chain_search` so that when the best was far, above 4 KiB, it checked whether a near best below 4 KiB sat within +2 B, then latched a shrunk reach per block when far-best searches were near-twin covered. After fixing the instrumentation so the near best is captured before the first far candidate overwrites it, json reported 99.4% of far-best searches near-twin covered, yet the shrunk parse won net on both axes. The classifier is decoupled from the benefit direction, because json's far-candidate harm is parse-level, through displaced repcode history and literal-flow differences over the whole block, and not per-search length myopia. Per-search accounting cannot price it, and only a real second parse measures the trade, which landed as the prefix dual-parse probe in [matchers](../perf/matchers.md).

### Far-search frequency latch

A far-search frequency latch for the reach decision, following the LDM quiet-latch pattern, counted far-best searches per block and shrank when the count said the far class was absent. json had about 119K far-best searches per block against dll's 50-150K, so the distributions overlap end to end. Both classes search far densely because the chain probes whatever its reach admits. Whether the far class is present is about what the far candidates cost the parse and not how often they win, so frequency carries no signal, the same conclusion as the near-twin latch.

### LDM split fingerprint

Deriving the LDM split fingerprint from the gear fold at the trigger, through `mix64(fold)` or a patched `mix64(fold ^ oldest_lane*K)`, moved dll100 no-latch size from 20,806,075 with xxh64 to 21,126,156, +1.54%. The latched number stayed at -0.013% and the table simulation showed parity, with 12 against 25 false checksums per 1.6M splits and 1017 against 1037 twin misses of 554K. The unpatched fold also collides outright on oldest-byte differences whose table entries share parity, because the difference survives only in bit 63, which the trigger mask forces to zero, covering about 50% of byte pairs. A fingerprint correlated with the trigger condition shifts with the split placement, so the candidate stream differs systematically in exactly the low-yield regions the latch would kill, and the simulation models the table and not the anchor and skip cascade through the parse, so it cannot see it. The landed 8-lane window fold is trigger-independent and tracks xxh64's unlatched size to -0.04%.

### Cheaper split fingerprint alone

Cheapening the split pass by compute removal alone, swapping xxh64 for any cheaper fingerprint while keeping everything else, cut dll100 instructions 4% but left wall flat across interleaved rounds. The pass is bound by random 8 MiB-table traffic in the bucket scan and insert, not by fingerprint arithmetic, and the out-of-order engine hides the serial-multiply chain under those misses. Instruction deltas are necessary but not sufficient for armed-LDM wall gains, and the wall came only after the gear chain's per-byte serial add was broken by 4-byte re-association and the scan's per-entry op count was cut through the AVX-512 checksum mask.

## Emit, entropy and job boundaries

### Emit skipping inserted positions

Skipping positions already inserted by scan pairs, as idempotent insertion, changed the output and raised instructions +2.6% after the fix. Insertion is not unconditionally idempotent, because hash-collision overwrites during backward extension matter and redundant reinsertion restores slot values, and the cost also exceeds the benefit.

### Naive Predefined and Repeat thresholds

Naive Predefined and Repeat thresholds in `choose_table` had negligible effect. The cost-comparison approach through the selectEncodingType port plus repeat mode later handled it, and naive thresholds should not be used.

### Store gate on average literal entropy

Pricing the store gate by average residual literal entropy, feeding back bits per byte per block with the gate `ml*cost ≥ ilog2+7`, read text.balanced 346, -6.5% against zstd-6, and json +16%. The average anti-predicts, because text's residual stream is hyper-skewed at about 0.2 bits/B, since the matcher hoovers up everything matchable, yet text wants the loose gate. The displaced literals' marginal cost, their code length under the block's Huffman table, is the right price and is the implemented form.

### Sampled entropy decision

Reusing the probe pass's about 2016 sampled bytes as a count histogram with a Miller-Madow estimate for the table-strategy gate, and dropping the exact pass, was falsified on paper before building. The MM correction fixes the estimator's mean, about 8.000 for uniform, but not its spread. The standard deviation of a 2016-sample plug-in entropy over 256 bins is about 0.11 bits, so the 7.97 bar sits at about 0.3 sigma, a per-block coin flip. That is the exact failure mode the opt gate's exact histogram was built to kill at its measured 3x. Sigma scales as 1/sqrt(N), so a clean 6-sigma margin needs about 1M samples, which is the whole block. The exact pass is mandatory at every level, and its cost can be cut only through better binning, never through sampling.

### Gate-extension placement

A gate-extension placement hunt used 4 builds: a watermark store in the 3 scan-loop epilogues, gap-side-only bookkeeping with catch-up before and after the scan loops, and a struct-tail field. Every build's instructions rose +0.1 to +0.5% on text cells, yet cycles rose +3 to +8% on exactly one fast-tier cell, and which cell moved with the build, with fastest on the gap variants and dfast plus chain on the epilogue-store variant. Branch misses and icache misses measured equal, so the loss is pure scheduling and decode placement. This confirms the layout-lottery lesson at instruction parity: even zero-touch scan bodies re-roll the dice, and the perturbation enters through the per-block dispatch path and outlined-function placement. No placement dominated across 4 tries, and the variant shipped with one bad cell at text.fastest -7% wall, still 12% ahead of libzstd-1. Do not chase per-cell wall with source-order or field-order tweaks, because each is a fresh roll and not a fix.

### Ultra job-boundary seeding with the strip reachable

Ultra job-boundary statistics seeding with the strip reachable ran a throwaway parse of `[job.start-K, job.start)` against the prefilled strip to warm the job's `opt_state`, at K of 128 KiB and 1 MiB. json.ultra.bulk-mt moved from -1.85% to -1.92% at K=128 KiB and was worse at 1 MiB, recovering none of the boundary loss. The boundary cost is a price-feedback basin and not a calibration gap: a BASE-priced parse with the 8 MiB strip reachable picks near-offset and ll0 chain shapes and `updateStats` reinforces them, so stats sampled from more reachable-strip parsing deepen the wrong basin. The seeding that works re-bases the window to the span itself, an empty-window frame start that landed on 2026-09-12, because sampling the right distance classes is worthless if the seed parse itself runs in the wrong shape.

### SeqStore cursor emission

Emitting outputs as raw seqStore cursors, with a block-start reserve in `start_matching_codes`, lit and seq cursor fields replacing the `literals` and `seqs` Vec fields in TableEmit and DfastEmit, and `push_seq_packed` writing through cursors with lengths committed through `set_len` at the tail, was byte-identical under the full-ladder dump. gungraun read fastest Ir +0.2 to +0.6%, with random +1.5% because the extra work is in the miss path, and fast Ir exactly unchanged, while wall read json.fastest 540 to 532-538 and json.fast 391 to 374-375, -4% and reproduced twice.

The premise that Vec fields pin 6 registers is false. LLVM already rematerializes the len and cap at the cold emit sites, so only the two pointers were live, and the cursors codegen to the same two pointers, with fast's identical Ir as the proof, leaving no spill traffic to reclaim. The block reserve plus the reshuffled allocation added miss-path Ir and rolled the layout die, giving fast -4% at unchanged Ir. libzstd's seqStore discipline is enforced by an outlined `ZSTD_storeSeq` behind one pointer, and with `inline(always)` emit LLVM promotes any cursor back into registers, so memory residency is unreachable on this route. The outlined-emit variant would pay per-match call overhead on json's about one emit per 16-30 B density before any speculative win.

### Strip retention cap without seed confirmation

A Fast strip-prefill retention cap without seed confirmation grew text.fastest bulk-mt8 from 108607 to 163586 B, with ratio 309 to 205. One job's strip tail carried a 194-byte duplicated statement at offset 2244, the nearest-first seed picked it over the 441226 tile period and burned its budget, and with the head entries capped away the job had no far candidates at all, while other jobs stayed at parity because the seed carried them. The retention and churn model of about 8 head inserts buried per slot holds only for aperiodic content. A period-close-to-window corpus's period-old twins hash their slot once per period and survive newest-wins, so the strip's far-match payload lives in both the table's lucky head entries and the seed. Cap one only when the other is confirmed to carry the class, and both landed together in `3ba7335` and `27e8d52`.

### Shared derived verdicts for self-fill

Shared derived verdicts for the fast-row self-fill's span passes, computing `strip_is_uniform` and the gate probe table once per region and folding per job, were tried as the dll100 wall lever. The uniform scan is about 0 on real data, with jobdecomp `uniform_ns` at 0.02 ms over 40 jobs because AVX-512 early-exits on the first mismatching 64-byte block, and the gate replay's probe table is a last-writer-wins fold over a span-relative segment grid whose job offsets differ, so no byte-exact cross-job derivation exists. The earlier bandwidth-bound framing was wrong twice over: the wall was the gear-scan loop itself at 43.5% of job time, with `gear_feed` at 32.6% of process samples and spilled-checkpoint asm at about 7 uops per byte, and the only shareable quantities, the position-pure split values, are shareable precisely because the gear state forgets everything older than 64 bytes. The landed shape parallelizes the scan per job instead of sharing across jobs, in [matchers](../perf/matchers.md).

## Dictionary-frame rows

### Chain-hash width on dictionary frames

Applying the 4-byte chain-hash width alone to dictionary frames, with an accept bar of 5, the offset-pays gate on and walk depth doubled, grew the fixture -5 from 8,708 to 8,736, +28 B, and -9 from 8,442 to 8,451, +9 B. The 4-byte classes bury the 5-byte twins the accept bar would take, and the twins the CLI converts at ml=4 with 2^11-2^13 dict offsets sit behind the pays gate. Every lazy-family knob is contingent on the others, with the joint result at -190/-183 B recorded in [encoding](../perf/encoding.md).

### Chain walk depth x16

A chain-walk depth of 16 on dictionary frames, beyond the landed 8, moved -5 from 8,518 to 8,499, -19 B, and -9 stayed byte-identical at 8,259. -9 saturates at 8 and is already past the CLI, while the remaining tail is -5 only, below 0.3%, and doubles the walk attempts. It is recorded as bounded headroom and not landed.

### Oversized dfast match tables

Oversized dfast match tables on dictionary frames at 4 logs above the row's sizing, tried on 2026-09-18 as an anti-shadowing substitute for C's frozen-CDict and fresh-CCtx dual-table probe, recovered -19/-23 B summed on fixture -3/-4 raw. Size alone recovers a sliver, because within one block the payload's own inserts still overwrite the dict twins' slots, which is the burial the dual table exists to prevent. The split-table probe is not worth porting for under 25 B, and the width-4 short hash plus dense small fill in [encoding](../perf/encoding.md) were the real levers.

### Frozen-dict-table dual probing

Frozen-dict-table dual probing ported libzstd's dictMatchState attach, snapshotting the post-fill tables and falling back to them when a payload insert shadows a dict twin, and it also tried CDict-geometry oversized dfast dict tables at 2^17/2^17 against the srcLog-clamped 2^16/2^15 and dropping the dfast dict fill's `acquire_seed`. The three changes recovered -4 B, -2 B and 0 B summed on fixture -4, about 6 B combined. Candidate coverage was already complete after width 4, with `seqstats` showing `missed_ref_matches=0`, and the residue was cParams table selection, where the payload rows of at most 16 KiB swap strategy class, landed in [encoding](../perf/encoding.md). The earlier oversize falsification's stale 5-byte-hash premise did not matter, and the width-4 retry loses the same way.

### Opt rows 11 and 12 on the dictionary swap

Using the at-most-16 KiB clevels opt rows verbatim at 11 and 12, with btopt at S3/S4, T12/T24, mls 4/3 and integer prices, on the dictionary swap grew fixture -11/-12 raw by +7.3/+6.9, worse than the unswapped Row matcher at +4.6. The table's row knobs are speed and tuning choices for libzstd's parser and not ours: integer prices lose 2.4 points, with ultra at the same S and T at +5.1/+4.4, and the T12/T24 early stop loses another 1.5-2 points, with T999 at +2.9. The swap lands with ultra, T999, mls 3 and an S floor of 4 in [encoding](../perf/encoding.md).

### Insert budget on dictionary tree fill

Raising the insert budget on the dictionary tree fill to insert_log 8 or 12 against the row's searchLog, and raising searchLog past 4 to 5, 6 or 8 on the swapped opt rows, produced byte-identical output at every probed value. The 16 KiB dict's tree exhausts its candidates at 1<<4 compares, so the fill budget and search depth are not binding on this payload class. The knobs stay at the table's rows from 13 up.

### Mid targetLengths on swapped opt rows

Mid targetLengths of 64, 128, 256 and 512 on the swapped opt rows read -12 raw +2.9 for T999 against +2.9 to +3.5 for T256 to T64, monotone toward 999. The DP's early-stop jump never pays on dictionary frames, because long dict matches make the accepted-jump path common but suboptimal, and every row at T 12-512 loses to the full DP.

### btopt-DP density behind the -11/-12 residue

A btopt-DP density behavior behind the -11/-12 raw residue, where the CLI's own -11 was denser than every row we could express, does not reproduce. On two fresh seed-42 fixtures at 70/54 and 76/48 with the same recipe, our swapped rows were ahead of the CLI's -11 by 73-144 B, and ahead of its -16/-19 by 143-249 B, while the CLI's internal btopt-beats-btultra2 ordering itself reproduced at 8059 against 8164, measured on 2026-09-22. The earlier number rode the own-dict pairing, because dict content quality swings about 440 B across train splits, dwarfing the 45-200 B parse-gap class. Do not re-scan the opt DP for it.

### hash_log 16 on small-dict chain rows

hash_log 16 on the small-dict chain rows, against the landed H15, was byte-identical to H15 on both fixtures. 32 KiB of 4-byte classes over 64 KiB buckets is already collision-free at H15, and only the H14 dilution was binding.

### Depth and lazy settings on small-dict chain rows

Depth 128 and 256, plus 512 for row 8, with lazy 3 on the small-dict chain rows under the landed fills, was byte-identical at every probed value, re-confirmed after the ring, hash and fill fixes. The candidate set is the binding constraint once fills are libzstd-shaped, not the walk budget, and the rows keep 64/64/256 with lazy 2.

### Two-state candidate-set split

Porting the CLI's two-state candidate-set split was meant to close the -7/-8 residue of +9 to +16 B, with CDict-attach semantics where the dict's tables live in their own match state, the local chain is walked first, the dict walk uses the remaining attempt budget and a strictly longer match replaces. It does not reproduce under the trainer. On the fresh 3-seed fixture, -7/-8 were ahead at every pairing, with cli-raw 0 to -13 B and ours-fmt -26 to -64 B, and the only behind cells across 8 seeds were ours-raw seed-33 at +7 to +10 B, inside the pairing's plus or minus 20 B split variance. Per-file decomposition shows 7 of 10 losing files with zero missed ref matches, and the 3 events are same-position alternate picks, either a longer match we emit or a farther 22 B match that libzstd's own depth-2 gain compare would reject. The two-state structure changes tie and priority order only on these rows, and there are no absent candidates to port. The -6 against 7/8 pairing decay is the CLI's row ladder self-improving about 22 B from lazy mml3 to lazy2 mml3, against our tuned rows 6-8 that emit identical bytes, so it is row-power alignment and not a candidate-set gap, measured on 2026-09-23 and recorded in [encoding](../perf/encoding.md).

### Skipping the emit's covered fill

Skipping the emit's covered fill below the scan's insert frontier with `from_abs = max(start, filled_from)`, the frontier-skip form of the greedy self-reference fix, landed as `23dd5bce` and was reverted the same round. On a 3-seed fixture at 86/54 with the same dict on both sides and the `a1c7a7cc` reference build, every dict row regressed. Chain rows lost -4 to -8 by +14 to +81 B per seed and level across all four dict pairings, including the cli-raw and cli-fmt cells the branch's own 76/48 fixture had measured as improved, and opt rows lost -11 to -16 by +144 to +452 B.

The skip removes more than the self-link. The scan's rep-probe advance of `pos = probe` leaves positions never inserted, and the re-inserting fill was the only thing that ever covered them. Opt walks probe far past the emission start, so the skip also drops the interior fills wholesale. The corruption diagnosis stands, since a Python replay proved the self-referencing slot, and the closable shape is insert-every-position-exactly-once: fill the rep-advance holes without re-inserting heads, following libzstd's `[nextToUpdate, ip)` catch-up discipline, and not frontier skipping.

### Fast-row LDM at the mid-size bar

Arming the fast rows' LDM at the mid-size bar of 32 MiB, the chain row's `LdmArming::Frame` class, so dll32-bulk joins the far class is impossible to keep byte-identical. The corpus, where json, text and skewed die at the alphabet gate and random never emits a candidate but passes it at 256 symbols, and dll32 are indistinguishable at the only decision point that matters. The frame header is serialized before any block parses, and the window must widen because offsets above 2 MiB are illegal under the stock W20/W21 header, so random's header would move with dll32's. Header-time decisions can only key on declared geometry and not content, so the falsification stands for every decision made after the header serializes. The workable shape is to decide before it, and that screen landed on 2026-09-24 as the pre-header far-class screen over the head sample in [matchers](../perf/matchers.md), where the earlier bar was the full 64 MiB window, which separates by size alone.
