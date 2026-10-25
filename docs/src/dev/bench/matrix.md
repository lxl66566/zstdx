# Bench Matrix (2026-10-25)

This page holds the raw measurement tables for the 2026-10-25 pre-release pass. Each section states what the table measures and how to read its columns. Throughout the page `x = ours_time / zstd_time`, so values below 1 mean we are faster, throughput is MiB/s of raw input, and ratio is input bytes divided by output bytes. The comparison target is libzstd 1.5.7 with `zstdmt` enabled. Conclusions are in [snapshot.md](snapshot.md), and earlier archives from the 2026-10-18, 2026-10-01, 09-28, 09-27 and 09-19 passes live in this file's git history.

## Run provenance

The pass ran at commit `49f48b20` on 2026-10-25 from a clean tree in one continuous session, covering the 197-commit window since the v0.2.1 release commit `4a51bb6c`. The machine is an AMD Eng Sample 100-000000870-32_Y, a Zen4-class part with 32 visible cores, AVX-512 and BMI2, a maximum clock of 5386 MHz, and the same machine as every pass since 09-12. The toolchain is rustc 1.100.0-nightly (8925ea358 2026-08-20) in the release profile with codegen-units set to 1, unchanged from the 2026-10-18 pass, and the harness header prints `libzstd 1.5.7, binding 10507`. The corpus is unchanged, with json.raw from 2026-09-15 and the other four shapes from 2026-09-07, and `bench/big/dll100.*` is the same 2026-09-19 set with dll100.raw at 104857600 bytes. Workspace tests were green before the pass, roundtrip verification was on for every cell, and all runs were under `flock /root/programs/fork/zstd-bench.lock`.

The pass found one beyond-noise speed regression and fixed it in-session: dll100.best on both single-thread paths, bulk 31 to 28 MiB/s and stream 32 to 28, at byte-identical output. The cause was code placement, not search logic. The nbSeq emit fix `93c84b22` grew `FrameCompressor::compress` by 44 bytes, which shifted every dubt instantiation by 0x20 and rolled the inlined count loop to a head at 48 modulo 64, straddling a 64-byte I-cache and uop-cache line at identical instruction, branch and miss counts, plus 12.9% cycles. The fix extracts the count loop behind the R32 two-copy phase-proof entry stub, the same pattern `extend_match_aligned` already uses, so one of the two copies always sits at a phase of 31 or less. All dll100 encode cells in the tables below come from the post-fix rerun logs (`enc-st-dll-afterfix`, `enc-stream-dll-afterfix`); every other cell comes from the pass runs. The full mechanism walk is in the [matchers notes](../perf/matchers.md).

Encoder outputs are byte-identical to the 2026-10-18 pass on every measured surface: all 120 ratio cells size-equal, the geo-mean unchanged at +8.69%, all 110 ladder ratio cells equal and dll100 tier sizes identical. Speed moves concentrate where the window's commits point: the tree tiers on zeros read 7% to 12% faster (best 42873 to 46031, opt 41220 to 45229, ultra 39748 to 44516 MiB/s), and random's opt-tier ladder rows read four times the previous pass (level 17: 2297 to 9292 MiB/s) at byte-exact ratio 1.00, where the raw-block fast path now skips the tree parse on pure noise; levels 20-22 keep the full ultra parse. Decode stream cells drift a few percent better again, with dll zst9 bulk 0.742 to 0.728 and dll zst1 stream x1.28 to 1.24.

The commands all ran through `cargo run --release -p zstdx-bench` exactly as the 2026-10-18 pass, with `ratio` at 108 s, `dec-st` swept at 4k, 16k, 64k, 256k and 1m pulls, `enc-st` chunked per shape with dll100 through `--shape zeros --file bench/big/dll100.raw`, `enc-mt` at workers 8 and 16, `enc-stream` at workers 8 with the standalone-median rerun for text fastest and fast, `small --level 1,3,9 --shape json,text`, `files --budget-ms 1000 --threads 2,4,8,16 bench/big/dll100.zst3`, and the full ladder chunked per shape and level span. The enc-stream single-thread cells carry the full 4k-1m pull sweep this pass, and every cell stays within 5% of its 64 KiB value with a max spread of json.opt 1.7%. The full session took about 90 minutes plus the post-fix dll100 rerun.

## Decode single-thread

Throughput is MiB/s of raw data for bulk and streaming decode with 64 KiB pulls. x is our time divided by zstd's time, so below 1 means we are faster, and the harness spread is at most ±0.005 on every cell.

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

