# Performance Snapshot (2026-11-21)

This page summarizes the 2026-11-21 release pass at commit `cc2e46ff`, run on one machine, one corpus and the same flags on both sides under the shared lock, driven by `bash bench/pass.sh all`. The comparison target is the zstd crate and libzstd 1.5.7 with zstdmt, and the ladder pairs fastest, fast, balanced, best, opt and ultra against libzstd levels 1, 3, 9, 13, 17 and 19. Raw tables, provenance and per-chunk commands are in [matrix.md](matrix.md), including the full numeric 1-22 ladder and the small-payload table. Throughout this page x is our time divided by the reference time, so values below 1 mean we are faster, and speeds are MiB/s of raw input.

The pass covers the twenty-three commits since the 2026-11-16 R109 pass and the v0.6.0 cut. The perf-relevant deltas: decode now derives its packed sequence baselines from the running FSE state, priced at minus 0.66 percent Ir on the dll100 target, and `DecoderOptions::verify_checksum(false)` with the CLI's `-d --no-check` opts a decoder out of checksum verification, where libzstd offers only the experimental forceIgnoreChecksum. The pooled stream core shrinks its window capacity at level-down, which closes the settled fast-row stream penalty that the page-dispersity protocol worked around for twelve passes. MT encode gained the backward strip replay with the LDM fill lane lease, and the joiner dropped its insert-run prefetch. The windowed LDM snapshot adoption is pinned to the publish lattice, a determinism fix that moves the dll400 and dll200 l9 w16 cells onto fixed sizes while the corpus and dll100 bytes stay identical. The MT stream core surfaces its poison waits behind queued jobs, and every waited mutex pairs with its own condvar, which fixed the macOS CI hang. The CLI feeds MT file compression through a read-only mmap into `read::OwnedEncoder`, and the Windows parity round fixed the CRLF display expansion, the CRT strerror texts and the dict=input identity. The window closes with test infrastructure: the FFI test overread clamp, CI on macOS and Windows, a nextest adoption with suite tightening, a fresh fuzz campaign of about 5.65 million leg units clean, the stmdiff, cfg_dump, jobrestart and write-loop probes with the R117-R118 falsification records, binary-pinned fixtures, and memory bounding for the paranoid suite.

Caveats for reading every table: the pass runs the R86 harness with the adaptive sample floor, whose enc-st and enc-stream single-thread cells share the machine in pairs, so absolute MiB/s on those cells reads below the serial tables of pre-R86 passes and cross-pass comparisons use x; a verdict on a 10 GB/s-class cell needs a `--parallel 1` rerun per the [methodology](methodology.md). Noise runs ±10% between sessions. The toolchain moved to rustc 1.102.0-nightly from R109's 1.100.0-nightly. The zstd decode side has read 4 to 12 percent below its R104 absolutes on the json, skewed and dll100 cells in both this pass and R109 while our side held, so the decode x cells that lean on those shapes carry that standing drift. This session's pooled-layout lottery caught the balanced row instead of the fast row, so the balanced verdict cells below carry standalone medians, exactly as the fast row did for the twelve passes before the window's fix.

## Headline results

Decode single-thread reads x0.81 to 1.03 bulk and x0.85 to 1.02 stream on json, text and skewed, with json.zst3 at 0.88 bulk, text.zst3 at 0.95, skewed.zst9 at 0.81 and skewed.zst1 at 0.97. Random and zeros are ours at every tier, with random.zst3 stream at 0.80. Dll100 reads x0.93 to 1.02 across both paths, the bulk band tightening from R109's 0.95 to 1.02 as our side gained 1.4 to 2.3 percent under the packed-FSE-baseline cut while the zstd side held, all inside the standing drift caveat above, with the attribution in [dec-gap](dec-gap.md).

