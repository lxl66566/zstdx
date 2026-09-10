# Perf vs zstd crate · current snapshot (2026-09-11)

> Fresh full pass at `96867c5`, one day, one machine, one corpus. Raw tables + per-section commands: [matrix.md](matrix.md). Comparison target: zstd crate 0.13.3 / libzstd 1.5.7 (zstdmt), rustc 1.100.0-nightly, AMD Zen4-class 32C (Eng Sample 100-000000870-32_Y). Caveats: ±10% noise, sustained clocks ~70-77% of max boost, per-side budget 1.5-2s; dec-st/enc-st double-passed, mt/stream sections single-pass.

## Headline

- **Decode ST bulk**: we win 10/11 cells (x 0.21-0.80), parity on skewed.zst9 (1.00); absolute 636-11157 MiB/s ours. Caveat kept from methodology: zstd's bulk API is a slow wrapper — the honest decode gap is the streaming column.
- **Decode ST streaming**: zstd wins every compressible shape — json x1.26-1.36, skewed x1.11-1.31, text x1.04-1.22; we win random (x0.80, 10872 vs 8706 MiB/s) and hold zeros (1.00). text.zst9 is near-parity (1.04, 11588 vs 12099).
- **Encode ST**: we win text (fastest/fast/balanced: x0.67-0.93), skewed (fastest x0.45, balanced x0.055 at 1935 vs 105 MiB/s), zeros (x0.016-0.27); we lose json at every level (x1.15-1.78 except ultra) and random at low levels (x1.27-1.43). **Best level is now the biggest encoder deficit**: x4.52 json / x2.44 text / x3.52 skewed / x29 random (historical 2026-09-10: Best led at x0.48-0.69) — the opt-parser core bought ratio (json 6.83 vs 6.08, text 404.8 vs 383.9, skewed 2.00 vs 1.84) at 2.4-29x speed. Ultra (vs zstd-19) wins speed on 4/5 shapes (json x0.71, skewed x0.44, random x0.46, zeros x0.016; text x0.92) with ratio parity.
- **Encode MT**: big win at equal workers where it matters — json.fast mt16 x0.31 (3203 vs 985 MiB/s), text.fast mt8/mt16 x0.13-0.14 (~16000/13751 vs ~2000), skewed mt8 0.21-0.77. Losses: json.fastest.mt8 x1.37 (zstd-l1-mt8 hits 4121), text.balanced.mt16 x1.28 (but zstd-mt ratio collapses to 212.6 vs our 360.9 there). Ratio preservation is ours alone: every mt cell within 0.5% of our ST; zstd-mt loses 1.4-8x ratio on text (189 vs 333 at fast, 39.8 vs 309 at fastest). Our cold-pool mt16 beats zstd's warm-pool reference (3203 vs 1608 MiB/s).
- **Streaming encode ST**: text wins (fastest x0.29 with a 17x ratio win — zstd collapses to ratio 18.3 vs our 309; fast x0.84); json loses (fastest x1.64, fast x1.18, best x4.00 — bounded by the Best core, not the pipeline).
- **Streaming encode MT8**: json fast/balanced win big (x0.27/x0.50), text fast x0.42; fastest parity on both (x1.03/x1.00); best loses x2.3-2.6 (same Best-core deficit). Stream reaches 61-80% (json) / 19-63% (text) of our own bulk-mt8 ceiling.
- **Decode MT** (our exclusive dimension, libzstd has none): still a negative asset — no scaling (text/random 0.93-0.98x ST), skewed.zst9 anti-scales to 0.69x ST at every width; best case json.zst3 mt16 = 1.29x ST, still below the zstd ST streaming reference (0.93x).

## Decode ST (2 passes; MiB/s of raw; x = ours_time/zstd_time, <1 = we faster)

