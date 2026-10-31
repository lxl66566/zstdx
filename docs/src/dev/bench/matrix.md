# Bench Matrix (2026-10-31)

This page holds the raw measurement tables for the 2026-10-30 pass with the 2026-10-31 section refresh described under Run provenance. Each section states what the table measures and how to read its columns. Throughout the page `x = ours_time / zstd_time`, so values below 1 mean we are faster, throughput is MiB/s of raw input, and ratio is input bytes divided by output bytes. The comparison target is libzstd 1.5.7 with `zstdmt` enabled. Conclusions are in [snapshot.md](snapshot.md), and earlier archives from the 2026-10-30, 10-25, 10-18, 10-01, 09-28, 09-27 and 09-19 passes live in this file's git history.

## Run provenance

The pass ran at commit `f7680db7` on 2026-10-30 from a clean tree in one continuous session, covering the encode window since the v0.3.0 release pass at `49f48b20`. The machine is an AMD Eng Sample 100-000000870-32_Y, a Zen4-class part with 32 visible cores, AVX-512 and BMI2, a maximum clock of 5386 MHz, and the same machine as every pass since 09-12. The toolchain is rustc 1.100.0-nightly (8925ea358 2026-08-20) in the release profile with codegen-units set to 1, unchanged from the 2026-10-25 pass, and the harness header prints `libzstd 1.5.7, binding 10507`. The corpus is unchanged, with json.raw from 2026-09-15 and the other four shapes from 2026-09-07, and `bench/big/dll100.*` is the same 2026-09-19 set with dll100.raw at 104857600 bytes. Workspace tests were green before the pass, roundtrip verification was on for every cell, and all runs were under `flock /root/programs/fork/zstd-bench.lock`.

Four encoder commits in the window moved the measured cells, and the rest of the tree kept its bytes. The gated dense geometry `e94ce0f2` scans the json-class dense body at a 2^13 table, moving json fastest to x1.34 bulk and 1.10 mt8 at 0.34% denser output. The balanced row substitution `13d76d43` runs text-class and dll-class heads on the row twin at libzstd's L9 geometry, moving text balanced to 0.95 bulk at an unchanged 384.66 ratio and dll balanced to 1.12 at ratio 5.37 against 5.31. The wide-head donation stand-down `834f6ba3` lifts the bulk-mt cells, with text.balanced.mt8 moving 1.05 to 0.85 and mt16 1.15 to 0.81. The btlazy SIMD arming `1f208e1d` arms the long compare on single-thread best rows by parse feedback, holding dll best at x1.22 bulk and 1.16 stream at byte-identical output. The mid-band dictionary rows `9ea40553` and the empty-frame clamp `396e2291` touch dictionary frames and CLI edges off this corpus.

The commands ran through `cargo run --release -p zstdx-bench` with `dec-st` at 64 KiB pulls, `enc-st` chunked per shape with dll100 through `--file bench/big/dll100.raw`, `enc-mt` at workers 8 and 16 in two runs, `enc-stream` at workers 8 with the standalone-median rerun for text fastest and fast, and `ratio` at 141 s. The timing sections ran between 11:57 and 12:44, the ratio sweep ran in a resumed session afterwards, and every log carried its footer before the docs were written.

The 2026-10-31 refresh at `0a60b531` reran five sections on the same machine and toolchain through the prebuilt release binary under the shared lock: `matrix --mode dec-mt` with `files --threads 2,4,8,16` on `bench/big/dll100.zst3`, `matrix --mode enc-stream` and the same section over `--file bench/big/dll100.raw`, `small --shape json,text --level 1,3,9`, `matrix --mode enc-st --full-ladder` split per shape with the json and skewed tails split again by level range after a session interruption, and a `dec-st` pull sweep at `--pull 4k,16k,64k,256k,1m` with the four dll100 `.zst` inputs feeding the README figures. Every rerun section ran at the default per-side budget of 500 ms, interleaved, roundtrip-gated, with the harness header printing `libzstd 1.5.7, binding 10507`. A single-core co-tenant dump from a parallel worktree ran during parts of the dll100 stream segment, and the json deep-tier stream cells read slower in both refresh invocations than in the pass, so treat those cells as parity-band rather than wins. The full ladder's slow cells stop at three rounds per side as before, and the fast-side budget rule still accumulates over a hundred rounds on random levels 17-19 where the reference side is the slow one.

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

This table carries the 2026-10-31 refresh at `0a60b531`. Throughput is MiB/s at 1, 2, 4, 8 and 16 workers, and libzstd has no MT decode, so the reference column is its single-thread streaming cell. The dll row comes from `files --threads`, the solo tool at the same budget, with its reference taken from this pass's `dll100.zst3` streaming cell.

