# Performance Snapshot (2026-11-02)

This page summarizes the 2026-11-02 release pass at commit `2ba353fa`, run on one machine, one corpus and the same flags on both sides under the shared lock, driven by `bash bench/pass.sh all` through one release build at a summed section wall of 32.2 minutes. The comparison target is the zstd crate and libzstd 1.5.7 with zstdmt, and the ladder pairs fastest, fast, balanced, best, opt and ultra against libzstd levels 1, 3, 9, 13, 17 and 19. Raw tables, provenance and per-chunk commands are in [matrix.md](matrix.md), including the full numeric 1-22 ladder and the small-payload table. Throughout this page x is our time divided by the reference time, so values below 1 mean we are faster, and speeds are MiB/s of raw input.

The pass covers the encode window since the 2026-11-01 R86 pass, whose three commits moved no stable byte on any cell here. The one structural mover is the windowed LDM far class `6fe3c447`, which arms far history on the Best rows' bulk-MT and pledged-stream jobs: the R88 record has dll100 best bulk-mt8 at 20,515,629 B from 22,328,775 with its ceiling lifted from 44 to 57 MiB/s, while the unpledged stream-mt8 grid and every single-thread cell keep their bytes, so the pass reads as wall noise around the R86 verdicts everywhere else. The pooled-core reach re-declaration `489d04b4` and the donation-discard fix `532c2598` repair a belt-only degenerate frame and a growing-grid checksum corner, byte-identical through the 150-cell dump gate.

Caveats for reading every table: the pass runs the R86 harness, whose enc-st and enc-stream single-thread cells share the machine in pairs, so absolute MiB/s on those cells reads below the serial tables of pre-R86 passes and cross-pass comparisons use x; a verdict on a 10 GB/s-class cell needs a `--parallel 1` rerun per the [methodology](methodology.md). Noise runs ±10% between sessions, every cell carries interleaved medians at the default per-side budget of 500 ms, and spreads sit in the raw tables.

## Headline results

Decode single-thread reads uniformly x0.98 to 1.38 on both the bulk and streaming paths across the compressible shapes: json spans 1.17 to 1.38, text 0.98 to 1.21, skewed 1.09 to 1.35 and dll 1.26 to 1.38, with json.zst3 at bulk 1.38 against stream 1.32, text.zst3 bulk at 1.00 and dll100.zst3 bulk at 1.38. Random wins both paths at 0.99 and 0.81, zeros reads 0.92 bulk against 1.00 stream. The remaining gap is the fused decode loop's op volume, measured in [dec-gap](dec-gap.md) and [mzstd-dec](../review/mzstd-dec.md).

Encode single-thread on json reads fastest 1.35, fast 1.06, balanced 0.77 with +21% ratio, best 1.37, and opt and ultra at 1.01 and 1.00 at ratio parity, with json.opt 9 bytes ahead of zstd-17. Text reads fastest 0.84, fast 0.58, balanced 0.94 at +1.65% ratio, best 0.85, opt 0.50 and ultra 0.59. Skewed reads fastest 0.45, fast 1.05, balanced 0.039, best 1.56, and opt and ultra 0.80 and 0.94 at ratio parity. The two best rows read high in-pass, and a `--parallel 1` rerun settles json.best at 1.28, text.best at 0.69 and the ladder's text.l15 at 0.49 against the R86 1.23, 0.66 and 0.48 at identical bytes, so the in-pass gap is pair-sharing tax, not an encoder move.

Random and zeros are ours at every tier, up to 300 times on incompressible high tiers. Zeros fastest and fast read 0.27 and 0.18, and random 0.93 and 0.93. On dll, fastest reads 1.09 at -49.6% size with ratio 4.35 against zstd-1's 2.19, fast 0.97 at -31.7%, balanced 1.18 at -14.0%, best 1.17 at -12.8%, opt 0.89 at -16.2% and ultra 1.03 at -16.5%.

Encode multi-thread has one remaining zstd win: json.fastest.mt8 at x1.12 inside its known noise band of 0.95 to 1.65, while its mt16 cell wins at 0.33. Every other cell is ahead, with text.balanced at 0.88 and 0.90, and our cold-pool mt16 json.fast at 3366 MiB/s still beats zstd's warm-pool reference at 1572.

