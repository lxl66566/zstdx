# Performance Snapshot (2026-11-16)

This page summarizes the 2026-11-16 release pass at commit `00b51bd0`, run on one machine, one corpus and the same flags on both sides under the shared lock, driven by `bash bench/pass.sh all` through one release build at a summed section wall of 22.9 minutes. The comparison target is the zstd crate and libzstd 1.5.7 with zstdmt, and the ladder pairs fastest, fast, balanced, best, opt and ultra against libzstd levels 1, 3, 9, 13, 17 and 19. Raw tables, provenance and per-chunk commands are in [matrix.md](matrix.md), including the full numeric 1-22 ladder and the small-payload table. Throughout this page x is our time divided by the reference time, so values below 1 mean we are faster, and speeds are MiB/s of raw input.

The pass covers the thirty-five commits since the 2026-11-07 R104 pass and the v0.5.0 release cut: the R107 decode table-build round, the R108 staged-to-win feed, and the full-repo code-review hardening sweep that closes the window. The hardening sweep fixed twenty-three verified defects. The memory-safety one first: a multi-frame MT decode lane wrote past the caller's buffer through the raw and rle block arms, and a single write-limit precheck at the shared block-executor entry now covers the frame lanes, the piece executor and the serial stage B. The correctness set: the FrameCompressor's pre-split carry now survives owned-window compaction through a frame-owned carry buffer, where the old splice built a structurally valid frame with a self-consistent checksum over the wrong payload; small-window frames honor Block_Maximum_Size = min(window, 128 KiB); the read core ends its frame at EOF instead of at the length hint; stream finish patches the split block's last wire fragment; raw-content dictionaries no longer satisfy Repeat modes; and a reused decoder no longer pre-reserves the declared window, so a nine-byte frame with a 2^27 window descriptor stopped peaking at about 134 MB on its second decode. The FFI round brings error-code and out-param parity for five introspection and decompress contracts, and the CLI round closes eleven parser, list and console divergences against the zstd 1.5.7 reference. The perf-relevant deltas: the X2 Huffman tail now finishes through the pair table, whose prior gungraun read the decode dll band at minus 0.13 to 0.67 percent Ir; the carry ownership adds one per-block memcpy, which gungraun priced at zero Ir movement on the measured encode and stream targets; and the pledged-mt l9 grid floors at the bulk strip, so the dump cells for skewed and text l9 pledged-mt became byte-identical to bulk, 3,461 and 12 bytes closer. The balanced row's head verdicts also arm on open single-threaded streams this window, which moves the three balanced stream cells onto their bulk frames' bytes.

Caveats for reading every table: the pass runs the R86 harness with the adaptive sample floor this window landed, whose enc-st and enc-stream single-thread cells share the machine in pairs, so absolute MiB/s on those cells reads below the serial tables of pre-R86 passes and cross-pass comparisons use x; a verdict on a 10 GB/s-class cell needs a `--parallel 1` rerun per the [methodology](methodology.md). Noise runs ±10% between sessions, and this session's zstd decode side read 4 to 8 percent below its R104 absolutes on the json, skewed and dll100 cells while text, random and zeros held, so the decode x cells that lean on those shapes carry that drift; every cell carries interleaved medians at the default per-side budget of 500 ms, and spreads sit in the raw tables.

## Headline results

Decode single-thread reads x0.89 to 1.02 bulk and x0.93 to 1.04 stream on json, text and skewed, with json.zst3 at 0.89 bulk, text.zst3 at 0.95, skewed.zst9 at 0.79 and skewed.zst1 at 0.98. Random and zeros are ours at every tier, with random.zst3 stream at 0.80. Dll100 reads x0.95 to 1.02 on both paths, its band inside parity for the first time, the R107 table-build diets and the X2 tail holding under the session drift caveat above, with the attribution in [dec-gap](dec-gap.md).

Encode multi-thread keeps one zstd win, json.fastest.mt8 at x1.13 inside its known noise band of 0.95 to 1.65, while its mt16 cell wins at 0.32. Every other cell is ahead, with text.balanced at 0.93 and 0.90 on both counts, and our cold-pool mt16 json.fast at 3347 MiB/s still beats zstd's warm-pool reference at 1615.

Streaming encode single-thread reads json.fast 0.95, json.opt 0.80 and json.ultra 0.99, text.best 0.62, and text.fastest and fast 0.32 and 0.90 as in-pass readings, because the same-process page-dispersity collapse reproduced for the twelfth consecutive pass; the standalone medians read 6878 and 6440 MiB/s against R104's 6149 and 5938. The balanced row's open-stream arm moved json.balanced from x1.34 to 0.64, text.balanced from 1.22 to 1.05 and dll100.balanced from 1.35 to 0.93, all three onto their bulk-parity frames.

