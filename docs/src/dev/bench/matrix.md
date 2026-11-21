# Bench Matrix (2026-11-21)

This page holds the raw measurement tables for the 2026-11-21 release pass. Each section states what the table measures and how to read its columns. Throughout the page `x = ours_time / zstd_time`, so values below 1 mean we are faster, throughput is MiB/s of raw input, and ratio is input bytes divided by output bytes. The comparison target is libzstd 1.5.7 with `zstdmt` enabled. Conclusions are in [snapshot.md](snapshot.md), and earlier archives from the 2026-11-16 R109, 2026-11-07 R104, 2026-11-03 R94, 2026-11-02 R89, 2026-11-01 R86, 2026-10-31, 10-30, 10-25, 10-18, 10-01, 09-28, 09-27 and 09-19 passes live in this file's git history.

## Run provenance

The pass ran at commit `cc2e46ff` on 2026-11-21, driven by `bash bench/pass.sh all`, and the benches ran that tree's build. The release tree adds only a clippy cleanup and the version bump on top of it, with no behavior change. The machine is an AMD Eng Sample 100-000000870-32_Y, a Zen4-class part with 32 visible cores, AVX-512 and BMI2, a maximum clock of 5386 MHz, and the same machine as every pass since 09-12. The toolchain is rustc 1.102.0-nightly (5f4effd57 2026-11-13), moved this window from R109's 1.100.0-nightly (8925ea358), in the release profile with codegen-units set to 1, and the harness header prints `libzstd 1.5.7, binding 10507`. The corpus is unchanged, with json.raw from 2026-09-15 and the other four shapes from 2026-09-07, and `bench/big/dll100.*` is the same 2026-09-19 set with dll100.raw at 104857600 bytes. The workspace, paranoid and no-default-features suites were green at this commit before the pass — 816 workspace tests plus 25 doctests, 620 and 132 paranoid tests across the lib and the CLI, 319 no-default-features tests and the ffi C harness against the pinned reference tree — and roundtrip verification was on for every cell.

The session ran under the shared machine lock with no other bench session in between, in three stages. The driver timed nine daily sections at a summed wall of 635.0 s, through small. The ratio section's stock parallel-16 run and a parallel-8 retry were both SIGTERM'd by earlyoom under co-tenant memory pressure, so the sweep ran at `--parallel 4` for a 172.4 s section wall. Sizes are schedule-independent, so the sweep stands, and the constraint is why that wall reads above R109's 93.6 s at parallel 16. The five ladder shapes followed in two further invocations of the same driver under the same lock. The section logs live in `bench/scratch/pass-r110-rel`, including the two README side runs below. The balanced-row verdict reruns quoted in the encode sections ran in the same locked session and sit beside them as `solo-*.log`, with the same-session cross-tree A/B evidence under `invest/`.

The harness is unchanged from R109, the R86 one plus the adaptive sample floor: the enc-st and enc-stream single-thread cells run two at a time, so the header reads `st cells parallel 2`; a warmup round pair at eight times the budget runs twice instead of three; asymmetric cells stop once the slow side spent the full budget and the fast side one sixteenth of it; and every timed encode cell gates on its own last round. Absolute MiB/s on those shared cells reads below the serial readings of every pre-R86 table, so cross-pass comparisons on those cells use x, and a verdict on a cell in the 10 GB/s class needs a `--parallel 1` rerun, as the [methodology](methodology.md) page records. The dec-st section stays serial. Two side runs feed the README figures beside the pass: `matrix --mode dec-st --pull 4k,16k,64k,256k,1m` over the corpus plus the four dll100 `.zst` files, and the text fastest/fast standalone enc-stream rerun after a dropped page cache under the page-dispersity protocol.

Twenty-three commits landed in the window since the R109 pass and the v0.6.0 cut. On decode, the packed sequence baselines now derive from the running FSE state (1876934c), priced at minus 0.66 percent Ir on the dll100 decode target, and `DecoderOptions::verify_checksum(false)` with the CLI's `-d --no-check` opts a decoder out of checksum verification (99d745c8). On encode, the pooled stream core shrinks its window capacity at level-down (a5050ae4), which closes the settled fast-row stream penalty of the previous twelve passes, and MT encode gained the backward strip replay with the LDM fill lane lease (4d50f28e) plus the joiner insert-run prefetch drop (946b1624). The windowed LDM snapshot adoption is now pinned to the publish lattice (6523ee80), a determinism fix that moves the dll400 and dll200 l9 w16 cells onto fixed sizes while the corpus and dll100 bytes stay identical.

