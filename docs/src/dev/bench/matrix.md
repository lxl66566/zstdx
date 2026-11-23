# Bench Matrix (2026-11-23)

This page holds the raw measurement tables for the 2026-11-23 release pass. Each section states what the table measures and how to read its columns. Throughout the page `x = ours_time / zstd_time`, so values below 1 mean we are faster, throughput is MiB/s of raw input, and ratio is input bytes divided by output bytes. The comparison target is libzstd 1.5.7 with `zstdmt` enabled. Conclusions are in [snapshot.md](snapshot.md), and earlier archives from the 2026-11-21 R110, 2026-11-16 R109, 2026-11-07 R104, 2026-11-03 R94, 2026-11-02 R89, 2026-11-01 R86, 2026-10-31, 10-30, 10-25, 10-18, 10-01, 09-28, 09-27 and 09-19 passes live in this file's git history.

## Run provenance

The pass ran on the v0.7.1 release tree, commit `6b1483bc` plus the dependency upgrade and the priceset cfg fix that ride this release, driven by `bash bench/pass.sh all` one section per invocation under the shared machine lock, with no other bench session in between. The machine is an AMD Eng Sample 100-000000870-32_Y, a Zen4-class part with 32 visible cores, AVX-512 and BMI2, a maximum clock of 5386 MHz, and the same machine as every pass since 09-12. The toolchain is rustc 1.102.0-nightly (5f4effd57 2026-11-13), unchanged from R110, in the release profile with codegen-units set to 1, and the harness header prints `libzstd 1.5.7, binding 10507`. The dependency upgrade moved the zstd crate from 0.13.5 to 0.14.4 with clap 4.6 and gungraun 0.20 alongside; zstd-sys stays at 2.1.3 and bundles the same libzstd 1.5.7, and the ratio sweep below reproduces every R110 size byte for byte on both sides, so the reference side is byte-flat through the binding move. The corpus is unchanged, with json.raw from 2026-09-15 and the other four shapes from 2026-09-07, and `bench/big/dll100.*` is the same 2026-09-19 set with dll100.raw at 104857600 bytes. The workspace, paranoid, no-default-features and ffi C-harness suites were green on the release tree before the pass — 818 workspace tests plus 25 doctests, 622 paranoid tests, the no-default-features suite restored by the priceset call sites learning their cfg, and the ffi C harness against the pinned reference tree — and roundtrip verification was on for every cell.

The ten daily sections summed to 706.2 s of wall, with enc-st at 230.6 s and enc-stream at 269.3 s the heavy pair, and the ratio section ran its stock parallel-16 schedule clean in 89.7 s with no earlyoom pressure this session, against R110's parallel-4 run at 172.4 s. The five ladder shapes followed in three further invocations at a summed 652.5 s, with skewed the heaviest at 328.9 s.

The harness is unchanged from R110, the R86 one plus the adaptive sample floor: the enc-st and enc-stream single-thread cells run two at a time, so the header reads `st cells parallel 2`; a warmup round pair at eight times the budget runs twice instead of three; asymmetric cells stop once the slow side spent the full budget and the fast side one sixteenth of it; and every timed encode cell gates on its own last round. Absolute MiB/s on those shared cells reads below the serial readings of every pre-R86 table, so cross-pass comparisons on those cells use x, and a verdict on a cell in the 10 GB/s class needs a `--parallel 1` rerun, as the [methodology](methodology.md) page records. The dec-st section stays serial. Three side runs feed the README figures beside the pass: `matrix --mode dec-st --pull 4k,16k,64k,256k,1m` over the corpus plus the four dll100 `.zst` files, `matrix --mode enc-stream --mt-workers 8` over the corpus, and the same command with `--file bench/big/dll100.raw`.

Four commits landed in the window since the R110 pass and the v0.7.0 cut, three of them the opt-tier performance family this release carries: the tree walks' per-compare diet (fbb14c72), the DP core's per-position diet (75972110) and the AVX-2 vectorized ultra variant scan (6b1483bc), with the seed-local diet falsified on record (90825af1). The release tree adds the dependency upgrade, the priceset call-site cfg that restores the no-std build, and the version bump. Determinism held everywhere the bytes must hold: the ratio sweep reproduces all 120 R110 cells byte for byte with the zstd reference side byte-flat, the full 110-cell ladder reproduces R110 on both sides, and the dll100 bulk anchors match R110's documented bytes at every tier.

