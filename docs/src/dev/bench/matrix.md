# Bench Matrix (2026-11-16)

This page holds the raw measurement tables for the 2026-11-16 release pass. Each section states what the table measures and how to read its columns. Throughout the page `x = ours_time / zstd_time`, so values below 1 mean we are faster, throughput is MiB/s of raw input, and ratio is input bytes divided by output bytes. The comparison target is libzstd 1.5.7 with `zstdmt` enabled. Conclusions are in [snapshot.md](snapshot.md), and earlier archives from the 2026-11-07 R104, 2026-11-03 R94, 2026-11-02 R89, 2026-11-01 R86, 2026-10-31, 10-30, 10-25, 10-18, 10-01, 09-28, 09-27 and 09-19 passes live in this file's git history.

## Run provenance

The pass ran at commit `00b51bd0` on the post-R108 hardening tree, on 2026-11-16, driven by `bash bench/pass.sh all` in one locked segment with no other bench session in between, through one release build at a summed section wall of 22.9 minutes. The machine is an AMD Eng Sample 100-000000870-32_Y, a Zen4-class part with 32 visible cores, AVX-512 and BMI2, a maximum clock of 5386 MHz, and the same machine as every pass since 09-12. The toolchain is rustc 1.100.0-nightly (8925ea358 2026-08-20), unchanged since R94, in the release profile with codegen-units set to 1, and the harness header prints `libzstd 1.5.7, binding 10507`. The corpus is unchanged, with json.raw from 2026-09-15 and the other four shapes from 2026-09-07, and `bench/big/dll100.*` is the same 2026-09-19 set with dll100.raw at 104857600 bytes. The workspace suite (815 tests), the paranoid belt (609) and the no-default-features suite (335) were green at this commit before the pass, roundtrip verification was on for every cell, and the section logs live in `bench/scratch/pass-r109-rel`, including the two README side runs below.

The harness is the R86 one plus the adaptive sample floor this window landed: the enc-st and enc-stream single-thread cells run two at a time, so the header reads `st cells parallel 2`; a warmup round pair at eight times the budget runs twice instead of three; asymmetric cells stop once the slow side spent the full budget and the fast side one sixteenth of it; and every timed encode cell gates on its own last round. Absolute MiB/s on those shared cells reads below the serial readings of every pre-R86 table, so cross-pass comparisons on those cells use x, and a verdict on a cell in the 10 GB/s class needs a `--parallel 1` rerun, as the [methodology](methodology.md) page records. The dec-st section stays serial. Two side runs feed the README figures beside the pass: `matrix --mode dec-st --pull 4k,16k,64k,256k,1m` over the corpus plus the four dll100 `.zst` files, and the text fastest/fast standalone enc-stream rerun after a dropped page cache under the page-dispersity protocol, both in the same locked session as the pass.

Thirty-five commits landed in the window since the R104 pass and the v0.5.0 release cut. The R107 decode round rebuilt the sequence tables in the reference's two-pass shape and dieted the X1 rebuild chain, R108 fed the pooled stream core's blocks straight into the matcher window tail, and the hardening sweep that follows fixed twenty-three verified defects across the memory-safety, correctness, FFI and CLI surfaces, with the memory-safety lane write limit, the frame-owned carry, the Block_Maximum_Size cap, the EOF frame end, the split-block finish fragment, the raw-dictionary Repeat rejection and the decoder's lazy window reserve on the list. The balanced row's head verdicts armed on open single-threaded streams, the X2 tail finished through the pair table, and the pledged-mt l9 grid floored at the bulk strip.

Determinism held everywhere the bytes must not move except at the window's one documented lever: the ratio sweep moved exactly one of 120 cells, json.balanced stream single-thread onto the bulk cell's exact frame through the open-stream head-verdict arm, and every other cell, the dll stream grid, the full ladder and the small-payload bytes reproduce the R104 sizes. The dump-grid lever, the pledged-mt l9 floor, closes the skewed and text l9 pledged stream cells onto their bulk counterparts, 3,461 and 12 bytes closer, recorded with the dump anchors in the bench Readme.