Streaming encode single-thread reads json.fast 0.96 and json.fastest 1.29, text.best 0.64, and text.fastest and fast 0.33 and 0.95 as in-pass readings, because the same-process page-dispersity collapse reproduced for the ninth consecutive pass; the standalone medians read 6638 and 6082 MiB/s. Json.opt.stream reads 0.99 and json.ultra.stream 0.97 at unchanged bytes.

Streaming encode MT8 keeps every json cell at or below x0.63. Every text cell wins, with balanced at 0.81. Dll100 stream-mt8 leads at fast 0.54, opt 0.67 and ultra 0.92, and trails at fastest 2.05, balanced 1.21 and best 3.09, running at 47% to 91% of its own bulk-mt8 ceilings. The windowed LDM far class lifted the best row's ceiling from 44 to 57 MiB/s and cut its bulk-mt8 bytes by 8.1%, while the unpledged stream grid keeps the pre-class bytes, so arming the unpledged path stays the open item in the [todo](../todo.md).

Decode MT carries json mt16 at 1.56 times our single-thread and 1.20 times the zstd stream reference, and dll100.zst3 mt16 at 1.78 times single-thread and 1.32 times the reference, the strongest scaling row on a real-binary payload. Skewed reads 1.01 times single-thread and 0.83 times the reference, with its absolute band wandering ±15% across passes, and text is flat at 0.98.

The ratio sweep reproduced the 2026-10-30 sizes byte for byte at a geo-mean of +8.70% denser over 120 cells, with the losing set unchanged. The dump belt read byte-identical through the R88 gates.

The full 1-22 ladder holds the R86 shape: deep json at or under parity, levels 17 through 22 at x0.52 to 1.15 with level 19 at 1.00, json rows 5-8 and 10-12 at 1.01 to 1.34 with their byte wins, text chain rows at 0.33 to 0.84 with level 9 at 0.94, zeros levels 1-4 at 0.28, 0.29, 0.19 and 0.19, random levels 17-19 at 2222 to 3582 MiB/s at byte-exact ratio under the asymmetric-cell cap, and the skewed hole at levels 13-16 at 1.14 to 1.64. Small payloads read json level 9 at x1.08, 1.09, 0.67 and 1.01.

## Decode single-thread

Throughput is MiB/s of raw data for bulk decode and streaming decode with 64 KiB pulls. x is our time divided by zstd's time, so below 1 means we are faster.

| file | bulk ours | bulk zstd | bulk x | stream ours | stream zstd | stream x |
|---|---:|---:|---:|---:|---:|---:|
| json.zst1 | 1782 | 2267 | 1.27 | 1787 | 2194 | 1.23 |
| json.zst3 | 1479 | 2032 | 1.38 | 1424 | 1873 | 1.32 |
| json.zst9 | 1772 | 2349 | 1.33 | 1684 | 2132 | 1.27 |
| json.zst19 | 2226 | 2641 | 1.19 | 2241 | 2620 | 1.17 |
| text.zst1 | 6275 | 6750 | 1.08 | 6291 | 7597 | 1.21 |
| text.zst3 | 9276 | 9235 | 1.00 | 10418 | 11145 | 1.07 |
| text.zst9 | 10000 | 9897 | 0.99 | 11567 | 12077 | 1.05 |
| text.zst19 | 10046 | 9863 | 0.98 | 11618 | 12158 | 1.05 |
| skewed.zst1 | 2325 | 3077 | 1.33 | 2597 | 2841 | 1.10 |
| skewed.zst3 | 1275 | 1635 | 1.29 | 1299 | 1536 | 1.18 |
| skewed.zst9 | 656 | 886 | 1.35 | 617 | 793 | 1.29 |
| skewed.zst19 | 2229 | 2971 | 1.33 | 2469 | 2693 | 1.09 |
| random.zst3 | 9052 | 8965 | 0.99 | 11126 | 8937 | 0.81 |
| zeros.zst3 | 12389 | 11299 | 0.92 | 12881 | 12925 | 1.00 |
| dll100.zst1 | 1188 | 1578 | 1.33 | 1206 | 1514 | 1.26 |
| dll100.zst3 | 1263 | 1747 | 1.38 | 1295 | 1710 | 1.32 |
| dll100.zst9 | 1573 | 2135 | 1.36 | 1596 | 2077 | 1.30 |
| dll100.zst19 | 1362 | 1830 | 1.34 | 1391 | 1800 | 1.29 |

