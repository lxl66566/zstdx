# Encode dll100 Gap Against libzstd

This page answers where the low-tier dll100 encode gap against libzstd comes from, which part of it is instruction volume, and which part is a matcher-architecture difference. It measures the real opponent core against core on one whole-file pass, measured on 2026-09-20 on the bench machine. The x notation is our time divided by the reference time, so values above 1 mean we are slower.

## The gap

The measurement is whole-file callgrind instruction counts over the 100 MB payload in one pass, plus per-symbol breakdowns of both encoders. The reference is zstd crate 0.13.2, which links libzstd 1.5.7, through a twin harness calling `zstd::bulk::compress`, the exact matrix `enc-st` cell body. Our side is `zstdx-bench prof enc <lvl> 1` with a bulk slice and checksums off, and literal volumes come from `seqstats --ref-frame` against the zstd CLI frames.

| tier | ours instructions | zstd instructions | ours/zstd | ours size | zstd size | sequences ours/ref | literals ours/ref |
|---|---:|---:|---:|---:|---:|---:|---:|
| l1 fastest | 3,792,480,451 | 2,969,259,712 | 1.277 | 48,802,511 | 47,922,691 | 4.85 M / 5.00 M | 46.5 MB / 45.4 MB |
| l3 fast | 3,238,547,300 | 2,959,509,856 | 1.094 | 30,183,932 | 30,874,173 | 5.51 M / 6.01 M | 19.8 MB / 21.8 MB |
| l9 balanced | 10,903,348,910 | 11,269,595,186 | 0.967 | 20,782,718 | 22,701,602 | 3.39 M / 4.76 M | none |

Balanced is not an instruction gap at all. We execute 3.3% fewer instructions than libzstd at level 9 while running at x1.49 the wall, and the chain walk's latency-bound regime, a link chase at about 66 cycles per step described on the [matchers page](../perf/matchers.md), is the whole story on this tier. Libzstd's level 9 runs `ZSTD_RowFindBestMatch`, which holds 16-entry contiguous rows at one cache line per bucket, against our position-indexed chain with 8 MB of H21 heads and 4 MB of C20 links of random accesses. Closing this gap is a matcher-architecture question about the row matcher, not an attrition question. Fastest and fast are instruction-volume gaps at 1.28x and 1.09x, and the per-symbol split shows where.

## Where the gap comes from

Per-function instructions for one dll100 pass at the three tiers:

At level 1, our totals are scan `start_matching_fast` at 2,118.4M (55.9%), literals Huffman stream `write_packed_codes_dual_impl` at 516.2M (13.6%), `encode_sequences` at 432.0M (11.4%), `insert_covered` at 425.0M (11.2%), `histogram_literals` at 128.8M, `choose_tables_fast` at 52.8M and libm `__log2_fma` at 17.2M. Zstd's totals are `ZSTD_compressBlock_fast` at 1,704.8M (57.4%, with storeSeq inlined), `ZSTD_encodeSequences_bmi2` at 529.8M, `HUF_compress1X_usingCTable` at 328.7M, `HIST_count_parallel_wksp` at 187.4M and `ZSTD_seqToCodes` at 95.2M. The 823M gap decomposes into the scan loop at +414M (50%), `insert_covered` at +425M (52%) and the literals Huffman stream at +187M (23%), offset by sequence encode at -193M where we are ahead, literals histogram at -59M, table builds at -15M and the entropy-bound `log2` calls at +17M that only we pay.

At level 3, our totals are `start_matching_dfast` at 2,327.4M (71.9%), `encode_sequences` at 490.3M and the Huffman stream at 217.3M. Zstd's totals are `ZSTD_compressBlock_doubleFast` at 1,789.1M, `encodeSequences` at 602.0M, `seqToCodes` at 108.0M, HUF at 145.8M and `ZSTD_splitBlock` with fingerprinting at 73.8M. The 279M gap is about the scan loop's +538M minus the emit, histogram and split wins of -259M, so the dfast scan excess is the whole level 3 gap at x1.30 on the scan symbol.

At level 9, our totals are `start_matching_codes` at 8.70G (79.8%), LDM `gear_feed` at 0.62G, the DUBT head `find_best` at 0.50G, `ldm_generate` at 0.23G and emit plus entropy at 0.49G. Zstd's totals are `RowFindBestMatch_noDict_5_4` at 8.54G, `lazy2_row` at 1.32G and `ZSTD_count` at 0.18G. Matcher-side instructions are a wash, with ours at 10.05G against their 10.04G counting the LDM machinery we carry for the -8.4% size.

## Evidence per component

The literals Huffman stream was the one emit-side gap and is closed, with the discipline described on the [encoding page](../perf/encoding.md). Per literal byte, with near-identical volumes on both sides, ours read 11.1 instructions per symbol against zstd's 7.2 at level 1 and 11.0 against 6.7 at level 3. The stream loop paid an nb extract of a `mov` plus `and $0xf`, a `shr %cl` because the BMI2 dispatch wrapper only tail-jumped the baseline implementation so `shrx` never materialized, and a separate count add per symbol. Libzstd's `HUF_CElt` discipline lands all three in one whole-element operation each: the shift count is the element itself, which x86 masks to 6 bits, the OR payload is the raw element, and the bit counter accumulates the whole element whose low byte is exactly the sum of nb because carries only propagate upward. Our table already had that layout, with nb in the low nibble, so adopting the discipline was arithmetic only. Dll100 instructions moved -4.0% at level 1, -2.0% at level 3 and -0.4% at level 9, output is byte-identical everywhere, gungraun measured encode json fastest -1.6% and text -1.0%, and wall moved dll100 level 1 +0.8% interleaved. The stream loop now sits at libzstd's per-symbol floor, and that part of the gap is gone.

