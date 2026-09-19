# Perf vs zstd crate · current snapshot (2026-09-19)

> Release pass at `e0b6078b`, 2026-09-19, one machine, one corpus, same flags. Provenance, raw tables and per-chunk commands: [matrix.md](matrix.md), including the full 1-22 numeric ladder (T7) and the small-payload table (T8). The 67 commits since the 09-18/09-19 passes were correctness/review fixes plus two decoder-stream staging commits (`read_to_end` probe growth, `write::Decoder` bounded cursor) — corpus encoder outputs are byte-identical (the ratio sweep reproduces +8.67% exactly), so speed moved only where those commits touch and the dll rows are new. **dll100 (100 MB of real system ELF binaries, `bench/gen_big.sh`) is promoted to a formal shape**: full six-tier encode, four-level decode and an MT-decode scaling row now sit in the shape axes. Comparison target: zstd crate / libzstd 1.5.7 (zstdmt); the ladder pairs fastest/fast/balanced/best/opt/ultra vs libzstd 1/3/9/13/17/19. Caveats: ±10% noise between runs; per-side budget 1s (500 ms on the full ladder and small); interleaved medians, spreads in the raw tables.

## Headline

- **Decode ST bulk**: we win 17/18 cells (x 0.21-0.89); skewed.zst9 the lone parity (1.01). Absolute 650-12452 MiB/s ours; dll 1144-1527 MiB/s. json zst9/zst19 bulk ticked up to 0.71/0.60 (was 0.73/0.62).
- **Decode ST streaming** (core-vs-core): zstd still wins every compressible shape but the staging commits narrowed the text column — json x1.19-1.37, skewed x1.10-1.32, **text x1.04-1.22 (was 1.09-1.24)**, dll x1.30-1.37; we win random (x0.80) and hold zeros (1.00).
- **Encode ST**: json fastest/fast x1.43/1.04, **balanced ahead at x0.86 carrying +21% density**, best/opt/ultra x1.40/1.10/1.11 at ratio parity or denser (json.opt 9 B ahead of zstd-17). text: fastest/fast x0.74/0.52 (we win); best/opt/ultra ahead x0.92/0.60/0.69; balanced x1.47 (was 1.51), still +1.65% denser than zstd-9. skewed: low tiers ahead or parity (fast x1.07 near-tie, best x2.12, opt/ultra x1.05/1.09). random/zeros: we win at every tier (up to 300× on incompressible high tiers). **dll (new axis)**: fastest x1.36 (+1.8% size), fast x1.06 (−2.2%), balanced x1.49 at −8.4% size, and the first-measured best/opt/ultra x1.39/1.10/1.19 at −12.8/−10.3/−10.5% size — double-digit density wins from balanced up.
- **Encode MT**: json.fastest.mt8 x1.18 the one remaining clean zstd mt win (noisy 0.92-1.57; mt16 ours x0.34); every other cell ahead except text.balanced x1.48 (was 1.51-1.54). Our cold-pool mt16 json.fast (3402) still beats zstd's warm-pool reference (1636).
- **Streaming encode ST**: json.fast ahead (x0.94); json.fastest x1.31; best-tier bounded by the core (json/text.best x1.46/1.40).
- **Streaming encode MT8**: all json cells ahead (x0.56/0.19/0.34/0.73/0.42/0.53); text fastest/fast/best/opt/ultra ahead (x0.52/0.28/0.68/0.59/0.68); text.balanced x1.52 the last stream-mt cell behind.
- **Decode MT** (our exclusive dimension, libzstd has none): **dll100.zst3 mt16 = 1.82× our ST = 1.30× the zstd stream reference** — the strongest scaling row, on the real-binary payload; json.zst3 mt16 = 1.57× ST = 1.17× ref; skewed 1.09-1.21× ST but below the ref; text flat 0.97-0.98×; random 0.98× ST = 1.07× ref.
- **Ratio sweep**: geo-mean **+8.67%** denser over 120 cells (bulk-st +1.42%), outputs byte-identical to 09-18. Remaining losing cells: json.fast mt −0.12..−0.14%, skewed.opt −0.06..−0.07%, plus −0.00..−0.04% near-ties on skewed.fast/fastest/ultra and text.ultra mt.
- **Full 1-22 ladder** (release gate, both sides at the same numeric level): structure unchanged from 09-18 — wins or ties at the ladder ends and on random/zeros throughout; the holes are json 5-8 (x1.19-1.62) and 10-12 (x1.82-2.87, the chain-family rows; our level-9 row beats them on both axes) and skewed 13-16 speed (x1.25-2.58 at ratio parity). Full tables in [matrix.md](matrix.md) T7.