## Decode single-thread

Throughput is MiB/s of raw data for bulk and streaming decode with 64 KiB pulls. x is our time divided by zstd's time, so below 1 means we are faster, and the harness spread is at most ±0.02 on every cell.

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

Every corpus shape reads at or under parity bulk except skewed.zst19 at 1.02, with skewed.zst9 the widest win at 0.80, and the stream column holds 0.91 to 1.04 with random's 0.81 the widest win. The dll100 bulk band reads 0.93 to 1.01 against R110's 0.93 to 1.01, so the packed-FSE-baseline gain held through the binding move, and the stream band tightens to 0.96 to 1.01. The pull-sweep side run reads dll100 streaming at 0.96 to 1.05 across every pull size. The zstd side's standing drift below its R104 absolutes on the json, skewed and dll100 cells persists through both R110 and this pass while our side held, so the x cells on those shapes carry that drift. The multi-frame payload decodes ahead single-threaded on both paths; its parallel story is the section below.

## Decode multi-thread scaling

Throughput is MiB/s at 1, 2, 4, 8 and 16 workers, and libzstd has no MT decode, so the reference column is its single-thread streaming cell. The dll row comes from `files --threads`, the solo tool at the same budget, with its reference taken from this pass's `dll100.zst3` streaming cell.

| file | ours ST | zstd stream ST ref | mt2 | mt4 | mt8 | mt16 |
|---|---:|---:|---:|---:|---:|---:|
| json.zst3 | 1458 | 1705 | 2645 | 3070 | 3179 | 3187 |
| text.zst3 | 11852 | 11019 | 11602 | 11451 | 11608 | 11586 |
| skewed.zst9 | 960 | 695 | 1365 | 1615 | 1733 | 1552 |
| random.zst3 | 9675 | 8988 | 9444 | 9546 | 9554 | 9061 |
| dll100.zst3 | 1713 | 1602 | 2460 | 2896 | 3040 | 3074 |

Json mt16 runs at 1.87 times the reference, dll100 mt16 at 1.92, so the strongest single-frame rows hold or gain, and json's own-ST multiple reads 2.19 this pass against R110's 1.66 because this section's json st1 cell read 1458 against dec-st's 2099, a low draw of that solo cell rather than an MT gain. Skewed reads 1.80 times single-thread and 2.49 times the reference at mt8 against R110's 1.69 and 2.34, with mt16 settling at 1.62 against R110's 1.60. Text stays flat at 0.98 and random at 0.94 to 0.99 times single-thread. The remaining walls are sequence-read and aggregate-bandwidth bound, tracked as todo 1 in the [todo](../todo.md).

The files-multi section times the four multi-frame payloads at every worker count, the lane budget's home ground:

| payload | ST | mt2 | mt4 | mt8 | mt16 | mt16 / ST |
|---|---:|---:|---:|---:|---:|---:|
| multi.zst3 | 3409 | 4401 | 6029 | 9131 | 9833 | 2.88 |
| multi_small.zst3 | 2500 | 2701 | 4316 | 5914 | 7109 | 2.84 |
| multi_balanced.zst3 | 2492 | 3319 | 4289 | 6077 | 8068 | 3.24 |
| multi_twodom.zst3 | 3412 | 4389 | 4509 | 4784 | 5428 | 1.59 |

Multi Balanced at mt16 runs 3.24 times its single-thread decode, multi 2.88 and multi_small 2.84, against the single-frame corpus cells' 0.94 to 1.80 band, so the lane budget's staging split holds at release scale. The two-domain payload scales at 1.59, its second frame's restart-poor half setting the pace, which is the R101 twodom pricing's predicted shape.

## Encode single-thread bulk

Throughput and ratio are per shape with checksums off on both sides, and the tier column names the ladder tier. The pairing is libzstd levels 1, 3, 9, 13, 17 and 19, and x is our time divided by zstd's, so below 1 means we are faster. The dll row uses the 100 MB ELF-concat payload `bench/big/dll100.raw` from `bench/gen_big.sh`, whose ratios are payload sizes rather than the 32 MiB corpus. These cells share the machine in pairs, so the absolute MiB/s reads below the serial tables and x is the comparable column.

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

