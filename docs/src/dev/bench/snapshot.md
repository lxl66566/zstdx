# Performance Snapshot (2026-11-07)

This page summarizes the 2026-11-07 release pass at commit `3a05e92c`, run on one machine, one corpus and the same flags on both sides under the shared lock, driven by `bash bench/pass.sh all` through one release build at a summed section wall of 32.3 minutes. The comparison target is the zstd crate and libzstd 1.5.7 with zstdmt, and the ladder pairs fastest, fast, balanced, best, opt and ultra against libzstd levels 1, 3, 9, 13, 17 and 19. Raw tables, provenance and per-chunk commands are in [matrix.md](matrix.md), including the full numeric 1-22 ladder and the small-payload table. Throughout this page x is our time divided by the reference time, so values below 1 mean we are faster, and speeds are MiB/s of raw input.

The pass covers the sixty-three-commit window since the 2026-11-03 R94 pass, whose tables carried the decoder's sequence-loop program end to end. This window's encoder side is the R95 opt sprint and the stream machinery behind it: the fill-descent cut at one compare below the search depth moved json.opt walls 14 to 18 percent inside its stated +0.5 percent size bar, the far admission tier widened the feed jobs' match window and made json l16 denser by 98 KB, the stepped finish drain and the owned-source and NT-store feed entries landed on the read side, and the balanced dll class dieted to the light attempts budget for dll bulk walls at +0.36 percent bytes. The decoder side carried the multi-frame lane budget, whose plan-time staging split lifted skewed MT decode from flat to 1.72 times single-thread and put the multi-frame payloads at 2.8 to 3.1 times, and the R103 ramp-gate fix restored per-job gating on wholesale LDM emissions. The FFI round landed 29 further reference symbols, outside the timed paths. The one apparent regression, dll100 balanced stream-mt8 at 174 down to 128 MiB/s, resolved as a placement reading rather than a tree change: the same-day A/B against the pre-R94 tree reads 155 against 159 in the pass context at identical bytes and 1.74 percent fewer instructions, the cell's wall being its single slowest 12.5 MiB job of eight on the two-CCD host.

Caveats for reading every table: the pass runs the R86 harness, whose enc-st and enc-stream single-thread cells share the machine in pairs, so absolute MiB/s on those cells reads below the serial tables of pre-R86 passes and cross-pass comparisons use x; a verdict on a 10 GB/s-class cell needs a `--parallel 1` rerun per the [methodology](methodology.md). Noise runs ±10% between sessions, every cell carries interleaved medians at the default per-side budget of 500 ms, and spreads sit in the raw tables.

## Headline results

Decode single-thread reads x0.90 to 1.03 bulk and x0.93 to 1.05 stream on json, text and skewed, with json.zst3 at 0.97 bulk, text.zst3 at 0.95, skewed.zst9 at 0.90 and skewed.zst1 at 0.97. Random and zeros are ours at every tier, with random.zst3 stream at 0.80. Dll100 reads x1.02 to 1.07 on both paths, the one remaining decode band, attributed in [dec-gap](dec-gap.md).

Encode multi-thread has one remaining zstd win: json.fastest.mt8 at x1.14 inside its known noise band of 0.95 to 1.65, while its mt16 cell wins at 0.34. Every other cell is ahead, with text.balanced at 0.93 and 0.96 on both counts, and our cold-pool mt16 json.fast at 3318 MiB/s still beats zstd's warm-pool reference at 1638.

Streaming encode single-thread reads json.fast 0.95, json.opt 0.85 and json.ultra 1.05, text.best 0.64, and text.fastest and fast 0.36 and 1.05 as in-pass readings, because the same-process page-dispersity collapse reproduced for the eleventh consecutive pass; the standalone medians read 6149 and 5938 MiB/s against R94's 6504 and 6021.

Streaming encode MT8 keeps every json cell at or below x0.66. Every text cell wins, with balanced at 0.83. Dll100 stream-mt8 leads at fast 0.51, opt 0.50 and ultra 0.68, and trails at fastest 2.02, balanced 1.45 and best 1.58, running at 88 to 98 percent of its own bulk-mt8 ceilings, with the balanced row's 71 percent a placement reading of its single-slowest-job wall.