The rest of the window is stream and platform hardening plus test infrastructure. The MT stream core surfaces its poison waits behind queued jobs (e537f572), and every waited mutex pairs with its own condvar (c85b8c01), which fixed the macOS CI hang. The CLI feeds MT file compression through a read-only mmap into `read::OwnedEncoder` (2aa77204), and the Windows parity round fixed the CRLF display expansion, the CRT strerror texts and the dict=input identity (9039a2c1, d0cd39d2, 7a95d79e). The FFI test clamps its mid-frame inBuffer to the payload length (cc2e46ff). CI now runs the workspace gates on macOS and Windows (a64c9bf5), the suite tightened around a nextest adoption (a762336d), a fresh fuzz campaign ran about 5.65 million leg units clean (cbb0af25), and the probe tooling grew stmdiff with cfg_dump (fe7ef666), jobrestart (c0fc306b), the R117-R118 falsification records (dfd9e547) and the write-loop probe that closed todo 38's input arm (4888cfcf). The fixture data is pinned binary in .gitattributes against autocrlf checkout (6927f948), the paranoid suite bounds its memory-heavy tests (9008538c), and the jump-table mutant pins per libzstd build flavor (f3cd0c80).

Determinism held everywhere the bytes must hold: the ratio sweep reproduces all 120 R109 cells byte for byte with the zstd reference side byte-flat, the full 110-cell ladder reproduces R109 on both sides, and the window's one byte-moving lever, the publish-lattice pin, sits outside the corpus and dll100 cells.

## Decode single-thread

Throughput is MiB/s of raw data for bulk and streaming decode with 64 KiB pulls. x is our time divided by zstd's time, so below 1 means we are faster, and the harness spread is at most ±0.02 on every cell.

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

Every corpus shape reads at or under parity bulk except skewed.zst19 at 1.03, with skewed.zst9 the widest win at 0.81, and the stream column holds 0.93 to 1.02 with random's 0.80 the widest win. The dll100 bulk band reads 0.93 to 1.01 against R109's 0.95 to 1.02: our side gained 1.4 to 2.3 percent while the zstd side held, consistent with the packed-FSE-baseline cut priced at minus 0.66 percent Ir, and the stream band holds at 0.97 to 1.02. The pull-sweep side run reads dll100 streaming at 0.96 to 1.04 across every pull size. The zstd side has now read 4 to 12 percent below its R104 absolutes on the json, skewed and dll100 cells in both this pass and R109 while our side held, so the x cells on those shapes carry that standing drift. The multi-frame payload decodes ahead single-threaded on both paths; its parallel story is the section below.

## Decode multi-thread scaling

Throughput is MiB/s at 1, 2, 4, 8 and 16 workers, and libzstd has no MT decode, so the reference column is its single-thread streaming cell. The dll row comes from `files --threads`, the solo tool at the same budget, with its reference taken from this pass's `dll100.zst3` streaming cell.

| file | ours ST | zstd stream ST ref | mt2 | mt4 | mt8 | mt16 |
|---|---:|---:|---:|---:|---:|---:|
| json.zst3 | 1915 | 1702 | 2114 | 2963 | 3082 | 3184 |
| text.zst3 | 11908 | 11057 | 11408 | 11195 | 11396 | 11422 |
| skewed.zst9 | 962 | 695 | 1310 | 1558 | 1624 | 1536 |
| random.zst3 | 9731 | 8930 | 9559 | 9583 | 9641 | 9100 |
| dll100.zst3 | 1745 | 1637 | 2378 | 2830 | 2973 | 3033 |

Json mt16 runs at 1.66 times single-thread and 1.87 times the reference, dll100 mt16 at 1.74 and 1.85, so the strongest single-frame rows hold. Skewed reads 1.69 times single-thread and 2.34 times the reference at mt8 against R109's 1.67 and 2.36, with mt16 settling at 1.60 against R109's 1.48, both inside the cell's recorded band. Text stays flat at 0.96 and random at 0.94 to 0.99 times single-thread. The remaining walls are sequence-read and aggregate-bandwidth bound, tracked as todo 1 in the [todo](../todo.md).

The files-multi section times the four multi-frame payloads at every worker count, the lane budget's home ground:

| payload | ST | mt2 | mt4 | mt8 | mt16 | mt16 / ST |
|---|---:|---:|---:|---:|---:|---:|
| multi.zst3 | 3406 | 4331 | 6062 | 9009 | 9855 | 2.90 |
| multi_small.zst3 | 2501 | 2818 | 4442 | 5636 | 6881 | 2.75 |
| multi_balanced.zst3 | 2512 | 3308 | 4204 | 6339 | 8363 | 3.33 |
| multi_twodom.zst3 | 3406 | 4378 | 4895 | 4821 | 5597 | 1.64 |

