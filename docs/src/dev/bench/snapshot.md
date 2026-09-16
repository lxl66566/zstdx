# Perf vs zstd crate · current snapshot (2026-09-16)

> Fresh full pass at `dedd267d` (same day, one machine, one corpus, same flags; 96 commits after the 09-14 pass at `ad57445` — the DUBT/u32 Best-tier finder, the fill-lag clamp, the cold-start DUBT head, the dense-mode fast scan, the block splitter + exact-cost entropy merges, the arithmetic FSE tables, the stream-mt posted-job queue, and checksum verification on every decode path all landed in between, and `json.raw` was regenerated 09-15 — so both speed and ratio columns moved by design). Raw tables + per-section commands: [matrix.md](matrix.md). Comparison target: zstd crate / libzstd 1.5.7 (zstdmt), rustc 1.100.0-nightly, AMD Zen4-class 32C (Eng Sample 100-000000870-32_Y). Ladder pairs numeric levels (fastest/fast/balanced/best/opt/ultra vs libzstd 1/3/9/13/17/19). Caveats: ±10% noise between runs; per-side budget 1s; interleaved medians, spreads in the raw tables.

## Headline

- **Decode ST bulk**: we win 10/11 cells (x 0.23-0.84), parity on skewed.zst9 (1.02); absolute 635-11772 MiB/s ours. The honest decode gap remains the streaming column (zstd's bulk API is a slow wrapper).
- **Decode ST streaming** (core-vs-core): zstd wins every compressible shape — json x1.27-1.36, skewed x1.09-1.31, text x1.04-1.23; we win random (x0.80) and hold zeros (1.00). Unchanged from 09-14 (the stream core had no commits; the residue is the fused sequence loop, serial-chain limit).
- **Encode ST**: json fastest/fast x1.44/1.04; **balanced ahead at x0.76 carrying +21% density**; best/opt/ultra x1.43/1.10/1.12 at ratio parity or denser (json.opt 12 B ahead of zstd-17). text: fastest/fast x0.84/0.53 (we win); **best/opt/ultra ahead x0.95/0.61/0.69**; balanced x3.47 — the cold-start DUBT head's deliberate trade (ratio flipped −2.78%→+1.65% denser). skewed: low tiers ahead or parity (best x2.15, opt/ultra x1.05/1.07). random/zeros: we win at every tier (up to 300× on incompressible high tiers).
- **Encode MT**: json.fastest.mt8 x1.17 the one remaining clean zstd mt win (noisy 0.87-1.51; mt16 ours x0.35); every other cell ahead except text.balanced x2.81-2.85 (the DUBT head's serial per-frame cost, width-independent at 444 MiB/s). Our cold-pool mt16 (3202) still beats zstd's warm-pool reference (1593-1598).
- **Streaming encode ST**: json.fast crossed ahead (x0.94); json.fastest x1.30; best-tier bounded by the core (json.best x1.43, text.best x1.44 — both were x2.6-4.1 on 09-14).
- **Streaming encode MT8**: all json cells ahead (fastest/fast/balanced/best/opt/ultra x0.65/0.22/0.32/0.78/0.43/0.52); text fastest/fast ahead (x0.68/0.35), best/opt/ultra at parity (x1.17/0.99/1.03); text.balanced x3.02 (the DUBT head).
- **Decode MT** (our exclusive dimension, libzstd has none): json.zst3 now scales — mt16 = **1.54× our ST = 1.16× the zstd stream reference** (was 1.11×/0.83×); skewed recovered from 0.71× anti-scaling to 1.06-1.22× ST but stays below the zstd ref; text flat ~1.00×; random 0.98× ST = 1.06× ref. Positive asset on json, neutral elsewhere.
- **Ratio sweep**: geo-mean **+8.66%** denser over 120 cells (bulk-st +1.40%). Losing cells reduced to two residues: text.best −0.15..−0.23% and skewed.opt −0.06..−0.07%; text.balanced flipped to +1.65..+1.71%.

## Decode ST (MiB/s of raw; x = ours_time/zstd_time, <1 = we faster)

| file | bulk ours | bulk zstd | bulk x | stream ours | stream zstd | stream x |
|---|---:|---:|---:|---:|---:|---:|
| json.zst1 | 1737 | 1269 | 0.73 | 1729 | 2188 | 1.27 |
| json.zst3 | 1439 | 1123 | 0.78 | 1377 | 1876 | 1.36 |
| json.zst9 | 1730 | 1227 | 0.71 | 1646 | 2132 | 1.30 |
| text.zst1 | 5481 | 2261 | 0.41 | 6182 | 7572 | 1.23 |
| text.zst3 | 8320 | 2509 | 0.30 | 10396 | 11107 | 1.07 |
| text.zst9 | 9077 | 2566 | 0.28 | 11589 | 12089 | 1.04 |
| skewed.zst1 | 2317 | 1548 | 0.67 | 2563 | 2787 | 1.09 |
| skewed.zst3 | 1250 | 1053 | 0.84 | 1252 | 1537 | 1.23 |
| skewed.zst9 | 650 | 662 | 1.02 | 604 | 792 | 1.31 |
| random.zst3 | 8932 | 2351 | 0.26 | 11096 | 8893 | 0.80 |
| zeros.zst3 | 11772 | 2632 | 0.23 | 12866 | 12894 | 1.00 |

## Encode ST bulk (checksums off; x = ours_time/zstd_time; pairs 1/3/9/13/17/19)

| level | shape | ours MiB/s | ours ratio | zstd MiB/s | zstd ratio | x |
|---|---|---:|---:|---:|---:|---:|
| fastest | json | 596 | 6.39 | 855 | 6.11 | 1.44 |
| fastest | text | 12413 | 309.23 | 10449 | 308.94 | 0.84 |
| fastest | skewed | 2966 | 2.00 | 1272 | 2.00 | 0.43 |
| fastest | random | 2568 | 1.00 | 2312 | 1.00 | 0.90 |
| fastest | zeros | 50024 | 32483 | 13294 | 32171 | 0.27 |
| fast | json | 457 | 5.30 | 476 | 5.29 | 1.04 |
| fast | text | 13814 | 333.03 | 7259 | 332.90 | 0.53 |
| fast | skewed | 219 | 1.92 | 223 | 1.92 | 1.02 |
| fast | random | 2563 | 1.00 | 2144 | 1.00 | 0.84 |
| fast | zeros | 49631 | 32483 | 8713 | 32171 | 0.18 |
| balanced | json | 159 | 7.20 | 121 | 5.95 | 0.76 |
| balanced | text | 499 | 384.65 | 1730 | 378.41 | 3.47 |
| balanced | skewed | 1757 | 2.00 | 75 | 1.84 | 0.042 |
| balanced | random | 1220 | 1.00 | 1600 | 1.00 | 1.31 |
| balanced | zeros | 16080 | 32483 | 1744 | 32202 | 0.108 |
| best | json | 26 | 6.26 | 37 | 6.10 | 1.43 |
| best | text | 775 | 385.04 | 738 | 385.90 | 0.95 |
| best | skewed | 6 | 2.00 | 14 | 1.84 | 2.15 |
| best | random | 1901 | 1.00 | 265 | 1.00 | 0.14 |
| best | zeros | 8515 | 32483 | 831 | 32202 | 0.098 |
| opt | json | 6 | 7.49 | 7 | 7.49 | 1.10 |
| opt | text | 755 | 410.18 | 461 | 410.11 | 0.61 |
| opt | skewed | 3 | 2.00 | 3 | 2.00 | 1.05 |
| opt | random | 2341 | 1.00 | 12 | 1.00 | 0.005 |
| opt | zeros | 30874 | 32483 | 936 | 32202 | 0.030 |
| ultra | json | 3 | 7.42 | 3 | 7.42 | 1.12 |
| ultra | text | 396 | 414.05 | 274 | 413.98 | 0.69 |
| ultra | skewed | 2 | 2.00 | 2 | 2.00 | 1.07 |
| ultra | random | 2345 | 1.00 | 8 | 1.00 | 0.003 |
| ultra | zeros | 30057 | 32483 | 673 | 32202 | 0.022 |

Ratio verdict: denser or at parity on json at every level (opt 12 B ahead); text wins every tier incl. the flipped balanced (+1.65%); skewed wins everywhere except near-ties; random/zeros tie or win. Self-regression watch (not a zstd gap): zeros.balanced/best absolutes dropped 2.9×/5.1× vs 09-14 (still 9-10× zstd) — unattributed, suspect the rep1 covered-fill chain-linking; low priority. Checksum on/off (ours): json.fast 1.03, text.fast 0.85.

## Encode MT bulk (1 pass; cold pool per call both sides; MiB/s)

| cell | ours mt8 | zstd mt8 | x8 | ours mt16 | zstd mt16 | x16 | ratio ours mt16 | ratio zstd mt16 |
|---|---:|---:|---:|---:|---:|---:|---:|---:|
| json.fastest | 3706 | 4307 | 1.17 | 5324 | 1859 | 0.35 | 6.34 | 6.11 |
| json.fast | 2550 | 1061 | 0.42 | 3202 | 1047 | 0.33 | 5.30 | 5.31 |
| json.balanced | 420 | 204 | 0.48 | 419 | 202 | 0.49 | 7.19 | 5.95 |
| text.fastest | 21112 | 11318 | 0.54 | 19311 | 2275 | 0.12 | 309.09 | 39.78 |
| text.fast | 15243 | 2289 | 0.15 | 14775 | 2180 | 0.15 | 332.94 | 189.16 |
| text.balanced | 444 | 1284 | 2.85 | 444 | 1254 | 2.81 | 384.60 | 378.30 |
| skewed.fastest | 4478 | 3300 | 0.74 | 3644 | 1576 | 0.44 | 2.00 | 2.00 |
| skewed.fast | 1290 | 639 | 0.50 | 1621 | 639 | 0.39 | 1.92 | 1.92 |
| skewed.balanced | 728 | 122 | 0.17 | 728 | 122 | 0.17 | 2.00 | 1.84 |

## Streaming encode (64KiB pulls; json/text; interleaved medians)

| cell | ST ours | ST zstd | ST x | MT8 ours | MT8 zstd | MT8 x |
|---|---:|---:|---:|---:|---:|---:|
| json.fastest | 594 | 774 | 1.30 | 2694 | 1686 | 0.65 |
| json.fast | 467 | 441 | 0.94 | 1866 | 400 | 0.22 |
| json.balanced | — | — | — | 364 | 114 | 0.32 |
| json.best | 25 | 36 | 1.43 | 68 | 52 | 0.78 |
| json.opt | — | — | — | 16 | 7 | 0.43 |
| json.ultra | — | — | — | 6 | 3 | 0.52 |
| text.fastest | 6771 | 1776 | 0.26 | 6174 | 4210 | 0.68 |
| text.fast | 6978 | 5463 | 0.78 | 4932 | 1730 | 0.35 |
| text.balanced | — | — | — | 337 | 1011 | 3.02 |
| text.best | 479 | 689 | 1.44 | 308 | 362 | 1.17 |
| text.opt | — | — | — | 382 | 379 | 0.99 |
| text.ultra | — | — | — | 236 | 240 | 1.03 |

Unknown-size text.fastest streaming: zstd emits 1.84MB (ratio 18.3) vs our 108KB (ratio 309). Stream/ceiling ratios in [matrix.md](matrix.md) T5.

## Decode MT scaling (1 pass; our solo dimension; MiB/s)

| file | ours ST | mt2 | mt4 | mt8 | mt16 | zstd ST stream ref |
|---|---:|---:|---:|---:|---:|---:|
| json.zst3 | 1413 | 1710 | 2004 | 2089 | 2180 | 1882 |
| text.zst3 | 10514 | 10518 | 10523 | 10396 | 10418 | 11134 |
| skewed.zst9 | 614 | 734 | 649 | 699 | 746 | 796 |
| random.zst3 | 9727 | 9534 | 9533 | 9553 | 9541 | 8954 |

## Compression-ratio sweep (`zstdx-bench ratio`, 2026-09-16)

Geo-mean Δ over 120 cells **+8.66%**; per mode bulk-st +1.40% / bulk-mt +11.06% / stream-st +11.41% / stream-mt +11.10%; per shape json +4.41%, text +40.28%, skewed +1.41%, random ±0, zeros +1.99%. Losing cells (complete): text.best −0.15..−0.23% (all modes), skewed.opt −0.06..−0.07%. Not cell-comparable to the 09-14 sweep (output changed by design on several rows + json.raw regenerated; details in [matrix.md](matrix.md) T6).

## Top open deficits (from this run, x = ours/zstd wall time)

1. **text.balanced speed x3.47 ST / x2.81-3.02 mt** — the cold-start DUBT head's cost; ratio now +1.65% denser than zstd-9. Open lever: a cheap cold-start discriminator beyond the alphabet gate (todo 9); would also recover the stream-mt8 cell (x3.02) and the mt job-zero tax.
2. **Streaming decode on compressible shapes**: json x1.27-1.36, skewed x1.09-1.31, text x1.04-1.21 — core-vs-core, the fused loop vs libzstd's pipeline (todo 2). Structural levers falsified; only uop attrition remains, each cut a register-allocation lottery.
3. **json.fastest x1.44 ST / x1.17 mt8** — the steady scan loop's inherent branch-mispredict budget; dense-mode hosting spent the last angle, realistic ceiling ~x1.3-1.4 (todo 3). json.fast x1.04 is near closed.
4. **Best-tier core**: json x1.43 (ceiling ~1.2-1.3: per-node codegen + event volume), skewed x2.15 (memory-latency tree walk, ceiling ~2.16-2.22) (todo 6). Caps the two stream ST cells (json/text.best x1.43/1.44).
5. **json opt/ultra x1.10-1.12, skewed opt/ultra x1.05-1.07** — same per-node codegen + event-volume residue at ratio parity or denser (todo 6).
6. **MT decode**: text flat ~1.00×, skewed below the zstd stream ref — decoder-side piece-parallel stage B to spend the ramp guarantee (todo 1's open half; measured ceilings json 8.0×, skewed/text ~2×).
7. **dll100 large-binary ST** (09-16 doc baselines, todo 8): fastest x~1.44-1.55, fast x1.16, balanced x~1.55 — matcher-side instruction counts; per-position C-parity restructures falsified, emit live-set shrink the one untried lever.
8. **Small band 64K-1M** fastest/fast x~1.37-1.65 (todo 7): the same scan deficit plus per-block entropy share; below 64K fixed-cost-bound (largely pooled away, json-4K x1.62).
9. **Ratio residues**: text.best −0.15..−0.23% (head parse, todo 9b), skewed.opt −0.07% (near-tie), dictionary small-payload +3% parse-side (todo 10).
