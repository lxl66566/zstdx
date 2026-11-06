# Bench Matrix (2026-11-07)

This page holds the raw measurement tables for the 2026-11-07 release pass. Each section states what the table measures and how to read its columns. Throughout the page `x = ours_time / zstd_time`, so values below 1 mean we are faster, throughput is MiB/s of raw input, and ratio is input bytes divided by output bytes. The comparison target is libzstd 1.5.7 with `zstdmt` enabled. Conclusions are in [snapshot.md](snapshot.md), and earlier archives from the 2026-11-03 R94, 2026-11-02 R89, 2026-11-01 R86, 2026-10-31, 10-30, 10-25, 10-18, 10-01, 09-28, 09-27 and 09-19 passes live in this file's git history.

## Run provenance

The pass ran at commit `3a05e92c` on the R103 tree, on 2026-11-07, driven by `bash bench/pass.sh all` split into eight locked segments with no other bench session in between, through one release build at a summed section wall of 32.3 minutes. The machine is an AMD Eng Sample 100-000000870-32_Y, a Zen4-class part with 32 visible cores, AVX-512 and BMI2, a maximum clock of 5386 MHz, and the same machine as every pass since 09-12. The toolchain is rustc 1.100.0-nightly (8925ea358 2026-08-20), unchanged from R94, in the release profile with codegen-units set to 1, and the harness header prints `libzstd 1.5.7, binding 10507`. The corpus is unchanged, with json.raw from 2026-09-15 and the other four shapes from 2026-09-07, and `bench/big/dll100.*` is the same 2026-09-19 set with dll100.raw at 104857600 bytes. The workspace suite, the paranoid belt, the ffi C harness, the i686 and s390x paranoid suites and the all-levels bulk and stream dump gates were green at this commit before the pass, roundtrip verification was on for every cell, and the section logs live in `bench/scratch/pass-r104-rel`.

The harness is the R86 one unchanged. The enc-st and enc-stream single-thread cells run two at a time, so the header reads `st cells parallel 2`; asymmetric cells stop once the slow side spent the full budget and the fast side an eighth of it; and every timed encode cell gates on its own last round. Absolute MiB/s on those shared cells reads below the serial readings of every pre-R86 table, so cross-pass comparisons on those cells use x, and a verdict on a cell in the 10 GB/s class needs a `--parallel 1` rerun, as the [methodology](methodology.md) page records. The dec-st section stays serial. The multi-frame payloads `multi*.zst3` joined the daily set this pass through the files-multi section and the dec-st `--file` grid, so the decode tables carry their first release-pass rows.

Sixty-three commits landed in the window since the R94 pass. The encoder side carries the R95 sprint's opt family — the fill-descent cut at one compare below the search depth, the far admission tier on the feed jobs, the long-compare strip crossing and the tree-fill fold — plus the stepped finish drain, the zero-copy owned source and the NT-store feed reader on the stream side, and the balanced dll class's diet to the light attempts budget. The decoder side carries the multi-frame lane budget, the frame-lane poison surfacing, the plan-time gated head ramp and the R103 ramp-gate fix on wholesale LDM emissions. The FFI round landed 29 further reference symbols, outside the timed paths.

Determinism held everywhere the bytes must not move except at the window's three documented trades: the ratio sweep moved exactly 12 of 120 cells, the eight opt cells on json and text from the fill-descent cut inside its stated +0.5 percent bar and the four skewed.best cells from the repcode-probe gate at denser output, and the ladder moved json and text l16-l17 with skewed l13-l15 on the same two levers while random and zeros stayed byte-exact. Every other cell, the dll stream grid and the small-payload bytes reproduce the R94 sizes.

## Decode single-thread

Throughput is MiB/s of raw data for bulk and streaming decode with 64 KiB pulls. x is our time divided by zstd's time, so below 1 means we are faster, and the harness spread is at most ±0.02 on every cell.

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
| multi.zst3 | 3215 | 3167 | 0.99 | 3214 | 3199 | 1.00 |

Every corpus shape reads at or under parity bulk except json.zst1 at 1.00 and skewed.zst19 at 1.02, with skewed.zst9 the widest win at 0.90, and the stream column holds 0.93 to 1.05 with random's 0.80 the widest win. Dll100 keeps its 1.02 to 1.07 band on both paths, the FSE-build residue and the copy-loop lumps in [dec-gap](dec-gap.md). The multi-frame payload decodes at parity on both paths single-threaded; its parallel story is the section below.

## Decode multi-thread scaling