| file | bulk ours | bulk zstd | bulk x | stream ours | stream zstd | stream x |
|---|---:|---:|---:|---:|---:|---:|
| json.zst1 | 1738 | 1186 | 0.68 | 1736 | 2190 | 1.26 |
| json.zst3 | 1428 | 1072 | 0.75 | 1380 | 1872 | 1.36 |
| json.zst9 | 1720 | 1144 | 0.67 | 1646 | 2138 | 1.30 |
| text.zst1 | 5428 | 2007 | 0.37 | 6230 | 7583 | 1.22 |
| text.zst3 | 8226 | 2222 | 0.27 | 10418 | 11105 | 1.07 |
| text.zst9 | 9003 | 2284 | 0.25 | 11588 | 12099 | 1.04 |
| skewed.zst1 | 2268 | 1420 | 0.63 | 2546 | 2824 | 1.11 |
| skewed.zst3 | 1230 | 986 | 0.80 | 1260 | 1526 | 1.21 |
| skewed.zst9 | 636 | 638 | 1.00 | 602 | 788 | 1.31 |
| random.zst3 | 8688 | 2034 | 0.23 | 10872 | 8706 | 0.80 |
| zeros.zst3 | 11157 | 2292 | 0.21 | 12851 | 12896 | 1.00 |

## Encode ST bulk (2 passes; checksums off; x = ours_time/zstd_time)

| level | shape | ours MiB/s | ours ratio | zstd MiB/s | zstd ratio | x |
|---|---|---:|---:|---:|---:|---:|
| fastest | json | 477 | 6.15 | 852 | 6.11 | 1.78 |
| fastest | text | 11279 | 309.17 | 10455 | 308.94 | 0.93 |
| fastest | skewed | 2665 | 2.00 | 1210 | 2.00 | 0.45 |
| fastest | random | 1482 | 1.00 | 1966 | 1.00 | 1.33 |
| fastest | zeros | 49130 | 32483 | 13278 | 32171 | 0.27 |
| fast | json | 371 | 5.36 | 480 | 5.29 | 1.29 |
| fast | text | 10821 | 332.97 | 7189 | 332.90 | 0.67 |
| fast | skewed | 164 | 1.92 | 225 | 1.92 | 1.37 |
| fast | random | 1291 | 1.00 | 1841 | 1.00 | 1.43 |
| fast | zeros | 48518 | 32483 | 8650 | 32171 | 0.18 |
| balanced | json | 149 | 6.08 | 185 | 5.76 | 1.24 |
| balanced | text | 3168 | 361.45 | 2748 | 370.17 | 0.87 |
| balanced | skewed | 1935 | 2.00 | 105 | 1.86 | 0.055 |
| balanced | random | 1284 | 1.00 | 1639 | 1.00 | 1.27 |
| balanced | zeros | 46475 | 32483 | 2779 | 32202 | 0.060 |
| best | json | 12 | 6.83 | 54 | 6.08 | 4.52 |
| best | text | 358 | 404.78 | 872 | 383.85 | 2.44 |
| best | skewed | 6 | 2.00 | 24 | 1.84 | 3.52 |
| best | random | 22 | 1.00 | 632 | 1.00 | 29.2 |
| best | zeros | 42649 | 32483 | 959 | 32202 | 0.023 |
| opt | json | 8 | 7.46 | 10 | 7.10 | 1.15 |
| opt | text | 470 | 408.76 | 528 | 406.09 | 1.12 |
| opt | skewed | 7 | 2.00 | 8 | 2.00 | 1.10 |
| opt | random | 22 | 1.00 | 21 | 1.00 | 0.96 |
| opt | zeros | 43128 | 32483 | 1050 | 32202 | 0.024 |
| ultra | json | 4 | 7.44 | 3 | 7.42 | 0.71 |
| ultra | text | 290 | 411.58 | 266 | 413.98 | 0.92 |
| ultra | skewed | 4 | 2.00 | 2 | 2.00 | 0.44 |
| ultra | random | 16 | 1.00 | 8 | 1.00 | 0.46 |
| ultra | zeros | 40880 | 32483 | 649 | 32202 | 0.016 |

Ratio verdict: we now match or beat zstd at every ST level on json (6.15/6.11 fastest → 7.44/7.42 ultra), on text except balanced/ultra (marginal losses 361 vs 370, 411.6 vs 414.0), on skewed everywhere except fast (tie), and tie on random/zeros. Checksum on/off (ours): json.fast 1.00-1.03, text.fast 0.91-0.93 (on faster).

## Encode MT bulk (1 pass; cold pool per call both sides; MiB/s)

