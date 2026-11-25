# Bench Matrix (2026-11-25)

This page holds the raw measurement tables for the 2026-11-25 release pass. Each section states what the table measures and how to read its columns. Throughout the page `x = ours_time / zstd_time`, so values below 1 mean we are faster, throughput is MiB/s of raw input, and ratio is input bytes divided by output bytes. The comparison target is libzstd 1.6.0 with `zstdmt` enabled, reached through the zstd crate's zstd-sys 2.2.0 binding. Conclusions are in [snapshot.md](snapshot.md), and earlier archives from the 2026-11-23 R111, 2026-11-21 R110, 2026-11-16 R109, 2026-11-07 R104, 2026-11-03 R94, 2026-11-02 R89, 2026-11-01 R86, 2026-10-31, 10-30, 10-25, 10-18, 10-01, 09-28, 09-27 and 09-19 passes live in this file's git history.

## Run provenance

The pass ran on the v0.7.2 release tree, as individual sections under the shared machine lock rather than one `bash bench/pass.sh all` invocation, with no other bench session in between. The machine is an AMD Eng Sample 100-000000870-32_Y, a Zen4-class part with 32 visible cores, AVX-512 and BMI2, a maximum clock of 5386 MHz, and the same machine as every pass since 09-12. The toolchain is rustc 1.102.0-nightly (5f4effd57 2026-11-13), unchanged since R110, in the release profile with codegen-units set to 1. The reference side moved this release: the zstd crate rides 0.14.4 to 0.14.5 with clap to 4.6.10, and zstd-sys 2.1.3 to 2.2.0, bundling libzstd 1.6.0, so the harness header now prints `libzstd 1.6.0, binding 10600`; commit de829310 adapted the reference-side gates to the new build, and the core-five sweep below reproduces every R111 reference byte, so the reference side is byte-flat through the binding move. The corpus is unchanged, with json.raw from 2026-09-15 and the other four shapes from 2026-09-07, `bench/big/dll100.*` the same 2026-09-19 set with dll100.raw at 104857600 bytes, and the real payloads built by `bench/gen_real.sh` from the same pinned downloads as the 2026-11-24 calibration. The suites were green on the release tree: 820 workspace nextest tests, 818 carried plus the 2 the 1.6.0 adaptation fixed, 586 paranoid tests, 321 no-default-features tests, 25 doctests, the ffi C harness against the pinned reference tree (libzstd 1.6.0 at d9c0c7e2), and a parity stride sample reading PARITY OK with 0 hard divergences, 76 oneshot asymmetries and 1 block-cap, the third adjudication kind the 1.6.0 one-shot block bound added. Roundtrip verification was on for every cell. The ratio section ran its stock parallel-16 schedule clean in 87.9 s and the real-axis sweep in 45.6 s.

The best-row verdict fix `b67c8c39` landed mid-pass, so the sections split into two waves: everything through the five ladders and the first stream-ladder invocation ran before it, and the real-corpus ratio and encode sections, the enc-stream-mt8 section and the stream-ladder rerun ran after it. The decode sections are unaffected either way, and the log size lines separate the waves on the moved cells exactly: every pre-fix dll100.best cell reads the wide-ring state at about 19.32 MB and about 1 MiB/s, and every post-fix single-thread cell reads 19,565,854 to 19,565,857 B at 26-33 MiB/s. The fix moved only those dll100.best cells and the jsonreal best stream-mt cells, so every other pre-fix section cell is identical to the release tree, and the tables below carry the post-fix readings wherever the cell moved. The enc-mt8-real and enc-mt16-real invocations produced no cells because the matrix tool routes raw payloads through the enc-st, enc-stream and enc-feed modes only, so the bulk-MT real axes stay unrecorded, and the small section's real-shape invocation emitted no cells, so the small-payload axes stay on the synthetic shapes.

The timing harness is unchanged from R111, the R86 one plus the adaptive sample floor: the enc-st and enc-stream single-thread cells run two at a time, so the header reads `st cells parallel 2`; a warmup round pair at eight times the budget runs twice instead of three; asymmetric cells stop once the slow side spent the full budget and the fast side one sixteenth of it; and every timed encode cell gates on its own last round. The dec-st section stays serial, the stream-ladder section passes `--parallel 1` per the [methodology](methodology.md) page, and absolute MiB/s on the shared cells reads below the serial readings of every pre-R86 table, so cross-pass comparisons on those cells use x. The README figures moved onto the real payloads this release, and `bench/plot_readme.py` reads them from this pass's logs: the decode figure from `dec-st-real.log`, `dec-mt-real.log` and `files-dll.log`, the encode pareto from the post-fix stream-ladder logs under `bench/data/stream-ladder-20261126/`, and the stream-mt8 figure from `enc-stream-mt8.log`.

