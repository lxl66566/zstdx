# Performance Snapshot (2026-11-03)

This page summarizes the 2026-11-03 release pass at commit `b1cccf79`, run on one machine, one corpus and the same flags on both sides under the shared lock, driven by `bash bench/pass.sh all` through one release build at a summed section wall of 31.4 minutes. The comparison target is the zstd crate and libzstd 1.5.7 with zstdmt, and the ladder pairs fastest, fast, balanced, best, opt and ultra against libzstd levels 1, 3, 9, 13, 17 and 19. Raw tables, provenance and per-chunk commands are in [matrix.md](matrix.md), including the full numeric 1-22 ladder and the small-payload table. Throughout this page x is our time divided by the reference time, so values below 1 mean we are faster, and speeds are MiB/s of raw input.

The pass covers the ninety-eight-commit window since the 2026-11-02 R89 pass, whose tables predated the decoder's sequence-loop program. That program is the window's structural mover and this pass carries it end to end: the libzstd-shaped sequence loop, its port into MT stage B, one-shot block bodies parsed in place, one-shot literals staged inside the output buffer, the X2 Huffman hot stretch as the amd64 asm loop shape, the X1 rank-by-rank table fill, and stage-A workers leased from a parked pool. The decoder's single-thread cells moved from x1.05 to 1.38 on every compressible shape to x0.92 to 1.03, the MT decode rows re-based onto the faster single-thread column and widened their lead, and the dll100 residue settled at 1.02 to 1.08. On the encoder side the ahead-of-time serial LDM feed and the strip admission stride moved the twelve deep MT ratio cells by at most 0.06 percent while lifting dll100 opt and ultra stream-mt8 to 0.51 and 0.64, and the unpledged stream grid's deep LDM band lifted dll100 best stream-mt8 from 27 to 55 MiB/s at 20.52 MB output, closing R89's open stream item. The FFI window landed the reference `decompressStream` stage machine as a port, outside the timed paths.

Caveats for reading every table: the pass runs the R86 harness, whose enc-st and enc-stream single-thread cells share the machine in pairs, so absolute MiB/s on those cells reads below the serial tables of pre-R86 passes and cross-pass comparisons use x; a verdict on a 10 GB/s-class cell needs a `--parallel 1` rerun per the [methodology](methodology.md). Noise runs ±10% between sessions, every cell carries interleaved medians at the default per-side budget of 500 ms, and spreads sit in the raw tables.

## Headline results

Decode single-thread reads x0.92 to 1.03 bulk and x0.81 to 1.05 stream on json, text and skewed, with json.zst3 at 0.99 bulk, text.zst3 at 0.96, skewed.zst9 at 0.92 and skewed.zst1 at 0.98. Random and zeros are ours at every tier, with random.zst3 stream at 0.81. Dll100 reads x1.02 to 1.08 on both paths, the one remaining decode band, attributed in [dec-gap](dec-gap.md).

Encode multi-thread has one remaining zstd win: json.fastest.mt8 at x1.11 inside its known noise band of 0.95 to 1.65, while its mt16 cell wins at 0.32. Every other cell is ahead, with text.balanced at 0.90 on both counts, and our cold-pool mt16 json.fast at 3381 MiB/s still beats zstd's warm-pool reference at 1593.

Streaming encode single-thread reads json.fast 0.96, json.opt 0.94 and json.ultra 1.00, text.best 0.65, and text.fastest and fast 0.34 and 0.97 as in-pass readings, because the same-process page-dispersity collapse reproduced for the tenth consecutive pass; the standalone medians read 6504 and 6021 MiB/s against R89's 6638 and 6082.

Streaming encode MT8 keeps every json cell at or below x0.62, with the deep rows at 0.27 through the serial LDM feed. Every text cell wins, with balanced at 0.82. Dll100 stream-mt8 leads at fast 0.51, opt 0.51 and ultra 0.64, and trails at fastest 2.12, balanced 1.14 and best 1.49, running at 89% to 96% of its own bulk-mt8 ceilings. The unpledged deep band closed the best row's stream gap: 55 MiB/s against R89's 27, at 20.52 MB output against the 20.52 MB bulk cell.

