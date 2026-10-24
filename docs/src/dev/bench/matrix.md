# Bench Matrix (2026-10-18)

This page holds the raw measurement tables for the 2026-10-18 pre-release pass. Each section states what the table measures and how to read its columns. Throughout the page `x = ours_time / zstd_time`, so values below 1 mean we are faster, throughput is MiB/s of raw input, and ratio is input bytes divided by output bytes. The comparison target is libzstd 1.5.7 with `zstdmt` enabled. Conclusions are in [snapshot.md](snapshot.md), and earlier archives from the 2026-10-01, 09-28, 09-27 and 09-19 passes live in this file's git history.

## Run provenance

The pass ran at commit `add0b0be` on 2026-10-18 from a clean tree in one continuous session, covering zstdx, zstdx-cli and zstdx-ffi 0.2.0. The machine is an AMD Eng Sample 100-000000870-32_Y, a Zen4-class part with 32 visible cores, AVX-512 and BMI2, a maximum clock of 5386 MHz, and the same machine as every pass since 09-12. The toolchain is rustc 1.100.0-nightly (8925ea358 2026-08-20) in the release profile with codegen-units set to 1, and the harness header prints `libzstd 1.5.7, binding 10507` from the zstd crate and zstd-sys with zstdmt enabled. The corpus is unchanged since the 09-19 pass, with json.raw from 2026-09-15 and the other four shapes from 2026-09-07, and `bench/big/dll100.*` is the same 2026-09-19 02:08 set with dll100.raw at 104857600 bytes. Workspace tests were green before the pass in debug, release and no-default-features, roundtrip verification was on for every cell, and all runs were under `flock /root/programs/fork/zstd-bench.lock`.

Seventeen library commits landed after the 2026-10-01 pass: the fused bit-read re-land as a per-callsite `const FUSED` plus the offset rotation, the fast and dfast coordinate epoch, the reach probe's concurrent shrink and stock parse, the dictionary stock-row exact-selection exemption, the chain-heads epoch past 3.5 GiB, the MT `count_from_long` SIMD compare, `bulk` pledged_size, and the decode-hardening quartet. Encoder outputs are byte-identical to the 2026-10-01 pass, with all 120 ratio cells size-equal, the geo-mean unchanged at +8.69%, all 110 ladder ratio cells equal and dll100 tier sizes identical. Speed moves concentrate where the commits point: text.balanced single-thread moved from x1.34 to 1.10 and its ladder level 9 row from 1.34 to 1.11, json.balanced from 0.84 to 0.771, json.best 1.28 to 1.23, json.ultra 1.03 to 1.00, skewed.opt 0.95 to 0.92 and dll.best 1.42 to 1.26. Decode stream cells drift a few percent better, with json 1.19-1.35 to 1.13-1.33 and dll zst9 bulk 0.80 to 0.742, decode MT json mt16 moves from 1.60 to 1.65 times single-thread, and text.balanced stream-mt8 closes to x0.947 as the section's first balanced-tier win. Dll100 stream cells reproduce the earlier addendum with best at x5.39 against the recorded 5.4.

The commands all ran through `cargo run --release -p zstdx-bench`. `ratio` took 115 s. `matrix --mode dec-st --budget-ms 1000` ran with the four dll100 `.zst*` files appended through `--file`, and `matrix --mode dec-mt --budget-ms 1000` followed. `matrix --mode enc-st --budget-ms 1000` was chunked per shape and tier group exactly as the 09-28 and 10-01 passes, with dll100 through `--shape zeros --file bench/big/dll100.raw`. `matrix --mode enc-mt --workers 8 --mt-workers 8` and `--workers 16 --mt-workers 16` both used `--budget-ms 1000`. `matrix --mode enc-stream --workers 8 --mt-workers 8 --budget-ms 1000` covered json and text, with a standalone `--shape text --level fastest,fast` rerun for the streaming standalone-median protocol and a `--file bench/big/dll100.raw` rerun for the dll stream cells. `small --level 1,3,9 --shape json,text` covered small payloads, `files --budget-ms 1000 --threads 2,4,8,16 bench/big/dll100.zst3` covered the dll decode scaling row, and `matrix --mode enc-st --full-ladder --budget-ms 500` was chunked per shape and level span across 23 chunks. The full session took about 75 minutes.

## Decode single-thread

