# Bench Matrix (2026-10-30)

This page holds the raw measurement tables for the 2026-10-30 pass. Each section states what the table measures and how to read its columns. Throughout the page `x = ours_time / zstd_time`, so values below 1 mean we are faster, throughput is MiB/s of raw input, and ratio is input bytes divided by output bytes. The comparison target is libzstd 1.5.7 with `zstdmt` enabled. Conclusions are in [snapshot.md](snapshot.md), and earlier archives from the 2026-10-25, 10-18, 10-01, 09-28, 09-27 and 09-19 passes live in this file's git history.

## Run provenance

The pass ran at commit `f7680db7` on 2026-10-30 from a clean tree in one continuous session, covering the encode window since the v0.3.0 release pass at `49f48b20`. The machine is an AMD Eng Sample 100-000000870-32_Y, a Zen4-class part with 32 visible cores, AVX-512 and BMI2, a maximum clock of 5386 MHz, and the same machine as every pass since 09-12. The toolchain is rustc 1.100.0-nightly (8925ea358 2026-08-20) in the release profile with codegen-units set to 1, unchanged from the 2026-10-25 pass, and the harness header prints `libzstd 1.5.7, binding 10507`. The corpus is unchanged, with json.raw from 2026-09-15 and the other four shapes from 2026-09-07, and `bench/big/dll100.*` is the same 2026-09-19 set with dll100.raw at 104857600 bytes. Workspace tests were green before the pass, roundtrip verification was on for every cell, and all runs were under `flock /root/programs/fork/zstd-bench.lock`.

Four encoder commits in the window moved the measured cells, and the rest of the tree kept its bytes. The gated dense geometry `e94ce0f2` scans the json-class dense body at a 2^13 table, moving json fastest to x1.34 bulk and 1.10 mt8 at 0.34% denser output. The balanced row substitution `13d76d43` runs text-class and dll-class heads on the row twin at libzstd's L9 geometry, moving text balanced to 0.95 bulk at an unchanged 384.66 ratio and dll balanced to 1.12 at ratio 5.37 against 5.31. The wide-head donation stand-down `834f6ba3` lifts the bulk-mt cells, with text.balanced.mt8 moving 1.05 to 0.85 and mt16 1.15 to 0.81. The btlazy SIMD arming `1f208e1d` arms the long compare on single-thread best rows by parse feedback, holding dll best at x1.22 bulk and 1.16 stream at byte-identical output. The mid-band dictionary rows `9ea40553` and the empty-frame clamp `396e2291` touch dictionary frames and CLI edges off this corpus.

The commands ran through `cargo run --release -p zstdx-bench` with `dec-st` at 64 KiB pulls, `enc-st` chunked per shape with dll100 through `--file bench/big/dll100.raw`, `enc-mt` at workers 8 and 16 in two runs, `enc-stream` at workers 8 with the standalone-median rerun for text fastest and fast, and `ratio` at 141 s. The MT decode scaling table, the full 1-22 ladder and the small-payload tables carry the 2026-10-25 pass, because their rows sat still in this window with json level 1 the one exception, its sizes 0.34% denser through the dense geometry. The timing sections ran between 11:57 and 12:44, the ratio sweep ran in a resumed session afterwards, and every log carried its footer before the docs were written.

## Decode single-thread

Throughput is MiB/s of raw data for bulk and streaming decode with 64 KiB pulls. x is our time divided by zstd's time, so below 1 means we are faster, and the harness spread is at most ±0.01 on every cell.

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

Bulk holds 17 of 18 wins with skewed.zst9 at x0.99 as the near-tie. The stream cells read level with the 2026-10-25 pass, with json 1.17 to 1.32 against its 1.17 to 1.29, dll 1.25 to 1.31 against its 1.24 to 1.29, and random at 0.81. Zeros bulk reads 10288 against the release pass's 11956, and that cell is the free noise-calibration loop with zero code-path overlap, so it reads this session's machine as slower.

## Decode multi-thread scaling