Decode MT re-based onto the ported stage-B loop: json mt16 at 1.48 times this pass's single-thread and 1.59 times the zstd stream reference, and dll100.zst3 mt16 at 1.75 times single-thread and 1.83 times the reference, the strongest scaling row on a real-binary payload. Skewed reads 0.99 times single-thread at 1.21 times the reference, and text is flat at 0.98.

The ratio sweep holds the +8.70% geo-mean over 120 cells, with 108 cells byte-identical to R89 and the twelve deep-MT movers within 0.06 percent. The losing set is unchanged in shape, with json.ultra MT at -0.03% replacing text.ultra MT, which the feed moved to parity.

The full 1-22 ladder holds its shape: deep json at or under parity with levels 17 and 19 at 0.99 and level 22 at 0.52, json rows 5-8 and 10-12 at 1.12 to 1.34 with their byte wins, text chain rows at 0.32 to 0.85 with level 9 at 0.92, zeros levels 1-4 at 0.28, 0.28, 0.18 and 0.19, random levels 17-19 at 2183 to 3492 MiB/s at byte-exact ratio under the asymmetric-cell cap, and the skewed hole at levels 13-16 at 1.10 to 1.69. Small payloads improved across the level-3 row at identical bytes, with json l3 4K at 1.12 and text l3 4K at 1.06.

## Decode single-thread

Throughput is MiB/s of raw data for bulk decode and streaming decode with 64 KiB pulls. x is our time divided by zstd's time, so below 1 means we are faster.

| file | bulk ours | bulk zstd | bulk x | stream ours | stream zstd | stream x |
|---|---:|---:|---:|---:|---:|---:|
| json.zst1 | 2224 | 2268 | 1.02 | 2111 | 2180 | 1.03 |
| json.zst3 | 1793 | 1782 | 0.99 | 1828 | 1869 | 1.02 |
| json.zst9 | 2414 | 2359 | 0.98 | 2121 | 2146 | 1.01 |
| json.zst19 | 2652 | 2642 | 1.00 | 2551 | 2624 | 1.03 |
| text.zst1 | 7381 | 6844 | 0.93 | 7325 | 7556 | 1.03 |
| text.zst3 | 9708 | 9270 | 0.96 | 10991 | 11115 | 1.01 |
| text.zst9 | 10225 | 9907 | 0.97 | 11950 | 12108 | 1.01 |
| text.zst19 | 10164 | 9971 | 0.98 | 11910 | 12199 | 1.03 |
| skewed.zst1 | 3202 | 3129 | 0.98 | 3065 | 2839 | 0.93 |
| skewed.zst3 | 1629 | 1635 | 1.00 | 1564 | 1534 | 0.98 |
| skewed.zst9 | 969 | 889 | 0.92 | 820 | 794 | 0.97 |
| skewed.zst19 | 2904 | 2983 | 1.03 | 2767 | 2718 | 0.98 |
| random.zst3 | 9048 | 8959 | 0.99 | 11111 | 8951 | 0.81 |
| zeros.zst3 | 12302 | 11498 | 0.94 | 12894 | 12911 | 1.00 |
| dll100.zst1 | 1460 | 1569 | 1.08 | 1402 | 1497 | 1.07 |
| dll100.zst3 | 1691 | 1749 | 1.03 | 1624 | 1707 | 1.05 |
| dll100.zst9 | 2072 | 2141 | 1.03 | 1978 | 2081 | 1.05 |
| dll100.zst19 | 1798 | 1834 | 1.02 | 1725 | 1803 | 1.05 |

Every corpus shape reads at or under parity bulk except json.zst1 and skewed.zst19 at 1.02 and 1.03, and the stream column holds 0.93 to 1.05 with random's 0.81 the widest win. Dll100's 1.02 to 1.08 band is the FSE-build residue and the copy-loop lumps in [dec-gap](dec-gap.md).