The window since the R111 pass and the v0.7.1 cut carries the real-corpus follow-through and the best-row verdict family. The bench-side commits are the encode-pareto plot refactor (58908134), the real-corpus axes `ea548613` whose follow-through this pass records as side sections, and the row-scan guard hoist from the real-corpus diet (740bea9b). The engine commits are the best-tier store gate with the per-frame tree-domain verdict (5bcceab1), the plain-lattice jobs resolving the balanced row's storage verdict (c9605c7a), the 1.6.0 reference-gate adaptation (de829310) and the verdict fix `b67c8c39`, with the README figure move and the version bump riding the release. Determinism held everywhere the bytes must hold: the core-five sweep moved exactly 12 zstdx-side cells against R111, all best-tier, and every other cell reproduces R111 byte for byte with the reference side byte-flat; the fix's own gates kept the 120-cell sweep byte-identical at the geo-mean below and moved exactly one real-axis cell, jsonreal.best.stream-mt. The full ladder reproduces R111 byte for byte outside the best-tier rows 13-15, which carry the store gate's moves: json 13-15 denser at 7.24 against R111's 6.26, 6.32 and 6.32, skewed 13-15 at 2.00 against 1.84, and text 13-15 giving back 81 bytes per cell.

## Decode single-thread

Throughput is MiB/s of raw data for bulk and streaming decode with 64 KiB pulls. x is our time divided by zstd's time, so below 1 means we are faster, and the harness spread is at most ±0.02 on every cell.

| file | bulk ours | bulk zstd | bulk x | stream ours | stream zstd | stream x |
|---|---:|---:|---:|---:|---:|---:|
| json.zst1 | 2281 | 2137 | 0.94 | 2140 | 2063 | 0.96 |
| json.zst3 | 2100 | 1881 | 0.90 | 1848 | 1748 | 0.95 |
| json.zst9 | 2452 | 2220 | 0.91 | 2117 | 2037 | 0.96 |
| json.zst19 | 2674 | 2531 | 0.95 | 2562 | 2534 | 0.99 |
| text.zst1 | 7411 | 6608 | 0.89 | 7396 | 7418 | 1.00 |
| text.zst3 | 9577 | 9077 | 0.95 | 10977 | 11026 | 1.00 |
| text.zst9 | 10095 | 9760 | 0.97 | 11847 | 12058 | 1.02 |
| text.zst19 | 10146 | 9726 | 0.96 | 11588 | 12048 | 1.04 |
| skewed.zst1 | 3172 | 3069 | 0.97 | 3045 | 2816 | 0.93 |
| skewed.zst3 | 1656 | 1584 | 0.96 | 1550 | 1502 | 0.97 |
| skewed.zst9 | 958 | 792 | 0.83 | 809 | 717 | 0.89 |
| skewed.zst19 | 2908 | 2972 | 1.02 | 2705 | 2681 | 0.99 |
| random.zst3 | 8887 | 8903 | 1.00 | 10956 | 8878 | 0.81 |
| zeros.zst3 | 12020 | 11280 | 0.94 | 12889 | 12915 | 1.00 |
| dll100.zst1 | 1483 | 1520 | 1.02 | 1429 | 1462 | 1.02 |
| dll100.zst3 | 1728 | 1669 | 0.97 | 1655 | 1628 | 0.98 |
| dll100.zst9 | 2113 | 2038 | 0.96 | 2011 | 1977 | 0.98 |
| dll100.zst19 | 1834 | 1745 | 0.95 | 1761 | 1719 | 0.98 |

Every corpus shape reads at or under parity bulk except skewed.zst19 at 1.02, with skewed.zst9 the widest win at 0.83, and the stream column holds 0.89 to 1.04 with random's 0.81 the widest win. The dll100 bulk band reads 0.95 to 1.02 against R111's 0.93 to 1.01, and the stream band 0.98 to 1.02 against R111's 0.96 to 1.01. The pull sweep reads dll100 streaming at 0.96 to 1.03 across every pull size. The reference side reads 1 to 3 percent above its R111 absolutes on the json and dll100 cells through the binding move while our side held, so the x cells on those shapes carry that reference move; the text, skewed, random and zeros references stayed flat. The multi payload has no cell in this section this pass; its solo single-thread decode reads 3366 MiB/s in the files-multi section below, and no zstd-side multi cell ran.

## Decode single-thread, real corpus

The real payloads run the same interleaved bulk and streaming cells as the section above, with the streaming rows at five pull sizes. The table carries the 64 KiB pull; the jsonreal stream band across every pull reads 1.03 to 1.08, textreal 0.89 to 0.99, and this log's dll100 rows, the same fixture as the section above, read 0.95 to 1.03.