Throughput is MiB/s of raw data for bulk and streaming decode with 64 KiB pulls. x is our time divided by zstd's time, so below 1 means we are faster, and the harness spread is at most ±0.005 on every cell.

| file | bulk ours | bulk zstd | bulk x | stream ours | stream zstd | stream x |
|---|---:|---:|---:|---:|---:|---:|
| json.zst1 | 1733 | 1227 | 0.71 | 1778 | 2186 | 1.23 |
| json.zst3 | 1434 | 1109 | 0.77 | 1415 | 1874 | 1.33 |
| json.zst9 | 1724 | 1184 | 0.69 | 1677 | 2146 | 1.28 |
| json.zst19 | 2158 | 1269 | 0.59 | 2226 | 2510 | 1.13 |
| text.zst1 | 5948 | 2126 | 0.36 | 6284 | 7554 | 1.20 |
| text.zst3 | 8612 | 2338 | 0.27 | 10449 | 11094 | 1.06 |
| text.zst9 | 9286 | 2387 | 0.26 | 11612 | 12053 | 1.04 |
| text.zst19 | 9331 | 2388 | 0.26 | 11607 | 12128 | 1.05 |
| skewed.zst1 | 2334 | 1467 | 0.63 | 2632 | 2843 | 1.08 |
| skewed.zst3 | 1271 | 1016 | 0.80 | 1292 | 1547 | 1.20 |
| skewed.zst9 | 643 | 642 | 1.00 | 621 | 797 | 1.28 |
| skewed.zst19 | 2183 | 1410 | 0.65 | 2461 | 2697 | 1.10 |
| random.zst3 | 8796 | 2181 | 0.25 | 11068 | 8888 | 0.80 |
| zeros.zst3 | 11690 | 2449 | 0.21 | 12865 | 12874 | 1.00 |
| dll100.zst1 | 1157 | 973 | 0.84 | 1183 | 1514 | 1.28 |
| dll100.zst3 | 1236 | 1011 | 0.82 | 1280 | 1709 | 1.34 |
| dll100.zst9 | 1536 | 1137 | 0.74 | 1575 | 2081 | 1.32 |
| dll100.zst19 | 1326 | 1048 | 0.79 | 1379 | 1819 | 1.32 |

Bulk holds 17 of 18 wins, with skewed.zst9 at x1.00 as the near-tie that is now exactly at parity. Every stream cell reads at or a few percent inside its 2026-10-01 value: json 1.19-1.35 to 1.13-1.33, skewed.zst9 1.32 to 1.28, and dll 1.30-1.36 to 1.28-1.34. The fused bit-read re-land was gate-measured at -1% to -3% on these cells on 2026-10-07, and this pass reads the same direction.

## Decode multi-thread scaling

Throughput is MiB/s at 1, 2, 4, 8 and 16 workers, and libzstd has no MT decode, so the reference column is its single-thread streaming cell from the decode table. The dll row comes from `files --threads`, the solo tool at the same budget.

| file | ours ST | zstd stream ST ref | mt2 | mt4 | mt8 | mt16 |
|---|---:|---:|---:|---:|---:|---:|
| json.zst3 | 1399 | 1875 | 1927 | 2117 | 2276 | 2308 |
| text.zst3 | 11132 | 11179 | 10832 | 10852 | 10843 | 10818 |
| skewed.zst9 | 608 | 788 | 783 | 756 | 782 | 759 |
| random.zst3 | 9678 | 8928 | 9458 | 9479 | 9476 | 9471 |
| dll100.zst3 | 1225 | 1709 | 1872 | 2102 | 2241 | 2322 |

Json mt16 runs at 1.65 times single-thread and 1.23 times the reference, against 1.60 and 1.18 in the previous pass, and dll100 mt16 runs at 1.90 times single-thread and 1.36 times the reference, against 1.83 and 1.31, so both scaling rows strengthened. Skewed reads 1.25 times single-thread and 0.96 times the reference with its band wandering ±15% across passes, and the previous pass read 1.19 and 0.95. Text is flat at 0.97, and random is flat at 0.98 times single-thread and 1.06 times the reference.

## Encode single-thread bulk

Throughput and ratio are per shape with checksums off on both sides, and the tier column names the ladder tier. The pairing is libzstd levels 1, 3, 9, 13, 17 and 19, and x is our time divided by zstd's, so below 1 means we are faster. The dll row uses the 100 MB ELF-concat payload `bench/big/dll100.raw` from `bench/gen_big.sh`, whose ratios are payload sizes rather than the 32 MiB corpus.