Encode multi-thread keeps one zstd win, json.fastest.mt8 at x1.14 inside its known noise band of 0.95 to 1.65, while its mt16 cell wins at 0.34. Every other cell is ahead, with text.balanced at 0.97 and 0.96 on both counts, and our cold-pool mt16 json.fast at 3343 MiB/s still beats zstd's warm-pool reference at 1608.

Streaming encode single-thread reads json.fast 0.94, json.opt 0.89 and json.ultra 1.06, text.best 0.64, text.opt 0.50 and text.ultra 0.62. The text fast row reads healthy in-pass for the first time since R86 at 6812 and 6522 MiB/s with x0.26 and 0.78, the twelve-pass page-dispersity collapse closed by the pooled window capacity shrink, and the standalone medians agree at 6900 and 6409. The balanced row caught this session's layout lottery: json.balanced carries the solo median 146 at x0.70 and text.balanced the solo 1541 at 1.04, against in-pass readings of 126 and 1132, with the same-session interleaved A/B reading 144 to 148 MiB/s on both this tree and R109's.

Streaming encode MT8 keeps every json cell at or below x0.67. Every text cell wins, with balanced at 0.83. Dll100 stream-mt8 leads at fast 0.49, opt 0.50 and ultra 0.66, and trails at fastest 2.05, balanced 1.42 with the solo median at 1.16, and best 1.58, running at 83 to 98 percent of its own bulk-mt8 ceilings.

Decode MT held its scaling: json mt16 at 1.66 times this pass's single-thread and 1.87 times the zstd stream reference, dll100.zst3 mt16 at 1.74 and 1.85, skewed at 1.69 times single-thread and 2.34 times the reference at mt8, and the multi-frame payloads at 2.75 to 3.33 times single-thread at mt16.

The ratio sweep holds the +8.70% geo-mean over 120 cells, with all 120 cells reproducing the R109 sizes byte for byte and the zstd reference side byte-flat, so the window moved no corpus byte. The losing set is unchanged from R109 at the same bytes.

The full 1-22 ladder holds its shape: json at levels 17 and 19 reads 0.90 and 1.09 at the cut's unchanged bytes, with level 22 at 0.63, json rows 5-8 and 10-12 at 1.11 to 1.34 with their byte wins, text chain rows at 0.32 to 0.77 with level 9 at 1.35 in the collapsed in-pass reading, zeros levels 1-4 at 0.18 to 0.29, random levels 17-19 at 2236 to 3608 MiB/s at byte-exact ratio, and the skewed hole at levels 13-16 at 1.18 to 1.55. The deep rows l13-l22 moved against R109 by up to a third, and the same-session interleaved A/B reads those cells dead even between the two trees, so the deep deltas are cross-session draws under the methodology's rule. Small payloads held their band at deterministic sizes, with json l3 4K at 1.15 and text l3 4K at 1.15.

## Decode single-thread

Throughput is MiB/s of raw data for bulk decode and streaming decode with 64 KiB pulls. x is our time divided by zstd's time, so below 1 means we are faster.

