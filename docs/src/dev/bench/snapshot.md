# Performance Snapshot (2026-10-25)

This page summarizes the 2026-10-25 pre-release pass at commit `49f48b20`, the v0.3.0 release pass, run on one machine, one corpus and the same flags on both sides. The comparison target is the zstd crate and libzstd 1.5.7 with zstdmt, and the ladder pairs fastest, fast, balanced, best, opt and ultra against libzstd levels 1, 3, 9, 13, 17 and 19. Raw tables, provenance and per-chunk commands are in [matrix.md](matrix.md), including the full numeric 1-22 ladder and the small-payload table. Encoder outputs are byte-identical to the 2026-10-18 pass: all 120 ratio cells and all 110 ladder cells are size-equal and the geo-mean stays at +8.69%. The pass found one beyond-noise speed regression, dll100.best on the single-thread paths at minus 10%, root-caused to a code-placement phase effect from the nbSeq emit fix and fixed in-session by extracting the count loop behind the R32 two-copy phase-proof entry; the dll encode tables carry the post-fix rerun and read best restored to 31 MiB/s bulk, 31 stream and a 29 MiB-s MT ceiling. Speed gains land where the window points: the zeros tree tiers read 7% to 12% faster, and random's opt-tier ladder rows read four times the previous pass at byte-exact ratio 1.00 through the raw-block fast path skipping the tree parse on pure noise. Caveats: ±10% noise between runs, a per-side budget of 1 second and 500 ms on the full ladder and small payloads, interleaved medians, and spreads in the raw tables. Throughout this page x is our time divided by the reference time, so values below 1 mean we are faster, and speeds are MiB/s of raw input.

## Headline results

Decode single-thread bulk wins 17 of 18 cells at x0.21 to 0.99. Skewed.zst9 sits at x0.99 as the near-tie. Absolute throughput runs 624 to 11956 MiB/s, and the dll cells run 1178 to 1560 MiB/s.

Decode single-thread streaming still goes to zstd on every compressible shape: json at x1.17 to 1.29, text at 1.06 to 1.19, skewed at 1.09 to 1.27 and dll at 1.24 to 1.29. We win random at 0.81 and hold zeros at 1.00, and every compressible band tightened a few percent against the previous pass.

Encode single-thread on json reads fastest 1.46, fast 1.05, balanced 0.77 with +21% density, best 1.24, and opt and ultra 1.00 and 1.01 at ratio parity or denser, with json.opt 9 bytes ahead of zstd-17. Four of six json tiers are at parity or ahead. Text reads fastest 0.81, fast 0.54, balanced 1.10 at +1.65% density, best 0.65, opt 0.53 and ultra 0.62. Skewed reads fastest 0.44, balanced 0.041, fast 1.06, best 1.81, and opt and ultra 0.93 and 0.95 at ratio parity.

Random and zeros are ours at every tier, up to 300 times on incompressible high tiers. Zeros fastest and fast read 0.27 and 0.18, and random 0.89 and 0.89, with the zeros tree tiers this pass's largest bulk gain at 7% to 12%. On dll, fastest reads 1.05 at -49.6% size with ratio 4.35 against zstd-1's 2.19, fast 0.99 at -31.7%, balanced 1.31 at -13.0%, best 1.26 at -12.8%, opt 1.08 at -16.2% and ultra 1.12 at -16.5%.

Encode multi-thread has one remaining zstd win: json.fastest.mt8 at x1.20 inside its known noise band of 0.95 to 1.65. Every other cell is ahead or inside text.balanced's own 0.99 to 1.47 spread, and our cold-pool mt16 json.fast at 3351 MiB/s still beats zstd's warm-pool reference at 1575.

Streaming encode single-thread reads json.fast 0.95 and json.fastest 1.33, text.best 0.65, and text.fastest and fast 0.26 and 0.76 as standalone medians, because the in-pass page-dispersity collapse reproduced for the fifth consecutive pass. Json.opt at 0.95 and json.ultra at 0.98 stay wins.