## Decode single-thread

Throughput is MiB/s of raw data for bulk and streaming decode with 64 KiB pulls. x is our time divided by zstd's time, so below 1 means we are faster, and the harness spread is at most ±0.02 on every cell.

| file | bulk ours | bulk zstd | bulk x | stream ours | stream zstd | stream x |
|---|---:|---:|---:|---:|---:|---:|
| json.zst1 | 2252 | 2107 | 0.94 | 2127 | 2032 | 0.96 |
| json.zst3 | 2077 | 1842 | 0.89 | 1849 | 1702 | 0.92 |
| json.zst9 | 2442 | 2163 | 0.89 | 2136 | 1985 | 0.93 |
| json.zst19 | 2676 | 2514 | 0.94 | 2560 | 2501 | 0.98 |
| text.zst1 | 7352 | 6501 | 0.89 | 7316 | 7354 | 1.01 |
| text.zst3 | 9620 | 9123 | 0.95 | 10709 | 10981 | 1.03 |
| text.zst9 | 10166 | 9777 | 0.96 | 11564 | 12029 | 1.04 |
| text.zst19 | 10101 | 9784 | 0.97 | 11616 | 12091 | 1.04 |
| skewed.zst1 | 3096 | 3028 | 0.98 | 3066 | 2850 | 0.93 |
| skewed.zst3 | 1644 | 1566 | 0.95 | 1551 | 1471 | 0.95 |
| skewed.zst9 | 978 | 772 | 0.79 | 820 | 690 | 0.84 |
| skewed.zst19 | 2899 | 2961 | 1.02 | 2749 | 2687 | 0.98 |
| random.zst3 | 8950 | 8942 | 1.00 | 11086 | 8871 | 0.80 |
| zeros.zst3 | 12079 | 11139 | 0.93 | 12879 | 12882 | 1.00 |
| dll100.zst1 | 1473 | 1504 | 1.02 | 1424 | 1449 | 1.02 |
| dll100.zst3 | 1705 | 1641 | 0.96 | 1618 | 1599 | 0.99 |
| dll100.zst9 | 2090 | 2002 | 0.96 | 2003 | 1948 | 0.97 |
| dll100.zst19 | 1807 | 1718 | 0.95 | 1754 | 1693 | 0.97 |
| multi.zst3 | 3203 | 2977 | 0.93 | 3227 | 3027 | 0.94 |

Every corpus shape reads at or under parity bulk except skewed.zst19 at 1.02, with skewed.zst9 the widest win at 0.79, and the stream column holds 0.93 to 1.04 with random's 0.80 the widest win. This session's zstd side read 4 to 8 percent below its R104 absolutes on the json, skewed and dll100 cells while our side held, so the dll100 band's move from R104's 1.02 to 1.07 onto 0.95 to 1.02 mixes the R107 diets' and the X2 tail's real instruction cuts with that drift; the pull-sweep side run reads dll100 streaming at 0.98 to 1.03 across every pull size. The multi-frame payload decodes ahead single-threaded on both paths; its parallel story is the section below.

## Decode multi-thread scaling

Throughput is MiB/s at 1, 2, 4, 8 and 16 workers, and libzstd has no MT decode, so the reference column is its single-thread streaming cell. The dll row comes from `files --threads`, the solo tool at the same budget, with its reference taken from this pass's `dll100.zst3` streaming cell.

| file | ours ST | zstd stream ST ref | mt2 | mt4 | mt8 | mt16 |
|---|---:|---:|---:|---:|---:|---:|
| json.zst3 | 2016 | 1708 | 2548 | 2911 | 2999 | 3060 |
| text.zst3 | 11915 | 11002 | 11716 | 11704 | 11698 | 11730 |
| skewed.zst9 | 980 | 695 | 1296 | 1537 | 1641 | 1451 |
| random.zst3 | 9683 | 8780 | 9511 | 9596 | 9590 | 9048 |
| dll100.zst3 | 1705 | 1618 | 2408 | 2776 | 2932 | 3011 |