Multi BalANCED at mt16 runs 3.33 times its single-thread decode, multi 2.90 and multi_small 2.75, against the single-frame corpus cells' 0.94 to 1.75 band, so the lane budget's staging split holds at release scale. The two-domain payload scales at 1.64, its second frame's restart-poor half setting the pace, which is the R101 twodom pricing's predicted shape.

## Encode single-thread bulk

Throughput and ratio are per shape with checksums off on both sides, and the tier column names the ladder tier. The pairing is libzstd levels 1, 3, 9, 13, 17 and 19, and x is our time divided by zstd's, so below 1 means we are faster. The dll row uses the 100 MB ELF-concat payload `bench/big/dll100.raw` from `bench/gen_big.sh`, whose ratios are payload sizes rather than the 32 MiB corpus. These cells share the machine in pairs, so the absolute MiB/s reads below the serial tables and x is the comparable column.

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

This session's pooled-layout lottery caught the balanced row instead of the fast row. Json.balanced reads 140 in-pass against the same-session solo median of 153 at x0.78, and text.balanced reads 1365 in-pass against the same-session old-tree reading of 1841 and R109's 1808 at 0.88, with no solo rerun taken for that cell. The fast row's twelve-pass collapse is gone: text.fastest reads 12049 at x0.84 and text.fast 12553 at 0.57, both at the pre-collapse band, which is the pooled window capacity shrink (a5050ae4) landing. The deep rows read within their wander of R109 at byte-identical output, with json.opt at 0.91 against 0.85, dll100.opt 0.93 against 0.88, json.ultra 1.09 against 1.08 and dll100.ultra 1.05 against 1.03, and the best rows stay the n=3 wander class with json.best at 1.38 against R109's 1.36 and skewed.best at 1.52 against 1.49. The dll rows keep the landed geometry: balanced at 19,570,844 B against zstd's 22,701,602, 13.8 percent smaller, fastest and fast at 49.8 and 31.8 percent smaller. Checksums on versus off on our side read json.fast 1.04 and text.fast 0.84.

## Encode multi-thread bulk

Throughput is MiB/s from one pass with a cold pool per call on both sides, and the ratio columns are at 16 workers. The x8 and x16 columns are our time divided by zstd's at the same worker count, so below 1 means we are faster.

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

Json.fastest.mt8 at x1.14 stays the one zstd MT win, inside the cell's known noise band of 0.95 to 1.65 across passes, and its mt16 cell wins at 0.34. Every other cell is ahead, with text.balanced at 0.97 and 0.96 on the pair-sharing session band and skewed.balanced at 0.17 and 0.19. Our cold-pool mt16 json.fast at 3343 MiB/s still beats zstd's warm-pool reference at 1608 this run.

## Encode streaming

Throughput uses 64 KiB pulls and interleaved medians, with single-thread and MT8 columns. x is our time divided by zstd's at the same mode, so below 1 means we are faster. The single-thread cells share the machine in pairs, so x is the comparable column and the absolute MiB/s reads below the serial tables.

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

The two text fast cells read healthy in-pass for the first time since R86: text.fastest at 6812 MiB/s and x0.26, text.fast at 6522 and 0.78, against R109's collapsed 5394 and 5352, and the post-drop-caches standalone medians agree at 6900 and 6409 MiB/s with x0.26 and 0.81. The pooled window capacity shrink at level-down (a5050ae4) closed the twelve-pass page-dispersity family recorded in the [workflow pitfalls](../pitfalls/workflow.md). The same per-process pooled-core layout lottery moved to the balanced row this session, so the two starred cells carry in-pass readings and their verdicts ride the standalone medians: json.balanced reads 146 MiB/s solo at x0.70 with the same-session interleaved A/B at 144 to 148 on both the cc2e46ff and R109 trees, and text.balanced reads 1541 solo at 1.04 against the in-pass 1132. Json.best wanders at n=3 with 19 in-pass and 19 solo at x1.41, and the deep json rows read opt 0.89 and ultra 1.06 at byte-identical output.

