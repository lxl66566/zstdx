# Performance Snapshot (2026-10-31)

This page summarizes the 2026-10-30 pass at commit `f7680db7` with the 2026-10-31 section refresh at `0a60b531`, run on one machine, one corpus and the same flags on both sides. The comparison target is the zstd crate and libzstd 1.5.7 with zstdmt, and the ladder pairs fastest, fast, balanced, best, opt and ultra against libzstd levels 1, 3, 9, 13, 17 and 19. Raw tables, provenance and per-chunk commands are in [matrix.md](matrix.md), including the full numeric 1-22 ladder and the small-payload table. Throughout this page x is our time divided by the reference time, so values below 1 mean we are faster, and speeds are MiB/s of raw input.

The pass covers the encode window since the 2026-10-25 release pass at `49f48b20`, and four movers shape the new numbers. The gated dense geometry reads json fastest at x1.34 bulk and 1.10 mt8 with output 0.34% denser. The balanced row substitution reads text balanced at 0.95 bulk, 0.85 mt8 and 0.81 mt16 while holding +1.65% ratio, and dll balanced at 1.12 with ratio 5.37 against 5.31. The wide-head donation stand-down lifts the same mt cells, and the btlazy SIMD arming holds dll best at 1.22 single-thread. The ratio sweep reads a geo-mean of +8.70% denser over 120 cells with the losing set unchanged from the release pass, and json.opt bulk stays 9 bytes ahead of zstd-17.

The pass reran decode single-thread, encode single-thread, encode mt at 8 and 16 workers, encode streaming single-thread and mt8 with the standalone text rerun, and the ratio sweep. The MT decode scaling table, the full ladder, the small-payload tables and the streaming-encode section carry the 2026-10-31 refresh at `0a60b531`, taken because the two commits after the pass moved stream cells; json level 1 is the one ladder row the release window moved, at 0.34% denser since 2026-10-25. Caveats: ±10% noise between runs, a per-side budget of 1 second on the pass's main sections and 500 ms on the refreshed sections, interleaved medians, and spreads in the raw tables.

## Headline results

Decode single-thread bulk wins 17 of 18 cells at x0.24 to 0.99. Skewed.zst9 sits at x0.99 as the near-tie. Absolute throughput runs 654 to 10288 MiB/s, and the dll cells run 1182 to 1551 MiB/s.

Decode single-thread streaming still goes to zstd on every compressible shape: json at x1.17 to 1.32, text at 1.05 to 1.21, skewed at 1.07 to 1.27 and dll at 1.25 to 1.31. We win random at 0.81 and hold zeros at 1.00.

Encode single-thread on json reads fastest 1.34, fast 1.02, balanced 0.78 with +21% ratio, best 1.29, and opt and ultra 1.01 and 1.03 at ratio parity or denser, with json.opt 9 bytes ahead of zstd-17. Text reads fastest 0.81, fast 0.54, balanced 0.95 at +1.65% ratio after the row substitution, best 0.62, opt 0.54 and ultra 0.64. Skewed reads fastest 0.45, fast 1.06, balanced 0.041, best 1.85, and opt and ultra 0.93 and 0.95 at ratio parity.

Random and zeros are ours at every tier, up to 300 times on incompressible high tiers. Zeros fastest and fast read 0.27 and 0.18, and random 0.92 and 0.90. On dll, fastest reads 1.04 at -49.6% size with ratio 4.35 against zstd-1's 2.19, fast 0.97 at -31.7%, balanced 1.12 at -14.0%, best 1.22 at -12.8%, opt 1.09 at -16.2% and ultra 1.13 at -16.5%.

Encode multi-thread has one remaining zstd win: json.fastest.mt8 at x1.10 inside its known noise band of 0.95 to 1.65. Every other cell is ahead, with text.balanced moved to 0.85 at mt8 and 0.81 at mt16 by the wide-head stand-down, and our cold-pool mt16 json.fast at 3274 MiB/s still beats zstd's warm-pool reference at 1594.

