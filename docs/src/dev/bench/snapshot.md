# Performance Snapshot (2026-11-23)

This page summarizes the 2026-11-23 release pass for v0.7.1, run on one machine, one corpus and the same flags on both sides under the shared lock, driven by `bash bench/pass.sh all`. The comparison target is the zstd crate and libzstd 1.5.7 with zstdmt, and the ladder pairs fastest, fast, balanced, best, opt and ultra against libzstd levels 1, 3, 9, 13, 17 and 19. Raw tables, provenance and per-chunk commands are in [matrix.md](matrix.md), including the full numeric 1-22 ladder and the small-payload table. Throughout this page x is our time divided by the reference time, so values below 1 mean we are faster, and speeds are MiB/s of raw input.

The pass covers the four commits since the 2026-11-21 R110 pass and the v0.7.0 cut, three of them the opt-tier performance family this release carries: the tree walks' per-compare diet, the DP core's per-position diet, and the AVX-2 vectorized ultra variant scan, with the seed-local diet falsified on record. The release tree adds the dependency upgrade — the zstd crate 0.13.5 to 0.14.4 with the same bundled libzstd 1.5.7, plus clap 4.6 and gungraun 0.20 — the priceset call-site cfg that restores the no-std build broken since the AVX-2 commit, and the version bump. Every size the sweep and ladder cover reproduces R110 byte for byte on both sides, so the window's commits and the binding move changed no output byte.

Caveats for reading every table: the pass runs the R86 harness with the adaptive sample floor, whose enc-st and enc-stream single-thread cells share the machine in pairs, so absolute MiB/s on those cells reads below the serial tables of pre-R86 passes and cross-pass comparisons use x; a verdict on a 10 GB/s-class cell needs a `--parallel 1` rerun per the [methodology](methodology.md). Noise runs ±10% between sessions. The toolchain holds at rustc 1.102.0-nightly from R110. The zstd decode side keeps its standing drift 4 to 12 percent below its R104 absolutes on the json, skewed and dll100 cells across R110 and this pass while our side held. This session's enc-stream corpus pairing read its fast rows 21 to 30 percent below the dll-paired run with both sides depressed together, a pairing-level condition rather than the one-sided pooled-layout lottery, and the dll-paired run read every cell at its healthy level, so the streaming table's single-thread cells carry the dll-paired readings.

## Headline results

The opt-tier family is the release's story, and it reads through every section at byte-identical output. Bulk single-thread json.opt moves from R110's x0.91 to 0.82 and json.ultra from 1.09 to 0.97, text.opt from 0.48 to 0.45 and text.ultra from 0.61 to 0.50, dll100.opt from 0.93 to 0.83 and dll100.ultra from 1.05 to 0.93. The deep ladder rows gain the same shape, json level 17 from 0.90 to 0.76, level 19 from 1.09 to 0.91, level 22 from 0.63 to 0.50, and the text chain end from 0.58 to 0.50 at level 19 and 0.46 to 0.37 at level 22.

Decode single-thread reads x0.80 to 1.02 bulk and x0.91 to 1.04 stream on json, text and skewed, with skewed.zst9 at 0.80 the widest win and random.zst3 stream at 0.81. Dll100 holds its band at 0.93 to 1.01 bulk, stream 0.96 to 1.01, and the pull sweep reads 0.95 to 1.05 across 4 KiB to 1 MiB pulls.

Encode multi-thread keeps one zstd win, json.fastest.mt8 at x1.16 inside its known noise band of 0.95 to 1.65, while its mt16 cell wins at 0.33. Every other cell is ahead, text.balanced improves to 0.88 and 0.87 from R110's 0.97 and 0.96, and our cold-pool mt16 json.fast at 3326 MiB/s still beats zstd's warm-pool reference at 1625.

Streaming encode single-thread reads json.fast 0.94, json.opt 0.81 and json.ultra 0.93 on the dll-paired cells, text.best 0.62, text.opt 0.47 and text.ultra 0.51. The fast rows read healthy on the dll pairing at text.fastest 6749 MiB/s with x0.26 and text.fast 6490 at 0.79, while the corpus pairing read them at 5344 and 6879 with the zstd side down equally. The balanced row read healthy in both pairings, text at 1506 and 1490, closing R110's solo-median carry.

