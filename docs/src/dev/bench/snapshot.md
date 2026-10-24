# Performance Snapshot (2026-10-18)

This page summarizes the 2026-10-18 pre-release pass at commit `add0b0be`, run on one machine, one corpus and the same flags on both sides. The comparison target is the zstd crate and libzstd 1.5.7 with zstdmt, and the ladder pairs fastest, fast, balanced, best, opt and ultra against libzstd levels 1, 3, 9, 13, 17 and 19. Raw tables, provenance and per-chunk commands are in [matrix.md](matrix.md), including the full numeric 1-22 ladder and the small-payload table. Encoder outputs are byte-identical to the 2026-10-01 pass: all 120 ratio cells and all 110 ladder cells are size-equal and the geo-mean stays at +8.69%. Speed moves concentrate where the seventeen interim commits point: the reach-probe parse takes text.balanced bulk single-thread from x1.34 to 1.10 and its ladder row from x1.34 to 1.11, and json.balanced from 0.84 to 0.77. The tree tiers read at or inside their bands, with json.best 1.28 to 1.23, dll.best 1.42 to 1.26, json.ultra 1.03 to 1.00 and skewed.opt 0.95 to 0.92. Decode stream cells drift a few percent better, decode MT json mt16 moves from 1.60 to 1.65 times single-thread and dll100 from 1.83 to 1.90, and text.balanced stream-mt8 closes to x0.95, so every stream-mt cell now wins. Caveats: ±10% noise between runs, a per-side budget of 1 second and 500 ms on the full ladder and small payloads, interleaved medians, and spreads in the raw tables. Throughout this page x is our time divided by the reference time, so values below 1 mean we are faster, and speeds are MiB/s of raw input.

## Headline results

Decode single-thread bulk wins 17 of 18 cells at x0.21 to 1.00. Skewed.zst9 sits exactly at parity at x1.00. Absolute throughput runs 640 to 12236 MiB/s, and the dll cells run 1157 to 1536 MiB/s.

Decode single-thread streaming still goes to zstd on every compressible shape: json at x1.13 to 1.33, text at 1.04 to 1.20, skewed at 1.08 to 1.28 and dll at 1.28 to 1.34. We win random at 0.80 and hold zeros at 1.00, and the band tightened 1% to 6% against the previous pass.

Encode single-thread on json reads fastest 1.45, fast 1.03, balanced 0.77 with +21% density, best 1.23, and opt and ultra 1.01 and 1.00 at ratio parity or denser, with json.opt 9 bytes ahead of zstd-17. Four of six json tiers are now at parity or ahead. Text reads fastest 0.79, fast 0.54, balanced 1.10 at +1.65% density, best 0.61, opt 0.55 and ultra 0.64. Skewed reads fastest 0.44, balanced 0.041, fast 1.07, best 1.91, and opt and ultra 0.92 and 0.95 at ratio parity.

Random and zeros are ours at every tier, up to 300 times on incompressible high tiers. Zeros fastest and fast read 0.27 and 0.18, and random 0.90 and 0.84. On dll, fastest reads 1.06 at -49.6% size with ratio 4.35 against zstd-1's 2.19, fast 0.97 at -31.7%, balanced 1.30 at -13.0%, best 1.26 at -12.8%, opt 1.09 at -16.2% and ultra 1.12 at -16.5%.

Encode multi-thread has one remaining zstd win: json.fastest.mt8 at x1.19 with a spread of 0.95 to 1.68. Every other cell is ahead or inside text.balanced's own 0.99 to 1.47 spread, and our cold-pool mt16 json.fast at 3307 MiB/s still beats zstd's warm-pool reference at 1593.

Streaming encode single-thread reads json.fast 0.94 and json.fastest 1.32, text.best 0.63, and text.fastest and fast 0.25 and 0.79 as standalone medians, because the in-pass page-dispersity collapse reproduced for the fourth consecutive pass. Json.opt at 0.97 and json.ultra at 0.98 stay wins.