The bulk column agrees with the streaming column, with every compressible shape reading x0.98 to 1.38 on both paths. Text bulk sits at parity, random wins both paths, and the residual gap on both paths is the fused loop's op volume, closed as a measured floor in [dec-gap](dec-gap.md) and [mzstd-dec](../review/mzstd-dec.md).

## Encode single-thread bulk

Throughput and ratio are per shape, with checksums off on both sides. The tier column names the ladder tier and the pairing is 1, 3, 9, 13, 17 and 19 against libzstd. x is our time divided by zstd's, so below 1 means we are faster. These cells share the machine in pairs, so x is the comparable column.

| level | shape | ours MiB/s | ours ratio | zstd MiB/s | zstd ratio | x |
|---|---|---:|---:|---:|---:|---:|
| fastest | json | 623 | 6.41 | 840 | 6.11 | 1.35 |
| fastest | text | 12031 | 309.23 | 10008 | 308.94 | 0.84 |
| fastest | skewed | 2736 | 2.00 | 1240 | 2.00 | 0.45 |
| fastest | random | 2090 | 1.00 | 1950 | 1.00 | 0.93 |
| fastest | zeros | 48871 | 32577 | 13223 | 32171 | 0.27 |
| fastest | dll | 484 | 4.35 | 530 | 2.19 | 1.09 |
| fast | json | 445 | 5.30 | 472 | 5.29 | 1.06 |
| fast | text | 12328 | 333.03 | 7121 | 332.90 | 0.58 |
| fast | skewed | 211 | 1.92 | 223 | 1.92 | 1.05 |
| fast | random | 2158 | 1.00 | 1995 | 1.00 | 0.93 |
| fast | zeros | 47730 | 32577 | 8631 | 32171 | 0.18 |
| fast | dll | 428 | 4.97 | 420 | 3.40 | 0.97 |
| balanced | json | 149 | 7.20 | 114 | 5.95 | 0.77 |
| balanced | text | 1830 | 384.66 | 1729 | 378.41 | 0.94 |
| balanced | skewed | 1918 | 2.00 | 74 | 1.84 | 0.039 |
| balanced | random | 2264 | 1.00 | 1424 | 1.00 | 0.63 |
| balanced | zeros | 32826 | 32577 | 1747 | 32202 | 0.053 |
| balanced | dll | 123 | 5.37 | 144 | 4.62 | 1.18 |
| best | json | 22 | 6.26 | 30 | 6.10 | 1.37 |
| best | text | 767 | 386.42 | 647 | 385.90 | 0.85 |
| best | skewed | 5 | 1.84 | 9 | 1.84 | 1.56 |
| best | random | 2163 | 1.00 | 122 | 1.00 | 0.058 |
| best | zeros | 45163 | 32577 | 774 | 32171 | 0.017 |
| best | dll | 26 | 5.35 | 30 | 4.67 | 1.17 |
| opt | json | 6 | 7.49 | 5 | 7.49 | 1.01 |
| opt | text | 853 | 410.23 | 427 | 410.11 | 0.50 |
| opt | skewed | 2 | 2.00 | 2 | 2.00 | 0.80 |
| opt | random | 2244 | 1.00 | 9 | 1.00 | 0.004 |
| opt | zeros | 38804 | 32577 | 843 | 32202 | 0.022 |
| opt | dll | 13 | 6.04 | 11 | 5.06 | 0.89 |
| ultra | json | 3 | 7.42 | 3 | 7.42 | 1.00 |
| ultra | text | 422 | 414.12 | 247 | 413.98 | 0.59 |
| ultra | skewed | 2 | 2.00 | 2 | 2.00 | 0.94 |
| ultra | random | 2240 | 1.00 | 6 | 1.00 | 0.003 |
| ultra | zeros | 37153 | 32577 | 598 | 32202 | 0.016 |
| ultra | dll | 8 | 6.32 | 9 | 5.28 | 1.03 |