Streaming encode MT8 keeps every json cell at or below x0.67. Every text cell wins, with balanced at 0.79 improving from R110's 0.83. Dll100 stream-mt8 leads at fast 0.51, opt 0.46 and ultra 0.63, and trails at fastest 2.05, balanced 1.13 improving from 1.42, and best 1.57, running at 84 to 95 percent of its own bulk-mt8 ceilings.

Decode MT held its scaling: json mt16 at 1.87 times the zstd stream reference, dll100 at 1.92 improving from 1.85, skewed at 1.80 times single-thread and 2.49 times the reference at mt8 improving from 1.69 and 2.34, and the multi-frame payloads at 2.84 to 3.24 times single-thread at mt16.

The ratio sweep holds the +8.70% geo-mean over 120 cells, with all 120 cells reproducing the R110 sizes byte for byte and the zstd reference side byte-flat through the binding move. The losing set is unchanged from R110 at the same bytes.

The full 1-22 ladder holds its shape with the deep rows improved: json at levels 17 and 19 reads 0.76 and 0.91 at the cut's unchanged bytes, with level 22 at 0.50, json rows 5-8 and 10-12 at 1.18 to 1.34 with their byte wins, text chain rows at 0.31 to 0.67, zeros levels 1-4 at 0.18 to 0.29, random levels 17-19 at 2145 to 3434 MiB/s at byte-exact ratio, and the skewed hole at levels 13-16 at 1.12 to 1.68. Small payloads held their band at deterministic sizes, with json l3 4K at 1.17 and text l3 4K at 1.15.

## Decode single-thread

Throughput is MiB/s of raw data for bulk decode and streaming decode with 64 KiB pulls. x is our time divided by zstd's time, so below 1 means we are faster.

| file | bulk ours | bulk zstd | bulk x | stream ours | stream zstd | stream x |
|---|---:|---:|---:|---:|---:|---:|
| json.zst1 | 2279 | 2110 | 0.93 | 2150 | 2035 | 0.95 |
| json.zst3 | 2099 | 1843 | 0.88 | 1854 | 1703 | 0.92 |
| json.zst9 | 2445 | 2157 | 0.88 | 2129 | 1932 | 0.91 |
| json.zst19 | 2686 | 2512 | 0.94 | 2564 | 2488 | 0.97 |
| text.zst1 | 7474 | 6572 | 0.88 | 7419 | 7348 | 0.99 |
| text.zst3 | 9703 | 9090 | 0.94 | 10973 | 10989 | 1.00 |
| text.zst9 | 10188 | 9792 | 0.96 | 11818 | 12021 | 1.02 |
| text.zst19 | 10163 | 9747 | 0.96 | 11634 | 12088 | 1.04 |
| skewed.zst1 | 3175 | 3077 | 0.97 | 3064 | 2846 | 0.93 |
| skewed.zst3 | 1662 | 1568 | 0.94 | 1561 | 1475 | 0.94 |
| skewed.zst9 | 960 | 771 | 0.80 | 820 | 696 | 0.85 |
| skewed.zst19 | 2925 | 2973 | 1.02 | 2705 | 2678 | 0.99 |
| random.zst3 | 8967 | 8898 | 1.00 | 11020 | 8943 | 0.81 |
| zeros.zst3 | 12095 | 11303 | 0.93 | 12855 | 12875 | 1.00 |
| dll100.zst1 | 1489 | 1504 | 1.01 | 1435 | 1451 | 1.01 |
| dll100.zst3 | 1728 | 1641 | 0.95 | 1660 | 1602 | 0.97 |
| dll100.zst9 | 2111 | 2002 | 0.95 | 2023 | 1944 | 0.96 |
| dll100.zst19 | 1836 | 1715 | 0.93 | 1771 | 1696 | 0.96 |
| multi.zst3 | 3249 | 3009 | 0.93 | 3242 | 3027 | 0.93 |

Every corpus shape reads at or under parity bulk except skewed.zst19 at 1.02, and the stream column holds 0.91 to 1.04 with random's 0.81 the widest win. Dll100's bulk band reads 0.93 to 1.01 against R110's 0.93 to 1.01, so the packed-FSE-baseline gain held through the binding move, and the stream band tightens to 0.96 to 1.01. The pull-size sweep behind the README figure agrees, with dll100 streaming x cells of 0.95 to 1.05 across 4 KiB to 1 MiB pulls. The remaining FSE-build residue and the copy-loop lumps are in [dec-gap](dec-gap.md).

## Encode single-thread bulk

Throughput and ratio are per shape, with checksums off on both sides. The tier column names the ladder tier and the pairing is 1, 3, 9, 13, 17 and 19 against libzstd. x is our time divided by zstd's, so below 1 means we are faster. These cells share the machine in pairs, so x is the comparable column.

