# Perf vs zstd crate · current snapshot (2026-09-14)

> **Addendum 2026-09-14 (post-pass, `0d45b5c`)**: the Best tier moved to libzstd's DUBT finder after this pass — json.best x1.627 (was x2.16) at ratio 6.25, text.best x1.184 (was x1.60) at −0.45% vs zstd-13, skewed x2.87 (was x3.71); json.best stream ST x1.69 (was x4.05), stream-mt8 **x0.93 — ahead of zstd** (was x3.31), text.best stream x1.79 (was x2.60). The 120-cell ratio sweep re-ran clean: geo-mean +8.27% (was +8.23%), best cells +0.62-0.77% denser than zstd. Full re-pass pending the next snapshot day.

> Fresh full pass at `ad57445` (one day, one machine, one corpus, same flags as the 09-12 pass; the six intervening encoder-perf commits are all byte-identical-output, so every ratio column reproduces exactly and only speed moved). Raw tables + per-section commands: [matrix.md](matrix.md). Comparison target: zstd crate / libzstd 1.5.7 (zstdmt), rustc 1.100.0-nightly, AMD Zen4-class 32C (Eng Sample 100-000000870-32_Y). Ladder pairs numeric levels (fastest/fast/balanced/best/opt/ultra vs libzstd 1/3/9/13/17/19). Caveats: ±10% noise between runs; per-side budget 1.5-2s; dec-st/enc-st/enc-stream double-passed, enc-mt/dec-mt single-pass.

## Headline

- **Decode ST bulk**: we win 10/11 cells (x 0.21-0.83), parity on skewed.zst9 (1.03); absolute 635-11591 MiB/s ours. No decoder commits since 09-12 — column reproduces it. Caveat kept: zstd's bulk API is a slow wrapper; the honest decode gap is the streaming column.
- **Decode ST streaming** (core-vs-core): zstd wins every compressible shape — json x1.27-1.35, skewed x1.10-1.32, text x1.04-1.21; we win random (x0.81) and hold zeros (1.00). Our own stream-vs-bulk overhead is ≤2%; the residue is the fused sequence loop itself (serial-chain limit).
- **Encode ST**: the perf lands moved the low tiers — json fastest/fast/balanced x1.57/1.11/1.34 (was 1.67/1.21/1.51), text fastest/fast/balanced x0.79/0.55/0.59 (was 0.83/0.60/0.65), skewed fast x1.09 (was 1.22). We win text and skewed at those tiers, zeros and random at every tier (up to 300x on incompressible high tiers). We still lose json at every level; json.balanced is a deliberate density trade (+19.5% denser than zstd-9 at x1.34). best/opt/ultra speed unchanged: best x1.77-6.2 (largest wall gap), opt/ultra x1.04-1.49.
- **Encode MT**: wins where it matters — json.fast mt8/mt16 x0.42/0.32, text.fast x0.15-0.16, text.fastest mt16 x0.13, skewed 0.15-0.76. Losses: json.fastest.mt8 x1.27 (zstd-l1-mt8, the one clean zstd mt win; mt16 ours x0.39), json/text.balanced.mt16 x0.75/0.63 — but zstd-mt pays 2-9x ratio there where we preserve ST ratio within ±0.5%. Our cold-pool mt16 (3340) beats zstd's warm-pool reference (1580-1615).
- **Streaming encode ST**: text wins (fastest x0.25 with a 17x ratio win, fast x0.80); json.fast now at parity (x1.00, was 1.11); json.fastest x1.44; best-tier bounded by the core (json.best x4.05, text.best x2.60).
- **Streaming encode MT8** (post 09-14 pool/overlap/buffer-pool commits): json.fastest x0.72, json.fast x0.22, text.fastest x0.75, text.fast x0.39 — the fastest/fast tier is fully ahead of zstd now (was x1.22/x0.31/x1.90/x0.78); the between-pass bimodality is diagnosed (THP fallback under interleaved allocation) and fixed by pooled accumulate buffers (see mt-stream). The best-tier core and the balanced tier are unchanged.
- **Decode MT** (our exclusive dimension, libzstd has none): still a negative asset — no scaling (text/random ~1.00x ST), skewed.zst9 anti-scales to 0.71x ST; best case json.zst3 mt16 = 1.11x ST, 0.83x of the zstd ST streaming reference.
- **Ratio sweep**: identical to 09-12 by construction (+9.02% geo-mean over 120 cells; bulk-st +1.73%). Only sizable deficit: text.Balanced −2.78%.

## Decode ST (2 passes; MiB/s of raw; x = ours_time/zstd_time, <1 = we faster)