## Encode single-thread bulk

Throughput and ratio are per shape, with checksums off on both sides. The tier column names the ladder tier and the pairing is 1, 3, 9, 13, 17 and 19 against libzstd. x is our time divided by zstd's, so below 1 means we are faster. These cells share the machine in pairs, so x is the comparable column.

| level | shape | ours MiB/s | ours ratio | zstd MiB/s | zstd ratio | x |
|---|---|---:|---:|---:|---:|---:|
| fastest | json | 633 | 6.41 | 833 | 6.11 | 1.31 |
| fastest | text | 12057 | 309.23 | 10065 | 308.94 | 0.84 |
| fastest | skewed | 2653 | 2.00 | 1210 | 2.00 | 0.46 |
| fastest | random | 2171 | 1.00 | 1984 | 1.00 | 0.92 |
| fastest | zeros | 48291 | 32577 | 13094 | 32171 | 0.27 |
| fastest | dll | 483 | 4.35 | 531 | 2.19 | 1.10 |
| fast | json | 448 | 5.30 | 470 | 5.29 | 1.05 |
| fast | text | 12480 | 333.03 | 7137 | 332.90 | 0.57 |
| fast | skewed | 209 | 1.92 | 227 | 1.92 | 1.08 |
| fast | random | 2205 | 1.00 | 2011 | 1.00 | 0.91 |
| fast | zeros | 48602 | 32577 | 8645 | 32171 | 0.18 |
| fast | dll | 424 | 4.97 | 428 | 3.40 | 1.01 |
| balanced | json | 149 | 7.20 | 114 | 5.95 | 0.76 |
| balanced | text | 1835 | 384.66 | 1720 | 378.41 | 0.94 |
| balanced | skewed | 1899 | 2.00 | 73 | 1.84 | 0.039 |
| balanced | random | 2302 | 1.00 | 1456 | 1.00 | 0.64 |
| balanced | zeros | 32403 | 32577 | 1743 | 32202 | 0.054 |
| balanced | dll | 125 | 5.37 | 145 | 4.62 | 1.16 |
| best | json | 23 | 6.26 | 31 | 6.10 | 1.33 |
| best | text | 972 | 386.42 | 652 | 385.90 | 0.69 |
| best | skewed | 5 | 1.84 | 8 | 1.84 | 1.61 |
| best | random | 2164 | 1.00 | 126 | 1.00 | 0.058 |
| best | zeros | 44571 | 32577 | 781 | 32171 | 0.018 |
| best | dll | 26 | 5.35 | 32 | 4.67 | 1.17 |
| opt | json | 6 | 7.49 | 6 | 7.49 | 1.01 |
| opt | text | 851 | 410.23 | 427 | 410.11 | 0.50 |
| opt | skewed | 3 | 2.00 | 2 | 2.00 | 0.70 |
| opt | random | 2254 | 1.00 | 9 | 1.00 | 0.004 |
| opt | zeros | 41005 | 32577 | 844 | 32202 | 0.020 |
| opt | dll | 13 | 6.04 | 11 | 5.06 | 0.93 |
| ultra | json | 3 | 7.42 | 3 | 7.42 | 1.04 |
| ultra | text | 416 | 414.12 | 252 | 413.98 | 0.60 |
| ultra | skewed | 2 | 2.00 | 2 | 2.00 | 0.94 |
| ultra | random | 2298 | 1.00 | 6 | 1.00 | 0.003 |
| ultra | zeros | 37538 | 32577 | 602 | 32202 | 0.016 |
| ultra | dll | 8 | 6.32 | 9 | 5.28 | 1.02 |

No single-thread cell moved its bytes this window, so the readings are wall noise around the R89 verdicts at identical output. We are denser or at parity on json and text at every level, with json.opt 9 bytes ahead, skewed wins everywhere except the chain-row near-tie, random and zeros tie or win, and dll is denser at every tier, double digits from balanced up. The dll deep rows read opt 0.93 and ultra 1.02 against R89's 0.89 and 1.03, n=3 wander on unchanged bytes. Checksums on versus off on our side read json.fast 1.03 and text.fast 0.84.