| level | shape | ours MiB/s | ours ratio | zstd MiB/s | zstd ratio | x |
|---|---:|---:|---:|---:|---:|---:|
| fastest | json | 632 | 6.41 | 844 | 6.11 | 1.34 |
| fastest | text | 12004 | 309.23 | 9945 | 308.94 | 0.83 |
| fastest | skewed | 2595 | 2.00 | 1203 | 2.00 | 0.46 |
| fastest | random | 2121 | 1.00 | 2009 | 1.00 | 0.94 |
| fastest | zeros | 47951 | 32577 | 13153 | 32171 | 0.27 |
| fastest | dll | 492 | 4.36 | 526 | 2.19 | 1.07 |
| fast | json | 446 | 5.30 | 469 | 5.29 | 1.05 |
| fast | text | 12386 | 333.03 | 7150 | 332.90 | 0.58 |
| fast | skewed | 208 | 1.92 | 223 | 1.92 | 1.07 |
| fast | random | 2206 | 1.00 | 1870 | 1.00 | 0.85 |
| fast | zeros | 47412 | 32577 | 8486 | 32171 | 0.18 |
| fast | dll | 429 | 4.98 | 425 | 3.40 | 0.99 |
| balanced | json | 149 | 7.20 | 115 | 5.95 | 0.78 |
| balanced | text | 1818 | 384.66 | 1629 | 378.41 | 0.89 |
| balanced | skewed | 1792 | 2.00 | 73 | 1.84 | 0.041 |
| balanced | random | 2309 | 1.00 | 1428 | 1.00 | 0.62 |
| balanced | zeros | 32813 | 32577 | 1737 | 32202 | 0.053 |
| balanced | dll | 139 | 5.36 | 140 | 4.62 | 1.01 |
| best | json | 21 | 6.26 | 30 | 6.10 | 1.41 |
| best | text | 994 | 386.42 | 583 | 385.90 | 0.59 |
| best | skewed | 5 | 1.84 | 8 | 1.84 | 1.53 |
| best | random | 2104 | 1.00 | 165 | 1.00 | 0.078 |
| best | zeros | 42735 | 32577 | 626 | 32171 | 0.015 |
| best | dll | 27 | 5.36 | 31 | 4.67 | 1.13 |
| opt | json | 7 | 7.48 | 6 | 7.49 | 0.82 |
| opt | text | 928 | 408.84 | 421 | 410.11 | 0.45 |
| opt | skewed | 3 | 2.00 | 2 | 2.00 | 0.70 |
| opt | random | 2151 | 1.00 | 9 | 1.00 | 0.004 |
| opt | zeros | 37615 | 32577 | 816 | 32202 | 0.022 |
| opt | dll | 14 | 6.03 | 11 | 5.06 | 0.83 |
| ultra | json | 3 | 7.42 | 3 | 7.42 | 0.97 |
| ultra | text | 492 | 414.12 | 248 | 413.98 | 0.50 |
| ultra | skewed | 2 | 2.00 | 2 | 2.00 | 0.91 |
| ultra | random | 2287 | 1.00 | 6 | 1.00 | 0.003 |
| ultra | zeros | 43790 | 32577 | 612 | 32202 | 0.014 |
| ultra | dll | 9 | 6.32 | 8 | 5.28 | 0.93 |

The balanced row reads healthy in-pass with no lottery carry this pass: json.balanced at x0.78 matching R110's solo median, text.balanced at 0.89 against R110's in-pass 1.19 with the solo median at 0.88, and text.fastest and text.fast back at their healthy band of 0.83 and 0.58. The opt and ultra rows carry the window's commits: json.opt 0.82 against R110's 0.91, json.ultra 0.97 against 1.09, text.opt 0.45 against 0.48, text.ultra 0.50 against 0.61, dll100.opt 0.83 against 0.93 and dll100.ultra 0.93 against 1.05, all at byte-identical output. We stay denser or at parity on json and text at every level, skewed wins everywhere except the best-tier wander, random and zeros tie or win, and dll is denser at every tier, dll100 balanced at 19,570,844 B against zstd's 22,701,602. Checksums on versus off on our side read json.fast 1.02 and text.fast 0.85.

## Encode multi-thread bulk

Throughput is MiB/s at 8 and 16 workers with one pass, using a cold pool per call on both sides. The x8 and x16 columns are our time divided by zstd's at the same worker count, and the ratio columns are at 16 workers.