Throughput is MiB/s at 1, 2, 4, 8 and 16 workers, and libzstd has no MT decode, so the reference column is its single-thread streaming cell. The dll row comes from `files --threads`, the solo tool at the same budget, with its reference taken from this pass's `dll100.zst3` streaming cell.

| file | ours ST | zstd stream ST ref | mt2 | mt4 | mt8 | mt16 |
|---|---:|---:|---:|---:|---:|---:|
| json.zst3 | 2024 | 1875 | 2582 | 2961 | 3160 | 3191 |
| text.zst3 | 11712 | 11128 | 11532 | 11396 | 11422 | 11498 |
| skewed.zst9 | 985 | 795 | 1367 | 1608 | 1690 | 1624 |
| random.zst3 | 9751 | 8957 | 9552 | 9550 | 9663 | 9091 |
| dll100.zst3 | 1690 | 1629 | 2389 | 2861 | 3020 | 3039 |

Json mt16 runs at 1.58 times single-thread and 1.70 times the reference, dll100 mt16 at 1.80 and 1.87, so the strongest row widened further. Skewed is the window's mover: the multi-frame lane budget's plan-time staging split lifted it from R94's flat 0.99 times single-thread to 1.39 at mt2, 1.63 at mt4 and 1.72 at mt8, at 2.13 times the reference, with mt16 settling at 1.65. Text stays flat at 0.98 and random at 0.93 to 0.99 times single-thread. The remaining walls are sequence-read and aggregate-bandwidth bound, tracked as todo 1 in the [todo](../todo.md).

The files-multi section times the four multi-frame payloads at every worker count, the lane budget's home ground:

| payload | ST | mt2 | mt4 | mt8 | mt16 | mt16 / ST |
|---|---:|---:|---:|---:|---:|---:|
| multi.zst3 | 3373 | 3076 | 6075 | 8964 | 9562 | 2.84 |
| multi_small.zst3 | 2455 | 2810 | 4526 | 5954 | 7099 | 2.89 |
| multi_balanced.zst3 | 2468 | 3200 | 4054 | 6717 | 7633 | 3.09 |
| multi_twodom.zst3 | 3354 | 4337 | 4765 | 4750 | 5439 | 1.62 |

Multi BalANCED at mt16 runs 3.09 times its single-thread decode and multi 2.84, against the single-frame corpus cells' 1.0 to 1.8 band, so the lane budget's staging split holds at release scale. The two-domain payload scales at 1.62, its second frame's restart-poor half setting the pace, which is the R101 twodom pricing's predicted shape.

## Encode single-thread bulk

Throughput and ratio are per shape with checksums off on both sides, and the tier column names the ladder tier. The pairing is libzstd levels 1, 3, 9, 13, 17 and 19, and x is our time divided by zstd's, so below 1 means we are faster. The dll row uses the 100 MB ELF-concat payload `bench/big/dll100.raw` from `bench/gen_big.sh`, whose ratios are payload sizes rather than the 32 MiB corpus. These cells share the machine in pairs, so the absolute MiB/s reads below the serial tables and x is the comparable column.

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

The opt rows carry the fill-descent cut's trade: json.opt reads 0.87 in-pass and 0.855 on the serial rerun against R94's 1.01, at +0.13 percent bytes, and skewed.opt reads 0.86 in-pass and 0.79 serial against R94's 0.70, the same row-17 cut at byte-identical output. Text.best read 0.89 in-pass through pair-sharing and settled at 0.686 on the `--parallel 1` rerun, the R94 serial band. The best rows stay the n=3 wander class, with json.best at 1.46 in-pass and 1.26 serial against R94's 1.33. The dll rows read the diet's new geometry: balanced gains wall at +0.36 percent bytes for x1.11 against R94's 1.16, and opt lands at 0.89 against R94's 0.93 at +0.25 percent bytes from the cut. Checksums on versus off on our side read json.fast 1.03 and text.fast 0.94.

## Encode multi-thread bulk

Throughput is MiB/s from one pass with a cold pool per call on both sides, and the ratio columns are at 16 workers. The x8 and x16 columns are our time divided by zstd's at the same worker count, so below 1 means we are faster.

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

Json.fastest.mt8 at x1.14 stays the one zstd MT win, inside the cell's known noise band of 0.95 to 1.65 across passes, and its mt16 cell wins at 0.34. Every other cell is ahead, with text.balanced at 0.93 and 0.96 on the pair-sharing session band and skewed.balanced at 0.20 and 0.18. Our cold-pool mt16 json.fast at 3318 MiB/s still beats zstd's warm-pool reference at 1638 this run.

## Encode streaming

