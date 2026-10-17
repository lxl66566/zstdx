# Perf vs zstd crate · current snapshot (2026-10-18)

> Pre-release pass at `add0b0be`, 2026-10-18, one machine, one corpus, same flags. Provenance, raw tables and per-chunk commands: [matrix.md](matrix.md), including the full 1-22 numeric ladder (T7) and the small-payload table (T8). Encoder outputs are **byte-identical to the 10-01 pass** (all 120 ratio cells and all 110 ladder cells size-equal, geo-mean +8.69% unchanged); speed moves concentrate where the 17 interim commits point — R40/R41's reach-probe parse takes text.balanced bulk ST x1.34 → **1.10** (ladder l9 x1.34 → 1.11) and json.balanced x0.84 → **0.77**; the tree tiers read at or inside their bands (json.best 1.28 → 1.23, dll.best 1.42 → **1.26**, json.ultra 1.03 → 1.00, skewed.opt 0.95 → 0.92); decode stream cells drift a few % better (R37/R38 direction); dec-mt json mt16 1.60 → **1.65× ST**, dll100 1.83 → **1.90×**; text.balanced stream-mt8 closes to **x0.95 — every stream-mt cell now wins**. Comparison target: zstd crate / libzstd 1.5.7 (zstdmt); the ladder pairs fastest/fast/balanced/best/opt/ultra vs libzstd 1/3/9/13/17/19. Caveats: ±10% noise between runs; per-side budget 1s (500 ms on the full ladder and small); interleaved medians, spreads in the raw tables.

## Headline

