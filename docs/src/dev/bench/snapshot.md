# Performance Snapshot (2026-11-01)

This page summarizes the 2026-11-01 R86 fast pass at commit `9fe6a721`, run on one machine, one corpus and the same flags on both sides under the shared lock, driven by `bash bench/pass.sh all` through one release build at a total wall of 32.3 minutes. The comparison target is the zstd crate and libzstd 1.5.7 with zstdmt, and the ladder pairs fastest, fast, balanced, best, opt and ultra against libzstd levels 1, 3, 9, 13, 17 and 19. Raw tables, provenance and per-chunk commands are in [matrix.md](matrix.md), including the full numeric 1-22 ladder and the small-payload table. Throughout this page x is our time divided by the reference time, so values below 1 mean we are faster, and speeds are MiB/s of raw input.

The pass covers the encode window since the 2026-10-31 refresh, and two movers shape the new numbers. The flat price-table mirrors `0c53c87b` with the opt finder's head-slot prefetch `b4d90620` take the deep rows to wins: json.opt at x0.985, json.ultra at 1.006, dll100.opt at 0.919, the first deep-tier dll win, and dll100.ultra at 1.032, with the stream cells following at dll100.opt.stream 0.872 and stream-mt8 0.667; the prefetch's locked A/B numbers are in [matchers](../perf/matchers.md). The btlazy driver rep0 skip `a6baa167` moves json.best to 1.229, dll100.best to 1.201 and json.best.stream-mt8 to 0.642. The dec-st bulk cell carries the reference-side buffer-reuse fix `7817e052`, so the decode headline changes meaning this pass: the earlier bulk wins partly measured the reference's per-call output allocation, and bulk now agrees with the streaming column.

Caveats for reading every table: the pass runs the R86 harness, whose enc-st and enc-stream single-thread cells share the machine in pairs, so absolute MiB/s on those cells reads below the serial tables of earlier passes and cross-pass comparisons use x; a verdict on a 10 GB/s-class cell needs a `--parallel 1` rerun per the [methodology](methodology.md). Noise runs ±10% between sessions, every cell carries interleaved medians at the default per-side budget of 500 ms, and spreads sit in the raw tables.

## Headline results

Decode single-thread reads uniformly x1.00 to 1.38 on both the bulk and streaming paths across the compressible shapes, the first pass where the two columns agree: json spans 1.17 to 1.38, text 0.99 to 1.21, skewed 1.08 to 1.36 and dll 1.25 to 1.38, with json.zst3 at bulk 1.38 against stream 1.31, text.zst3 bulk at 1.00 and dll100.zst3 bulk at 1.38. Random wins both paths at 0.99 and 0.80, zeros reads 0.92 bulk against 1.00 stream. The remaining gap is the fused decode loop's op volume, measured in [dec-gap](dec-gap.md) and [mzstd-dec](../review/mzstd-dec.md).

Encode single-thread on json reads fastest 1.36, fast 1.08, balanced 0.76 with +21% ratio, best 1.23, and opt and ultra at 0.985 and 1.006 at ratio parity, with json.opt 9 bytes ahead of zstd-17. Text reads fastest 0.85, fast 0.58, balanced 0.94 at +1.65% ratio, best 0.66, opt 0.49 and ultra 0.57. Skewed reads fastest 0.46, fast 1.05, balanced 0.040, best 1.58, and opt and ultra 0.80 and 0.95 at ratio parity.

Random and zeros are ours at every tier, up to 300 times on incompressible high tiers. Zeros fastest and fast read 0.28 and 0.18, and random 0.91 and 0.91. On dll, fastest reads 1.13 at -49.6% size with ratio 4.35 against zstd-1's 2.19, fast 1.02 at -31.7%, balanced 1.17 at -14.0%, best 1.20 at -12.8% on the rep0 skip, opt 0.919 at -16.2% and ultra 1.032 at -16.5% on the price-table movers.

Encode multi-thread has one remaining zstd win: json.fastest.mt8 at x1.12 inside its known noise band of 0.95 to 1.65, while its mt16 cell wins at 0.31. Every other cell is ahead, with text.balanced at 0.86 and 0.89, and our cold-pool mt16 json.fast at 3313 MiB/s still beats zstd's warm-pool reference at 1596.