| file | bulk ours | bulk zstd | bulk x | stream ours | stream zstd | stream x |
|---|---:|---:|---:|---:|---:|---:|
| jsonreal.zst1 | 2822 | 2955 | 1.05 | 2716 | 2894 | 1.07 |
| jsonreal.zst3 | 2864 | 2896 | 1.01 | 2774 | 2871 | 1.03 |
| jsonreal.zst9 | 3180 | 3233 | 1.02 | 3050 | 3162 | 1.04 |
| textreal.zst1 | 1761 | 1712 | 0.97 | 1660 | 1634 | 0.98 |
| textreal.zst3 | 1614 | 1423 | 0.88 | 1457 | 1309 | 0.90 |
| textreal.zst9 | 1635 | 1451 | 0.89 | 1468 | 1311 | 0.89 |

Jsonreal decodes at 1.01 to 1.08 of the reference time on every cell, textreal ahead at 0.88 to 0.99 with its widest wins on the zst3 and zst9 cells. This section is the README decode figure's data source.

## Decode multi-thread scaling

Throughput is MiB/s at 1, 2, 4, 8 and 16 workers, and libzstd has no MT decode, so the reference column is its single-thread streaming cell. The dll row comes from `files --threads`, the solo tool at the same budget, with its reference taken from this pass's `dll100.zst3` streaming cell.

| file | ours ST | zstd stream ST ref | mt2 | mt4 | mt8 | mt16 |
|---|---:|---:|---:|---:|---:|---:|
| json.zst3 | 2009 | 1747 | 2648 | 3039 | 3190 | 3211 |
| text.zst3 | 11636 | 11068 | 11375 | 11194 | 11429 | 11326 |
| skewed.zst9 | 965 | 716 | 1371 | 1598 | 1737 | 1719 |
| random.zst3 | 9723 | 8780 | 9519 | 9625 | 9673 | 9099 |
| dll100.zst3 | 1730 | 1628 | 2436 | 2886 | 3014 | 3131 |

Json mt16 runs at 1.84 times the reference and 1.60 times its own single-thread, dll100 mt16 at 1.92 times the reference and 1.81 times its own, so the strongest single-frame rows hold. Skewed reads 1.80 times single-thread and 2.42 times the reference at mt8 against R111's 1.80 and 2.49, with mt16 at 1.78 and 2.40. Text stays flat at 0.97 and random at 0.94 to 0.99 times single-thread. This section's json st1 cell read 2009 against dec-st's 2100, a 4 percent draw of that solo cell. The remaining walls are sequence-read and aggregate-bandwidth bound, tracked as todo 1 in the [todo](../todo.md).

The real payloads run the same solo scaling section, and their denser frames give the segment-parallel decoder more work per staging span:

| file | ours ST | zstd stream ST ref | mt2 | mt4 | mt8 | mt16 |
|---|---:|---:|---:|---:|---:|---:|
| jsonreal.zst3 | 2981 | 2869 | 3739 | 4127 | 4313 | 4154 |
| textreal.zst3 | 1603 | 1311 | 1897 | 2272 | 2756 | 2850 |

Jsonreal mt16 reads 1.45 times the reference and textreal mt16 2.17 times, against the corpus cells' 0.97 to 1.92 band.

The files-multi section times the four multi-frame payloads at every worker count, the lane budget's home ground:

| payload | ST | mt2 | mt4 | mt8 | mt16 | mt16 / ST |
|---|---:|---:|---:|---:|---:|---:|
| multi.zst3 | 3366 | 4213 | 6119 | 9075 | 9757 | 2.90 |
| multi_small.zst3 | 2503 | 2855 | 4572 | 5971 | 7104 | 2.84 |
| multi_balanced.zst3 | 2492 | 3015 | 3974 | 6287 | 8039 | 3.23 |
| multi_twodom.zst3 | 3419 | 4410 | 4851 | 4807 | 5529 | 1.62 |

Multi Balanced at mt16 runs 3.23 times its single-thread decode, multi 2.90 and multi_small 2.84, against the single-frame corpus cells' 0.94 to 1.80 band, so the lane budget's staging split holds at release scale. The two-domain payload scales at 1.62, its second frame's restart-poor half setting the pace, which is the R101 twodom pricing's predicted shape.

## Encode single-thread bulk

Throughput and ratio are per shape with checksums off on both sides, and the tier column names the ladder tier. The pairing is libzstd levels 1, 3, 9, 13, 17 and 19, and x is our time divided by zstd's, so below 1 means we are faster. These cells share the machine in pairs, so the absolute MiB/s reads below the serial tables and x is the comparable column. The dll row carries the post-fix real-corpus section's reading; the core-five section ran before the verdict fix, and its only moved cell is dll100.best.