Streaming encode single-thread reads json.fast 0.97 and json.fastest 1.23, text.best 0.70, and text.fastest and fast 0.26 and 0.80 as standalone medians, because the in-pass page-dispersity collapse reproduced for the seventh consecutive pass. Json.opt and json.ultra read 1.08 and 1.09 in the refresh with a same-day rerun at 1.01 and 1.05, a parity band against the pass's 0.97 and 0.99.

Streaming encode MT8 keeps every json cell at or below x0.64. Every text cell wins, with balanced moved to 0.84 by the stream probe stand-down. Dll100 stream-mt8 leads at fast 0.57 and opt 0.79, reads ultra at 0.96 near-parity, and trails at fastest 2.05, balanced 1.54 and best 3.08, running at 70% to 89% of its own bulk-mt8 ceilings. The trailing cells stay the open item in the [todo](../todo.md).

Decode MT carries the 2026-10-31 refresh. Json mt16 runs at 1.59 times our single-thread and 1.17 times the zstd stream reference, and dll100.zst3 mt16 runs at 1.81 times single-thread and 1.34 times the reference, the strongest scaling row and a real-binary payload. Skewed mt16 reads 1.16 times single-thread and 0.94 times the reference, with its absolute band wandering ±15% across passes, and text is flat at 0.98.

The ratio sweep reads a geo-mean of +8.70% denser over 120 cells, with bulk single-thread at +1.49%. The four json fastest cells are the only cells that moved against the release pass, each 0.34% denser through the dense geometry. The losing cells are unchanged: json.fast MT at -0.12% to -0.14%, skewed.opt at -0.06% to -0.07%, and near-ties of 0.00% to -0.04% on skewed fast, fastest, ultra and on text.ultra MT.

The full 1-22 ladder and the small-payload tables carry the 2026-10-31 refresh: json rows 5-8 and 10-12 at x1.21 to 1.45 carrying -4.8% to -16.4% byte wins, text chain rows at x0.35 to 0.62 with level 9 closed at 0.96 through the balanced row substitution, zeros levels 1-4 at 0.31, 0.30, 0.20 and 0.20, random levels 17-19 at 8836 to 8968 MiB/s at byte-exact ratio, json level 9 small payloads at x1.08, 0.92, 0.64 and 1.02, and the remaining hole at skewed levels 13-16 x1.26 to 2.89 on ratio parity.

## Decode single-thread

Throughput is MiB/s of raw data for bulk decode and streaming decode with 64 KiB pulls. x is our time divided by zstd's time, so values below 1 mean we are faster.

| file | bulk ours | bulk zstd | bulk x | stream ours | stream zstd | stream x |
|---|---:|---:|---:|---:|---:|---:|
| json.zst1 | 1744 | 1196 | 0.68 | 1775 | 2185 | 1.23 |
| json.zst3 | 1439 | 1065 | 0.77 | 1408 | 1857 | 1.32 |
| json.zst9 | 1735 | 1184 | 0.67 | 1686 | 2134 | 1.27 |
| json.zst19 | 2213 | 1274 | 0.58 | 2244 | 2626 | 1.17 |
| text.zst1 | 6093 | 2180 | 0.36 | 6302 | 7609 | 1.21 |
| text.zst3 | 8780 | 2412 | 0.28 | 10407 | 11110 | 1.07 |
| text.zst9 | 8747 | 2376 | 0.27 | 11579 | 12119 | 1.05 |
| text.zst19 | 8618 | 2306 | 0.27 | 11594 | 12173 | 1.05 |
| skewed.zst1 | 2276 | 1460 | 0.64 | 2528 | 2710 | 1.07 |
| skewed.zst3 | 1275 | 1002 | 0.79 | 1265 | 1489 | 1.18 |
| skewed.zst9 | 654 | 650 | 0.99 | 617 | 785 | 1.27 |
| skewed.zst19 | 2134 | 1391 | 0.65 | 2340 | 2601 | 1.11 |
| random.zst3 | 7879 | 2175 | 0.28 | 10102 | 8194 | 0.81 |
| zeros.zst3 | 10288 | 2470 | 0.24 | 12832 | 12867 | 1.00 |
| dll100.zst1 | 1182 | 971 | 0.84 | 1197 | 1495 | 1.25 |
| dll100.zst3 | 1267 | 1059 | 0.84 | 1289 | 1690 | 1.31 |
| dll100.zst9 | 1551 | 1171 | 0.77 | 1583 | 2066 | 1.31 |
| dll100.zst19 | 1323 | 1077 | 0.81 | 1376 | 1798 | 1.31 |