| file | ours ST | zstd stream ST ref | mt2 | mt4 | mt8 | mt16 |
|---|---:|---:|---:|---:|---:|---:|
| json.zst3 | 1384 | 1878 | 1797 | 2025 | 2188 | 2196 |
| text.zst3 | 10850 | 11216 | 10756 | 10951 | 10554 | 10671 |
| skewed.zst9 | 647 | 792 | 736 | 757 | 768 | 747 |
| random.zst3 | 9793 | 8812 | 9467 | 9455 | 9441 | 9511 |
| dll100.zst3 | 1260 | 1703 | 1852 | 2103 | 2226 | 2278 |

Json mt16 runs at 1.59 times single-thread and 1.17 times the reference. Dll100 mt16 runs at 1.81 times single-thread and 1.34 times the reference, so the strongest scaling row holds its band. Skewed reads 1.16 times single-thread and 0.94 times the reference with its band wandering ±15% across passes, text is flat at 0.98, and random is flat at 0.97 times single-thread and 1.08 times the reference. The json single-thread solo cell is a first-run page-state outlier: the section's opening invocation read 1014 with every mt cell unchanged, and the immediate rerun read 1384, so the warm reading is the tabulated one and the mt16-over-ST factor moves with it (2.22 on the cold reading).

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

Throughput uses 64 KiB pulls and interleaved medians, with single-thread and MT8 columns. x is our time divided by zstd's at the same mode, so below 1 means we are faster. This section carries the 2026-10-31 refresh at `0a60b531`, because the two commits after the 2026-10-30 pass touched stream cells: the stream probe stand-down `b8247325` moved dll l9 stream bytes and lifts text.balanced stream-mt8 from x1.05 to 0.84, and the armed-row driver state `0a60b531` re-rolled the fast-row layout.

| cell | ST ours | ST zstd | ST x | MT8 ours | MT8 zstd | MT8 x |
|---|---:|---:|---:|---:|---:|---:|
| json.fastest | 628 | 771 | 1.23 | 3023 | 1804 | 0.61 |
| json.fast | 465 | 450 | 0.97 | 1716 | 396 | 0.23 |
| json.balanced | 84 | 105 | 1.25 | 343 | 111 | 0.31 |
| json.best | 24 | 33 | 1.42 | 77 | 49 | 0.64 |
| json.opt | 6 | 7 | 1.08 | 16 | 7 | 0.42 |
| json.ultra | 3 | 3 | 1.09 | 6 | 3 | 0.51 |
| text.fastest | 6633* | 1748 | 0.26 | 7999 | 3785 | 0.48 |
| text.fast | 6391* | 5144 | 0.80 | 5604 | 1638 | 0.30 |
| text.balanced | 1302 | 1535 | 1.18 | 1175 | 980 | 0.84 |
| text.best | 907 | 632 | 0.70 | 792 | 359 | 0.44 |
| text.opt | 725 | 407 | 0.56 | 660 | 377 | 0.57 |
| text.ultra | 378 | 246 | 0.65 | 362 | 229 | 0.64 |

The two text single-thread fast cells carry standalone medians under the established protocol. The in-pass table read 4695 and 4653 at x0.365 and 1.030, the same-process page-dispersity collapse reproduced for the seventh consecutive pass, and the mechanism and reproducer are in the [workflow pitfalls](../pitfalls/workflow.md). Unknown-size text.fastest streaming has zstd emitting 1.84 MB at ratio 18.3 against our 108 KB at ratio 309. Json's deep-tier stream cells drifted this session: best reads 1.42 against the pass's 1.21, and the same-day dll-section rerun read 1.33, while opt and ultra read 1.08 and 1.09 against 0.97 and 0.99 with the rerun at 1.01 and 1.05, so the two deep tiers sit at parity-band and best carries a real slower reading. Stream-mt8 against our own bulk-mt8 ceilings reads json 84%, 73%, 85%, 100%, 133% and 120%, and text 40%, 40%, 70%, 84%, 103% and 109%.