| level | shape | ours MiB/s | ours ratio | zstd MiB/s | zstd ratio | x |
|---|---:|---:|---:|---:|---:|---:|
| fastest | json | 616 | 6.41 | 818 | 6.11 | 1.33 |
| fastest | text | 11702 | 309.23 | 8103 | 308.94 | 0.69 |
| fastest | skewed | 2634 | 2.00 | 1177 | 2.00 | 0.45 |
| fastest | random | 2075 | 1.00 | 1934 | 1.00 | 0.93 |
| fastest | zeros | 47867 | 32577 | 9978 | 32171 | 0.21 |
| fastest | dll | 484 | 4.36 | 513 | 2.19 | 1.05 |
| fast | json | 465 | 5.30 | 467 | 5.29 | 1.00 |
| fast | text | 12100 | 333.03 | 7078 | 332.90 | 0.59 |
| fast | skewed | 208 | 1.92 | 217 | 1.92 | 1.04 |
| fast | random | 2122 | 1.00 | 1948 | 1.00 | 0.91 |
| fast | zeros | 47437 | 32577 | 8477 | 32171 | 0.18 |
| fast | dll | 420 | 4.98 | 412 | 3.40 | 0.98 |
| balanced | json | 144 | 7.20 | 114 | 5.95 | 0.77 |
| balanced | text | 1810 | 384.66 | 1627 | 378.41 | 0.90 |
| balanced | skewed | 1831 | 2.00 | 69 | 1.84 | 0.038 |
| balanced | random | 2187 | 1.00 | 1384 | 1.00 | 0.62 |
| balanced | zeros | 29681 | 32577 | 1738 | 32202 | 0.059 |
| balanced | dll | 138 | 5.36 | 139 | 4.62 | 1.00 |
| best | json | 65 | 7.24 | 26 | 6.10 | 0.40 |
| best | text | 989 | 386.06 | 601 | 385.90 | 0.61 |
| best | skewed | 464 | 2.00 | 8 | 1.84 | 0.018 |
| best | random | 2013 | 1.00 | 116 | 1.00 | 0.058 |
| best | zeros | 31204 | 32577 | 627 | 32171 | 0.020 |
| best | dll | 26 | 5.36 | 30 | 4.67 | 1.14 |
| opt | json | 7 | 7.48 | 5 | 7.49 | 0.80 |
| opt | text | 929 | 408.84 | 414 | 410.11 | 0.45 |
| opt | skewed | 3 | 2.00 | 2 | 2.00 | 0.69 |
| opt | random | 2117 | 1.00 | 9 | 1.00 | 0.004 |
| opt | zeros | 37197 | 32577 | 800 | 32202 | 0.022 |
| opt | dll | 13 | 6.03 | 11 | 5.06 | 0.85 |
| ultra | json | 3 | 7.42 | 3 | 7.42 | 0.92 |
| ultra | text | 493 | 414.12 | 246 | 413.98 | 0.50 |
| ultra | skewed | 2 | 2.00 | 2 | 2.00 | 0.94 |
| ultra | random | 2214 | 1.00 | 6 | 1.00 | 0.003 |
| ultra | zeros | 43249 | 32577 | 600 | 32202 | 0.014 |
| ultra | dll | 9 | 6.32 | 8 | 5.28 | 0.94 |

The best row is this table's story. The store gate and the per-frame tree-domain verdict (5bcceab1) moved json.best 13.5 percent denser to ratio 7.24 against zstd's unchanged 6.10 and skewed.best 7.7 percent denser to 2.00 against 1.84, at byte-identical opt and ultra output, and the near verdict's speed win holds through the fix's gates at 74-77 MiB/s for json.best and 544-569 MiB/s for skewed.best; this pass's own cells read json.best at 65 MiB/s x0.40, an n=3 draw of the same class, and skewed.best at 464 MiB/s x0.018, both against R111's x1.41 and 1.53 losses. Dll100.best carries the verdict fix's Dense class: the pre-fix sections read it collapsed at about 1 MiB/s on the wide ring's 19,321,629 B, the fix returns it to the R111 class in every mode at 19,565,858 bulk bytes, and this pass's post-fix cells read 26-27 MiB/s single-thread with the mt cells near 50. The reference side's 10 GB/s-class fastest cells read below their R111 absolutes this pass (text 8103 against 9945, zeros 9978 against 13153), so the x cells on those shapes carry the reference move. The balanced row reads healthy in-pass with no layout-lottery carry: json.balanced at 0.77 against R111's 0.78, text.balanced at 0.90 against 0.89, and the fast rows at 1.00 and 0.59. The dll rows keep their landed geometry: balanced at 19,570,844 B against zstd's 22,701,602, 13.8 percent smaller, best at 19,565,854 against 22,456,971, 12.9 percent smaller, fastest and fast at 49.8 and 31.8 percent smaller. Checksums on versus off on our side read json.fast 1.00 and text.fast 0.85.

