# Bench Matrix (2026-11-01)

This page holds the raw measurement tables for the 2026-11-01 R86 fast pass. Each section states what the table measures and how to read its columns. Throughout the page `x = ours_time / zstd_time`, so values below 1 mean we are faster, throughput is MiB/s of raw input, and ratio is input bytes divided by output bytes. The comparison target is libzstd 1.5.7 with `zstdmt` enabled. Conclusions are in [snapshot.md](snapshot.md), and earlier archives from the 2026-10-31, 10-30, 10-25, 10-18, 10-01, 09-28, 09-27 and 09-19 passes live in this file's git history.

## Run provenance

The pass ran at commit `9fe6a721` on 2026-11-01 from the merged R86 tree in one continuous session under `flock /root/programs/fork/zstd-bench.lock`, driven by `bash bench/pass.sh all` through one release build at a total wall of 32.3 minutes. The machine is an AMD Eng Sample 100-000000870-32_Y, a Zen4-class part with 32 visible cores, AVX-512 and BMI2, a maximum clock of 5386 MHz, and the same machine as every pass since 09-12. The toolchain is rustc 1.100.0-nightly (8925ea358 2026-08-20) in the release profile with codegen-units set to 1, and the harness header prints `libzstd 1.5.7, binding 10507`. The corpus is unchanged, with json.raw from 2026-09-15 and the other four shapes from 2026-09-07, and `bench/big/dll100.*` is the same 2026-09-19 set with dll100.raw at 104857600 bytes. Workspace tests were green before the pass, roundtrip verification was on for every cell, and the section logs live in `bench/scratch/pass-r86-final`.

This is the first pass on the R86 harness. The enc-st and enc-stream single-thread cells run two at a time, so the header reads `st cells parallel 2`; asymmetric cells stop once the slow side spent the full budget and the fast side an eighth of it; and every timed encode cell gates on its own last round. Absolute MiB/s on those shared cells reads below the serial readings of every earlier table, with json.opt at 6 MiB/s against the serial 7 class, so cross-pass comparisons on enc-st and enc-stream single-thread cells use x only. A verdict on a cell in the 10 GB/s class needs a `--parallel 1` rerun, as the [methodology](methodology.md) page records. The dec-st section stays serial and its bulk cell carries the reference-side fix `7817e052`: the zstd bulk reference now reuses its Decompressor and output buffer, so the earlier tables' bulk wins partly measured the reference's per-call allocation, and the corrected bulk x now agrees with the stream column.

Three encoder commits in the window moved cells. The flat price-table mirrors `0c53c87b` with the head-slot prefetch `b4d90620` move the opt rows: dll100.opt from 1.09 to 0.919, dll100.ultra from 1.13 to 1.032, json.opt to 0.985 and json.ultra to 1.006, and the same rows' stream cells follow, with dll100.opt.stream at 0.872 and its stream-mt8 at 0.667; the prefetch's locked A/B numbers are in [matchers](../perf/matchers.md). The btlazy driver rep0 skip `a6baa167` moves json.best from 1.29 to 1.229 and dll100.best from 1.22 to 1.201, with json.best.stream-mt8 at 0.642.

Determinism held everywhere the bytes must not move: the ratio sweep reproduced the 2026-10-30 sizes cell for cell at a geo-mean of +8.70%, worst json.fast.bulk-mt at -0.14% and best text.fastest.stream-st at +1594.10%, and the dump belt read byte-identical in both directions.

## Decode single-thread

Throughput is MiB/s of raw data for bulk and streaming decode with 64 KiB pulls. x is our time divided by zstd's time, so below 1 means we are faster, and the harness spread is at most ±0.02 on every cell.

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

With the reference-side buffer reuse in place the bulk column agrees with the streaming column for the first time: every compressible shape reads x1.00 to 1.38 on both paths, with json.zst3 at bulk 1.38 against stream 1.31, text.zst3 bulk at 1.00 and dll100.zst3 bulk at 1.38. The earlier tables' 17-of-18 bulk wins came from the reference's per-call output allocation, which cost its bulk path 34% to 76% of the time per shape. Text bulk now sits at parity with 0.99 to 1.08, random wins both paths at 0.99 bulk and 0.80 stream, zeros reads 0.92 bulk against 1.00 stream, and the stream cells read level with the 2026-10-31 refresh, with json 1.17 to 1.31 against its 1.17 to 1.32 and dll 1.25 to 1.32 against its 1.25 to 1.31. The residual gap on both paths is the fused decode loop's op volume, measured in [dec-gap](dec-gap.md) and [mzstd-dec](../review/mzstd-dec.md).