The dll100 stream cells read single-thread fastest 500 against 509 at x1.02, fast 436 against 394 at 0.90, balanced 108 against 138 at 1.27, best 29 against 36 at 1.26, opt 10 against 13 at 1.16 and ultra 7 against 8 at 1.12, all six cells level with or slower than the pass's readings inside the noise band. Stream-mt8 reads fastest 859 against 1753 at x2.05, fast 769 against 430 at 0.57, balanced 126 against 194 at 1.54, best 25 against 78 at 3.08, opt 29 against 23 at 0.79 and ultra 17 against 17 at 0.96 near-parity. The cells run at 70% to 89% of the bulk-mt8 ceilings of 1016, 957, 181, 28, 34 and 20 MiB/s, with best the highest share at 89% and balanced the lowest at 70%, and the fastest, balanced and best losses stay the open item in the [todo](../todo.md).

## Compression-ratio sweep

The sweep runs `zstdx-bench ratio` on 5 shapes, 6 tiers, bulk and stream modes and single-thread and 4-worker MT, one deterministic pass per cell with checksums off. The delta is ours over zstd's ratio minus 1, aggregated as a geo-mean, and every cell is roundtrip-gated. The wall time is 141 s.

The geo-mean over 120 cells is **+8.70%**. By mode it reads bulk single-thread **+1.49%**, bulk MT **+11.09%**, stream single-thread **+11.44%** and stream MT **+11.12%**. By shape it reads json +4.46%, text +40.36%, skewed +1.41%, random plus or minus zero and zeros +2.06%.

The four json fastest cells are the only cells that moved against the 2026-10-25 sweep, each 0.34% denser through the dense geometry, and every other cell is byte-identical. The complete losing cells are json.fast bulk MT at -0.14% and stream MT at -0.12%, text.ultra bulk MT and stream MT at -0.02%, skewed.opt at -0.06% to -0.07%, skewed.ultra at -0.04%, skewed.fast bulk MT at -0.01% and stream MT at -0.03%, and skewed.fastest at -0.00%. Everything else is at parity or denser, random ties byte-exact at every cell, text.balanced reads +1.65% to +1.71%, and json.opt bulk single-thread is 9 bytes ahead of zstd-17.

## Full numeric ladder

This section carries the 2026-10-31 refresh at `0a60b531`, rerun split per shape under the shared lock at the same per-side budget of 500 ms, and the cell text reads ours/zstd throughput followed by the x ratio. Cells whose three minimum rounds already cover the 500 ms budget stop at n=3, which affects json levels 6-22 and skewed 4 and 13-22, so those absolutes carry the widest drift band. The rest accumulate n of at least 4, with text at 4 or more, random at 32 or more and zeros at 392 or more. x is our time divided by zstd's, so below 1 means we are faster. Every ratio cell reproduces the 2026-10-25 bytes, with json level 1 the one moved row at 0.34% denser through the dense geometry.

### Ladder speed

| level | json | text | skewed | random | zeros |
|---|---|---|---|---|---|
| 1 | 614/853 x1.39 | 12135/10109 x0.83 | 2828/1245 x0.44 | 2186/1981 x0.90 | 42538/13117 x0.31 |
| 2 | 503/633 x1.26 | 11902/9843 x0.83 | 2760/874 x0.32 | 2173/1945 x0.90 | 43037/13078 x0.30 |
| 3 | 458/471 x1.03 | 12525/7097 x0.57 | 205/223 x1.08 | 2286/1903 x0.83 | 42328/8453 x0.20 |
| 4 | 412/435 x1.05 | 12043/6936 x0.58 | 152/166 x1.09 | 2288/1955 x0.86 | 41810/8430 x0.20 |
| 5 | 201/265 x1.32 | 7341/4575 x0.62 | 2038/128 x0.063 | 2419/1731 x0.72 | 40257/6264 x0.16 |
| 6 | 143/186 x1.30 | 5098/2670 x0.52 | 2030/107 x0.052 | 2432/1725 x0.71 | 38233/2764 x0.072 |
| 7 | 124/164 x1.33 | 4313/2509 x0.58 | 1578/99 x0.062 | 2423/1675 x0.69 | 38197/2733 x0.072 |
| 8 | 99/125 x1.27 | 3704/1709 x0.46 | 1617/81 x0.050 | 2416/1669 x0.69 | 37415/1759 x0.047 |
| 9 | 154/117 x0.76 | 1795/1727 x0.96 | 1833/74 x0.040 | 2429/1555 x0.64 | 27911/1731 x0.062 |
| 10 | 54/75 x1.45 | 2969/1511 x0.51 | 1300/60 x0.047 | 2349/1267 x0.54 | 36792/1684 x0.046 |
| 11 | 45/54 x1.22 | 2401/1491 x0.62 | 1450/48 x0.032 | 2402/1348 x0.56 | 37589/1684 x0.045 |
| 12 | 38/45 x1.21 | 2312/800 x0.35 | 1169/23 x0.020 | 2260/604 x0.27 | 36140/922 x0.026 |
| 13 | 19/24 x1.28 | 1085/654 x0.60 | 6/10 x1.65 | 2252/234 x0.10 | 36056/777 x0.022 |
| 14 | 15/18 x1.25 | 934/539 x0.58 | 6/11 x1.98 | 2252/115 x0.051 | 26171/664 x0.025 |
| 15 | 17/13 x0.82 | 871/429 x0.49 | 6/8 x1.26 | 2218/94 x0.043 | 27577/591 x0.021 |
| 16 | 5/7 x1.37 | 809/497 x0.62 | 3/8 x2.89 | 2216/16 x0.007 | 35530/1027 x0.029 |
| 17 | 6/6 x1.04 | 799/433 x0.54 | 3/3 x0.94 | 8836/11 x0.001 | 25650/850 x0.033 |
| 18 | 3/4 x1.33 | 467/362 x0.78 | 2/3 x1.36 | 8962/10 x0.001 | 35900/819 x0.023 |
| 19 | 3/3 x1.02 | 395/251 x0.64 | 2/2 x0.94 | 8968/7 x0.001 | 29474/608 x0.021 |
| 20 | 3/2 x0.75 | 261/191 x0.72 | 2/2 x0.76 | 2203/6 x0.003 | 25434/368 x0.009 |
| 21 | 2/2 x0.95 | 265/137 x0.52 | 2/1 x0.89 | 2197/8 x0.003 | 31969/211 x0.007 |
| 22 | 1/1 x0.51 | 251/119 x0.48 | 2/1 x0.88 | 2193/9 x0.004 | 33647/180 x0.005 |