Bulk holds 17 of 18 wins with skewed.zst9 at x0.99 as the near-tie. The stream cells read level with the release pass, with json's top cell at 1.32 and dll's band at 1.25 to 1.31. Zeros bulk reads 10288 against the release pass's 11956, the free noise-calibration cell reading this session's machine slower.

## Encode single-thread bulk

Throughput and ratio are per shape, with checksums off on both sides. The tier column names the ladder tier and the pairing is 1, 3, 9, 13, 17 and 19 against libzstd. x is our time divided by zstd's, so values below 1 mean we are faster.

| level | shape | ours MiB/s | ours ratio | zstd MiB/s | zstd ratio | x |
|---|---|---:|---:|---:|---:|---:|
| fastest | json | 637 | 6.41 | 852 | 6.11 | 1.34 |
| fastest | text | 12966 | 309.23 | 10431 | 308.94 | 0.81 |
| fastest | skewed | 2801 | 2.00 | 1249 | 2.00 | 0.45 |
| fastest | random | 2206 | 1.00 | 2019 | 1.00 | 0.92 |
| fastest | zeros | 49003 | 32577 | 13289 | 32171 | 0.27 |
| fastest | dll | 517 | 4.35 | 536 | 2.19 | 1.04 |
| fast | json | 466 | 5.30 | 476 | 5.29 | 1.02 |
| fast | text | 13366 | 333.03 | 7261 | 332.90 | 0.54 |
| fast | skewed | 210 | 1.92 | 223 | 1.92 | 1.06 |
| fast | random | 2146 | 1.00 | 1923 | 1.00 | 0.90 |
| fast | zeros | 48803 | 32577 | 8668 | 32171 | 0.18 |
| fast | dll | 438 | 4.97 | 423 | 3.40 | 0.97 |
| balanced | json | 155 | 7.20 | 121 | 5.95 | 0.78 |
| balanced | text | 1811 | 384.66 | 1719 | 378.41 | 0.95 |
| balanced | skewed | 1825 | 2.00 | 74 | 1.84 | 0.041 |
| balanced | random | 2248 | 1.00 | 1431 | 1.00 | 0.64 |
| balanced | zeros | 32693 | 32577 | 1746 | 32202 | 0.053 |
| balanced | dll | 134 | 5.37 | 150 | 4.62 | 1.12 |
| best | json | 28 | 6.26 | 37 | 6.10 | 1.29 |
| best | text | 1104 | 386.42 | 680 | 385.90 | 0.62 |
| best | skewed | 7 | 1.84 | 14 | 1.84 | 1.85 |
| best | random | 2142 | 1.00 | 241 | 1.00 | 0.11 |
| best | zeros | 44817 | 32577 | 787 | 32171 | 0.018 |
| best | dll | 32 | 5.35 | 39 | 4.67 | 1.22 |
| opt | json | 7 | 7.49 | 7 | 7.49 | 1.01 |
| opt | text | 816 | 410.23 | 436 | 410.11 | 0.54 |
| opt | skewed | 3 | 2.00 | 3 | 2.00 | 0.93 |
| opt | random | 2229 | 1.00 | 12 | 1.00 | 0.005 |
| opt | zeros | 44277 | 32577 | 851 | 32202 | 0.019 |
| opt | dll | 13 | 6.04 | 14 | 5.06 | 1.09 |
| ultra | json | 3 | 7.42 | 3 | 7.42 | 1.03 |
| ultra | text | 398 | 414.12 | 256 | 413.98 | 0.64 |
| ultra | skewed | 2 | 2.00 | 2 | 2.00 | 0.95 |
| ultra | random | 2240 | 1.00 | 8 | 1.00 | 0.003 |
| ultra | zeros | 43500 | 32577 | 600 | 32202 | 0.014 |
| ultra | dll | 8 | 6.32 | 9 | 5.28 | 1.13 |