Streaming encode MT8 keeps every json cell at or below x0.68. Every text cell wins, with balanced at 0.77. Dll100 stream-mt8 leads at fast 0.46, opt 0.48 and ultra 0.65, and trails at fastest 1.87, balanced 1.13 and best 1.62, running at 89 to 95 percent of its own bulk-mt8 ceilings, with the balanced row's 179 MiB/s back inside the 160 to 200 placement band R104 priced.

Decode MT held its scaling: json mt16 at 1.52 times this pass's single-thread and 1.79 times the zstd stream reference, dll100.zst3 mt16 at 1.78 and 1.86, skewed at 1.67 times single-thread and 2.36 times the reference at mt8, and the multi-frame payloads at 2.85 to 3.11 times single-thread at mt16.

The ratio sweep holds the +8.70% geo-mean over 120 cells, with 119 cells byte-identical to R104 and the one mover the balanced arm's own: json.balanced stream single-thread dropped 46,619 B onto the bulk cell's exact frame. The losing set is unchanged from R104 at the same bytes.

The full 1-22 ladder holds its shape: json at levels 17 and 19 reads 0.84 and 1.00 at the cut's unchanged bytes, with level 22 at 0.54, json rows 5-8 and 10-12 at 1.11 to 1.32 with their byte wins, text chain rows at 0.32 to 0.72 with level 9 at 0.89, zeros levels 1-4 at 0.18 to 0.29, random levels 17-19 at 2120 to 3331 MiB/s at byte-exact ratio, and the skewed hole at levels 13-16 at 1.12 to 1.61. Small payloads held their band at identical bytes, with json l3 4K at 1.18 and text l3 4K at 1.14.

## Decode single-thread

Throughput is MiB/s of raw data for bulk decode and streaming decode with 64 KiB pulls. x is our time divided by zstd's time, so below 1 means we are faster.

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

Every corpus shape reads at or under parity bulk except skewed.zst19 at 1.02, and the stream column holds 0.93 to 1.04 with random's 0.80 the widest win. Dll100's 0.95 to 1.02 band replaces R104's 1.02 to 1.07: our side held its absolutes while the R107 diets and the X2 tail cut its instruction count, though this session's zstd side also read 4 to 5 percent below its R104 absolutes on the same cells, so the band's center sits at parity rather than clearly ahead. The pull-size sweep behind the README figure agrees, with dll100 streaming speedups of 0.98 to 1.03 across 4 KiB to 1 MiB pulls. The remaining FSE-build residue and the copy-loop lumps are in [dec-gap](dec-gap.md).

## Encode single-thread bulk

Throughput and ratio are per shape, with checksums off on both sides. The tier column names the ladder tier and the pairing is 1, 3, 9, 13, 17 and 19 against libzstd. x is our time divided by zstd's, so below 1 means we are faster. These cells share the machine in pairs, so x is the comparable column.

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

The carry ownership's per-block memcpy leaves no wall trace: json.opt reads 0.85 against R104's 0.87, dll100.opt 0.88 against 0.89, and the ultra rows hold 1.08 and 1.03 against 1.09 and 1.05, all at byte-identical output. Text.best read 0.60 in-pass through pair-sharing against its 0.69 serial band. We stay denser or at parity on json and text at every level, skewed wins everywhere except the best-tier wander, random and zeros tie or win, and dll is denser at every tier at 13.3 percent or more from balanced up, dll100 balanced at 19,570,844 B against zstd's 22,701,602. Checksums on versus off on our side read json.fast 1.04 and text.fast 0.85.

## Encode multi-thread bulk

Throughput is MiB/s at 8 and 16 workers with one pass, using a cold pool per call on both sides. The x8 and x16 columns are our time divided by zstd's at the same worker count, and the ratio columns are at 16 workers.

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

Json.fastest.mt8 at x1.13 stays the one zstd MT win, inside the cell's known noise band of 0.95 to 1.65 across passes, and its mt16 cell wins at 0.32. Every other cell is ahead, with text.balanced at 0.93 and 0.90 and skewed.balanced at 0.18 on both counts. Our cold-pool mt16 json.fast at 3347 MiB/s still beats zstd's warm-pool reference at 1615 this run.

## Streaming encode

Throughput uses 64 KiB pulls and interleaved medians, with single-thread and MT8 columns. x is our time divided by zstd's at the same mode, so below 1 means we are faster. The single-thread cells share the machine in pairs, so x is the comparable column.

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