Decode MT widened on two fronts: json mt16 at 1.58 times this pass's single-thread and 1.70 times the zstd stream reference, dll100.zst3 mt16 at 1.80 and 1.87, and the lane budget lifted skewed from R94's flat 0.99 times single-thread to 1.72 at mt8 and 2.13 times the reference. The multi-frame payloads scale at 2.84 to 3.09 times single-thread at mt16, the strongest decode scaling rows on any payload.

The ratio sweep holds the +8.69% geo-mean over 120 cells, with 108 cells byte-identical to R94 and the twelve movers sitting inside the window's two documented trades: the fill-descent cut's eight opt cells at +0.12 to +0.34 percent and the repcode gate's four skewed.best cells at -0.022 percent. The cut flipped json.opt from its 9-byte win to -0.12 percent, so the losing set grows by two opt cells at the documented bar.

The full 1-22 ladder holds its shape and deepened: json at levels 17 and 19 reads 0.85 and 0.95 through the cut at +0.13 percent bytes, with level 22 at 0.53, json rows 5-8 and 10-12 at 1.18 to 1.33 with their byte wins, text chain rows at 0.33 to 0.72 with level 9 at 0.94, zeros levels 1-4 at 0.19 to 0.29, random levels 17-19 at 2181 to 3375 MiB/s at byte-exact ratio, and the skewed hole at levels 13-16 at 1.09 to 1.60. Small payloads held their improved band at identical bytes, with json l3 4K at 1.12 and text l3 4K at 1.08.

## Decode single-thread

Throughput is MiB/s of raw data for bulk decode and streaming decode with 64 KiB pulls. x is our time divided by zstd's time, so below 1 means we are faster.

| file | bulk ours | bulk zstd | bulk x | stream ours | stream zstd | stream x |
|---|---:|---:|---:|---:|---:|---:|
| json.zst1 | 2235 | 2239 | 1.00 | 2137 | 2192 | 1.03 |
| json.zst3 | 2071 | 2010 | 0.97 | 1847 | 1869 | 1.01 |
| json.zst9 | 2424 | 2332 | 0.96 | 2120 | 2145 | 1.01 |
| json.zst19 | 2653 | 2612 | 0.98 | 2551 | 2619 | 1.03 |
| text.zst1 | 7355 | 6728 | 0.91 | 7345 | 7549 | 1.03 |
| text.zst3 | 9663 | 9191 | 0.95 | 10951 | 11133 | 1.02 |
| text.zst9 | 10155 | 9849 | 0.97 | 11820 | 12137 | 1.03 |
| text.zst19 | 10185 | 9848 | 0.97 | 11582 | 12155 | 1.05 |
| skewed.zst1 | 3177 | 3078 | 0.97 | 3060 | 2848 | 0.93 |
| skewed.zst3 | 1643 | 1630 | 0.99 | 1551 | 1531 | 0.99 |
| skewed.zst9 | 978 | 882 | 0.90 | 820 | 791 | 0.96 |
| skewed.zst19 | 2903 | 2969 | 1.02 | 2674 | 2688 | 1.01 |
| random.zst3 | 8982 | 8909 | 0.99 | 11088 | 8910 | 0.80 |
| zeros.zst3 | 12132 | 11244 | 0.94 | 12882 | 12928 | 1.00 |
| dll100.zst1 | 1457 | 1562 | 1.07 | 1410 | 1511 | 1.07 |
| dll100.zst3 | 1690 | 1733 | 1.03 | 1629 | 1707 | 1.05 |
| dll100.zst9 | 2077 | 2126 | 1.02 | 1983 | 2078 | 1.05 |
| dll100.zst19 | 1796 | 1825 | 1.02 | 1736 | 1806 | 1.04 |

Every corpus shape reads at or under parity bulk except json.zst1 at 1.00 and skewed.zst19 at 1.02, and the stream column holds 0.93 to 1.05 with random's 0.80 the widest win. Dll100's 1.02 to 1.07 band is the FSE-build residue and the copy-loop lumps in [dec-gap](dec-gap.md).

## Encode single-thread bulk

Throughput and ratio are per shape, with checksums off on both sides. The tier column names the ladder tier and the pairing is 1, 3, 9, 13, 17 and 19 against libzstd. x is our time divided by zstd's, so below 1 means we are faster. These cells share the machine in pairs, so x is the comparable column.