The window moved no stable byte here, so the readings are wall noise around the R86 verdicts. The R86 movers hold: the price-table mirrors with the head-slot prefetch keep json.opt and json.ultra at 1.01 and 1.00 at ratio parity and dll100.opt at 0.89 with dll100.ultra at 1.03, and the rep0 skip keeps skewed.best at 1.56. We are denser or at parity on json and text at every level, with json.opt 9 bytes ahead, skewed wins everywhere except the chain-row near-tie, random and zeros tie or win, and dll is denser at every tier, double digits from balanced up. The dll fast-class cells read 1.09, 0.97 and 1.18 against the serial band's 1.04, 0.97 and 1.12, the pair-sharing tax. Checksums on versus off on our side read json.fast 1.00 and text.fast 0.85.

## Encode multi-thread bulk

Throughput is MiB/s at 8 and 16 workers with one pass, using a cold pool per call on both sides. The x8 and x16 columns are our time divided by zstd's at the same worker count, and the ratio columns are at 16 workers.

| cell | ours mt8 | zstd mt8 | x8 | ours mt16 | zstd mt16 | x16 | ratio ours mt16 | ratio zstd mt16 |
|---|---:|---:|---:|---:|---:|---:|---:|---:|
| json.fastest | 3636 | 4028 | 1.12 | 5401 | 1751 | 0.33 | 6.35 | 6.11 |
| json.fast | 2383 | 1006 | 0.42 | 3366 | 1019 | 0.31 | 5.30 | 5.31 |
| json.balanced | 413 | 204 | 0.49 | 510 | 200 | 0.40 | 7.19 | 5.95 |
| text.fastest | 18854 | 10698 | 0.57 | 16583 | 2187 | 0.13 | 309.08 | 39.78 |
| text.fast | 11785 | 2109 | 0.18 | 11224 | 2061 | 0.18 | 332.94 | 189.16 |
| text.balanced | 1363 | 1210 | 0.88 | 1313 | 1196 | 0.90 | 384.59 | 378.30 |
| skewed.fastest | 4027 | 3217 | 0.80 | 3545 | 1523 | 0.43 | 2.00 | 2.00 |
| skewed.fast | 1216 | 607 | 0.50 | 1645 | 634 | 0.39 | 1.92 | 1.92 |
| skewed.balanced | 712 | 123 | 0.17 | 693 | 121 | 0.17 | 2.00 | 1.84 |

Json.fastest.mt8 at x1.12 is the one zstd MT win, inside the cell's known noise band of 0.95 to 1.65 across passes, and its mt16 cell wins at 0.33. Our cold-pool mt16 json.fast at 3366 MiB/s still beats zstd's warm-pool reference at 1572 this run.

## Streaming encode

Throughput uses 64 KiB pulls and interleaved medians, with single-thread and MT8 columns. x is our time divided by zstd's at the same mode, so below 1 means we are faster. The single-thread cells share the machine in pairs, so x is the comparable column.

| cell | ST ours | ST zstd | ST x | MT8 ours | MT8 zstd | MT8 x |
|---|---:|---:|---:|---:|---:|---:|
| json.fastest | 573 | 743 | 1.29 | 3282 | 1879 | 0.58 |
| json.fast | 453 | 435 | 0.96 | 2200 | 394 | 0.18 |
| json.balanced | 76 | 98 | 1.32 | 396 | 115 | 0.29 |
| json.best | 20 | 25 | 1.21 | 83 | 50 | 0.63 |
| json.opt | 5 | 5 | 0.99 | 18 | 7 | 0.38 |
| json.ultra | 3 | 2 | 0.97 | 6 | 3 | 0.48 |
| text.fastest | 5067* | 1697 | 0.33 | 7671 | 3935 | 0.51 |
| text.fast | 5014* | 4754 | 0.95 | 5724 | 1597 | 0.28 |
| text.balanced | 1352 | 1597 | 1.18 | 1210 | 964 | 0.81 |
| text.best | 881 | 584 | 0.64 | 824 | 352 | 0.41 |
| text.opt | 810 | 410 | 0.51 | 756 | 368 | 0.49 |
| text.ultra | 413 | 249 | 0.60 | 396 | 227 | 0.57 |