| cell | ours mt8 | zstd mt8 | x8 | ours mt16 | zstd mt16 | x16 | ratio ours mt16 | ratio zstd mt16 |
|---|---:|---:|---:|---:|---:|---:|---:|---:|
| json.fastest | 3597 | 4006 | 1.16 | 5340 | 1737 | 0.33 | 6.35 | 6.11 |
| json.fast | 2457 | 989 | 0.40 | 3326 | 999 | 0.30 | 5.30 | 5.31 |
| json.balanced | 411 | 200 | 0.48 | 536 | 196 | 0.37 | 7.19 | 5.95 |
| text.fastest | 18634 | 10557 | 0.56 | 16211 | 2155 | 0.13 | 309.08 | 39.78 |
| text.fast | 12138 | 2033 | 0.17 | 11132 | 2069 | 0.19 | 332.94 | 189.16 |
| text.balanced | 1344 | 1179 | 0.88 | 1347 | 1182 | 0.87 | 384.59 | 378.30 |
| skewed.fastest | 4050 | 3154 | 0.78 | 3468 | 1496 | 0.43 | 2.00 | 2.00 |
| skewed.fast | 1201 | 619 | 0.51 | 1640 | 627 | 0.38 | 1.92 | 1.92 |
| skewed.balanced | 626 | 121 | 0.20 | 618 | 119 | 0.18 | 2.00 | 1.84 |

Json.fastest.mt8 at x1.16 stays the one zstd MT win, inside the cell's known noise band of 0.95 to 1.65 across passes, and its mt16 cell wins at 0.33. Every other cell is ahead, with text.balanced improving to 0.88 and 0.87 from R110's 0.97 and 0.96, and skewed.balanced at 0.20 and 0.18. Our cold-pool mt16 json.fast at 3326 MiB/s still beats zstd's warm-pool reference at 1625 this run against R110's 1608.

## Streaming encode

Throughput uses 64 KiB pulls and interleaved medians, with single-thread and MT8 columns. x is our time divided by zstd's at the same mode, so below 1 means we are faster. The single-thread cells below come from the dll-paired enc-stream side run, this session's healthy pairing, and the MT8 cells from the corpus section.

| cell | ST ours | ST zstd | ST x | MT8 ours | MT8 zstd | MT8 x |
|---|---:|---:|---:|---:|---:|---:|
| json.fastest | 609 | 758 | 1.25 | 3423 | 1718 | 0.49 |
| json.fast | 463 | 437 | 0.94 | 1926 | 401 | 0.21 |
| json.balanced | 151 | 100 | 0.67 | 381 | 114 | 0.30 |
| json.best | 21 | 26 | 1.26 | 78 | 51 | 0.67 |
| json.opt | 7 | 5 | 0.81 | 32 | 7 | 0.22 |
| json.ultra | 3 | 3 | 0.93 | 11 | 3 | 0.28 |
| text.fastest | 6749 | 1769 | 0.26 | 7591 | 4039 | 0.53 |
| text.fast | 6490 | 5104 | 0.79 | 5601 | 1641 | 0.29 |
| text.balanced | 1506 | 1595 | 1.06 | 1200 | 941 | 0.79 |
| text.best | 865 | 608 | 0.62 | 862 | 300 | 0.35 |
| text.opt | 878 | 412 | 0.47 | 766 | 362 | 0.48 |
| text.ultra | 475 | 245 | 0.51 | 299 | 233 | 0.78 |

The corpus pairing of the pass's own enc-stream section read its fast rows low on both sides together, text.fastest at 5344 against the dll-paired 6749 and json.fastest at 494 against 609, with the zstd side down 22 to 30 percent as well, so the table carries the dll-paired cells, whose every cell reads at its healthy level against R110, text.fastest at 6749 against 6812 and json.fastest at 609 against 612. The balanced row read healthy in both pairings, text at 1506 and 1490, closing R110's solo-median carry. The opt-tier family gains read through the grid: json.opt 0.81 against R110's 0.89, json.ultra 0.93 against 1.06, text.opt 0.47 against 0.50, text.ultra 0.51 against 0.62, and the MT8 column with json.opt 0.22 against 0.24, json.ultra 0.28 against 0.30, text.opt 0.48 against 0.51 and text.ultra 0.78 against 0.93. Unknown-size text.fastest streaming has zstd emitting 1.84 MB at ratio 18.3 against our 108 KB at ratio 309. Dll100 stream cells are in the [matrix](matrix.md) streaming section, with fast and balanced flipping to wins at 0.92 and 0.94.

