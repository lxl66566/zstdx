# Perf vs zstd crate · current snapshot (2026-10-01)

> Pre-release pass at `1249735b`, 2026-10-01, one machine, one corpus, same flags. Provenance, raw tables and per-chunk commands: [matrix.md](matrix.md), including the full 1-22 numeric ladder (T7) and the small-payload table (T8). The pass re-baselines everything after R30-R36: encoder outputs on the 32 MiB corpus are **byte-identical to 09-28** (ratio gate — all 120 cells size-equal, geo-mean +8.69% unchanged; the ladder's json l5-8/10-12 rows carry R33's already-recorded diet), while R30's hugepage-backed matcher buffers move the big-table tiers beyond noise — json best/opt/ultra x1.41/1.10/1.12 → **1.28/1.01/1.03**, text best 0.84 → **0.67** and balanced 1.42 → **1.34**, skewed best/opt/ultra 2.11/1.05/1.06 → **1.88/0.95/0.95** (opt/ultra now wins), dll balanced/opt/ultra 1.45/1.16/1.20 → **1.31/1.09/1.13** (R36's AVX2 entropy kernels are dispatch-gated off on this AVX-512 machine — byte-identical, inert here). Decode reproduces 09-28 within noise everywhere. Comparison target: zstd crate / libzstd 1.5.7 (zstdmt); the ladder pairs fastest/fast/balanced/best/opt/ultra vs libzstd 1/3/9/13/17/19. Caveats: ±10% noise between runs; per-side budget 1s (500 ms on the full ladder and small); interleaved medians, spreads in the raw tables.

## Headline