The two text single-thread fast cells carry in-pass readings at 5394 and 5352 MiB/s, and the same-process page-dispersity collapse reproduced for the twelfth consecutive pass, with the mechanism and reproducer in the [workflow pitfalls](../pitfalls/workflow.md); a verdict on either cell needs the standalone rerun protocol, and the standalone medians read 6878 and 6440 MiB/s against R104's 6149 and 5938. The balanced row's open-stream arm landed this window: json.balanced reads 156 MiB/s at x0.64 on the bulk-parity frame of 4,661,008 B against the old exclusion's 4,707,627, text.balanced 1526 at 1.05 at byte-identical output, and dll100.balanced 140 at 0.93, all three cells' walls landing at their bulk cells' own band. Unknown-size text.fastest streaming has zstd emitting 1.84 MB at ratio 18.3 against our 108 KB at ratio 309. Dll100 stream cells are in the [matrix](matrix.md) streaming section.

## Decode multi-thread scaling

Fixtures are the same files as the single-thread table, and throughput is MiB/s at 1, 2, 4, 8 and 16 workers. The last column is zstd's single-thread streaming decode, because libzstd has no MT decode, so the scaling columns have no zstd counterpart.

| file | ours ST | mt2 | mt4 | mt8 | mt16 | zstd ST stream ref |
|---|---:|---:|---:|---:|---:|---:|
| json.zst3 | 2016 | 2548 | 2911 | 2999 | 3060 | 1708 |
| text.zst3 | 11915 | 11716 | 11704 | 11698 | 11730 | 11002 |
| skewed.zst9 | 980 | 1296 | 1537 | 1641 | 1451 | 695 |
| random.zst3 | 9683 | 9511 | 9596 | 9590 | 9048 | 8780 |
| dll100.zst3 | 1705 | 2408 | 2776 | 2932 | 3011 | 1618 |
| multi.zst3 | 3403 | 4399 | 6087 | 9205 | 9698 | 3027 |

Dll100 is measured through `files --threads`, with its reference taken from this pass's dll100.zst3 streaming cell, and the multi row is the files-multi section against the dec-ST stream reference. Skewed runs 1.67 times single-thread and 2.36 times the reference at mt8, with mt16 settling at 1.48 inside its recorded band. The multi-frame payloads scale at 2.85 to 3.11 times single-thread at mt16 in the [matrix](matrix.md) files-multi table. Json mt16 reads 1.52 times single-thread and 1.79 times the reference, dll100 1.78 and 1.86, and text is flat at 0.98.

## Compression-ratio sweep

The sweep runs `zstdx-bench ratio` on 5 shapes, 6 tiers, bulk and stream modes and single-thread and 4-worker MT, one deterministic pass per cell with checksums off. The reported delta is ours over zstd's ratio minus 1, aggregated as a geo-mean, and every cell is roundtrip-gated. The geo-mean over 120 cells is **+8.70%**, with 119 cells reproducing the R104 sizes byte for byte and the one mover the balanced arm's json.balanced stream single-thread at 4,661,008 B, 46,619 B under its R104 open frame and byte-identical to the bulk cell. By mode it reads bulk single-thread +1.47%, bulk MT +11.08%, stream single-thread +11.46% and stream MT +11.11%. By shape it reads json +4.48%, text +40.29%, skewed +1.42%, random plus or minus zero and zeros +2.06%. The losing set is unchanged from R104 at identical bytes: text.opt stream-mt at -0.31%, json.opt at -0.11 to -0.13 percent, json.fast bulk MT at -0.14% and stream MT at -0.12%, skewed.opt at -0.06% and skewed.ultra at -0.03 to -0.05%. Details are in the [matrix](matrix.md) ratio section.

## Open and closed deficits

Each row names a measured deficit and its status in this pass. The x figures are wall-time ratios, so values below 1 mean we are faster.