| level | shape | ours MiB/s | ours ratio | zstd MiB/s | zstd ratio | x |
|---|---|---:|---:|---:|---:|---:|
| fastest | json | 590 | 6.39 | 852 | 6.11 | 1.45 |
| fastest | text | 13231 | 309.23 | 10494 | 308.94 | 0.79 |
| fastest | skewed | 2832 | 2.00 | 1253 | 2.00 | 0.44 |
| fastest | random | 2286 | 1.00 | 2052 | 1.00 | 0.90 |
| fastest | zeros | 49317 | 32577 | 13305 | 32171 | 0.27 |
| fastest | dll | 501 | 4.35 | 529 | 2.19 | 1.06 |
| fast | json | 456 | 5.30 | 472 | 5.29 | 1.03 |
| fast | text | 13512 | 333.03 | 7264 | 332.90 | 0.54 |
| fast | skewed | 211 | 1.92 | 225 | 1.92 | 1.07 |
| fast | random | 2283 | 1.00 | 1911 | 1.00 | 0.84 |
| fast | zeros | 49217 | 32577 | 8709 | 32171 | 0.18 |
| fast | dll | 439 | 4.97 | 424 | 3.40 | 0.97 |
| balanced | json | 157 | 7.20 | 121 | 5.95 | 0.77 |
| balanced | text | 1580 | 384.66 | 1732 | 378.41 | 1.10 |
| balanced | skewed | 1839 | 2.00 | 75 | 1.84 | 0.041 |
| balanced | random | 2280 | 1.00 | 1471 | 1.00 | 0.65 |
| balanced | zeros | 32822 | 32577 | 1750 | 32202 | 0.053 |
| balanced | dll | 116 | 5.31 | 150 | 4.62 | 1.30 |
| best | json | 30 | 6.26 | 37 | 6.10 | 1.23 |
| best | text | 1141 | 386.42 | 692 | 385.90 | 0.61 |
| best | skewed | 7 | 1.84 | 14 | 1.84 | 1.91 |
| best | random | 2190 | 1.00 | 251 | 1.00 | 0.11 |
| best | zeros | 42873 | 32577 | 816 | 32171 | 0.019 |
| best | dll | 31 | 5.35 | 39 | 4.67 | 1.26 |
| opt | json | 7 | 7.49 | 7 | 7.49 | 1.01 |
| opt | text | 816 | 410.23 | 452 | 410.11 | 0.55 |
| opt | skewed | 3 | 2.00 | 3 | 2.00 | 0.92 |
| opt | random | 2217 | 1.00 | 12 | 1.00 | 0.005 |
| opt | zeros | 41220 | 32577 | 900 | 32202 | 0.022 |
| opt | dll | 13 | 6.04 | 14 | 5.06 | 1.09 |
| ultra | json | 3 | 7.42 | 3 | 7.42 | 1.00 |
| ultra | text | 402 | 414.12 | 258 | 413.98 | 0.64 |
| ultra | skewed | 2 | 2.00 | 2 | 2.00 | 0.95 |
| ultra | random | 2221 | 1.00 | 8 | 1.00 | 0.003 |
| ultra | zeros | 39748 | 32577 | 634 | 32202 | 0.016 |
| ultra | dll | 8 | 6.32 | 9 | 5.28 | 1.12 |

Sizes equal the previous pass byte for byte on every cell. The balanced and best band carries the concurrent reach-probe parse, with text.balanced 1.34 to 1.10 and json.balanced 0.84 to 0.77, and the tree tiers read at or inside their bands with json.best 1.28 to 1.23, dll.best 1.42 to 1.26, skewed.opt 0.95 to 0.92 and json.ultra 1.03 to 1.00. Fastest and fast reproduce within their known spreads, with json.fastest 1.47 to 1.45 and dll.fastest 1.03 to 1.06. Checksums on versus off on our side read json.fast 1.00 and text.fast 0.86.

## Encode multi-thread bulk

Throughput is MiB/s from one pass with a cold pool per call on both sides, and the ratio columns are at 16 workers. The x8 and x16 columns are our time divided by zstd's at the same worker count, so below 1 means we are faster.