Streaming encode MT8 keeps every json and text cell at or below x0.66, with text.balanced reading exactly 0.998 for parity rather than a win. Dll100 stream-mt8 leads at fast 0.50, opt 0.79 and ultra 0.99, and trails at fastest 2.39, balanced 1.34 and best 3.08, running at 84% to 93% of its own bulk-mt8 ceilings. The count_run phase fix restored the single-thread best cell without moving these, and the remaining fastest and best losses stay the open item in the [todo](../todo.md).

Decode MT is our exclusive dimension because libzstd has no MT decode. Json mt16 runs at 1.56 times our single-thread and 1.17 times the zstd stream reference, against 1.65 and 1.23 in the previous pass, with mt8 and mt16 reading level. Dll100.zst3 mt16 runs at 1.83 times single-thread and 1.34 times the reference, the strongest scaling row and a real-binary payload. Skewed mt16 reads 1.21 times single-thread and 0.95 times the reference, with its absolute band wandering ±15% across passes, and text is flat at 0.97.

The ratio sweep reads a geo-mean of +8.69% denser over 120 cells, with bulk single-thread at +1.48%, and outputs are byte-identical to every sweep since 2026-09-27. The losing cells are unchanged: json.fast MT at -0.12% to -0.14%, skewed.opt at -0.06% to -0.07%, and near-ties of 0.00% to 0.04% on skewed fast, fastest and ultra and on text.ultra MT.

The full 1-22 ladder keeps json rows 5-8 and 10-12 at x1.22 to 1.33 carrying -4.8% to -16.4% byte wins. Skewed rows 17-22 hold x0.76 to 0.93 and json rows 19-22 stay at or below 1.01. Text chain rows hold their wins at x0.43 to 0.62 for levels 5-8 and 0.34 to 0.58 for 10-12. Zeros levels 1-4 read 0.27, 0.27, 0.18 and 0.18, and random levels 1-4 read 0.84 to 0.90, while random levels 17-19 read 9292, 9035 and 9526 MiB-s at byte-exact ratio through the raw-block fast path. The remaining holes are text level 9 at x1.18 and skewed levels 13-16 at x1.11 to 2.17 at ratio parity.

Small payloads read json level 9 at x1.07, 0.90, 0.65 and 1.00 for 1K, 4K, 64K and 1M at stock-row bytes, and text level 9 keeps the swap's ratio at x1.69, 1.43, 2.78 and 1.00. The 64K cells wander a drift band across passes, with json level 3 at 1.01, 0.83 and 0.97 over the last three. Level 1 at 1.25 to 1.92 and level 3 at 0.97 to 1.32 remain the accepted trades.

## Decode single-thread

Throughput is MiB/s of raw data for bulk decode and streaming decode with 64 KiB pulls. x is our time divided by zstd's time, so values below 1 mean we are faster.

| file | bulk ours | bulk zstd | bulk x | stream ours | stream zstd | stream x |
|---|---:|---:|---:|---:|---:|---:|
| json.zst1 | 1769 | 1242 | 0.70 | 1800 | 2195 | 1.22 |
| json.zst3 | 1469 | 1118 | 0.76 | 1449 | 1872 | 1.29 |
| json.zst9 | 1762 | 1202 | 0.68 | 1708 | 2149 | 1.26 |
| json.zst19 | 2216 | 1278 | 0.58 | 2233 | 2605 | 1.17 |
| text.zst1 | 5997 | 2124 | 0.35 | 6388 | 7573 | 1.19 |
| text.zst3 | 8628 | 2330 | 0.27 | 10391 | 11107 | 1.07 |
| text.zst9 | 9263 | 2389 | 0.26 | 11470 | 12121 | 1.06 |
| text.zst19 | 9316 | 2381 | 0.26 | 11316 | 12159 | 1.07 |
| skewed.zst1 | 2342 | 1470 | 0.63 | 2586 | 2848 | 1.10 |
| skewed.zst3 | 1273 | 1012 | 0.80 | 1308 | 1537 | 1.18 |
| skewed.zst9 | 650 | 644 | 0.99 | 625 | 792 | 1.27 |
| skewed.zst19 | 2221 | 1420 | 0.64 | 2474 | 2685 | 1.09 |
| random.zst3 | 8777 | 2170 | 0.25 | 10982 | 8888 | 0.81 |
| zeros.zst3 | 11956 | 2449 | 0.21 | 12886 | 12902 | 1.00 |
| dll100.zst1 | 1178 | 963 | 0.82 | 1217 | 1506 | 1.24 |
| dll100.zst3 | 1258 | 999 | 0.79 | 1316 | 1702 | 1.29 |
| dll100.zst9 | 1560 | 1134 | 0.73 | 1609 | 2070 | 1.29 |
| dll100.zst19 | 1352 | 1043 | 0.77 | 1412 | 1791 | 1.27 |