Json mt16 runs at 1.52 times single-thread and 1.79 times the reference, dll100 mt16 at 1.78 and 1.86, so the strongest single-frame rows hold. Skewed reads 1.67 times single-thread and 2.36 times the reference at mt8 against R104's 1.72 and 2.13, with mt16 settling at 1.48 against R104's 1.65, both inside the cell's recorded band. Text stays flat at 0.98 and random at 0.93 to 0.99 times single-thread. The remaining walls are sequence-read and aggregate-bandwidth bound, tracked as todo 1 in the [todo](../todo.md).

The files-multi section times the four multi-frame payloads at every worker count, the lane budget's home ground:

| payload | ST | mt2 | mt4 | mt8 | mt16 | mt16 / ST |
|---|---:|---:|---:|---:|---:|---:|
| multi.zst3 | 3403 | 4399 | 6087 | 9205 | 9698 | 2.85 |
| multi_small.zst3 | 2472 | 2853 | 4516 | 5998 | 7037 | 2.85 |
| multi_balanced.zst3 | 2490 | 3156 | 3992 | 5781 | 7751 | 3.11 |
| multi_twodom.zst3 | 3394 | 4349 | 4825 | 4792 | 5222 | 1.54 |

Multi BalANCED at mt16 runs 3.11 times its single-thread decode and multi and multi_small 2.85 each, against the single-frame corpus cells' 0.98 to 1.8 band, so the lane budget's staging split holds at release scale. The two-domain payload scales at 1.54, its second frame's restart-poor half setting the pace, which is the R101 twodom pricing's predicted shape.

## Encode single-thread bulk

Throughput and ratio are per shape with checksums off on both sides, and the tier column names the ladder tier. The pairing is libzstd levels 1, 3, 9, 13, 17 and 19, and x is our time divided by zstd's, so below 1 means we are faster. The dll row uses the 100 MB ELF-concat payload `bench/big/dll100.raw` from `bench/gen_big.sh`, whose ratios are payload sizes rather than the 32 MiB corpus. These cells share the machine in pairs, so the absolute MiB/s reads below the serial tables and x is the comparable column.

| level | shape | ours MiB/s | ours ratio | zstd MiB/s | zstd ratio | x |
|---|---:|---:|---:|---:|---:|---:|
| fastest | json | 630 | 6.41 | 825 | 6.11 | 1.31 |
| fastest | text | 11958 | 309.23 | 10035 | 308.94 | 0.84 |
| fastest | skewed | 2618 | 2.00 | 1205 | 2.00 | 0.46 |
| fastest | random | 2100 | 1.00 | 1909 | 1.00 | 0.90 |
| fastest | zeros | 49024 | 32577 | 13225 | 32171 | 0.27 |
| fastest | dll | 493 | 4.36 | 528 | 2.19 | 1.07 |
| fast | json | 454 | 5.30 | 475 | 5.29 | 1.05 |
| fast | text | 12205 | 333.03 | 7139 | 332.90 | 0.58 |
| fast | skewed | 212 | 1.92 | 219 | 1.92 | 1.03 |
| fast | random | 2219 | 1.00 | 1924 | 1.00 | 0.87 |
| fast | zeros | 48961 | 32577 | 8614 | 32171 | 0.18 |
| fast | dll | 427 | 4.98 | 425 | 3.40 | 1.00 |
| balanced | json | 153 | 7.20 | 114 | 5.95 | 0.75 |
| balanced | text | 1808 | 384.66 | 1630 | 378.41 | 0.88 |
| balanced | skewed | 1812 | 2.00 | 73 | 1.84 | 0.040 |
| balanced | random | 2264 | 1.00 | 1401 | 1.00 | 0.62 |
| balanced | zeros | 32440 | 32577 | 1739 | 32202 | 0.054 |
| balanced | dll | 140 | 5.36 | 143 | 4.62 | 1.02 |
| best | json | 22 | 6.26 | 31 | 6.10 | 1.36 |
| best | text | 981 | 386.42 | 590 | 385.90 | 0.60 |
| best | skewed | 6 | 1.84 | 8 | 1.84 | 1.49 |
| best | random | 2181 | 1.00 | 133 | 1.00 | 0.072 |
| best | zeros | 44329 | 32577 | 627 | 32171 | 0.014 |
| best | dll | 27 | 5.36 | 31 | 4.67 | 1.16 |
| opt | json | 7 | 7.48 | 6 | 7.49 | 0.85 |
| opt | text | 897 | 408.84 | 424 | 410.11 | 0.47 |
| opt | skewed | 2 | 2.00 | 2 | 2.00 | 0.83 |
| opt | random | 2189 | 1.00 | 9 | 1.00 | 0.004 |
| opt | zeros | 40876 | 32577 | 827 | 32202 | 0.020 |
| opt | dll | 14 | 6.03 | 12 | 5.06 | 0.88 |
| ultra | json | 3 | 7.42 | 3 | 7.42 | 1.08 |
| ultra | text | 420 | 414.12 | 250 | 413.98 | 0.59 |
| ultra | skewed | 2 | 2.00 | 2 | 2.00 | 0.89 |
| ultra | random | 2296 | 1.00 | 7 | 1.00 | 0.003 |
| ultra | zeros | 43940 | 32577 | 610 | 32202 | 0.014 |
| ultra | dll | 8 | 6.32 | 8 | 5.28 | 1.03 |