The opt-tier family is this table's story: every opt and ultra cell improved against R110 at byte-identical output, with json.opt at 0.82 against 0.91, json.ultra at 0.97 against 1.09, text.opt at 0.45 against 0.48, text.ultra at 0.50 against 0.61, dll100.opt at 0.83 against 0.93 and dll100.ultra at 0.93 against 1.05. The balanced row reads healthy in-pass this pass with no layout-lottery carry: json.balanced at 0.78 against R110's solo-median 0.78, text.balanced at 0.89 against the in-pass 1.19 with R110's solo median at 0.88, and the fast rows stay at their healthy band. The best rows keep the n=3 wander class with json.best at 1.41 against R110's 1.38 and skewed.best at 1.53 against 1.52. The dll rows keep the landed geometry at byte-identical sizes: balanced at 19,570,844 B against zstd's 22,701,602, 13.8 percent smaller, fastest and fast at 49.8 and 31.8 percent smaller. Checksums on versus off on our side read json.fast 1.02 and text.fast 0.85.

## Encode multi-thread bulk

Throughput is MiB/s from one pass with a cold pool per call on both sides, and the ratio columns are at 16 workers. The x8 and x16 columns are our time divided by zstd's at the same worker count, so below 1 means we are faster.

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

Json.fastest.mt8 at x1.16 stays the one zstd MT win, inside the cell's known noise band of 0.95 to 1.65 across passes, and its mt16 cell wins at 0.33. Every other cell is ahead, with text.balanced reading 0.88 and 0.87 against R110's 0.97 and 0.96, and skewed.balanced at 0.20 and 0.18. Our cold-pool mt16 json.fast at 3326 MiB/s still beats zstd's warm-pool reference at 1625 this run against R110's 1608.

## Encode streaming

Throughput uses 64 KiB pulls and interleaved medians, with single-thread and MT8 columns. x is our time divided by zstd's at the same mode, so below 1 means we are faster. The single-thread cells below come from the dll-paired enc-stream side run, this session's healthy pairing; the MT8 cells come from the corpus section, whose MT cells run serial either way.

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

The corpus pairing of the pass's own enc-stream section read its fast rows low on both sides together, text.fastest at 5344 against the dll-paired 6749 and json.fastest at 494 against 609, with the zstd side down 22 to 30 percent as well, a pairing-level condition rather than the one-sided pooled-layout lottery of the pitfalls record. The dll-paired run read every cell at its healthy level against R110, text.fastest at 6749 against 6812 and json.fastest at 609 against 612, so the table carries the dll-paired cells and no solo rerun was needed. The balanced row read healthy in both pairings, text at 1506 and 1490, so R110's solo-median carry closes. The opt-tier family reads through the whole grid: json.opt at 0.81 against R110's 0.89, json.ultra at 0.93 against 1.06, text.opt at 0.47 against 0.50, text.ultra at 0.51 against 0.62, and the MT8 column gains likewise with json.opt at 0.22 against 0.24, json.ultra at 0.28 against 0.30, text.opt at 0.48 against 0.51 and text.ultra at 0.78 against 0.93.

The dll100 stream cells read single-thread fastest 473 against 502 at x1.05, fast 429 against 394 at 0.92, balanced 138 against 129 at 0.94, best 28 against 31 at 1.11, opt 14 against 11 at 0.79 and ultra 9 against 8 at 0.93, every tier at or under R110's x with fast and balanced flipping to wins. Stream-mt8 reads fastest 916 against 1916 at x2.05, fast 845 against 426 at 0.51, balanced 173 against 195 at 1.13, best 52 against 81 at 1.57, opt 53 against 24 at 0.46 and ultra 27 against 17 at 0.63, with balanced improving from R110's in-pass 1.42 toward its solo 1.16. The cells run at 92, 95, 84, 95, 95 and 90 percent of the bulk-mt8 ceilings of 999, 889, 205, 55, 56 and 30 MiB/s, and best keeps its deep-band output at 19.57 MB single-threaded. The remaining zstd win is the non-iso-ratio fastest row, where zstd's 1916 MiB/s headline buys 48 MB of output against our 24 MB, so the two run at different output sizes. Json.fastest.stream-mt8 holds at x0.49.