- **Decode ST bulk**: we win 17/18 cells (x 0.21-1.00); skewed.zst9 sits exactly at parity (x1.00). Absolute 640-12236 MiB/s ours; dll 1157-1536 MiB/s. Every cell reproduces 10-01 or reads a few % better.
- **Decode ST streaming** (core-vs-core): zstd still wins every compressible shape — json x1.13-1.33, text x1.04-1.20, skewed x1.08-1.28, dll x1.28-1.34; we win random (x0.80) and hold zeros (1.00). The band tightened 1-6% vs 10-01 (R37/R38).
- **Encode ST**: json fastest/fast x1.45/1.03, **balanced ahead at x0.77 carrying +21% density**, best x1.23, opt/ultra x1.01/**1.00** at ratio parity or denser (json.opt 9 B ahead of zstd-17) — four of six json tiers now at parity-or-ahead. text: fastest/fast x0.79/0.54 (we win); **balanced x1.10 at +1.65% density** (was 1.34 — R40/R41); best/opt/ultra ahead x0.61/0.55/0.64. skewed: fastest/balanced far ahead (x0.44/0.041), fast a near-tie (x1.07), best behind (x1.91), **opt/ultra ahead (x0.92/0.95)** at ratio parity. random/zeros: we win at every tier (up to 300× on incompressible high tiers); zeros fastest/fast x0.27/0.18, random x0.90/0.84. **dll**: fastest x1.06 at **−49.6% size vs zstd-1** (r 4.35 vs 2.19), fast ahead x0.97 at −31.7%, balanced x1.30 at −13.0%, best x1.26 at −12.8%, opt x1.09 at −16.2%, ultra x1.12 at −16.5%.
- **Encode MT**: json.fastest.mt8 x1.19 (spread 0.95-1.68) the one remaining zstd mt win; every other cell ahead or inside text.balanced's own 0.99-1.47 spread. Our cold-pool mt16 json.fast (3307) still beats zstd's warm-pool reference (1593).
- **Streaming encode ST**: json.fast ahead (x0.94); json.fastest x1.32; text.best x0.63; **text.fastest/fast x0.25/0.79 — standalone medians** (the in-pass page-dispersity collapse reproduced for the fourth consecutive pass; mechanism in the workflow pitfalls); both stay wins. json.opt x0.97 and json.ultra x0.98 stay wins.
- **Streaming encode MT8**: every json/text cell ahead — **text.balanced x0.95 (was 0.98, now a win)**, every other cell ≤x0.64. dll100 stream-mt8 reproduces the 10-02 addendum: ahead at fast/opt (x0.80/0.95), trailing fastest x3.52, balanced x1.15, ultra x1.21 and best x5.39 (the `count_from_long` SIMD fix held) — [todo](../todo.md) 17.
- **Decode MT** (our exclusive dimension, libzstd has none): json mt16 = 1.65× our ST = 1.23× the zstd stream reference (mt2 1.38× ST), **dll100.zst3 mt16 = 1.90× ST = 1.36× ref** (the strongest scaling row, on the real-binary payload), skewed mt16 1.25× ST / 0.96× ref (the shape's absolute band wanders ±15% across passes), text flat 0.97×.
- **Ratio sweep**: geo-mean **+8.69% denser** over 120 cells (bulk-st +1.48%) — outputs byte-identical to the 10-01 sweep. Losing cells unchanged: json.fast mt −0.12..−0.14%, skewed.opt −0.06..−0.07%, plus −0.00..−0.04% near-ties on skewed.fast/fastest/ultra and text.ultra mt.
- **Full 1-22 ladder** (release gate): json rows 5-8 and 10-12 hold R33's diet at **x1.23-1.31 carrying −4.8..−16.4% byte wins**; the R40/R41 parse lifts the chain band — text l9 x1.34 → **1.11**, json l9 x0.83 → 0.76; skewed 17-22 hold x0.77-0.96 and json 19-22 all ≤1.01; text chain rows hold their wins (l5-8 x0.42-0.62, l10-12 x0.36-0.57); zeros l1-4 x0.27/0.27/0.18/0.18 and random l1-4 x0.87-0.92 held. Remaining holes: text l9 (x1.11) and skewed 13-16 speed (x1.17-2.27 at ratio parity). Full tables in [matrix.md](matrix.md) T7.
- **Small payloads**: json l9 1K/4K/64K/1M **x1.11/0.90/0.64/0.99** at stock-row bytes, text l9 keeps the swap's ratio at x1.73/1.49/2.46/1.00 — reproduces 10-01 cell for cell (level 3's 64K cells improved to 0.83/0.90). Level 1 (x1.24-1.78) and level 3 (x0.83-1.34) — the accepted trades. Full table in [matrix.md](matrix.md) T8.

## Decode ST (MiB/s of raw; x = ours_time/zstd_time, <1 = we faster)

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

## Encode ST bulk (checksums off; x = ours_time/zstd_time; pairs 1/3/9/13/17/19)

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

Ratio verdict (unchanged from 10-01, outputs byte-identical): denser or at parity on json and text at every level (json.opt 9 B ahead); skewed wins everywhere except near-ties; random/zeros tie or win; dll denser at every tier — −49.6% bytes at fastest, −31.7% at fast, double digits from balanced up. Checksum on/off (ours): json.fast 1.00, text.fast 0.86.

## Encode MT bulk (1 pass; cold pool per call both sides; MiB/s)

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

## Streaming encode (64KiB pulls; json/text; interleaved medians)

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

*Standalone medians (the in-pass collapse read 5601/5502 — fourth consecutive pass; [workflow pitfalls](../pitfalls/workflow.md)). Unknown-size text.fastest streaming: zstd emits 1.84MB (ratio 18.3) vs our 108KB (ratio 309). dll100 stream cells in [matrix.md](matrix.md) T5.

## Decode MT scaling (1 pass; our solo dimension; MiB/s)

| file | ours ST | mt2 | mt4 | mt8 | mt16 | zstd ST stream ref |
|---|---:|---:|---:|---:|---:|---:|
| json.zst3 | 1399 | 1927 | 2117 | 2276 | 2308 | 1875 |
| text.zst3 | 11132 | 10832 | 10852 | 10843 | 10818 | 11179 |
| skewed.zst9 | 608 | 783 | 756 | 782 | 759 | 788 |
| random.zst3 | 9678 | 9458 | 9479 | 9476 | 9471 | 8928 |
| dll100.zst3 | 1225 | 1872 | 2102 | 2241 | 2322 | 1709 |

dll100 measured via `files --threads` (see [matrix.md](matrix.md) T2) — the restart-point decode scales best on the large real-binary payload (mt16 1.90× ST = 1.36× ref, up from 1.83×/1.31×). json mt16 1.65× ST / 1.23× ref (was 1.60×/1.18×); skewed's mt rows hold 1.25× ST at 0.96× ref; text flat 0.97×.

## Compression-ratio sweep (`zstdx-bench ratio`, 2026-10-18)

Geo-mean Δ over 120 cells **+8.69%**; per mode bulk-st +1.48% / bulk-mt +11.08% / stream-st +11.43% / stream-mt +11.11%; per shape json +4.41%, text +40.36%, skewed +1.41%, random ±0, zeros +2.06%. Outputs byte-identical to the 10-01 sweep. Losing cells (complete): json.fast mt −0.12..−0.14%, text.ultra mt −0.02%, skewed.opt −0.06..−0.07%, skewed.ultra −0.04%, skewed.fast mt −0.01..−0.03%, skewed.fastest −0.00%. Details in [matrix.md](matrix.md) T6.

## Top open deficits (from this run, x = ours/zstd wall time)

1. **Small-payload level 9** (todo 4): CLOSED 2026-09-28 (R27) — the flip was R8's small-src btlazy swap, now an 8 KiB head verdict (bar 48 distinct): json l9 x1.11/0.90/0.64/0.99 at 1K/4K/64K/1M (stock row), text keeps the swap at x1.73/1.49/2.46/1.00 carrying −0.9% vs zstd-9 at 64K. Level 1's 1K-4K slide (the R14 tiny dense bars) stays as the accepted trade.
2. **json chain rows 5-8 and 10-12** (full-ladder T7): CLOSED 2026-10-01 (R33) — head-gated attempts diet (`RowAttempts::Light`): x1.23-1.31 at −4.8..−16.4% bytes vs zstd; every other corpus byte-identical (mechanism and gates in [matchers](perf/matchers.md)). This pass holds those cells.
3. **text.balanced speed**: ST x1.34 → **1.10** after R40/R41's concurrent probe parse (2026-10-17, `measure_shrink_concurrent` + the stock probe's parallel parse; bulk-mt8 x1.08, stream-mt8 x0.95 — a win now); the residual cold-start DUBT head per-frame cost stands; ratio +1.65% denser than zstd-9 (floor in [todo](../todo.md)).
4. **Streaming decode on compressible shapes**: json x1.13-1.33, skewed x1.08-1.28, text x1.04-1.20, dll x1.28-1.34 — the fused-loop serial chain remains (closed as a measured op-volume floor, [dec-gap](dec-gap.md)).
5. **json.fastest x1.45 ST / x1.19 mt8** (the cells' known spreads) — the steady scan loop's inherent branch-mispredict budget; realistic ceiling ~x1.3-1.4 (todo 3). json.fast x1.03 is near closed.
6. **Best-tier core**: json x1.23, skewed x1.91 (memory-latency tree walk; floor in [todo](../todo.md)). Caps the json.best stream ST cell (x1.16).
7. **json opt/ultra**: CLOSED 2026-10-01 (R30's hugepage-backed buffers) — this pass reads x1.01/**1.00** (ratio parity, json.opt 9 B ahead) and skewed opt/ultra ahead (x0.92/0.95).
8. **MT decode scaling** (todo 1): json mt16 1.65× ST = 1.23× ref, dll100 1.90×/1.36×, skewed 1.25× ST / 0.96× ref (its band wanders); text flat 0.97× — the memory-bound verdict/execute walls stand (no lever short of overlapping execute with the verdict, a falsified class).
9. **dll100 balanced x1.30 at −13.0% size** (best now x1.26 — was 1.42; the chain chase remains; floor in [todo](../todo.md), differential attribution in [enc-dll-gap](enc-dll-gap.md)); fastest/fast are speed near-parity carrying −49.6/−31.7% size.
10. **text ST stream fastest/fast**: the in-pass page-dispersity collapse reproduced for the fourth consecutive pass (5601/5502 in-pass vs 6960/6695 standalone, x0.25/0.79) — root-caused 2026-09-29 (workflow pitfalls: a best/L13 row preceding the pooled ST core scatters the big buffers' pages; THP recovers ~11%, residual unattributed, todo 15). The real residue vs 09-19 is the R23 unpledged-stream screen's ~4% sampling tax; still wins.
11. **Fast-row screen tax on compressible-flat data**: random fastest/fast x0.90/0.84 (the shortened bar leg plus the 1 MiB twin-scan reject — a deliberate arming-recall bound); zeros fastest/fast x0.27/0.18 and skewed chain rows x≤0.06 all held. Every cell still wins.
12. **Ratio residues**: json.fast mt −0.12..−0.14%, skewed.opt −0.07% (near-tie), dictionary small-payload parse-side (todo 6).
13. **dll100 streaming-MT encode** (todo 17): stream-mt8 vs libzstd-mt8 trails at four of six tiers (fastest x3.52, balanced x1.15, ultra x1.21, best x5.39) and runs at 52-93% of its own bulk-mt8 ceilings — the libzstd-MT losses are the open item ([todo](../todo.md) 17); the stream-vs-ceiling gap itself matches text's recorded fast-row class.