## Encode single-thread bulk, real corpus

The real payloads run the same six-tier cells; every row is post-fix. Dll100's rows sit in the table above, measured in the same section.

| level | shape | ours MiB/s | ours ratio | zstd MiB/s | zstd ratio | x |
|---|---:|---:|---:|---:|---:|---:|
| fastest | jsonreal | 801 | 7.94 | 1114 | 7.89 | 1.37 |
| fastest | textreal | 240 | 2.47 | 451 | 2.45 | 1.88 |
| fast | jsonreal | 772 | 9.11 | 867 | 9.09 | 1.11 |
| fast | textreal | 247 | 2.82 | 261 | 2.82 | 1.05 |
| balanced | jsonreal | 189 | 11.86 | 207 | 10.69 | 1.10 |
| balanced | textreal | 53 | 3.23 | 72 | 3.21 | 1.37 |
| best | jsonreal | 33 | 12.20 | 40 | 10.81 | 1.18 |
| best | textreal | 6 | 3.44 | 11 | 3.33 | 1.84 |
| opt | jsonreal | 15 | 12.16 | 12 | 12.19 | 0.86 |
| opt | textreal | 5 | 3.57 | 4 | 3.59 | 0.82 |
| ultra | jsonreal | 8 | 12.75 | 6 | 12.76 | 0.82 |
| ultra | textreal | 3 | 3.68 | 3 | 3.68 | 0.99 |

The tier map matches the 2026-11-24 calibration: the fast tiers lose their synthetic amplification (jsonreal fastest at x1.37 against the synthetic 1.33, textreal fastest at 1.88 against 0.69), the balanced tier stays denser but slower (jsonreal at +11.01 percent size and x1.10, textreal at +0.61 and 1.37), and the deep tiers read ahead (jsonreal opt and ultra at 0.86 and 0.82, textreal at 0.82 and 0.99). Jsonreal.best reads +12.94 percent against libzstd-13 at 2,749,265 B, past its own balanced cell, and textreal.best +3.38 percent at x1.84.

## Encode multi-thread bulk

Throughput is MiB/s with a cold pool per call on both sides at 8 workers, and the ratio columns are at mt8. No mt16 bulk section ran this pass. The x8 column is our time divided by zstd's at the same worker count, so below 1 means we are faster.

| cell | ours mt8 | zstd mt8 | x8 | ratio ours mt8 | ratio zstd mt8 |
|---|---:|---:|---:|---:|---:|
| json.fastest | 3538 | 3971 | 1.13 | 6.38 | 6.11 |
| json.fast | 2344 | 993 | 0.41 | 5.30 | 5.31 |
| json.balanced | 391 | 198 | 0.49 | 7.19 | 5.95 |
| text.fastest | 18465 | 10437 | 0.56 | 309.15 | 39.78 |
| text.fast | 11817 | 2015 | 0.17 | 332.94 | 189.16 |
| text.balanced | 1229 | 1202 | 0.96 | 384.59 | 378.30 |
| skewed.fastest | 4082 | 3141 | 0.77 | 2.00 | 2.00 |
| skewed.fast | 1231 | 601 | 0.48 | 1.92 | 1.92 |
| skewed.balanced | 770 | 116 | 0.14 | 2.00 | 1.84 |

Json.fastest.mt8 at x1.13 stays the one zstd MT win, inside the cell's known noise band of 0.95 to 1.65 across passes. Every other cell is ahead, with text.balanced reading 0.96 against R111's 0.88 inside its 0.80 to 1.06 spread and skewed.balanced at 0.14 against 0.20. The log's warm-pool reference reads zstd json.fast mt16 at 1556 MiB/s against R111's 1625.

## Encode streaming

Throughput uses 64 KiB pulls and interleaved medians, with single-thread and MT8 columns from the post-fix enc-stream-mt8 section, which covers the corpus, the real payloads and dll100 in one run. x is our time divided by zstd's at the same mode, so below 1 means we are faster.