Streaming encode single-thread reads json.fast 0.96 and json.fastest 1.24, text.best 0.64, and text.fastest and fast 0.34 and 0.95 as in-pass readings, because the same-process page-dispersity collapse reproduced for the eighth consecutive pass. Json.opt.stream reads 0.94 through the price-table movers.

Streaming encode MT8 keeps every json cell at or below x0.64. Every text cell wins, with balanced at 0.78. Dll100 stream-mt8 leads at fast 0.52, opt 0.67 and ultra 0.90, and trails at fastest 2.14, balanced 1.09 and best 3.16, running at 61% to 91% of its own bulk-mt8 ceilings. The trailing cells stay the open item in the [todo](../todo.md).

Decode MT carries json mt16 at 1.55 times our single-thread and 1.18 times the zstd stream reference, and dll100.zst3 mt16 at 1.78 times single-thread and 1.32 times the reference, the strongest scaling row on a real-binary payload. Skewed reads 1.16 times single-thread and 0.95 times the reference, with its absolute band wandering ±15% across passes, and text is flat at 0.98.

The ratio sweep reproduced the 2026-10-30 sizes byte for byte at a geo-mean of +8.70% denser over 120 cells, with the losing set unchanged. The dump belt read byte-identical in both directions.

The full 1-22 ladder reads deep json at or under parity for the first time, levels 17 through 22 at x0.52 to 1.15 with level 19 at 0.99, carries json rows 5-8 and 10-12 at 1.12 to 1.33 with their byte wins, text chain rows at 0.33 to 0.72 with level 9 at 0.95, zeros levels 1-4 at 0.29, 0.29, 0.19 and 0.19, random levels 17-19 at 2109 to 3392 MiB/s at byte-exact ratio under the asymmetric-cell cap, and the skewed hole at levels 13-16 narrowed to 1.12 to 1.59. Small payloads read json level 9 at x1.09, 0.93, 0.66 and 1.01.

## Decode single-thread

Throughput is MiB/s of raw data for bulk decode and streaming decode with 64 KiB pulls. x is our time divided by zstd's time, so below 1 means we are faster.

| file | bulk ours | bulk zstd | bulk x | stream ours | stream zstd | stream x |
|---|---:|---:|---:|---:|---:|---:|
| json.zst1 | 1776 | 2264 | 1.28 | 1785 | 2189 | 1.23 |
| json.zst3 | 1476 | 2039 | 1.38 | 1425 | 1873 | 1.31 |
| json.zst9 | 1769 | 2354 | 1.33 | 1680 | 2141 | 1.27 |
| json.zst19 | 2227 | 2630 | 1.18 | 2240 | 2630 | 1.17 |
| text.zst1 | 6234 | 6715 | 1.08 | 6260 | 7559 | 1.21 |
| text.zst3 | 9169 | 9178 | 1.00 | 10393 | 11115 | 1.07 |
| text.zst9 | 9914 | 9791 | 0.99 | 11559 | 12097 | 1.05 |
| text.zst19 | 9914 | 9811 | 0.99 | 11614 | 12180 | 1.05 |
| skewed.zst1 | 2326 | 3067 | 1.32 | 2637 | 2845 | 1.08 |
| skewed.zst3 | 1276 | 1639 | 1.28 | 1299 | 1533 | 1.18 |
| skewed.zst9 | 656 | 889 | 1.36 | 616 | 789 | 1.28 |
| skewed.zst19 | 2227 | 2976 | 1.34 | 2455 | 2697 | 1.10 |
| random.zst3 | 8989 | 8872 | 0.99 | 11032 | 8797 | 0.80 |
| zeros.zst3 | 12213 | 11152 | 0.92 | 12869 | 12902 | 1.00 |
| dll100.zst1 | 1183 | 1572 | 1.33 | 1202 | 1507 | 1.25 |
| dll100.zst3 | 1263 | 1746 | 1.38 | 1297 | 1708 | 1.32 |
| dll100.zst9 | 1571 | 2137 | 1.36 | 1594 | 2071 | 1.30 |
| dll100.zst19 | 1364 | 1833 | 1.34 | 1390 | 1806 | 1.30 |