This table carries the 2026-10-25 measurement, because the decode surfaces sat still in this window. Throughput is MiB/s at 1, 2, 4, 8 and 16 workers, and libzstd has no MT decode, so the reference column is its single-thread streaming cell from the decode table. The dll row comes from `files --threads`, the solo tool at the same budget.

| file | ours ST | zstd stream ST ref | mt2 | mt4 | mt8 | mt16 |
|---|---:|---:|---:|---:|---:|---:|
| json.zst3 | 1409 | 1880 | 1824 | 2075 | 2202 | 2193 |
| text.zst3 | 11222 | 11178 | 10842 | 10831 | 10826 | 10850 |
| skewed.zst9 | 624 | 792 | 758 | 712 | 754 | 752 |
| random.zst3 | 9700 | 8873 | 9382 | 9320 | 9323 | 9323 |
| dll100.zst3 | 1250 | 1702 | 1828 | 2088 | 2216 | 2287 |

Json mt16 runs at 1.56 times single-thread and 1.17 times the reference. Dll100 mt16 runs at 1.83 times single-thread and 1.34 times the reference, so the strongest scaling row holds its band. Skewed reads 1.21 times single-thread and 0.95 times the reference with its band wandering ±15% across passes, text is flat at 0.97, and random is flat at 0.96 times single-thread and 1.05 times the reference.

## Encode single-thread bulk

Throughput and ratio are per shape with checksums off on both sides, and the tier column names the ladder tier. The pairing is libzstd levels 1, 3, 9, 13, 17 and 19, and x is our time divided by zstd's, so below 1 means we are faster. The dll row uses the 100 MB ELF-concat payload `bench/big/dll100.raw` from `bench/gen_big.sh`, whose ratios are payload sizes rather than the 32 MiB corpus.

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

The window's movers stand out against the release pass: json fastest 1.46 to 1.34 at 0.34% denser bytes, text balanced 1.10 to 0.95 at an unchanged ratio, dll balanced 1.31 to 1.12 at 5.31 to 5.37 and dll best 1.26 to 1.22 on the armed long compare. The dll fast cell crosses to a win at 0.97 against 0.99, json best reads 1.29 against 1.24 inside its spread, and every other band reproduces. Checksums on versus off on our side read json.fast 1.00 and text.fast 0.85.

## Encode multi-thread bulk

Throughput is MiB/s from one pass with a cold pool per call on both sides, and the ratio columns are at 16 workers. The x8 and x16 columns are our time divided by zstd's at the same worker count, so below 1 means we are faster.

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

Json.fastest.mt8 at x1.10 is the one zstd MT win, inside the cell's known noise band of 0.95 to 1.65 across passes and improved from 1.20 by the dense geometry. Text.balanced moved from 1.05 and 1.15 to 0.85 and 0.81 through the wide-head donation stand-down, and every other cell is ahead. Our cold-pool mt16 json.fast at 3274 MiB/s still beats zstd's warm-pool reference at 1594 this run.

## Encode streaming

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

The two text single-thread fast cells carry standalone medians under the established protocol. The in-pass table read 5424 and 5370 at x0.320 and 0.930, the same-process page-dispersity collapse reproduced for the sixth consecutive pass, and the mechanism and reproducer are in the [workflow pitfalls](../pitfalls/workflow.md). Unknown-size text.fastest streaming has zstd emitting 1.84 MB at ratio 18.3 against our 108 KB at ratio 309. Text.balanced stream-mt8 reads x1.05 this pass against the release pass's 0.998, inside the cell's 0.94 to 1.18 spread and the one stream-mt cell above the line. Stream-mt8 against our own bulk-mt8 ceilings reads json 88%, 75%, 87%, 99%, 123% and 120%, and text 41%, 42%, 64%, 101%, 111% and 102%.