The hardening window's one encode-side wall risk was the carry ownership's per-block memcpy, and it leaves no trace: json.opt reads 0.85 against R104's 0.87, dll100.opt 0.88 against 0.89, json.ultra 1.08 against 1.09 and dll100.ultra 1.03 against 1.05, every size byte-identical. The best rows stay the n=3 wander class, with json.best at 1.36 in-pass against R104's 1.46 and skewed.best at 1.49 against 1.60. The dll rows read the diet's landed geometry unchanged: balanced at 19,570,844 B against zstd's 22,701,602, 13.8 percent smaller, fastest and fast at 49.8 and 31.2 percent smaller. Checksums on versus off on our side read json.fast 1.04 and text.fast 0.85.

## Encode multi-thread bulk

Throughput is MiB/s from one pass with a cold pool per call on both sides, and the ratio columns are at 16 workers. The x8 and x16 columns are our time divided by zstd's at the same worker count, so below 1 means we are faster.

| cell | ours mt8 | zstd mt8 | x8 | ours mt16 | zstd mt16 | x16 | ratio ours mt16 | ratio zstd mt16 |
|---|---:|---:|---:|---:|---:|---:|---:|---:|
| json.fastest | 3580 | 3981 | 1.13 | 5305 | 1722 | 0.32 | 6.35 | 6.11 |
| json.fast | 2408 | 995 | 0.42 | 3347 | 1006 | 0.29 | 5.30 | 5.31 |
| json.balanced | 414 | 201 | 0.48 | 521 | 199 | 0.38 | 7.19 | 5.95 |
| text.fastest | 18400 | 10388 | 0.57 | 16824 | 2135 | 0.13 | 309.08 | 39.78 |
| text.fast | 11524 | 2066 | 0.18 | 11132 | 2050 | 0.18 | 332.94 | 189.16 |
| text.balanced | 1299 | 1244 | 0.93 | 1326 | 1200 | 0.90 | 384.59 | 378.30 |
| skewed.fastest | 4088 | 3246 | 0.79 | 3546 | 1492 | 0.42 | 2.00 | 2.00 |
| skewed.fast | 1246 | 562 | 0.46 | 1630 | 628 | 0.39 | 1.92 | 1.92 |
| skewed.balanced | 654 | 119 | 0.18 | 656 | 119 | 0.18 | 2.00 | 1.84 |