Bulk holds 17 of 18 wins with skewed.zst9 at x0.99 as the near-tie. Every stream cell reads a few percent better than the 2026-10-18 pass: json 1.13-1.33 to 1.17-1.29, dll 1.28-1.34 to 1.24-1.29, and random improves to 0.81 from 0.80. The dll stream cells are the pass's strongest decode movers, one to three percent per cell.

## Decode multi-thread scaling

Throughput is MiB/s at 1, 2, 4, 8 and 16 workers, and libzstd has no MT decode, so the reference column is its single-thread streaming cell from the decode table. The dll row comes from `files --threads`, the solo tool at the same budget.

| file | ours ST | zstd stream ST ref | mt2 | mt4 | mt8 | mt16 |
|---|---:|---:|---:|---:|---:|---:|
| json.zst3 | 1409 | 1880 | 1824 | 2075 | 2202 | 2193 |
| text.zst3 | 11222 | 11178 | 10842 | 10831 | 10826 | 10850 |
| skewed.zst9 | 624 | 792 | 758 | 712 | 754 | 752 |
| random.zst3 | 9700 | 8873 | 9382 | 9320 | 9323 | 9323 |
| dll100.zst3 | 1250 | 1702 | 1828 | 2088 | 2216 | 2287 |

Json mt16 runs at 1.56 times single-thread and 1.17 times the reference, against 1.65 and 1.23 in the previous pass, with mt8 and mt16 reading level this pass. Dll100 mt16 runs at 1.83 times single-thread and 1.34 times the reference, against 1.90 and 1.36, so the strongest scaling row holds its band. Skewed reads 1.21 times single-thread and 0.95 times the reference with its band wandering ±15% across passes, text is flat at 0.97, and random is flat at 0.96 times single-thread and 1.05 times the reference.

## Encode single-thread bulk

Throughput and ratio are per shape with checksums off on both sides, and the tier column names the ladder tier. The pairing is libzstd levels 1, 3, 9, 13, 17 and 19, and x is our time divided by zstd's, so below 1 means we are faster. The dll row uses the 100 MB ELF-concat payload `bench/big/dll100.raw` from `bench/gen_big.sh`, whose ratios are payload sizes rather than the 32 MiB corpus. The dll cells are the post-count-fix rerun.

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

Sizes equal the previous pass byte for byte on every cell. The zeros tree tiers carry the window's largest bulk gains, best 42873 to 46031, opt 41220 to 45229 and ultra 39748 to 44516 MiB/s. The remaining bands hold: text.balanced 1.10, json.balanced 0.77, json.best 1.24, dll.best 1.26 at the restored placement, dll.balanced 1.31 against zstd-9's 150 MiB/s this run, and the fast and fastest tiers reproduce within their known spreads. Checksums on versus off on our side read json.fast 1.03 and text.fast 0.84. Text.balanced bulk reads 1576 here and 1410 in the same session's ladder run, two same-session draws at the edge of that cell's band, with the ladder's median protocol the slower one.

## Encode multi-thread bulk

Throughput is MiB/s from one pass with a cold pool per call on both sides, and the ratio columns are at 16 workers. The x8 and x16 columns are our time divided by zstd's at the same worker count, so below 1 means we are faster.

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

Json.fastest.mt8 at x1.20 remains the one clean zstd MT win, inside the cell's known noise band of 0.95 to 1.65 across passes. Every other mt8 cell is ahead or, for text.balanced at 1.05, inside its own 0.99 to 1.47 spread. Our cold-pool mt16 json.fast at 3351 MiB/s still beats zstd's warm-pool reference at 1575 this run.

## Encode streaming

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

The two text single-thread fast cells carry standalone medians under the established protocol. The in-pass table read 5442 and 5391 at x0.315 and 0.918, the same-process page-dispersity collapse reproduced for the fifth consecutive pass, and the mechanism and reproducer are in the [workflow pitfalls](../pitfalls/workflow.md). Unknown-size text.fastest streaming has zstd emitting 1.84 MB at ratio 18.3 against our 108 KB at ratio 309. Text.balanced stream-mt8 reads x0.998 this pass against 0.947, exact parity rather than a win, the one stream-mt cell at the line. Stream-mt8 against our own bulk-mt8 ceilings reads json 94%, 98%, 88%, 101%, 131% and 120%, and text 43%, 44%, 91%, 87%, 105% and 108%.