Throughput uses 64 KiB pulls and interleaved medians, with single-thread and MT8 columns. x is our time divided by zstd's at the same mode, so below 1 means we are faster. The single-thread cells share the machine in pairs, so x is the comparable column and the absolute MiB/s reads below the serial tables.

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

The two text single-thread fast cells carry in-pass readings at 4668 and 4644 MiB/s, and the same-process page-dispersity collapse reproduced for the eleventh consecutive pass, with the mechanism and reproducer in the [workflow pitfalls](../pitfalls/workflow.md); a verdict on either cell needs the standalone rerun protocol, and the standalone medians read 6149 and 5938 MiB/s at x0.27 and 0.83 against R94's 6504 and 6021 at 0.27 and 0.86. The rerun needs a dropped page cache to read its clean session. Unknown-size text.fastest streaming has zstd emitting 1.84 MB at ratio 18.3 against our 108 KB at ratio 309. The deep json rows read opt 0.85 at the cut's +0.13 percent bytes and ultra 1.05.

The dll100 stream cells read single-thread fastest 468 against 502 at x1.07, fast 412 against 391 at 0.96, balanced 99 against 132 at 1.35, best 25 against 30 at 1.25, opt 13 against 11 at 0.87 and ultra 7 against 8 at 1.05. Stream-mt8 reads fastest 940 against 1744 at x2.02, fast 844 against 426 at 0.51, balanced 128 against 192 at 1.45, best 55 against 85 at 1.58, opt 50 against 25 at 0.50 and ultra 27 against 18 at 0.68. The balanced row's drop from R94's 174 to 128 MiB/s is a placement reading, not a tree change: the cell runs 8 jobs of 12.5 MiB on 8 workers whose wall is the single slowest job at about 500 ms against a 60 ms fastest, so it reads one core's speed, and the two-CCD host moves that core between roughly 160 and 200 MiB/s through L3 sharing and boost state at identical instruction counts. The same-day A/B of the pre-R94 tree against this one reads 155 against 159 in the pass context with HEAD at 1.74 percent fewer instructions and identical bytes, so both passes sample the same band. The cells run at 90, 88, 71, 98, 94 and 93 percent of the bulk-mt8 ceilings of 1041, 959, 181, 56, 53 and 29 MiB/s, and best keeps its deep-band output at 20.52 MB. The remaining zstd wins are the non-iso-ratio fastest row, where zstd's 1744 MiB/s headline buys 48 MB of output against our 24 MB, and the balanced and best rows.

## Compression-ratio sweep

The sweep runs `zstdx-bench ratio` on 5 shapes, 6 tiers, bulk and stream modes and single-thread and 4-worker MT, one deterministic pass per cell with checksums off. The delta is ours over zstd's ratio minus 1, aggregated as a geo-mean, and every cell is roundtrip-gated. The wall time is 113.9 s at parallel 8.

The geo-mean over 120 cells is **+8.69%**. By mode it reads bulk single-thread **+1.47%**, bulk MT **+11.08%**, stream single-thread **+11.42%** and stream MT **+11.11%**. By shape it reads json +4.44%, text +40.29%, skewed +1.42%, random plus or minus zero and zeros +2.06%.

One hundred eight cells reproduce the R94 sizes byte for byte. The twelve movers are the window's two documented levers: the eight opt cells on json and text, every mode, from the fill-descent cut at +0.12 and +0.34 percent, and the four skewed.best cells from the repcode-probe gate at -0.022 percent, denser. The complete losing set grew by the cut's flip of json.opt from its 9-byte win to -0.12 percent and text.opt stream-mt to -0.31 percent, joining json.fast bulk MT at -0.12% and stream MT at -0.11%, skewed.opt at -0.04% to -0.05% and skewed.ultra at -0.03% to -0.04%. Everything else is at parity or denser, random ties byte-exact at every cell, text.balanced reads +1.65% to +1.71%, and skewed.best joins the winners at +0.05%.

## Full numeric ladder

The cell text reads ours/zstd throughput followed by the x ratio. Cells whose three minimum rounds already cover the 500 ms budget stop at n=3, which affects json levels 6-22 and skewed 4-22, so those absolutes carry the widest drift band and share the machine in pairs. The asymmetric-cell cap stops random levels 17-22 at n of 5 to 8. x is our time divided by zstd's, so below 1 means we are faster. Every ladder size move is one of the two window levers: json and text l16-l17 from the fill-descent cut, skewed l13-l15 from the repcode gate, all other rows byte-identical to R94.

### Ladder speed