## Decode multi-thread scaling

Throughput is MiB/s at 1, 2, 4, 8 and 16 workers, and libzstd has no MT decode, so the reference column is its single-thread streaming cell. The dll row comes from `files --threads`, the solo tool at the same budget, with its reference taken from this pass's `dll100.zst3` streaming cell.

| file | ours ST | zstd stream ST ref | mt2 | mt4 | mt8 | mt16 |
|---|---:|---:|---:|---:|---:|---:|
| json.zst3 | 1429 | 1877 | 1856 | 2016 | 2189 | 2223 |
| text.zst3 | 11075 | 11203 | 10972 | 10883 | 10896 | 10878 |
| skewed.zst9 | 647 | 788 | 756 | 755 | 758 | 752 |
| random.zst3 | 9715 | 9024 | 9505 | 9499 | 9507 | 9506 |
| dll100.zst3 | 1266 | 1708 | 1839 | 2041 | 2204 | 2255 |

Json mt16 runs at 1.55 times single-thread and 1.18 times the reference. Dll100 mt16 runs at 1.78 times single-thread and 1.32 times the reference, so the strongest scaling row holds its band. Skewed reads 1.16 times single-thread and 0.95 times the reference with its band wandering ±15% across passes, text is flat at 0.98, and random is flat at 0.98 times single-thread and 1.05 times the reference.

## Encode single-thread bulk

Throughput and ratio are per shape with checksums off on both sides, and the tier column names the ladder tier. The pairing is libzstd levels 1, 3, 9, 13, 17 and 19, and x is our time divided by zstd's, so below 1 means we are faster. The dll row uses the 100 MB ELF-concat payload `bench/big/dll100.raw` from `bench/gen_big.sh`, whose ratios are payload sizes rather than the 32 MiB corpus. These cells share the machine in pairs, so the absolute MiB/s reads below the serial tables and x is the comparable column.

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

The window's movers are the opt rows and the best rows: the price-table mirrors with the head-slot prefetch take json.opt to 0.985, json.ultra to 1.006, dll100.opt to 0.919 and dll100.ultra to 1.032, the first deep-tier dll win, and the rep0 skip takes json.best to 1.229, dll100.best to 1.201 and skewed.best to 1.58 from 1.85. Json reads fastest 1.36, fast 1.08 and balanced 0.76 at +21% ratio, text holds fastest 0.85, fast 0.58 and balanced 0.94, and random and zeros win at every tier. The dll fast-class cells read 1.13, 1.02 and 1.17 against the serial band's 1.04, 0.97 and 1.12, the pair-sharing tax those rows pay alongside their bandwidth-heavy neighbors, while carrying -49.6% bytes at fastest, -31.7% at fast and -14.0% at balanced. Checksums on versus off on our side read json.fast 1.03 and text.fast 0.85.

## Encode multi-thread bulk

Throughput is MiB/s from one pass with a cold pool per call on both sides, and the ratio columns are at 16 workers. The x8 and x16 columns are our time divided by zstd's at the same worker count, so below 1 means we are faster.

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

Json.fastest.mt8 at x1.12 is the one zstd MT win, inside the cell's known noise band of 0.95 to 1.65 across passes, and its mt16 cell wins at 0.31. Every other cell is ahead, with text.balanced at 0.86 and 0.89 and skewed.balanced at 0.18 and 0.17. Our cold-pool mt16 json.fast at 3313 MiB/s still beats zstd's warm-pool reference at 1596 this run.

## Encode streaming

Throughput uses 64 KiB pulls and interleaved medians, with single-thread and MT8 columns. x is our time divided by zstd's at the same mode, so below 1 means we are faster. The single-thread cells share the machine in pairs, so x is the comparable column and the absolute MiB/s reads below the serial tables.

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

The two text single-thread fast cells carry in-pass readings at 5067 and 5076 MiB/s, and the same-process page-dispersity collapse reproduced for the eighth consecutive pass, with the mechanism and reproducer in the [workflow pitfalls](../pitfalls/workflow.md); a verdict on either cell needs the standalone rerun protocol. Unknown-size text.fastest streaming has zstd emitting 1.84 MB at ratio 18.3 against our 108 KB at ratio 309. The opt rows carry the window's movers: json.opt.stream reads 0.94 and dll100.opt.stream 0.87, and json.best.stream-mt8 reads 0.642 on the rep0 skip. Stream-mt8 against our own bulk-mt8 ceilings reads json 85%, 93%, 87%, 193%, 129% and 120%, and text 41%, 45%, 94%, 95%, 112% and 108%, with the json best and opt rows above their bulk ceilings through the burst pipeline's overlap.

