# Bench Matrix (2026-11-03)

This page holds the raw measurement tables for the 2026-11-03 release pass. Each section states what the table measures and how to read its columns. Throughout the page `x = ours_time / zstd_time`, so values below 1 mean we are faster, throughput is MiB/s of raw input, and ratio is input bytes divided by output bytes. The comparison target is libzstd 1.5.7 with `zstdmt` enabled. Conclusions are in [snapshot.md](snapshot.md), and earlier archives from the 2026-11-02 R89, 2026-11-01 R86, 2026-10-31, 10-30, 10-25, 10-18, 10-01, 09-28, 09-27 and 09-19 passes live in this file's git history.

## Run provenance

The pass ran at commit `b1cccf79` on the R93 tree, from 2026-11-03 12:46 through 13:17, driven by `bash bench/pass.sh all` split into seven locked segments with no other bench session in between, through one release build at a summed section wall of 31.4 minutes. The machine is an AMD Eng Sample 100-000000870-32_Y, a Zen4-class part with 32 visible cores, AVX-512 and BMI2, a maximum clock of 5386 MHz, and the same machine as every pass since 09-12. The toolchain is rustc 1.100.0-nightly (8925ea358 2026-08-20) in the release profile with codegen-units set to 1, and the harness header prints `libzstd 1.5.7, binding 10507`. The corpus is unchanged, with json.raw from 2026-09-15 and the other four shapes from 2026-09-07, and `bench/big/dll100.*` is the same 2026-09-19 set with dll100.raw at 104857600 bytes. The workspace suite, the paranoid belt, the ffi C harness and the i686 paranoid suite were green at this commit before the pass, roundtrip verification was on for every cell, and the section logs live in `bench/scratch/pass-r94-rel`.

The harness is the R86 one unchanged. The enc-st and enc-stream single-thread cells run two at a time, so the header reads `st cells parallel 2`; asymmetric cells stop once the slow side spent the full budget and the fast side an eighth of it; and every timed encode cell gates on its own last round. Absolute MiB/s on those shared cells reads below the serial readings of every pre-R86 table, so cross-pass comparisons on those cells use x, and a verdict on a cell in the 10 GB/s class needs a `--parallel 1` rerun, as the [methodology](methodology.md) page records. The dec-st section stays serial.

Ninety-eight commits landed in the window since the R89 pass. The decoder carries the whole post-R89 sequence-loop program, which the R89 tables predate: the loop structure itself, its port into MT stage B, one-shot block bodies parsed in place off the input slice, one-shot literals staged inside the output buffer, the X2 Huffman hot stretch as the amd64 asm loop shape, the X1 rank-by-rank table fill, and stage-A workers leased from a parked pool. The encoder window has two landed movers on the deep MT rows: the ahead-of-time serial LDM feed for the opt jobs, which moved the twelve deep bulk-mt and stream-mt cells of the ratio sweep by at most 0.06 percent, and the unpledged stream grid's deep LDM band, which moved dll100 best stream-mt8 from 22,328,775 to 20,515,621 B at a ceiling the stream cell now reaches 95 percent of. Every single-thread bulk size and every ladder size is identical to R89. The FFI window landed the reference `decompressStream` stage machine as a port, outside the timed paths here.

Determinism held everywhere the bytes must not move: the ratio sweep reproduced the R89 sizes on 108 of 120 cells with the twelve movers named above, worst json.fast.bulk-mt at -0.14% and best text.fastest.stream-st at +1594.10%, the ladder and dll single-thread sizes read identical to R89, and the small-payload cells read identical bytes.

## Decode single-thread

Throughput is MiB/s of raw data for bulk and streaming decode with 64 KiB pulls. x is our time divided by zstd's time, so below 1 means we are faster, and the harness spread is at most ±0.02 on every cell.

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