## Decode multi-thread scaling

Fixtures are the same files as the single-thread table, and throughput is MiB/s at 1, 2, 4, 8 and 16 workers. The last column is zstd's single-thread streaming decode, because libzstd has no MT decode, so the scaling columns have no zstd counterpart.

| file | ours ST | mt2 | mt4 | mt8 | mt16 | zstd ST stream ref |
|---|---:|---:|---:|---:|---:|---:|
| json.zst3 | 1458 | 2645 | 3070 | 3179 | 3187 | 1705 |
| text.zst3 | 11852 | 11602 | 11451 | 11608 | 11586 | 11019 |
| skewed.zst9 | 960 | 1365 | 1615 | 1733 | 1552 | 695 |
| random.zst3 | 9675 | 9444 | 9546 | 9554 | 9061 | 8988 |
| dll100.zst3 | 1713 | 2460 | 2896 | 3040 | 3074 | 1602 |
| multi.zst3 | 3409 | 4401 | 6029 | 9131 | 9833 | 3027 |

Dll100 is measured through `files --threads`, with its reference taken from this pass's dll100.zst3 streaming cell, and the multi row is the files-multi section against the dec-ST stream reference. Skewed runs 1.80 times single-thread and 2.49 times the reference at mt8, improving R110's 1.69 and 2.34, with mt16 settling at 1.62. The multi-frame payloads scale at 2.84 to 3.24 times single-thread at mt16 in the [matrix](matrix.md) files-multi table. Json mt16 reads 1.87 times the reference, dll100 1.92 improving from 1.85, and text is flat at 0.98. This section's json st1 cell read 1458 against dec-st's 2099, a low draw of that solo cell, so json's own-ST multiple reads high this pass while the reference multiple holds.

## Compression-ratio sweep

The sweep runs `zstdx-bench ratio` on 5 shapes, 6 tiers, bulk and stream modes and single-thread and 4-worker MT, one deterministic pass per cell with checksums off. The reported delta is ours over zstd's ratio minus 1, aggregated as a geo-mean, and every cell is roundtrip-gated. The geo-mean over 120 cells is **+8.70%**, with all 120 cells reproducing the R110 sizes byte for byte and the zstd reference side byte-flat through the binding move, so the window's commits moved no corpus byte. By mode it reads bulk single-thread +1.47%, bulk MT +11.08%, stream single-thread +11.46% and stream MT +11.11%. By shape it reads json +4.48%, text +40.29%, skewed +1.42%, random plus or minus zero and zeros +2.06%. The losing set is unchanged from R110 at identical bytes: text.opt stream-mt at -0.31%, json.opt at -0.11 to -0.13 percent, json.fast bulk MT at -0.14% and stream MT at -0.12%, skewed.opt at -0.06% and skewed.ultra at -0.03 to -0.05%. The sweep ran its stock parallel-16 schedule clean at 89.7 s with no earlyoom pressure, against R110's forced parallel 4. Details are in the [matrix](matrix.md) ratio section.

## Open and closed deficits

Each row names a measured deficit and its status in this pass. The x figures are wall-time ratios, so values below 1 mean we are faster.