| cell | ST ours | ST zstd | ST x | MT8 ours | MT8 zstd | MT8 x |
|---|---:|---:|---:|---:|---:|---:|
| json.fastest | 608 | 744 | 1.23 | 3356 | 1828 | 0.54 |
| json.fast | 455 | 433 | 0.95 | 2315 | 392 | 0.17 |
| json.balanced | 153 | 97 | 0.64 | 366 | 109 | 0.30 |
| json.best | 72 | 27 | 0.39 | 339 | 50 | 0.15 |
| json.opt | 7 | 5 | 0.75 | 32 | 6 | 0.20 |
| json.ultra | 3 | 2 | 0.80 | 11 | 3 | 0.25 |
| text.fastest | 6536 | 1704 | 0.26 | 8014 | 3474 | 0.44 |
| text.fast | 6476 | 5025 | 0.77 | 5654 | 1603 | 0.28 |
| text.balanced | 1483 | 1586 | 1.07 | 1173 | 915 | 0.78 |
| text.best | 973 | 871 | 0.90 | 309 | 302 | 0.98 |
| text.opt | 879 | 396 | 0.45 | 754 | 356 | 0.47 |
| text.ultra | 480 | 239 | 0.50 | 421 | 226 | 0.54 |
| jsonreal.fastest | 773 | 1003 | 1.30 | 1155 | 1840 | 1.63 |
| jsonreal.fast | 758 | 737 | 0.97 | 1081 | 627 | 0.58 |
| jsonreal.balanced | 187 | 193 | 1.04 | 415 | 189 | 0.47 |
| jsonreal.best | 34 | 41 | 1.21 | 94 | 73 | 0.76 |
| jsonreal.opt | 14 | 12 | 0.80 | 61 | 14 | 0.22 |
| jsonreal.ultra | 7 | 6 | 0.78 | 25 | 7 | 0.26 |
| textreal.fastest | 241 | 428 | 1.77 | 626 | 1491 | 2.48 |
| textreal.fast | 263 | 251 | 0.95 | 622 | 242 | 0.39 |
| textreal.balanced | 54 | 63 | 1.17 | 113 | 130 | 1.14 |
| textreal.best | 6 | 12 | 1.86 | 25 | 21 | 0.87 |
| textreal.opt | 5 | 4 | 0.78 | 18 | 5 | 0.28 |
| textreal.ultra | 3 | 3 | 0.92 | 10 | 3 | 0.35 |

The pre-fix corpus enc-stream run read the same cells at their healthy levels (text.fastest 6722 at x0.26, json.fastest 608 at 1.22, text.balanced 1494 at 1.07), so this session carries no pairing split: both runs agree, and the table carries the post-fix one. The balanced row reads healthy in both, closing R111's solo-median carry. Json.best reads 72 MiB/s at x0.39 single-thread and 339 MiB/s at x0.15 at mt8, the near verdict holding through the streaming path; the text.best mt8 cell reads x0.98 on an n=5 draw whose band runs 0.56 to 1.56, against R111's 0.35. On the real payloads the fast tiers read at or ahead on jsonreal (fast x0.97, balanced 1.04) while textreal's fastest and balanced rows trail at 1.77 and 1.17, and jsonreal's balanced stream-mt8 cell wins at x0.47 on the plain-lattice jobs. Unknown-size text.fastest streaming has zstd emitting 1.84 MB at ratio 18.3 against our 108 KB at ratio 309.

The dll100 stream cells read single-thread fastest 484 against 495 at x1.02, fast 424 against 390 at 0.91, balanced 140 against 131 at 0.94, best 27 against 29 at 1.08, opt 13 against 11 at 0.78 and ultra 9 against 8 at 0.88. Stream-mt8 reads fastest 909 against 1761 at x1.90, fast 844 against 422 at 0.50, balanced 166 against 202 at 1.19, best 50 against 70 at 1.41, opt 51 against 24 at 0.47 and ultra 27 against 17 at 0.63. The cells run at 91, 95, 84, 98, 94 and 90 percent of the bulk-mt8 ceilings of 999, 885, 198, 51, 54 and 30 MiB/s. The three trailing tiers carry the capture's density: zstd's 1761 MiB/s fastest headline buys 48 MB of output against our 24 MB, and the balanced and best rows run at the mt cells' job-split bytes, so those cells are not iso-ratio. Json.best.stream-mt8 holds at x0.15.

## Stream ladder

The stream-ladder section runs `matrix --mode enc-stream --full-ladder --no-mt --parallel 1` over json, text, jsonreal, textreal and dll100, levels 1-22 on both sides at 64 KiB pulls, and feeds the README encode figure cell by cell. The pass's first invocation predates the verdict fix and its dll100 rows 13-15 read the wide-ring state at about 1 MiB/s, so the recorded ladder is the post-fix rerun under `bench/data/stream-ladder-20261126/`, whose only byte changes against the first invocation are those three dll rows. On the recorded ladder, dll100.l13 reads 33 against 35 MiB/s at x1.09 on 19,565,857 B, json.l13-15 read 74-77 MiB/s at x0.21 to 0.43, and the corpus rows agree with the ladder section below at their shared n classes.

## Compression-ratio sweep

The sweep runs `zstdx-bench ratio` on the 5 synthetic shapes, 6 tiers, bulk and stream modes and single-thread and 4-worker MT, one deterministic pass per cell with checksums off. The delta is ours over zstd's ratio minus 1, aggregated as a geo-mean, and every cell is roundtrip-gated. The wall time is 87.9 s at the stock parallel 16.

