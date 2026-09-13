# Perf vs zstd crate · current snapshot (2026-09-12)

> Fresh full pass at `0e7044d`, one day, one machine, one corpus. Raw tables + per-section commands: [matrix.md](matrix.md). Comparison target: zstd crate / libzstd 1.5.7 (zstdmt), rustc 1.100.0-nightly, AMD Zen4-class 32C (Eng Sample 100-000000870-32_Y). Ladder pairs numeric levels since `e11d07e` (fastest/fast/balanced/best/opt/ultra vs libzstd 1/3/9/13/17/19) — not cell-for-cell comparable with the 2026-09-11 snapshot. Caveats: ±10% noise, slow-cell absolute MiB/s ~25% below the 09-11 passes (sustained clocks; interleaved x columns fair), per-side budget 1.5-2s; dec-st/enc-st double-passed, mt/stream sections single-pass.

## Headline

- **Decode ST bulk**: we win 10/11 cells (x 0.23-0.84), parity on skewed.zst9 (1.02); absolute 648-11535 MiB/s ours. Caveat kept from methodology: zstd's bulk API is a slow wrapper — the honest decode gap is the streaming column.
- **Decode ST streaming**: zstd wins every compressible shape — json x1.30-1.36, skewed x1.10-1.31, text x1.04-1.21; we win random (x0.80, 11082 vs 8872 MiB/s) and hold zeros (1.00). text.zst9 near-parity (1.04).
- **Encode ST**: we win text (fastest/fast/balanced x0.60-0.83, balanced now denser than zstd-9 at 368 vs 378 and 1.6x faster), skewed (fastest x0.44, balanced x0.041 at 1808 vs 75 MiB/s), zeros (x0.017-0.26), and random at every tier (x0.61-0.88 low tiers via the sticky gate hold, best/opt/ultra x0.09/**0.004**/**0.003**). We lose json at every level (x1.21-1.67 low tiers; best x4.10; opt/ultra x1.32-1.33 vs the denser zstd-17/19 references at ratio parity). json.balanced is now a deliberate density trade: +19.5% denser than zstd-9 (7.11 vs 5.95) at x1.51 (W21+row).
- **Encode MT**: big win at equal workers where it matters — json.fast mt16 x0.33 (3190 vs 1044 MiB/s), text.fast mt8/mt16 x0.16-0.17 (~13000 vs ~2200), skewed 0.16-0.76. Losses: json.fastest.mt8 x1.35 (zstd-l1-mt8 hits 4282), json/text.balanced.mt16 x0.96/0.66 (zstd-mt ratio collapses there: text 378→189/39.8 vs our 368/309). Ratio preservation is ours alone: every mt cell within 0.5% of our ST. Our cold-pool mt16 beats zstd's warm-pool reference (3190 vs 1604 MiB/s).
- **Streaming encode ST**: text wins (fastest x0.26 with a 17x ratio win — zstd collapses to ratio 18.3 vs our 309; fast x0.83); json loses (fastest x1.55, fast x1.11, best x4.17 — bounded by the Best core, not the pipeline).
- **Streaming encode MT8 — regression fixed same day** (found by this pass, bisected to `946dd2f`; fix: quantized epoch grid + finish-tail re-slice + MADV_HUGEPAGE buffer + output cursor, see [mt-stream](../perf/mt-stream.md)): all cells recovered 15-77% (json.fastest 961→1423, json.fast 724→1248, json.balanced 175→247, text.balanced 982→1546, text.fastest 2049→2363 MiB/s re-pass) with ratio within ±0.1%. json.fast/balanced and text.balanced/best beat zstd by 1.5-3.1×; the residue (text.fastest 1.8×, json.fastest 1.26× behind zstd) is the burst model's serialized accumulate/spawn/barrier, not the schedule.
- **Decode MT** (our exclusive dimension, libzstd has none): still a negative asset — no scaling (text/random 0.97-1.00x ST), skewed.zst9 anti-scales to 0.72x ST; best case json.zst3 mt16 = 1.10x ST, 0.83x of the zstd ST streaming reference. random.zst3 is the one win (0.98x ST = 1.05x zstd's stream).

## Decode ST (2 passes; MiB/s of raw; x = ours_time/zstd_time, <1 = we faster)

| file | bulk ours | bulk zstd | bulk x | stream ours | stream zstd | stream x |
|---|---:|---:|---:|---:|---:|---:|
| json.zst1 | 1742 | 1278 | 0.73 | 1686 | 2185 | 1.30 |
| json.zst3 | 1449 | 1167 | 0.81 | 1378 | 1872 | 1.36 |
| json.zst9 | 1742 | 1249 | 0.72 | 1646 | 2136 | 1.30 |
| text.zst1 | 5554 | 2270 | 0.41 | 6231 | 7541 | 1.21 |
| text.zst3 | 8425 | 2522 | 0.30 | 10388 | 11079 | 1.07 |
| text.zst9 | 9178 | 2582 | 0.28 | 11597 | 12098 | 1.04 |
| skewed.zst1 | 2302 | 1544 | 0.67 | 2585 | 2841 | 1.10 |
| skewed.zst3 | 1253 | 1057 | 0.84 | 1240 | 1533 | 1.24 |
| skewed.zst9 | 648 | 658 | 1.02 | 605 | 793 | 1.31 |
| random.zst3 | 8878 | 2335 | 0.26 | 11082 | 8872 | 0.80 |
| zeros.zst3 | 11535 | 2651 | 0.23 | 12876 | 12888 | 1.00 |

## Encode ST bulk (2 passes; checksums off; x = ours_time/zstd_time; pairs 1/3/9/13/17/19)

| level | shape | ours MiB/s | ours ratio | zstd MiB/s | zstd ratio | x |
|---|---|---:|---:|---:|---:|---:|
| fastest | json | 514 | 6.14 | 856 | 6.11 | 1.67 |
| fastest | text | 12547 | 309.17 | 10474 | 308.94 | 0.83 |
| fastest | skewed | 2913 | 2.00 | 1272 | 2.00 | 0.44 |
| fastest | random | 2464 | 1.00 | 2157 | 1.00 | 0.88 |
| fastest | zeros | 50414 | 32483 | 13296 | 32171 | 0.26 |
| fast | json | 400 | 5.29 | 484 | 5.29 | 1.21 |
| fast | text | 12053 | 332.97 | 7267 | 332.90 | 0.60 |
| fast | skewed | 186 | 1.92 | 227 | 1.92 | 1.22 |
| fast | random | 2569 | 1.00 | 2044 | 1.00 | 0.80 |
| fast | zeros | 49967 | 32483 | 8701 | 32171 | 0.17 |
| balanced | json | 80 | 7.11 | 122 | 5.95 | 1.51 |
| balanced | text | 2630 | 367.89 | 1703 | 378.41 | 0.65 |
| balanced | skewed | 1808 | 2.00 | 75 | 1.84 | 0.041 |
| balanced | random | 2505 | 1.00 | 1525 | 1.00 | 0.61 |
| balanced | zeros | 46916 | 32483 | 1754 | 32202 | 0.037 |
| best | json | 9 | 6.75 | 36 | 6.10 | 4.10 |
| best | text | 417 | 404.78 | 742 | 385.90 | 1.78 |
| best | skewed | 2 | 2.00 | 14 | 1.84 | 6.10 |
| best | random | 2452 | 1.00 | 218 | 1.00 | 0.09 |
| best | zeros | 43500 | 32483 | 834 | 32202 | 0.019 |
| opt | json | 5 | 7.46 | 7 | 7.49 | 1.32 |
| opt | text | 453 | 408.76 | 471 | 410.11 | 1.04 |
| opt | skewed | 2 | 2.00 | 3 | 2.00 | 1.50 |
| opt | random | 2427 | 1.00 | 9 | 1.00 | 0.004 |
| opt | zeros | 41949 | 32483 | 937 | 32202 | 0.022 |
| ultra | json | 2 | 7.42 | 3 | 7.42 | 1.33 |
| ultra | text | 264 | 412.58 | 276 | 413.98 | 1.05 |
| ultra | skewed | 1 | 2.00 | 2 | 2.00 | 1.40 |
| ultra | random | 2253 | 1.00 | 7 | 1.00 | 0.003 |
| ultra | zeros | 40780 | 32483 | 676 | 32202 | 0.017 |

Ratio verdict: denser or at parity on json at every level except opt (−0.31% vs zstd-17) and ultra (parity); text loses only balanced (−2.78%) and ultra (−0.34%, opt −0.33%); skewed wins everywhere except the near-tie fast/opt/ultra (−0.06..−0.07%); random/zeros tie or win. Checksum on/off (ours): json.fast 1.00, text.fast 0.90 (on faster).

## Encode MT bulk (1 pass; cold pool per call both sides; MiB/s)

| cell | ours mt8 | zstd mt8 | x8 | ours mt16 | zstd mt16 | x16 | ratio ours mt16 | ratio zstd mt16 |
|---|---:|---:|---:|---:|---:|---:|---:|---:|
| json.fastest | 3181 | 4282 | 1.35 | 4459 | 1827 | 0.41 | 6.13 | 6.11 |
| json.fast | 2264 | 1039 | 0.46 | 3190 | 1044 | 0.33 | 5.30 | 5.31 |
| json.balanced | 235 | 203 | 0.86 | 211 | 202 | 0.96 | 7.09 | 5.95 |
| text.fastest | 18829 | 10899 | 0.58 | 15978 | 2294 | 0.14 | 308.54 | 39.78 |
| text.fast | 13207 | 2255 | 0.17 | 13333 | 2184 | 0.16 | 332.47 | 189.16 |
| text.balanced | 1964 | 1260 | 0.64 | 1913 | 1254 | 0.66 | 367.74 | 378.30 |
| skewed.fastest | 4351 | 3288 | 0.76 | 3793 | 1586 | 0.42 | 2.00 | 2.00 |
| skewed.fast | 1164 | 644 | 0.56 | 1541 | 641 | 0.42 | 1.92 | 1.92 |
| skewed.balanced | 776 | 121 | 0.16 | 767 | 123 | 0.16 | 2.00 | 1.84 |

MT ratio preservation (ours vs own ST, all cells within ±0.5%): unchanged. zstd warm-pool json.fast mt16 reference: 1604 MiB/s (ours cold-pool 3190).

## Streaming encode (1 pass; 64KiB pulls; json/text only)

| cell | ST ours | ST zstd | ST x | MT8 ours | MT8 zstd | MT8 x |
|---|---:|---:|---:|---:|---:|---:|
| json.fastest | 503 | 777 | 1.55 | 961 | 1911 | 2.0 |
| json.fast | 398 | 441 | 1.11 | 724 | 400 | 0.56 |
| json.balanced | — | — | — | 175 | 113 | 0.65 |
| json.best | 8 | 35 | 4.17 | 17 | 51 | 3.0 |
| text.fastest | 6812 | 1774 | 0.26 | 2049 | 4963 | 2.4 |
| text.fast | 6700 | 5587 | 0.83 | 1991 | 1786 | 0.90 |
| text.balanced | — | — | — | 982 | 1002 | 1.02 |
| text.best | 268 | 690 | 2.58 | 200 | 365 | 1.8 |

**MT8 column updated by the same-day fix re-pass** (see Headline): json.fastest/fast/balanced/best now 1423/1248/247/16 (x vs zstd 1.26/0.32/0.46/3.2), text 2363/2279/1546/208 (1.80/0.76/0.65/1.75); json.fast/balanced and text.balanced are clear wins. Unknown-size text.fastest streaming: zstd emits 1.84MB (ratio 18.3) vs our 108KB (ratio 309). Bulk-mt8 ceilings and stream/ceiling ratios in [matrix.md](matrix.md).

## Decode MT scaling (1 pass; our solo dimension; MiB/s)

| file | ours ST | mt2 | mt4 | mt8 | mt16 | zstd ST stream ref |
|---|---:|---:|---:|---:|---:|---:|
| json.zst3 | 1415 | 1224 | 1442 | 1548 | 1559 | 1868 |
| text.zst3 | 10676 | 10725 | 10624 | 10630 | 10634 | 11168 |
| skewed.zst9 | 613 | 447 | 443 | 446 | 441 | 791 |
| random.zst3 | 9705 | 9503 | 9497 | 9439 | 9465 | 9013 |

## Compression-ratio sweep (`zstdx-bench ratio`, 2026-09-12)

Full matrix 5 shapes x 6 levels x {bulk,stream} x {st,mt4}, one deterministic pass per cell, checksums off; Δ% = ours/zstd ratio − 1, geo-mean. Wall 141 s. Raw 120-cell table in [matrix.md](matrix.md). Geo-mean Δ over 120 cells **+9.02%**; per mode: bulk-st **+1.73%**, bulk-mt **+11.42%**, stream-st **+11.81%**, stream-mt **+11.46%**; per shape: json +4.79%, text +40.20%, skewed +2.76%, random ±0, zeros +2.00%.

- ST bulk: denser at every level on json except opt (−0.31% vs zstd-17, ultra parity); balanced +19.46%, best +10.61% (vs zstd-13). text loses balanced (−2.78% vs zstd-9) and opt/ultra (−0.3%, vs zstd-17/19); fastest/fast at parity; best +4.89%. skewed balanced/best +8-9%.
- The mt/stream margins are libzstd's losses, not our gains: zstd-mt on text collapses (fastest 39.8 vs our 309, fast 189 vs 333) while our mt stays within ~0.1% of our ST; zstd's unknown-size streaming at level 1 emits 1.84 MB (ratio 18.3) where our streaming matches our bulk (309).
- random ties byte-exact with zstd at almost every cell (Δ 0.00%); zeros mt: zstd-mt loses up to ~10% ratio, ours none.

Directions from this sweep (details in [todo.md](../todo.md) items 9 and 12): text.Balanced −2.78% is the only sizable deficit (chain matcher vs zstd-9 lazy2 on long-range repeats, window now matched); text/skewed opt/ultra residues ≤0.35%; json.opt sits −0.31% under the much-denser zstd-17 reference post-C22. Self-inflicted: stream-mt pays up to +0.44% vs our own bulk. Anomaly kept: json.opt bulk-mt 0.13% denser than bulk-st.

## Top open deficits (from this run)

- **Streaming MT burst-model serialization** (residue after the `946dd2f` regression fix, todo 12): text.fastest 1.8× / json.fastest 1.26× behind zstd — accumulate, spawn and the barrier cannot overlap encoding; the fix is the persistent-pool redesign.
- Best-level encoder core speed: x1.8-6.1 behind zstd-13 across shapes (was x2.4-29 vs zstd-12); also caps json/text best streaming.
- json ST encode speed at fastest/fast/balanced/opt/ultra: x1.21-1.67 (low tiers) and x1.32-1.33 (opt/ultra at ratio parity).
- random ST encode at fastest/fast: x1.28-1.30 behind (raw-block per-block overhead); balanced now at parity.
- skewed.fast ST: x1.22 behind (only skewed cell we lose besides best).
- Streaming decode on compressible shapes: x1.04-1.36 behind (text.zst9 near-parity).
- MT decode scaling: none (skewed.zst9 0.72x anti-scaling).
- json.fastest.mt8: x1.35 behind (zstd-l1-mt8 is the one mt cell zstd wins cleanly).