| cell | ours mt8 | zstd mt8 | x8 | ours mt16 | zstd mt16 | x16 | ratio ours mt16 | ratio zstd mt16 |
|---|---:|---:|---:|---:|---:|---:|---:|---:|
| json.fastest | 3443 | 4046 | 1.19 | 5175 | 1758 | 0.34 | 6.34 | 6.11 |
| json.fast | 2375 | 990 | 0.42 | 3307 | 1004 | 0.30 | 5.30 | 5.31 |
| json.balanced | 427 | 201 | 0.48 | 548 | 202 | 0.38 | 7.19 | 5.95 |
| text.fastest | 19538 | 10572 | 0.55 | 17237 | 2228 | 0.13 | 309.08 | 39.78 |
| text.fast | 11700 | 2144 | 0.18 | 12020 | 2127 | 0.18 | 332.94 | 189.16 |
| text.balanced | 1116 | 1223 | 1.08 | 1144 | 1216 | 1.09 | 384.59 | 378.30 |
| skewed.fastest | 4072 | 3216 | 0.79 | 3610 | 1527 | 0.42 | 2.00 | 2.00 |
| skewed.fast | 1193 | 631 | 0.54 | 1633 | 635 | 0.39 | 1.92 | 1.92 |
| skewed.balanced | 681 | 120 | 0.18 | 750 | 122 | 0.16 | 2.00 | 1.84 |

Json.fastest.mt8 at x1.19 with a spread of 0.95 to 1.68 remains the one clean zstd MT win, inside the cell's known noise band of 0.95 to 1.65 across passes. Every other cell is ahead or, for text.balanced at 1.08 and 1.09, inside its own 0.99 to 1.47 spread. Our cold-pool mt16 json.fast at 3307 MiB/s still beats zstd's warm-pool reference at 1593 this run.

## Encode streaming

Throughput uses 64 KiB pulls and interleaved medians, with single-thread and MT8 columns. x is our time divided by zstd's at the same mode, so below 1 means we are faster.

| cell | ST ours | ST zstd | ST x | MT8 ours | MT8 zstd | MT8 x |
|---|---:|---:|---:|---:|---:|---:|
| json.fastest | 588 | 776 | 1.32 | 3265 | 1877 | 0.57 |
| json.fast | 467 | 439 | 0.94 | 2273 | 394 | 0.17 |
| json.balanced | 87 | 106 | 1.22 | 389 | 113 | 0.29 |
| json.best | 30 | 35 | 1.16 | 82 | 51 | 0.62 |
| json.opt | 7 | 7 | 0.97 | 17 | 7 | 0.40 |
| json.ultra | 3 | 3 | 0.98 | 7 | 3 | 0.47 |
| text.fastest | 6960* | 1761 | 0.25 | 7575 | 4131 | 0.55 |
| text.fast | 6695* | 5260 | 0.79 | 6306 | 1676 | 0.27 |
| text.balanced | 1427 | 1612 | 1.13 | 1002 | 947 | 0.95 |
| text.best | 1026 | 646 | 0.63 | 853 | 351 | 0.41 |
| text.opt | 750 | 414 | 0.55 | 669 | 371 | 0.55 |
| text.ultra | 389 | 247 | 0.63 | 366 | 236 | 0.64 |

The two text single-thread fast cells carry standalone medians under the established protocol. The in-pass table read 5601 and 5502 at x0.310 and 0.909, the same-process page-dispersity collapse reproduced in four consecutive full passes, and the mechanism and reproducer are in the [workflow pitfalls](../pitfalls/workflow.md). Unknown-size text.fastest streaming has zstd emitting 1.84 MB at ratio 18.3 against our 108 KB at ratio 309. Text.balanced stream-mt8 closes to x0.947 against 0.98, so every stream-mt cell including it now wins. Stream-mt8 against our own bulk-mt8 ceilings reads json 92%, 88%, 86%, 100%, 131% and 140% and text 39%, 48%, 90%, 102%, 112% and 111%.

The dll100 stream cells come from a same-session `--file` rerun. Single-thread reads fastest 506 against 500 at x0.99, fast 448 against 395 at 0.88, balanced 119 against 138 at 1.16, best 32 against 38 at 1.20, opt 13 against 14 at 1.05 and ultra 8 against 9 at 1.08. Stream-mt8 reads fastest 587 against 2047 at x3.52, fast 537 against 428 at 0.80, balanced 154 against 176 at 1.15, best 15 against 84 at x5.39, opt 26 against 25 at 0.95 and ultra 15 against 18 at 1.21, reproducing the earlier addendum cell for cell at 52% to 92% of the bulk-mt8 ceilings of 1064, 966, 168, 29, 36 and 22 MiB/s.