The geo-mean over 120 cells is **+9.51%**, up from R111's +8.70% on the best-row wins alone. By mode it reads bulk single-thread **+2.24%**, bulk MT **+11.92%**, stream single-thread **+12.30%** and stream MT **+11.95%**. By shape it reads json +7.05%, text +40.27%, skewed +2.78%, random plus or minus zero and zeros +2.06%.

Exactly 12 zstdx-side cells moved against R111, all best-tier, from the store gate: json.best at -13.5 to -13.6 percent bytes across its 4 modes (+18.63 to +18.66 ratio delta), skewed.best at -7.7 percent (+8.46 to +8.48), and text.best at +81 bytes per cell (+0.04 to +0.11 ratio delta). Every other cell reproduces the R111 sizes byte for byte and the zstd reference side is byte-flat through the 1.6.0 binding move, so the window's other commits moved no corpus byte. The losing set is unchanged from R111 at the same bytes: text.opt at -0.29 to -0.31 percent across its modes, json.opt bulk-st at -0.13% and stream-st at -0.11% with the MT modes at parity, json.fast bulk MT at -0.14% and stream MT at -0.12%, skewed.opt at -0.06% and skewed.ultra at -0.03 to -0.05%. Everything else is at parity or denser, random ties byte-exact at every cell, and text.balanced reads +1.65 to +1.71%.

## Real-corpus ratio sweep

The same sweep over the two real shapes runs 48 cells and took 45.6 s. The geo-mean is **+1.79%**, reading jsonreal **+3.26%** and textreal **+0.34%**; by mode it reads bulk-st +2.30%, bulk-mt +1.55%, stream-st +2.38% and stream-mt +0.92%. The best row is the strongest tier at +8.06 to +8.51 percent by mode over the two shapes, +12.94 to +13.74 on jsonreal alone. The jsonreal.best.stream-mt cell reads 2,754,919 B at +13.51% against libzstd's 3,127,129, the cell the verdict fix restored from the strip-keyed 3,910,629.

The recorded deficits sit in the MT modes and the balanced row's stream side. Jsonreal.balanced reads +11.01% bulk-st and +11.08% stream-st but +5.00% bulk-mt and +0.89% stream-mt on the plain-lattice jobs, and textreal.balanced reads +0.61% and +0.63% single-threaded but -0.27% bulk-mt and -2.30% stream-mt, the worst cell of the sweep and the un-ported stream-side storage substitution tracked as todo 42 in the [todo](../todo.md). The opt and ultra MT cells read behind their single-thread cells on both shapes: jsonreal.opt -1.81%, jsonreal.ultra -0.60%, textreal.opt -1.66% and textreal.ultra -1.23% at mt, against -0.25, -0.03, -0.73 and -0.01 single-threaded.

## Full numeric ladder

The cell text reads ours/zstd throughput followed by the x ratio. Cells whose three minimum rounds already cover the 500 ms budget stop at n=3, which affects json levels 6-22 and skewed 4-22, so those absolutes carry the widest drift band and share the machine in pairs. The asymmetric-cell cap stops random levels 17-22 at n of 3 to 5. x is our time divided by zstd's, so below 1 means we are faster. Every ladder size outside the best-tier rows 13-15 is byte-identical to R111 on both sides.

### Ladder speed