This is the first release pass whose tables carry the libzstd-shaped sequence loop and the literals-side rounds behind it. Against the R89 bulk cells, json moved 1.27, 1.38, 1.33 and 1.19 to 1.02, 0.99, 0.98 and 1.00, text moved 1.08, 1.00, 0.99 and 0.98 to 0.93, 0.96, 0.97 and 0.98, skewed moved 1.33, 1.29, 1.35 and 1.33 to 0.98, 1.00, 0.92 and 1.03, and dll100 moved 1.33, 1.38, 1.36 and 1.34 to 1.08, 1.03, 1.03 and 1.02. The stream cells read x0.93 to 1.05 on the corpus shapes. Dll100 keeps a 1.02 to 1.08 band on both paths, with its attribution and the closed lumps in [dec-gap](dec-gap.md).

## Decode multi-thread scaling

Throughput is MiB/s at 1, 2, 4, 8 and 16 workers, and libzstd has no MT decode, so the reference column is its single-thread streaming cell. The dll row comes from `files --threads`, the solo tool at the same budget, with its reference taken from this pass's `dll100.zst3` streaming cell.

| file | ours ST | zstd stream ST ref | mt2 | mt4 | mt8 | mt16 |
|---|---:|---:|---:|---:|---:|---:|
| json.zst3 | 2008 | 1873 | 2491 | 2764 | 2993 | 2970 |
| text.zst3 | 12046 | 11185 | 11707 | 11692 | 11726 | 11750 |
| skewed.zst9 | 979 | 797 | 979 | 977 | 943 | 968 |
| random.zst3 | 9752 | 8978 | 9459 | 9501 | 9486 | 9508 |
| dll100.zst3 | 1694 | 1624 | 2340 | 2780 | 2958 | 2973 |

The stage-B executor now runs the ported loop shape, so the single-thread column rose and every scaling ratio reads against the faster base. Json mt16 runs at 1.48 times single-thread and 1.59 times the reference, dll100 mt16 at 1.75 and 1.83, so the strongest row widened its lead. Skewed reads 0.99 times single-thread at 1.21 times the reference, text is flat at 0.98, and random is flat at 0.97 times single-thread and 1.06 times the reference. The remaining walls are sequence-read and aggregate-bandwidth bound, tracked as todo 1 in the [todo](../todo.md).

## Encode single-thread bulk

Throughput and ratio are per shape with checksums off on both sides, and the tier column names the ladder tier. The pairing is libzstd levels 1, 3, 9, 13, 17 and 19, and x is our time divided by zstd's, so below 1 means we are faster. The dll row uses the 100 MB ELF-concat payload `bench/big/dll100.raw` from `bench/gen_big.sh`, whose ratios are payload sizes rather than the 32 MiB corpus. These cells share the machine in pairs, so the absolute MiB/s reads below the serial tables and x is the comparable column.

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

No single-thread cell moved its bytes this window, so every delta against R89 is wall noise at identical output. The one visible mover is text.best at 0.69 against R89's 0.85, which settles on the serial band the R89 `--parallel 1` rerun already measured at 0.69. Json reads fastest 1.31, fast 1.05 and balanced 0.76 at +21% ratio, text holds fastest 0.84, fast 0.57 and balanced 0.94, and random and zeros win at every tier. The best rows stay the n=3 wander band, with json.best at 1.33 and skewed.best at 1.61 against R89's 1.37 and 1.56. The dll deep rows read opt 0.93 and ultra 1.02 against R89's 0.89 and 1.03, and dll.fast reads 1.01 against the serial band's 0.97, the pair-sharing tax. Checksums on versus off on our side read json.fast 1.03 and text.fast 0.84.

## Encode multi-thread bulk

Throughput is MiB/s from one pass with a cold pool per call on both sides, and the ratio columns are at 16 workers. The x8 and x16 columns are our time divided by zstd's at the same worker count, so below 1 means we are faster.

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

Json.fastest.mt8 at x1.11 is the one zstd MT win, inside the cell's known noise band of 0.95 to 1.65 across passes, and its mt16 cell wins at 0.32. Every other cell is ahead, with text.balanced at 0.90 on both counts and skewed.balanced at 0.19 and 0.18. Our cold-pool mt16 json.fast at 3381 MiB/s still beats zstd's warm-pool reference at 1593 this run.