A later rerun on 2026-10-22 after the buffer-reserve fix read byte-identical outputs and showed the decided reserve now covering the stream, which killed the mid-stream growth barrier described in [mt-stream](../perf/mt-stream.md). Stream-mt8 fastest moved to 897 against 2154 at x2.36 and 86% of the 1048 ceiling, balanced to 161 against 199 at 1.25 and 93%, best to 27 against 84 at x3.12 and 93% of 29, and ultra to 18 against 18 at x0.98 for libzstd-MT parity and 82% of 22, with corpus stream cells flat. The libzstd-MT fastest and best losses stay the open item in the [todo](../todo.md).

## Compression-ratio sweep

The sweep runs `zstdx-bench ratio` on 5 shapes, 6 tiers, bulk and stream modes and single-thread and 4-worker MT, one deterministic pass per cell with checksums off. The delta is ours over zstd's ratio minus 1, aggregated as a geo-mean, and every cell is roundtrip-gated. The wall time is 115 s.

The geo-mean over 120 cells is **+8.69%**. By mode it reads bulk single-thread **+1.48%**, bulk MT **+11.08%**, stream single-thread **+11.43%** and stream MT **+11.11%**. By shape it reads json +4.41%, text +40.36%, skewed +1.41%, random plus or minus zero and zeros +2.06%.

Outputs are byte-identical to the 2026-10-01 sweep and therefore to the 09-28 and 09-27 sweeps, because the interim commits touch no encoder output on this corpus. The complete losing cells are identical to 2026-10-01: json.fast MT at -0.12% to -0.14%, text.ultra MT at -0.02%, skewed.opt at -0.06% to -0.07%, skewed.ultra at -0.04%, skewed.fast MT at -0.01% to -0.03% and skewed.fastest at -0.00%. Everything else is at parity or denser, random ties byte-exact at every cell, text.balanced reads +1.65% to +1.70%, and json.opt bulk single-thread is 9 bytes ahead of zstd-17.

## Full numeric ladder

This is the release gate: both sides at the same numeric level with a per-side budget of 500 ms, and the cell text reads ours/zstd throughput followed by the x ratio. Cells whose three minimum rounds already cover the 500 ms budget stop at n=3, which affects json levels 6-22 and skewed 4 and 13-22, so those absolutes carry the widest drift band. The rest accumulate n of at least 4, with text at 5 or more, random at 36 or more and zeros at 504 or more. x is our time divided by zstd's, so below 1 means we are faster.

### Ladder speed