## Compression-ratio sweep

The sweep runs `zstdx-bench ratio` on 5 shapes, 6 tiers, bulk and stream modes and single-thread and 4-worker MT, one deterministic pass per cell with checksums off. The delta is ours over zstd's ratio minus 1, aggregated as a geo-mean, and every cell is roundtrip-gated. The wall time is 89.7 s at the stock parallel 16.

The geo-mean over 120 cells is **+8.70%**. By mode it reads bulk single-thread **+1.47%**, bulk MT **+11.08%**, stream single-thread **+11.46%** and stream MT **+11.11%**. By shape it reads json +4.48%, text +40.29%, skewed +1.42%, random plus or minus zero and zeros +2.06%.

All 120 cells reproduce the R110 sizes byte for byte and the zstd reference side is byte-flat through the binding move, so the window's opt-tier commits moved no corpus byte. The losing set is unchanged from R110 at the same bytes: text.opt at -0.29 to -0.31 percent across its modes, json.opt at -0.11 to -0.13, json.fast bulk MT at -0.14% and stream MT at -0.12%, skewed.opt at -0.06% and skewed.ultra at -0.03 to -0.05%. Everything else is at parity or denser, random ties byte-exact at every cell, and text.balanced reads +1.65 to +1.71%.

## Full numeric ladder

The cell text reads ours/zstd throughput followed by the x ratio. Cells whose three minimum rounds already cover the 500 ms budget stop at n=3, which affects json levels 6-22 and skewed 4-22, so those absolutes carry the widest drift band and share the machine in pairs. The asymmetric-cell cap stops random levels 17-22 at n of 3 to 5. x is our time divided by zstd's, so below 1 means we are faster. Every ladder size is byte-identical to R110 on both sides.

### Ladder speed

| level | json | text | skewed | random | zeros |
|---|---|---|---|---|---|
| 1 | 622/829 x1.33 | 10069/7837 x0.81 | 1930/852 x0.45 | 2032/1895 x0.93 | 45679/13197 x0.29 |
| 2 | 510/631 x1.24 | 11845/9838 x0.83 | 2581/856 x0.33 | 2097/1949 x0.93 | 45254/13119 x0.29 |
| 3 | 439/466 x1.06 | 12145/7035 x0.58 | 202/221 x1.09 | 1992/1860 x0.94 | 48171/8570 x0.18 |
| 4 | 447/448 x1.00 | 10480/6600 x0.63 | 152/163 x1.07 | 1942/1811 x0.94 | 47741/8504 x0.18 |
| 5 | 194/260 x1.34 | 6916/4530 x0.66 | 2205/127 x0.06 | 2067/1600 x0.78 | 46609/6311 x0.14 |
| 6 | 142/185 x1.30 | 5298/2530 x0.48 | 2017/107 x0.05 | 2156/1637 x0.75 | 44480/2766 x0.06 |
| 7 | 124/161 x1.30 | 4573/2402 x0.53 | 1959/100 x0.05 | 2191/1582 x0.73 | 45670/2743 x0.06 |
| 8 | 99/124 x1.25 | 3871/1618 x0.42 | 1854/82 x0.04 | 2190/1633 x0.75 | 44098/1758 x0.04 |
| 9 | 147/114 x0.78 | 1804/1616 x0.89 | 1767/72 x0.04 | 2179/1390 x0.64 | 32775/1737 x0.05 |
| 10 | 64/83 x1.30 | 3046/1440 x0.47 | 1624/56 x0.04 | 2207/1083 x0.49 | 42012/1680 x0.04 |
| 11 | 48/58 x1.18 | 2385/1383 x0.58 | 1703/37 x0.02 | 2220/1118 x0.50 | 43462/1680 x0.04 |
| 12 | 41/48 x1.19 | 1961/611 x0.31 | 1349/21 x0.01 | 2131/511 x0.24 | 43483/917 x0.02 |
| 13 | 24/31 x1.31 | 939/581 x0.60 | 5/7 x1.38 | 2105/117 x0.06 | 44309/632 x0.01 |
| 14 | 17/22 x1.28 | 877/485 x0.55 | 6/8 x1.51 | 2092/76 x0.04 | 41468/546 x0.01 |
| 15 | 17/13 x0.77 | 833/382 x0.46 | 5/6 x1.12 | 2117/73 x0.03 | 40292/481 x0.01 |
| 16 | 8/8 x0.99 | 950/480 x0.51 | 3/5 x1.68 | 2103/11 x0.00 | 39883/1000 x0.03 |
| 17 | 7/5 x0.76 | 944/421 x0.45 | 3/2 x0.71 | 2145/9 x0.00 | 43989/813 x0.02 |
| 18 | 3/4 x1.04 | 540/360 x0.67 | 2/2 x1.09 | 3434/8 x0.00 | 32755/789 x0.02 |
| 19 | 3/3 x0.91 | 496/247 x0.50 | 2/1 x0.87 | 3386/6 x0.00 | 35899/595 x0.02 |
| 20 | 3/2 x0.74 | 308/195 x0.63 | 2/1 x0.77 | 2226/6 x0.00 | 32816/361 x0.01 |
| 21 | 2/2 x0.83 | 309/136 x0.43 | 1/1 x0.87 | 2187/7 x0.00 | 27164/208 x0.01 |
| 22 | 2/1 x0.50 | 327/121 x0.37 | 1/1 x0.90 | 2198/9 x0.00 | 39811/179 x0.00 |