| file | bulk ours | bulk zstd | bulk x | stream ours | stream zstd | stream x |
|---|---:|---:|---:|---:|---:|---:|
| json.zst1 | 2278 | 2112 | 0.93 | 2130 | 2038 | 0.96 |
| json.zst3 | 2088 | 1843 | 0.88 | 1851 | 1703 | 0.92 |
| json.zst9 | 2440 | 2164 | 0.89 | 2122 | 1978 | 0.93 |
| json.zst19 | 2682 | 2501 | 0.93 | 2555 | 2493 | 0.98 |
| text.zst1 | 7504 | 6599 | 0.88 | 7343 | 7360 | 1.00 |
| text.zst3 | 9750 | 9214 | 0.95 | 11003 | 10987 | 1.00 |
| text.zst9 | 10258 | 9914 | 0.97 | 11968 | 12045 | 1.01 |
| text.zst19 | 10350 | 9813 | 0.95 | 11921 | 12096 | 1.02 |
| skewed.zst1 | 3172 | 3077 | 0.97 | 3066 | 2846 | 0.93 |
| skewed.zst3 | 1663 | 1572 | 0.95 | 1565 | 1462 | 0.94 |
| skewed.zst9 | 957 | 777 | 0.81 | 815 | 694 | 0.85 |
| skewed.zst19 | 2841 | 2938 | 1.03 | 2743 | 2676 | 0.98 |
| random.zst3 | 8996 | 8928 | 1.00 | 10288 | 8278 | 0.80 |
| zeros.zst3 | 11822 | 11215 | 0.95 | 12825 | 12875 | 1.00 |
| dll100.zst1 | 1496 | 1504 | 1.01 | 1417 | 1446 | 1.02 |
| dll100.zst3 | 1745 | 1644 | 0.94 | 1637 | 1596 | 0.98 |
| dll100.zst9 | 2124 | 2010 | 0.95 | 1993 | 1941 | 0.97 |
| dll100.zst19 | 1833 | 1713 | 0.93 | 1745 | 1688 | 0.97 |
| multi.zst3 | 3120 | 2884 | 0.92 | 3248 | 3028 | 0.93 |

Every corpus shape reads at or under parity bulk except skewed.zst19 at 1.03, and the stream column holds 0.93 to 1.02 with random's 0.80 the widest win. Dll100's bulk band reads 0.93 to 1.01 against R109's 0.95 to 1.02: our side gained 1.4 to 2.3 percent while the zstd side held, matching the packed-FSE-baseline cut at minus 0.66 percent Ir, and the stream band holds at 0.97 to 1.02. The pull-size sweep behind the README figure agrees, with dll100 streaming x cells of 0.96 to 1.04 across 4 KiB to 1 MiB pulls. The remaining FSE-build residue and the copy-loop lumps are in [dec-gap](dec-gap.md).

## Encode single-thread bulk

Throughput and ratio are per shape, with checksums off on both sides. The tier column names the ladder tier and the pairing is 1, 3, 9, 13, 17 and 19 against libzstd. x is our time divided by zstd's, so below 1 means we are faster. These cells share the machine in pairs, so x is the comparable column.

| level | shape | ours MiB/s | ours ratio | zstd MiB/s | zstd ratio | x |
|---|---:|---:|---:|---:|---:|---:|
| fastest | json | 628 | 6.41 | 841 | 6.11 | 1.34 |
| fastest | text | 12049 | 309.23 | 10053 | 308.94 | 0.84 |
| fastest | skewed | 2630 | 2.00 | 1211 | 2.00 | 0.46 |
| fastest | random | 2129 | 1.00 | 1894 | 1.00 | 0.90 |
| fastest | zeros | 48599 | 32577 | 13224 | 32171 | 0.27 |
| fastest | dll | 498 | 4.36 | 523 | 2.19 | 1.05 |
| fast | json | 451 | 5.30 | 479 | 5.29 | 1.06 |
| fast | text | 12553 | 333.03 | 7171 | 332.90 | 0.57 |
| fast | skewed | 209 | 1.92 | 225 | 1.92 | 1.07 |
| fast | random | 2168 | 1.00 | 1752 | 1.00 | 0.81 |
| fast | zeros | 48882 | 32577 | 8589 | 32171 | 0.18 |
| fast | dll | 441 | 4.98 | 423 | 3.40 | 0.97 |
| balanced | json | 140 | 7.20 | 114 | 5.95 | 0.82 |
| balanced | text | 1365 | 384.66 | 1628 | 378.41 | 1.19 |
| balanced | skewed | 1809 | 2.00 | 73 | 1.84 | 0.040 |
| balanced | random | 2245 | 1.00 | 1387 | 1.00 | 0.62 |
| balanced | zeros | 33300 | 32577 | 1747 | 32202 | 0.052 |
| balanced | dll | 132 | 5.36 | 142 | 4.62 | 1.08 |
| best | json | 22 | 6.26 | 30 | 6.10 | 1.38 |
| best | text | 920 | 386.42 | 581 | 385.90 | 0.63 |
| best | skewed | 5 | 1.84 | 8 | 1.84 | 1.52 |
| best | random | 2217 | 1.00 | 135 | 1.00 | 0.058 |
| best | zeros | 44850 | 32577 | 630 | 32171 | 0.014 |
| best | dll | 27 | 5.36 | 30 | 4.67 | 1.13 |
| opt | json | 6 | 7.48 | 6 | 7.49 | 0.91 |
| opt | text | 883 | 408.84 | 421 | 410.11 | 0.48 |
| opt | skewed | 2 | 2.00 | 2 | 2.00 | 0.89 |
| opt | random | 2266 | 1.00 | 9 | 1.00 | 0.004 |
| opt | zeros | 40824 | 32577 | 836 | 32202 | 0.020 |
| opt | dll | 12 | 6.03 | 12 | 5.06 | 0.93 |
| ultra | json | 3 | 7.42 | 3 | 7.42 | 1.09 |
| ultra | text | 414 | 414.12 | 249 | 413.98 | 0.61 |
| ultra | skewed | 2 | 2.00 | 2 | 2.00 | 1.01 |
| ultra | random | 2262 | 1.00 | 6 | 1.00 | 0.003 |
| ultra | zeros | 43683 | 32577 | 610 | 32202 | 0.014 |
| ultra | dll | 8 | 6.32 | 8 | 5.28 | 1.05 |