## Decode ST (MiB/s of raw; x = ours_time/zstd_time, <1 = we faster)

| file | bulk ours | bulk zstd | bulk x | stream ours | stream zstd | stream x |
|---|---:|---:|---:|---:|---:|---:|
| json.zst1 | 1731 | 1277 | 0.74 | 1728 | 2189 | 1.27 |
| json.zst3 | 1438 | 1144 | 0.80 | 1373 | 1875 | 1.37 |
| json.zst9 | 1724 | 1231 | 0.71 | 1640 | 2143 | 1.31 |
| json.zst19 | 2166 | 1293 | 0.60 | 2194 | 2606 | 1.19 |
| text.zst1 | 5971 | 2258 | 0.38 | 6236 | 7590 | 1.22 |
| text.zst3 | 8760 | 2515 | 0.29 | 10434 | 11123 | 1.07 |
| text.zst9 | 9364 | 2544 | 0.27 | 11599 | 12109 | 1.04 |
| text.zst19 | 9382 | 2519 | 0.27 | 11559 | 12150 | 1.05 |
| skewed.zst1 | 2310 | 1536 | 0.67 | 2590 | 2847 | 1.10 |
| skewed.zst3 | 1246 | 1061 | 0.85 | 1256 | 1534 | 1.22 |
| skewed.zst9 | 650 | 660 | 1.01 | 603 | 793 | 1.32 |
| skewed.zst19 | 2203 | 1491 | 0.68 | 2432 | 2695 | 1.11 |
| random.zst3 | 9010 | 2368 | 0.26 | 11138 | 8933 | 0.80 |
| zeros.zst3 | 12452 | 2635 | 0.21 | 12898 | 12901 | 1.00 |
| dll100.zst1 | 1144 | 1023 | 0.89 | 1159 | 1510 | 1.30 |
| dll100.zst3 | 1226 | 1094 | 0.89 | 1247 | 1708 | 1.37 |
| dll100.zst9 | 1527 | 1247 | 0.82 | 1539 | 2084 | 1.35 |
| dll100.zst19 | 1322 | 1134 | 0.86 | 1343 | 1803 | 1.34 |

## Encode ST bulk (checksums off; x = ours_time/zstd_time; pairs 1/3/9/13/17/19)

| level | shape | ours MiB/s | ours ratio | zstd MiB/s | zstd ratio | x |
|---|---|---:|---:|---:|---:|---:|
| fastest | json | 600 | 6.39 | 858 | 6.11 | 1.43 |
| fastest | text | 14212 | 309.22 | 10455 | 308.94 | 0.74 |
| fastest | skewed | 2962 | 2.00 | 1274 | 2.00 | 0.43 |
| fastest | random | 2623 | 1.00 | 2214 | 1.00 | 0.84 |
| fastest | zeros | 50683 | 32483 | 13363 | 32171 | 0.26 |
| fastest | dll | 392 | 2.15 | 530 | 2.19 | 1.36 |
| fast | json | 458 | 5.30 | 477 | 5.29 | 1.04 |
| fast | text | 14085 | 333.02 | 7275 | 332.90 | 0.52 |
| fast | skewed | 210 | 1.92 | 224 | 1.92 | 1.07 |
| fast | random | 2597 | 1.00 | 2068 | 1.00 | 0.80 |
| fast | zeros | 50425 | 32483 | 8730 | 32171 | 0.17 |
| fast | dll | 411 | 3.47 | 434 | 3.40 | 1.06 |
| balanced | json | 142 | 7.20 | 122 | 5.95 | 0.86 |
| balanced | text | 1178 | 384.64 | 1734 | 378.41 | 1.47 |
| balanced | skewed | 1860 | 2.00 | 75 | 1.84 | 0.040 |
| balanced | random | 2305 | 1.00 | 1600 | 1.00 | 0.69 |
| balanced | zeros | 31840 | 32483 | 1752 | 32202 | 0.054 |
| balanced | dll | 101 | 5.05 | 151 | 4.62 | 1.49 |
| best | json | 27 | 6.26 | 37 | 6.10 | 1.40 |
| best | text | 824 | 386.39 | 742 | 385.90 | 0.92 |
| best | skewed | 6 | 1.84 | 14 | 1.84 | 2.12 |
| best | random | 2377 | 1.00 | 264 | 1.00 | 0.11 |
| best | zeros | 42266 | 32483 | 836 | 32171 | 0.020 |
| best | dll | 28 | 5.35 | 39 | 4.67 | 1.39 |
| opt | json | 6 | 7.49 | 7 | 7.49 | 1.10 |
| opt | text | 773 | 410.18 | 463 | 410.11 | 0.60 |
| opt | skewed | 3 | 2.00 | 3 | 2.00 | 1.05 |
| opt | random | 2393 | 1.00 | 12 | 1.00 | 0.005 |
| opt | zeros | 40881 | 32483 | 943 | 32202 | 0.022 |
| opt | dll | 13 | 5.64 | 14 | 5.06 | 1.10 |
| ultra | json | 3 | 7.42 | 3 | 7.42 | 1.11 |
| ultra | text | 393 | 414.04 | 272 | 413.98 | 0.69 |
| ultra | skewed | 2 | 2.00 | 2 | 2.00 | 1.09 |
| ultra | random | 2390 | 1.00 | 8 | 1.00 | 0.003 |
| ultra | zeros | 39985 | 32483 | 675 | 32202 | 0.017 |
| ultra | dll | 8 | 5.89 | 9 | 5.28 | 1.19 |

