# Performance Snapshot (2026-11-25)

This page summarizes the 2026-11-25 release pass for v0.7.2, run on one machine, one corpus and the same flags on both sides under the shared lock, as individual sections rather than one `pass.sh all` invocation. The comparison target is the zstd crate's zstd-sys 2.2.0 binding over libzstd 1.6.0 with zstdmt, and the ladder pairs fastest, fast, balanced, best, opt and ultra against libzstd levels 1, 3, 9, 13, 17 and 19. Raw tables, provenance and per-chunk commands are in [matrix.md](matrix.md), including the full numeric 1-22 ladder, the small-payload table and the real-corpus tables. Throughout this page x is our time divided by the reference time, so values below 1 mean we are faster, and speeds are MiB/s of raw input.

The pass covers the window since the 2026-11-23 R111 pass and the v0.7.1 cut: the real-corpus axes landing and this pass's recording of them as side sections, the row-scan guard hoist from the real-corpus diet, the best-tier store gate with the per-frame tree-domain verdict, the plain-lattice jobs resolving the balanced row's storage verdict, the 1.6.0 reference-gate adaptation, and the best-row verdict fix that rode the release, with the README figure move and the version bump. The verdict fix landed mid-pass, so the sections split into a pre-fix and a post-fix wave; the fix moved only dll100.best cells and the jsonreal best stream-mt cells, and the tables carry the post-fix readings there. The reference side moved to libzstd 1.6.0 through zstd-sys 2.2.0 and is byte-flat at every sweep cell against R111.

Caveats for reading every table: the pass runs the R86 harness with the adaptive sample floor, whose enc-st and enc-stream single-thread cells share the machine in pairs, so absolute MiB/s on those cells reads below the serial tables of pre-R86 passes and cross-pass comparisons use x; a verdict on a 10 GB/s-class cell needs a `--parallel 1` rerun per the [methodology](methodology.md). Noise runs ±10% between sessions. The toolchain holds at rustc 1.102.0-nightly since R110. The reference side reads 1 to 3 percent above its R111 absolutes on the json and dll100 decode cells while our side held, and its 10 GB/s-class fastest cells read below R111 (text.fastest bulk 8103 against 9945, zeros.fastest 9978 against 13153), so the x cells on those shapes carry the reference move. Both enc-stream runs read the fast rows at their healthy level, so this session carries no pairing split and no lottery carry.

## Headline results

The best-row verdict family is the release's story. The store gate moved json.best 13.5 percent denser to ratio 7.24 against libzstd-13's 6.10 and skewed.best 7.7 percent denser to 2.00 against 1.84, past their own balanced-tier cells, and the tree-domain verdict's speed win holds through the fix's gates: json.best reads 74-77 MiB/s at x0.472 against R111's x1.41, skewed.best 544-569 MiB/s at x0.021 against 1.53, and this pass's own cells read json.best at 65 MiB/s x0.40 and 72 MiB/s x0.39 on the n=3 wander class, skewed.best at 464 MiB/s x0.018. The verdict fix's Dense class returns dll100.best to its R111 class in every mode, 26-33 MiB/s single-threaded across the post-fix sections and about 50 at mt8, at 19,565,858 bulk bytes with the wide ring's +1.25 percent given back, and the frame-head keying restores jsonreal.best.stream-mt to 2,754,919 bytes at +13.51 percent against libzstd. Text.best gives back 81 bytes per cell. The mechanism and the fix's own gates are in [matchers](../perf/matchers.md).

The real-corpus axes are recorded for the first time. The 48-cell ratio sweep reads a +1.79% geo-mean, jsonreal +3.26% and textreal +0.34%, with the best row the strongest tier at +12.94 to +13.74 percent on jsonreal. Decode reads jsonreal at x1.01 to 1.08, textreal ahead at 0.88 to 0.99 and dll100 at 0.95 to 1.03; MT decode scales jsonreal.zst3 to 1.45 and textreal.zst3 to 2.17 times the zstd stream reference at mt16. Streaming encode reads the real payloads at fast x0.97 and balanced 1.04 on jsonreal, textreal fastest and balanced trailing at 1.77 and 1.17, and jsonreal.balanced.stream-mt8 winning at x0.47. The worst recorded cell is textreal.balanced.stream-mt at -2.30%, the un-ported stream-side storage substitution tracked as todo 42. The README figures now draw their cells from these payloads.