The balanced bulk cells caught this session's pooled-layout lottery: json.balanced reads 140 in-pass with the solo median at 153 and x0.78, and text.balanced reads 1365 in-pass against the same-session old-tree 1841 and R109's 1808 at 0.88, with no solo taken for that cell. The fast row reads at its pre-collapse band, text.fastest at 0.84 and text.fast at 0.57, the pooled window capacity shrink landing. The deep rows hold inside their wander at byte-identical output, with json.opt 0.91 against R109's 0.85, dll100.opt 0.93 against 0.88, the ultra rows at 1.09 and 1.05 against 1.08 and 1.03, and text.best 0.63 in-pass with the solo at 0.59. We stay denser or at parity on json and text at every level, skewed wins everywhere except the best-tier wander, random and zeros tie or win, and dll is denser at every tier at 12.8 percent or more from balanced up, dll100 balanced at 19,570,844 B against zstd's 22,701,602. Checksums on versus off on our side read json.fast 1.04 and text.fast 0.84.

## Encode multi-thread bulk

Throughput is MiB/s at 8 and 16 workers with one pass, using a cold pool per call on both sides. The x8 and x16 columns are our time divided by zstd's at the same worker count, and the ratio columns are at 16 workers.

| cell | ours mt8 | zstd mt8 | x8 | ours mt16 | zstd mt16 | x16 | ratio ours mt16 | ratio zstd mt16 |
|---|---:|---:|---:|---:|---:|---:|---:|---:|
| json.fastest | 3705 | 4142 | 1.14 | 5347 | 1815 | 0.34 | 6.35 | 6.11 |
| json.fast | 2474 | 1042 | 0.43 | 3343 | 1037 | 0.31 | 5.30 | 5.31 |
| json.balanced | 413 | 206 | 0.50 | 524 | 202 | 0.39 | 7.19 | 5.95 |
| text.fastest | 19131 | 10686 | 0.57 | 15811 | 2210 | 0.14 | 309.08 | 39.78 |
| text.fast | 11794 | 2194 | 0.18 | 11157 | 2205 | 0.20 | 332.94 | 189.16 |
| text.balanced | 1289 | 1264 | 0.97 | 1305 | 1265 | 0.96 | 384.59 | 378.30 |
| skewed.fastest | 4203 | 3247 | 0.77 | 3628 | 1604 | 0.44 | 2.00 | 2.00 |
| skewed.fast | 1208 | 601 | 0.50 | 1673 | 651 | 0.39 | 1.92 | 1.92 |
| skewed.balanced | 711 | 122 | 0.17 | 645 | 122 | 0.19 | 2.00 | 1.84 |

