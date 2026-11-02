# Bench Matrix (2026-11-02)

This page holds the raw measurement tables for the 2026-11-02 release pass. Each section states what the table measures and how to read its columns. Throughout the page `x = ours_time / zstd_time`, so values below 1 mean we are faster, throughput is MiB/s of raw input, and ratio is input bytes divided by output bytes. The comparison target is libzstd 1.5.7 with `zstdmt` enabled. Conclusions are in [snapshot.md](snapshot.md), and earlier archives from the 2026-11-01 R86, 2026-10-31, 10-30, 10-25, 10-18, 10-01, 09-28, 09-27 and 09-19 passes live in this file's git history.

## Run provenance

The pass ran at commit `2ba353fa` on the merged R88 tree, from 2026-11-01 23:41 through 2026-11-02 00:17, driven by `bash bench/pass.sh all` split into six locked segments with no other bench session in between, through one release build at a summed section wall of 32.2 minutes. The machine is an AMD Eng Sample 100-000000870-32_Y, a Zen4-class part with 32 visible cores, AVX-512 and BMI2, a maximum clock of 5386 MHz, and the same machine as every pass since 09-12. The toolchain is rustc 1.100.0-nightly (8925ea358 2026-08-20) in the release profile with codegen-units set to 1, and the harness header prints `libzstd 1.5.7, binding 10507`. The corpus is unchanged, with json.raw from 2026-09-15 and the other four shapes from 2026-09-07, and `bench/big/dll100.*` is the same 2026-09-19 set with dll100.raw at 104857600 bytes. The R88 belt had the workspace suite and `--profile paranoid -p zstdx` green at this commit before the pass, roundtrip verification was on for every cell, and the section logs live in `bench/scratch/pass-r89-rel`.

The harness is the R86 one unchanged. The enc-st and enc-stream single-thread cells run two at a time, so the header reads `st cells parallel 2`; asymmetric cells stop once the slow side spent the full budget and the fast side an eighth of it; and every timed encode cell gates on its own last round. Absolute MiB/s on those shared cells reads below the serial readings of every pre-R86 table, so cross-pass comparisons on those cells use x, and a verdict on a cell in the 10 GB/s class needs a `--parallel 1` rerun, as the [methodology](methodology.md) page records. The dec-st section stays serial and its bulk cell carries the reference-side fix `7817e052`: the zstd bulk reference reuses its Decompressor and output buffer, so the bulk x agrees with the stream column.

Three encoder commits landed in the window since the R86 pass, and none moved a stable byte on this pass's cells. The windowed LDM far class `6fe3c447` arms far history on the Best rows' bulk-MT and pledged-stream jobs: the R88 record has dll100 best bulk-mt8 at 20,515,629 B from 22,328,775 with its ceiling lifted from 44 to 57 MiB/s, which this pass's stream-mt8 reference row confirms, while the unpledged stream-mt8 grid and every single-thread cell keep their bytes. The pooled-core reach re-declaration `489d04b4` and the donation-discard fix `532c2598` repair a belt-only degenerate frame and a growing-grid checksum corner, with the 150-cell dump gate byte-identical on both.

Determinism held everywhere the bytes must not move: the ratio sweep reproduced the 2026-10-30 sizes cell for cell at a geo-mean of +8.70%, worst json.fast.bulk-mt at -0.14% and best text.fastest.stream-st at +1594.10%, the ladder and dll single-thread sizes read identical to the R86 pass, and the dump belt read byte-identical through the R88 gates.

## Decode single-thread

Throughput is MiB/s of raw data for bulk and streaming decode with 64 KiB pulls. x is our time divided by zstd's time, so below 1 means we are faster, and the harness spread is at most ±0.02 on every cell.

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

Every compressible shape reads x1.05 to 1.38 on both paths in this pass, which predates the libzstd-shaped sequence loop of 2026-11-02. The post-loop cells are in [dec-gap](dec-gap.md).

## Decode multi-thread scaling

Throughput is MiB/s at 1, 2, 4, 8 and 16 workers, and libzstd has no MT decode, so the reference column is its single-thread streaming cell. The dll row comes from `files --threads`, the solo tool at the same budget, with its reference taken from this pass's `dll100.zst3` streaming cell.