## Encode single-thread bulk

Throughput and ratio are per shape, with checksums off on both sides. The tier column names the ladder tier and the pairing is 1, 3, 9, 13, 17 and 19 against libzstd. x is our time divided by zstd's, so values below 1 mean we are faster. The dll cells are the post-count-fix rerun.

| level | shape | ours MiB/s | ours ratio | zstd MiB/s | zstd ratio | x |
|---|---|---:|---:|---:|---:|---:|
| fastest | json | 579 | 6.39 | 846 | 6.11 | 1.46 |
| fastest | text | 12999 | 309.23 | 10489 | 308.94 | 0.81 |
| fastest | skewed | 2811 | 2.00 | 1249 | 2.00 | 0.44 |
| fastest | random | 2285 | 1.00 | 2043 | 1.00 | 0.89 |
| fastest | zeros | 49568 | 32577 | 13286 | 32171 | 0.27 |
| fastest | dll | 507 | 4.35 | 531 | 2.19 | 1.05 |
| fast | json | 450 | 5.30 | 471 | 5.29 | 1.05 |
| fast | text | 13494 | 333.03 | 7272 | 332.90 | 0.54 |
| fast | skewed | 213 | 1.92 | 225 | 1.92 | 1.06 |
| fast | random | 2212 | 1.00 | 1958 | 1.00 | 0.89 |
| fast | zeros | 49184 | 32577 | 8692 | 32171 | 0.18 |
| fast | dll | 430 | 4.97 | 424 | 3.40 | 0.99 |
| balanced | json | 158 | 7.20 | 121 | 5.95 | 0.77 |
| balanced | text | 1576 | 384.66 | 1728 | 378.41 | 1.10 |
| balanced | skewed | 1817 | 2.00 | 74 | 1.84 | 0.041 |
| balanced | random | 2292 | 1.00 | 1480 | 1.00 | 0.65 |
| balanced | zeros | 32983 | 32577 | 1748 | 32202 | 0.053 |
| balanced | dll | 115 | 5.31 | 151 | 4.62 | 1.31 |
| best | json | 29 | 6.26 | 36 | 6.10 | 1.24 |
| best | text | 1020 | 386.42 | 664 | 385.90 | 0.65 |
| best | skewed | 8 | 1.84 | 14 | 1.84 | 1.81 |
| best | random | 2229 | 1.00 | 242 | 1.00 | 0.11 |
| best | zeros | 46031 | 32577 | 788 | 32171 | 0.017 |
| best | dll | 31 | 5.35 | 39 | 4.67 | 1.26 |
| opt | json | 7 | 7.49 | 7 | 7.49 | 1.00 |
| opt | text | 826 | 410.23 | 435 | 410.11 | 0.53 |
| opt | skewed | 3 | 2.00 | 3 | 2.00 | 0.93 |
| opt | random | 2261 | 1.00 | 12 | 1.00 | 0.005 |
| opt | zeros | 45229 | 32577 | 852 | 32202 | 0.019 |
| opt | dll | 13 | 6.04 | 14 | 5.06 | 1.08 |
| ultra | json | 3 | 7.42 | 3 | 7.42 | 1.01 |
| ultra | text | 405 | 414.12 | 253 | 413.98 | 0.62 |
| ultra | skewed | 2 | 2.00 | 2 | 2.00 | 0.95 |
| ultra | random | 2273 | 1.00 | 8 | 1.00 | 0.003 |
| ultra | zeros | 44516 | 32577 | 605 | 32202 | 0.014 |
| ultra | dll | 8 | 6.32 | 9 | 5.28 | 1.12 |