Streaming encode MT8 has every json and text cell ahead. Text.balanced reads 0.95 and is a win now, and every other cell is at or below x0.64. Dll100 stream-mt8 reproduces the earlier addendum: ahead at fast 0.80 and opt 0.95, trailing at fastest 3.52, balanced 1.15, ultra 1.21 and best 5.39, where the `count_from_long` SIMD fix held. The later buffer-reserve fix closed the scheduling half at byte-identical sizes: fastest moved to x2.36, best to x3.12 and ultra to x0.98 parity, as recorded in the [matrix](matrix.md) streaming table.

Decode MT is our exclusive dimension because libzstd has no MT decode. Json mt16 runs at 1.65 times our single-thread and 1.23 times the zstd stream reference, with mt2 at 1.38 times single-thread. Dll100.zst3 mt16 runs at 1.90 times single-thread and 1.36 times the reference, the strongest scaling row and a real-binary payload. Skewed mt16 reads 1.25 times single-thread and 0.96 times the reference, with its absolute band wandering ±15% across passes, and text is flat at 0.97.

The ratio sweep reads a geo-mean of +8.69% denser over 120 cells, with bulk single-thread at +1.48%, and outputs are byte-identical to the 2026-10-01 sweep. The losing cells are unchanged: json.fast MT at -0.12% to -0.14%, skewed.opt at -0.06% to -0.07%, and near-ties of 0.00% to 0.04% on skewed fast, fastest and ultra and on text.ultra MT.

The full 1-22 ladder keeps json rows 5-8 and 10-12 at x1.23 to 1.31 carrying -4.8% to -16.4% byte wins. The reach-probe parse lifts the chain band, taking text level 9 from x1.34 to 1.11 and json level 9 from 0.83 to 0.76. Skewed rows 17-22 hold x0.77 to 0.96 and json rows 19-22 stay at or below 1.01. Text chain rows hold their wins at x0.42 to 0.62 for levels 5-8 and 0.36 to 0.57 for 10-12. Zeros levels 1-4 read 0.27, 0.27, 0.18 and 0.18, and random levels 1-4 read 0.87 to 0.92. The remaining holes are text level 9 at x1.11 and skewed levels 13-16 at x1.17 to 2.27 at ratio parity.

Small payloads read json level 9 at x1.11, 0.90, 0.64 and 0.99 for 1K, 4K, 64K and 1M at stock-row bytes, and text level 9 keeps the swap's ratio at x1.73, 1.49, 2.46 and 1.00. Level 3 at 64K improved to 0.83 and 0.90. Level 1 at 1.24 to 1.78 and level 3 at 0.83 to 1.34 remain the accepted trades.

## Decode single-thread

Throughput is MiB/s of raw data for bulk decode and streaming decode with 64 KiB pulls. x is our time divided by zstd's time, so values below 1 mean we are faster.

| file | bulk ours | bulk zstd | bulk x | stream ours | stream zstd | stream x |
|---|---:|---:|---:|---:|---:|---:|
| json.zst1 | 1733 | 1227 | 0.71 | 1778 | 2186 | 1.23 |
| json.zst3 | 1434 | 1109 | 0.77 | 1415 | 1874 | 1.33 |
| json.zst9 | 1724 | 1184 | 0.69 | 1677 | 2146 | 1.28 |
| json.zst19 | 2158 | 1269 | 0.59 | 2226 | 2510 | 1.13 |
| text.zst1 | 5948 | 2126 | 0.36 | 6284 | 7554 | 1.20 |
| text.zst3 | 8612 | 2338 | 0.27 | 10449 | 11094 | 1.06 |
| text.zst9 | 9286 | 2387 | 0.26 | 11612 | 12053 | 1.04 |
| text.zst19 | 9331 | 2388 | 0.26 | 11607 | 12128 | 1.05 |
| skewed.zst1 | 2334 | 1467 | 0.63 | 2632 | 2843 | 1.08 |
| skewed.zst3 | 1271 | 1016 | 0.80 | 1292 | 1547 | 1.20 |
| skewed.zst9 | 643 | 642 | 1.00 | 621 | 797 | 1.28 |
| skewed.zst19 | 2183 | 1410 | 0.65 | 2461 | 2697 | 1.10 |
| random.zst3 | 8796 | 2181 | 0.25 | 11068 | 8888 | 0.80 |
| zeros.zst3 | 11690 | 2449 | 0.21 | 12865 | 12874 | 1.00 |
| dll100.zst1 | 1157 | 973 | 0.84 | 1183 | 1514 | 1.28 |
| dll100.zst3 | 1236 | 1011 | 0.82 | 1280 | 1709 | 1.34 |
| dll100.zst9 | 1536 | 1137 | 0.74 | 1575 | 2081 | 1.32 |
| dll100.zst19 | 1326 | 1048 | 0.79 | 1379 | 1819 | 1.32 |