With the reference-side buffer reuse in place the bulk column agrees with the streaming column for the first time, and the earlier tables' 17-of-18 bulk wins traced to the reference's per-call output allocation. Text bulk sits at parity, random wins both paths, and the residual gap on both paths is the fused loop's op volume, closed as a measured floor in [dec-gap](dec-gap.md) and [mzstd-dec](../review/mzstd-dec.md).

## Encode single-thread bulk

Throughput and ratio are per shape, with checksums off on both sides. The tier column names the ladder tier and the pairing is 1, 3, 9, 13, 17 and 19 against libzstd. x is our time divided by zstd's, so below 1 means we are faster. These cells share the machine in pairs, so x is the comparable column.

| level | shape | ours MiB/s | ours ratio | zstd MiB/s | zstd ratio | x |
|---|---|---:|---:|---:|---:|---:|
| fastest | json | 615 | 6.41 | 835 | 6.11 | 1.36 |
| fastest | text | 11749 | 309.23 | 9872 | 308.94 | 0.85 |
| fastest | skewed | 2588 | 2.00 | 1200 | 2.00 | 0.46 |
| fastest | random | 2008 | 1.00 | 1821 | 1.00 | 0.91 |
| fastest | zeros | 47918 | 32577 | 13202 | 32171 | 0.28 |
| fastest | dll | 472 | 4.35 | 526 | 2.19 | 1.13 |
| fast | json | 434 | 5.30 | 469 | 5.29 | 1.08 |
| fast | text | 12345 | 333.03 | 7130 | 332.90 | 0.58 |
| fast | skewed | 212 | 1.92 | 223 | 1.92 | 1.05 |
| fast | random | 2043 | 1.00 | 1833 | 1.00 | 0.91 |
| fast | zeros | 47955 | 32577 | 8671 | 32171 | 0.18 |
| fast | dll | 420 | 4.97 | 427 | 3.40 | 1.02 |
| balanced | json | 150 | 7.20 | 114 | 5.95 | 0.76 |
| balanced | text | 1831 | 384.66 | 1715 | 378.41 | 0.94 |
| balanced | skewed | 1824 | 2.00 | 73 | 1.84 | 0.040 |
| balanced | random | 2163 | 1.00 | 1360 | 1.00 | 0.63 |
| balanced | zeros | 32638 | 32577 | 1743 | 32202 | 0.053 |
| balanced | dll | 123 | 5.37 | 144 | 4.62 | 1.17 |
| best | json | 24 | 6.26 | 31 | 6.10 | 1.23 |
| best | text | 972 | 386.42 | 641 | 385.90 | 0.66 |
| best | skewed | 5 | 1.84 | 8 | 1.84 | 1.58 |
| best | random | 1927 | 1.00 | 118 | 1.00 | 0.061 |
| best | zeros | 43448 | 32577 | 765 | 32171 | 0.018 |
| best | dll | 26 | 5.35 | 32 | 4.67 | 1.20 |
| opt | json | 6 | 7.49 | 6 | 7.49 | 0.985 |
| opt | text | 851 | 410.23 | 416 | 410.11 | 0.49 |
| opt | skewed | 2 | 2.00 | 2 | 2.00 | 0.80 |
| opt | random | 2140 | 1.00 | 9 | 1.00 | 0.004 |
| opt | zeros | 36495 | 32577 | 808 | 32202 | 0.022 |
| opt | dll | 13 | 6.04 | 12 | 5.06 | 0.919 |
| ultra | json | 3 | 7.42 | 3 | 7.42 | 1.01 |
| ultra | text | 421 | 414.12 | 242 | 413.98 | 0.57 |
| ultra | skewed | 2 | 2.00 | 2 | 2.00 | 0.95 |
| ultra | random | 2142 | 1.00 | 6 | 1.00 | 0.003 |
| ultra | zeros | 42940 | 32577 | 585 | 32202 | 0.013 |
| ultra | dll | 8 | 6.32 | 9 | 5.28 | 1.03 |