Ratio verdict: denser or at parity on json and text at every level (json.opt 9 B ahead); skewed wins everywhere except near-ties; random/zeros tie or win; dll denser from fast up, double digits from balanced up. Checksum on/off (ours): json.fast 1.03, text.fast 0.84.

## Encode MT bulk (1 pass; cold pool per call both sides; MiB/s)

| cell | ours mt8 | zstd mt8 | x8 | ours mt16 | zstd mt16 | x16 | ratio ours mt16 | ratio zstd mt16 |
|---|---:|---:|---:|---:|---:|---:|---:|---:|
| json.fastest | 3702 | 4332 | 1.18 | 5346 | 1824 | 0.34 | 6.34 | 6.11 |
| json.fast | 2605 | 1069 | 0.41 | 3402 | 1054 | 0.31 | 5.30 | 5.31 |
| json.balanced | 337 | 204 | 0.61 | 331 | 202 | 0.61 | 7.19 | 5.95 |
| text.fastest | 22185 | 11496 | 0.52 | 20199 | 2328 | 0.11 | 309.08 | 39.78 |
| text.fast | 12782 | 2319 | 0.18 | 11759 | 2258 | 0.19 | 332.94 | 189.16 |
| text.balanced | 855 | 1259 | 1.48 | 856 | 1260 | 1.48 | 384.59 | 378.30 |
| skewed.fastest | 4302 | 3310 | 0.77 | 3657 | 1601 | 0.44 | 2.00 | 2.00 |
| skewed.fast | 1302 | 645 | 0.50 | 1664 | 647 | 0.39 | 1.92 | 1.92 |
| skewed.balanced | 699 | 122 | 0.18 | 697 | 122 | 0.17 | 2.00 | 1.84 |

## Streaming encode (64KiB pulls; json/text; interleaved medians)

| cell | ST ours | ST zstd | ST x | MT8 ours | MT8 zstd | MT8 x |
|---|---:|---:|---:|---:|---:|---:|
| json.fastest | 596 | 780 | 1.31 | 3193 | 1893 | 0.56 |
| json.fast | 473 | 443 | 0.94 | 2201 | 410 | 0.19 |
| json.balanced | — | — | — | 332 | 114 | 0.34 |
| json.best | 24 | 36 | 1.46 | 71 | 51 | 0.73 |
| json.opt | — | — | — | 16 | 7 | 0.42 |
| json.ultra | — | — | — | 6 | 3 | 0.53 |
| text.fastest | 7243 | 1775 | 0.25 | 7800 | 4070 | 0.52 |
| text.fast | 6943 | 5456 | 0.79 | 6185 | 1713 | 0.28 |
| text.balanced | — | — | — | 663 | 1004 | 1.52 |
| text.best | 495 | 691 | 1.40 | 526 | 357 | 0.68 |
| text.opt | — | — | — | 647 | 378 | 0.59 |
| text.ultra | — | — | — | 353 | 239 | 0.68 |