Json.fastest.mt8 at x1.13 stays the one zstd MT win, inside the cell's known noise band of 0.95 to 1.65 across passes, and its mt16 cell wins at 0.32. Every other cell is ahead, with text.balanced at 0.93 and 0.90 on the pair-sharing session band and skewed.balanced at 0.18 on both counts. Our cold-pool mt16 json.fast at 3347 MiB/s still beats zstd's warm-pool reference at 1615 this run.

## Encode streaming

Throughput uses 64 KiB pulls and interleaved medians, with single-thread and MT8 columns. x is our time divided by zstd's at the same mode, so below 1 means we are faster. The single-thread cells share the machine in pairs, so x is the comparable column and the absolute MiB/s reads below the serial tables.

| cell | ST ours | ST zstd | ST x | MT8 ours | MT8 zstd | MT8 x |
|---|---:|---:|---:|---:|---:|---:|
| json.fastest | 612 | 757 | 1.24 | 3260 | 1855 | 0.58 |
| json.fast | 457 | 437 | 0.95 | 2279 | 401 | 0.18 |
| json.balanced | 156 | 99 | 0.64 | 388 | 114 | 0.29 |
| json.best | 21 | 25 | 1.23 | 77 | 52 | 0.68 |
| json.opt | 7 | 5 | 0.80 | 31 | 7 | 0.23 |
| json.ultra | 3 | 3 | 0.99 | 11 | 3 | 0.30 |
| text.fastest | 5394* | 1749 | 0.32 | 7535 | 3918 | 0.52 |
| text.fast | 5352* | 4838 | 0.90 | 5812 | 1644 | 0.28 |
| text.balanced | 1526 | 1592 | 1.05 | 1214 | 934 | 0.77 |
| text.best | 1010 | 630 | 0.62 | 890 | 304 | 0.34 |
| text.opt | 841 | 408 | 0.49 | 722 | 372 | 0.52 |
| text.ultra | 409 | 245 | 0.60 | 269 | 231 | 0.86 |

The two text single-thread fast cells carry in-pass readings at 5394 and 5352 MiB/s, and the same-process page-dispersity collapse reproduced for the twelfth consecutive pass, with the mechanism and reproducer in the [workflow pitfalls](../pitfalls/workflow.md); a verdict on either cell needs the standalone rerun protocol, and the post-drop-caches standalone medians read 6878 and 6440 MiB/s at x0.26 and 0.81 against R104's 6149 and 5938 at 0.27 and 0.83. The balanced row's open-stream arm moved its three single-thread cells onto their bulk frames' bytes: json.balanced reads 156 MiB/s at x0.64 against R104's 68 at 1.34, text.balanced 1526 at 1.05 against 1313 at 1.22, dll100.balanced 140 at 0.93 against 99 at 1.35, and the deep json rows read opt 0.80 and ultra 0.99 at byte-identical output.

The dll100 stream cells read single-thread fastest 477 against 504 at x1.06, fast 434 against 391 at 0.90, balanced 140 against 131 at 0.93, best 28 against 30 at 1.06, opt 14 against 11 at 0.80 and ultra 8 against 8 at 1.03. Stream-mt8 reads fastest 986 against 1862 at x1.87, fast 927 against 426 at 0.46, balanced 179 against 204 at 1.13, best 51 against 83 at 1.62, opt 51 against 24 at 0.48 and ultra 27 against 17 at 0.65. The balanced row's 179 MiB/s sits back inside the 160 to 200 placement band: the cell runs 8 jobs of 12.5 MiB on 8 workers whose wall is the single slowest job, and the two-CCD host moves that core through L3 sharing and boost state at identical instruction counts, the A/B recorded in the [workflow pitfalls](../pitfalls/workflow.md). The cells run at 93, 95, 89, 94, 91 and 93 percent of the bulk-mt8 ceilings of 1059, 975, 201, 54, 56 and 29 MiB/s, and best keeps its deep-band output at 19.57 MB. The remaining zstd win is the non-iso-ratio fastest row, where zstd's 1862 MiB/s headline buys 48 MB of output against our 24 MB, so the two run at different output sizes.