The dll100 stream cells read single-thread fastest 499 against 509 at x1.02, fast 446 against 396 at 0.89, balanced 118 against 139 at 1.18, best 32 against 37 at 1.16, opt 13 against 14 at 1.05 and ultra 8 against 9 at 1.07, with best carrying the armed long compare and balanced the substituted row. Stream-mt8 reads fastest 956 against 2143 at x2.26, fast 818 against 435 at 0.53, balanced 134 against 199 at 1.48, best 27 against 85 at 3.20, opt 30 against 25 at 0.83 and ultra 17 against 18 at 1.04 near-parity. The cells run at 75% to 96% of the bulk-mt8 ceilings of 1045, 960, 178, 28, 34 and 21 MiB/s, with best the highest share at 96% and balanced the lowest at 75%, and the fastest and best losses stay the open item in the [todo](../todo.md).

## Compression-ratio sweep

The sweep runs `zstdx-bench ratio` on 5 shapes, 6 tiers, bulk and stream modes and single-thread and 4-worker MT, one deterministic pass per cell with checksums off. The delta is ours over zstd's ratio minus 1, aggregated as a geo-mean, and every cell is roundtrip-gated. The wall time is 141 s.

The geo-mean over 120 cells is **+8.70%**. By mode it reads bulk single-thread **+1.49%**, bulk MT **+11.09%**, stream single-thread **+11.44%** and stream MT **+11.12%**. By shape it reads json +4.46%, text +40.36%, skewed +1.41%, random plus or minus zero and zeros +2.06%.

The four json fastest cells are the only cells that moved against the 2026-10-25 sweep, each 0.34% denser through the dense geometry, and every other cell is byte-identical. The complete losing cells are json.fast bulk MT at -0.14% and stream MT at -0.12%, text.ultra bulk MT and stream MT at -0.02%, skewed.opt at -0.06% to -0.07%, skewed.ultra at -0.04%, skewed.fast bulk MT at -0.01% and stream MT at -0.03%, and skewed.fastest at -0.00%. Everything else is at parity or denser, random ties byte-exact at every cell, text.balanced reads +1.65% to +1.71%, and json.opt bulk single-thread is 9 bytes ahead of zstd-17.

## Full numeric ladder

This section carries the 2026-10-25 pass. Json level 1 is the one row the window moved, its sizes 0.34% denser through the dense geometry and its speed to this pass's x1.34 enc-st reading, and every other row is current. Both sides run at the same numeric level with a per-side budget of 500 ms, and the cell text reads ours/zstd throughput followed by the x ratio. Cells whose three minimum rounds already cover the 500 ms budget stop at n=3, which affects json levels 6-22 and skewed 4 and 13-22, so those absolutes carry the widest drift band. The rest accumulate n of at least 4, with text at 5 or more, random at 36 or more and zeros at 504 or more. x is our time divided by zstd's, so below 1 means we are faster.

### Ladder speed