The dll100 stream cells read single-thread fastest 461 against 501 at x1.04, fast 427 against 389 at 0.91, balanced 108 against 131 at 1.21, best 26 against 30 at 1.12, opt 13 against 11 at 0.87 and ultra 8 against 8 at 0.98, with best improved from 1.26 through the rep0 skip and opt from 1.16 through the price-table movers. Stream-mt8 reads fastest 936 against 2019 at x2.14, fast 817 against 425 at 0.52, balanced 167 against 181 at 1.09, best 27 against 84 at 3.16, opt 37 against 25 at 0.67 and ultra 20 against 18 at 0.90. The cells run at 61% to 91% of the bulk-mt8 ceilings of 1033, 978, 194, 44, 41 and 24 MiB/s, with best the lowest share at 61%, and the fastest and best losses stay the open item in the [todo](../todo.md).

## Compression-ratio sweep

The sweep runs `zstdx-bench ratio` on 5 shapes, 6 tiers, bulk and stream modes and single-thread and 4-worker MT, one deterministic pass per cell with checksums off. The delta is ours over zstd's ratio minus 1, aggregated as a geo-mean, and every cell is roundtrip-gated. The wall time is 120.7 s at parallel 8.

The geo-mean over 120 cells is **+8.70%**. By mode it reads bulk single-thread **+1.49%**, bulk MT **+11.09%**, stream single-thread **+11.44%** and stream MT **+11.12%**. By shape it reads json +4.46%, text +40.36%, skewed +1.41%, random plus or minus zero and zeros +2.06%.

Every cell reproduced the 2026-10-30 sizes byte for byte, so the cell-level story is unchanged. The complete losing cells are json.fast bulk MT at -0.14% and stream MT at -0.12%, text.ultra bulk MT and stream MT at -0.02%, skewed.opt at -0.06% to -0.07%, skewed.ultra at -0.04%, skewed.fast bulk MT at -0.01% and stream MT at -0.03%, and skewed.fastest at -0.00%. Everything else is at parity or denser, random ties byte-exact at every cell, text.balanced reads +1.65% to +1.71%, and json.opt bulk single-thread is 9 bytes ahead of zstd-17.

## Full numeric ladder

The cell text reads ours/zstd throughput followed by the x ratio. Cells whose three minimum rounds already cover the 500 ms budget stop at n=3, which affects json levels 6-22 and skewed 4-22, so those absolutes carry the widest drift band and share the machine in pairs. The asymmetric-cell cap stops random levels 17-22 at n of 5 to 8, where the 2026-10-31 refresh accumulated 35 to 139 pairs on the same cells. x is our time divided by zstd's, so below 1 means we are faster. Every ratio cell reproduces the 2026-10-25 bytes, with json level 1 the one moved row at 0.34% denser through the dense geometry.

### Ladder speed