| file | ours ST | zstd stream ST ref | mt2 | mt4 | mt8 | mt16 |
|---|---:|---:|---:|---:|---:|---:|
| json.zst3 | 1435 | 1877 | 1816 | 2072 | 2200 | 2243 |
| text.zst3 | 11289 | 11217 | 10994 | 10953 | 11037 | 10961 |
| skewed.zst9 | 654 | 794 | 753 | 640 | 758 | 658 |
| random.zst3 | 9736 | 8844 | 9472 | 9501 | 9499 | 9506 |
| dll100.zst3 | 1267 | 1710 | 1819 | 2070 | 2203 | 2250 |

Json mt16 runs at 1.56 times single-thread and 1.20 times the reference. Dll100 mt16 runs at 1.78 times single-thread and 1.32 times the reference, so the strongest scaling row holds its band. Skewed reads 1.01 times single-thread and 0.83 times the reference with its band wandering ±15% across passes, text is flat at 0.98, and random is flat at 0.98 times single-thread and 1.07 times the reference.

## Encode single-thread bulk

Throughput and ratio are per shape with checksums off on both sides, and the tier column names the ladder tier. The pairing is libzstd levels 1, 3, 9, 13, 17 and 19, and x is our time divided by zstd's, so below 1 means we are faster. The dll row uses the 100 MB ELF-concat payload `bench/big/dll100.raw` from `bench/gen_big.sh`, whose ratios are payload sizes rather than the 32 MiB corpus. These cells share the machine in pairs, so the absolute MiB/s reads below the serial tables and x is the comparable column.

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

No single-thread cell moved its bytes this window, so the readings are wall noise around the R86 verdicts. The R86 movers hold their bands: json.opt at 1.01 and json.ultra at 1.00 at ratio parity, dll100.opt at 0.89 and dll100.ultra at 1.03, with the deep dll rows carrying -16.2% and -16.5% bytes. Json reads fastest 1.35, fast 1.06 and balanced 0.77 at +21% ratio, text holds fastest 0.84, fast 0.58 and balanced 0.94, and random and zeros win at every tier. The best rows read high in-pass, with json.best at 1.37 against the R86 1.23, text.best at 0.85 against 0.66 and the ladder's text.l15 at 0.56 against 0.48, and a `--parallel 1` rerun settles all three at 1.28, 0.69 and 0.49 against the R86 1.23, 0.66 and 0.48, so the in-pass gap is pair-sharing tax at identical bytes, not an encoder move. The dll fast-class cells read 1.09, 0.97 and 1.18 against the serial band's 1.04, 0.97 and 1.12, the same tax. Checksums on versus off on our side read json.fast 1.00 and text.fast 0.85.

## Encode multi-thread bulk

Throughput is MiB/s from one pass with a cold pool per call on both sides, and the ratio columns are at 16 workers. The x8 and x16 columns are our time divided by zstd's at the same worker count, so below 1 means we are faster.

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

Json.fastest.mt8 at x1.12 is the one zstd MT win, inside the cell's known noise band of 0.95 to 1.65 across passes, and its mt16 cell wins at 0.33. Every other cell is ahead, with text.balanced at 0.88 and 0.90 and skewed.balanced at 0.17 and 0.17. Our cold-pool mt16 json.fast at 3366 MiB/s still beats zstd's warm-pool reference at 1572 this run.

## Encode streaming

Throughput uses 64 KiB pulls and interleaved medians, with single-thread and MT8 columns. x is our time divided by zstd's at the same mode, so below 1 means we are faster. The single-thread cells share the machine in pairs, so x is the comparable column and the absolute MiB/s reads below the serial tables.

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

The two text single-thread fast cells carry in-pass readings at 5067 and 5014 MiB/s, and the same-process page-dispersity collapse reproduced for the ninth consecutive pass, with the mechanism and reproducer in the [workflow pitfalls](../pitfalls/workflow.md); a verdict on either cell needs the standalone rerun protocol, and the standalone medians read 6638 and 6082 MiB/s this pass. Unknown-size text.fastest streaming has zstd emitting 1.84 MB at ratio 18.3 against our 108 KB at ratio 309. The deep json rows hold their R86 verdicts at unchanged bytes, with json.opt.stream at 0.99 and json.ultra.stream at 0.97. Stream-mt8 against our own bulk-mt8 ceilings reads json 85%, 83%, 87%, 203%, 129% and 120%, and text 41%, 43%, 86%, 85%, 114% and 110%, with the json best and opt rows above their bulk ceilings through the burst pipeline's overlap.