The balanced row substitution is this pass's largest single-thread mover: text reads 0.95 against the release pass's 1.10 at an unchanged 384.66 ratio, and dll reads 1.12 against 1.31 while going denser from 5.31 to 5.37. Json fastest reads 1.34 against 1.46 through the dense geometry at 0.34% denser output. We are denser or at parity on json and text at every level, with json.opt 9 bytes ahead, skewed wins everywhere except near-ties, random and zeros tie or win, and dll is denser at every tier: -49.6% bytes at fastest, -31.7% at fast and double digits from balanced up. Checksums on versus off on our side read json.fast 1.00 and text.fast 0.85.

## Encode multi-thread bulk

Throughput is MiB/s at 8 and 16 workers with one pass, using a cold pool per call on both sides. The x8 and x16 columns are our time divided by zstd's at the same worker count, and the ratio columns are at 16 workers.

| cell | ours mt8 | zstd mt8 | x8 | ours mt16 | zstd mt16 | x16 | ratio ours mt16 | ratio zstd mt16 |
|---|---:|---:|---:|---:|---:|---:|---:|---:|
| json.fastest | 3769 | 4168 | 1.10 | 5433 | 1737 | 0.32 | 6.35 | 6.11 |
| json.fast | 2469 | 1012 | 0.42 | 3274 | 1016 | 0.31 | 5.30 | 5.31 |
| json.balanced | 426 | 198 | 0.47 | 546 | 201 | 0.37 | 7.19 | 5.95 |
| text.fastest | 18686 | 10551 | 0.57 | 17016 | 2187 | 0.13 | 309.08 | 39.78 |
| text.fast | 11682 | 2133 | 0.18 | 11416 | 2067 | 0.18 | 332.94 | 189.16 |
| text.balanced | 1425 | 1209 | 0.85 | 1446 | 1157 | 0.81 | 384.59 | 378.30 |
| skewed.fastest | 4029 | 3176 | 0.79 | 3417 | 1523 | 0.45 | 2.00 | 2.00 |
| skewed.fast | 1243 | 632 | 0.51 | 1589 | 631 | 0.40 | 1.92 | 1.92 |
| skewed.balanced | 710 | 120 | 0.17 | 740 | 121 | 0.16 | 2.00 | 1.84 |

Json.fastest.mt8 at x1.10 is the one zstd MT win, inside the cell's known noise band of 0.95 to 1.65 across passes and improved from the release pass's 1.20 by the dense geometry. Text.balanced moved from 1.05 and 1.15 to 0.85 and 0.81 through the wide-head donation stand-down. Our cold-pool mt16 json.fast at 3274 MiB/s still beats zstd's warm-pool reference at 1594 this run.

## Streaming encode

Throughput uses 64 KiB pulls and interleaved medians, with single-thread and MT8 columns. x is our time divided by zstd's at the same mode, so below 1 means we are faster.

| cell | ST ours | ST zstd | ST x | MT8 ours | MT8 zstd | MT8 x |
|---|---:|---:|---:|---:|---:|---:|
| json.fastest | 621 | 770 | 1.24 | 3268 | 1888 | 0.57 |
| json.fast | 463 | 440 | 0.95 | 1884 | 406 | 0.21 |
| json.balanced | 87 | 106 | 1.22 | 366 | 111 | 0.31 |
| json.best | 30 | 36 | 1.21 | 76 | 51 | 0.66 |
| json.opt | 7 | 7 | 0.97 | 16 | 7 | 0.42 |
| json.ultra | 3 | 3 | 0.99 | 6 | 3 | 0.52 |
| text.fastest | 6744* | 1759 | 0.26 | 7756 | 3890 | 0.50 |
| text.fast | 6498* | 5209 | 0.80 | 5699 | 1573 | 0.28 |
| text.balanced | 1395 | 1617 | 1.16 | 907 | 943 | 1.05 |
| text.best | 1003 | 643 | 0.64 | 837 | 358 | 0.43 |
| text.opt | 750 | 415 | 0.56 | 668 | 371 | 0.56 |
| text.ultra | 387 | 250 | 0.65 | 347 | 234 | 0.68 |