Unknown-size text.fastest streaming: zstd emits 1.84MB (ratio 18.3) vs our 108KB (ratio 309). Stream/ceiling ratios in [matrix.md](matrix.md) T5.

## Decode MT scaling (1 pass; our solo dimension; MiB/s)

| file | ours ST | mt2 | mt4 | mt8 | mt16 | zstd ST stream ref |
|---|---:|---:|---:|---:|---:|---:|
| json.zst3 | 1402 | 1585 | 2014 | 2160 | 2199 | 1884 |
| text.zst3 | 11188 | 10885 | 10866 | 10918 | 10921 | 11183 |
| skewed.zst9 | 613 | 739 | 725 | 667 | 682 | 795 |
| random.zst3 | 9749 | 9532 | 9561 | 9567 | 9560 | 8977 |
| dll100.zst3 | 1220 | 1774 | 2004 | 2192 | 2217 | 1708 |

dll100 measured via `files --threads` (see [matrix.md](matrix.md) T2) — the restart-point decode scales best on the large real-binary payload.

## Compression-ratio sweep (`zstdx-bench ratio`, 2026-09-19)

Geo-mean Δ over 120 cells **+8.67%**; per mode bulk-st +1.42% / bulk-mt +11.08% / stream-st +11.43% / stream-mt +11.11%; per shape json +4.41%, text +40.36%, skewed +1.41%, random ±0, zeros +1.99%. Outputs byte-identical to the 09-18 sweep (the intervening commits moved no corpus cell's bytes). Losing cells (complete): json.fast mt −0.12..−0.14%, text.ultra mt −0.04%, skewed.opt −0.06..−0.07%, skewed.ultra −0.04%, skewed.fast mt −0.01..−0.03%, skewed.fastest −0.00%. Details in [matrix.md](matrix.md) T6.

## Top open deficits (from this run, x = ours/zstd wall time)

1. **text.balanced speed x1.47 ST / x1.48 mt / x1.52 stream-mt8** — the residual cold-start DUBT head per-frame cost; ratio +1.65% denser than zstd-9 (floor in [todo](../todo.md)).
2. **Streaming decode on compressible shapes**: json x1.19-1.37, skewed x1.10-1.32, text x1.04-1.22 — narrowed by the staging commits; the fused-loop serial chain remains (todo 2).
3. **json.fastest x1.43 ST / x1.18 mt8** — the steady scan loop's inherent branch-mispredict budget; realistic ceiling ~x1.3-1.4 (todo 3). json.fast x1.04 is near closed.
4. **Best-tier core**: json x1.40, skewed x2.12 (memory-latency tree walk; floor in [todo](../todo.md)). Caps the two stream ST cells (json/text.best x1.46/1.40).
5. **json opt/ultra x1.10-1.11, skewed.ultra x1.09** — per-node codegen + event-volume residue at ratio parity or denser (floor in [todo](../todo.md)).
6. **json chain rows 5-8 and 10-12** (full-ladder T7): x1.19-1.62 and x1.82-2.87, the weakest speed band — the deep-chain rows lose to libzstd's chain on both axes while our own level-9 row beats them; a ladder-tuning question, not a matcher-core one.
7. **MT decode**: text flat ~0.98×, skewed below the zstd stream ref — decoder-side piece-parallel stage B to spend the ramp guarantee (todo 1's open half). json (1.17× ref) and dll (1.30× ref) scale well.
8. **dll100 low-tier encode** (full tier rows now in the tables above; floor in [todo](../todo.md)): fastest x1.36, fast x1.06, balanced x1.49 at −8.4% size — matcher-side instruction counts; emit live-set shrink the one untried lever. High tiers x1.10-1.39 at −10..−13% size.
9. **Small band 64K-1M** fastest x~1.36-1.48 (this run: json-64K/1M x1.41/1.36, text x1.41/1.48; level 3 near parity 0.96-1.31; level 9 ahead 0.54-0.90 except the 1M cells x1.04-1.06) (todo 4).
10. **Ratio residues**: json.fast mt −0.12..−0.14%, skewed.opt −0.07% (near-tie), dictionary small-payload +3% parse-side (todo 6).