| file | bulk ours | bulk zstd | bulk x | stream ours | stream zstd | stream x |
|---|---:|---:|---:|---:|---:|---:|
| json.zst1 | 1742 | 1290 | 0.74 | 1723 | 2185 | 1.27 |
| json.zst3 | 1417 | 1167 | 0.83 | 1385 | 1872 | 1.35 |
| json.zst9 | 1719 | 1249 | 0.73 | 1649 | 2136 | 1.30 |
| text.zst1 | 5540 | 2270 | 0.41 | 6231 | 7541 | 1.21 |
| text.zst3 | 8564 | 2522 | 0.30 | 10413 | 11084 | 1.06 |
| text.zst9 | 9342 | 2582 | 0.28 | 11590 | 12098 | 1.04 |
| skewed.zst1 | 2306 | 1544 | 0.67 | 2583 | 2841 | 1.10 |
| skewed.zst3 | 1243 | 1057 | 0.84 | 1257 | 1533 | 1.22 |
| skewed.zst9 | 635 | 658 | 1.03 | 602 | 793 | 1.32 |
| random.zst3 | 8899 | 2335 | 0.25 | 11108 | 8872 | 0.81 |
| zeros.zst3 | 11591 | 2651 | 0.21 | 12847 | 12888 | 1.00 |

Versus 09-12: every cell within ±3% (documented noise; no decoder commits intervened).

## Encode ST bulk (2 passes; checksums off; x = ours_time/zstd_time; pairs 1/3/9/13/17/19)

| level | shape | ours MiB/s | ours ratio | zstd MiB/s | zstd ratio | x |
|---|---|---:|---:|---:|---:|---:|
| fastest | json | 548 | 6.14 | 859 | 6.11 | 1.57 |
| fastest | text | 13225 | 309.17 | 10502 | 308.94 | 0.79 |
| fastest | skewed | 2877 | 2.00 | 1259 | 2.00 | 0.44 |
| fastest | random | 2335 | 1.00 | 2092 | 1.00 | 0.90 |
| fastest | zeros | 50333 | 32483 | 13309 | 32171 | 0.26 |
| fast | json | 431 | 5.29 | 478 | 5.29 | 1.11 |
| fast | text | 13295 | 332.97 | 7271 | 332.90 | 0.55 |
| fast | skewed | 209 | 1.92 | 229 | 1.92 | 1.09 |
| fast | random | 2328 | 1.00 | 1892 | 1.00 | 0.81 |
| fast | zeros | 50045 | 32483 | 8701 | 32171 | 0.17 |
| balanced | json | 90 | 7.11 | 121 | 5.95 | 1.34 |
| balanced | text | 2890 | 367.89 | 1700 | 378.41 | 0.59 |
| balanced | skewed | 2041 | 2.00 | 75 | 1.84 | 0.037 |
| balanced | random | 2317 | 1.00 | 1529 | 1.00 | 0.66 |
| balanced | zeros | 46548 | 32483 | 1754 | 32202 | 0.038 |
| best | json | 9 | 6.75 | 37 | 6.10 | 4.22 |
| best | text | 416 | 404.78 | 733 | 385.90 | 1.77 |
| best | skewed | 2 | 2.00 | 14 | 1.84 | 6.22 |
| best | random | 2323 | 1.00 | 253 | 1.00 | 0.11 |
| best | zeros | 43534 | 32483 | 831 | 32202 | 0.019 |
| opt | json | 5 | 7.46 | 7 | 7.49 | 1.30 |
| opt | text | 451 | 408.76 | 469 | 410.11 | 1.04 |
| opt | skewed | 2 | 2.00 | 3 | 2.00 | 1.49 |
| opt | random | 2329 | 1.00 | 12 | 1.00 | 0.005 |
| opt | zeros | 41904 | 32483 | 936 | 32202 | 0.023 |
| ultra | json | 2 | 7.42 | 3 | 7.42 | 1.32 |
| ultra | text | 265 | 412.58 | 274 | 413.98 | 1.05 |
| ultra | skewed | 1 | 2.00 | 2 | 2.00 | 1.40 |
| ultra | random | 2341 | 1.00 | 8 | 1.00 | 0.003 |
| ultra | zeros | 40703 | 32483 | 673 | 32202 | 0.017 |

Ratio verdict (deterministic, same as 09-12): denser or at parity on json at every level except opt (−0.31% vs zstd-17) and ultra (parity); text loses balanced (−2.78%) and opt/ultra (−0.3%); skewed wins everywhere except near-ties; random/zeros tie or win. Checksum on/off (ours): json.fast 1.01, text.fast 0.86 (on faster).

## Encode MT bulk (1 pass; cold pool per call both sides; MiB/s)

