# Perf vs zstd crate · current snapshot (2026-09-28)

> Post-release-fix pass at `b213d1b8`, 2026-09-28, one machine, one corpus, same flags. Provenance, raw tables and per-chunk commands: [matrix.md](matrix.md), including the full 1-22 numeric ladder (T7) and the small-payload table (T8). The pass resolves the 09-27 release pass's three beyond-noise drops: R27 content-gates the small-src btlazy swap on an 8 KiB head verdict (json l9 small payloads back to the stock row), R28 reverts the fused bit-read decoder commit (MT decode restored), R29 puts the far-screen's flat-head walk on a sampling diet with a same-day codegen fix (zeros/random fast rows recovered, text fast rows held). Encoder outputs are byte-identical to 09-27 on the corpus (dump gate) — the only byte movement is the T8 json l9 band; the ratio geo-mean holds at +8.69%. Comparison target: zstd crate / libzstd 1.5.7 (zstdmt); the ladder pairs fastest/fast/balanced/best/opt/ultra vs libzstd 1/3/9/13/17/19. Caveats: ±10% noise between runs; per-side budget 1s (500 ms on the full ladder and small); interleaved medians, spreads in the raw tables.

## Headline

- **Decode ST bulk**: we win 17/18 cells (x 0.21-0.88); skewed.zst9 the lone near-tie (x1.01). Absolute 647-12327 MiB/s ours; dll 1152-1537 MiB/s. The R28 revert gives back the fused reads' ST slice where it was real (dll bulk zst9 0.80, json zst9 stream 1.28).
- **Decode ST streaming** (core-vs-core): zstd still wins every compressible shape — json x1.18-1.34, text x1.04-1.20, skewed x1.10-1.30, dll x1.29-1.36; we win random (x0.80) and hold zeros (1.00).
- **Encode ST**: json fastest/fast x1.44/1.05, **balanced ahead at x0.87 carrying +21% density**, best/opt/ultra x1.41/1.10/1.12 at ratio parity or denser (json.opt 9 B ahead of zstd-17). text: fastest/fast x0.78/0.54 (we win); balanced x1.42 at +1.65% density; best x0.84; opt/ultra ahead x0.61/0.69. skewed: fastest/balanced far ahead (x0.44/0.043), fast a near-tie (x1.06), best/opt/ultra behind at ratio parity (x2.11/1.05/1.06). random/zeros: we win at every tier (up to 300× on incompressible high tiers) — the fast-row screen tax is paid down: **zeros fastest/fast x0.27/0.18** (was 0.31/0.21 before R29), random x0.92/0.85. **dll**: fastest speed parity x1.01 at **−49.6% size vs zstd-1** (r 4.35 vs 2.19), fast ahead x0.96 at −31.7%, balanced x1.45 at −13.0%, best x1.31 at −12.8%, opt x1.16 at −16.2%, ultra x1.20 at −16.5%.
- **Encode MT**: json.fastest.mt8 x1.17 the one remaining clean zstd mt win; every other cell ahead. Our cold-pool mt16 json.fast (3289) still beats zstd's warm-pool reference (1643).
- **Streaming encode ST**: json.fast ahead (x0.95); json.fastest x1.31; text.best x0.77; **text.fastest/fast x0.25/0.78 — standalone medians** (both full passes read a ~25% collapse that never reproduces in narrow invocations; mechanism open, twice-reproduced, recorded in the workflow pitfalls); the real residue vs 09-19 is the R23 screen's ~4% sampling tax; both stay wins.
- **Streaming encode MT8**: every json cell ahead (x0.56/0.18/0.31/0.66/0.40/0.50); text fastest/fast/best/opt/ultra ahead (x0.55/0.29/0.44/0.58/0.67); **text.balanced closed to x1.03** — no stream-mt cell loses by more than 3%.
- **Decode MT** (our exclusive dimension, libzstd has none): **restored by the R28 revert** — json mt16 = 1.58× our ST = 1.19× the zstd stream reference (mt2 1.13× ST), **dll100.zst3 mt16 = 1.84× ST = 1.32× ref** (the strongest scaling row, on the real-binary payload); skewed mt16 674 vs ref 795 (the ref itself moved 682→793 since 09-19); text flat 0.98×.
- **Ratio sweep**: geo-mean **+8.69% denser** over 120 cells (bulk-st +1.48%) — outputs byte-identical to the 09-27 sweep. Losing cells unchanged: json.fast mt −0.12..−0.14%, skewed.opt −0.06..−0.07%, plus −0.00..−0.04% near-ties on skewed.fast/fastest/ultra and text.ultra mt.
- **Full 1-22 ladder** (release gate): the text chain rows hold their wins (l5-8 x0.43-0.63, l10-12 x0.38-0.61); zeros l1-4 recovered (x0.27/0.27/0.18/0.18) and random l1-4 tightened; json 5-8 and 10-12 closed 2026-10-01 (R33's head-gated attempts diet: x1.42-1.65 -> **x1.19-1.29** carrying −4.8..−16.4% byte wins, every other corpus byte-identical — [matchers](perf/matchers.md)); the remaining holes are text l9 (x1.43) and skewed 13-16 speed (x1.28-2.58 at ratio parity). Wins or ties everywhere else including the whole 19-22 band (except json.l19 x1.11). Full tables in [matrix.md](matrix.md) T7.
- **Small payloads**: R27's head verdict restores the level-9 band — json l9 1K/4K/64K/1M now **x1.16/0.95/0.66/1.09** at stock-row bytes (was 1.39/1.40/2.04/1.07 through the unconditional swap; skewed-64K l9 41→2063 MiB/s), text l9 keeps the swap's ratio at x1.73/1.47/2.79/1.08. Level 1 (x1.25-1.73) and level 3 (x1.02-1.40) unchanged — the accepted trades. Full table in [matrix.md](matrix.md) T8.

## Decode ST (MiB/s of raw; x = ours_time/zstd_time, <1 = we faster)

| file | bulk ours | bulk zstd | bulk x | stream ours | stream zstd | stream x |
|---|---:|---:|---:|---:|---:|---:|
| json.zst1 | 1751 | 1272 | 0.73 | 1727 | 2169 | 1.26 |
| json.zst3 | 1446 | 1152 | 0.80 | 1377 | 1851 | 1.34 |
| json.zst9 | 1741 | 1239 | 0.71 | 1649 | 2117 | 1.28 |
| json.zst19 | 2170 | 1325 | 0.61 | 2193 | 2577 | 1.18 |
| text.zst1 | 6029 | 2244 | 0.37 | 6265 | 7541 | 1.20 |
| text.zst3 | 8765 | 2500 | 0.29 | 10421 | 11091 | 1.06 |
| text.zst9 | 9443 | 2543 | 0.27 | 11617 | 12100 | 1.04 |
| text.zst19 | 9494 | 2539 | 0.27 | 11588 | 12120 | 1.05 |
| skewed.zst1 | 2287 | 1524 | 0.67 | 2590 | 2842 | 1.10 |
| skewed.zst3 | 1246 | 1045 | 0.84 | 1266 | 1529 | 1.21 |
| skewed.zst9 | 647 | 654 | 1.01 | 607 | 786 | 1.30 |
| skewed.zst19 | 2191 | 1481 | 0.68 | 2399 | 2686 | 1.12 |
| random.zst3 | 8840 | 2306 | 0.26 | 11020 | 8820 | 0.80 |
| zeros.zst3 | 12327 | 2621 | 0.21 | 12869 | 12896 | 1.00 |
| dll100.zst1 | 1152 | 1012 | 0.88 | 1164 | 1504 | 1.29 |
| dll100.zst3 | 1236 | 1087 | 0.88 | 1254 | 1699 | 1.36 |
| dll100.zst9 | 1537 | 1235 | 0.80 | 1542 | 2068 | 1.34 |
| dll100.zst19 | 1336 | 1089 | 0.82 | 1348 | 1797 | 1.33 |

## Encode ST bulk (checksums off; x = ours_time/zstd_time; pairs 1/3/9/13/17/19)

| level | shape | ours MiB/s | ours ratio | zstd MiB/s | zstd ratio | x |
|---|---|---:|---:|---:|---:|---:|
| fastest | json | 590 | 6.39 | 852 | 6.11 | 1.44 |
| fastest | text | 13473 | 309.23 | 10439 | 308.94 | 0.78 |
| fastest | skewed | 2912 | 2.00 | 1273 | 2.00 | 0.44 |
| fastest | random | 2411 | 1.00 | 2216 | 1.00 | 0.92 |
| fastest | zeros | 49454 | 32577 | 13320 | 32171 | 0.27 |
| fastest | dll | 532 | 4.35 | 535 | 2.19 | 1.01 |
| fast | json | 451 | 5.30 | 472 | 5.29 | 1.05 |
| fast | text | 13346 | 333.03 | 7249 | 332.90 | 0.54 |
| fast | skewed | 212 | 1.92 | 224 | 1.92 | 1.06 |
| fast | random | 2418 | 1.00 | 2040 | 1.00 | 0.85 |
| fast | zeros | 49030 | 32577 | 8688 | 32171 | 0.18 |
| fast | dll | 447 | 4.97 | 426 | 3.40 | 0.96 |
| balanced | json | 138 | 7.20 | 120 | 5.95 | 0.87 |
| balanced | text | 1225 | 384.66 | 1703 | 378.41 | 1.42 |
| balanced | skewed | 1726 | 2.00 | 74 | 1.84 | 0.043 |
| balanced | random | 2284 | 1.00 | 1579 | 1.00 | 0.69 |
| balanced | zeros | 31077 | 32577 | 1751 | 32202 | 0.056 |
| balanced | dll | 104 | 5.31 | 152 | 4.62 | 1.45 |
| best | json | 26 | 6.26 | 36 | 6.10 | 1.41 |
| best | text | 845 | 386.42 | 712 | 385.90 | 0.84 |
| best | skewed | 6 | 1.84 | 13 | 1.84 | 2.11 |
| best | random | 2347 | 1.00 | 253 | 1.00 | 0.11 |
| best | zeros | 41481 | 32577 | 839 | 32171 | 0.020 |
| best | dll | 28 | 5.35 | 37 | 4.67 | 1.31 |
| opt | json | 6 | 7.49 | 7 | 7.49 | 1.10 |
| opt | text | 761 | 410.23 | 460 | 410.11 | 0.61 |
| opt | skewed | 3 | 2.00 | 3 | 2.00 | 1.05 |
| opt | random | 2364 | 1.00 | 12 | 1.00 | 0.005 |
| opt | zeros | 40346 | 32577 | 938 | 32202 | 0.023 |
| opt | dll | 11 | 6.04 | 13 | 5.06 | 1.16 |
| ultra | json | 3 | 7.42 | 3 | 7.42 | 1.12 |
| ultra | text | 395 | 414.12 | 271 | 413.98 | 0.69 |
| ultra | skewed | 2 | 2.00 | 2 | 2.00 | 1.06 |
| ultra | random | 2392 | 1.00 | 8 | 1.00 | 0.003 |
| ultra | zeros | 39561 | 32577 | 673 | 32202 | 0.017 |
| ultra | dll | 7 | 6.32 | 9 | 5.28 | 1.20 |

Ratio verdict: denser or at parity on json and text at every level (json.opt 9 B ahead); skewed wins everywhere except near-ties; random/zeros tie or win; dll denser at every tier — −49.6% bytes at fastest, −31.7% at fast, double digits from balanced up. Checksum on/off (ours): json.fast 1.00, text.fast 0.85.

## Encode MT bulk (1 pass; cold pool per call both sides; MiB/s)

| cell | ours mt8 | zstd mt8 | x8 | ours mt16 | zstd mt16 | x16 | ratio ours mt16 | ratio zstd mt16 |
|---|---:|---:|---:|---:|---:|---:|---:|---:|
| json.fastest | 3633 | 4224 | 1.17 | 5203 | 1808 | 0.35 | 6.34 | 6.11 |
| json.fast | 2533 | 1048 | 0.41 | 3289 | 1038 | 0.32 | 5.30 | 5.31 |
| json.balanced | 356 | 203 | 0.57 | 418 | 205 | 0.49 | 7.19 | 5.95 |
| text.fastest | 20682 | 10782 | 0.53 | 18089 | 2268 | 0.13 | 309.08 | 39.78 |
| text.fast | 12086 | 2240 | 0.19 | 12112 | 2200 | 0.18 | 332.94 | 189.16 |
| text.balanced | 1032 | 1258 | 1.21 | 1054 | 1253 | 1.18 | 384.59 | 378.30 |
| skewed.fastest | 4212 | 3245 | 0.77 | 3651 | 1528 | 0.42 | 2.00 | 2.00 |
| skewed.fast | 1284 | 639 | 0.50 | 1649 | 639 | 0.39 | 1.92 | 1.92 |
| skewed.balanced | 830 | 122 | 0.15 | 826 | 121 | 0.15 | 2.00 | 1.84 |

## Streaming encode (64KiB pulls; json/text; interleaved medians)

| cell | ST ours | ST zstd | ST x | MT8 ours | MT8 zstd | MT8 x |
|---|---:|---:|---:|---:|---:|---:|
| json.fastest | 595 | 776 | 1.31 | 3406 | 1922 | 0.56 |
| json.fast | 467 | 442 | 0.95 | 2272 | 403 | 0.18 |
| json.balanced | — | — | — | 352 | 112 | 0.31 |
| json.best | 25 | 35 | 1.36 | 80 | 52 | 0.66 |
| json.opt | — | — | — | 16 | 7 | 0.40 |
| json.ultra | — | — | — | 6 | 3 | 0.50 |
| text.fastest | 6990 | 1735 | 0.25 | 7560 | 4144 | 0.55 |
| text.fast | 6702 | 5250 | 0.78 | 5512 | 1614 | 0.29 |
| text.balanced | — | — | — | 949 | 975 | 1.03 |
| text.best | 853 | 655 | 0.77 | 841 | 363 | 0.44 |
| text.opt | — | — | — | 666 | 380 | 0.58 |
| text.ultra | — | — | — | 363 | 244 | 0.67 |

Unknown-size text.fastest streaming: zstd emits 1.84MB (ratio 18.3) vs our 108KB (ratio 309). Stream/ceiling ratios in [matrix.md](matrix.md) T5.

## Decode MT scaling (1 pass; our solo dimension; MiB/s)

| file | ours ST | mt2 | mt4 | mt8 | mt16 | zstd ST stream ref |
|---|---:|---:|---:|---:|---:|---:|
| json.zst3 | 1404 | 1589 | 1992 | 2192 | 2224 | 1873 |
| text.zst3 | 11034 | 10919 | 10898 | 10888 | 10847 | 11161 |
| skewed.zst9 | 648 | 739 | 733 | 652 | 674 | 795 |
| random.zst3 | 9732 | 9498 | 9530 | 9543 | 9529 | 8817 |
| dll100.zst3 | 1217 | 1809 | 2060 | 2220 | 2240 | 1699 |

dll100 measured via `files --threads` (see [matrix.md](matrix.md) T2) — the restart-point decode scales best on the large real-binary payload. json and dll are back at their 09-19 scaling (the 09-27 drop was the R28-reverted fused read body); skewed mt16 trails its ref (the ref itself drifted since 09-19).

## Compression-ratio sweep (`zstdx-bench ratio`, 2026-09-28)

Geo-mean Δ over 120 cells **+8.69%**; per mode bulk-st +1.48% / bulk-mt +11.08% / stream-st +11.43% / stream-mt +11.11%; per shape json +4.41%, text +40.36%, skewed +1.41%, random ±0, zeros +2.06%. Outputs byte-identical to the 09-27 sweep (dump-gated). Losing cells (complete): json.fast mt −0.12..−0.14%, text.ultra mt −0.02%, skewed.opt −0.06..−0.07%, skewed.ultra −0.04%, skewed.fast mt −0.01..−0.03%, skewed.fastest −0.00%. Details in [matrix.md](matrix.md) T6.

## Top open deficits (from this run, x = ours/zstd wall time)

1. **Small-payload level 9** (todo 4): CLOSED 2026-09-28 (R27) — the flip was R8's small-src btlazy swap (bisect-pinned; the R14/R20 suspects were wrong: the R20 screen never runs below 32 MiB and the R14 entropy tails are ±1 B here). The swap is now an 8 KiB head verdict (bar 48 distinct): json l9 x1.16/0.95/0.66/1.09 at 1K/4K/64K/1M (stock row), text keeps the swap at x1.73/1.47/2.79/1.08 carrying −0.9% vs zstd-9 at 64K. Level 1's 1K-4K slide (the R14 tiny dense bars) stays as the accepted trade.
2. **json chain rows 5-8 and 10-12** (full-ladder T7): **CLOSED 2026-10-01 (R33)** — a head-gated attempts diet for the row band (`RowAttempts::Light`: narrow-alphabet frames run 2/3/5/8 and 12/32/32 attempts at rows 5-8/10-12, the R27 verdict discipline): json x1.44/1.53/1.64/1.47 -> **1.29/1.26/1.27/1.23** and x1.61/1.63/1.58 -> **1.25/1.19/1.25** at −4.8..−16.4% bytes vs zstd (ratios 5.86-7.25 vs zstd's 5.57-6.08); text/skewed/random/zeros/dll byte-identical, text's own wins untouched (mechanism and gates in [matchers](perf/matchers.md)).
3. **text.balanced speed x1.42 ST** — the residual cold-start DUBT head per-frame cost; ratio +1.65% denser than zstd-9 (floor in [todo](../todo.md)).
4. **Streaming decode on compressible shapes**: json x1.18-1.34, skewed x1.10-1.30, text x1.04-1.20 — the fused-loop serial chain remains (closed as a measured op-volume floor, [dec-gap](dec-gap.md)).
5. **json.fastest x1.44 ST / x1.17 mt8** — the steady scan loop's inherent branch-mispredict budget; realistic ceiling ~x1.3-1.4 (todo 3). json.fast x1.05 is near closed.
6. **Best-tier core**: json x1.41, skewed x2.11 (memory-latency tree walk; floor in [todo](../todo.md)). Caps the json.best stream ST cell (x1.36).
7. **json opt/ultra x1.10-1.12, skewed opt/ultra x1.05-1.06** — per-node codegen + event-volume residue at ratio parity or denser (floor in [todo](../todo.md)).
8. **MT decode regression** (todo 1) — **resolved R28 (2026-09-28)**: bisect confirmed the fused bit-read decoder commit (`b889d6d2`: json mt2 −33% at flat ST; the fused body's six-widths-six-bases live set fits only the flat executor, the staging pass spilled it — three rework shapes all falsified, [negative](../negative/decoding.md)). Reverted: json mt16 1.58× ST = 1.19× ref (mt2 1.13× ST), dll 1.84×/1.32×; the ST stream json/dll cells gave back their +1.7-2.8% (json x1.34, dll x1.36 worst cells). skewed mt16 (674) still trails its ref (795) — the ref's own drift, not ours.
9. **dll100 balanced x1.45 at −13.0% size** (floor in [todo](../todo.md), differential attribution in [enc-dll-gap](enc-dll-gap.md)); fastest/fast are now speed parity-or-ahead carrying −49.6/−31.7% size — the remaining gap is the balanced chain chase.
10. **text ST stream fastest/fast**: both full passes (09-27, 09-28) read a ~25% collapse (5240/5118, 5163/5084) that never reproduces standalone — six clean narrow runs across two days and three builds read 6571-6990/6309-6702 (x0.25/0.78), with the pass's own ceilings depressed in the same window. **Root-caused 2026-09-29** (workflow pitfalls): a same-process state — any best/L13 row preceding the pooled ST core scatters the physical pages the next row's big buffers land on (pagemap contiguity 0.29 vs 0.51; the 128 KiB staged→win copies eat 38.8% of cycles); full passes hit it because T5's json.best runs before text within one process. THP recovers ~11%, residual unattributed (todo 15: hugepage-backed matcher buffers). The real residue vs 09-19 (7243/6943) is the R23 unpledged-stream screen's sampling tax (~4%); still wins.
11. **Fast-row screen tax on compressible-flat data**: the flat-head half is recovered (2026-09-28, R29 + same-day codegen fix — `strided_distinct`'s SIMD uniformity probe and bounded cap hunt: zeros fastest/fast x0.31/0.21 → x0.27/0.18, 12 of the 16 lost points back; [matchers](perf/matchers.md)); random fastest/fast x0.92/0.85 (the bar leg shortened, the 1 MiB twin-scan reject is the residue — a deliberate arming-recall bound); skewed chain rows −12..−42% (x≤0.06). The far-class screens cost even when they find nothing, but every cell still wins.
12. **Ratio residues**: json.fast mt −0.12..−0.14%, skewed.opt −0.07% (near-tie), dictionary small-payload parse-side (todo 6).