The emit-side live-set shrink hypothesis does not explain the gap. Summing the serialization side at every tier, with ours covering `encode_sequences`, the Huffman stream, histograms and builds and theirs covering `encodeSequences`, `seqToCodes`, HUF, HIST and builds, gives ours 1,156M against theirs 1,140M at level 1 for parity, 823M against 969M at level 3 where we are ahead, and 538M against 695M at level 9 where we are ahead. The emit path was at parity or ahead before this round, with the Huffman stream loop as the single exception that is now closed. The remaining excess is matcher-side.

The fast and dfast scan loops carry +414M and +538M instructions. Their register-budget levers are measured out, with the const-log, OR-fold and restructure entries recorded in the [matcher negative results](../negative/matchers.md). `insert_covered` carries +425M at level 1 and is uop-volume-bound at about 9 to 14 instructions per insert. Every fill-thinning policy costs size on json and text, and the density-gated stride-2 form has already shipped. A dual-anchor-everywhere fill buys 0.46% dll size for 7.3% wall at level 1, which the size axis rejects, and a const-log instantiation of the outlined body would save about 1 instruction per insert, about 0.7% of level 1 and below the noise floor.

## The balanced tier after the storage swap

The balanced tier's latency mechanism is closed as of 2026-10-30. Row-based candidate storage, the lever this page named, landed as a per-frame verdict over the frame head's sampled alphabet: wide heads on frame-continuous frames take the tagged row twin of row 9 with LDM, narrow heads keep the chain, and the dll100 cell moved from 114 MiB/s at ratio 5.31 to 134 at 5.37 against libzstd's 149 at 4.62, x1.30 to x1.12. Text reads 1856 MiB/s at 384.66 against libzstd's 1726 at 378.41, ahead on both axes from the chain's 1560. The mechanism, geometry and gates are on the [matchers page](../perf/matchers.md) under the balanced storage swap. What remains of the dll cell is the W26-plus-LDM density lead's price, with an attempts diet for the dll class as the named follow-up.

The fastest and fast tiers are bounded by scan-loop instruction volume. The loop's register budget re-rolls on every added live value, so only register-negative changes convert, and the landed examples are the const-log instantiation and the fused table allocation. No removable live value remains identified in the loop.

`insert_covered` fill density is a speed-priority knob that stays rejected on the size axis. Its remaining const-log saving is below the noise floor, so no open lever is identified there either.

## The opt and ultra tier

Measured on 2026-10-30: solo perf stat on dll100 plus whole-program callgrind on dll32 at levels 17 and 19, our side against the zstd crate twin harness at the plain numeric levels. Ours carries the optLdm far classes over the W26 window, the reference runs plain L17 and L19 with no long-distance machinery, so the two sides do not parse the same stream: dll100 reads ratio 6.04 against 5.06 at l17 and 6.32 against 5.28 at l19.

| cell | ours Ir | zstd Ir | ratio | search ours/zstd | fill ours/zstd | DP ours/zstd |
|---|---:|---:|---:|---|---|---|
| dll32 l17 | 13.23G | 10.88G | 1.22 | 4.96G / 3.72G | 4.49G / 2.88G | 3.11G / 3.12G |
| dll32 l19 | 26.83G | 22.20G | 1.21 | 12.67G / 7.45G | 2.19G / 0.78G | 10.62G / 12.51G |

Per pass on dll100 ours runs 68.2G instructions at l17 against the reference's 52.1G and 144.1G against 108.4G at l19, at cycles x1.06 and x1.18 and IPC ahead on both cells. The DP driver is at parity at l17 and ahead at l19, the entropy side is ahead at both, and the excess sits in the tree search and the fill. The search's extra events are the density mechanism itself, the wider candidate set the far classes and the W26-to-window floor lift feed the DP, and the per-event remainder is the same distributed codegen differential the best tier's decomposition bounded. The compare lump inside both search and fill was measured directly and is small on this tier: the long-compare verdict port is falsified in the [matcher negative results](../negative/matchers.md), so the residual x1.06 to x1.18 wall is the density lead's price at instruction parity of the mechanism, and no safe lever is identified short of cutting the far classes themselves.

A 2026-11-03 profile-guided probe added the layout axis to the opt residue. At dll100 l17 bulk single-thread, the PGO build of one commit regressed 6.5 percent against its plain build at md5-identical output, 7.097 to 7.559 seconds per pass over six locked position-alternating rounds. Code layout is closed as a recovery lever on this tier, and the far-class event volume stands as the whole attribution. The probe, its skewed and json cells and the perf stat decomposition are on the [matchers page](../perf/matchers.md).