| cell | ours mt8 | zstd mt8 | x8 | ours mt16 | zstd mt16 | x16 | ratio ours mt16 | ratio zstd mt16 |
|---|---:|---:|---:|---:|---:|---:|---:|---:|
| json.fastest | 3403 | 4326 | 1.27 | 4681 | 1838 | 0.39 | 6.13 | 6.11 |
| json.fast | 2551 | 1066 | 0.42 | 3340 | 1057 | 0.32 | 5.30 | 5.31 |
| json.balanced | 294 | 206 | 0.71 | 271 | 202 | 0.75 | 7.09 | 5.95 |
| text.fastest | 19712 | 11017 | 0.56 | 17297 | 2270 | 0.13 | 308.54 | 39.78 |
| text.fast | 15034 | 2244 | 0.15 | 13874 | 2160 | 0.16 | 332.47 | 189.16 |
| text.balanced | 1937 | 1254 | 0.64 | 1995 | 1244 | 0.63 | 367.74 | 378.30 |
| skewed.fastest | 4298 | 3269 | 0.76 | 3598 | 1496 | 0.42 | 2.00 | 2.00 |
| skewed.fast | 1280 | 647 | 0.51 | 1634 | 642 | 0.39 | 1.92 | 1.92 |
| skewed.balanced | 807 | 123 | 0.15 | 802 | 124 | 0.16 | 2.00 | 1.84 |

## Streaming encode (64KiB pulls; json/text only; ST = 2-pass medians)

| cell | ST ours | ST zstd | ST x | MT8 ours | MT8 zstd | MT8 x |
|---|---:|---:|---:|---:|---:|---:|
| json.fastest | 540 | 777 | 1.44 | 1479 | 1800 | 1.22 |
| json.fast | 444 | 443 | 1.00 | 1148 | 403 | 0.35 |
| json.balanced | — | — | — | 188 | 112 | 0.60 |
| json.best | 9 | 35 | 4.05 | 15 | 52 | 3.31 |
| text.fastest | 6994 | 1773 | 0.25 | 2287 | 4317 | 1.90 |
| text.fast | 6888 | 5493 | 0.80 | 1728-2200 | 1714 | 0.79-0.97 |
| text.balanced | — | — | — | 485-1485 | 971 | 0.66-0.81 |
| text.best | 266 | 691 | 2.60 | 161-191 | 368 | 1.90-2.28 |

Unknown-size text.fastest streaming: zstd emits 1.84MB (ratio 18.3) vs our 108KB (ratio 309). Bulk-mt8 ceilings and stream/ceiling ratios in [matrix.md](matrix.md).

## Decode MT scaling (1 pass; our solo dimension; MiB/s)

| file | ours ST | mt2 | mt4 | mt8 | mt16 | zstd ST stream ref |
|---|---:|---:|---:|---:|---:|---:|
| json.zst3 | 1402 | 1254 | 1439 | 1544 | 1551 | 1877 |
| text.zst3 | 10633 | 10684 | 10709 | 10703 | 10672 | 11179 |
| skewed.zst9 | 635 | 450 | 456 | 452 | 456 | 794 |
| random.zst3 | 9788 | 9574 | 9583 | 9610 | 9596 | 9035 |

## Compression-ratio sweep (`zstdx-bench ratio`, 2026-09-14)

Identical cell-for-cell to the 09-12 sweep (byte-identical-output commits): geo-mean Δ over 120 cells **+9.02%**; per mode bulk-st +1.73% / bulk-mt +11.42% / stream-st +11.81% / stream-mt +11.46%; per shape json +4.78%, text +40.22%, skewed +2.75%, random ±0, zeros +1.99%. Complete losing-cell list in [matrix.md](matrix.md) T6; the only sizable one is text.Balanced −2.73..−2.79% across all modes.

## Top open deficits (from this run, x = ours/zstd wall time)

1. **Best-tier encoder core (L13)**: json x4.22, skewed x6.22, text x1.77 — the largest single wall-clock gap, and it caps the streaming columns too (json.best stream ST x4.05 / stream-mt8 x3.31, text.best x2.60/x1.9-2.3). Per-block DP cost over the candidate space (todo 6).
2. **json ST encode speed**: fastest x1.57 (largest low-tier gap), fast x1.11, balanced x1.34 (carries +19.5% density), opt/ultra x1.30-1.32 at ratio parity or denser (todos 4-7).
3. **Streaming decode on compressible shapes**: json x1.27-1.35, skewed x1.10-1.32, text x1.04-1.21 — core-vs-core (our stream ≈ our bulk; the fused loop vs libzstd's pipeline) (todo 2).
4. ~~Streaming MT burst-model serialization~~ resolved 09-14: persistent pool + overlapped accumulation + pooled buffers put all fastest/fast stream-mt8 cells ahead of zstd (text.fastest x0.75, json.fastest x0.72); residues move to strip-prefill amortization and job sizing (todo 12).
5. **MT decode**: no scaling, skewed anti-scales 0.71x ST (todo 1, stage-B parallelization — leapfrog dimension).
6. **json.fastest.mt8**: x1.27 (the one clean zstd mt win; we win mt16 at x0.39).
7. **random fastest/fast ST**: x0.90/0.81 minor residue (raw-block per-block overhead).
8. **Ratio residues** (only cells we lose): text.Balanced −2.78%, text opt/ultra −0.3%, json.opt −0.3% (todo 9).