## Compression-ratio sweep

The sweep runs `zstdx-bench ratio` on 5 shapes, 6 tiers, bulk and stream modes and single-thread and 4-worker MT, one deterministic pass per cell with checksums off. The delta is ours over zstd's ratio minus 1, aggregated as a geo-mean, and every cell is roundtrip-gated. The wall time is 93.3 s at parallel 16.

The geo-mean over 120 cells is **+8.70%**. By mode it reads bulk single-thread **+1.47%**, bulk MT **+11.08%**, stream single-thread **+11.46%** and stream MT **+11.11%**. By shape it reads json +4.48%, text +40.29%, skewed +1.42%, random plus or minus zero and zeros +2.06%.

One hundred nineteen cells reproduce the R104 sizes byte for byte. The one mover is the balanced arm's own: json.balanced stream single-thread dropped from 4,707,627 to 4,661,008 B, the bulk cell's exact frame, a 46,619 B gain through the open-stream head-verdict arm at the 8 MiB gate. The losing set is unchanged from R104 at the same bytes: text.opt at -0.29 to -0.31 percent across its modes, json.opt at -0.11 to -0.13, json.fast bulk MT at -0.14% and stream MT at -0.12%, skewed.opt at -0.06% and skewed.ultra at -0.03 to -0.05%. Everything else is at parity or denser, random ties byte-exact at every cell, text.balanced reads +1.65 to +1.71%, and the sweep's wall gain over R104's 113.9 s is the adaptive sample floor, not a cell change.

## Full numeric ladder

The cell text reads ours/zstd throughput followed by the x ratio. Cells whose three minimum rounds already cover the 500 ms budget stop at n=3, which affects json levels 6-22 and skewed 4-22, so those absolutes carry the widest drift band and share the machine in pairs. The asymmetric-cell cap stops random levels 17-22 at n of 3 to 5. x is our time divided by zstd's, so below 1 means we are faster. Every ladder size is byte-identical to R104: the window's two byte-moving levers, the open-stream arm and the pledged-mt floor, sit outside the ladder's bulk cells.

### Ladder speed