| level | shape | ours MiB/s | ours ratio | zstd MiB/s | zstd ratio | x |
|---|---|---:|---:|---:|---:|---:|
| fastest | json | 629 | 6.41 | 816 | 6.11 | 1.30 |
| fastest | text | 11689 | 309.23 | 9981 | 308.94 | 0.84 |
| fastest | skewed | 2578 | 2.00 | 851 | 2.00 | 0.46 |
| fastest | random | 2071 | 1.00 | 1916 | 1.00 | 0.94 |
| fastest | zeros | 48704 | 32577 | 9078 | 32171 | 0.27 |
| fastest | dll | 478 | 4.35 | 527 | 2.19 | 1.10 |
| fast | json | 442 | 5.30 | 461 | 5.29 | 1.04 |
| fast | text | 12295 | 333.03 | 7034 | 332.90 | 0.58 |
| fast | skewed | 207 | 1.92 | 219 | 1.92 | 1.06 |
| fast | random | 2096 | 1.00 | 1909 | 1.00 | 0.93 |
| fast | zeros | 47828 | 32577 | 8563 | 32171 | 0.19 |
| fast | dll | 412 | 4.97 | 419 | 3.40 | 1.00 |
| balanced | json | 145 | 7.20 | 111 | 5.95 | 0.76 |
| balanced | text | 1743 | 384.66 | 1722 | 378.41 | 0.99 |
| balanced | skewed | 1719 | 2.00 | 71 | 1.84 | 0.043 |
| balanced | random | 2191 | 1.00 | 1383 | 1.00 | 0.63 |
| balanced | zeros | 32070 | 32577 | 1735 | 32202 | 0.056 |
| balanced | dll | 131 | 5.35 | 144 | 4.62 | 1.11 |
| best | json | 21 | 6.26 | 30 | 6.10 | 1.46 |
| best | text | 711 | 386.42 | 642 | 385.90 | 0.89 |
| best | skewed | 5 | 1.84 | 8 | 1.84 | 1.60 |
| best | random | 2135 | 1.00 | 116 | 1.00 | 0.055 |
| best | zeros | 44229 | 32577 | 770 | 32171 | 0.018 |
| best | dll | 25 | 5.35 | 31 | 4.67 | 1.17 |
| opt | json | 6 | 7.48 | 6 | 7.49 | 0.87 |
| opt | text | 895 | 408.84 | 428 | 410.11 | 0.48 |
| opt | skewed | 2 | 2.00 | 2 | 2.00 | 0.86 |
| opt | random | 2210 | 1.00 | 9 | 1.00 | 0.004 |
| opt | zeros | 40861 | 32577 | 838 | 32202 | 0.021 |
| opt | dll | 13 | 6.03 | 11 | 5.06 | 0.89 |
| ultra | json | 3 | 7.42 | 3 | 7.42 | 1.09 |
| ultra | text | 421 | 414.12 | 242 | 413.98 | 0.57 |
| ultra | skewed | 2 | 2.00 | 2 | 2.00 | 0.96 |
| ultra | random | 2197 | 1.00 | 6 | 1.00 | 0.003 |
| ultra | zeros | 43232 | 32577 | 590 | 32202 | 0.016 |
| ultra | dll | 8 | 6.32 | 9 | 5.28 | 1.05 |

The opt rows carry the fill-descent cut: json.opt reads 0.87 in-pass and 0.855 on the serial rerun against R94's 1.01, at +0.13 percent bytes that flip the cell's 9-byte size win to -0.12 percent. Text.best read 0.89 in-pass through pair-sharing and settles at 0.686 on the serial rerun, the R94 band. We stay denser or at parity on json and text at every level, skewed wins everywhere except the chain-row near-tie, random and zeros tie or win, and dll is denser at every tier at 13.7 percent or more from balanced up. The dll balanced cell traded +0.36 percent bytes for its diet and reads 1.11 against R94's 1.16 at a higher absolute wall. Checksums on versus off on our side read json.fast 1.03 and text.fast 0.94.

## Encode multi-thread bulk