## Encode single-thread bulk

Throughput and ratio are per shape, with checksums off on both sides. The tier column names the ladder tier and the pairing is 1, 3, 9, 13, 17 and 19 against libzstd. x is our time divided by zstd's, so values below 1 mean we are faster.

| level | shape | ours MiB/s | ours ratio | zstd MiB/s | zstd ratio | x |
|---|---|---:|---:|---:|---:|---:|
| fastest | json | 590 | 6.39 | 852 | 6.11 | 1.45 |
| fastest | text | 13231 | 309.23 | 10494 | 308.94 | 0.79 |
| fastest | skewed | 2832 | 2.00 | 1253 | 2.00 | 0.44 |
| fastest | random | 2286 | 1.00 | 2052 | 1.00 | 0.90 |
| fastest | zeros | 49317 | 32577 | 13305 | 32171 | 0.27 |
| fastest | dll | 501 | 4.35 | 529 | 2.19 | 1.06 |
| fast | json | 456 | 5.30 | 472 | 5.29 | 1.03 |
| fast | text | 13512 | 333.03 | 7264 | 332.90 | 0.54 |
| fast | skewed | 211 | 1.92 | 225 | 1.92 | 1.07 |
| fast | random | 2283 | 1.00 | 1911 | 1.00 | 0.84 |
| fast | zeros | 49217 | 32577 | 8709 | 32171 | 0.18 |
| fast | dll | 439 | 4.97 | 424 | 3.40 | 0.97 |
| balanced | json | 157 | 7.20 | 121 | 5.95 | 0.77 |
| balanced | text | 1580 | 384.66 | 1732 | 378.41 | 1.10 |
| balanced | skewed | 1839 | 2.00 | 75 | 1.84 | 0.041 |
| balanced | random | 2280 | 1.00 | 1471 | 1.00 | 0.65 |
| balanced | zeros | 32822 | 32577 | 1750 | 32202 | 0.053 |
| balanced | dll | 116 | 5.31 | 150 | 4.62 | 1.30 |
| best | json | 30 | 6.26 | 37 | 6.10 | 1.23 |
| best | text | 1141 | 386.42 | 692 | 385.90 | 0.61 |
| best | skewed | 7 | 1.84 | 14 | 1.84 | 1.91 |
| best | random | 2190 | 1.00 | 251 | 1.00 | 0.11 |
| best | zeros | 42873 | 32577 | 816 | 32171 | 0.019 |
| best | dll | 31 | 5.35 | 39 | 4.67 | 1.26 |
| opt | json | 7 | 7.49 | 7 | 7.49 | 1.01 |
| opt | text | 816 | 410.23 | 452 | 410.11 | 0.55 |
| opt | skewed | 3 | 2.00 | 3 | 2.00 | 0.92 |
| opt | random | 2217 | 1.00 | 12 | 1.00 | 0.005 |
| opt | zeros | 41220 | 32577 | 900 | 32202 | 0.022 |
| opt | dll | 13 | 6.04 | 14 | 5.06 | 1.09 |
| ultra | json | 3 | 7.42 | 3 | 7.42 | 1.00 |
| ultra | text | 402 | 414.12 | 258 | 413.98 | 0.64 |
| ultra | skewed | 2 | 2.00 | 2 | 2.00 | 0.95 |
| ultra | random | 2221 | 1.00 | 8 | 1.00 | 0.003 |
| ultra | zeros | 39748 | 32577 | 634 | 32202 | 0.016 |
| ultra | dll | 8 | 6.32 | 9 | 5.28 | 1.12 |