## Encode streaming

Throughput uses 64 KiB pulls and interleaved medians, with single-thread and MT8 columns. x is our time divided by zstd's at the same mode, so below 1 means we are faster. The single-thread cells share the machine in pairs, so x is the comparable column and the absolute MiB/s reads below the serial tables.

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

The two text single-thread fast cells carry in-pass readings at 5065 and 5016 MiB/s, and the same-process page-dispersity collapse reproduced for the tenth consecutive pass, with the mechanism and reproducer in the [workflow pitfalls](../pitfalls/workflow.md); a verdict on either cell needs the standalone rerun protocol, and the standalone medians read 6504 and 6021 MiB/s at x0.27 and 0.86 against R89's 6638 and 6082 at 0.26 and 0.85. The rerun needs a dropped page cache to read its clean session: back-to-back runs after the decode sweep read both sides about 25 percent low, zstd included, at stable x. Unknown-size text.fastest streaming has zstd emitting 1.84 MB at ratio 18.3 against our 108 KB at ratio 309. The deep json rows read opt 0.94 and ultra 1.00 at unchanged bytes, and the deep mt rows carry the serial LDM feed's bytes: json.opt.stream-mt8 reads 0.27 and json.ultra.stream-mt8 0.27 against R89's 0.38 and 0.48, with the feed's size deltas at -15.7 KB and +17 KB.

The dll100 stream cells read single-thread fastest 473 against 500 at x1.05, fast 426 against 390 at 0.92, balanced 111 against 132 at 1.18, best 27 against 29 at 1.10, opt 13 against 11 at 0.85 and ultra 8 against 8 at 1.00. Stream-mt8 reads fastest 935 against 1971 at x2.12, fast 809 against 410 at 0.51, balanced 174 against 200 at 1.14, best 55 against 83 at 1.49, opt 49 against 25 at 0.51 and ultra 28 against 18 at 0.64. The unpledged deep band landed this window, so the cells run at 89%, 81%, 91%, 95%, 96% and 90% of the bulk-mt8 ceilings of 1051, 994, 191, 58, 51 and 31 MiB/s, with best at 55 MiB/s against R89's 27 and its bytes down from 22.33 to 20.52 MB. The remaining zstd wins are the non-iso-ratio fastest row, where zstd's 1971 MiB/s headline buys 48 MB of output against our 24 MB, and the balanced row at 1.14.

## Compression-ratio sweep

The sweep runs `zstdx-bench ratio` on 5 shapes, 6 tiers, bulk and stream modes and single-thread and 4-worker MT, one deterministic pass per cell with checksums off. The delta is ours over zstd's ratio minus 1, aggregated as a geo-mean, and every cell is roundtrip-gated. The wall time is 113.8 s at parallel 8.

The geo-mean over 120 cells is **+8.70%**. By mode it reads bulk single-thread **+1.49%**, bulk MT **+11.10%**, stream single-thread **+11.44%** and stream MT **+11.12%**. By shape it reads json +4.46%, text +40.37%, skewed +1.41%, random plus or minus zero and zeros +2.06%.

One hundred eight cells reproduce the R89 sizes byte for byte. The twelve movers are the opt and ultra bulk-mt and stream-mt cells on json, text and skewed, the serial LDM feed and the strip admission stride of this window, each within 0.06 percent: json opt and text opt read denser, skewed opt denser, json ultra and skewed ultra slightly larger, text ultra denser. The complete losing cells are json.fast bulk MT at -0.14% and stream MT at -0.12%, json.ultra MT at -0.03%, skewed.opt at -0.06%, skewed.ultra at -0.04% to -0.05%, skewed.fast bulk MT at -0.01% and stream MT at -0.03%, and skewed.fastest at -0.00%. Everything else is at parity or denser, random ties byte-exact at every cell, text.balanced reads +1.65% to +1.71%, and json.opt bulk single-thread is 9 bytes ahead of zstd-17.