Json.fastest.mt8 at x1.14 stays the one zstd MT win, inside the cell's known noise band of 0.95 to 1.65 across passes, and its mt16 cell wins at 0.34. Every other cell is ahead, with text.balanced at 0.97 and 0.96 and skewed.balanced at 0.17 and 0.19. Our cold-pool mt16 json.fast at 3343 MiB/s still beats zstd's warm-pool reference at 1608 this run.

## Streaming encode

Throughput uses 64 KiB pulls and interleaved medians, with single-thread and MT8 columns. x is our time divided by zstd's at the same mode, so below 1 means we are faster. The single-thread cells share the machine in pairs, so x is the comparable column.

| cell | ST ours | ST zstd | ST x | MT8 ours | MT8 zstd | MT8 x |
|---|---:|---:|---:|---:|---:|---:|
| json.fastest | 612 | 765 | 1.25 | 3448 | 1642 | 0.49 |
| json.fast | 469 | 441 | 0.94 | 1983 | 396 | 0.20 |
| json.balanced | 126* | 100 | 0.76 | 360 | 115 | 0.32 |
| json.best | 19 | 25 | 1.38 | 76 | 52 | 0.67 |
| json.opt | 6 | 5 | 0.89 | 28 | 7 | 0.24 |
| json.ultra | 2 | 3 | 1.06 | 10 | 3 | 0.30 |
| text.fastest | 6812 | 1775 | 0.26 | 7643 | 3786 | 0.49 |
| text.fast | 6522 | 5113 | 0.78 | 5676 | 1727 | 0.30 |
| text.balanced | 1132* | 1586 | 1.40 | 1172 | 978 | 0.83 |
| text.best | 964 | 618 | 0.64 | 809 | 307 | 0.38 |
| text.opt | 830 | 417 | 0.50 | 727 | 371 | 0.51 |
| text.ultra | 406 | 250 | 0.62 | 253 | 234 | 0.93 |

The two text fast cells read healthy in-pass for the first time since R86, at 6812 and 6522 MiB/s with x0.26 and 0.78, and the post-drop-caches standalone medians agree at 6900 and 6409: the pooled window capacity shrink at level-down closed the twelve-pass page-dispersity family, whose mechanism and reproducer stay recorded in the [workflow pitfalls](../pitfalls/workflow.md). The same per-process layout lottery caught the balanced row this session, so the two starred cells carry in-pass readings with verdicts on the standalone medians: json.balanced at 146 MiB/s solo and x0.70 on the bulk-parity frame of 4,661,008 B, with the same-session interleaved A/B reading 144 to 148 MiB/s on both this tree and R109's, and text.balanced at 1541 solo and x1.04 at byte-identical output against the in-pass 1132. Unknown-size text.fastest streaming has zstd emitting 1.84 MB at ratio 18.3 against our 108 KB at ratio 309. Dll100 stream cells are in the [matrix](matrix.md) streaming section.

## Decode multi-thread scaling

Fixtures are the same files as the single-thread table, and throughput is MiB/s at 1, 2, 4, 8 and 16 workers. The last column is zstd's single-thread streaming decode, because libzstd has no MT decode, so the scaling columns have no zstd counterpart.

| file | ours ST | mt2 | mt4 | mt8 | mt16 | zstd ST stream ref |
|---|---:|---:|---:|---:|---:|---:|
| json.zst3 | 1915 | 2114 | 2963 | 3082 | 3184 | 1702 |
| text.zst3 | 11908 | 11408 | 11195 | 11396 | 11422 | 11057 |
| skewed.zst9 | 962 | 1310 | 1558 | 1624 | 1536 | 695 |
| random.zst3 | 9731 | 9559 | 9583 | 9641 | 9100 | 8930 |
| dll100.zst3 | 1745 | 2378 | 2830 | 2973 | 3033 | 1637 |
| multi.zst3 | 3406 | 4331 | 6062 | 9009 | 9855 | 3028 |