The ratio verdict is unchanged from the previous sweep because outputs are byte-identical. We are denser or at parity on json and text at every level, with json.opt 9 bytes ahead, skewed wins everywhere except near-ties, random and zeros tie or win, and dll is denser at every tier: -49.6% bytes at fastest, -31.7% at fast and double digits from balanced up. Checksums on versus off on our side read json.fast 1.00 and text.fast 0.86.

## Encode multi-thread bulk

Throughput is MiB/s at 8 and 16 workers with one pass, using a cold pool per call on both sides. The x8 and x16 columns are our time divided by zstd's at the same worker count, and the ratio columns are at 16 workers.

| cell | ours mt8 | zstd mt8 | x8 | ours mt16 | zstd mt16 | x16 | ratio ours mt16 | ratio zstd mt16 |
|---|---:|---:|---:|---:|---:|---:|---:|---:|
| json.fastest | 3443 | 4046 | 1.19 | 5175 | 1758 | 0.34 | 6.34 | 6.11 |
| json.fast | 2375 | 990 | 0.42 | 3307 | 1004 | 0.30 | 5.30 | 5.31 |
| json.balanced | 427 | 201 | 0.48 | 548 | 202 | 0.38 | 7.19 | 5.95 |
| text.fastest | 19538 | 10572 | 0.55 | 17237 | 2228 | 0.13 | 309.08 | 39.78 |
| text.fast | 11700 | 2144 | 0.18 | 12020 | 2127 | 0.18 | 332.94 | 189.16 |
| text.balanced | 1116 | 1223 | 1.08 | 1144 | 1216 | 1.09 | 384.59 | 378.30 |
| skewed.fastest | 4072 | 3216 | 0.79 | 3610 | 1527 | 0.42 | 2.00 | 2.00 |
| skewed.fast | 1193 | 631 | 0.54 | 1633 | 635 | 0.39 | 1.92 | 1.92 |
| skewed.balanced | 681 | 120 | 0.18 | 750 | 122 | 0.16 | 2.00 | 1.84 |

## Streaming encode

Throughput uses 64 KiB pulls and interleaved medians, with single-thread and MT8 columns. x is our time divided by zstd's at the same mode, so values below 1 mean we are faster.

| cell | ST ours | ST zstd | ST x | MT8 ours | MT8 zstd | MT8 x |
|---|---:|---:|---:|---:|---:|---:|
| json.fastest | 588 | 776 | 1.32 | 3265 | 1877 | 0.57 |
| json.fast | 467 | 439 | 0.94 | 2273 | 394 | 0.17 |
| json.balanced | 87 | 106 | 1.22 | 389 | 113 | 0.29 |
| json.best | 30 | 35 | 1.16 | 82 | 51 | 0.62 |
| json.opt | 7 | 7 | 0.97 | 17 | 7 | 0.40 |
| json.ultra | 3 | 3 | 0.98 | 7 | 3 | 0.47 |
| text.fastest | 6960* | 1761 | 0.25 | 7575 | 4131 | 0.55 |
| text.fast | 6695* | 5260 | 0.79 | 6306 | 1676 | 0.27 |
| text.balanced | 1427 | 1612 | 1.13 | 1002 | 947 | 0.95 |
| text.best | 1026 | 646 | 0.63 | 853 | 351 | 0.41 |
| text.opt | 750 | 414 | 0.55 | 669 | 371 | 0.55 |
| text.ultra | 389 | 247 | 0.63 | 366 | 236 | 0.64 |