The ratio verdict is unchanged from the previous sweep because outputs are byte-identical. We are denser or at parity on json and text at every level, with json.opt 9 bytes ahead, skewed wins everywhere except near-ties, random and zeros tie or win, and dll is denser at every tier: -49.6% bytes at fastest, -31.7% at fast and double digits from balanced up. Checksums on versus off on our side read json.fast 1.03 and text.fast 0.84.

## Encode multi-thread bulk

Throughput is MiB/s at 8 and 16 workers with one pass, using a cold pool per call on both sides. The x8 and x16 columns are our time divided by zstd's at the same worker count, and the ratio columns are at 16 workers.

| cell | ours mt8 | zstd mt8 | x8 | ours mt16 | zstd mt16 | x16 | ratio ours mt16 | ratio zstd mt16 |
|---|---:|---:|---:|---:|---:|---:|---:|---:|
| json.fastest | 3410 | 4057 | 1.20 | 5172 | 1773 | 0.34 | 6.34 | 6.11 |
| json.fast | 2362 | 983 | 0.42 | 3351 | 1027 | 0.31 | 5.30 | 5.31 |
| json.balanced | 425 | 200 | 0.47 | 523 | 200 | 0.38 | 7.19 | 5.95 |
| text.fastest | 18758 | 10247 | 0.55 | 16564 | 2276 | 0.14 | 309.08 | 39.78 |
| text.fast | 11496 | 2065 | 0.18 | 11371 | 2155 | 0.19 | 332.94 | 189.16 |
| text.balanced | 1139 | 1196 | 1.05 | 1095 | 1258 | 1.15 | 384.59 | 378.30 |
| skewed.fastest | 3990 | 3167 | 0.79 | 3629 | 1561 | 0.43 | 2.00 | 2.00 |
| skewed.fast | 1172 | 628 | 0.54 | 1654 | 644 | 0.39 | 1.92 | 1.92 |
| skewed.balanced | 680 | 121 | 0.18 | 751 | 121 | 0.16 | 2.00 | 1.84 |

## Streaming encode

Throughput uses 64 KiB pulls and interleaved medians, with single-thread and MT8 columns. x is our time divided by zstd's at the same mode, so below 1 means we are faster.

| cell | ST ours | ST zstd | ST x | MT8 ours | MT8 zstd | MT8 x |
|---|---:|---:|---:|---:|---:|---:|
| json.fastest | 581 | 773 | 1.33 | 3165 | 1909 | 0.60 |
| json.fast | 463 | 440 | 0.95 | 2328 | 405 | 0.17 |
| json.balanced | 87 | 105 | 1.21 | 383 | 114 | 0.30 |
| json.best | 29 | 35 | 1.18 | 80 | 52 | 0.65 |
| json.opt | 7 | 7 | 0.95 | 17 | 7 | 0.42 |
| json.ultra | 3 | 3 | 0.98 | 6 | 3 | 0.50 |
| text.fastest | 6724* | 1754 | 0.26 | 8053 | 3785 | 0.47 |
| text.fast | 6430* | 4898 | 0.76 | 5720 | 1620 | 0.28 |
| text.balanced | 1420 | 1612 | 1.14 | 988 | 982 | 1.00 |
| text.best | 1011 | 662 | 0.65 | 851 | 353 | 0.42 |
| text.opt | 759 | 418 | 0.55 | 642 | 368 | 0.57 |
| text.ultra | 389 | 253 | 0.65 | 357 | 234 | 0.66 |