Dll100 is measured through `files --threads`, with its reference taken from this pass's dll100.zst3 streaming cell, and the multi row is the files-multi section against the dec-ST stream reference. Skewed runs 1.69 times single-thread and 2.34 times the reference at mt8, with mt16 settling at 1.60 inside its recorded band. The multi-frame payloads scale at 2.75 to 3.33 times single-thread at mt16 in the [matrix](matrix.md) files-multi table. Json mt16 reads 1.66 times single-thread and 1.87 times the reference, dll100 1.74 and 1.85, and text is flat at 0.96.

## Compression-ratio sweep

The sweep runs `zstdx-bench ratio` on 5 shapes, 6 tiers, bulk and stream modes and single-thread and 4-worker MT, one deterministic pass per cell with checksums off. The reported delta is ours over zstd's ratio minus 1, aggregated as a geo-mean, and every cell is roundtrip-gated. The geo-mean over 120 cells is **+8.70%**, with all 120 cells reproducing the R109 sizes byte for byte and the zstd reference side byte-flat, so the window's encode-side commits moved no corpus byte. By mode it reads bulk single-thread +1.47%, bulk MT +11.08%, stream single-thread +11.46% and stream MT +11.11%. By shape it reads json +4.48%, text +40.29%, skewed +1.42%, random plus or minus zero and zeros +2.06%. The losing set is unchanged from R109 at identical bytes: text.opt stream-mt at -0.31%, json.opt at -0.11 to -0.13 percent, json.fast bulk MT at -0.14% and stream MT at -0.12%, skewed.opt at -0.06% and skewed.ultra at -0.03 to -0.05%. The sweep ran at parallel 4 under the earlyoom constraint the provenance records, and sizes are schedule-independent. Details are in the [matrix](matrix.md) ratio section.

## Open and closed deficits

Each row names a measured deficit and its status in this pass. The x figures are wall-time ratios, so values below 1 mean we are faster.