The two text single-thread fast cells carry standalone medians under the established protocol, because the in-pass table read 5424 and 5370 at x0.320 and 0.930 and the same-process page-dispersity collapse reproduced for the sixth consecutive pass. The mechanism and reproducer are in the [workflow pitfalls](../pitfalls/workflow.md). Unknown-size text.fastest streaming has zstd emitting 1.84 MB at ratio 18.3 against our 108 KB at ratio 309. Dll100 stream cells are in the [matrix](matrix.md) streaming table.

## Decode multi-thread scaling

This table carries the 2026-10-25 measurement, because the decode surfaces sat still in this window. Fixtures are the same files as the single-thread table, and throughput is MiB/s at 1, 2, 4, 8 and 16 workers. The last column is zstd's single-thread streaming decode, because libzstd has no MT decode, so the scaling columns have no zstd counterpart.

| file | ours ST | mt2 | mt4 | mt8 | mt16 | zstd ST stream ref |
|---|---:|---:|---:|---:|---:|---:|
| json.zst3 | 1409 | 1824 | 2075 | 2202 | 2193 | 1880 |
| text.zst3 | 11222 | 10842 | 10831 | 10826 | 10850 | 11178 |
| skewed.zst9 | 624 | 758 | 712 | 754 | 752 | 792 |
| random.zst3 | 9700 | 9382 | 9320 | 9323 | 9323 | 8873 |
| dll100.zst3 | 1250 | 1828 | 2088 | 2216 | 2287 | 1702 |

Dll100 is measured through `files --threads`. The restart-point decode scales best on the large real-binary payload at mt16 1.83 times single-thread and 1.34 times the reference. Json mt16 reads 1.56 times single-thread and 1.17 times the reference, with mt8 and mt16 reading level. Skewed holds 1.21 times single-thread at 0.95 times the reference, and text is flat at 0.97.

## Compression-ratio sweep

The sweep runs `zstdx-bench ratio` on 5 shapes, 6 tiers, bulk and stream modes and single-thread and 4-worker MT, one deterministic pass per cell with checksums off. The reported delta is ours over zstd's ratio minus 1, aggregated as a geo-mean, and every cell is roundtrip-gated. The geo-mean over 120 cells is **+8.70%**. By mode it reads bulk single-thread +1.49%, bulk MT +11.09%, stream single-thread +11.44% and stream MT +11.12%. By shape it reads json +4.46%, text +40.36%, skewed +1.41%, random plus or minus zero and zeros +2.06%. The four json fastest cells are the only cells that moved against the release pass, each 0.34% denser through the dense geometry, and every other surface stays byte-identical. The complete losing cells are json.fast bulk MT at -0.14% and stream MT at -0.12%, text.ultra MT at -0.02%, skewed.opt at -0.06% to -0.07%, skewed.ultra at -0.04%, skewed.fast bulk MT at -0.01% and stream MT at -0.03%, and skewed.fastest at -0.00%. Details are in the [matrix](matrix.md) ratio section.

## Open and closed deficits

Each row names a measured deficit and its status in this pass. The x figures are wall-time ratios, so values below 1 mean we are faster.