The two text single-thread fast cells carry standalone medians under the established protocol, because the in-pass table read 5601 and 5502 at x0.310 and 0.909 and the same-process page-dispersity collapse reproduced in four consecutive full passes. The mechanism and reproducer are in the [workflow pitfalls](../pitfalls/workflow.md). Unknown-size text.fastest streaming has zstd emitting 1.84 MB at ratio 18.3 against our 108 KB at ratio 309. Dll100 stream cells are in the [matrix](matrix.md) streaming table.

## Decode multi-thread scaling

Fixtures are the same files as the single-thread table, and throughput is MiB/s at 1, 2, 4, 8 and 16 workers. The last column is zstd's single-thread streaming decode, because libzstd has no MT decode, so the scaling columns have no zstd counterpart.

| file | ours ST | mt2 | mt4 | mt8 | mt16 | zstd ST stream ref |
|---|---:|---:|---:|---:|---:|---:|
| json.zst3 | 1399 | 1927 | 2117 | 2276 | 2308 | 1875 |
| text.zst3 | 11132 | 10832 | 10852 | 10843 | 10818 | 11179 |
| skewed.zst9 | 608 | 783 | 756 | 782 | 759 | 788 |
| random.zst3 | 9678 | 9458 | 9479 | 9476 | 9471 | 8928 |
| dll100.zst3 | 1225 | 1872 | 2102 | 2241 | 2322 | 1709 |

Dll100 is measured through `files --threads`. The restart-point decode scales best on the large real-binary payload at mt16 1.90 times single-thread and 1.36 times the reference, up from 1.83 and 1.31. Json mt16 reads 1.65 times single-thread and 1.23 times the reference, up from 1.60 and 1.18. Skewed holds 1.25 times single-thread at 0.96 times the reference, and text is flat at 0.97.

## Compression-ratio sweep

The sweep runs `zstdx-bench ratio` on 5 shapes, 6 tiers, bulk and stream modes and single-thread and 4-worker MT, one deterministic pass per cell with checksums off. The reported delta is ours over zstd's ratio minus 1, aggregated as a geo-mean, and every cell is roundtrip-gated. The geo-mean over 120 cells is **+8.69%**. By mode it reads bulk single-thread +1.48%, bulk MT +11.08%, stream single-thread +11.43% and stream MT +11.11%. By shape it reads json +4.41%, text +40.36%, skewed +1.41%, random plus or minus zero and zeros +2.06%. Outputs are byte-identical to the 2026-10-01 sweep. The complete losing cells are json.fast MT at -0.12% to -0.14%, text.ultra MT at -0.02%, skewed.opt at -0.06% to -0.07%, skewed.ultra at -0.04%, skewed.fast MT at -0.01% to -0.03% and skewed.fastest at -0.00%. Details are in the [matrix](matrix.md) ratio section.

## Open and closed deficits

Each row names a measured deficit and its status in this pass. The x figures are wall-time ratios, so values below 1 mean we are faster.