| level | json | text | skewed | random | zeros |
|---|---|---|---|---|---|
| 1 | 629/836 x1.33 | 11802/9885 x0.84 | 2586/1192 x0.46 | 1992/1871 x0.94 | 45413/13163 x0.29 |
| 2 | 518/631 x1.22 | 11550/9762 x0.85 | 2554/848 x0.33 | 1988/1900 x0.95 | 45532/13162 x0.29 |
| 3 | 450/465 x1.04 | 12000/7089 x0.59 | 206/220 x1.07 | 1957/1837 x0.94 | 48418/8547 x0.18 |
| 4 | 443/449 x1.01 | 10807/6704 x0.62 | 152/163 x1.07 | 1951/1822 x0.93 | 48596/8535 x0.18 |
| 5 | 208/266 x1.28 | 6864/4548 x0.66 | 2186/127 x0.06 | 2119/1653 x0.78 | 47040/6329 x0.14 |
| 6 | 147/186 x1.26 | 5249/2542 x0.48 | 2050/108 x0.05 | 2135/1624 x0.76 | 44455/2778 x0.06 |
| 7 | 123/162 x1.32 | 4483/2387 x0.54 | 1961/100 x0.05 | 2133/1564 x0.73 | 45864/2756 x0.06 |
| 8 | 98/125 x1.27 | 3824/1624 x0.42 | 1808/81 x0.04 | 2124/1605 x0.76 | 44110/1767 x0.04 |
| 9 | 151/114 x0.74 | 1825/1628 x0.89 | 1735/71 x0.04 | 2114/1348 x0.64 | 31644/1733 x0.06 |
| 10 | 67/84 x1.25 | 3075/1449 x0.47 | 1581/56 x0.04 | 2123/1064 x0.50 | 44469/1691 x0.04 |
| 11 | 50/57 x1.15 | 2468/1400 x0.57 | 1650/35 x0.02 | 2116/949 x0.45 | 44224/1682 x0.04 |
| 12 | 44/49 x1.11 | 2388/761 x0.32 | 1401/20 x0.01 | 2054/485 x0.24 | 43131/909 x0.02 |
| 13 | 23/30 x1.35 | 992/581 x0.58 | 6/8 x1.39 | 2030/108 x0.05 | 44210/628 x0.01 |
| 14 | 17/22 x1.24 | 864/479 x0.55 | 6/8 x1.53 | 2026/75 x0.04 | 41147/545 x0.01 |
| 15 | 18/13 x0.75 | 832/378 x0.45 | 5/6 x1.12 | 1981/72 x0.04 | 40590/477 x0.01 |
| 16 | 8/8 x1.01 | 920/481 x0.52 | 3/5 x1.61 | 2051/11 x0.01 | 42230/990 x0.02 |
| 17 | 7/6 x0.84 | 915/424 x0.46 | 3/2 x0.72 | 2120/9 x0.00 | 42744/802 x0.02 |
| 18 | 3/4 x1.14 | 492/354 x0.72 | 2/2 x1.12 | 3379/8 x0.00 | 35158/779 x0.02 |
| 19 | 3/3 x1.00 | 420/243 x0.58 | 2/1 x0.85 | 3331/6 x0.00 | 34905/566 x0.02 |
| 20 | 3/2 x0.82 | 284/188 x0.66 | 2/1 x0.76 | 2069/6 x0.00 | 32850/333 x0.01 |
| 21 | 2/2 x0.86 | 265/125 x0.47 | 1/1 x0.84 | 2070/7 x0.00 | 30984/186 x0.01 |
| 22 | 2/1 x0.54 | 267/112 x0.42 | 1/1 x0.89 | 2080/9 x0.00 | 42137/167 x0.00 |

The deep json rows hold their lead at the cut's unchanged bytes: levels 17 and 19 read 0.84 and 1.00 against R104's 0.85 and 0.95, level 20 at 0.82 and level 22 at 0.54 keep their byte wins, and level 18 at 1.14 stays the one exception, with levels 13 and 14 at 1.35 and 1.24 holding theirs. Text keeps its chain-row wins at 0.32 to 0.72 through levels 12-22, with level 9 at 0.89 against R104's 0.94. The skewed hole at levels 13-16 reads 1.12 to 1.61 at ratio parity or better, with skewed 17-22 at 0.72 to 1.12. Random levels 17-19 read 2120 to 3379 MiB/s at byte-exact ratio 1.00 through the raw-block fast path. The n=3 cells still wander the widest, with json level 15 at 0.75 and skewed level 16 at 1.61 the largest moves inside that band against R104's 0.78 and 1.44.

### Ladder ratio

The ratio table reads ours over zstd at the same numeric level, where a higher number means we are denser. Every row reproduces the R104 bytes.

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
| 1 | json | 1.27 | 1.42 | 1.54 | 1.35 |
| 1 | text | 1.67 | 1.63 | 2.05 | 1.55 |
| 3 | json | 1.35 | 1.18 | 1.00 | 1.15 |
| 3 | text | 1.44 | 1.14 | 1.12 | 1.03 |
| 9 | json | 1.10 | 0.92 | 0.65 | 1.01 |
| 9 | text | 1.85 | 1.59 | 2.83 | 1.00 |

Every cell reads identical bytes to R104 and the walls hold their bands, with json level 3 at 4K at 1.18 and text at 1.14 against R104's 1.12 and 1.08, and json level 9 at 4K at 0.92 against 0.97. Text level 9 keeps the swap's bytes at x1.85, 1.59, 2.83 and 1.00, and its 64K cell holds 2.83 for the fourth consecutive pass against the 2.76 to 2.86 spread before the run. Level 1's accepted trades are unchanged.