Decode single-thread reads x0.83 to 1.02 bulk and x0.89 to 1.04 stream on json, text and skewed, with skewed.zst9 at 0.83 the widest win and random.zst3 stream at 0.81. Dll100 holds its band at 0.95 to 1.02 bulk and 0.98 to 1.02 stream, and the pull sweep reads 0.96 to 1.03 across 4 KiB to 1 MiB pulls.

Encode multi-thread keeps one zstd win, json.fastest.mt8 at x1.13 inside its known noise band of 0.95 to 1.65. Every other cell is ahead, text.balanced at 0.96 against R111's 0.88 inside its spread, and skewed.balanced at 0.14. No mt16 bulk section ran this pass.

Streaming encode single-thread reads json.best 0.39, json.opt 0.75 and json.ultra 0.80, text.best 0.90, text.opt 0.45 and text.ultra 0.50. The balanced row reads text at 1.07 and json at 0.64, and the fast rows at 0.95 and 0.77. Stream-mt8 keeps every json cell at or below x0.54, wins five of six text cells with balanced at 0.78, and reads text.best at 0.98 on an n=5 wander draw. Dll100 stream-mt8 leads at fast 0.50, opt 0.47 and ultra 0.63, and trails at fastest 1.90, balanced 1.19 and best 1.41, running at 84 to 98 percent of its own bulk-mt8 ceilings.

Decode MT held its scaling: json mt16 at 1.84 times the zstd stream reference, dll100 at 1.92, skewed at 1.80 times single-thread and 2.42 times the reference at mt8, and the multi-frame payloads at 2.84 to 3.23 times single-thread at mt16.

The ratio sweep holds **+9.51%** over 120 cells, up from R111's +8.70% on the best-row wins alone: exactly 12 best-tier cells moved and every other cell reproduces R111 byte for byte with the reference side byte-flat through the binding move. The losing set is unchanged from R111 at the same bytes.

The full 1-22 ladder holds its shape with the best rows transformed: json 13-15 at x0.20 to 0.40 against R111's 1.31, 1.28 and 0.77, skewed 13-15 at x0.02 to 0.05 against 1.38, 1.51 and 1.12, text chain rows at 0.31 to 0.67, random levels 17-19 at 2222 to 3432 MiB/s at byte-exact ratio, and zeros levels 1-4 at 0.18 to 0.23. Small payloads held their band at deterministic sizes, with json l3 4K at 1.16 and text l3 4K at 1.12.

## Decode single-thread

Throughput is MiB/s of raw data for bulk decode and streaming decode with 64 KiB pulls. x is our time divided by zstd's time, so below 1 means we are faster.

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

Every corpus shape reads at or under parity bulk except skewed.zst19 at 1.02, and the stream column holds 0.89 to 1.04 with random's 0.81 the widest win. Dll100's bulk band reads 0.95 to 1.02 against R111's 0.93 to 1.01, and the stream band 0.98 to 1.02. The pull-size sweep behind the README figure agrees, with dll100 streaming x cells of 0.96 to 1.03 across 4 KiB to 1 MiB pulls. The remaining FSE-build residue and the copy-loop lumps are in [dec-gap](dec-gap.md).

## Encode single-thread bulk

Throughput and ratio are per shape, with checksums off on both sides. The tier column names the ladder tier and the pairing is 1, 3, 9, 13, 17 and 19 against libzstd. x is our time divided by zstd's, so below 1 means we are faster. These cells share the machine in pairs, so x is the comparable column. The dll row carries the post-fix reading.

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

The balanced row reads healthy in-pass with no lottery carry: json.balanced at x0.77 against R111's 0.78, text.balanced at 0.90 against 0.89, and the fast rows at 1.00 and 0.59. The best row carries the verdict family: json.best denser and faster at 7.24 against 6.10, skewed.best at 2.00 against 1.84 and 464 against 8 MiB/s, dll100.best back at x1.14 through the Dense stock ring at 19,565,854 B. The opt and ultra rows hold the v0.7.1 family's gains at byte-identical output: json.opt 0.80 and json.ultra 0.92, text.opt 0.45 and text.ultra 0.50, dll100.opt 0.85 and dll100.ultra 0.94. We stay denser or at parity on json and text at every level, skewed wins everywhere, random and zeros tie or win, and dll is denser at every tier, dll100 balanced at 19,570,844 B against zstd's 22,701,602, 13.8 percent smaller, best 12.9, opt 16.1 and ultra 16.6. Checksums on versus off on our side read json.fast 1.00 and text.fast 0.85.