| Deficit | Status in this pass |
|---|---|
| Small-payload level 9 | Closed on 2026-09-28. The flip was the small-src btlazy swap, now an 8 KiB head verdict with a 48-distinct bar. The cells read json level 9 at x1.10, 0.92, 0.65 and 1.01 at 1K, 4K, 64K and 1M on the stock row, text at 1.85, 1.59, 2.83 and 1.00 on the swap row. The level 1 slide at 1K-4K remains the accepted trade |
| Json chain rows 5-8 and 10-12 | Closed on 2026-10-01 with the head-gated attempts diet. The rows carry the ladder at x1.11 to 1.32 and the byte wins against zstd, with the mechanism and `RowAttempts::Light` in [matchers](../perf/matchers.md) |
| text.balanced speed | Closed on 2026-10-30 by the balanced row substitution. Bulk reads x0.88 single-thread on the pair-shared session band, 0.93 at mt8 and 0.90 at mt16, stream single-thread reads 1.05 and stream-mt8 0.77. Ratio holds +1.65 to +1.71% denser than zstd-9 |
| Decode single-thread on compressible shapes | Closed on 2026-11-02 by the libzstd-shaped sequence loop: x0.79 to 1.02 bulk and 0.80 to 1.04 stream on json, text and skewed this pass. The dll100 band reads 0.95 to 1.02, inside parity for the first time through the R107 table-build diets and the X2 pair-table tail at this session's drift caveat, with the attribution in [dec-gap](dec-gap.md) |
| json.fastest | x1.31 single-thread and 1.13 mt8, both inside the cells' known spreads. The steady scan loop's branch-mispredict budget sets a realistic ceiling of about x1.3 to 1.4, and the issue-and-serialization decomposition closed the asm family. Json.fast at 1.05 is near closed |
| Best-tier core | Json 1.36 in-pass against R104's 1.46 and 1.26 serial, skewed 1.49, dll 1.16, n=3 wander at identical bytes, a memory-latency tree walk with its floor in the [todo](../todo.md). It caps the json.best stream single-thread cell at x1.23, while json.best.stream-mt8 reads 0.68 |
| json opt and ultra | Opt holds x0.85 in-pass at the cut's unchanged -0.13 percent bytes, ultra 1.08, and dll100.opt reads 0.88 at 16.1 percent fewer bytes, with the deep-row attribution in [enc-dll-gap](enc-dll-gap.md) |
| MT decode scaling | Skewed runs 1.67 times single-thread at mt8 and 2.36 times the reference, the multi-frame payloads 2.85 to 3.11 times single-thread at mt16, json mt16 1.52 and 1.79, dll100 1.78 and 1.86, text flat at 0.98. The walls are sequence-read and aggregate-bandwidth bound, tracked as todo 1 in the [todo](../todo.md) |
| dll100 balanced | Bulk reads x1.02 at -13.8% size, with best at 1.16 and the deep rows ahead at 0.88 and 1.03. The chain chase remains, with its floor in the [todo](../todo.md) and its differential attribution in [enc-dll-gap](enc-dll-gap.md). Fastest and fast are speed near-parity at 1.07 and 1.00 carrying -49.8% and -31.2% size |
| text single-thread stream fastest and fast | The in-pass page-dispersity collapse reproduced for the twelfth consecutive pass at 5394 and 5352 MiB/s in-pass, at x0.32 and 0.90. It was root-caused on 2026-09-29 in the [workflow pitfalls](../pitfalls/workflow.md), where a best or L13 row preceding the pooled single-thread core scatters the big buffers' pages and THP recovers about 11%, leaving the residual unattributed. Both cells still win on the standalone medians, 6878 and 6440 MiB/s at x0.26 and 0.81 |
| Fast-row screen tax on compressible-flat data | Random fastest and fast read x0.90 and 0.87 from the shortened bar leg plus the 1 MiB twin-scan reject, a deliberate arming-recall bound. Zeros fastest and fast read 0.27 and 0.18, and skewed chain rows read at or below 0.06. Every cell still wins |
| Ratio residues | Json.opt at -0.11 to -0.13% and text.opt stream-mt at -0.31% from the fill-descent cut inside its +0.5 percent bar, json.fast MT at -0.12% to -0.14%, skewed.opt at -0.06% and skewed.ultra at -0.03 to -0.05% as near-ties, and the dictionary small-payload parse on the encoding side. All at R104's exact bytes |
| dll100 streaming-MT encode | Stream-mt8 trails libzstd-mt8 at three of six tiers: fastest x1.87, balanced 1.13 and best 1.62; leads at fast 0.46, opt 0.48 and ultra 0.65. The balanced row read 179 MiB/s this pass against R104's 128, back inside the 160 to 200 single-slowest-job placement band the same-day A/B priced at identical bytes and 1.74 percent fewer instructions, so both passes sample the same band. The cells run at 93, 95, 89, 94, 91 and 93 percent of their own bulk-mt8 ceilings of 1059, 975, 201, 54, 56 and 29 MiB/s, and zstd's 1862 MiB/s fastest headline buys 48 MB of output against our 24 MB, so the two run at different output sizes |
| Placement-phase regressions | Closed class since 2026-10-25: the nbSeq emit fix shifted the dubt count loop onto a 64-byte line boundary for a 10% dll100.best loss at identical instruction counts. The R32 two-copy entry stub in `count_run` now pins the loop head at a phase of 31 or less, following `extend_match_aligned`. The full walk is in [matchers](../perf/matchers.md) |