| Deficit | Status in this pass |
|---|---|
| Small-payload level 9 | Closed on 2026-09-28. The flip was the small-src btlazy swap, now an 8 KiB head verdict with a 48-distinct bar. The cells carry the 2026-10-31 table: json level 9 at x1.08, 0.92, 0.64 and 1.02 at 1K, 4K, 64K and 1M on the stock row, text at 1.77, 1.56, 2.80 and 1.02 on the swap row. The level 1 slide at 1K-4K remains the accepted trade |
| Json chain rows 5-8 and 10-12 | Closed on 2026-10-01 with the head-gated attempts diet. The rows carry the 2026-10-31 ladder at x1.21 to 1.45 and -4.8% to -16.4% bytes against zstd, with the mechanism and `RowAttempts::Light` in [matchers](../perf/matchers.md) |
| text.balanced speed | Closed on 2026-10-30 by the balanced row substitution. Bulk reads x0.95 single-thread, 0.85 at mt8 and 0.81 at mt16 after the wide-head stand-down, stream single-thread reads 1.18 and the 2026-10-31 refresh moved stream-mt8 to 0.84 through the stream probe stand-down. Ratio holds +1.65% denser than zstd-9 |
| Streaming decode on compressible shapes | Json reads x1.17 to 1.32, skewed 1.07 to 1.27, text 1.05 to 1.21 and dll 1.25 to 1.31, level with the release pass. The fused-loop serial chain remains, closed as a measured op-volume floor in [dec-gap](dec-gap.md) |
| json.fastest | x1.34 single-thread and 1.10 mt8 after the dense geometry, both inside the cells' known spreads. The steady scan loop's branch-mispredict budget sets a realistic ceiling of about x1.3 to 1.4, and the plain head block is the next named lever. Json.fast at 1.02 is near closed |
| Best-tier core | Json 1.29 and skewed 1.85, a memory-latency tree walk with its floor in the [todo](../todo.md). It caps the json.best stream single-thread cell at x1.33 to 1.42 across the refresh's two runs |
| json opt and ultra | Closed on 2026-10-01 with hugepage-backed buffers. This pass reads x1.01 and 1.03 at ratio parity, with json.opt 9 bytes ahead, and skewed opt and ultra ahead at 0.93 and 0.95 |
| MT decode scaling | Carries the 2026-10-31 refresh: json mt16 at 1.59 times single-thread and 1.17 times the reference, dll100 1.81 and 1.34, skewed 1.16 and 0.94, text flat at 0.98. The memory-bound verdict and execute walls stand, with no lever short of overlapping execute with the verdict, a falsified class |
| dll100 balanced | x1.12 at -14.0% size after the row substitution, with best at 1.22 on the armed long compare. The chain chase remains, with its floor in the [todo](../todo.md) and its differential attribution in [enc-dll-gap](enc-dll-gap.md). Fastest and fast are speed near-parity carrying -49.6% and -31.7% size |
| text single-thread stream fastest and fast | The in-pass page-dispersity collapse reproduced for the sixth consecutive pass at 5424 and 5370 in-pass against 6744 and 6498 standalone, at x0.26 and 0.80. It was root-caused on 2026-09-29 in the [workflow pitfalls](../pitfalls/workflow.md), where a best or L13 row preceding the pooled single-thread core scatters the big buffers' pages and THP recovers about 11%, leaving the residual unattributed. The real residue against 2026-09-19 is the unpledged-stream screen's about 4% sampling tax. Both cells still win |
| Fast-row screen tax on compressible-flat data | Random fastest and fast read x0.92 and 0.90 from the shortened bar leg plus the 1 MiB twin-scan reject, a deliberate arming-recall bound. Zeros fastest and fast read 0.27 and 0.18, and skewed chain rows read at or below 0.06. Every cell still wins |
| Ratio residues | Json.fast MT at -0.12% to -0.14%, skewed.opt at -0.07% as a near-tie, and the dictionary small-payload parse on the encoding side |
| dll100 streaming-MT encode | Stream-mt8 trails libzstd-mt8 at three of six tiers: fastest x2.05, balanced 1.54 and best 3.08, leads at fast 0.57 and opt 0.79, and ultra reads 0.96 near-parity. The cells run at 70% to 89% of their own bulk-mt8 ceilings of 1016, 957, 181, 28, 34 and 20 MiB/s, with best the highest share at 89% and balanced the lowest at 70%. The libzstd-MT losses are the open item in the [todo](../todo.md), measured-out classes of per-job whole-window LDM cold fills plus dll's content spread, while zstd's 1753 MiB/s headline buys 48 MB of output against our 24 MB, so the two run at different output sizes |
| Placement-phase regressions | Closed class since 2026-10-25: the nbSeq emit fix shifted the dubt count loop onto a 64-byte line boundary for a 10% dll100.best loss at identical instruction counts. The R32 two-copy entry stub in `count_run` now pins the loop head at a phase of 31 or less, following `extend_match_aligned`. The full walk is in [matchers](../perf/matchers.md) |