## Encode multi-thread bulk

Throughput is MiB/s at 8 workers with one pass, using a cold pool per call on both sides. The x8 column is our time divided by zstd's at the same worker count, and the ratio columns are at mt8. No mt16 bulk section ran this pass.

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

Json.fastest.mt8 at x1.13 stays the one zstd MT win, inside the cell's known noise band of 0.95 to 1.65 across passes. Every other cell is ahead, with text.balanced reading 0.96 against R111's 0.88 inside its 0.80 to 1.06 spread and skewed.balanced at 0.14 against 0.20. The warm-pool reference reads zstd json.fast mt16 at 1556 MiB/s against R111's 1625.

## Streaming encode

Throughput uses 64 KiB pulls and interleaved medians, with single-thread and MT8 columns from the post-fix enc-stream section. x is our time divided by zstd's at the same mode, so below 1 means we are faster.

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

The pre-fix corpus run read the same cells at their healthy levels, so no dll-paired rerun was needed and no pairing split shows this session. Json.best reads 0.39 single-threaded and 0.15 at mt8, the near verdict holding through the streaming path; text.best's mt8 cell reads 0.98 on an n=5 draw whose band runs 0.56 to 1.56, against R111's 0.35. The balanced row reads healthy in both runs, text at 1483 and 1494, closing R111's solo-median carry. Unknown-size text.fastest streaming has zstd emitting 1.84 MB at ratio 18.3 against our 108 KB at ratio 309. The real payloads' cells and the dll100 stream cells are in the [matrix](matrix.md) streaming section, with dll100 fast and balanced at 0.91 and 0.94 single-threaded and best back at 1.08 through the Dense class.

## Decode multi-thread scaling

Fixtures are the same files as the single-thread table, and throughput is MiB/s at 1, 2, 4, 8 and 16 workers. The last column is zstd's single-thread streaming decode, because libzstd has no MT decode, so the scaling columns have no zstd counterpart.

| file | ours ST | mt2 | mt4 | mt8 | mt16 | zstd ST stream ref |
|---|---:|---:|---:|---:|---:|---:|
| json.zst3 | 2009 | 2648 | 3039 | 3190 | 3211 | 1747 |
| text.zst3 | 11636 | 11375 | 11194 | 11429 | 11326 | 11068 |
| skewed.zst9 | 965 | 1371 | 1598 | 1737 | 1719 | 716 |
| random.zst3 | 9723 | 9519 | 9625 | 9673 | 9099 | 8780 |
| dll100.zst3 | 1730 | 2436 | 2886 | 3014 | 3131 | 1628 |

Dll100 is measured through `files --threads`, with its reference taken from this pass's dll100.zst3 streaming cell. Json mt16 reads 1.84 times the reference and 1.60 times its own single-thread, dll100 1.92 and 1.81, skewed 1.80 times single-thread and 2.42 times the reference at mt8, text flat at 0.97, random at 0.94 to 0.99. The multi-frame payloads scale at 2.84 to 3.23 times single-thread at mt16 in the [matrix](matrix.md) files-multi table, and the real payloads reach 1.45 times the reference on jsonreal and 2.17 on textreal at mt16. The walls are sequence-read and aggregate-bandwidth bound, tracked as todograph item 1 (`todograph show 1`).

## Compression-ratio sweep

The sweep runs `zstdx-bench ratio` on 5 shapes, 6 tiers, bulk and stream modes and single-thread and 4-worker MT, one deterministic pass per cell with checksums off. The reported delta is ours over zstd's ratio minus 1, aggregated as a geo-mean, and every cell is roundtrip-gated. The geo-mean over 120 cells is **+9.51%**, up from R111's +8.70% on the best-row wins alone: exactly 12 best-tier cells moved from the store gate, json.best -13.5 to -13.6 percent bytes across its modes, skewed.best -7.7 and text.best +81 bytes per cell, and every other cell reproduces R111 byte for byte with the zstd reference side byte-flat through the 1.6.0 binding move. By mode it reads bulk single-thread +2.24%, bulk MT +11.92%, stream single-thread +12.30% and stream MT +11.95%. By shape it reads json +7.05%, text +40.27%, skewed +2.78%, random plus or minus zero and zeros +2.06%. The losing set is unchanged from R111 at identical bytes: text.opt at -0.29 to -0.31 percent, json.opt bulk-st at -0.13% and stream-st at -0.11%, json.fast MT at -0.12% to -0.14%, skewed.opt at -0.06% and skewed.ultra at -0.03 to -0.05%. The sweep ran its stock parallel-16 schedule clean at 87.9 s. Details are in the [matrix](matrix.md) ratio sections.