| level | json | text | skewed | random | zeros |
|---|---|---|---|---|---|
| 1 | 619/832 x1.33 | 11978/9981 x0.84 | 1895/851 x0.46 | 2052/1916 x0.94 | 34253/9078 x0.27 |
| 2 | 519/628 x1.21 | 11627/9771 x0.84 | 2558/851 x0.33 | 2100/1972 x0.93 | 44304/13072 x0.29 |
| 3 | 441/465 x1.05 | 11781/7034 x0.60 | 207/219 x1.06 | 2028/1909 x0.93 | 44047/8563 x0.19 |
| 4 | 415/428 x1.03 | 10126/6554 x0.65 | 152/163 x1.08 | 2033/1888 x0.93 | 43843/8483 x0.19 |
| 5 | 204/264 x1.29 | 6649/4464 x0.67 | 2125/124 x0.059 | 2080/1657 x0.78 | 43264/6237 x0.14 |
| 6 | 146/187 x1.28 | 5282/2676 x0.51 | 1983/105 x0.053 | 2127/1627 x0.77 | 44481/2771 x0.062 |
| 7 | 124/163 x1.32 | 4718/2544 x0.54 | 1957/99 x0.051 | 2221/1622 x0.72 | 44115/2747 x0.062 |
| 8 | 99/125 x1.27 | 4000/1728 x0.43 | 1713/81 x0.047 | 2219/1618 x0.73 | 43089/1768 x0.041 |
| 9 | 150/113 x0.77 | 1833/1714 x0.94 | 1761/71 x0.041 | 2184/1383 x0.63 | 30869/1735 x0.056 |
| 10 | 67/85 x1.27 | 3064/1523 x0.50 | 1590/55 x0.035 | 2192/989 x0.45 | 42446/1674 x0.040 |
| 11 | 49/58 x1.18 | 2492/1414 x0.57 | 1659/35 x0.021 | 2204/1107 x0.51 | 41105/1683 x0.041 |
| 12 | 40/49 x1.21 | 2395/791 x0.33 | 1320/20 x0.015 | 2053/515 x0.25 | 41680/905 x0.022 |
| 13 | 23/30 x1.33 | 922/631 x0.69 | 5/8 x1.60 | 1974/116 x0.058 | 43002/770 x0.018 |
| 14 | 17/21 x1.28 | 775/514 x0.66 | 6/8 x1.44 | 2032/76 x0.036 | 41058/641 x0.016 |
| 15 | 16/13 x0.81 | 835/415 x0.50 | 6/6 x1.09 | 1973/73 x0.037 | 39051/576 x0.015 |
| 16 | 8/8 x1.05 | 916/485 x0.53 | 3/4 x1.44 | 2198/13 x0.006 | 39389/996 x0.025 |
| 17 | 6/6 x0.85 | 913/428 x0.47 | 2/2 x0.73 | 2181/9 x0.004 | 41013/838 x0.020 |
| 18 | 3/4 x1.13 | 492/354 x0.72 | 2/2 x1.10 | 3375/8 x0.002 | 38695/783 x0.020 |
| 19 | 3/3 x0.95 | 421/242 x0.57 | 2/1 x0.88 | 3245/6 x0.002 | 36070/590 x0.016 |
| 20 | 3/2 x0.77 | 275/188 x0.68 | 2/1 x0.78 | 2234/6 x0.002 | 31966/358 x0.011 |
| 21 | 2/2 x0.85 | 263/126 x0.48 | 1/1 x0.87 | 2181/7 x0.003 | 27874/207 x0.007 |
| 22 | 2/1 x0.53 | 266/113 x0.42 | 1/1 x0.88 | 2302/9 x0.004 | 33703/180 x0.005 |

The deep json rows widened their lead through the fill-descent cut: levels 17 and 19 read 0.85 and 0.95 against R94's 0.99 and 0.99 at +0.13 percent bytes, level 20 at 0.77 and level 22 at 0.53 keep their byte wins, and level 18 at 1.13 stays the one exception, with levels 13 and 14 at 1.33 and 1.28 holding theirs. Level 16 read the far admission tier's -98 KB at 7.30 against zstd's 7.10 and crosses to parity in wall. Text keeps its chain-row wins at 0.33 to 0.72 through levels 12-22, with level 9 at 0.94. The skewed hole at levels 13-16 reads 1.09 to 1.60 at ratio parity or better, with skewed 17-22 at 0.73 to 1.10 and the repcode gate's instructions cut behind the l13 row's byte gain. Random levels 17-19 read 2181 to 3375 MiB/s at byte-exact ratio 1.00 through the raw-block fast path. The n=3 cells still wander the widest, with json level 15 at 0.81 and skewed level 13 at 1.60 the largest moves inside that band against R94's 0.78 and 1.69.

### Ladder ratio