## Full numeric ladder

The cell text reads ours/zstd throughput followed by the x ratio. Cells whose three minimum rounds already cover the 500 ms budget stop at n=3, which affects json levels 6-22 and skewed 4-22, so those absolutes carry the widest drift band and share the machine in pairs. The asymmetric-cell cap stops random levels 17-22 at n of 5 to 8. x is our time divided by zstd's, so below 1 means we are faster. Every ratio cell reproduces the R89 bytes.

### Ladder speed

| level | json | text | skewed | random | zeros |
|---|---|---|---|---|---|
| 1 | 630/837 x1.33 | 9062/7214 x0.82 | 2623/1202 x0.46 | 1579/1484 x0.93 | 47640/13243 x0.28 |
| 2 | 559/669 x1.20 | 11537/9697 x0.85 | 2565/845 x0.33 | 2068/1965 x0.94 | 47865/13186 x0.28 |
| 3 | 449/469 x1.05 | 12008/7019 x0.58 | 205/222 x1.08 | 2017/1905 x0.94 | 46796/8583 x0.18 |
| 4 | 418/442 x1.05 | 10403/6588 x0.63 | 151/163 x1.08 | 1984/1863 x0.94 | 45008/8554 x0.19 |
| 5 | 208/263 x1.27 | 6822/4497 x0.66 | 2132/127 x0.059 | 2082/1621 x0.78 | 44701/6304 x0.14 |
| 6 | 149/187 x1.25 | 5422/2694 x0.50 | 1997/107 x0.053 | 2087/1609 x0.77 | 46058/2782 x0.060 |
| 7 | 123/161 x1.31 | 4589/2555 x0.56 | 1999/100 x0.050 | 2198/1643 x0.74 | 45584/2758 x0.060 |
| 8 | 98/123 x1.26 | 3838/1726 x0.45 | 1830/82 x0.045 | 2207/1641 x0.74 | 44901/1763 x0.039 |
| 9 | 148/116 x0.77 | 1828/1665 x0.92 | 1847/72 x0.039 | 2198/1397 x0.64 | 31875/1740 x0.054 |
| 10 | 67/83 x1.23 | 3093/1525 x0.50 | 1581/56 x0.036 | 2192/997 x0.46 | 43591/1677 x0.039 |
| 11 | 49/56 x1.16 | 2479/1407 x0.57 | 1698/35 x0.020 | 2199/1144 x0.52 | 42235/1684 x0.040 |
| 12 | 43/48 x1.12 | 1834/577 x0.32 | 1449/21 x0.014 | 1550/436 x0.30 | 41799/929 x0.022 |
| 13 | 23/31 x1.34 | 988/666 x0.67 | 5/8 x1.69 | 2048/122 x0.063 | 44073/781 x0.018 |
| 14 | 18/22 x1.27 | 866/543 x0.62 | 6/8 x1.49 | 2141/76 x0.036 | 41859/652 x0.016 |
| 15 | 18/14 x0.78 | 824/424 x0.52 | 6/6 x1.10 | 2159/73 x0.034 | 42638/570 x0.013 |
| 16 | 7/9 x1.28 | 873/485 x0.56 | 3/5 x1.54 | 2145/13 x0.006 | 41957/1012 x0.023 |
| 17 | 5/6 x0.99 | 864/431 x0.50 | 3/2 x0.68 | 2183/9 x0.004 | 42155/841 x0.020 |
| 18 | 3/4 x1.16 | 488/356 x0.73 | 2/2 x1.18 | 3449/8 x0.002 | 41820/795 x0.019 |
| 19 | 3/3 x0.99 | 423/248 x0.59 | 2/2 x0.90 | 3492/6 x0.002 | 39071/592 x0.015 |
| 20 | 3/2 x0.83 | 281/194 x0.69 | 2/1 x0.75 | 2181/6 x0.003 | 34802/357 x0.010 |
| 21 | 2/2 x0.86 | 276/138 x0.50 | 1/1 x0.86 | 2180/7 x0.003 | 31592/203 x0.007 |
| 22 | 2/1 x0.51 | 273/121 x0.44 | 1/1 x0.86 | 2172/9 x0.004 | 28166/173 x0.006 |