The dll100 stream cells read single-thread fastest 465 against 503 at x1.04, fast 426 against 391 at 0.92, balanced 107 against 132 at 1.22, best 27 against 30 at 1.14, opt 13 against 11 at 0.88 and ultra 8 against 8 at 0.99. Stream-mt8 reads fastest 940 against 1939 at x2.05, fast 818 against 434 at 0.54, balanced 173 against 201 at 1.21, best 27 against 81 at 3.09, opt 37 against 25 at 0.67 and ultra 20 against 18 at 0.92. The cells run at 47% to 91% of the bulk-mt8 ceilings of 1059, 971, 191, 57, 42 and 23 MiB/s, with best the lowest share at 47%: the windowed LDM far class lifted this ceiling from 44 to 57 MiB/s and cut its bulk-mt8 bytes by 8.1%, while the unpledged stream-mt8 grid keeps the pre-class bytes at 22.33 MB, so arming the unpledged path is the open item in the [todo](../todo.md).

## Compression-ratio sweep

The sweep runs `zstdx-bench ratio` on 5 shapes, 6 tiers, bulk and stream modes and single-thread and 4-worker MT, one deterministic pass per cell with checksums off. The delta is ours over zstd's ratio minus 1, aggregated as a geo-mean, and every cell is roundtrip-gated. The wall time is 111.4 s at parallel 8.

The geo-mean over 120 cells is **+8.70%**. By mode it reads bulk single-thread **+1.49%**, bulk MT **+11.09%**, stream single-thread **+11.44%** and stream MT **+11.12%**. By shape it reads json +4.46%, text +40.36%, skewed +1.41%, random plus or minus zero and zeros +2.06%.

Every cell reproduced the 2026-10-30 sizes byte for byte, so the cell-level story is unchanged. The complete losing cells are json.fast bulk MT at -0.14% and stream MT at -0.12%, text.ultra bulk MT and stream MT at -0.02%, skewed.opt at -0.06% to -0.07%, skewed.ultra at -0.04%, skewed.fast bulk MT at -0.01% and stream MT at -0.03%, and skewed.fastest at -0.00%. Everything else is at parity or denser, random ties byte-exact at every cell, text.balanced reads +1.65% to +1.71%, and json.opt bulk single-thread is 9 bytes ahead of zstd-17.

## Full numeric ladder

The cell text reads ours/zstd throughput followed by the x ratio. Cells whose three minimum rounds already cover the 500 ms budget stop at n=3, which affects json levels 6-22 and skewed 4-22, so those absolutes carry the widest drift band and share the machine in pairs. The asymmetric-cell cap stops random levels 17-22 at n of 5 to 8. x is our time divided by zstd's, so below 1 means we are faster. Every ratio cell reproduces the R86 bytes.

### Ladder speed