Throughput is MiB/s at 8 and 16 workers with one pass, using a cold pool per call on both sides. The x8 and x16 columns are our time divided by zstd's at the same worker count, and the ratio columns are at 16 workers.

| cell | ours mt8 | zstd mt8 | x8 | ours mt16 | zstd mt16 | x16 | ratio ours mt16 | ratio zstd mt16 |
|---|---:|---:|---:|---:|---:|---:|---:|---:|
| json.fastest | 3696 | 4128 | 1.14 | 5289 | 1777 | 0.34 | 6.35 | 6.11 |
| json.fast | 2497 | 1006 | 0.40 | 3318 | 1018 | 0.31 | 5.30 | 5.31 |
| json.balanced | 410 | 205 | 0.51 | 497 | 200 | 0.40 | 7.19 | 5.95 |
| text.fastest | 18969 | 10304 | 0.55 | 16133 | 2241 | 0.14 | 309.08 | 39.78 |
| text.fast | 11771 | 2284 | 0.19 | 11870 | 2130 | 0.18 | 332.94 | 189.16 |
| text.balanced | 1359 | 1260 | 0.93 | 1280 | 1229 | 0.96 | 384.59 | 378.30 |
| skewed.fastest | 4187 | 3233 | 0.77 | 3629 | 1605 | 0.44 | 2.00 | 2.00 |
| skewed.fast | 1284 | 649 | 0.51 | 1689 | 619 | 0.37 | 1.92 | 1.92 |
| skewed.balanced | 611 | 121 | 0.20 | 658 | 120 | 0.18 | 2.00 | 1.84 |

Json.fastest.mt8 at x1.14 is the one zstd MT win, inside the cell's known noise band of 0.95 to 1.65 across passes, and its mt16 cell wins at 0.34. Every other cell is ahead, with text.balanced at 0.93 and 0.96 and skewed.balanced at 0.20 and 0.18. Our cold-pool mt16 json.fast at 3318 MiB/s still beats zstd's warm-pool reference at 1638 this run.

## Streaming encode

Throughput uses 64 KiB pulls and interleaved medians, with single-thread and MT8 columns. x is our time divided by zstd's at the same mode, so below 1 means we are faster. The single-thread cells share the machine in pairs, so x is the comparable column.

| cell | ST ours | ST zstd | ST x | MT8 ours | MT8 zstd | MT8 x |
|---|---:|---:|---:|---:|---:|---:|
| json.fastest | 589 | 742 | 1.27 | 3312 | 1875 | 0.56 |
| json.fast | 460 | 437 | 0.95 | 2153 | 398 | 0.19 |
| json.balanced | 68 | 91 | 1.34 | 354 | 111 | 0.32 |
| json.best | 19 | 26 | 1.38 | 77 | 50 | 0.66 |
| json.opt | 6 | 5 | 0.85 | 30 | 7 | 0.23 |
| json.ultra | 2 | 2 | 1.05 | 10 | 3 | 0.30 |
| text.fastest | 4668* | 1696 | 0.36 | 7805 | 3769 | 0.49 |
| text.fast | 4644* | 4871 | 1.05 | 5789 | 1522 | 0.26 |
| text.balanced | 1313 | 1604 | 1.22 | 1175 | 984 | 0.83 |
| text.best | 901 | 628 | 0.64 | 818 | 360 | 0.44 |
| text.opt | 830 | 418 | 0.50 | 647 | 373 | 0.58 |
| text.ultra | 401 | 254 | 0.62 | 289 | 233 | 0.81 |

The two text single-thread fast cells carry in-pass readings at 4668 and 4644 MiB/s, and the same-process page-dispersity collapse reproduced for the eleventh consecutive pass, with the mechanism and reproducer in the [workflow pitfalls](../pitfalls/workflow.md); a verdict on either cell needs the standalone rerun protocol, and the standalone medians read 6149 and 5938 MiB/s against R94's 6504 and 6021. Unknown-size text.fastest streaming has zstd emitting 1.84 MB at ratio 18.3 against our 108 KB at ratio 309. Dll100 stream cells are in the [matrix](matrix.md) streaming section, with the balanced stream-mt8 regression recorded there.

## Decode multi-thread scaling