## Encode multi-thread bulk

Throughput is MiB/s at 8 and 16 workers with one pass, using a cold pool per call on both sides. The x8 and x16 columns are our time divided by zstd's at the same worker count, and the ratio columns are at 16 workers.

| cell | ours mt8 | zstd mt8 | x8 | ours mt16 | zstd mt16 | x16 | ratio ours mt16 | ratio zstd mt16 |
|---|---:|---:|---:|---:|---:|---:|---:|---:|
| json.fastest | 3708 | 4081 | 1.11 | 5333 | 1717 | 0.32 | 6.35 | 6.11 |
| json.fast | 2434 | 1000 | 0.41 | 3381 | 1011 | 0.30 | 5.30 | 5.31 |
| json.balanced | 447 | 205 | 0.46 | 545 | 200 | 0.37 | 7.19 | 5.95 |
| text.fastest | 18441 | 10011 | 0.55 | 17198 | 2211 | 0.13 | 309.08 | 39.78 |
| text.fast | 11492 | 2133 | 0.19 | 11938 | 2073 | 0.17 | 332.94 | 189.16 |
| text.balanced | 1360 | 1203 | 0.90 | 1326 | 1222 | 0.90 | 384.59 | 378.30 |
| skewed.fastest | 4109 | 3226 | 0.79 | 3547 | 1538 | 0.43 | 2.00 | 2.00 |
| skewed.fast | 1213 | 633 | 0.52 | 1657 | 637 | 0.38 | 1.92 | 1.92 |
| skewed.balanced | 621 | 120 | 0.19 | 688 | 121 | 0.18 | 2.00 | 1.84 |

Json.fastest.mt8 at x1.11 is the one zstd MT win, inside the cell's known noise band of 0.95 to 1.65 across passes, and its mt16 cell wins at 0.32. Our cold-pool mt16 json.fast at 3381 MiB/s still beats zstd's warm-pool reference at 1593 this run.

## Streaming encode

Throughput uses 64 KiB pulls and interleaved medians, with single-thread and MT8 columns. x is our time divided by zstd's at the same mode, so below 1 means we are faster. The single-thread cells share the machine in pairs, so x is the comparable column.

| cell | ST ours | ST zstd | ST x | MT8 ours | MT8 zstd | MT8 x |
|---|---:|---:|---:|---:|---:|---:|
| json.fastest | 557 | 746 | 1.25 | 3226 | 1917 | 0.61 |
| json.fast | 458 | 437 | 0.96 | 2171 | 403 | 0.19 |
| json.balanced | 80 | 102 | 1.26 | 385 | 114 | 0.30 |
| json.best | 21 | 26 | 1.27 | 82 | 51 | 0.62 |
| json.opt | 5 | 5 | 0.94 | 26 | 7 | 0.27 |
| json.ultra | 3 | 3 | 1.00 | 11 | 3 | 0.27 |
| text.fastest | 5065* | 1694 | 0.34 | 7900 | 3766 | 0.48 |
| text.fast | 5016* | 4843 | 0.97 | 5753 | 1649 | 0.29 |
| text.balanced | 1364 | 1608 | 1.18 | 1213 | 983 | 0.82 |
| text.best | 835 | 612 | 0.65 | 837 | 357 | 0.43 |
| text.opt | 803 | 413 | 0.51 | 727 | 371 | 0.51 |
| text.ultra | 409 | 252 | 0.61 | 383 | 229 | 0.60 |

The two text single-thread fast cells carry in-pass readings at 5065 and 5016 MiB/s, and the same-process page-dispersity collapse reproduced for the tenth consecutive pass, with the mechanism and reproducer in the [workflow pitfalls](../pitfalls/workflow.md); a verdict on either cell needs the standalone rerun protocol, and the standalone medians read 6504 and 6021 MiB/s against R89's 6638 and 6082. Unknown-size text.fastest streaming has zstd emitting 1.84 MB at ratio 18.3 against our 108 KB at ratio 309. Dll100 stream cells are in the [matrix](matrix.md) streaming section.