| Deficit | Status in this pass |
|---|---|
| Small-payload level 9 | Closed on 2026-09-28. The flip was the small-src btlazy swap, now an 8 KiB head verdict with a 48-distinct bar. Json level 9 reads x1.11, 0.90, 0.64 and 0.99 at 1K, 4K, 64K and 1M on the stock row. Text keeps the swap at x1.73, 1.49, 2.46 and 1.00 carrying -0.9% against zstd-9 at 64K. The level 1 slide at 1K-4K remains the accepted trade |
| Json chain rows 5-8 and 10-12 | Closed on 2026-10-01 with the head-gated attempts diet. The rows read x1.23 to 1.31 at -4.8% to -16.4% bytes against zstd while every other corpus stays byte-identical, with the mechanism and `RowAttempts::Light` in [matchers](../perf/matchers.md). This pass holds those cells |
| text.balanced speed | Single-thread moved from x1.34 to 1.10 after the concurrent probe parse on 2026-10-17 through `measure_shrink_concurrent`, and bulk-mt8 reads 1.08 while stream-mt8 reads 0.95 and is a win. The residual cold-start DUBT head cost stands, and ratio is +1.65% denser than zstd-9 |
| Streaming decode on compressible shapes | Json reads x1.13 to 1.33, skewed 1.08 to 1.28, text 1.04 to 1.20 and dll 1.28 to 1.34. The fused-loop serial chain remains, closed as a measured op-volume floor in [dec-gap](dec-gap.md) |
| json.fastest | x1.45 single-thread and 1.19 mt8, inside the cells' known spreads. The steady scan loop's branch-mispredict budget sets a realistic ceiling of about x1.3 to 1.4. Json.fast at 1.03 is near closed |
| Best-tier core | Json 1.23 and skewed 1.91, a memory-latency tree walk with its floor in the [todo](../todo.md). It caps the json.best stream single-thread cell at x1.16 |
| json opt and ultra | Closed on 2026-10-01 with hugepage-backed buffers. This pass reads x1.01 and 1.00 at ratio parity, with json.opt 9 bytes ahead, and skewed opt and ultra ahead at 0.92 and 0.95 |
| MT decode scaling | Json mt16 reads 1.65 times single-thread and 1.23 times the reference, dll100 1.90 and 1.36, skewed 1.25 and 0.96, and text is flat at 0.97. The memory-bound verdict and execute walls stand, with no lever short of overlapping execute with the verdict, a falsified class |
| dll100 balanced | x1.30 at -13.0% size, with best at 1.26 against 1.42 before. The chain chase remains, with its floor in the [todo](../todo.md) and its differential attribution in [enc-dll-gap](enc-dll-gap.md). Fastest and fast are speed near-parity carrying -49.6% and -31.7% size |
| text single-thread stream fastest and fast | The in-pass page-dispersity collapse reproduced for the fourth consecutive pass at 5601 and 5502 in-pass against 6960 and 6695 standalone, at x0.25 and 0.79. It was root-caused on 2026-09-29 in the [workflow pitfalls](../pitfalls/workflow.md), where a best or L13 row preceding the pooled single-thread core scatters the big buffers' pages and THP recovers about 11%, leaving the residual unattributed. The real residue against 2026-09-19 is the unpledged-stream screen's about 4% sampling tax. Both cells still win |
| Fast-row screen tax on compressible-flat data | Random fastest and fast read x0.90 and 0.84 from the shortened bar leg plus the 1 MiB twin-scan reject, a deliberate arming-recall bound. Zeros fastest and fast read 0.27 and 0.18, and skewed chain rows read at or below 0.06. Every cell still wins |
| Ratio residues | Json.fast MT at -0.12% to -0.14%, skewed.opt at -0.07% as a near-tie, and the dictionary small-payload parse on the encoding side |
| dll100 streaming-MT encode | Stream-mt8 trails libzstd-mt8 at four of six tiers: fastest x3.52, balanced 1.15, ultra 1.21 and best 5.39, while running at 52% to 93% of its own bulk-mt8 ceilings. The libzstd-MT losses are the open item in the [todo](../todo.md). The buffer-reserve fix on 2026-10-22 through `reserve_decided` lifted the cells to 82% to 93% of their bulk ceilings at byte-identical sizes: fastest 587 to 897 MiB/s at x2.36, best 16 to 27 at x3.12, ultra 15 to 18 at x0.98 parity and balanced 157 to 161. The remaining fastest and best losses are measured-out classes: per-job whole-window LDM cold fills plus dll's content spread, while zstd's 2154 MiB/s headline buys 48 MB of output against our 24 MB, so the two run at different output sizes |