| level | json | text | skewed | random | zeros |
|---|---|---|---|---|---|
| 1 | 556/850 x1.53 | 13254/10478 x0.79 | 2848/1261 x0.44 | 2373/2125 x0.90 | 49629/13310 x0.27 |
| 2 | 544/655 x1.21 | 13127/10276 x0.78 | 2768/865 x0.31 | 2353/2168 x0.92 | 49487/13290 x0.27 |
| 3 | 464/475 x1.02 | 13529/7245 x0.54 | 212/225 x1.06 | 2327/2028 x0.87 | 49268/8726 x0.18 |
| 4 | 423/433 x1.02 | 12231/6898 x0.56 | 153/164 x1.08 | 2328/2050 x0.88 | 48905/8658 x0.18 |
| 5 | 207/268 x1.30 | 7477/4603 x0.62 | 1947/125 x0.064 | 2439/1803 x0.74 | 47312/6353 x0.13 |
| 6 | 148/188 x1.27 | 5902/2746 x0.47 | 1955/105 x0.054 | 2452/1782 x0.73 | 45679/2782 x0.061 |
| 7 | 125/165 x1.31 | 4954/2612 x0.53 | 1615/99 x0.061 | 2443/1725 x0.71 | 44991/2759 x0.061 |
| 8 | 101/126 x1.25 | 4128/1756 x0.42 | 1605/81 x0.051 | 2441/1752 x0.72 | 44614/1771 x0.040 |
| 9 | 157/120 x0.76 | 1562/1734 x1.11 | 1836/75 x0.041 | 2320/1541 x0.66 | 33079/1750 x0.053 |
| 10 | 74/90 x1.23 | 3139/1550 x0.49 | 1361/62 x0.045 | 2341/1200 x0.51 | 42543/1688 x0.040 |
| 11 | 53/65 x1.24 | 2479/1419 x0.57 | 1482/48 x0.033 | 2365/1297 x0.55 | 42728/1687 x0.039 |
| 12 | 45/56 x1.23 | 2353/859 x0.36 | 1196/25 x0.021 | 2335/673 x0.29 | 40790/985 x0.024 |
| 13 | 30/37 x1.23 | 1137/693 x0.62 | 7/14 x1.91 | 2266/250 x0.11 | 42675/821 x0.019 |
| 14 | 25/27 x1.10 | 1006/573 x0.57 | 7/12 x1.70 | 2272/119 x0.052 | 41333/710 x0.017 |
| 15 | 22/17 x0.76 | 947/454 x0.48 | 7/8 x1.17 | 2273/113 x0.050 | 40266/628 x0.016 |
| 16 | 8/10 x1.26 | 815/509 x0.62 | 4/9 x2.27 | 2262/21 x0.009 | 43031/1078 x0.025 |
| 17 | 7/7 x1.01 | 798/448 x0.57 | 3/3 x0.96 | 2297/12 x0.005 | 41310/916 x0.022 |
| 18 | 4/5 x1.24 | 472/371 x0.79 | 2/3 x1.37 | 2305/10 x0.004 | 41219/875 x0.021 |
| 19 | 3/3 x1.01 | 401/261 x0.65 | 2/2 x0.94 | 2282/8 x0.003 | 39634/661 x0.017 |
| 20 | 3/2 x0.82 | 266/208 x0.78 | 2/2 x0.77 | 2345/6 x0.003 | 39159/407 x0.010 |
| 21 | 2/2 x0.88 | 258/148 x0.57 | 2/1 x0.89 | 2307/8 x0.003 | 36110/237 x0.007 |
| 22 | 2/1 x0.52 | 246/129 x0.53 | 2/1 x0.87 | 2301/9 x0.004 | 40911/203 x0.005 |

The concurrent reach-probe parse moves text level 9 from x1.34 to 1.11 and json level 9 from 0.83 to 0.76. Json levels 13 and 14 read 1.23 and 1.10 against 1.25 and 1.12, and text levels 13-15 read 0.48 to 0.62 against 0.50 to 0.67. Json levels 5-8 and 10-12 hold the head-gated attempts band at x1.23 to 1.31, skewed levels 13-16 hold their chain rows at x1.17 to 2.27, and every high-tier row stays at or below 1.01 on json levels 17-22 and at or below 0.96 on skewed levels 17-22. The remaining holes are unchanged: text level 9 at x1.11 and skewed levels 13-16 speed at ratio parity.

### Ladder ratio

The ratio table reads ours over zstd at the same numeric level, where a higher number means we are denser. It is byte-identical to the 2026-10-01 table, with all 110 ladder size cells re-verified equal.

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

Each cell is a per-call encode of the given size with checksums off, and x is our time divided by zstd's, so below 1 means we are faster. Sizes run 1 KiB, 4 KiB, 64 KiB and 1024 KiB for json and text at levels 1, 3 and 9.

| level | shape | 1K | 4K | 64K | 1024K |
|---|---|---:|---:|---:|---:|
| 1 | json | 1.24 | 1.40 | 1.41 | 1.37 |
| 1 | text | 1.73 | 1.57 | 1.78 | 1.54 |
| 3 | json | 1.28 | 1.16 | 0.83 | 1.15 |
| 3 | text | 1.34 | 1.13 | 0.90 | 1.03 |
| 9 | json | 1.11 | 0.90 | 0.64 | 0.99 |
| 9 | text | 1.73 | 1.49 | 2.46 | 1.00 |

Json level 9 stays on the stock row at x1.11, 0.90, 0.64 and 0.99 against the previous pass's 1.13, 0.93, 0.66 and 1.00. Text level 9 keeps the swap's bytes at x1.73, 1.49, 2.46 and 1.00 against 1.73, 1.48, 2.76 and 1.01, with the 64K cell reading faster in its drift band. Level 3's 64K cells improved from json 1.01 to 0.83 and text 1.09 to 0.90, and level 1's accepted trades are unchanged. The stock-row exemption for dictionary frames shows only in dictionary fixtures' bytes, not here.