The dll100 stream cells read single-thread fastest 419 against 504 at x1.18, fast 378 against 392 at 1.04, balanced 131 against 130 at 1.02 with the solo median at 139 and x0.95, best 25 against 32 at 1.24 with the solo at 25 against 30 at 1.17 and the same-session A/B medians at 32 against 32, opt 13 against 12 at 0.91 with the solo at 14 and 0.92, and ultra 8 against 8 at 1.10. Stream-mt8 reads fastest 916 against 1702 at x2.05, fast 827 against 429 at 0.49, balanced 151 against 193 at 1.42 with the solo median at 138 against 160 at 1.16, best 52 against 82 at 1.58, opt 49 against 24 at 0.50 and ultra 26 against 17 at 0.66. The cells run at 92, 93, 83, 98, 91 and 90 percent of the bulk-mt8 ceilings of 991, 889, 181, 53, 54 and 29 MiB/s, and best keeps its deep-band output at 19.57 MB. The remaining zstd win is the non-iso-ratio fastest row, where zstd's 1702 MiB/s headline buys 48 MB of output against our 24 MB, so the two run at different output sizes. Json.fastest.stream-mt8 improved to x0.49 from R109's 0.58.

## Compression-ratio sweep

The sweep runs `zstdx-bench ratio` on 5 shapes, 6 tiers, bulk and stream modes and single-thread and 4-worker MT, one deterministic pass per cell with checksums off. The delta is ours over zstd's ratio minus 1, aggregated as a geo-mean, and every cell is roundtrip-gated. The wall time is 172.4 s at parallel 4, the schedule the earlyoom constraint forced, as the provenance records.

The geo-mean over 120 cells is **+8.70%**. By mode it reads bulk single-thread **+1.47%**, bulk MT **+11.08%**, stream single-thread **+11.46%** and stream MT **+11.11%**. By shape it reads json +4.48%, text +40.29%, skewed +1.42%, random plus or minus zero and zeros +2.06%.

All 120 cells reproduce the R109 sizes byte for byte and the zstd reference side is byte-flat, so the window's encode-side commits moved no corpus byte. The losing set is unchanged from R109 at the same bytes: text.opt at -0.29 to -0.31 percent across its modes, json.opt at -0.11 to -0.13, json.fast bulk MT at -0.14% and stream MT at -0.12%, skewed.opt at -0.06% and skewed.ultra at -0.03 to -0.05%. Everything else is at parity or denser, random ties byte-exact at every cell, and text.balanced reads +1.65 to +1.71%.

## Full numeric ladder

The cell text reads ours/zstd throughput followed by the x ratio. Cells whose three minimum rounds already cover the 500 ms budget stop at n=3, which affects json levels 6-22 and skewed 4-22, so those absolutes carry the widest drift band and share the machine in pairs. The asymmetric-cell cap stops random levels 17-22 at n of 3 to 5. x is our time divided by zstd's, so below 1 means we are faster. Every ladder size is byte-identical to R109 on both sides: the window's one byte-moving lever, the publish-lattice pin, sits outside the ladder's corpus cells.

### Ladder speed

| level | json | text | skewed | random | zeros |
|---|---|---|---|---|---|
| 1 | 627/835 x1.33 | 11866/9855 x0.84 | 1966/859 x0.44 | 2180/2008 x0.92 | 44901/13201 x0.29 |
| 2 | 513/630 x1.23 | 11765/9766 x0.84 | 2624/863 x0.33 | 2159/2021 x0.94 | 45050/13170 x0.29 |
| 3 | 470/479 x1.02 | 12229/7087 x0.58 | 205/219 x1.07 | 2113/1962 x0.93 | 47741/8509 x0.18 |
| 4 | 418/441 x1.06 | 10904/6663 x0.62 | 151/165 x1.09 | 2115/1955 x0.92 | 48173/8516 x0.18 |
| 5 | 196/260 x1.33 | 6936/4485 x0.65 | 2200/127 x0.06 | 2274/1708 x0.75 | 45685/6302 x0.14 |
| 6 | 142/185 x1.31 | 5409/2526 x0.47 | 2051/108 x0.05 | 2328/1663 x0.72 | 43131/2777 x0.06 |
| 7 | 121/162 x1.34 | 4542/2390 x0.53 | 2008/100 x0.05 | 2271/1637 x0.72 | 45634/2753 x0.06 |
| 8 | 97/124 x1.26 | 3922/1631 x0.42 | 1836/82 x0.04 | 2272/1685 x0.74 | 44641/1768 x0.04 |
| 9 | 145/114 x0.80 | 1173/1623 x1.35 | 1710/73 x0.04 | 2263/1436 x0.64 | 31794/1741 x0.05 |
| 10 | 63/83 x1.30 | 2994/1445 x0.48 | 1561/56 x0.04 | 2236/968 x0.43 | 41903/1684 x0.04 |
| 11 | 48/56 x1.17 | 2470/1425 x0.58 | 1646/31 x0.02 | 2232/1154 x0.51 | 42720/1683 x0.04 |
| 12 | 44/48 x1.11 | 2420/780 x0.32 | 1389/21 x0.02 | 2210/532 x0.24 | 42790/914 x0.02 |
| 13 | 23/31 x1.37 | 748/581 x0.77 | 5/8 x1.43 | 2198/131 x0.06 | 43977/631 x0.01 |
| 14 | 17/22 x1.34 | 849/487 x0.58 | 5/8 x1.53 | 2190/77 x0.04 | 41799/557 x0.01 |
| 15 | 16/14 x0.84 | 796/384 x0.48 | 5/6 x1.18 | 2200/72 x0.03 | 40503/492 x0.01 |
| 16 | 8/9 x1.09 | 880/460 x0.52 | 3/5 x1.55 | 2202/11 x0.00 | 42580/1013 x0.02 |
| 17 | 6/5 x0.90 | 904/425 x0.47 | 2/2 x0.84 | 2236/9 x0.00 | 44421/824 x0.02 |
| 18 | 3/4 x1.24 | 488/351 x0.72 | 2/2 x1.24 | 3608/8 x0.00 | 33409/800 x0.02 |
| 19 | 2/3 x1.09 | 419/243 x0.58 | 1/2 x1.04 | 3465/6 x0.00 | 35718/605 x0.02 |
| 20 | 2/2 x0.84 | 276/198 x0.72 | 2/1 x0.79 | 2260/5 x0.00 | 33391/368 x0.01 |
| 21 | 2/2 x0.94 | 272/142 x0.52 | 1/1 x0.93 | 2221/7 x0.00 | 27908/210 x0.01 |
| 22 | 1/1 x0.63 | 269/124 x0.46 | 1/1 x0.98 | 2202/9 x0.00 | 36558/182 x0.00 |