The two text single-thread fast cells carry in-pass readings at 5067 and 5014 MiB/s, and the same-process page-dispersity collapse reproduced for the ninth consecutive pass, with the mechanism and reproducer in the [workflow pitfalls](../pitfalls/workflow.md); a verdict on either cell needs the standalone rerun protocol, and the standalone medians read 6638 and 6082 MiB/s this pass. Unknown-size text.fastest streaming has zstd emitting 1.84 MB at ratio 18.3 against our 108 KB at ratio 309. Dll100 stream cells are in the [matrix](matrix.md) streaming section.

## Decode multi-thread scaling

Fixtures are the same files as the single-thread table, and throughput is MiB/s at 1, 2, 4, 8 and 16 workers. The last column is zstd's single-thread streaming decode, because libzstd has no MT decode, so the scaling columns have no zstd counterpart.

| file | ours ST | mt2 | mt4 | mt8 | mt16 | zstd ST stream ref |
|---|---:|---:|---:|---:|---:|---:|
| json.zst3 | 1435 | 1816 | 2072 | 2200 | 2243 | 1877 |
| text.zst3 | 11289 | 10994 | 10953 | 11037 | 10961 | 11217 |
| skewed.zst9 | 654 | 753 | 640 | 758 | 658 | 794 |
| random.zst3 | 9736 | 9472 | 9501 | 9499 | 9506 | 8844 |
| dll100.zst3 | 1267 | 1819 | 2070 | 2203 | 2250 | 1710 |

Dll100 is measured through `files --threads`, with its reference taken from this pass's `dll100.zst3` streaming cell. The restart-point decode scales best on the large real-binary payload at mt16 1.78 times single-thread and 1.32 times the reference. Json mt16 reads 1.56 times single-thread and 1.20 times the reference, with mt8 and mt16 reading level. Skewed holds 1.01 times single-thread at 0.83 times the reference with its band wandering ±15% across passes, and text is flat at 0.98.

## Compression-ratio sweep

The sweep runs `zstdx-bench ratio` on 5 shapes, 6 tiers, bulk and stream modes and single-thread and 4-worker MT, one deterministic pass per cell with checksums off. The reported delta is ours over zstd's ratio minus 1, aggregated as a geo-mean, and every cell is roundtrip-gated. The geo-mean over 120 cells is **+8.70%**, reproducing the 2026-10-30 sizes byte for byte. By mode it reads bulk single-thread +1.49%, bulk MT +11.09%, stream single-thread +11.44% and stream MT +11.12%. By shape it reads json +4.46%, text +40.36%, skewed +1.41%, random plus or minus zero and zeros +2.06%. The complete losing cells are unchanged: json.fast bulk MT at -0.14% and stream MT at -0.12%, text.ultra MT at -0.02%, skewed.opt at -0.06% to -0.07%, skewed.ultra at -0.04%, skewed.fast bulk MT at -0.01% and stream MT at -0.03%, and skewed.fastest at -0.00%. Details are in the [matrix](matrix.md) ratio section.

## Open and closed deficits

Each row names a measured deficit and its status in this pass. The x figures are wall-time ratios, so values below 1 mean we are faster.

