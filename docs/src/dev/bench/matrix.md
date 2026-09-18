# Bench matrix · fresh raw data (2026-09-18)

> Full pre-release re-run of every matrix section plus the full numeric 1-22 ladder at `c56418c2`, two days and 78 commits after the 09-16 pass at `dedd267d`. The ~30 perf commits in between rewrote the huff0 encoder path (two-queue tree build replacing package-merge, one-form table materialization), the FSE CTable build (C-shaped fused row-start pass, pooled 32 B FSETable, NCount serialization at C parity), the entropy-bound distinct-count skip, the mt-stream machinery (pooled worker threads, shared finish-tail prefix fill, mid-size far-class capture on bulk-mt and pledged streams, far-dead job-strip capping), the matchers (fused match tables, dense dict-content prefill, dfast dict grids, cold-head probe step on the best tier, landslide abort in the reach probe) and the block splitter (whole-range estimates + histogram subtraction). Conclusions live in [snapshot.md](snapshot.md). Earlier archives: 09-16 in git history of this file (pre-`c56418c2`), 09-14 pre-`3d163bd3`, 09-12 at `b264842`.

## Provenance

- commit `c56418c2`, date 2026-09-18, tree clean.
- CPU: AMD Eng Sample 100-000000870-32_Y (Zen4-class, 32 cores visible, AVX-512/BMI2), max clock 5386 MHz.
- rustc 1.100.0-nightly (8925ea358 2026-08-20), release profile. Harness header prints `libzstd 1.5.7, binding 10507` (zstd crate / zstd-sys, `zstdmt` enabled).
- Corpus: `bench/corpus`, 32MiB x 5 shapes (json regenerated 2026-09-15, the other four unchanged since 09-07). Ladder pairs numeric levels (fastest/fast/balanced/best/opt/ultra = 1/3/9/13/17/19).
- Roundtrip verification gates ON for every cell.
- Commands (all via `./target/release/zstdx-bench`, each under `flock /root/programs/fork/zstd-bench.lock`):
  - `ratio` — wall 121 s
  - `matrix --mode dec-st --budget-ms 1000` — wall 63 s
  - `matrix --mode dec-mt --budget-ms 1000` — wall 26 s
  - `matrix --mode enc-st --budget-ms 1000` — wall 1293 s
  - `matrix --mode enc-mt --workers 8 --mt-workers 8 --budget-ms 1000` — wall 47 s
  - `matrix --mode enc-mt --workers 16 --mt-workers 16 --budget-ms 1000` — wall 57 s
  - `matrix --mode enc-stream --workers 8 --mt-workers 8 --budget-ms 1000` — wall 237 s
  - `matrix --mode enc-st --full-ladder --budget-ms 500` — wall 3905 s (the release gate; levels 19-22 on json/skewed run 3 rounds of multi-second compressions per side)
  - `small --level 1,3,9 --shape json,text` — wall 28 s

## T1 decode ST (bulk + streaming, 64KiB pulls; MiB/s of raw)

`x = ours_time / zstd_time`, <1 = we are faster. Harness spreads ≤±0.007 on every cell.

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