Fixtures are the same files as the single-thread table, and throughput is MiB/s at 1, 2, 4, 8 and 16 workers. The last column is zstd's single-thread streaming decode, because libzstd has no MT decode, so the scaling columns have no zstd counterpart.

| file | ours ST | mt2 | mt4 | mt8 | mt16 | zstd ST stream ref |
|---|---:|---:|---:|---:|---:|---:|
| json.zst3 | 2024 | 2582 | 2961 | 3160 | 3191 | 1875 |
| text.zst3 | 11712 | 11532 | 11396 | 11422 | 11498 | 11128 |
| skewed.zst9 | 985 | 1367 | 1608 | 1690 | 1624 | 795 |
| random.zst3 | 9751 | 9552 | 9550 | 9663 | 9091 | 8957 |
| dll100.zst3 | 1690 | 2389 | 2861 | 3020 | 3039 | 1629 |
| multi.zst3 | 3373 | 3076 | 6075 | 8964 | 9562 | 3199 |

Dll100 is measured through `files --threads`, with its reference taken from this pass's dll100.zst3 streaming cell, and the multi row is the files-multi section against the dec-ST stream reference. The lane-budget window re-based skewed upward: mt8 runs 1.72 times single-thread and 2.13 times the reference against R94's 0.99 and 1.21, and the multi-frame payloads reach 2.84 to 3.09 times single-thread at mt16 in the [matrix](matrix.md) files-multi table. Json mt16 reads 1.58 times single-thread and 1.70 times the reference, dll100 1.80 and 1.87, and text is flat at 0.98.

## Compression-ratio sweep

The sweep runs `zstdx-bench ratio` on 5 shapes, 6 tiers, bulk and stream modes and single-thread and 4-worker MT, one deterministic pass per cell with checksums off. The reported delta is ours over zstd's ratio minus 1, aggregated as a geo-mean, and every cell is roundtrip-gated. The geo-mean over 120 cells is **+8.69%**, with 108 cells reproducing the R94 sizes byte for byte, the eight opt movers at +0.12 to +0.34 percent from the fill-descent cut and the four skewed.best movers at -0.022 percent from the repcode gate. By mode it reads bulk single-thread +1.47%, bulk MT +11.08%, stream single-thread +11.42% and stream MT +11.11%. By shape it reads json +4.44%, text +40.29%, skewed +1.42%, random plus or minus zero and zeros +2.06%. The losing set is json.opt and text.opt stream-mt from the cut inside its bar, json.fast bulk MT at -0.12% and stream MT at -0.11%, skewed.opt at -0.04% to -0.05% and skewed.ultra at -0.03% to -0.04%. Details are in the [matrix](matrix.md) ratio section.

## Open and closed deficits

Each row names a measured deficit and its status in this pass. The x figures are wall-time ratios, so values below 1 mean we are faster.