## Decode multi-thread scaling

Fixtures are the same files as the single-thread table, and throughput is MiB/s at 1, 2, 4, 8 and 16 workers. The last column is zstd's single-thread streaming decode, because libzstd has no MT decode, so the scaling columns have no zstd counterpart.

| file | ours ST | mt2 | mt4 | mt8 | mt16 | zstd ST stream ref |
|---|---:|---:|---:|---:|---:|---:|
| json.zst3 | 2008 | 2491 | 2764 | 2993 | 2970 | 1873 |
| text.zst3 | 12046 | 11707 | 11692 | 11726 | 11750 | 11185 |
| skewed.zst9 | 979 | 979 | 977 | 943 | 968 | 797 |
| random.zst3 | 9752 | 9459 | 9501 | 9486 | 9508 | 8978 |
| dll100.zst3 | 1694 | 2340 | 2780 | 2958 | 2973 | 1624 |

Dll100 is measured through `files --threads`, with its reference taken from this pass's `dll100.zst3` streaming cell. The restart-point decode scales best on the large real-binary payload at mt16 1.75 times single-thread and 1.83 times the reference. Json mt16 reads 1.48 times single-thread and 1.59 times the reference, re-based onto the ported loop's faster single-thread column. Skewed holds 0.99 times single-thread at 1.21 times the reference with its band wandering ±15% across passes, and text is flat at 0.98.

## Compression-ratio sweep

The sweep runs `zstdx-bench ratio` on 5 shapes, 6 tiers, bulk and stream modes and single-thread and 4-worker MT, one deterministic pass per cell with checksums off. The reported delta is ours over zstd's ratio minus 1, aggregated as a geo-mean, and every cell is roundtrip-gated. The geo-mean over 120 cells is **+8.70%**, with 108 cells reproducing the R89 sizes byte for byte and the twelve deep-MT movers within 0.06 percent. By mode it reads bulk single-thread +1.49%, bulk MT +11.10%, stream single-thread +11.44% and stream MT +11.12%. By shape it reads json +4.46%, text +40.37%, skewed +1.41%, random plus or minus zero and zeros +2.06%. The complete losing cells are json.fast bulk MT at -0.14% and stream MT at -0.12%, json.ultra MT at -0.03%, skewed.opt at -0.06%, skewed.ultra at -0.04% to -0.05%, skewed.fast bulk MT at -0.01% and stream MT at -0.03%, and skewed.fastest at -0.00%. Details are in the [matrix](matrix.md) ratio section.

## Open and closed deficits

Each row names a measured deficit and its status in this pass. The x figures are wall-time ratios, so values below 1 mean we are faster.