| Deficit | Status in this pass |
|---|---|
| Small-payload level 9 | Closed on 2026-09-28. The flip was the small-src btlazy swap, now an 8 KiB head verdict with a 48-distinct bar. The cells read json level 9 at x1.11, 0.95, 0.67 and 1.02 at 1K, 4K, 64K and 1M on the stock row, text at 1.74, 1.55, 2.83 and 1.01 on the swap row. The level 1 slide at 1K-4K remains the accepted trade |
| Json chain rows 5-8 and 10-12 | Closed on 2026-10-01 with the head-gated attempts diet. The rows carry the ladder at x1.18 to 1.34 and the byte wins against zstd, with the mechanism and `RowAttempts::Light` in [matchers](../perf/matchers.md) |
| text.balanced speed | Closed on 2026-10-30 by the balanced row substitution. Bulk MT reads x0.88 at mt8 and 0.87 at mt16, stream-mt8 reads 0.79, and the single-thread bulk cell reads a healthy in-pass 0.89 this pass. Ratio holds +1.65 to +1.71% denser than zstd-9 |
| Decode single-thread on compressible shapes | Closed on 2026-11-02 by the libzstd-shaped sequence loop: x0.80 to 1.02 bulk and 0.91 to 1.04 stream on json, text and skewed this pass. The dll100 band reads 0.93 to 1.01 at the standing drift caveat, with the attribution in [dec-gap](dec-gap.md) |
| json.fastest | x1.34 single-thread and 1.16 mt8, both inside the cells' known spreads. The steady scan loop's branch-mispredict budget sets a realistic ceiling of about x1.3 to 1.4, and the issue-and-serialization decomposition closed the asm family. Json.fast at 1.05 is near closed |
| Best-tier core | Json 1.41 in-pass against R110's 1.38, skewed 1.53, dll 1.13, n=3 wander at identical bytes, a memory-latency tree walk with its floor in the [todo](../todo.md). It caps the json.best stream single-thread cell at x1.26, while json.best.stream-mt8 reads 0.67 |
| json opt and ultra | The v0.7.1 opt-tier family moved both rows ahead at unchanged bytes: opt x0.82 bulk against R110's 0.91 and stream 0.81 against 0.89, ultra 0.97 and 0.93 against 1.09 and 1.06, dll100.opt 0.83 against 0.93 at 16.1 percent fewer bytes, with the deep-row attribution in [enc-dll-gap](enc-dll-gap.md) |
| MT decode scaling | Skewed runs 1.80 times single-thread at mt8 and 2.49 times the reference, the multi-frame payloads 2.84 to 3.24 times single-thread at mt16, json mt16 1.87 times the reference, dll100 1.92, text flat at 0.98. The walls are sequence-read and aggregate-bandwidth bound, tracked as todo 1 in the [todo](../todo.md) |
| dll100 balanced | Bulk reads x1.01 in-pass at -13.8% size, with best at 1.13 and the deep rows ahead at 0.83 and 0.93 after the opt-tier family. The chain chase remains, with its floor in the [todo](../todo.md) and its differential attribution in [enc-dll-gap](enc-dll-gap.md). Fastest and fast are speed near-parity at 1.07 and 0.99 carrying -49.8% and -31.8% size |
| text single-thread stream fastest and fast | Closed on 2026-11-21 by the pooled window capacity shrink at level-down (a5050ae4). The dll-paired cells read 6749 and 6490 MiB/s at x0.26 and 0.79 this pass; the corpus pairing read 5344 and 6879 with the zstd side down 22 to 30 percent as well, a pairing-level condition on both sides together. The mechanism and reproducer of the one-sided family stay recorded in the [workflow pitfalls](../pitfalls/workflow.md) |
| Balanced-row layout lottery | Dormant this pass: the balanced row read healthy in both enc-stream pairings, text at 1506 and 1490, and the enc-st bulk cell at 0.89, with no solo-median carry anywhere. The family's record and trigger stay in the [workflow pitfalls](../pitfalls/workflow.md): a preceding row in the same process scatters the pooled single-thread core's layout |
| Fast-row screen tax on compressible-flat data | Random fastest and fast read x0.94 and 0.85 from the shortened bar leg plus the 1 MiB twin-scan reject, a deliberate arming-recall bound. Zeros fastest and fast read 0.27 and 0.18, and skewed chain rows read at or below 0.06. Every cell still wins |
| Ratio residues | Json.opt at -0.11 to -0.13% and text.opt stream-mt at -0.31% from the fill-descent cut inside its +0.5 percent bar, json.fast MT at -0.12% to -0.14%, skewed.opt at -0.06% and skewed.ultra at -0.03 to -0.05% as near-ties, and the dictionary small-payload parse on the encoding side. All at R110's exact bytes |
| dll100 streaming-MT encode | Stream-mt8 trails libzstd-mt8 at three of six tiers: fastest x2.05, balanced 1.13 improving from R110's in-pass 1.42 toward the solo 1.16, and best 1.57; leads at fast 0.51, opt 0.46 improving from 0.50 and ultra 0.63. The cells run at 84 to 95 percent of their own bulk-mt8 ceilings, and zstd's 1916 MiB/s fastest headline buys 48 MB of output against our 24 MB, so the two run at different output sizes |
| Placement-phase regressions | Closed class since 2026-10-25: the nbSeq emit fix shifted the dubt count loop onto a 64-byte line boundary for a 10% dll100.best loss at identical instruction counts. The R32 two-copy entry stub in `count_run` now pins the loop head at a phase of 31 or less, following `extend_match_aligned`. The full walk is in [matchers](../perf/matchers.md) |