The deep json rows hold their lead at the cut's unchanged bytes: levels 17 and 19 read 0.90 and 1.09 against R109's 0.84 and 1.00, level 22 at 0.63 against 0.54, level 20 at 0.84 against 0.82, and level 18 at 1.24 stays the widest deficit, with levels 13 and 14 at 1.37 and 1.34 holding theirs and level 16 improving to 1.09 from R109's 1.61. Text keeps its chain-row wins at 0.32 to 0.77 through levels 12-22, and its level 9 cell carries the balanced row's collapsed in-pass reading at 1.35 against R109's 0.89, the same lottery family as the enc-st and enc-stream balanced cells above. The skewed hole at levels 13-16 reads 1.18 to 1.55 at ratio parity or better, with skewed 17-22 at 0.79 to 1.24. Random levels 17-19 read 2236 to 3608 MiB/s at byte-exact ratio 1.00 through the raw-block fast path.

The deep rows l13-l22 moved against R109 by up to a third, with text l13 at 0.58 to 0.77, skewed l19 at 0.85 to 1.04 and json l22 at 0.54 to 0.63, while json l16 improved from 1.61 to 1.09. The same-session interleaved A/B at rustc 1.102 reads the deep cells dead even between cc2e46ff and the R109 tree, json.balanced at 144 to 148 MiB/s on both, and the solo reruns put text.opt and text.ultra at 889 and 424 MiB/s against R109's 897 and 420. The old tree at the 1.100-era nightly collapses the same cells today, two runs out of two, so the ladder deep deltas are cross-session draws under the [methodology](methodology.md)'s cross-session rule and this table keeps their in-pass readings.

### Ladder ratio

The ratio table reads ours over zstd at the same numeric level, where a higher number means we are denser. Every row reproduces the R109 bytes.

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
|---|---|---:|---:|---:|---:|
| 1 | json | 1.27 | 1.43 | 1.48 | 1.33 |
| 1 | text | 1.70 | 1.59 | 1.93 | 1.55 |
| 3 | json | 1.31 | 1.15 | 1.03 | 1.16 |
| 3 | text | 1.40 | 1.15 | 1.17 | 1.04 |
| 9 | json | 1.10 | 0.94 | 0.67 | 1.02 |
| 9 | text | 1.80 | 1.58 | 2.84 | 1.01 |

The cells sit under the dump gates at deterministic sizes and the walls hold their R109 bands, with json level 3 at 4K at 1.15 and text at 1.15 against R109's 1.18 and 1.14, and json level 9 at 4K at 0.94 against 0.92. Text level 9 keeps the swap's bytes at x1.80, 1.58, 2.84 and 1.01, and its 64K cell reads 2.84 for the fifth consecutive pass inside the 2.76 to 2.86 spread. Level 1's accepted trades are unchanged.