The ratio table reads ours over zstd at the same numeric level, where a higher number means we are denser. The movers are json and text l16-l17 from the fill-descent cut and skewed l13-l15 from the repcode gate; every other row reproduces the R94 bytes.

| level | json | text | skewed | random | zeros |
|---|---|---|---|---|---|
| 1 | 6.41/6.11 | 309.23/308.94 | 2.00/2.00 | 1.00/1.00 | 32577/32171 |
| 2 | 6.39/5.73 | 312.55/315.95 | 2.00/2.00 | 1.00/1.00 | 32577/32171 |
| 3 | 5.30/5.29 | 333.03/332.90 | 1.92/1.92 | 1.00/1.00 | 32577/32171 |
| 4 | 5.29/5.28 | 333.26/333.23 | 1.81/1.81 | 1.00/1.00 | 32577/32171 |
| 5 | 5.86/5.57 | 356.06/358.32 | 2.00/1.86 | 1.00/1.00 | 32577/32171 |
| 6 | 6.63/5.76 | 367.53/370.17 | 2.00/1.86 | 1.00/1.00 | 32577/32202 |
| 7 | 6.75/5.84 | 372.60/374.93 | 2.00/1.84 | 1.00/1.00 | 32577/32202 |
| 8 | 7.16/5.99 | 375.02/378.25 | 2.00/1.84 | 1.00/1.00 | 32577/32202 |
| 9 | 7.20/5.95 | 384.66/378.41 | 2.00/1.84 | 1.00/1.00 | 32577/32202 |
| 10 | 7.18/6.03 | 378.68/381.88 | 2.00/1.84 | 1.00/1.00 | 32577/32202 |
| 11 | 7.25/6.08 | 380.76/383.83 | 2.00/1.84 | 1.00/1.00 | 32577/32202 |
| 12 | 7.25/6.08 | 380.79/383.85 | 2.00/1.84 | 1.00/1.00 | 32577/32202 |
| 13 | 6.26/6.10 | 386.42/385.90 | 1.84/1.84 | 1.00/1.00 | 32577/32171 |
| 14 | 6.32/6.13 | 388.47/388.11 | 1.84/1.84 | 1.00/1.00 | 32577/32171 |
| 15 | 6.32/6.16 | 389.32/388.95 | 1.84/1.84 | 1.00/1.00 | 32577/32171 |
| 16 | 7.30/7.10 | 408.54/406.09 | 2.00/2.00 | 1.00/1.00 | 32577/32202 |
| 17 | 7.48/7.49 | 408.84/410.11 | 2.00/2.00 | 1.00/1.00 | 32577/32202 |
| 18 | 7.42/7.42 | 413.37/412.91 | 2.00/2.00 | 1.00/1.00 | 32577/32202 |
| 19 | 7.42/7.42 | 414.12/413.98 | 2.00/2.00 | 1.00/1.00 | 32577/32202 |
| 20 | 7.42/7.41 | 414.12/413.98 | 2.00/2.00 | 1.00/1.00 | 32577/32233 |
| 21 | 7.41/7.41 | 414.20/414.09 | 2.00/2.00 | 1.00/1.00 | 32577/32233 |
| 22 | 7.41/7.41 | 414.22/414.12 | 2.00/2.00 | 1.00/1.00 | 32577/32233 |

## Small-payload encode

Each cell is a per-call encode of the given size with checksums off, and x is our time divided by zstd's, so below 1 means we are faster. Sizes run 1 KiB, 4 KiB, 64 KiB and 1024 KiB for json and text at levels 1, 3 and 9.

| level | shape | 1K | 4K | 64K | 1024K |
|---|---|---:|---:|---:|---:|
| 1 | json | 1.26 | 1.42 | 1.50 | 1.37 |
| 1 | text | 1.74 | 1.65 | 1.96 | 1.54 |
| 3 | json | 1.31 | 1.12 | 0.97 | 1.14 |
| 3 | text | 1.39 | 1.08 | 1.07 | 1.02 |
| 9 | json | 1.08 | 0.97 | 0.66 | 1.02 |
| 9 | text | 1.67 | 1.69 | 2.88 | 1.02 |

Every cell reads identical bytes to R94 and the walls hold their bands, with json level 3 at 4K at 1.12 and text at 1.08 against R94's 1.12 and 1.06, and json level 9 at 4K at 0.97 against 0.93. Text level 9 keeps the swap's bytes at x1.67, 1.69, 2.88 and 1.02, and its 64K cell holds 2.88 for the third consecutive pass against the 2.76 to 2.86 spread before it. Level 1's accepted trades are unchanged.
