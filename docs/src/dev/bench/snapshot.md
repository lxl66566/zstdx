# Perf vs zstd crate · current snapshot (2026-09-27)

> Release pass at `8c22a660`, 2026-09-27, one machine, one corpus, same flags. Provenance, raw tables and per-chunk commands: [matrix.md](matrix.md), including the full 1-22 numeric ladder (T7) and the small-payload table (T8). The 108 commits since the 09-19 pass are the R14-R26 LDM/far-class campaign (preSplit, gap-parse LDM consumers on the fast/dfast/chain rows, far-class screens including the small/mid-size band, MT/stream LDM captures, parallel LDM split scan, bulk-mt state pooling, stream-mt finish-tail publication, small-frame entropy tails) plus one decoder commit (fused bit reads in `decode_step`). Encoder outputs moved for the first time since the 09-16 corpus — zeros bulk-st cells −3 B, text.ultra −20 B, chain-ladder rows up to −17% bytes, dll transformed — so the ratio geo-mean ticked to +8.69% and several speed bands restructured: text chain rows ~2× faster, json 10-12 sharply faster, MT balanced rows +17-26%, dll fastest/fast at parity-or-better speed with double-digit-to-half size wins, while small-payload level 9 flipped from ahead to behind (T8), json/skewed MT decode dropped beyond noise, and the fast rows pay a screen tax on random/zeros. Comparison target: zstd crate / libzstd 1.5.7 (zstdmt); the ladder pairs fastest/fast/balanced/best/opt/ultra vs libzstd 1/3/9/13/17/19. Caveats: ±10% noise between runs; per-side budget 1s (500 ms on the full ladder and small); interleaved medians, spreads in the raw tables.

## Headline