The deep json rows sit at or under parity as in every pass since R86: levels 17 and 19 read 0.99, level 20 at 0.83 and level 22 at 0.51 carry the byte wins, and level 18 at 1.16 is the one exception, with levels 13 and 14 at 1.34 and 1.27 holding their byte wins. Text keeps its chain-row wins at 0.32 to 0.85 through levels 12-22, with level 9 at 0.92. The skewed hole at levels 13-16 reads 1.10 to 1.69 on ratio parity, with skewed 17-22 at 0.68 to 1.18. Random levels 17-19 read 2183 to 3492 MiB/s at byte-exact ratio 1.00 through the raw-block fast path, while levels 20-22 keep the full ultra parse. The n=3 cells still wander the widest, with json level 15 at 0.78 and skewed level 13 at 1.69 the largest moves inside that band against R89's 0.73 and 1.64.

### Ladder ratio

Every cell reproduces the R89 bytes, and the table is unchanged with it. The ratio table reads ours over zstd at the same numeric level, where a higher number means we are denser.

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
| 16 | 7.15/7.10 | 409.94/406.09 | 2.00/2.00 | 1.00/1.00 | 32577/32202 |
| 17 | 7.49/7.49 | 410.23/410.11 | 2.00/2.00 | 1.00/1.00 | 32577/32202 |
| 18 | 7.42/7.42 | 413.37/412.91 | 2.00/2.00 | 1.00/1.00 | 32577/32202 |
| 19 | 7.42/7.42 | 414.12/413.98 | 2.00/2.00 | 1.00/1.00 | 32577/32202 |
| 20 | 7.42/7.41 | 414.12/413.98 | 2.00/2.00 | 1.00/1.00 | 32577/32233 |
| 21 | 7.41/7.41 | 414.20/414.09 | 2.00/2.00 | 1.00/1.00 | 32577/32233 |
| 22 | 7.41/7.41 | 414.22/414.12 | 2.00/2.00 | 1.00/1.00 | 32577/32233 |

## Small-payload encode

Each cell is a per-call encode of the given size with checksums off, and x is our time divided by zstd's, so below 1 means we are faster. Sizes run 1 KiB, 4 KiB, 64 KiB and 1024 KiB for json and text at levels 1, 3 and 9.

| level | shape | 1K | 4K | 64K | 1024K |
|---|---|---:|---:|---:|---:|
| 1 | json | 1.29 | 1.42 | 1.50 | 1.34 |
| 1 | text | 1.66 | 1.63 | 1.91 | 1.54 |
| 3 | json | 1.28 | 1.12 | 0.98 | 1.14 |
| 3 | text | 1.40 | 1.06 | 1.08 | 1.03 |
| 9 | json | 1.07 | 0.93 | 0.66 | 1.01 |
| 9 | text | 1.78 | 1.62 | 2.86 | 1.02 |

The level-3 row improved across the board this window at identical bytes, with json 4K at 1.12 against R89's 1.31 and text 4K at 1.06 against 1.24, and json level 9 at 4K crossed to 0.93 against R89's 1.09. Json level 9 stays on the stock row and text level 9 keeps the swap's bytes at x1.78, 1.62, 2.86 and 1.02. The 64K cells keep their drift bands, json level 3 at 0.98, 0.83, 0.97, 0.99, 0.98, 1.02 and 0.98 over seven passes and text level 9 at 2.76, 2.46, 2.78, 2.80, 2.86, 2.86 and 2.86. Level 1's accepted trades are unchanged, and the stock-row exemption for dictionary frames shows only in dictionary fixtures' bytes, not here.