## Real-corpus record

The release pass records the two real payloads as side sections for the first time, and the README figures now draw their cells from them. The 48-cell ratio sweep reads a geo-mean of **+1.79%**, jsonreal **+3.26%** and textreal **+0.34%**, with the best row the strongest tier: +12.94 to +13.74 percent on jsonreal, +2.46 to +3.51 on textreal, and jsonreal.best.stream-mt restored to 2,754,919 bytes at +13.51% by the verdict fix. The balanced row's MT cells carry the open residue: jsonreal reads +11.01 and +11.08 percent single-threaded but +5.00 bulk-mt and +0.89 stream-mt, textreal +0.61 and +0.63 against -0.27 and -2.30, the worst recorded cell, the un-ported stream-side storage substitution tracked as todograph item 42.

Decode reads jsonreal at x1.01 to 1.08 on every cell and pull, textreal ahead at 0.88 to 0.99, and dll100 at 0.95 to 1.03; the MT rows scale jsonreal.zst3 to 1.45 and textreal.zst3 to 2.17 times the zstd stream reference at mt16. Bulk single-thread encode reads jsonreal fastest x1.37, balanced 1.10 at +11.01 percent size, best 1.18 at +12.94, and textreal fastest 1.88, balanced 1.37, best 1.84. Streaming reads the real payloads at fast x0.97 and balanced 1.04 on jsonreal, textreal fastest and balanced at 1.77 and 1.17, and stream-mt8 wins the jsonreal balanced and deep rows with balanced at 0.47 and best at 0.76 while textreal's fastest and balanced trail at 2.48 and 1.14. The stream ladder covers all five payloads and feeds the README encode figure from its post-fix run. The bulk-MT real axes stay unrecorded: the matrix tool routes raw payloads through the enc-st, enc-stream and enc-feed modes only.

## Open and closed deficits

Each row names a measured deficit and its status in this pass. The x figures are wall-time ratios, so values below 1 mean we are faster.