- **Decode ST bulk**: we win 17/18 cells (x 0.21-0.88); skewed.zst9 the lone loss (x1.03). Absolute 635-12458 MiB/s ours; dll 1156-1533 MiB/s. All cells within ±0.05 of 09-19; dll bulk zst3/9/19 improved to 0.86/0.81/0.85.
- **Decode ST streaming** (core-vs-core): zstd still wins every compressible shape — json x1.19-1.36, text x1.04-1.22, skewed x1.10-1.31, dll x1.24-1.37 (the json/dll stream cells gave back their +1.7-2.8% with the fused-bit-read revert, R28); we win random (x0.81) and hold zeros (1.00).
- **Encode ST**: json fastest/fast x1.43/1.02, **balanced ahead at x0.86 carrying +21% density**, best/opt/ultra x1.37/1.10/1.12 at ratio parity or denser (json.opt 9 B ahead of zstd-17). text: fastest/fast x0.77/0.53 (we win); balanced x1.41 (was 1.47) at +1.65% density; best improved to x0.84 (was 0.92); opt/ultra ahead x0.60/0.68. skewed: fastest/balanced far ahead (x0.43/0.042), fast a near-tie (x1.06), best/opt/ultra behind at ratio parity (x2.13/1.02/1.08). random/zeros: we win at every tier (up to 300× on incompressible high tiers), but the fast rows pay a screen tax (random fastest/fast −8%, zeros −16% ours; x0.91/0.85 and 0.31/0.21). **dll (rewritten by the LDM campaign)**: fastest speed parity x1.01 at **−49.6% size vs zstd-1** (r 4.35 vs 2.19), fast ahead x0.95 at −31.7%, balanced x1.43 at −13.0%, best x1.38 at −12.8%, opt x1.16 at −16.2%, ultra x1.19 at −16.5%.
- **Encode MT**: json.fastest.mt8 x1.15 the one remaining clean zstd mt win; every other cell ahead — the balanced rows gained 17-26% ours (json.balanced 0.61→0.56/0.50, text.balanced x1.48→1.23/1.21, skewed.balanced 0.17→0.15). Our cold-pool mt16 json.fast (3371) still beats zstd's warm-pool reference (1643).
- **Streaming encode ST**: json.fast ahead (x0.94); json.fastest x1.30; text.best improved to x0.81 (was 1.40, ours 495→849); text.fastest/fast regressed on our side (x0.33/0.98, was 0.25/0.79 — the unpledged-stream far-class screen cost) but stay wins.
- **Streaming encode MT8**: every json cell ahead (x0.58/0.18/0.31/0.67/0.42/0.53); text fastest/fast/best/opt/ultra ahead (x0.56/0.28/0.43/0.57/0.67); **text.balanced closed to x1.06** (was 1.52, ours 663→931) — no stream-mt cell loses by more than 6%.
- **Decode MT** (our exclusive dimension, libzstd has none): **dll100.zst3 mt16 = 1.80× our ST = 1.30× the zstd stream reference** — still the strongest scaling row, on the real-binary payload; **json restored to 1.61× ST = 1.21× ref (mt2 1.29× ST)** by the fused-bit-read revert (R28: the 09-27 pass's 1.40×/1.04× regression was bisected to `b889d6d2` — the fused body's live set fits only the flat executor's loop and spilled in the staging pass, json mt2 −33%; reverted, negative/decoding.md); skewed mt16 733 vs ref 793 (the ref itself moved 682→793 since 09-19); text flat 0.98×.
- **Ratio sweep**: geo-mean **+8.69% denser** over 120 cells (bulk-st +1.48%, was +1.42) — outputs no longer byte-identical (zeros bulk −3 B/cell, text.ultra −20 B). Losing cells unchanged from 09-19: json.fast mt −0.12..−0.14%, skewed.opt −0.06..−0.07%, plus −0.00..−0.04% near-ties on skewed.fast/fastest/ultra and text.ultra mt.
- **Full 1-22 ladder** (release gate, both sides at the same numeric level): the text chain rows flipped to wins (l5-8 x0.42-0.61, l10-12 x0.38-0.62) at −2..−5% bytes; json 10-12 narrowed to x1.63-1.65 (was 1.82-2.87); the remaining holes are json 5-8 (x1.40-1.61, now carrying −7..−17% byte wins), json 10-12, text l9 (x1.41) and skewed 13-16 speed (x1.27-2.53 at ratio parity). Wins or ties everywhere else including the whole 19-22 band (except json.l19 x1.11). Full tables in [matrix.md](matrix.md) T7.
- **Small payloads**: the level-9 band **flipped from ahead (x0.54-0.90) to behind (x1.07-2.78)**, worst at 64K (json x2.04, text x2.78); level 1 regressed at 1K-4K and text-64K (to x1.49-1.77); level 3 holds near parity. Full table in [matrix.md](matrix.md) T8.

## Decode ST (MiB/s of raw; x = ours_time/zstd_time, <1 = we faster)

| file | bulk ours | bulk zstd | bulk x | stream ours | stream zstd | stream x |
|---|---:|---:|---:|---:|---:|---:|
| json.zst1 | 1743 | 1285 | 0.74 | 1751 | 2196 | 1.25 |
| json.zst3 | 1431 | 1159 | 0.81 | 1402 | 1874 | 1.34 |
| json.zst9 | 1690 | 1251 | 0.74 | 1549 | 2146 | 1.39 |
| json.zst19 | 2178 | 1289 | 0.59 | 2192 | 2623 | 1.20 |
| text.zst1 | 6023 | 2276 | 0.38 | 6278 | 7579 | 1.21 |
| text.zst3 | 8834 | 2522 | 0.29 | 10138 | 11083 | 1.09 |
| text.zst9 | 9514 | 2565 | 0.27 | 11295 | 12110 | 1.07 |
| text.zst19 | 9595 | 2554 | 0.27 | 11343 | 12159 | 1.07 |
| skewed.zst1 | 2314 | 1543 | 0.67 | 2594 | 2849 | 1.10 |
| skewed.zst3 | 1250 | 1057 | 0.85 | 1267 | 1535 | 1.21 |
| skewed.zst9 | 635 | 657 | 1.03 | 608 | 795 | 1.31 |
| skewed.zst19 | 2190 | 1484 | 0.68 | 2368 | 2704 | 1.14 |
| random.zst3 | 8950 | 2316 | 0.26 | 11115 | 8871 | 0.80 |
| zeros.zst3 | 12458 | 2630 | 0.21 | 12845 | 12879 | 1.00 |
| dll100.zst1 | 1156 | 1017 | 0.88 | 1179 | 1463 | 1.24 |
| dll100.zst3 | 1235 | 1065 | 0.86 | 1273 | 1710 | 1.34 |
| dll100.zst9 | 1533 | 1237 | 0.81 | 1568 | 2081 | 1.33 |
| dll100.zst19 | 1332 | 1131 | 0.85 | 1376 | 1820 | 1.32 |

## Encode ST bulk (checksums off; x = ours_time/zstd_time; pairs 1/3/9/13/17/19)

| level | shape | ours MiB/s | ours ratio | zstd MiB/s | zstd ratio | x |
|---|---|---:|---:|---:|---:|---:|
| fastest | json | 599 | 6.39 | 859 | 6.11 | 1.43 |
| fastest | text | 13696 | 309.23 | 10520 | 308.94 | 0.77 |
| fastest | skewed | 2937 | 2.00 | 1272 | 2.00 | 0.43 |
| fastest | random | 2411 | 1.00 | 2192 | 1.00 | 0.91 |
| fastest | zeros | 42562 | 32577 | 13273 | 32171 | 0.31 |
| fastest | dll | 531 | 4.35 | 538 | 2.19 | 1.01 |
| fast | json | 465 | 5.30 | 475 | 5.29 | 1.02 |
| fast | text | 13667 | 333.03 | 7249 | 332.90 | 0.53 |
| fast | skewed | 213 | 1.92 | 225 | 1.92 | 1.06 |
| fast | random | 2405 | 1.00 | 2051 | 1.00 | 0.85 |
| fast | zeros | 42294 | 32577 | 8700 | 32171 | 0.21 |
| fast | dll | 451 | 4.97 | 430 | 3.40 | 0.95 |
| balanced | json | 141 | 7.20 | 121 | 5.95 | 0.86 |
| balanced | text | 1225 | 384.66 | 1730 | 378.41 | 1.41 |
| balanced | skewed | 1746 | 2.00 | 74 | 1.84 | 0.042 |
| balanced | random | 2297 | 1.00 | 1593 | 1.00 | 0.69 |
| balanced | zeros | 31370 | 32577 | 1752 | 32202 | 0.056 |
| balanced | dll | 106 | 5.31 | 152 | 4.62 | 1.43 |
| best | json | 27 | 6.26 | 37 | 6.10 | 1.37 |
| best | text | 865 | 386.42 | 724 | 385.90 | 0.84 |
| best | skewed | 7 | 1.84 | 14 | 1.84 | 2.13 |
| best | random | 2362 | 1.00 | 262 | 1.00 | 0.11 |
| best | zeros | 42662 | 32577 | 836 | 32171 | 0.020 |
| best | dll | 29 | 5.35 | 40 | 4.67 | 1.38 |
| opt | json | 6 | 7.49 | 7 | 7.49 | 1.10 |
| opt | text | 776 | 410.23 | 461 | 410.11 | 0.60 |
| opt | skewed | 3 | 2.00 | 3 | 2.00 | 1.02 |
| opt | random | 2382 | 1.00 | 12 | 1.00 | 0.005 |
| opt | zeros | 40893 | 32577 | 942 | 32202 | 0.023 |
| opt | dll | 12 | 6.04 | 14 | 5.06 | 1.16 |
| ultra | json | 3 | 7.42 | 3 | 7.42 | 1.12 |
| ultra | text | 394 | 414.12 | 269 | 413.98 | 0.68 |
| ultra | skewed | 2 | 2.00 | 2 | 2.00 | 1.08 |
| ultra | random | 2408 | 1.00 | 8 | 1.00 | 0.003 |
| ultra | zeros | 39180 | 32577 | 681 | 32202 | 0.017 |
| ultra | dll | 8 | 6.32 | 9 | 5.28 | 1.19 |

Ratio verdict: denser or at parity on json and text at every level (json.opt 9 B ahead); skewed wins everywhere except near-ties; random/zeros tie or win; dll denser at every tier — −49.6% bytes at fastest, −31.7% at fast, double digits from balanced up. Checksum on/off (ours): json.fast 1.00, text.fast 0.86.

## Encode MT bulk (1 pass; cold pool per call both sides; MiB/s)

| cell | ours mt8 | zstd mt8 | x8 | ours mt16 | zstd mt16 | x16 | ratio ours mt16 | ratio zstd mt16 |
|---|---:|---:|---:|---:|---:|---:|---:|---:|
| json.fastest | 3706 | 4217 | 1.15 | 5281 | 1822 | 0.35 | 6.34 | 6.11 |
| json.fast | 2581 | 1055 | 0.41 | 3371 | 1044 | 0.31 | 5.30 | 5.31 |
| json.balanced | 368 | 207 | 0.56 | 418 | 206 | 0.50 | 7.19 | 5.95 |
| text.fastest | 20696 | 11166 | 0.54 | 18706 | 2280 | 0.12 | 309.08 | 39.78 |
| text.fast | 12596 | 2295 | 0.18 | 12026 | 2189 | 0.18 | 332.94 | 189.16 |
| text.balanced | 1032 | 1264 | 1.23 | 1046 | 1247 | 1.21 | 384.59 | 378.30 |
| skewed.fastest | 4348 | 3278 | 0.76 | 3687 | 1587 | 0.43 | 2.00 | 2.00 |
| skewed.fast | 1281 | 649 | 0.51 | 1660 | 647 | 0.39 | 1.92 | 1.92 |
| skewed.balanced | 831 | 123 | 0.15 | 813 | 122 | 0.15 | 2.00 | 1.84 |

## Streaming encode (64KiB pulls; json/text; interleaved medians)

| cell | ST ours | ST zstd | ST x | MT8 ours | MT8 zstd | MT8 x |
|---|---:|---:|---:|---:|---:|---:|
| json.fastest | 594 | 772 | 1.30 | 3182 | 1899 | 0.58 |
| json.fast | 464 | 439 | 0.94 | 2279 | 402 | 0.18 |
| json.balanced | — | — | — | 370 | 116 | 0.31 |
| json.best | 26 | 35 | 1.37 | 79 | 53 | 0.67 |
| json.opt | — | — | — | 16 | 7 | 0.42 |
| json.ultra | — | — | — | 6 | 3 | 0.53 |
| text.fastest | 5240 | 1739 | 0.33 | 7456 | 4154 | 0.56 |
| text.fast | 5118 | 5013 | 0.98 | 5908 | 1603 | 0.28 |
| text.balanced | — | — | — | 931 | 981 | 1.06 |
| text.best | 849 | 686 | 0.81 | 856 | 368 | 0.43 |
| text.opt | — | — | — | 674 | 380 | 0.57 |
| text.ultra | — | — | — | 367 | 241 | 0.67 |

Unknown-size text.fastest streaming: zstd emits 1.84MB (ratio 18.3) vs our 108KB (ratio 309). Stream/ceiling ratios in [matrix.md](matrix.md) T5.

## Decode MT scaling (1 pass; our solo dimension; MiB/s)

| file | ours ST | mt2 | mt4 | mt8 | mt16 | zstd ST stream ref |
|---|---:|---:|---:|---:|---:|---:|
| json.zst3 | 1401 | 1271 | 1734 | 1999 | 1957 | 1887 |
| text.zst3 | 11110 | 10973 | 10966 | 10994 | 10941 | 11151 |
| skewed.zst9 | 605 | 636 | 638 | 609 | 592 | 791 |
| random.zst3 | 9732 | 9533 | 9565 | 9567 | 9546 | 8951 |
| dll100.zst3 | 1225 | 1520 | 1988 | 2101 | 2194 | 1710 |

dll100 measured via `files --threads` (see [matrix.md](matrix.md) T2) — the restart-point decode scales best on the large real-binary payload. json/skewed dropped vs 09-19 (json mt16 2199→1957) while their ST and zstd-ref cells held — a real movement on our piece-parallel path.

## Compression-ratio sweep (`zstdx-bench ratio`, 2026-09-27)

Geo-mean Δ over 120 cells **+8.69%**; per mode bulk-st +1.48% / bulk-mt +11.08% / stream-st +11.43% / stream-mt +11.11%; per shape json +4.41%, text +40.36%, skewed +1.41%, random ±0, zeros +2.06%. Outputs are not byte-identical to 09-19 (zeros bulk-st −3 B/cell, text.ultra −20 B; the tier-axis cells otherwise reproduce) — first byte movement since the 09-16 corpus. Losing cells (complete): json.fast mt −0.12..−0.14%, text.ultra mt −0.02%, skewed.opt −0.06..−0.07%, skewed.ultra −0.04%, skewed.fast mt −0.01..−0.03%, skewed.fastest −0.00%. Details in [matrix.md](matrix.md) T6.

## Top open deficits (from this run, x = ours/zstd wall time)

1. **Small-payload level 9** (todo 4): the whole 1K-1M band flipped from ahead (x0.54-0.90 on 09-19) to behind — json x1.39/1.40/2.04/1.07, text x1.65/1.45/2.78/1.08, worst at 64K. Level 1 also slid at 1K-4K and text-64K (to x1.49-1.77). Suspects: the R14 small-frame entropy tails and the R20 mid-band pre-header far-class screen (per-call costs on exactly this band).
2. **json chain rows 5-8 and 10-12** (full-ladder T7): x1.40-1.61 and x1.63-1.65 — 10-12 improved from 1.82-2.87 but 5-6 worsened (1.23/1.19→1.40/1.49), now carrying −7..−17% byte wins over zstd; our own level-9 row beats both bands on the two axes. A ladder-tuning question, not a matcher-core one.
3. **text.balanced speed x1.41 ST** (improved from 1.47) — the residual cold-start DUBT head per-frame cost; ratio +1.65% denser than zstd-9 (floor in [todo](../todo.md)).
4. **Streaming decode on compressible shapes**: json x1.20-1.39, skewed x1.10-1.31, text x1.07-1.21 — the fused-loop serial chain remains (closed as a measured op-volume floor, [dec-gap](dec-gap.md)).
5. **json.fastest x1.43 ST / x1.15 mt8** — the steady scan loop's inherent branch-mispredict budget; realistic ceiling ~x1.3-1.4 (todo 3). json.fast x1.02 is near closed.
6. **Best-tier core**: json x1.37, skewed x2.13 (memory-latency tree walk; floor in [todo](../todo.md)). Caps the json.best stream ST cell (x1.37).
7. **json opt/ultra x1.10-1.12, skewed opt/ultra x1.02-1.08** — per-node codegen + event-volume residue at ratio parity or denser (floor in [todo](../todo.md)).
8. **MT decode regression** (todo 1) — **resolved R28 (2026-09-28)**: bisect confirmed the fused bit-read decoder commit (`b889d6d2`: json mt2 −33% at flat ST; the fused body's six-widths-six-bases live set fits only the flat executor, the staging pass spilled it — three rework shapes all falsified, [negative](../negative/decoding.md)). Reverted: json mt16 1.61× ST = 1.21× ref (mt2 1.29× ST), dll 1.80×/1.30×; the ST stream json/dll cells gave back their +1.7-2.8% (json x1.36, dll x1.37 worst cells).
9. **dll100 balanced x1.43 at −13.0% size** (floor in [todo](../todo.md), differential attribution in [enc-dll-gap](enc-dll-gap.md)); fastest/fast are now speed parity-or-ahead carrying −49.6/−31.7% size — the remaining gap is the balanced chain chase.
10. **text ST stream fastest/fast**: ours −28/−26% vs 09-19 (7243→5240, 6943→5118) — the unpledged-stream far-class screen (R23); still wins at x0.33/0.98.
11. **Fast-row screen tax on compressible-flat data**: random fastest/fast ours −8%, zeros fastest/fast −16% (x0.91/0.85, 0.31/0.21 — still wins); skewed chain rows −12..−42% (x≤0.06). The far-class screens cost even when they find nothing.
12. **Ratio residues**: json.fast mt −0.12..−0.14%, skewed.opt −0.07% (near-tie), dictionary small-payload parse-side (todo 6).