| level | json | text | skewed | random | zeros |
|---|---|---|---|---|---|
| 1 | 630/812 x1.29 | 11518/8061 x0.70 | 2640/1205 x0.46 | 1993/1897 x0.96 | 42257/9901 x0.23 |
| 2 | 555/656 x1.18 | 11701/8007 x0.69 | 2544/782 x0.31 | 2010/1881 x0.93 | 43914/9939 x0.23 |
| 3 | 464/472 x1.02 | 11833/6980 x0.59 | 208/217 x1.05 | 1955/1818 x0.93 | 46467/8490 x0.18 |
| 4 | 441/426 x0.97 | 10798/6640 x0.62 | 149/159 x1.07 | 2042/1899 x0.93 | 46421/8404 x0.18 |
| 5 | 205/265 x1.29 | 6985/4480 x0.64 | 2109/125 x0.06 | 2305/1724 x0.75 | 44063/6293 x0.14 |
| 6 | 147/187 x1.27 | 5510/2535 x0.46 | 2002/106 x0.05 | 2215/1669 x0.75 | 44325/2772 x0.06 |
| 7 | 125/160 x1.27 | 4580/2397 x0.53 | 1826/98 x0.05 | 2242/1606 x0.71 | 43638/2747 x0.06 |
| 8 | 101/124 x1.22 | 3838/1609 x0.42 | 1718/79 x0.05 | 2253/1667 x0.74 | 40736/1764 x0.04 |
| 9 | 147/113 x0.78 | 1780/1621 x0.91 | 1719/68 x0.04 | 2244/1393 x0.62 | 31185/1743 x0.06 |
| 10 | 67/83 x1.23 | 3121/1450 x0.46 | 1596/52 x0.03 | 2261/898 x0.41 | 40671/1694 x0.04 |
| 11 | 51/57 x1.12 | 2568/1392 x0.55 | 1704/33 x0.02 | 2212/1113 x0.50 | 39770/1677 x0.04 |
| 12 | 45/46 x1.03 | 2484/775 x0.31 | 1449/19 x0.01 | 1964/514 x0.26 | 40429/887 x0.02 |
| 13 | 71/29 x0.40 | 872/581 x0.66 | 442/7 x0.02 | 2112/122 x0.05 | 32258/623 x0.02 |
| 14 | 57/20 x0.35 | 873/491 x0.56 | 144/8 x0.05 | 2109/76 x0.04 | 32499/547 x0.02 |
| 15 | 61/12 x0.20 | 864/384 x0.45 | 150/6 x0.04 | 2129/72 x0.03 | 32643/478 x0.015 |
| 16 | 8/8 x0.99 | 949/486 x0.52 | 3/4 x1.51 | 2085/12 x0.006 | 41982/988 x0.02 |
| 17 | 7/5 x0.80 | 944/432 x0.46 | 2/2 x0.71 | 2222/9 x0.004 | 40525/816 x0.02 |
| 18 | 3/4 x1.04 | 542/361 x0.67 | 2/2 x1.06 | 3311/7 x0.002 | 34465/786 x0.02 |
| 19 | 3/2 x0.89 | 498/248 x0.50 | 2/1 x0.82 | 3432/6 x0.002 | 32953/593 x0.02 |
| 20 | 3/2 x0.73 | 320/196 x0.61 | 1/1 x0.75 | 2264/5 x0.002 | 31811/353 x0.01 |
| 21 | 2/2 x0.82 | 302/139 x0.45 | 1/1 x0.86 | 2228/7 x0.003 | 27770/202 x0.007 |
| 22 | 2/1 x0.53 | 327/122 x0.37 | 1/1 x0.87 | 2224/9 x0.004 | 31826/175 x0.005 |

The best-tier rows carry the window's story. Json 13-15 read x0.20 to 0.40 at the store gate's bytes against R111's 1.31, 1.28 and 0.77, and skewed 13-15 read x0.02 to 0.05 against R111's 1.38, 1.51 and 1.12, closing the skewed hole the R111 record carried: skewed 13-15 now win at 144 to 442 MiB/s against zstd's 6 to 8. The remaining skewed losses sit at level 16 at 1.51 and level 18 at 1.06, and json's deep rows hold their R111 shape at 0.73 to 1.04. Text keeps its chain-row wins, levels 12-22 at 0.31 to 0.67 with the balanced level 9 cell at a healthy 0.91 in-pass. Random levels 17-19 read 2222 to 3432 MiB/s at byte-exact ratio 1.00 through the raw-block fast path. Cross-pass absolute moves on the deep rows stay inside the wander the methodology records for the n=2 and n=3 class.

### Ladder ratio

The ratio table reads ours over zstd at the same numeric level, where a higher number means we are denser. Every row outside 13-15 reproduces the R111 bytes.

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
| 13 | 7.24/6.10 | 386.06/385.90 | 2.00/1.84 | 1.00/1.00 | 32577/32171 |
| 14 | 7.24/6.13 | 388.09/388.11 | 2.00/1.84 | 1.00/1.00 | 32577/32171 |
| 15 | 7.24/6.16 | 388.96/388.95 | 2.00/1.84 | 1.00/1.00 | 32577/32171 |
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
| 1 | json | 1.31 | 1.40 | 1.60 | 1.36 |
| 1 | text | 1.66 | 1.56 | 1.85 | 1.53 |
| 3 | json | 1.32 | 1.16 | 0.99 | 1.15 |
| 3 | text | 1.37 | 1.12 | 1.06 | 1.02 |
| 9 | json | 1.11 | 0.96 | 0.67 | 1.03 |
| 9 | text | 1.79 | 1.57 | 2.82 | 1.01 |

The cells sit under the dump gates at deterministic sizes and the walls hold their bands, with json level 3 at 4K at 1.16 and text at 1.12 against R111's 1.17 and 1.15, and json level 9 at 4K at 0.96 against 0.95. Text level 9 keeps the swap's bytes at x1.79, 1.57, 2.82 and 1.01, and its 64K cell reads 2.82 for the seventh consecutive pass inside the 2.76 to 2.86 spread. Level 1's accepted trades are unchanged.