- **Decode ST bulk**: we win 17/18 cells (x 0.21-0.88); skewed.zst9 the lone near-tie (x1.03). Absolute 640-12236 MiB/s ours; dll 1144-1511 MiB/s. All cells reproduce 09-28 (no decode-path commits since R28).
- **Decode ST streaming** (core-vs-core): zstd still wins every compressible shape — json x1.19-1.35, text x1.05-1.21, skewed x1.10-1.32, dll x1.30-1.36; we win random (x0.80) and hold zeros (1.00).
- **Encode ST**: json fastest/fast x1.47/1.04, **balanced ahead at x0.84 carrying +21% density**, best/opt/ultra x1.28/1.01/1.03 at ratio parity or denser (json.opt 9 B ahead of zstd-17) — R30's hugepage backing took the opt/ultra residue to parity. text: fastest/fast x0.77/0.54 (we win); balanced x1.34 at +1.65% density; best/opt/ultra ahead x0.67/0.57/0.67. skewed: fastest/balanced far ahead (x0.44/0.043), fast a near-tie (x1.07), best behind (x1.88), **opt/ultra now ahead (x0.95/0.95)** at ratio parity. random/zeros: we win at every tier (up to 300× on incompressible high tiers); zeros fastest/fast x0.27/0.18, random x0.90/0.85. **dll**: fastest speed parity x1.03 at **−49.6% size vs zstd-1** (r 4.35 vs 2.19), fast ahead x0.97 at −31.7%, balanced x1.31 at −13.0%, best x1.42 at −12.8%, opt x1.09 at −16.2%, ultra x1.13 at −16.5%.
- **Encode MT**: json.fastest.mt8 x1.23 (the cell's 1.03-1.57 spread) the one remaining zstd mt win; every other cell ahead or inside text.balanced's own 0.99-1.47 spread. Our cold-pool mt16 json.fast (3379) still beats zstd's warm-pool reference (1597).
- **Streaming encode ST**: json.fast ahead (x0.94); json.fastest x1.34; text.best x0.65; **text.fastest/fast x0.25/0.78 — standalone medians** (the in-pass page-dispersity collapse reproduced for the third consecutive pass; mechanism in the workflow pitfalls); both stay wins.
- **Streaming encode MT8**: every json/text cell ahead — text.balanced closed to **x0.98** (was 1.03), every other cell ≤x0.64. dll100 stream-mt8 measured for the first time (2026-10-02): ahead only at fast/opt (x0.81/0.96), trailing fastest x3.56, balanced x1.15, ultra x1.22 and best **x14.27** at 46-93% of its own bulk-mt8 ceilings — [todo](../todo.md) 17.
- **Streaming pull sweep** (same-session addendum, `--pull 4k..1m`, the README-figure runs): both stream directions are pull-size-flat — every speedup stays within 5% of its 64 KiB value from 4 KiB to 1 MiB pulls; the T5 ST table is now complete (all six tiers measured: json.ultra stream ST **x0.98 a win**, json.balanced x1.23 and text.balanced x1.14 the losses, each carrying byte wins). Data in [matrix.md](matrix.md) T5 addendum.
- **Decode MT** (our exclusive dimension, libzstd has none): json mt16 = 1.60× our ST = 1.18× the zstd stream reference (mt2 1.31× ST), **dll100.zst3 mt16 = 1.83× ST = 1.31× ref** (the strongest scaling row, on the real-binary payload), skewed mt16 1.19× ST / 0.95× ref (reads far better than 09-28's 1.04×/0.85× — this shape's absolute band wanders ±15% across passes with no decode commit in range), text flat 0.98×.
- **Ratio sweep**: geo-mean **+8.69% denser** over 120 cells (bulk-st +1.48%) — outputs byte-identical to the 09-28 sweep. Losing cells unchanged: json.fast mt −0.12..−0.14%, skewed.opt −0.06..−0.07%, plus −0.00..−0.04% near-ties on skewed.fast/fastest/ultra and text.ultra mt.
- **Full 1-22 ladder** (release gate): json rows 5-8 and 10-12 hold R33's diet at **x1.22-1.31 carrying −4.8..−16.4% byte wins**; the hugepage backing lifts the btlazy/opt band — skewed 17-22 now x0.76-1.31 (was 0.86-1.46) and json 19-22 all ≤1.01 (json.l19's old x1.11 exception gone; json.l18 x1.22 the band's residue); text chain rows hold their wins (l5-8 x0.42-0.62, l10-12 x0.37-0.57); zeros l1-4 x0.27/0.27/0.18/0.18 and random l1-4 x0.89-0.95 held. Remaining holes: text l9 (x1.34) and skewed 13-16 speed (x1.17-2.22 at ratio parity). Full tables in [matrix.md](matrix.md) T7.
- **Small payloads**: json l9 1K/4K/64K/1M **x1.13/0.93/0.66/1.00** at stock-row bytes, text l9 keeps the swap's ratio at x1.73/1.48/2.76/1.01 — reproduces 09-28 cell for cell. Level 1 (x1.23-1.66) and level 3 (x1.01-1.38) unchanged — the accepted trades. Full table in [matrix.md](matrix.md) T8.

## Decode ST (MiB/s of raw; x = ours_time/zstd_time, <1 = we faster)

| file | bulk ours | bulk zstd | bulk x | stream ours | stream zstd | stream x |
|---|---:|---:|---:|---:|---:|---:|
| json.zst1 | 1744 | 1239 | 0.72 | 1738 | 2189 | 1.26 |
| json.zst3 | 1422 | 1132 | 0.80 | 1384 | 1873 | 1.35 |
| json.zst9 | 1722 | 1223 | 0.71 | 1643 | 2146 | 1.31 |
| json.zst19 | 2177 | 1293 | 0.59 | 2204 | 2624 | 1.19 |
| text.zst1 | 5951 | 2176 | 0.37 | 6253 | 7593 | 1.21 |
| text.zst3 | 8734 | 2409 | 0.28 | 10415 | 11142 | 1.07 |
| text.zst9 | 9399 | 2456 | 0.26 | 11570 | 12133 | 1.05 |
| text.zst19 | 9446 | 2447 | 0.26 | 11572 | 12140 | 1.05 |
| skewed.zst1 | 2311 | 1496 | 0.65 | 2578 | 2843 | 1.10 |
| skewed.zst3 | 1241 | 1022 | 0.82 | 1267 | 1534 | 1.21 |
| skewed.zst9 | 640 | 657 | 1.03 | 604 | 796 | 1.32 |
| skewed.zst19 | 2197 | 1446 | 0.66 | 2428 | 2695 | 1.11 |
| random.zst3 | 8896 | 2258 | 0.26 | 11096 | 8916 | 0.80 |
| zeros.zst3 | 12236 | 2534 | 0.21 | 12856 | 12874 | 1.00 |
| dll100.zst1 | 1144 | 992 | 0.87 | 1165 | 1512 | 1.30 |
| dll100.zst3 | 1227 | 1070 | 0.87 | 1254 | 1704 | 1.36 |
| dll100.zst9 | 1511 | 1213 | 0.80 | 1548 | 2079 | 1.34 |
| dll100.zst19 | 1320 | 1107 | 0.84 | 1356 | 1801 | 1.33 |

## Encode ST bulk (checksums off; x = ours_time/zstd_time; pairs 1/3/9/13/17/19)

| level | shape | ours MiB/s | ours ratio | zstd MiB/s | zstd ratio | x |
|---|---|---:|---:|---:|---:|---:|
| fastest | json | 580 | 6.39 | 852 | 6.11 | 1.47 |
| fastest | text | 13511 | 309.23 | 10456 | 308.94 | 0.77 |
| fastest | skewed | 2848 | 2.00 | 1260 | 2.00 | 0.44 |
| fastest | random | 2344 | 1.00 | 2099 | 1.00 | 0.90 |
| fastest | zeros | 49162 | 32577 | 13260 | 32171 | 0.27 |
| fastest | dll | 520 | 4.35 | 536 | 2.19 | 1.03 |
| fast | json | 453 | 5.30 | 472 | 5.29 | 1.04 |
| fast | text | 13630 | 333.03 | 7291 | 332.90 | 0.54 |
| fast | skewed | 211 | 1.92 | 224 | 1.92 | 1.07 |
| fast | random | 2334 | 1.00 | 1989 | 1.00 | 0.85 |
| fast | zeros | 49086 | 32577 | 8698 | 32171 | 0.18 |
| fast | dll | 439 | 4.97 | 427 | 3.40 | 0.97 |
| balanced | json | 146 | 7.20 | 120 | 5.95 | 0.84 |
| balanced | text | 1292 | 384.66 | 1731 | 378.41 | 1.34 |
| balanced | skewed | 1730 | 2.00 | 74 | 1.84 | 0.043 |
| balanced | random | 2272 | 1.00 | 1538 | 1.00 | 0.68 |
| balanced | zeros | 31547 | 32577 | 1747 | 32202 | 0.055 |
| balanced | dll | 114 | 5.31 | 150 | 4.62 | 1.31 |
| best | json | 28 | 6.26 | 36 | 6.10 | 1.28 |
| best | text | 1091 | 386.42 | 727 | 385.90 | 0.67 |
| best | skewed | 7 | 1.84 | 14 | 1.84 | 1.88 |
| best | random | 2263 | 1.00 | 248 | 1.00 | 0.11 |
| best | zeros | 42597 | 32577 | 824 | 32171 | 0.019 |
| best | dll | 28 | 5.35 | 39 | 4.67 | 1.42 |
| opt | json | 7 | 7.49 | 7 | 7.49 | 1.01 |
| opt | text | 811 | 410.23 | 463 | 410.11 | 0.57 |
| opt | skewed | 3 | 2.00 | 3 | 2.00 | 0.95 |
| opt | random | 2277 | 1.00 | 12 | 1.00 | 0.005 |
| opt | zeros | 41136 | 32577 | 919 | 32202 | 0.022 |
| opt | dll | 13 | 6.04 | 14 | 5.06 | 1.09 |
| ultra | json | 3 | 7.42 | 3 | 7.42 | 1.03 |
| ultra | text | 402 | 414.12 | 271 | 413.98 | 0.67 |
| ultra | skewed | 2 | 2.00 | 2 | 2.00 | 0.95 |
| ultra | random | 2280 | 1.00 | 8 | 1.00 | 0.003 |
| ultra | zeros | 39479 | 32577 | 660 | 32202 | 0.017 |
| ultra | dll | 8 | 6.32 | 9 | 5.28 | 1.13 |

Ratio verdict: denser or at parity on json and text at every level (json.opt 9 B ahead); skewed wins everywhere except near-ties; random/zeros tie or win; dll denser at every tier — −49.6% bytes at fastest, −31.7% at fast, double digits from balanced up. Checksum on/off (ours): json.fast 1.04, text.fast 0.85.

## Encode MT bulk (1 pass; cold pool per call both sides; MiB/s)

| cell | ours mt8 | zstd mt8 | x8 | ours mt16 | zstd mt16 | x16 | ratio ours mt16 | ratio zstd mt16 |
|---|---:|---:|---:|---:|---:|---:|---:|---:|
| json.fastest | 3393 | 4044 | 1.23 | 5135 | 1774 | 0.35 | 6.34 | 6.11 |
| json.fast | 2506 | 1021 | 0.41 | 3379 | 1017 | 0.30 | 5.30 | 5.31 |
| json.balanced | 360 | 201 | 0.56 | 434 | 201 | 0.46 | 7.19 | 5.95 |
| text.fastest | 19658 | 10492 | 0.54 | 17353 | 2247 | 0.13 | 309.08 | 39.78 |
| text.fast | 11748 | 2198 | 0.19 | 11961 | 2165 | 0.18 | 332.94 | 189.16 |
| text.balanced | 1046 | 1241 | 1.18 | 1049 | 1208 | 1.14 | 384.59 | 378.30 |
| skewed.fastest | 4119 | 3246 | 0.79 | 3596 | 1544 | 0.43 | 2.00 | 2.00 |
| skewed.fast | 1187 | 635 | 0.54 | 1660 | 639 | 0.39 | 1.92 | 1.92 |
| skewed.balanced | 720 | 121 | 0.18 | 718 | 121 | 0.17 | 2.00 | 1.84 |

## Streaming encode (64KiB pulls; json/text; interleaved medians)

| cell | ST ours | ST zstd | ST x | MT8 ours | MT8 zstd | MT8 x |
|---|---:|---:|---:|---:|---:|---:|
| json.fastest | 579 | 777 | 1.34 | 3215 | 1967 | 0.61 |
| json.fast | 470 | 440 | 0.94 | 2260 | 399 | 0.18 |
| json.balanced | — | — | — | 381 | 115 | 0.30 |
| json.best | 29 | 35 | 1.21 | 84 | 52 | 0.61 |
| json.opt | — | — | — | 17 | 7 | 0.40 |
| json.ultra | — | — | — | 7 | 3 | 0.49 |
| text.fastest | 7063 | 1767 | 0.25 | 7767 | 3972 | 0.51 |
| text.fast | 6776 | 5274 | 0.78 | 5738 | 1573 | 0.28 |
| text.balanced | — | — | — | 984 | 967 | 0.98 |
| text.best | 1001 | 651 | 0.65 | 824 | 358 | 0.44 |
| text.opt | — | — | — | 692 | 379 | 0.55 |
| text.ultra | — | — | — | 373 | 237 | 0.64 |

Unknown-size text.fastest streaming: zstd emits 1.84MB (ratio 18.3) vs our 108KB (ratio 309). Stream/ceiling ratios in [matrix.md](matrix.md) T5.

## Decode MT scaling (1 pass; our solo dimension; MiB/s)

| file | ours ST | mt2 | mt4 | mt8 | mt16 | zstd ST stream ref |
|---|---:|---:|---:|---:|---:|---:|
| json.zst3 | 1379 | 1802 | 2038 | 2170 | 2212 | 1879 |
| text.zst3 | 11132 | 10933 | 10919 | 10947 | 10918 | 11204 |
| skewed.zst9 | 635 | 760 | 756 | 757 | 754 | 796 |
| random.zst3 | 9776 | 9525 | 9539 | 9553 | 9532 | 8912 |
| dll100.zst3 | 1219 | 1826 | 2041 | 2183 | 2234 | 1704 |

dll100 measured via `files --threads` (see [matrix.md](matrix.md) T2) — the restart-point decode scales best on the large real-binary payload. json mt16 back at 1.60× ST / 1.18× ref (09-19-level scaling held since the R28 revert); skewed's mt rows now scale (1.19× ST) and sit at 0.95× ref — the shape's absolutes wander ±15% across passes (its zstd ref itself moved 682→793→796 since 09-19), so the movement is recorded, not attributed.

## Compression-ratio sweep (`zstdx-bench ratio`, 2026-10-01)

Geo-mean Δ over 120 cells **+8.69%**; per mode bulk-st +1.48% / bulk-mt +11.08% / stream-st +11.43% / stream-mt +11.11%; per shape json +4.41%, text +40.36%, skewed +1.41%, random ±0, zeros +2.06%. Outputs byte-identical to the 09-28 sweep. Losing cells (complete): json.fast mt −0.12..−0.14%, text.ultra mt −0.02%, skewed.opt −0.06..−0.07%, skewed.ultra −0.04%, skewed.fast mt −0.01..−0.03%, skewed.fastest −0.00%. Details in [matrix.md](matrix.md) T6.

## Top open deficits (from this run, x = ours/zstd wall time)

1. **Small-payload level 9** (todo 4): CLOSED 2026-09-28 (R27) — the flip was R8's small-src btlazy swap, now an 8 KiB head verdict (bar 48 distinct): json l9 x1.13/0.93/0.66/1.00 at 1K/4K/64K/1M (stock row), text keeps the swap at x1.73/1.48/2.76/1.01 carrying −0.9% vs zstd-9 at 64K. Level 1's 1K-4K slide (the R14 tiny dense bars) stays as the accepted trade.
2. **json chain rows 5-8 and 10-12** (full-ladder T7): CLOSED 2026-10-01 (R33) — head-gated attempts diet (`RowAttempts::Light`): x1.19-1.31 at −4.8..−16.4% bytes vs zstd; every other corpus byte-identical (mechanism and gates in [matchers](perf/matchers.md)). This pass holds those cells (x1.22-1.31).
3. **text.balanced speed x1.34 ST** (was 1.42 — R30's backed LDM/fused tables paid down part): the residual cold-start DUBT head per-frame cost; ratio +1.65% denser than zstd-9 (floor in [todo](../todo.md)).
4. **Streaming decode on compressible shapes**: json x1.19-1.35, skewed x1.10-1.32, text x1.05-1.21 — the fused-loop serial chain remains (closed as a measured op-volume floor, [dec-gap](dec-gap.md)).
5. **json.fastest x1.47 ST / x1.23 mt8** (the mt cell's 1.03-1.57 spread) — the steady scan loop's inherent branch-mispredict budget; realistic ceiling ~x1.3-1.4 (todo 3). json.fast x1.04 is near closed.
6. **Best-tier core**: json x1.28, skewed x1.88 (memory-latency tree walk; R30's backed heads+ring paid the TLB slice, the walk remains — floor in [todo](../todo.md)). Caps the json.best stream ST cell (x1.21).
7. **json opt/ultra x1.10-1.12**: CLOSED this pass — R30's hugepage-backed `opt_table`/`bt` buffers put json opt/ultra at x1.01/1.03 (ratio parity, json.opt 9 B ahead) and skewed opt/ultra ahead (x0.95/0.95).
8. **MT decode scaling** (todo 1): json mt16 1.60× ST = 1.18× ref, dll100 1.83×/1.31×, skewed 1.19× ST / 0.95× ref (its band wanders); text flat 0.98× — the memory-bound verdict/execute walls stand (no lever short of overlapping execute with the verdict, a falsified class).
9. **dll100 balanced x1.31 at −13.0% size** (was 1.45; improved by R30's backed tables — the chain chase remains; floor in [todo](../todo.md), differential attribution in [enc-dll-gap](enc-dll-gap.md)); fastest/fast are speed parity-or-ahead carrying −49.6/−31.7% size.
10. **text ST stream fastest/fast**: the in-pass page-dispersity collapse reproduced for the third consecutive pass (5643/5566 in-pass vs 7063/6776 standalone, x0.25/0.78) — root-caused 2026-09-29 (workflow pitfalls: a best/L13 row preceding the pooled ST core scatters the big buffers' pages; THP recovers ~11%, residual unattributed, todo 15). The real residue vs 09-19 is the R23 unpledged-stream screen's ~4% sampling tax; still wins.
11. **Fast-row screen tax on compressible-flat data**: random fastest/fast x0.90/0.85 (the shortened bar leg plus the 1 MiB twin-scan reject — a deliberate arming-recall bound); zeros fastest/fast x0.27/0.18 and skewed chain rows x≤0.06 all held. Every cell still wins.
12. **Ratio residues**: json.fast mt −0.12..−0.14%, skewed.opt −0.07% (near-tie), dictionary small-payload parse-side (todo 6).
13. **dll100 streaming-MT encode** (new, 2026-10-02): stream-mt8 vs libzstd-mt8 trails at four of six tiers (fastest x3.56, balanced x1.15, ultra x1.22, best **x14.27**) and runs at 46-93% of its own bulk-mt8 ceilings — the libzstd-MT losses are the open item ([todo](../todo.md) 17); the stream-vs-ceiling gap itself matches text's recorded fast-row class (40-43%).