The deep json rows moved onto the opt-tier family's gains at the cut's unchanged bytes: levels 17 and 19 read 0.76 and 0.91 against R110's 0.90 and 1.09, level 22 at 0.50 against 0.63, level 20 at 0.74 against 0.84, level 15 at 0.77 against 0.84, and level 16 at 0.99 against 1.09, with the best-tier rows 13 and 14 holding theirs at 1.31 and 1.28. Text keeps its chain-row wins and widens them at the deep end: levels 12-22 read 0.31 to 0.67, with level 13 at 0.60 against R110's 0.77, level 19 at 0.50 against 0.58 and level 22 at 0.37 against 0.46, and the balanced level 9 cell reads 0.89 in-pass against R110's collapsed 1.35, back at R109's healthy 0.89. The skewed hole at levels 13-16 reads 1.12 to 1.68 at ratio parity or better, with skewed 17-22 at 0.71 to 1.09 improving R110's 0.79 to 1.24 band. Random levels 17-19 read 2145 to 3434 MiB/s at byte-exact ratio 1.00 through the raw-block fast path.

Cross-pass absolute moves on the deep rows stay inside the wander the methodology records for the n=2 and n=3 class, and this window's deep gains line up cell for cell with the three opt-tier commits rather than scattering, with the enc-st and enc-stream sections reading the same family at their cells.

### Ladder ratio

The ratio table reads ours over zstd at the same numeric level, where a higher number means we are denser. Every row reproduces the R110 bytes.

| level | json | text | skewed | random | zeros |
|---|---:|---:|---:|---:|---:|
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
|---|---:|---:|---:|---:|---:|
| 1 | json | 1.28 | 1.43 | 1.50 | 1.34 |
| 1 | text | 1.79 | 1.64 | 1.93 | 1.55 |
| 3 | json | 1.36 | 1.17 | 1.04 | 1.16 |
| 3 | text | 1.41 | 1.15 | 1.11 | 1.05 |
| 9 | json | 1.11 | 0.95 | 0.67 | 1.02 |
| 9 | text | 1.74 | 1.55 | 2.83 | 1.01 |

The cells sit under the dump gates at deterministic sizes and the walls hold their bands, with json level 3 at 4K at 1.17 and text at 1.15 against R110's 1.15 and 1.15, and json level 9 at 4K at 0.95 against 0.94. Text level 9 keeps the swap's bytes at x1.74, 1.55, 2.83 and 1.01, and its 64K cell reads 2.83 for the sixth consecutive pass inside the 2.76 to 2.86 spread. Level 1's accepted trades are unchanged.