| Deficit | Status in this pass |
|---|---|
| Small-payload level 9 | Closed on 2026-09-28. The flip was the small-src btlazy swap, now an 8 KiB head verdict with a 48-distinct bar. The cells read json level 9 at x1.11, 0.96, 0.67 and 1.03 at 1K, 4K, 64K and 1M on the stock row, text at 1.79, 1.57, 2.82 and 1.01 on the swap row. The level 1 slide at 1K-4K remains the accepted trade |
| Json chain rows 5-8 and 10-12 | Closed on 2026-10-01 with the head-gated attempts diet. The rows carry the ladder at x1.03 to 1.29 and the byte wins against zstd, with the mechanism and `RowAttempts::Light` in [matchers](../perf/matchers.md) |
| text.balanced speed | Closed on 2026-10-30 by the balanced row substitution. Bulk MT reads x0.96 at mt8 against R111's 0.88 inside its 0.80 to 1.06 spread, stream-mt8 reads 0.78, and the single-thread bulk cell reads a healthy in-pass 0.90. Ratio holds +1.65 to +1.71% denser than zstd-9 |
| Decode single-thread on compressible shapes | Closed on 2026-11-02 by the libzstd-shaped sequence loop: x0.83 to 1.02 bulk and 0.89 to 1.04 stream on json, text and skewed this pass. The dll100 band reads 0.95 to 1.02 at the reference-move caveat, with the attribution in [dec-gap](dec-gap.md) |
| Best-tier core | Closed on 2026-11-25 by the store gate with the per-frame tree-domain verdict and the verdict fix. Json.best reads x0.40 in-pass and 74-77 MiB/s at x0.472 through the fix's gates against R111's x1.41, skewed.best x0.018 in-pass and 544-569 MiB/s at x0.021 against 1.53, at 13.5 and 7.7 percent denser output past the balanced-tier cells. Dll100.best keeps a measured residue at x1.14 through the Dense stock ring, with the mechanism in [matchers](../perf/matchers.md) |
| json opt and ultra | The v0.7.1 opt-tier family's gains hold at unchanged bytes: opt x0.80 bulk and 0.75 stream, ultra 0.92 and 0.80, dll100.opt 0.85 at 16.1 percent fewer bytes, with the deep-row attribution in [enc-dll-gap](enc-dll-gap.md) |
| json.fastest | x1.33 single-thread and 1.13 mt8, both inside the cells' known spreads. The steady scan loop's branch-mispredict budget sets a realistic ceiling of about x1.3 to 1.4, and the issue-and-serialization decomposition closed the asm family. Json.fast at 1.00 is near closed |
| MT decode scaling | Skewed runs 1.80 times single-thread at mt8 and 2.42 times the reference, the multi-frame payloads 2.84 to 3.23 times single-thread at mt16, json mt16 1.84 times the reference, dll100 1.92, text flat at 0.97, and the real payloads 1.45 on jsonreal and 2.17 on textreal at mt16. The walls are sequence-read and aggregate-bandwidth bound, tracked as todograph item 1 |
| dll100 balanced | Bulk reads x1.00 in-pass at -13.8% size, with best back at 1.14 through the Dense stock ring and the deep rows ahead at 0.85 and 0.94. The chain chase remains, with its differential attribution in [enc-dll-gap](enc-dll-gap.md). Fastest and fast are speed near-parity at 1.05 and 0.98 carrying -49.8% and -31.8% size |
| text single-thread stream fastest and fast | Closed on 2026-11-21 by the pooled window capacity shrink at level-down (a5050ae4). The cells read 6536 and 6476 MiB/s at x0.26 and 0.77 this pass, with the pre-fix run agreeing at 6722 and 6441 and no pairing split in the session. The mechanism and reproducer of the one-sided family stay recorded in the [workflow pitfalls](../pitfalls/workflow.md) |
| Balanced-row layout lottery | Dormant this pass: the balanced row read healthy in both enc-stream runs, text at 1483 and 1494, and the enc-st bulk cells at 0.77 and 0.90, with no solo-median carry anywhere. The family's record and trigger stay in the [workflow pitfalls](../pitfalls/workflow.md): a preceding row in the same process scatters the pooled single-thread core's layout |
| Fast-row screen tax on compressible-flat data | Random fastest and fast read x0.93 and 0.91 from the shortened bar leg plus the 1 MiB twin-scan reject, a deliberate arming-recall bound. Zeros fastest and fast read 0.21 and 0.18, and skewed chain rows read at or below 0.06. Every cell still wins |
| Ratio residues | Json.opt at -0.11 to -0.13% and text.opt stream-mt at -0.31% from the fill-descent cut inside its +0.5 percent bar, json.fast MT at -0.12% to -0.14%, skewed.opt at -0.06% and skewed.ultra at -0.03 to -0.05% as near-ties, and the dictionary small-payload parse on the encoding side. All at R111's exact bytes. On the real axes the opt and ultra MT cells read -0.60 to -1.81 percent where their single-thread cells read at or near parity |
| dll100 streaming-MT encode | Stream-mt8 trails libzstd-mt8 at three of six tiers: fastest x1.90, balanced 1.19 and best 1.41; leads at fast 0.50, opt 0.47 and ultra 0.63. The cells run at 84 to 98 percent of their own bulk-mt8 ceilings, and zstd's 1761 MiB/s fastest headline buys 48 MB of output against our 24 MB, so the two run at different output sizes |
| Real-corpus stream-side storage substitution | The balanced row's stream-mt cells read +0.89% on jsonreal and -2.30% on textreal against libzstd, where their bulk-mt cells read +5.00% and -0.27%, the un-ported stream side of the plain-lattice substitution, with the reach-band history forms in [mt-stream](../perf/mt-stream.md) and the record tracked as todograph item 42 |
| Placement-phase regressions | Closed class since 2026-10-25: the nbSeq emit fix shifted the dubt count loop onto a 64-byte line boundary for a 10% dll100.best loss at identical instruction counts. The R32 two-copy entry stub in `count_run` now pins the loop head at a phase of 31 or less, following `extend_match_aligned`. The full walk is in [matchers](../perf/matchers.md) |
