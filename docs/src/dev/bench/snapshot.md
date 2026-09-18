# Perf vs zstd crate · current snapshot (2026-09-18)

> Pre-release full pass at `c56418c2` (one machine, one corpus, same flags; 78 commits after the 09-16 pass at `dedd267d` — the huff0 two-queue tree build + one-form table materialization, the FSE CTable rewrite (C-shaped build, pooled 32 B tables, NCount at C parity), the mt-stream worker pool + shared finish-tail prefix fill + mid-size far-class captures, the matcher work (fused tables, dict grids, cold-head probe step, landslide abort) and the block-splitter estimates all landed in between — so both speed and ratio columns moved by design). Raw tables + per-section commands: [matrix.md](matrix.md), including the new full 1-22 numeric ladder (T7). Comparison target: zstd crate / libzstd 1.5.7 (zstdmt), rustc 1.100.0-nightly, AMD Zen4-class 32C (Eng Sample 100-000000870-32_Y). Ladder pairs numeric levels (fastest/fast/balanced/best/opt/ultra vs libzstd 1/3/9/13/17/19). Caveats: ±10% noise between runs; per-side budget 1s (500ms on the full ladder); interleaved medians, spreads in the raw tables.

## Headline

- **Decode ST bulk**: we win 10/11 cells (x 0.23-0.85), parity on skewed.zst9 (1.02); absolute 652-11476 MiB/s ours. The honest decode gap remains the streaming column (zstd's bulk API is a slow wrapper). Unchanged from 09-16 — the ST decode core had no commits.
- **Decode ST streaming** (core-vs-core): zstd wins every compressible shape — json x1.27-1.37, skewed x1.10-1.32, text x1.09-1.24; we win random (x0.81) and hold zeros (1.00). Unchanged (the fused sequence loop, serial-chain limit).
- **Encode ST**: json fastest/fast x1.43/1.03, **balanced ahead at x0.85 carrying +21% density**, best/opt/ultra x1.45/1.12/1.10 at ratio parity or denser (json.opt 9 B ahead of zstd-17). text: fastest/fast x0.74/0.52 (we win); **best/opt/ultra ahead x0.92/0.60/0.69, and text.best now denser too (386.39 vs 385.90 — the 09-16 −0.23% residue closed by the cold-head probe step)**; balanced x1.51 — down from x3.47, the cold-start DUBT head's per-frame cost cut by the entropy-build rewrite, still +1.65% denser than zstd-9. skewed: low tiers ahead or parity (best x2.15, opt/ultra x1.00/1.07 — opt crossed from 1.05). random/zeros: we win at every tier (up to 300× on incompressible high tiers); the 09-16 zeros.balanced/best fixed-cost regression is fully recovered (x0.053/0.020, was 0.108/0.098) and random.balanced flipped back ahead (x0.70, was 1.31).
- **Encode MT**: json.fastest.mt8 x1.16 the one remaining clean zstd mt win (noisy 0.84-1.67; mt16 ours x0.35); every other cell ahead except text.balanced x1.51-1.54 — halved from x2.81-2.85 and no longer width-independent (826 MiB/s at mt8 vs 444). Our cold-pool mt16 json.fast (3376) still beats zstd's warm-pool reference (1593).
- **Streaming encode ST**: json.fast ahead (x0.93); json.fastest x1.29; best-tier bounded by the core (json.best x1.46, text.best x1.40).
- **Streaming encode MT8**: all json cells ahead (x0.56/0.18/0.34/0.70/0.41/0.51); text fastest/fast ahead (x0.54/0.26), **best/opt/ultra now all ahead (x0.67/0.61/0.68 — were 1.17/0.99/1.03)** on the worker-pool + shared finish-tail fill; text.balanced x1.48 (was 3.02), the last stream-mt cell behind.
- **Decode MT** (our exclusive dimension, libzstd has none): json.zst3 mt16 = **1.60× our ST = 1.21× the zstd stream reference** (was 1.54×/1.16×); skewed 1.04-1.18× ST but below the zstd ref; text flat ~0.98×; random 0.98× ST = 1.07× ref. Positive asset on json, neutral elsewhere.
- **Ratio sweep**: geo-mean **+8.67%** denser over 120 cells (bulk-st +1.42%). text.best flipped to +0.12..+0.20% (was −0.15..−0.23%); remaining losing cells: json.fast mt −0.12..−0.14% (new, watch item), skewed.opt −0.06..−0.07%, plus −0.00..−0.04% near-ties on skewed.fast/fastest/ultra and text.ultra mt.
- **Full 1-22 ladder** (release gate, both sides at the same numeric level): every shape wins or ties at the ladder ends (1-4, 19-22 mostly) and on random/zeros throughout; the structural holes are json levels 5-12 (x1.17-2.84, the chain-family rows; our level-9 row beats them on both axes) and skewed 13-16 speed (x1.27-2.41 at ratio parity). Full per-level tables in [matrix.md](matrix.md) T7.

## Decode ST (MiB/s of raw; x = ours_time/zstd_time, <1 = we faster)

| file | bulk ours | bulk zstd | bulk x | stream ours | stream zstd | stream x |
|---|---:|---:|---:|---:|---:|---:|
| json.zst1 | 1741 | 1289 | 0.74 | 1728 | 2193 | 1.27 |
| json.zst3 | 1442 | 1173 | 0.82 | 1373 | 1875 | 1.37 |
| json.zst9 | 1737 | 1269 | 0.73 | 1627 | 2149 | 1.32 |
| text.zst1 | 5538 | 2275 | 0.41 | 6073 | 7543 | 1.24 |
| text.zst3 | 8455 | 2536 | 0.30 | 10072 | 11115 | 1.10 |
| text.zst9 | 9289 | 2572 | 0.28 | 11150 | 12110 | 1.09 |
| skewed.zst1 | 2309 | 1547 | 0.67 | 2581 | 2838 | 1.10 |
| skewed.zst3 | 1251 | 1059 | 0.85 | 1266 | 1534 | 1.21 |
| skewed.zst9 | 652 | 663 | 1.02 | 603 | 794 | 1.32 |
| random.zst3 | 8903 | 2335 | 0.27 | 11082 | 8943 | 0.81 |
| zeros.zst3 | 11476 | 2643 | 0.23 | 12876 | 12921 | 1.00 |

## Encode ST bulk (checksums off; x = ours_time/zstd_time; pairs 1/3/9/13/17/19)

| level | shape | ours MiB/s | ours ratio | zstd MiB/s | zstd ratio | x |
|---|---|---:|---:|---:|---:|---:|
| fastest | json | 599 | 6.39 | 854 | 6.11 | 1.43 |
| fastest | text | 14114 | 309.22 | 10439 | 308.94 | 0.74 |
| fastest | skewed | 2944 | 2.00 | 1275 | 2.00 | 0.43 |
| fastest | random | 2548 | 1.00 | 2194 | 1.00 | 0.86 |
| fastest | zeros | 49722 | 32483 | 13285 | 32171 | 0.27 |
| fast | json | 458 | 5.30 | 474 | 5.29 | 1.03 |
| fast | text | 14068 | 333.02 | 7280 | 332.90 | 0.52 |
| fast | skewed | 213 | 1.92 | 227 | 1.92 | 1.07 |
| fast | random | 2539 | 1.00 | 2031 | 1.00 | 0.80 |
| fast | zeros | 49339 | 32483 | 8694 | 32171 | 0.18 |
| balanced | json | 142 | 7.20 | 121 | 5.95 | 0.85 |
| balanced | text | 1093 | 384.64 | 1649 | 378.41 | 1.51 |
| balanced | skewed | 1879 | 2.00 | 74 | 1.84 | 0.039 |
| balanced | random | 2306 | 1.00 | 1626 | 1.00 | 0.70 |
| balanced | zeros | 32802 | 32483 | 1751 | 32202 | 0.053 |
| best | json | 25 | 6.26 | 36 | 6.10 | 1.45 |
| best | text | 807 | 386.39 | 737 | 385.90 | 0.92 |
| best | skewed | 6 | 1.84 | 13 | 1.84 | 2.15 |
| best | random | 2333 | 1.00 | 251 | 1.00 | 0.11 |
| best | zeros | 42189 | 32483 | 833 | 32202 | 0.020 |
| opt | json | 6 | 7.49 | 7 | 7.49 | 1.12 |
| opt | text | 772 | 410.18 | 460 | 410.11 | 0.60 |
| opt | skewed | 3 | 2.00 | 3 | 2.00 | 1.00 |
| opt | random | 2377 | 1.00 | 11 | 1.00 | 0.005 |
| opt | zeros | 40742 | 32483 | 939 | 32202 | 0.023 |
| ultra | json | 3 | 7.42 | 3 | 7.42 | 1.10 |
| ultra | text | 395 | 414.04 | 273 | 413.98 | 0.69 |
| ultra | skewed | 2 | 2.00 | 2 | 2.00 | 1.07 |
| ultra | random | 2392 | 1.00 | 8 | 1.00 | 0.003 |
| ultra | zeros | 39910 | 32483 | 677 | 32202 | 0.017 |

Ratio verdict: denser or at parity on json and text at every level (json.opt 9 B ahead; text.best flipped denser); skewed wins everywhere except near-ties; random/zeros tie or win. Checksum on/off (ours): json.fast 1.03, text.fast 0.85.

## Encode MT bulk (1 pass; cold pool per call both sides; MiB/s)

| cell | ours mt8 | zstd mt8 | x8 | ours mt16 | zstd mt16 | x16 | ratio ours mt16 | ratio zstd mt16 |
|---|---:|---:|---:|---:|---:|---:|---:|---:|
| json.fastest | 3742 | 4284 | 1.16 | 5333 | 1838 | 0.35 | 6.34 | 6.11 |
| json.fast | 2611 | 1062 | 0.41 | 3376 | 1047 | 0.31 | 5.30 | 5.31 |
| json.balanced | 321 | 205 | 0.64 | 328 | 200 | 0.62 | 7.19 | 5.95 |
| text.fastest | 22009 | 11074 | 0.50 | 19879 | 2367 | 0.12 | 309.08 | 39.78 |
| text.fast | 12261 | 2310 | 0.19 | 11452 | 2273 | 0.20 | 332.94 | 189.16 |
| text.balanced | 826 | 1276 | 1.54 | 812 | 1260 | 1.51 | 384.59 | 378.30 |
| skewed.fastest | 4271 | 3296 | 0.78 | 3704 | 1602 | 0.44 | 2.00 | 2.00 |
| skewed.fast | 1315 | 654 | 0.50 | 1685 | 655 | 0.39 | 1.92 | 1.92 |
| skewed.balanced | 710 | 123 | 0.17 | 704 | 122 | 0.18 | 2.00 | 1.84 |

## Streaming encode (64KiB pulls; json/text; interleaved medians)

| cell | ST ours | ST zstd | ST x | MT8 ours | MT8 zstd | MT8 x |
|---|---:|---:|---:|---:|---:|---:|
| json.fastest | 600 | 771 | 1.29 | 3419 | 1932 | 0.56 |
| json.fast | 473 | 440 | 0.93 | 2285 | 405 | 0.18 |
| json.balanced | — | — | — | 330 | 114 | 0.34 |
| json.best | 24 | 35 | 1.46 | 74 | 52 | 0.70 |
| json.opt | — | — | — | 16 | 7 | 0.41 |
| json.ultra | — | — | — | 6 | 3 | 0.51 |
| text.fastest | 7521 | 1763 | 0.23 | 7725 | 4126 | 0.54 |
| text.fast | 7303 | 5441 | 0.75 | 6322 | 1637 | 0.26 |
| text.balanced | — | — | — | 659 | 974 | 1.48 |
| text.best | 490 | 685 | 1.40 | 548 | 368 | 0.67 |
| text.opt | — | — | — | 645 | 387 | 0.61 |
| text.ultra | — | — | — | 357 | 244 | 0.68 |

Unknown-size text.fastest streaming: zstd emits 1.84MB (ratio 18.3) vs our 108KB (ratio 309). Stream/ceiling ratios in [matrix.md](matrix.md) T5 — text.opt/ultra stream-mt8 now beat the bulk-mt8 ceiling refs (the shared finish-tail fill is streaming-only).

## Decode MT scaling (1 pass; our solo dimension; MiB/s)

| file | ours ST | mt2 | mt4 | mt8 | mt16 | zstd ST stream ref |
|---|---:|---:|---:|---:|---:|---:|
| json.zst3 | 1413 | 1621 | 2066 | 2223 | 2262 | 1876 |
| text.zst3 | 10603 | 10516 | 10331 | 10474 | 10429 | 11181 |
| skewed.zst9 | 649 | 755 | 765 | 749 | 678 | 793 |
| random.zst3 | 9742 | 9573 | 9589 | 9571 | 9584 | 8923 |

## Compression-ratio sweep (`zstdx-bench ratio`, 2026-09-18)

Geo-mean Δ over 120 cells **+8.67%**; per mode bulk-st +1.42% / bulk-mt +11.08% / stream-st +11.43% / stream-mt +11.11%; per shape json +4.41%, text +40.36%, skewed +1.41%, random ±0, zeros +1.99%. Losing cells (complete): json.fast mt −0.12..−0.14%, text.ultra mt −0.04%, skewed.opt −0.06..−0.07%, skewed.ultra −0.04%, skewed.fast mt −0.01..−0.03%, skewed.fastest −0.00%. text.best flipped to +0.12..+0.20%. Details in [matrix.md](matrix.md) T6.

## Top open deficits (from this run, x = ours/zstd wall time)

1. **text.balanced speed x1.51 ST / x1.51-1.54 mt / x1.48 stream-mt8** — halved from x3.47/2.85/3.02 by the entropy-build rewrite; the residual is the cold-start DUBT head's remaining per-frame cost; ratio +1.65% denser than zstd-9 (todo 9).
2. **Streaming decode on compressible shapes**: json x1.27-1.37, skewed x1.10-1.32, text x1.09-1.24 — core-vs-core, the fused loop vs libzstd's pipeline (todo 2). Structural levers falsified; only uop attrition remains.
3. **json.fastest x1.43 ST / x1.16 mt8** — the steady scan loop's inherent branch-mispredict budget; realistic ceiling ~x1.3-1.4 (todo 3). json.fast x1.03 is near closed.
4. **Best-tier core**: json x1.45, skewed x2.15 (memory-latency tree walk) (todo 6). Caps the two stream ST cells (json/text.best x1.46/1.40).
5. **json opt/ultra x1.10-1.12, skewed.ultra x1.07** — per-node codegen + event-volume residue at ratio parity or denser (todo 6); skewed.opt crossed to x1.00.
6. **json chain rows 5-12** (full-ladder T7): x1.17-2.84, the weakest speed band — the deep-chain rows lose to libzstd's chain on both axes while our own level-9 row beats them; a ladder-tuning question, not a matcher-core one.
7. **MT decode**: text flat ~0.98×, skewed below the zstd stream ref — decoder-side piece-parallel stage B to spend the ramp guarantee (todo 1's open half).
8. **dll100 large-binary ST** (09-16 doc baselines, todo 8): fastest x~1.44-1.55, fast x1.16, balanced x~1.55 — matcher-side instruction counts; emit live-set shrink the one untried lever.
9. **Small band 64K-1M** fastest x~1.36-1.49 (this run: json-64K/1M x1.42/1.36, text x1.39/1.49; level 3 at parity 0.99-1.14; level 9 ahead 0.58-0.91 except the 1M cells x1.04-1.07) (todo 7).
10. **Ratio residues**: json.fast mt −0.12..−0.14% (new this run), skewed.opt −0.07% (near-tie), dictionary small-payload +3% parse-side (todo 10).