| Deficit | Status in this pass |
|---|---|
| Small-payload level 9 | Closed on 2026-09-28. The flip was the small-src btlazy swap, now an 8 KiB head verdict with a 48-distinct bar. The cells read json level 9 at x1.08, 0.97, 0.66 and 1.02 at 1K, 4K, 64K and 1M on the stock row, text at 1.67, 1.69, 2.88 and 1.02 on the swap row. The level 1 slide at 1K-4K remains the accepted trade |
| Json chain rows 5-8 and 10-12 | Closed on 2026-10-01 with the head-gated attempts diet. The rows carry the ladder at x1.18 to 1.33 and the byte wins against zstd, with the mechanism and `RowAttempts::Light` in [matchers](../perf/matchers.md) |
| text.balanced speed | Closed on 2026-10-30 by the balanced row substitution. Bulk reads x0.99 single-thread on the pair-shared session band, 0.93 at mt8 and 0.96 at mt16, stream single-thread reads 1.22 and stream-mt8 0.83. Ratio holds +1.65% denser than zstd-9 |
| Decode single-thread on compressible shapes | Closed on 2026-11-02 by the libzstd-shaped sequence loop: x0.90 to 1.03 bulk and 0.93 to 1.05 stream on json, text and skewed. The method was a reference-compiler differential, and the remaining dll100 band of 1.02 to 1.07 is in [dec-gap](dec-gap.md) |
| json.fastest | x1.30 single-thread and 1.14 mt8, both inside the cells' known spreads. The steady scan loop's branch-mispredict budget sets a realistic ceiling of about x1.3 to 1.4, and the issue-and-serialization decomposition closed the asm family. Json.fast at 1.04 is near closed |
| Best-tier core | Json 1.46 in-pass and 1.26 on the serial rerun against R94's 1.33, skewed 1.60, n=3 wander at identical bytes, a memory-latency tree walk with its floor in the [todo](../todo.md). It caps the json.best stream single-thread cell at x1.38, while json.best.stream-mt8 reads 0.66 |
| json opt and ultra | The R95 fill-descent cut moved json.opt to x0.85 serial and 0.87 in-pass at +0.13 percent bytes, flipping its 9-byte size win to -0.12 percent inside the cut's stated +0.5 percent bar. Ultra holds 1.05 to 1.09 at parity bytes, and dll100.opt reads 0.89 at +0.25 percent bytes |
| MT decode scaling | The lane-budget window lifted skewed from flat to 1.72 times single-thread at mt8 and 2.13 times the reference, and the multi-frame payloads run 2.84 to 3.09 times single-thread at mt16. Json mt16 reads 1.58 and 1.70, dll100 1.80 and 1.87, text flat at 0.98. The walls are sequence-read and aggregate-bandwidth bound, tracked as todo 1 in the [todo](../todo.md) |
| dll100 balanced | Bulk reads x1.11 at -13.7% size after the diet traded +0.36 percent bytes for wall, with best at 1.17 and the deep rows ahead at 0.89 and 1.05. The chain chase remains, with its floor in the [todo](../todo.md) and its differential attribution in [enc-dll-gap](enc-dll-gap.md). Fastest and fast are speed near-parity at 1.10 and 1.00 carrying -49.6% and -31.7% size |
| text single-thread stream fastest and fast | The in-pass page-dispersity collapse reproduced for the eleventh consecutive pass at 4668 and 4644 MiB/s in-pass, at x0.36 and 1.05. It was root-caused on 2026-09-29 in the [workflow pitfalls](../pitfalls/workflow.md), where a best or L13 row preceding the pooled single-thread core scatters the big buffers' pages and THP recovers about 11%, leaving the residual unattributed. Both cells still win on the standalone medians, 6149 and 5938 MiB/s at x0.27 and 0.83 |
| Fast-row screen tax on compressible-flat data | Random fastest and fast read x0.94 and 0.93 from the shortened bar leg plus the 1 MiB twin-scan reject, a deliberate arming-recall bound. Zeros fastest and fast read 0.27 and 0.19, and skewed chain rows read at or below 0.06. Every cell still wins |
| Ratio residues | Json.opt at -0.12% and text.opt stream-mt at -0.31% from the fill-descent cut inside its +0.5 percent bar, json.fast MT at -0.11% to -0.12%, skewed.opt at -0.04% and skewed.ultra at -0.03% to -0.04% as near-ties, and the dictionary small-payload parse on the encoding side |
| dll100 streaming-MT encode | Stream-mt8 trails libzstd-mt8 at three of six tiers: fastest x2.02, balanced 1.45 and best 1.58; leads at fast 0.51, opt 0.50 and ultra 0.68. The balanced row read 128 this pass against R94's 174 at byte-identical output, and the same-day A/B against the pre-R94 tree prices it as placement rather than code: 155 against 159 in the pass context with HEAD at 1.74 percent fewer instructions, the cell's wall being its single slowest 12.5 MiB job of eight, which the two-CCD host moves between roughly 160 and 200 MiB/s through L3 sharing and boost state. The cells run at 90, 88, 71, 98, 94 and 93 percent of their own bulk-mt8 ceilings of 1041, 959, 181, 56, 53 and 29 MiB/s, and zstd's 1744 MiB/s fastest headline buys 48 MB of output against our 24 MB, so the two run at different output sizes |
| Placement-phase regressions | Closed class since 2026-10-25: the nbSeq emit fix shifted the dubt count loop onto a 64-byte line boundary for a 10% dll100.best loss at identical instruction counts. The R32 two-copy entry stub in `count_run` now pins the loop head at a phase of 31 or less, following `extend_match_aligned`. The full walk is in [matchers](../perf/matchers.md) |