| cell | ours mt8 | zstd mt8 | x8 | ours mt16 | zstd mt16 | x16 | ratio ours mt16 | ratio zstd mt16 |
|---|---:|---:|---:|---:|---:|---:|---:|---:|
| json.fastest | 3025 | 4121 | 1.37 | 4414 | 1642 | 0.38 | 6.14 | 6.11 |
| json.fast | 2306 | 1042 | 0.45 | 3203 | 985 | 0.31 | 5.37 | 5.31 |
| json.balanced | 560 | 541 | 0.98 | 532 | 531 | 1.00 | 6.09 | 5.76 |
| text.fastest | 18218 | 11109 | 0.61 | 16392 | 1980 | 0.12 | 307.82 | 39.78 |
| text.fast | 16014 | 2040 | 0.13 | 13751 | 1921 | 0.14 | 332.43 | 189.16 |
| text.balanced | 2691 | 1640 | 0.61 | 1229 | 1582 | 1.28 | 360.85 | 212.55 |
| skewed.fastest | 3904 | 3027 | 0.77 | 3365 | 1378 | 0.41 | 2.00 | 2.00 |
| skewed.fast | 1055 | 618 | 0.58 | 1569 | 613 | 0.39 | 1.92 | 1.92 |
| skewed.balanced | 1656 | 345 | 0.21 | 762 | 345 | 0.46 | 2.00 | 1.86 |

MT ratio preservation (ours vs own ST, all cells within ±0.5%): fixed since `a37ebaa`; the old text mt16 collapse (historical r 19.7 @ `b1dd010`) is gone (332.43 now). zstd warm-pool json.fast mt16 reference: 1608 MiB/s.

## Streaming encode (1 pass; 64KiB pulls; json/text only)

| cell | ST ours | ST zstd | ST x | MT8 ours | MT8 zstd | MT8 x |
|---|---:|---:|---:|---:|---:|---:|
| json.fastest | 475 | 776 | 1.64 | 1865 | 1921 | 1.03 |
| json.fast | 374 | 440 | 1.18 | 1433 | 391 | 0.27 |
| json.balanced | — | — | — | 450 | 222 | 0.50 |
| json.best | 12 | 48 | 4.00 | 32 | 82 | 2.58 |
| text.fastest | 6010 | 1767 | 0.29 | 4459 | 4481 | 1.00 |
| text.fast | 6481 | 5463 | 0.84 | 3676 | 1543 | 0.42 |
| text.balanced | — | — | — | 1273 | 1213 | 0.96 |
| text.best | 340 | 799 | 2.36 | 275 | 631 | 2.30 |

Unknown-size text.fastest streaming: zstd emits 1.84MB (ratio 18.3) vs our 108KB (ratio 309) — 17x ratio + 3.4x speed. Bulk-mt8 ceilings and stream/ceiling ratios in [matrix.md](matrix.md).

## Decode MT scaling (1 pass; our solo dimension; MiB/s)

| file | ours ST | mt2 | mt4 | mt8 | mt16 | zstd ST stream ref |
|---|---:|---:|---:|---:|---:|---:|
| json.zst3 | 1359 | 1323 | 1648 | 1549 | 1747 | 1879 |
| text.zst3 | 10681 | 10285 | 10250 | 9955 | 10314 | 11181 |
| skewed.zst9 | 637 | 438 | 441 | 441 | 438 | 788 |
| random.zst3 | 9720 | 9511 | 9523 | 9507 | 9445 | 8777 |

## Top open deficits (from this run)

- Best-level encoder core speed: x2.4-29 behind zstd-12 across shapes; also caps json/text best streaming (bulk and stream MT both x2.3-4.0).
- json ST encode speed at fastest/fast/balanced/opt: x1.15-1.78 behind.
- random ST encode at fastest/fast/balanced: x1.27-1.43 behind (raw-block per-block overhead); random.best catastrophic (x29).
- skewed.fast ST: x1.37 behind (only skewed cell we lose besides best).
- Streaming decode on compressible shapes: x1.04-1.36 behind (text.zst9 near-parity).
- MT decode scaling: none (skewed.zst9 0.69x anti-scaling).
- json.fastest/text.balanced MT cells: x1.28-1.37 behind at one width each.