The two text single-thread fast cells carry standalone medians under the established protocol, because the in-pass table read 5442 and 5391 at x0.315 and 0.918 and the same-process page-dispersity collapse reproduced for the fifth consecutive pass. The mechanism and reproducer are in the [workflow pitfalls](../pitfalls/workflow.md). Unknown-size text.fastest streaming has zstd emitting 1.84 MB at ratio 18.3 against our 108 KB at ratio 309. Dll100 stream cells are in the [matrix](matrix.md) streaming table.

## Decode multi-thread scaling

Fixtures are the same files as the single-thread table, and throughput is MiB/s at 1, 2, 4, 8 and 16 workers. The last column is zstd's single-thread streaming decode, because libzstd has no MT decode, so the scaling columns have no zstd counterpart.

| file | ours ST | mt2 | mt4 | mt8 | mt16 | zstd ST stream ref |
|---|---:|---:|---:|---:|---:|---:|
| json.zst3 | 1409 | 1824 | 2075 | 2202 | 2193 | 1880 |
| text.zst3 | 11222 | 10842 | 10831 | 10826 | 10850 | 11178 |
| skewed.zst9 | 624 | 758 | 712 | 754 | 752 | 792 |
| random.zst3 | 9700 | 9382 | 9320 | 9323 | 9323 | 8873 |
| dll100.zst3 | 1250 | 1828 | 2088 | 2216 | 2287 | 1702 |

Dll100 is measured through `files --threads`. The restart-point decode scales best on the large real-binary payload at mt16 1.83 times single-thread and 1.34 times the reference, against 1.90 and 1.36 in the previous pass. Json mt16 reads 1.56 times single-thread and 1.17 times the reference, against 1.65 and 1.23, with mt8 and mt16 reading level this pass. Skewed holds 1.21 times single-thread at 0.95 times the reference, and text is flat at 0.97.

## Compression-ratio sweep

The sweep runs `zstdx-bench ratio` on 5 shapes, 6 tiers, bulk and stream modes and single-thread and 4-worker MT, one deterministic pass per cell with checksums off. The reported delta is ours over zstd's ratio minus 1, aggregated as a geo-mean, and every cell is roundtrip-gated. The geo-mean over 120 cells is **+8.69%**. By mode it reads bulk single-thread +1.48%, bulk MT +11.08%, stream single-thread +11.43% and stream MT +11.11%. By shape it reads json +4.41%, text +40.36%, skewed +1.41%, random plus or minus zero and zeros +2.06%. Outputs are byte-identical to every sweep since 2026-09-27. The complete losing cells are json.fast MT at -0.12% to -0.14%, text.ultra MT at -0.02%, skewed.opt at -0.06% to -0.07%, skewed.ultra at -0.04%, skewed.fast MT at -0.01% to -0.03% and skewed.fastest at -0.00%. Details are in the [matrix](matrix.md) ratio section.

## Open and closed deficits

Each row names a measured deficit and its status in this pass. The x figures are wall-time ratios, so values below 1 mean we are faster.