The bands read level with the 2026-10-25 table where the encoder window left them: json levels 5-8 at x1.27 to 1.33 carrying their byte wins, json 13 and 14 at 1.28 and 1.25, text levels 13-15 at 0.49 to 0.60, and the high tiers at or below 1.04 on json 17-22 and 0.94 on skewed 17-22, with json 18 at 1.33 and skewed 18 at 1.36 the two exceptions. Text level 9 flips from x1.18 to 0.96 through the balanced row substitution, closing the old hole. Random levels 17-19 read 8836, 8962 and 8968 MiB/s at byte-exact ratio 1.00 through the raw-block fast path, while levels 20-22 keep the full ultra parse. The n=3 cells wander the widest this session: json level 10 reads 1.45 against the old 1.20 and skewed level 16 reads 2.89 against 2.17, with both sides slower in absolute terms on json 10. The remaining hole is skewed levels 13-16 speed at 1.26 to 2.89 on ratio parity.

### Ladder ratio

This table carries the 2026-10-31 refresh: every cell reproduces the 2026-10-25 bytes, with json level 1 the one moved row at 6.41 against 6.39 through the dense geometry. The ratio table reads ours over zstd at the same numeric level, where a higher number means we are denser.

| level | json | text | skewed | random | zeros |
|---|---|---|---|---|---|
| 1 | 6.41/6.11 | 309.23/308.94 | 2.00/2.00 | 1.00/1.00 | 32577/32171 |
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

This section carries the 2026-10-31 refresh at `0a60b531`. Each cell is a per-call encode of the given size with checksums off, and x is our time divided by zstd's, so below 1 means we are faster. Sizes run 1 KiB, 4 KiB, 64 KiB and 1024 KiB for json and text at levels 1, 3 and 9.

| level | shape | 1K | 4K | 64K | 1024K |
|---|---|---:|---:|---:|---:|
| 1 | json | 1.25 | 1.42 | 1.49 | 1.34 |
| 1 | text | 1.67 | 1.54 | 1.89 | 1.56 |
| 3 | json | 1.31 | 1.17 | 0.99 | 1.15 |
| 3 | text | 1.37 | 1.14 | 1.06 | 1.03 |
| 9 | json | 1.08 | 0.92 | 0.64 | 1.02 |
| 9 | text | 1.77 | 1.56 | 2.80 | 1.02 |

Json level 9 stays on the stock row at x1.08, 0.92, 0.64 and 1.02. Text level 9 keeps the swap's bytes at x1.77, 1.56, 2.80 and 1.02. The two 64K cells keep wandering inside their drift band across passes, json level 3 at 1.01, 0.83, 0.97 and 0.99 over four passes and text level 9 at 2.76, 2.46, 2.78 and 2.80. Level 1's accepted trades are unchanged, and the stock-row exemption for dictionary frames shows only in dictionary fixtures' bytes, not here.