| Deficit | Status in this pass |
|---|---|
| Small-payload level 9 | Closed on 2026-09-28. The flip was the small-src btlazy swap, now an 8 KiB head verdict with a 48-distinct bar. The cells read json level 9 at x1.08, 1.09, 0.67 and 1.01 at 1K, 4K, 64K and 1M on the stock row, text at 1.89, 1.66, 2.86 and 1.02 on the swap row. The level 1 slide at 1K-4K remains the accepted trade |
| Json chain rows 5-8 and 10-12 | Closed on 2026-10-01 with the head-gated attempts diet. The rows carry the ladder at x1.01 to 1.34 and the byte wins against zstd, with the mechanism and `RowAttempts::Light` in [matchers](../perf/matchers.md) |
| text.balanced speed | Closed on 2026-10-30 by the balanced row substitution. Bulk reads x0.94 single-thread, 0.88 at mt8 and 0.90 at mt16, stream single-thread reads 1.18 and stream-mt8 0.81. Ratio holds +1.65% denser than zstd-9 |
| Streaming decode on compressible shapes | Bulk and stream read the same band at x0.98 to 1.38, after the reference-side buffer-reuse fix put the bulk column on the reference's real speed. The fused-loop serial chain remains, closed as a measured op-volume floor in [dec-gap](dec-gap.md) and [mzstd-dec](../review/mzstd-dec.md) |
| json.fastest | x1.35 single-thread and 1.12 mt8, both inside the cells' known spreads. The steady scan loop's branch-mispredict budget sets a realistic ceiling of about x1.3 to 1.4, and the plain head block is the next named lever. Json.fast at 1.06 is near closed |
| Best-tier core | Json 1.37 and skewed 1.56 this pass against the R86 1.23 and 1.58, an n=3 wander at identical bytes, a memory-latency tree walk with its floor in the [todo](../todo.md). It caps the json.best stream single-thread cell at x1.21, while json.best.stream-mt8 reads 0.63 |
| json opt and ultra | Closed on 2026-10-01 with hugepage-backed buffers, and the R86 price-table mirrors with the head-slot prefetch hold them at x1.01 and 1.00 at ratio parity, with json.opt 9 bytes ahead. The same movers hold dll100.opt at 0.89 and dll100.ultra at 1.03, and skewed opt and ultra ahead at 0.80 and 0.94 |
| MT decode scaling | Json mt16 at 1.56 times single-thread and 1.20 times the reference, dll100 1.78 and 1.32, skewed 1.01 and 0.83, text flat at 0.98. The memory-bound verdict and execute walls stand, with no lever short of overlapping execute with the verdict, a falsified class |
| dll100 balanced | x1.18 at -14.0% size, with best at 1.17 and the deep rows ahead at 0.89 and 1.03. The chain chase remains, with its floor in the [todo](../todo.md) and its differential attribution in [enc-dll-gap](enc-dll-gap.md). Fastest and fast are speed near-parity at 1.09 and 0.97 carrying -49.6% and -31.7% size |
| text single-thread stream fastest and fast | The in-pass page-dispersity collapse reproduced for the ninth consecutive pass at 5067 and 5014 MiB/s in-pass, at x0.33 and 0.95, with the standalone medians at 6638 and 6082 MiB/s. It was root-caused on 2026-09-29 in the [workflow pitfalls](../pitfalls/workflow.md), where a best or L13 row preceding the pooled single-thread core scatters the big buffers' pages and THP recovers about 11%, leaving the residual unattributed. Both cells still win |
| Fast-row screen tax on compressible-flat data | Random fastest and fast read x0.93 and 0.93 from the shortened bar leg plus the 1 MiB twin-scan reject, a deliberate arming-recall bound. Zeros fastest and fast read 0.27 and 0.18, and skewed chain rows read at or below 0.06. Every cell still wins |
| Ratio residues | Json.fast MT at -0.12% to -0.14%, skewed.opt at -0.07% as a near-tie, and the dictionary small-payload parse on the encoding side |
| dll100 streaming-MT encode | Stream-mt8 trails libzstd-mt8 at three of six tiers: fastest x2.05, balanced 1.21 and best 3.09, leads at fast 0.54, opt 0.67 and ultra 0.92. The cells run at 47% to 91% of their own bulk-mt8 ceilings of 1059, 971, 191, 57, 42 and 23 MiB/s, with best the lowest share at 47%. The windowed LDM far class lifted that best ceiling from 44 to 57 MiB/s and cut its bulk-mt8 bytes from 22.33 to 20.52 MB, while the unpledged stream grid keeps the pre-class bytes, so arming the unpledged path is the open item in the [todo](../todo.md). Meanwhile zstd's 1939 MiB/s fastest headline buys 48 MB of output against our 24 MB, so the two run at different output sizes |
| Placement-phase regressions | Closed class since 2026-10-25: the nbSeq emit fix shifted the dubt count loop onto a 64-byte line boundary for a 10% dll100.best loss at identical instruction counts. The R32 two-copy entry stub in `count_run` now pins the loop head at a phase of 31 or less, following `extend_match_aligned`. The full walk is in [matchers](../perf/matchers.md) |