The dll100 stream cells come from the post-count-fix rerun. Single-thread reads fastest 502 against 507 at x1.01, fast 437 against 392 at 0.90, balanced 119 against 139 at 1.17, best 31 against 38 at 1.21, opt 13 against 14 at 1.04 and ultra 8 against 9 at 1.08, with best restored from the pass's placement-loss 28 and the sizes byte-identical. Stream-mt8 reads fastest 884 against 2127 at x2.39, fast 829 against 424 at 0.50, balanced 155 against 208 at 1.34, best 26 against 81 at 3.08, opt 32 against 25 at 0.79 and ultra 18 against 18 at 0.99 parity. The cells run at 84% to 93% of the bulk-mt8 ceilings of 1049, 964, 167, 29, 36 and 21 MiB/s, and the fastest and best losses stay the open item in the [todo](../todo.md).

## Compression-ratio sweep

The sweep runs `zstdx-bench ratio` on 5 shapes, 6 tiers, bulk and stream modes and single-thread and 4-worker MT, one deterministic pass per cell with checksums off. The delta is ours over zstd's ratio minus 1, aggregated as a geo-mean, and every cell is roundtrip-gated. The wall time is 108 s.

The geo-mean over 120 cells is **+8.69%**. By mode it reads bulk single-thread **+1.48%**, bulk MT **+11.08%**, stream single-thread **+11.43%** and stream MT **+11.11%**. By shape it reads json +4.41%, text +40.36%, skewed +1.41%, random plus or minus zero and zeros +2.06%.

Outputs are byte-identical to the 2026-10-18 sweep and therefore to every sweep since 2026-09-27, because the interim commits touch no encoder output on this corpus. The complete losing cells are unchanged: json.fast MT at -0.12% to -0.14%, text.ultra MT at -0.02%, skewed.opt at -0.06% to -0.07%, skewed.ultra at -0.04%, skewed.fast MT at -0.01% to -0.03% and skewed.fastest at -0.00%. Everything else is at parity or denser, random ties byte-exact at every cell, text.balanced reads +1.65% to +1.70%, and json.opt bulk single-thread is 9 bytes ahead of zstd-17.

## Full numeric ladder

This is the release gate: both sides at the same numeric level with a per-side budget of 500 ms, and the cell text reads ours/zstd throughput followed by the x ratio. Cells whose three minimum rounds already cover the 500 ms budget stop at n=3, which affects json levels 6-22 and skewed 4 and 13-22, so those absolutes carry the widest drift band. The rest accumulate n of at least 4, with text at 5 or more, random at 36 or more and zeros at 504 or more. x is our time divided by zstd's, so below 1 means we are faster.

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

The bands hold against the previous pass: json levels 13 and 14 at 1.25 and 1.11 against 1.23 and 1.10, text levels 13-15 at 0.47 to 0.59 against 0.48 to 0.62, json levels 5-8 and 10-12 at x1.22 to 1.33 carrying their byte wins, skewed levels 13-16 at 1.11 to 2.17, and every high-tier row at or below 1.01 on json levels 17-22 and at or below 0.93 on skewed levels 17-22. Random levels 17-19 read 9292, 9035 and 9526 MiB/s against the previous pass's 2297, 2305 and 2282 at byte-exact ratio 1.00, the raw-block fast path now skipping the opt parse on pure noise, while levels 20-22 keep the full ultra parse at their previous speeds. Text level 9 reads 1410 against its own 1576 bulk draw and the previous pass's 1562, the widest same-cell spread in the pass. The remaining holes are unchanged: text level 9 at x1.18 and skewed levels 13-16 speed at ratio parity.

### Ladder ratio

The ratio table reads ours over zstd at the same numeric level, where a higher number means we are denser. It is byte-identical to the 2026-10-18 table, with all 110 ladder size cells re-verified equal.

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
| 1 | json | 1.25 | 1.41 | 1.52 | 1.39 |
| 1 | text | 1.67 | 1.60 | 1.92 | 1.58 |
| 3 | json | 1.31 | 1.14 | 0.97 | 1.14 |
| 3 | text | 1.32 | 1.10 | 1.06 | 1.02 |
| 9 | json | 1.07 | 0.90 | 0.65 | 1.00 |
| 9 | text | 1.69 | 1.43 | 2.78 | 1.00 |

Json level 9 stays on the stock row at x1.07, 0.90, 0.65 and 1.00 against the previous pass's 1.11, 0.90, 0.64 and 0.99. Text level 9 keeps the swap's bytes at x1.69, 1.43, 2.78 and 1.00. The two 64K cells wander inside their drift band across passes, json level 3 at 1.01, 0.83 and 0.97 over the last three passes and text level 9 at 2.76, 2.46 and 2.78, with json level 3 at 64K reading 0.97 this pass. Level 1's accepted trades are unchanged, and the stock-row exemption for dictionary frames shows only in dictionary fixtures' bytes, not here.