The window's movers sit in the deep rows: the price-table mirrors with the head-slot prefetch take json.opt to 0.985, json.ultra to 1.006 and dll100.opt to 0.919, the first deep-tier dll win, and the rep0 skip takes json.best to 1.229 and dll100.best to 1.201. We are denser or at parity on json and text at every level, with json.opt 9 bytes ahead, skewed wins everywhere except the chain-row near-tie, random and zeros tie or win, and dll is denser at every tier, double digits from balanced up. The dll fast-class cells read 1.13, 1.02 and 1.17 against the serial band's 1.04, 0.97 and 1.12, the pair-sharing tax. Checksums on versus off on our side read json.fast 1.03 and text.fast 0.85.

## Encode multi-thread bulk

Throughput is MiB/s at 8 and 16 workers with one pass, using a cold pool per call on both sides. The x8 and x16 columns are our time divided by zstd's at the same worker count, and the ratio columns are at 16 workers.

| cell | ours mt8 | zstd mt8 | x8 | ours mt16 | zstd mt16 | x16 | ratio ours mt16 | ratio zstd mt16 |
|---|---:|---:|---:|---:|---:|---:|---:|---:|
| json.fastest | 3592 | 4031 | 1.12 | 5258 | 1661 | 0.31 | 6.35 | 6.11 |
| json.fast | 2482 | 970 | 0.40 | 3313 | 984 | 0.30 | 5.30 | 5.31 |
| json.balanced | 447 | 196 | 0.44 | 540 | 197 | 0.37 | 7.19 | 5.95 |
| text.fastest | 18555 | 10711 | 0.57 | 16988 | 2069 | 0.12 | 309.08 | 39.78 |
| text.fast | 11308 | 1998 | 0.17 | 10982 | 1984 | 0.18 | 332.94 | 189.16 |
| text.balanced | 1335 | 1157 | 0.86 | 1312 | 1173 | 0.89 | 384.59 | 378.30 |
| skewed.fastest | 3962 | 3143 | 0.79 | 3507 | 1456 | 0.42 | 2.00 | 2.00 |
| skewed.fast | 1152 | 614 | 0.54 | 1600 | 621 | 0.38 | 1.92 | 1.92 |
| skewed.balanced | 629 | 119 | 0.18 | 678 | 119 | 0.17 | 2.00 | 1.84 |

Json.fastest.mt8 at x1.12 is the one zstd MT win, inside the cell's known noise band of 0.95 to 1.65 across passes, and its mt16 cell wins at 0.31. Our cold-pool mt16 json.fast at 3313 MiB/s still beats zstd's warm-pool reference at 1596 this run.

## Streaming encode

Throughput uses 64 KiB pulls and interleaved medians, with single-thread and MT8 columns. x is our time divided by zstd's at the same mode, so below 1 means we are faster. The single-thread cells share the machine in pairs, so x is the comparable column.

| cell | ST ours | ST zstd | ST x | MT8 ours | MT8 zstd | MT8 x |
|---|---:|---:|---:|---:|---:|---:|
| json.fastest | 610 | 754 | 1.24 | 3320 | 1937 | 0.60 |
| json.fast | 458 | 438 | 0.96 | 2210 | 396 | 0.18 |
| json.balanced | 80 | 101 | 1.25 | 385 | 112 | 0.29 |
| json.best | 21 | 24 | 1.15 | 79 | 50 | 0.64 |
| json.opt | 5 | 5 | 0.94 | 18 | 7 | 0.38 |
| json.ultra | 3 | 3 | 1.01 | 6 | 3 | 0.48 |
| text.fastest | 5067* | 1699 | 0.34 | 7725 | 3961 | 0.53 |
| text.fast | 5076* | 4821 | 0.95 | 5855 | 1563 | 0.27 |
| text.balanced | 1367 | 1612 | 1.18 | 1212 | 941 | 0.78 |
| text.best | 979 | 628 | 0.64 | 890 | 345 | 0.39 |
| text.opt | 792 | 400 | 0.51 | 746 | 356 | 0.48 |
| text.ultra | 410 | 240 | 0.59 | 396 | 223 | 0.56 |