| level | json | text | skewed | random | zeros |
|---|---|---|---|---|---|
| 1 | 578/850 x1.47 | 12850/10440 x0.81 | 2854/1257 x0.44 | 2269/2036 x0.90 | 48985/13285 x0.27 |
| 2 | 541/657 x1.22 | 12862/10265 x0.80 | 2781/858 x0.31 | 2286/2049 x0.90 | 49166/13273 x0.27 |
| 3 | 464/478 x1.04 | 13546/7263 x0.54 | 211/224 x1.07 | 2281/1905 x0.84 | 49126/8683 x0.18 |
| 4 | 422/434 x1.03 | 12134/6938 x0.57 | 151/165 x1.09 | 2259/1942 x0.86 | 48786/8626 x0.18 |
| 5 | 199/263 x1.32 | 7390/4591 x0.62 | 1996/126 x0.063 | 2377/1703 x0.72 | 47587/6289 x0.13 |
| 6 | 145/187 x1.29 | 5839/2746 x0.47 | 1989/106 x0.053 | 2365/1682 x0.71 | 45887/2768 x0.061 |
| 7 | 125/165 x1.33 | 4873/2602 x0.53 | 1664/99 x0.060 | 2359/1613 x0.68 | 45058/2749 x0.061 |
| 8 | 99/126 x1.27 | 4086/1748 x0.43 | 1653/82 x0.050 | 2403/1634 x0.68 | 44400/1772 x0.040 |
| 9 | 156/120 x0.77 | 1410/1659 x1.18 | 1860/75 x0.040 | 2204/1400 x0.64 | 32894/1749 x0.053 |
| 10 | 75/90 x1.20 | 3146/1549 x0.49 | 1343/61 x0.045 | 2334/1251 x0.54 | 42087/1688 x0.040 |
| 11 | 53/65 x1.23 | 2456/1424 x0.58 | 1518/48 x0.031 | 2342/1325 x0.57 | 42089/1687 x0.040 |
| 12 | 45/55 x1.22 | 2377/811 x0.34 | 1237/25 x0.020 | 2261/601 x0.27 | 43902/944 x0.021 |
| 13 | 29/36 x1.25 | 1125/662 x0.59 | 7/14 x1.83 | 2212/244 x0.11 | 46002/790 x0.017 |
| 14 | 24/27 x1.11 | 979/538 x0.56 | 7/12 x1.65 | 2175/116 x0.053 | 45612/673 x0.015 |
| 15 | 22/16 x0.74 | 919/430 x0.47 | 7/8 x1.11 | 2186/111 x0.051 | 45075/591 x0.013 |
| 16 | 8/10 x1.24 | 832/497 x0.60 | 4/8 x2.17 | 2194/21 x0.010 | 46253/1026 x0.022 |
| 17 | 7/7 x0.99 | 827/437 x0.53 | 3/3 x0.91 | 9292/12 x0.001 | 45671/861 x0.019 |
| 18 | 4/5 x1.23 | 476/365 x0.77 | 2/3 x1.32 | 9035/10 x0.001 | 45624/818 x0.018 |
| 19 | 3/3 x1.01 | 403/255 x0.63 | 2/2 x0.93 | 9526/8 x0.001 | 44670/611 x0.014 |
| 20 | 3/2 x0.80 | 261/194 x0.75 | 2/2 x0.76 | 2274/6 x0.003 | 44230/377 x0.009 |
| 21 | 2/2 x0.88 | 257/134 x0.52 | 2/1 x0.89 | 2290/8 x0.003 | 42675/212 x0.005 |
| 22 | 2/1 x0.52 | 243/117 x0.48 | 2/1 x0.87 | 2319/9 x0.004 | 42045/181 x0.004 |

The bands hold against the release pass: json levels 13 and 14 at 1.25 and 1.11, text levels 13-15 at 0.47 to 0.59, json levels 5-8 and 10-12 at x1.22 to 1.33 carrying their byte wins, skewed levels 13-16 at 1.11 to 2.17, and every high-tier row at or below 1.01 on json levels 17-22 and at or below 0.93 on skewed levels 17-22. Random levels 17-19 read 9292, 9035 and 9526 MiB/s at byte-exact ratio 1.00 through the raw-block fast path, while levels 20-22 keep the full ultra parse. The remaining holes are unchanged: text level 9 at x1.18 and skewed levels 13-16 speed at ratio parity.

### Ladder ratio

This table carries the 2026-10-25 pass with one row moved: json level 1 now reads 6.41 against the table's 6.39 through the dense geometry, and the sweep's json fastest cells carry the current values. The ratio table reads ours over zstd at the same numeric level, where a higher number means we are denser.