| Deficit | Status in this pass |
|---|---|
| Small-payload level 9 | Closed on 2026-09-28. The flip was the small-src btlazy swap, now an 8 KiB head verdict with a 48-distinct bar. The cells read json level 9 at x1.07, 0.93, 0.66 and 1.01 at 1K, 4K, 64K and 1M on the stock row, text at 1.78, 1.62, 2.86 and 1.02 on the swap row. The level 1 slide at 1K-4K remains the accepted trade |
| Json chain rows 5-8 and 10-12 | Closed on 2026-10-01 with the head-gated attempts diet. The rows carry the ladder at x1.12 to 1.34 and the byte wins against zstd, with the mechanism and `RowAttempts::Light` in [matchers](../perf/matchers.md) |
| text.balanced speed | Closed on 2026-10-30 by the balanced row substitution. Bulk reads x0.94 single-thread, 0.90 at mt8 and 0.90 at mt16, stream single-thread reads 1.18 and stream-mt8 0.82. Ratio holds +1.65% denser than zstd-9 |
| Decode single-thread on compressible shapes | Closed on 2026-11-02 by the libzstd-shaped sequence loop, and this pass carries the loop with the literals-side rounds behind it: x0.92 to 1.03 bulk and 0.93 to 1.05 stream on json, text and skewed. The method was a reference-compiler differential, and the remaining dll100 band is in [dec-gap](dec-gap.md) |
| json.fastest | x1.31 single-thread and 1.11 mt8, both inside the cells' known spreads. The steady scan loop's branch-mispredict budget sets a realistic ceiling of about x1.3 to 1.4, and the issue-and-serialization decomposition closed the asm family. Json.fast at 1.05 is near closed |
| Best-tier core | Json 1.33 and skewed 1.61 this pass against the R89 1.37 and 1.56, an n=3 wander at identical bytes, a memory-latency tree walk with its floor in the [todo](../todo.md). It caps the json.best stream single-thread cell at x1.27, while json.best.stream-mt8 reads 0.62 |
| json opt and ultra | Closed on 2026-10-01 with hugepage-backed buffers, and the R86 price-table mirrors hold them at x1.01 and 1.04 at ratio parity, with json.opt 9 bytes ahead. Skewed opt and ultra read 0.70 and 0.94, and dll100.opt reads 0.93 and dll100.ultra 1.02 against R89's 0.89 and 1.03, n=3 wander at unchanged bytes |
| MT decode scaling | The stage-B executor runs the ported loop, so the rows re-based up: json mt16 at 1.48 times this pass's single-thread and 1.59 times the reference, dll100 1.75 and 1.83, skewed 0.99 and 1.21, text flat at 0.98. The walls are sequence-read and aggregate-bandwidth bound, tracked as todo 1 in the [todo](../todo.md) |
| dll100 balanced | x1.16 at -14.0% size, with best at 1.17 and the deep rows ahead at 0.93 and 1.02. The chain chase remains, with its floor in the [todo](../todo.md) and its differential attribution in [enc-dll-gap](enc-dll-gap.md). Fastest and fast are speed near-parity at 1.10 and 1.01 carrying -49.6% and -31.7% size |
| text single-thread stream fastest and fast | The in-pass page-dispersity collapse reproduced for the tenth consecutive pass at 5065 and 5016 MiB/s in-pass, at x0.34 and 0.97. It was root-caused on 2026-09-29 in the [workflow pitfalls](../pitfalls/workflow.md), where a best or L13 row preceding the pooled single-thread core scatters the big buffers' pages and THP recovers about 11%, leaving the residual unattributed. Both cells still win |
| Fast-row screen tax on compressible-flat data | Random fastest and fast read x0.92 and 0.91 from the shortened bar leg plus the 1 MiB twin-scan reject, a deliberate arming-recall bound. Zeros fastest and fast read 0.27 and 0.18, and skewed chain rows read at or below 0.06. Every cell still wins |
| Ratio residues | Json.fast MT at -0.12% to -0.14%, json.ultra MT at -0.03% from the serial feed, skewed.opt at -0.06% as a near-tie, and the dictionary small-payload parse on the encoding side |
| dll100 streaming-MT encode | Stream-mt8 trails libzstd-mt8 at three of six tiers: fastest x2.12, balanced 1.14 and best 1.49; leads at fast 0.51, opt 0.51 and ultra 0.64. The cells run at 89% to 96% of their own bulk-mt8 ceilings of 1051, 994, 191, 58, 51 and 31 MiB/s. The unpledged deep band landed this window and closed the best row's stream gap, 55 MiB/s against R89's 27 at 20.52 MB output, and the serial feed lifted opt and ultra to 0.51 and 0.64. Meanwhile zstd's 1971 MiB/s fastest headline buys 48 MB of output against our 24 MB, so the two run at different output sizes |
| Placement-phase regressions | Closed class since 2026-10-25: the nbSeq emit fix shifted the dubt count loop onto a 64-byte line boundary for a 10% dll100.best loss at identical instruction counts. The R32 two-copy entry stub in `count_run` now pins the loop head at a phase of 31 or less, following `extend_match_aligned`. The full walk is in [matchers](../perf/matchers.md) |