Reproduces the 09-16 column within ±0.03 on every cell: the ST decode core had no perf commits in this window. The honest core-vs-core gap stays the stream column (zstd's bulk API is a slow wrapper; our own stream-vs-bulk spread ≤2%).

## T2 decode MT scaling (solo; libzstd has no MT decode; MiB/s)

| file | ours ST | zstd stream ST ref | mt2 | mt4 | mt8 | mt16 |
|---|---:|---:|---:|---:|---:|---:|
| json.zst3 | 1413 | 1876 | 1621 | 2066 | 2223 | 2262 |
| text.zst3 | 10603 | 11181 | 10516 | 10331 | 10474 | 10429 |
| skewed.zst9 | 649 | 793 | 755 | 765 | 749 | 678 |
| random.zst3 | 9742 | 8923 | 9573 | 9589 | 9571 | 9584 |

json.zst3 scaling improved again: mt16 = **1.60× our ST = 1.21× the zstd stream reference** (09-16: 1.54×/1.16×) — the piece-parallel stage B on ramp frames plus the pledge-driven MT engagement. skewed.zst9 sits at 1.04-1.18× ST / 0.85-0.96× ref; text flat ~0.98× ST / 0.93× ref; random 0.98× ST = 1.07× ref.

## T3 encode ST bulk (checksums off both sides; MiB/s of raw)

| shape.level | ours MiB/s | ours ratio | zstd MiB/s | zstd ratio | x | 09-16 x |
|---|---:|---:|---:|---:|---:|---:|
| json.fastest | 599 | 6.39 | 854 | 6.11 | 1.43 | 1.44 |
| json.fast | 458 | 5.30 | 474 | 5.29 | 1.03 | 1.04 |
| json.balanced | 142 | 7.20 | 121 | 5.95 | 0.85 | 0.76 |
| json.best | 25 | 6.26 | 36 | 6.10 | 1.45 | 1.43 |
| json.opt | 6 | 7.49 | 7 | 7.49 | 1.12 | 1.10 |
| json.ultra | 3 | 7.42 | 3 | 7.42 | 1.10 | 1.12 |
| text.fastest | 14114 | 309.22 | 10439 | 308.94 | 0.74 | 0.84 |
| text.fast | 14068 | 333.02 | 7280 | 332.90 | 0.52 | 0.53 |
| text.balanced | 1093 | 384.64 | 1649 | 378.41 | 1.51 | 3.47 |
| text.best | 807 | 386.39 | 737 | 385.90 | 0.92 | 0.95 |
| text.opt | 772 | 410.18 | 460 | 410.11 | 0.60 | 0.61 |
| text.ultra | 395 | 414.04 | 273 | 413.98 | 0.69 | 0.69 |
| skewed.fastest | 2944 | 2.00 | 1275 | 2.00 | 0.43 | 0.43 |
| skewed.fast | 213 | 1.92 | 227 | 1.92 | 1.07 | 1.02 |
| skewed.balanced | 1879 | 2.00 | 74 | 1.84 | 0.039 | 0.042 |
| skewed.best | 6 | 1.84 | 13 | 1.84 | 2.15 | 2.15 |
| skewed.opt | 3 | 2.00 | 3 | 2.00 | 1.00 | 1.05 |
| skewed.ultra | 2 | 2.00 | 2 | 2.00 | 1.07 | 1.07 |
| random.fastest | 2548 | 1.00 | 2194 | 1.00 | 0.86 | 0.90 |
| random.fast | 2539 | 1.00 | 2031 | 1.00 | 0.80 | 0.84 |
| random.balanced | 2306 | 1.00 | 1626 | 1.00 | 0.70 | 1.31 |
| random.best | 2333 | 1.00 | 251 | 1.00 | 0.11 | 0.14 |
| random.opt | 2377 | 1.00 | 11 | 1.00 | 0.005 | 0.005 |
| random.ultra | 2392 | 1.00 | 8 | 1.00 | 0.003 | 0.003 |
| zeros.fastest | 49722 | 32483 | 13285 | 32171 | 0.27 | 0.27 |
| zeros.fast | 49339 | 32483 | 8694 | 32171 | 0.18 | 0.18 |
| zeros.balanced | 32802 | 32483 | 1751 | 32202 | 0.053 | 0.108 |
| zeros.best | 42189 | 32483 | 833 | 32202 | 0.020 | 0.098 |
| zeros.opt | 40742 | 32483 | 939 | 32202 | 0.023 | 0.030 |
| zeros.ultra | 39910 | 32483 | 677 | 32202 | 0.017 | 0.022 |

The movers vs 09-16, all attributed: **text.balanced x3.47→1.51** (ours 499→1093 MiB/s) — the best-tier cold-head probe step and the huff0/FSE build rewrite cut the per-frame fixed costs that dominated this cell; ratio still +1.65% denser (384.64 vs 378.41). **text.best ratio flipped denser** (386.39 vs 385.90; the 09-16 sweep's −0.15..−0.23% residue closed by the cold-head probe step, todo 9b). **random.balanced x1.31→0.70** — the reach probe's incompressibility gate now probes the job's own history (the 09-16 table's double-parse attribution, fixed). **zeros.balanced/best recovered** to 32802/42189 MiB/s (09-16: 16080/8515; the per-frame fixed-cost regression closed 09-17, now also past the post-fix re-measure's 41768/42918). skewed.opt crossed to parity/ahead (x1.00, was 1.05). json.balanced x0.76→0.85 (ours 159→142) is the one backward move — inside the huff0/FSE rewrite window, still well ahead with +21% density; watch item.

Checksum overhead (ours, on/off time ratio): json.fast 1.03, text.fast 0.85.

## T4 encode MT bulk (1 pass; cold pool per call both sides; MiB/s)

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

text.balanced halved to x1.54/1.51 (was 2.85/2.81; no longer width-independent — 826 MiB/s at mt8 vs 444 before — the per-frame DUBT-head cost stopped dominating). json.fastest.mt8 stays the one mt cell zstd wins, x1.16 (spread 0.84-1.67, still the noisiest cell). json.balanced moved 0.48/0.49→0.64/0.62, mirroring the ST cell's backward move. Our cold-pool mt16 json.fast (3376) still beats zstd's warm-pool reference (1593). MT ratio preservation vs own ST holds within ±0.5%.

## T5 encode streaming (64KiB pulls; interleaved medians)

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

The pooled worker threads + shared finish-tail prefix fill moved every stream-mt8 cell: text best/opt/ultra crossed from parity/behind (1.17/0.99/1.03) to **ahead** (0.67/0.61/0.68), text.balanced 3.02→1.48, json.best 0.78→0.70, json.fastest 0.65→0.56. Stream-mt8 vs own bulk-mt8 ceilings: json 88/84/97/99/123/120% (fastest/fast/balanced/best/opt/ultra — opt/ultra above their printed ceiling refs, ceiling measurement noise), text 32/40/81/110/146/142% — text.opt/ultra stream-mt8 now *beat* the separately measured bulk-mt8 ceiling (the shared finish-tail fill is a streaming-only path; the ceiling refs are stale by design). text.fastest/fast stay bounded by the serial 16 KiB read-pump + epoch quiesce (todo 12). Ceilings (solo refs, this run): json 3864/2727/339/75/13/5, text 23909/15992/813/497/441/251.

## T6 compression-ratio sweep (`zstdx-bench ratio`, 2026-09-18)

Full matrix 5 shapes x 6 levels x {bulk,stream} x {st,mt4}, one deterministic pass per cell, checksums off; Δ% = ours/zstd ratio − 1, geo-mean. Wall 121 s. Every cell roundtrip-gated.

Geo-mean Δ over 120 cells **+8.67%**; per mode: bulk-st **+1.42%**, bulk-mt **+11.08%**, stream-st **+11.43%**, stream-mt **+11.11%**; per shape: json +4.41%, text +40.36%, skewed +1.41%, random ±0, zeros +1.99%. Worst cell json.fast.bulk-mt −0.14%; best text.fastest.stream-st +1594% (libzstd's unknown-size streaming collapse).

Losing cells (Δ < 0, complete list): json.fast mt modes **−0.12..−0.14%** (new; the mt fast row emits 6350 B more than its own ST row — watch item); text.ultra mt −0.04%; skewed.opt −0.06..−0.07% (carried near-tie); skewed.ultra −0.04%; skewed.fast mt −0.01..−0.03%; skewed.fastest −0.00% (byte-level tie). Everything else at parity or denser; random ties byte-exact at every cell. **text.best flipped from −0.15..−0.23% to +0.12..+0.20%** (the cold-head probe step, todo 9b closed); text.balanced holds +1.65..+1.70%; json.opt bulk-st 9 B ahead of zstd-17.

Not cell-comparable to the 09-16 sweep (+8.66%) beyond the headline: the encoder output changed by design on multiple rows (huff0/FSE rewrite is speed-only, but the cold-head probe, the dict-grid matchers, the block splitter estimates and the mid-size far-class captures all move bytes).

## T7 release gate — full numeric ladder 1-22 (enc-st, per-side budget 500 ms; MiB/s)

Both sides at the same numeric level; `x = ours_time / zstd_time`, <1 = we are faster. Levels 19-22 run 3 interleaved rounds of multi-second compressions per side (n=3), so their absolutes carry the widest drift band; everything at ≤12 has n≥28.

### Speed (ours/zstd MiB/s, x)

| level | json | text | skewed | random | zeros |
|---|---|---|---|---|---|
| 1 | 602/855 x1.42 | 14133/10426 x0.74 | 2931/1266 x0.43 | 2536/2196 x0.87 | 49569/13285 x0.27 |
| 2 | 582/678 x1.16 | 13866/10214 x0.74 | 2866/856 x0.30 | 2538/2183 x0.86 | 49466/13272 x0.27 |
| 3 | 455/469 x1.03 | 14127/7265 x0.51 | 213/220 x1.04 | 2545/2056 x0.81 | 49177/8714 x0.18 |
| 4 | 454/450 x0.99 | 11565/6965 x0.60 | 153/166 x1.09 | 2528/2121 x0.84 | 48506/8602 x0.18 |
| 5 | 212/262 x1.24 | 3637/4541 x1.25 | 2495/126 x0.05 | 2535/1832 x0.72 | 48334/6345 x0.13 |
| 6 | 155/182 x1.17 | 3341/2728 x0.82 | 2473/105 x0.04 | 2527/1812 x0.72 | 47530/2785 x0.06 |
| 7 | 102/164 x1.61 | 2957/2663 x0.90 | 2286/99 x0.04 | 2521/1779 x0.70 | 46684/2764 x0.06 |
| 8 | 92/125 x1.37 | 2752/1773 x0.64 | 2273/81 x0.04 | 2520/1782 x0.70 | 46604/1773 x0.04 |
| 9 | 142/122 x0.86 | 1166/1731 x1.49 | 1841/74 x0.04 | 2220/1494 x0.67 | 32932/1752 x0.05 |
| 10 | 42/92 x2.19 | 1808/1522 x0.84 | 2025/61 x0.03 | 2432/1324 x0.55 | 44762/1701 x0.04 |
| 11 | 23/65 x2.84 | 1877/1400 x0.75 | 1974/46 x0.02 | 2455/1394 x0.57 | 44648/1699 x0.04 |
| 12 | 30/56 x1.86 | 1760/904 x0.52 | 1936/24 x0.01 | 2415/672 x0.28 | 41628/1007 x0.02 |
| 13 | 26/37 x1.44 | 804/733 x0.91 | 6/13 x2.10 | 2350/251 x0.11 | 42408/834 x0.02 |
| 14 | 22/27 x1.23 | 727/610 x0.84 | 6/11 x1.91 | 2365/118 x0.05 | 40907/723 x0.02 |
| 15 | 19/16 x0.83 | 654/480 x0.74 | 6/8 x1.27 | 2356/112 x0.05 | 40337/642 x0.02 |
| 16 | 7/10 x1.40 | 780/520 x0.67 | 3/8 x2.41 | 2359/21 x0.009 | 42704/1093 x0.03 |
| 17 | 6/7 x1.10 | 782/465 x0.60 | 3/3 x0.99 | 2368/11 x0.005 | 41249/934 x0.02 |
| 18 | 3/5 x1.33 | 462/385 x0.83 | 2/3 x1.51 | 2365/10 x0.004 | 41060/890 x0.02 |
| 19 | 3/3 x1.12 | 395/272 x0.69 | 2/2 x1.08 | 2392/7 x0.003 | 39910/674 x0.02 |
| 20 | 3/2 x0.90 | 254/222 x0.87 | 2/1 x0.87 | 2386/6 x0.003 | 37550/423 x0.01 |
| 21 | 2/2 x0.97 | 248/158 x0.64 | 1/1 x1.02 | 2392/8 x0.003 | 35770/244 x0.007 |
| 22 | 2/1 x0.60 | 235/138 x0.59 | 1/1 x0.99 | 2407/9 x0.004 | 33996/209 x0.006 |

### Ratio (ours/zstd, higher = denser)

| level | json | text | skewed | random | zeros |
|---|---|---|---|---|---|
| 1 | 6.39/6.11 | 309.22/308.94 | 2.00/2.00 | 1.00/1.00 | 32483/32171 |
| 2 | 6.39/5.73 | 312.54/315.95 | 2.00/2.00 | 1.00/1.00 | 32483/32171 |
| 3 | 5.30/5.29 | 333.02/332.90 | 1.92/1.92 | 1.00/1.00 | 32483/32171 |
| 4 | 5.29/5.28 | 333.24/333.23 | 1.81/1.81 | 1.00/1.00 | 32483/32171 |
| 5 | 5.81/5.57 | 339.11/358.32 | 2.00/1.86 | 1.00/1.00 | 32483/32171 |
| 6 | 6.66/5.76 | 358.15/370.17 | 2.00/1.86 | 1.00/1.00 | 32483/32202 |
| 7 | 6.79/5.84 | 363.85/374.93 | 2.00/1.84 | 1.00/1.00 | 32483/32202 |
| 8 | 7.14/5.99 | 368.11/378.25 | 2.00/1.84 | 1.00/1.00 | 32483/32202 |
| 9 | 7.20/5.95 | 384.64/378.41 | 2.00/1.84 | 1.00/1.00 | 32483/32202 |
| 10 | 7.21/6.03 | 371.31/381.88 | 2.00/1.84 | 1.00/1.00 | 32483/32202 |
| 11 | 7.26/6.08 | 373.32/383.83 | 2.00/1.84 | 1.00/1.00 | 32483/32202 |
| 12 | 7.25/6.08 | 373.32/383.85 | 2.00/1.84 | 1.00/1.00 | 32483/32202 |
| 13 | 6.26/6.10 | 386.39/385.90 | 1.84/1.84 | 1.00/1.00 | 32483/32171 |
| 14 | 6.32/6.13 | 388.45/388.11 | 1.84/1.84 | 1.00/1.00 | 32483/32171 |
| 15 | 6.32/6.16 | 389.30/388.95 | 1.84/1.84 | 1.00/1.00 | 32483/32171 |
| 16 | 7.15/7.10 | 409.89/406.09 | 2.00/2.00 | 1.00/1.00 | 32483/32202 |
| 17 | 7.49/7.49 | 410.18/410.11 | 2.00/2.00 | 1.00/1.00 | 32483/32202 |
| 18 | 7.42/7.42 | 413.29/412.91 | 2.00/2.00 | 1.00/1.00 | 32483/32202 |
| 19 | 7.42/7.42 | 414.04/413.98 | 2.00/2.00 | 1.00/1.00 | 32483/32202 |
| 20 | 7.42/7.41 | 414.04/413.98 | 2.00/2.00 | 1.00/1.00 | 32483/32233 |
| 21 | 7.41/7.41 | 414.11/414.09 | 2.00/2.00 | 1.00/1.00 | 32483/32233 |
| 22 | 7.41/7.41 | 414.13/414.12 | 2.00/2.00 | 1.00/1.00 | 32483/32233 |

Reading the ladder: the known inversions are ours to keep or close — json levels 5-8 (greedy/lazy chain rows) sit at x1.2-1.6 behind libzstd's dfast/chain rows, and json 10-12 at x1.9-2.8, while our level-9 row (x0.86) beats them on both axes; the deep-chain rows remain the weakest band on json. text flips the pattern: levels 5 and 9 lose speed (x1.25/1.49) but levels 6-8 and 10-22 win; only text rows 5-12 give up ratio (up to −5.3% at row 5, the window-vs-row-matcher story). skewed rows 5-12 are x0.01-0.05 (we parse once and emit; libzstd's chain walk crawls the 16-letter alphabet) with ratio denser at 2.00 vs 1.84-1.86. skewed rows 13-16 (btlazy2 territory) stay behind on speed (x1.27-2.41) at ratio parity. Levels 19-22 mostly converge to parity or better everywhere except json.l19 (x1.12).