| Deficit | Status in this pass |
|---|---|
| Small-payload level 9 | Closed on 2026-09-28. The flip was the small-src btlazy swap, now an 8 KiB head verdict with a 48-distinct bar. Json level 9 reads x1.07, 0.90, 0.65 and 1.00 at 1K, 4K, 64K and 1M on the stock row. Text keeps the swap at x1.69, 1.43, 2.78 and 1.00 carrying -0.9% against zstd-9 at 64K. The level 1 slide at 1K-4K remains the accepted trade |
| Json chain rows 5-8 and 10-12 | Closed on 2026-10-01 with the head-gated attempts diet. The rows read x1.22 to 1.33 at -4.8% to -16.4% bytes against zstd while every other corpus stays byte-identical, with the mechanism and `RowAttempts::Light` in [matchers](../perf/matchers.md). This pass holds those cells |
| text.balanced speed | Single-thread reads x1.10 bulk and 1.14 stream after the 2026-10-17 concurrent probe parse, bulk-mt8 reads 1.05 and stream-mt8 reads 0.998 for parity. The residual cold-start DUBT head cost stands, and ratio is +1.65% denser than zstd-9 |
| Streaming decode on compressible shapes | Json reads x1.17 to 1.29, skewed 1.09 to 1.27, text 1.06 to 1.19 and dll 1.24 to 1.29, each band a few percent tighter than the previous pass. The fused-loop serial chain remains, closed as a measured op-volume floor in [dec-gap](dec-gap.md) |
| json.fastest | x1.46 single-thread and 1.20 mt8, inside the cells' known spreads. The steady scan loop's branch-mispredict budget sets a realistic ceiling of about x1.3 to 1.4. Json.fast at 1.05 is near closed |
| Best-tier core | Json 1.24 and skewed 1.81, a memory-latency tree walk with its floor in the [todo](../todo.md). It caps the json.best stream single-thread cell at x1.18 |
| json opt and ultra | Closed on 2026-10-01 with hugepage-backed buffers. This pass reads x1.00 and 1.01 at ratio parity, with json.opt 9 bytes ahead, and skewed opt and ultra ahead at 0.93 and 0.95 |
| MT decode scaling | Json mt16 reads 1.56 times single-thread and 1.17 times the reference, dll100 1.83 and 1.34, skewed 1.21 and 0.95, and text is flat at 0.97. The memory-bound verdict and execute walls stand, with no lever short of overlapping execute with the verdict, a falsified class |
| dll100 balanced | x1.31 at -13.0% size, with best at 1.26 on the restored placement. The chain chase remains, with its floor in the [todo](../todo.md) and its differential attribution in [enc-dll-gap](enc-dll-gap.md). Fastest and fast are speed near-parity carrying -49.6% and -31.7% size |
| text single-thread stream fastest and fast | The in-pass page-dispersity collapse reproduced for the fifth consecutive pass at 5442 and 5391 in-pass against 6724 and 6430 standalone, at x0.26 and 0.76. It was root-caused on 2026-09-29 in the [workflow pitfalls](../pitfalls/workflow.md), where a best or L13 row preceding the pooled single-thread core scatters the big buffers' pages and THP recovers about 11%, leaving the residual unattributed. The real residue against 2026-09-19 is the unpledged-stream screen's about 4% sampling tax. Both cells still win |
| Fast-row screen tax on compressible-flat data | Random fastest and fast read x0.89 and 0.89 from the shortened bar leg plus the 1 MiB twin-scan reject, a deliberate arming-recall bound. Zeros fastest and fast read 0.27 and 0.18, and skewed chain rows read at or below 0.06. Every cell still wins |
| Ratio residues | Json.fast MT at -0.12% to -0.14%, skewed.opt at -0.07% as a near-tie, and the dictionary small-payload parse on the encoding side |
| dll100 streaming-MT encode | Stream-mt8 trails libzstd-mt8 at three of six tiers: fastest x2.39, balanced 1.34 and best 3.08, leads at fast 0.50, opt 0.79 and ultra 0.99 parity, and runs at 84% to 93% of its own bulk-mt8 ceilings of 1049, 964, 167, 29, 36 and 21 MiB/s. The libzstd-MT losses are the open item in the [todo](../todo.md), measured-out classes of per-job whole-window LDM cold fills plus dll's content spread, while zstd's 2127 MiB/s headline buys 48 MB of output against our 24 MB, so the two run at different output sizes |
| Placement-phase regressions | New class closed in-pass on 2026-10-25: the nbSeq emit fix shifted the dubt count loop onto a 64-byte line boundary for a 10% dll100.best loss at identical instruction counts. The R32 two-copy entry stub in `count_run` now pins the loop head at a phase of 31 or less, following `extend_match_aligned`. The full walk is in [matchers](../perf/matchers.md) |