| level | json | text | skewed | random | zeros |
|---|---|---|---|---|---|
| 1 | 6.39/6.11 | 309.23/308.94 | 2.00/2.00 | 1.00/1.00 | 32577/32171 |
| 2 | 6.39/5.73 | 312.55/315.95 | 2.00/2.00 | 1.00/1.00 | 32577/32171 |
| 3 | 5.30/5.29 | 333.03/332.90 | 1.92/1.92 | 1.00/1.00 | 32577/32171 |
| 4 | 5.29/5.28 | 333.26/333.23 | 1.81/1.81 | 1.00/1.00 | 32577/32171 |
| 5 | 5.86/5.57 | 356.06/358.32 | 2.00/1.86 | 1.00/1.00 | 32577/32171 |
| 6 | 6.63/5.76 | 367.54/370.17 | 2.00/1.86 | 1.00/1.00 | 32577/32202 |
| 7 | 6.75/5.84 | 372.60/374.93 | 2.00/1.84 | 1.00/1.00 | 32577/32202 |
| 8 | 7.16/5.99 | 375.04/378.25 | 2.00/1.84 | 1.00/1.00 | 32577/32202 |
| 9 | 7.20/5.95 | 384.66/378.41 | 2.00/1.84 | 1.00/1.00 | 32577/32202 |
| 10 | 7.18/6.03 | 378.68/381.88 | 2.00/1.84 | 1.00/1.00 | 32577/32202 |
| 11 | 7.25/6.08 | 380.76/383.83 | 2.00/1.84 | 1.00/1.00 | 32577/32202 |
| 12 | 7.25/6.08 | 380.79/383.85 | 2.00/1.84 | 1.00/1.00 | 32577/32202 |
| 13 | 6.26/6.10 | 386.42/385.90 | 1.84/1.84 | 1.00/1.00 | 32577/32171 |
| 14 | 6.32/6.13 | 388.47/388.11 | 1.84/1.84 | 1.00/1.00 | 32577/32171 |
| 15 | 6.32/6.16 | 389.32/388.95 | 1.84/1.84 | 1.00/1.00 | 32577/32171 |
| 16 | 7.15/7.10 | 409.94/406.09 | 2.00/2.00 | 1.00/1.00 | 32577/32202 |
| 17 | 7.49/7.49 | 410.23/410.11 | 2.00/2.00 | 1.00/1.00 | 32577/32202 |
| 18 | 7.42/7.42 | 413.37/412.91 | 2.00/2.00 | 1.00/1.00 | 32577/32202 |
| 19 | 7.42/7.42 | 414.12/413.98 | 2.00/2.00 | 1.00/1.00 | 32577/32202 |
| 20 | 7.42/7.41 | 414.12/413.98 | 2.00/2.00 | 1.00/1.00 | 32577/32233 |
| 21 | 7.41/7.41 | 414.20/414.09 | 2.00/2.00 | 1.00/1.00 | 32577/32233 |
| 22 | 7.41/7.41 | 414.22/414.12 | 2.00/2.00 | 1.00/1.00 | 32577/32233 |

## Small-payload encode

This section carries the 2026-10-25 pass, with json level 1 sizes 0.34% denser since at 1 MiB through the dense geometry. Each cell is a per-call encode of the given size with checksums off, and x is our time divided by zstd's, so below 1 means we are faster. Sizes run 1 KiB, 4 KiB, 64 KiB and 1024 KiB for json and text at levels 1, 3 and 9.

| level | shape | 1K | 4K | 64K | 1024K |
|---|---|---:|---:|---:|---:|
| 1 | json | 1.25 | 1.41 | 1.52 | 1.39 |
| 1 | text | 1.67 | 1.60 | 1.92 | 1.58 |
| 3 | json | 1.31 | 1.14 | 0.97 | 1.14 |
| 3 | text | 1.32 | 1.10 | 1.06 | 1.02 |
| 9 | json | 1.07 | 0.90 | 0.65 | 1.00 |
| 9 | text | 1.69 | 1.43 | 2.78 | 1.00 |

Json level 9 stays on the stock row at x1.07, 0.90, 0.65 and 1.00. Text level 9 keeps the swap's bytes at x1.69, 1.43, 2.78 and 1.00. The two 64K cells wander inside their drift band across passes, json level 3 at 1.01, 0.83 and 0.97 over three passes and text level 9 at 2.76, 2.46 and 2.78. Level 1's accepted trades are unchanged, and the stock-row exemption for dictionary frames shows only in dictionary fixtures' bytes, not here.