The two text single-thread fast cells carry in-pass readings at 5067 and 5076 MiB/s, and the same-process page-dispersity collapse reproduced for the eighth consecutive pass, with the mechanism and reproducer in the [workflow pitfalls](../pitfalls/workflow.md); a verdict on either cell needs the standalone rerun protocol. Unknown-size text.fastest streaming has zstd emitting 1.84 MB at ratio 18.3 against our 108 KB at ratio 309. Dll100 stream cells are in the [matrix](matrix.md) streaming section.

## Decode multi-thread scaling

Fixtures are the same files as the single-thread table, and throughput is MiB/s at 1, 2, 4, 8 and 16 workers. The last column is zstd's single-thread streaming decode, because libzstd has no MT decode, so the scaling columns have no zstd counterpart.

| file | ours ST | mt2 | mt4 | mt8 | mt16 | zstd ST stream ref |
|---|---:|---:|---:|---:|---:|---:|
| json.zst3 | 1429 | 1856 | 2016 | 2189 | 2223 | 1877 |
| text.zst3 | 11075 | 10972 | 10883 | 10896 | 10878 | 11203 |
| skewed.zst9 | 647 | 756 | 755 | 758 | 752 | 788 |
| random.zst3 | 9715 | 9505 | 9499 | 9507 | 9506 | 9024 |
| dll100.zst3 | 1266 | 1839 | 2041 | 2204 | 2255 | 1708 |

Dll100 is measured through `files --threads`, with its reference taken from this pass's `dll100.zst3` streaming cell. The restart-point decode scales best on the large real-binary payload at mt16 1.78 times single-thread and 1.32 times the reference. Json mt16 reads 1.55 times single-thread and 1.18 times the reference, with mt8 and mt16 reading level. Skewed holds 1.16 times single-thread at 0.95 times the reference, and text is flat at 0.98.

## Compression-ratio sweep

The sweep runs `zstdx-bench ratio` on 5 shapes, 6 tiers, bulk and stream modes and single-thread and 4-worker MT, one deterministic pass per cell with checksums off. The reported delta is ours over zstd's ratio minus 1, aggregated as a geo-mean, and every cell is roundtrip-gated. The geo-mean over 120 cells is **+8.70%**, reproducing the 2026-10-30 sizes byte for byte. By mode it reads bulk single-thread +1.49%, bulk MT +11.09%, stream single-thread +11.44% and stream MT +11.12%. By shape it reads json +4.46%, text +40.36%, skewed +1.41%, random plus or minus zero and zeros +2.06%. The complete losing cells are unchanged: json.fast bulk MT at -0.14% and stream MT at -0.12%, text.ultra MT at -0.02%, skewed.opt at -0.06% to -0.07%, skewed.ultra at -0.04%, skewed.fast bulk MT at -0.01% and stream MT at -0.03%, and skewed.fastest at -0.00%. Details are in the [matrix](matrix.md) ratio section.

## Open and closed deficits

Each row names a measured deficit and its status in this pass. The x figures are wall-time ratios, so values below 1 mean we are faster.