| level | json | text | skewed | random | zeros |
|---|---|---|---|---|---|
| 1 | 520/825 x1.35 | 11566/9933 x0.85 | 1958/859 x0.44 | 2053/1905 x0.92 | 46396/13182 x0.28 |
| 2 | 557/671 x1.20 | 11646/9775 x0.84 | 2707/873 x0.32 | 2113/1958 x0.93 | 45515/13194 x0.29 |
| 3 | 454/476 x1.05 | 12136/7121 x0.59 | 208/229 x1.10 | 2052/1909 x0.93 | 45002/8591 x0.19 |
| 4 | 438/439 x1.01 | 10553/6697 x0.63 | 152/165 x1.08 | 2018/1891 x0.93 | 44651/8553 x0.19 |
| 5 | 194/261 x1.34 | 6808/4514 x0.66 | 2020/125 x0.062 | 2130/1657 x0.78 | 43338/6287 x0.15 |
| 6 | 143/186 x1.30 | 5345/2707 x0.51 | 1829/105 x0.057 | 2176/1637 x0.75 | 45371/2779 x0.061 |
| 7 | 123/163 x1.32 | 4540/2560 x0.56 | 1936/100 x0.051 | 2196/1574 x0.72 | 45734/2754 x0.060 |
| 8 | 98/123 x1.26 | 3841/1731 x0.45 | 1708/82 x0.048 | 2251/1607 x0.71 | 45156/1769 x0.039 |
| 9 | 146/114 x0.77 | 1832/1722 x0.94 | 1870/71 x0.039 | 2236/1413 x0.64 | 31961/1742 x0.055 |
| 10 | 68/85 x1.25 | 3025/1532 x0.51 | 1620/57 x0.035 | 2267/1137 x0.50 | 43360/1694 x0.039 |
| 11 | 48/58 x1.23 | 2431/1450 x0.59 | 1661/39 x0.023 | 2234/1131 x0.50 | 42942/1684 x0.039 |
| 12 | 40/49 x1.21 | 2354/781 x0.33 | 1531/21 x0.014 | 1805/516 x0.27 | 42776/910 x0.021 |
| 13 | 23/31 x1.35 | 910/640 x0.71 | 5/8 x1.64 | 2140/111 x0.056 | 44451/773 x0.017 |
| 14 | 17/21 x1.27 | 860/530 x0.61 | 5/8 x1.47 | 2122/75 x0.035 | 44029/656 x0.015 |
| 15 | 18/13 x0.73 | 759/422 x0.56 | 5/6 x1.14 | 2178/75 x0.034 | 42838/577 x0.014 |
| 16 | 6/8 x1.34 | 873/483 x0.55 | 3/4 x1.45 | 2221/13 x0.006 | 41739/1009 x0.024 |
| 17 | 5/5 x0.95 | 869/432 x0.50 | 3/2 x0.71 | 2222/9 x0.004 | 40585/837 x0.021 |
| 18 | 3/4 x1.15 | 497/361 x0.73 | 2/2 x1.03 | 3539/8 x0.002 | 41729/795 x0.019 |
| 19 | 3/3 x1.00 | 423/249 x0.59 | 2/2 x0.86 | 3582/6 x0.002 | 37612/590 x0.016 |
| 20 | 3/2 x0.81 | 282/198 x0.70 | 2/1 x0.77 | 2236/6 x0.002 | 37268/364 x0.010 |
| 21 | 2/2 x0.84 | 278/139 x0.50 | 1/1 x0.89 | 2215/7 x0.003 | 40846/211 x0.005 |
| 22 | 2/1 x0.52 | 273/123 x0.45 | 1/1 x0.82 | 2246/9 x0.004 | 28759/177 x0.006 |

The deep json rows sit at or under parity as in the R86 pass: levels 17 and 19 read 0.95 and 1.00, level 20 at 0.81 and level 22 at 0.52 carry the byte wins, and level 18 at 1.15 is the one exception, with levels 13 and 14 at 1.35 and 1.27 holding their byte wins. Text keeps its chain-row wins at 0.33 to 0.84 through levels 12-22, with level 9 at 0.94. The skewed hole at levels 13-16 reads 1.14 to 1.64 on ratio parity, with skewed 17-22 at 0.71 to 1.03. Random levels 17-19 read 2222 to 3582 MiB/s at byte-exact ratio 1.00 through the raw-block fast path, while levels 20-22 keep the full ultra parse. The n=3 cells still wander the widest, with json level 15 at 0.73 against the R86 0.78 and skewed level 16 at 1.45 against 1.41 the largest moves inside that band.

### Ladder ratio

Every cell reproduces the R86 bytes, and the table is unchanged with it. The ratio table reads ours over zstd at the same numeric level, where a higher number means we are denser.

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
| 1 | json | 1.29 | 1.58 | 1.50 | 1.34 |
| 1 | text | 1.72 | 1.80 | 1.92 | 1.56 |
| 3 | json | 1.33 | 1.31 | 1.02 | 1.16 |
| 3 | text | 1.40 | 1.24 | 1.10 | 1.04 |
| 9 | json | 1.08 | 1.09 | 0.67 | 1.01 |
| 9 | text | 1.89 | 1.66 | 2.86 | 1.02 |

Json level 9 stays on the stock row at x1.08, 1.09, 0.67 and 1.01. Text level 9 keeps the swap's bytes at x1.89, 1.66, 2.86 and 1.02. The 4K cells wander the widest this pass, with json level 3 at 1.31 against the R86 1.17 and json level 9 at 1.09 against 0.93, and the two 64K cells keep their drift bands, json level 3 at 1.01, 0.83, 0.97, 0.99, 0.98 and 1.02 over six passes and text level 9 at 2.76, 2.46, 2.78, 2.80, 2.86 and 2.86. Level 1's accepted trades are unchanged, and the stock-row exemption for dictionary frames shows only in dictionary fixtures' bytes, not here.