| Deficit | Status in this pass |
|---|---|
| Small-payload level 9 | Closed on 2026-09-28. The flip was the small-src btlazy swap, now an 8 KiB head verdict with a 48-distinct bar. The cells read json level 9 at x1.10, 0.94, 0.67 and 1.02 at 1K, 4K, 64K and 1M on the stock row, text at 1.80, 1.58, 2.84 and 1.01 on the swap row. The level 1 slide at 1K-4K remains the accepted trade |
| Json chain rows 5-8 and 10-12 | Closed on 2026-10-01 with the head-gated attempts diet. The rows carry the ladder at x1.11 to 1.34 and the byte wins against zstd, with the mechanism and `RowAttempts::Light` in [matchers](../perf/matchers.md) |
| text.balanced speed | Closed on 2026-10-30 by the balanced row substitution. Bulk MT reads x0.97 at mt8 and 0.96 at mt16, stream-mt8 reads 0.83, and the single-thread cells carry this session's layout lottery with the solo stream median at 1541 MiB/s and x1.04. Ratio holds +1.65 to +1.71% denser than zstd-9 |
| Decode single-thread on compressible shapes | Closed on 2026-11-02 by the libzstd-shaped sequence loop: x0.81 to 1.03 bulk and 0.85 to 1.02 stream on json, text and skewed this pass. The dll100 band reads 0.93 to 1.02, tightened by the packed-FSE-baseline cut at this session's drift caveat, with the attribution in [dec-gap](dec-gap.md) |
| json.fastest | x1.34 single-thread and 1.14 mt8, both inside the cells' known spreads. The steady scan loop's branch-mispredict budget sets a realistic ceiling of about x1.3 to 1.4, and the issue-and-serialization decomposition closed the asm family. Json.fast at 1.06 is near closed |
| Best-tier core | Json 1.38 in-pass against R109's 1.36 and 1.26 serial, skewed 1.52, dll 1.13, n=3 wander at identical bytes, a memory-latency tree walk with its floor in the [todo](../todo.md). It caps the json.best stream single-thread cell at x1.38, while json.best.stream-mt8 reads 0.67 |
| json opt and ultra | Opt holds x0.91 in-pass at the cut's unchanged -0.13 percent bytes, ultra 1.09, and dll100.opt reads 0.93 at 16.1 percent fewer bytes, with the deep-row attribution in [enc-dll-gap](enc-dll-gap.md) |
| MT decode scaling | Skewed runs 1.69 times single-thread at mt8 and 2.34 times the reference, the multi-frame payloads 2.75 to 3.33 times single-thread at mt16, json mt16 1.66 and 1.87, dll100 1.74 and 1.85, text flat at 0.96. The walls are sequence-read and aggregate-bandwidth bound, tracked as todo 1 in the [todo](../todo.md) |
| dll100 balanced | Bulk reads x1.08 in-pass at -13.8% size, with best at 1.13 and the deep rows ahead at 0.93 and 1.05. The chain chase remains, with its floor in the [todo](../todo.md) and its differential attribution in [enc-dll-gap](enc-dll-gap.md). Fastest and fast are speed near-parity at 1.05 and 0.97 carrying -49.8% and -31.8% size |
| text single-thread stream fastest and fast | Closed on 2026-11-21 by the pooled window capacity shrink at level-down (a5050ae4). The in-pass cells read 6812 and 6522 MiB/s at x0.26 and 0.78, the first healthy in-pass readings since R86 after twelve consecutive passes of the page-dispersity collapse root-caused on 2026-09-29 in the [workflow pitfalls](../pitfalls/workflow.md), and the standalone medians agree at 6900 and 6409 MiB/s with x0.26 and 0.81 |
| Balanced-row layout lottery | Open family, documented in the [workflow pitfalls](../pitfalls/workflow.md): a preceding row in the same process scatters the pooled single-thread core's layout. This session it caught the balanced row, with json.balanced stream at 126 in-pass against the solo 146, text.balanced stream at 1132 against 1541, text.balanced bulk at 1365 against the same-session old-tree 1841, and dll100.balanced stream at 131 against the solo 139. Verdict cells carry the standalone medians, as the fast row did before a5050ae4 |
| Fast-row screen tax on compressible-flat data | Random fastest and fast read x0.90 and 0.81 from the shortened bar leg plus the 1 MiB twin-scan reject, a deliberate arming-recall bound. Zeros fastest and fast read 0.27 and 0.18, and skewed chain rows read at or below 0.06. Every cell still wins |
| Ratio residues | Json.opt at -0.11 to -0.13% and text.opt stream-mt at -0.31% from the fill-descent cut inside its +0.5 percent bar, json.fast MT at -0.12% to -0.14%, skewed.opt at -0.06% and skewed.ultra at -0.03 to -0.05% as near-ties, and the dictionary small-payload parse on the encoding side. All at R109's exact bytes |
| dll100 streaming-MT encode | Stream-mt8 trails libzstd-mt8 at three of six tiers: fastest x2.05, balanced 1.42 with the solo median at 1.16, and best 1.58; leads at fast 0.49, opt 0.50 and ultra 0.66. The cells run at 83 to 98 percent of their own bulk-mt8 ceilings, and zstd's 1702 MiB/s fastest headline buys 48 MB of output against our 24 MB, so the two run at different output sizes |
| Placement-phase regressions | Closed class since 2026-10-25: the nbSeq emit fix shifted the dubt count loop onto a 64-byte line boundary for a 10% dll100.best loss at identical instruction counts. The R32 two-copy entry stub in `count_run` now pins the loop head at a phase of 31 or less, following `extend_match_aligned`. The full walk is in [matchers](../perf/matchers.md) |