| Deficit | Status in this pass |
|---|---|
| Small-payload level 9 | Closed on 2026-09-28. The flip was the small-src btlazy swap, now an 8 KiB head verdict with a 48-distinct bar. The cells read json level 9 at x1.09, 0.93, 0.66 and 1.02 at 1K, 4K, 64K and 1M on the stock row, text at 1.77, 1.57, 2.86 and 1.02 on the swap row. The level 1 slide at 1K-4K remains the accepted trade |
| Json chain rows 5-8 and 10-12 | Closed on 2026-10-01 with the head-gated attempts diet. The rows carry the ladder at x1.12 to 1.33 and the byte wins against zstd, with the mechanism and `RowAttempts::Light` in [matchers](../perf/matchers.md) |
| text.balanced speed | Closed on 2026-10-30 by the balanced row substitution. Bulk reads x0.94 single-thread, 0.86 at mt8 and 0.89 at mt16, stream single-thread reads 1.18 and stream-mt8 0.78. Ratio holds +1.65% denser than zstd-9 |
| Streaming decode on compressible shapes | Bulk and stream now read the same band at x1.00 to 1.38, after the reference-side buffer-reuse fix put the bulk column on the reference's real speed. The fused-loop serial chain remains, closed as a measured op-volume floor in [dec-gap](dec-gap.md) and [mzstd-dec](../review/mzstd-dec.md) |
| json.fastest | x1.36 single-thread and 1.12 mt8, both inside the cells' known spreads. The steady scan loop's branch-mispredict budget sets a realistic ceiling of about x1.3 to 1.4, and the plain head block is the next named lever. Json.fast at 1.08 is near closed |
| Best-tier core | Json 1.23 and skewed 1.58 after the rep0 skip, a memory-latency tree walk with its floor in the [todo](../todo.md). It caps the json.best stream single-thread cell at x1.15, while json.best.stream-mt8 reads 0.64 |
| json opt and ultra | Closed on 2026-10-01 with hugepage-backed buffers, and this pass's price-table mirrors with the head-slot prefetch take them to x0.985 and 1.006 at ratio parity, with json.opt 9 bytes ahead. The same movers take dll100.opt to 0.919 and dll100.ultra to 1.032, and skewed opt and ultra ahead at 0.80 and 0.95 |
| MT decode scaling | Json mt16 at 1.55 times single-thread and 1.18 times the reference, dll100 1.78 and 1.32, skewed 1.16 and 0.95, text flat at 0.98. The memory-bound verdict and execute walls stand, with no lever short of overlapping execute with the verdict, a falsified class |
| dll100 balanced | x1.17 at -14.0% size, with best at 1.20 on the rep0 skip and the deep rows ahead at 0.919 and 1.032. The chain chase remains, with its floor in the [todo](../todo.md) and its differential attribution in [enc-dll-gap](enc-dll-gap.md). Fastest and fast are speed near-parity at 1.13 and 1.02 carrying -49.6% and -31.7% size |
| text single-thread stream fastest and fast | The in-pass page-dispersity collapse reproduced for the eighth consecutive pass at 5067 and 5076 MiB/s in-pass, at x0.34 and 0.95. It was root-caused on 2026-09-29 in the [workflow pitfalls](../pitfalls/workflow.md), where a best or L13 row preceding the pooled single-thread core scatters the big buffers' pages and THP recovers about 11%, leaving the residual unattributed. Both cells still win |
| Fast-row screen tax on compressible-flat data | Random fastest and fast read x0.91 and 0.91 from the shortened bar leg plus the 1 MiB twin-scan reject, a deliberate arming-recall bound. Zeros fastest and fast read 0.28 and 0.18, and skewed chain rows read at or below 0.06. Every cell still wins |
| Ratio residues | Json.fast MT at -0.12% to -0.14%, skewed.opt at -0.07% as a near-tie, and the dictionary small-payload parse on the encoding side |
| dll100 streaming-MT encode | Stream-mt8 trails libzstd-mt8 at three of six tiers: fastest x2.14, balanced 1.09 and best 3.16, leads at fast 0.52, opt 0.67 and ultra 0.90. The cells run at 61% to 91% of their own bulk-mt8 ceilings of 1033, 978, 194, 44, 41 and 24 MiB/s, with best the lowest share at 61%. The libzstd-MT losses are the open item in the [todo](../todo.md), measured-out classes of per-job whole-window LDM cold fills plus dll's content spread, while zstd's 2019 MiB/s headline buys 48 MB of output against our 24 MB, so the two run at different output sizes |
| Placement-phase regressions | Closed class since 2026-10-25: the nbSeq emit fix shifted the dubt count loop onto a 64-byte line boundary for a 10% dll100.best loss at identical instruction counts. The R32 two-copy entry stub in `count_run` now pins the loop head at a phase of 31 or less, following `extend_match_aligned`. The full walk is in [matchers](../perf/matchers.md) |