| level | json | text | skewed | random | zeros |
|---|---|---|---|---|---|
| 1 | 632/840 x1.33 | 12804/10320 x0.81 | 2594/1192 x0.46 | 1984/1859 x0.94 | 45246/13165 x0.29 |
| 2 | 524/635 x1.21 | 11751/9847 x0.84 | 2571/845 x0.33 | 1989/1870 x0.94 | 44913/13140 x0.29 |
| 3 | 442/470 x1.06 | 12867/7205 x0.56 | 207/221 x1.07 | 1916/1787 x0.93 | 44642/8556 x0.19 |
| 4 | 404/431 x1.06 | 11304/6846 x0.61 | 151/164 x1.09 | 1911/1786 x0.94 | 45324/8538 x0.19 |
| 5 | 199/265 x1.33 | 7430/4575 x0.62 | 2082/127 x0.061 | 1987/1545 x0.78 | 45686/6291 x0.14 |
| 6 | 143/186 x1.30 | 5920/2740 x0.46 | 1960/106 x0.054 | 1988/1536 x0.77 | 45928/2781 x0.061 |
| 7 | 124/161 x1.28 | 4716/2580 x0.55 | 1931/99 x0.051 | 2013/1502 x0.74 | 45794/2756 x0.060 |
| 8 | 101/125 x1.24 | 3901/1730 x0.44 | 1705/82 x0.048 | 2109/1569 x0.74 | 44453/1769 x0.040 |
| 9 | 149/115 x0.76 | 1824/1731 x0.95 | 1810/71 x0.040 | 2091/1340 x0.64 | 31415/1740 x0.055 |
| 10 | 65/83 x1.26 | 3070/1529 x0.50 | 1565/56 x0.036 | 2105/959 x0.46 | 43084/1677 x0.039 |
| 11 | 47/58 x1.21 | 2354/1356 x0.59 | 1684/32 x0.019 | 2098/1093 x0.52 | 41598/1680 x0.040 |
| 12 | 43/48 x1.12 | 2412/797 x0.33 | 1489/21 x0.014 | 1986/502 x0.25 | 42699/902 x0.021 |
| 13 | 22/31 x1.36 | 926/640 x0.69 | 5/8 x1.59 | 2015/110 x0.054 | 43709/770 x0.018 |
| 14 | 18/22 x1.26 | 950/545 x0.58 | 5/8 x1.49 | 2075/74 x0.036 | 41995/645 x0.015 |
| 15 | 17/13 x0.78 | 895/433 x0.48 | 5/6 x1.12 | 2042/72 x0.035 | 41785/558 x0.013 |
| 16 | 7/8 x1.27 | 884/494 x0.56 | 3/4 x1.41 | 2051/13 x0.006 | 41182/988 x0.024 |
| 17 | 5/5 x0.99 | 867/434 x0.50 | 3/2 x0.73 | 2109/9 x0.004 | 38751/814 x0.021 |
| 18 | 3/4 x1.15 | 494/353 x0.72 | 2/2 x1.03 | 3286/8 x0.002 | 39430/775 x0.019 |
| 19 | 3/3 x0.99 | 422/242 x0.57 | 2/1 x0.87 | 3392/7 x0.002 | 37484/558 x0.015 |
| 20 | 3/2 x0.80 | 280/186 x0.67 | 2/1 x0.77 | 2108/6 x0.003 | 37031/333 x0.009 |
| 21 | 2/2 x0.86 | 267/126 x0.47 | 1/1 x0.89 | 2126/7 x0.003 | 30112/179 x0.006 |
| 22 | 2/1 x0.52 | 268/112 x0.42 | 1/1 x0.84 | 2181/9 x0.004 | 42497/166 x0.004 |

The deep json rows now sit at or under parity: levels 17 and 19 read 0.99, level 20 at 0.80 and level 22 at 0.52 carry the byte wins, and level 18 at 1.15 is the one exception, with levels 13 and 14 at 1.36 and 1.26 holding their byte wins as before. Text keeps its chain-row wins at 0.33 to 0.72 through levels 12-22, with level 9 at 0.95. The skewed hole at levels 13-16 narrowed to 1.12 to 1.59 on ratio parity, from 1.26 to 2.89 in the refresh, with skewed 17-22 at 0.73 to 1.03. Random levels 17-19 read 2109 to 3392 MiB/s at byte-exact ratio 1.00 through the raw-block fast path, while levels 20-22 keep the full ultra parse. The n=3 cells still wander the widest, with json level 10 at 1.26 against the refresh's 1.45 the largest move inside that band.

### Ladder ratio

Every cell reproduces the 2026-10-25 bytes, with json level 1 the one moved row at 6.41 against 6.39 through the dense geometry. The ratio table reads ours over zstd at the same numeric level, where a higher number means we are denser.

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
| 1 | json | 1.27 | 1.42 | 1.48 | 1.34 |
| 1 | text | 1.68 | 1.63 | 1.94 | 1.56 |
| 3 | json | 1.32 | 1.17 | 0.98 | 1.16 |
| 3 | text | 1.41 | 1.12 | 1.08 | 1.03 |
| 9 | json | 1.09 | 0.93 | 0.66 | 1.01 |
| 9 | text | 1.77 | 1.57 | 2.86 | 1.02 |

Json level 9 stays on the stock row at x1.09, 0.93, 0.66 and 1.01. Text level 9 keeps the swap's bytes at x1.77, 1.57, 2.86 and 1.02. The two 64K cells keep wandering inside their drift band across passes, json level 3 at 1.01, 0.83, 0.97, 0.99 and 0.98 over five passes and text level 9 at 2.76, 2.46, 2.78, 2.80 and 2.86. Level 1's accepted trades are unchanged, and the stock-row exemption for dictionary frames shows only in dictionary fixtures' bytes, not here.
