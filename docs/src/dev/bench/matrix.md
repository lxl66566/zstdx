# Bench matrix · fresh raw data (2026-09-27)

> Release pass at `8c22a660` (zstdx / zstdx-cli 0.1.0): every matrix section re-run plus the full numeric 1-22 ladder. 108 commits after the 09-19 pass — the R14-R26 LDM/far-class campaign (preSplit port, gap-parse LDM consumers on the fast/dfast/chain rows, far-class screens including the small/mid-size band, bulk-mt/stream-mt LDM captures, the parallel LDM split scan, bulk-mt state pooling, stream-mt finish-tail publication, small-frame entropy tails) plus one decoder commit (fused bit reads in `decode_step`). **Encoder outputs are no longer byte-identical to 09-19** (first byte movement since the 09-16 corpus): every zeros bulk-st cell lost 3 B, the text/json chain-ladder rows and text ultra got denser (json l5-8 −7..−17% bytes, text l5-8 −2..−5%), and dll was transformed (fastest r 2.15→4.35, fast 3.47→4.97, opt 5.64→6.04, ultra 5.89→6.32 vs the zstd crate). Speed followed: text chain rows ~2× faster, json 10-12 sharply faster, MT balanced rows +17-26%, while json 5-6, the skewed chain rows and the random/zeros fast rows pay for the LDM screens — and small-payload level-9 flipped from ahead to behind (T8). Conclusions live in [snapshot.md](snapshot.md). Earlier archives: 09-19/09-18/09-16 passes in this file's git history.

## Provenance

- commit `8c22a660`, date 2026-09-27, tree clean.
- CPU: AMD Eng Sample 100-000000870-32_Y (Zen4-class, 32 cores visible, AVX-512/BMI2), max clock 5386 MHz — same machine as every pass since 09-12.
- rustc 1.100.0-nightly (8925ea358 2026-08-20), release profile. Harness header prints `libzstd 1.5.7, binding 10507` (zstd crate / zstd-sys, `zstdmt` enabled).
- Corpus unchanged since the 09-19 pass (json.raw 2026-09-15, the other four 2026-09-07); `bench/big/dll100.*` the same 2026-09-19 02:08 set (dll100.raw = 104857600 B).
- Roundtrip verification gates ON for every cell. All runs under `flock /root/programs/fork/zstd-bench.lock`. Long sections were chunked by `--shape`/`--level` to bound wall time; the zeros cells that ride along in the dll chunks are duplicates of the dedicated zeros run and were discarded.
- Commands (all via `cargo run --release -p zstdx-bench`):
  - `ratio` — wall 119 s
  - `matrix --mode dec-st --budget-ms 1000 --file bench/big/dll100.zst1 --file bench/big/dll100.zst3 --file bench/big/dll100.zst9 --file bench/big/dll100.zst19` — ~3 min
  - `matrix --mode dec-mt --budget-ms 1000` — ~40 s
  - `matrix --mode enc-st --budget-ms 1000`, chunked: `--shape json` split `--level fastest,fast,balanced` / `best,opt,ultra` (35 s / 2m50), `--shape skewed` split `fastest,fast,balanced` / `best,opt` / `ultra` (36 s / 2m24 / 2m46), `--shape text` whole (21 s), `--shape random` split `fastest,fast,balanced` / `best,opt` / `ultra` (16 s / 3m46 / 5m28), `--shape zeros` whole (3m09), dll via `--shape zeros --file bench/big/dll100.raw` at `fastest,fast,balanced` / `best` / `opt` / `ultra` (1m03 / 1m28 / 2m05 / 3m02)
  - `matrix --mode enc-mt --workers 8 --mt-workers 8 --budget-ms 1000` — 52 s; `--workers 16 --mt-workers 16` — 1m09
  - `matrix --mode enc-stream --workers 8 --mt-workers 8 --budget-ms 1000` — 3m55
  - `matrix --mode enc-st --full-ladder --budget-ms 500`, chunked per shape × level span: text 1-22 (37 s); json 1-14 / 15-17 / 18-19 / 20 / 21 / 22 (1m02 / 1m43 / 3m06 / 2m03 / 2m24 / 4m35); skewed 1-11 / 12-15 / 16-17 / 18-19 / 20 / 21 / 22 (1m42 / 2m30 / 2m51 / 4m54 / 3m14 / 3m38 / 3m39); random 1-13 / 14-16 / 17-18 / 19 / 20 / 21-22 (33 s / 1m29 / 3m51 / 2m48 / 3m27 / 5m01); zeros 1-14 / 15-19 / 20-22 (2m38 / 2m08 / 3m38) — 23 invocations, ≈64 min total; slowest chunks random 21-22 5m01, skewed 18-19 4m54, json 22 4m35
  - `small --level 1,3,9 --shape json,text`
  - `files --budget-ms 1000 --threads 2,4,8,16 bench/big/dll100.zst3` — the T2 dll row (solo tool; the zstd-ref number is T1's stream cell)

## T1 decode ST (bulk + streaming, 64KiB pulls; MiB/s of raw)

`x = ours_time / zstd_time`, <1 = we are faster. Harness spreads ≤±0.006 on every cell.

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

Every cell sits within ±0.05 of the 09-19 table (inside the ±10% run-to-run band): the dll stream column tightened (1.30-1.37 → 1.24-1.34) and dll bulk zst3/zst9/zst19 improved to 0.86/0.81/0.85, while text stream zst3/9/19 drifted back to 1.07-1.09 (was 1.04-1.07) and json.zst9 bulk to 0.74 (was 0.71). Bulk still wins 17/18 (skewed.zst9 1.03 the exception). Conclusions: [snapshot.md](snapshot.md).

## T2 decode MT scaling (solo; libzstd has no MT decode; MiB/s)

The dll row comes from `files --threads` (solo tool, same budget); its zstd-ref column is T1's stream cell.

| file | ours ST | zstd stream ST ref | mt2 | mt4 | mt8 | mt16 |
|---|---:|---:|---:|---:|---:|---:|
| json.zst3 | 1401 | 1887 | 1271 | 1734 | 1999 | 1957 |
| text.zst3 | 11110 | 11151 | 10973 | 10966 | 10994 | 10941 |
| skewed.zst9 | 605 | 791 | 636 | 638 | 609 | 592 |
| random.zst3 | 9732 | 8951 | 9533 | 9565 | 9567 | 9546 |
| dll100.zst3 | 1225 | 1710 | 1520 | 1988 | 2101 | 2194 |

json.zst3 mt16 = 1.40× ST = 1.04× ref (09-19: 1.57×/1.17×) — a real regression beyond noise, worst at mt2 (0.91× ST, was 1.13×); skewed mt cells also dropped ~13% (mt16 0.98× ST, 0.75× ref); text and random are flat/unchanged and dll scales as before (mt16 1.79× ST = 1.28× ref). ST and zstd-ref columns reproduce 09-19 within noise, so the movement is specific to our MT (piece-parallel) decode path on json/skewed; the single decoder commit in the range (`b889d6d2`, fused bit reads) is the candidate. Scaling verdicts: [snapshot.md](snapshot.md).

## T3 encode ST bulk (checksums off both sides; MiB/s of raw; pairs 1/3/9/13/17/19)

dll = `bench/big/dll100.raw` (100 MB, `bench/gen_big.sh`); its ratios are payload sizes, not the 32 MiB corpus.

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

json/skewed tier cells reproduce 09-19 within noise. The LDM campaign's costs and wins: random fastest/fast ours −8% (2411/2405 vs 2623/2597) and zeros fastest/fast −16% (42562/42294 vs 50683/50425) — the far-class screens tax the fast rows on incompressible data; text.balanced improved 1.47→1.41 and text.best 0.92→0.84; dll is a different encoder now — fastest at speed parity (x1.01) with double density (r 2.15→4.35, −49.6% bytes vs zstd), fast faster AND denser (x0.95, r 3.47→4.97), balanced 1.49→1.43 at 5.05→5.31, best x1.38 at −12.8% bytes, opt 1.10→1.16 at −16.2%, ultra 1.19 at −16.5%.

Checksum overhead (ours, on/off time ratio): json.fast 1.00, text.fast 0.86.

## T4 encode MT bulk (1 pass; cold pool per call both sides; MiB/s)

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

The balanced MT rows gained 17-26% on our side: text.balanced x1.48→1.23/1.21, skewed.balanced 0.17-0.18→0.15, json.balanced 0.61→0.56/0.50. json.fastest.mt8 x1.15 remains the one clean zstd mt win (ours stable 3706, zstd 4217; noisy 0.87-1.53). MT ratio preservation vs own ST holds within ~0.8% (json.fastest 6.39→6.34). Our cold-pool mt16 json.fast (3371) still beats zstd's warm-pool reference (1643, this run). Conclusions: [snapshot.md](snapshot.md).

## T5 encode streaming (64KiB pulls; interleaved medians)

| cell | ST ours | ST zstd | ST x | MT8 ours | MT8 zstd | MT8 x |
|---|---:|---:|---:|---:|---:|---:|
| json.fastest | 594 | 772 | 1.30 | 3182 | 1899 | 0.58 |
| json.fast | 464 | 439 | 0.94 | 2279 | 402 | 0.18 |
| json.balanced | — | — | — | 370 | 116 | 0.31 |
| json.best | 26 | 35 | 1.37 | 79 | 53 | 0.67 |
| json.opt | — | — | — | 16 | 7 | 0.42 |
| json.ultra | — | — | — | 6 | 3 | 0.53 |
| text.fastest | 6851 | 1739 | 0.25 | 7456 | 4154 | 0.56 |
| text.fast | 6571 | 5211 | 0.79 | 5908 | 1603 | 0.28 |
| text.balanced | — | — | — | 931 | 981 | 1.06 |
| text.best | 849 | 686 | 0.81 | 856 | 368 | 0.43 |
| text.opt | — | — | — | 674 | 380 | 0.57 |
| text.ultra | — | — | — | 367 | 241 | 0.67 |

Stream-mt8 vs own ceilings: json 84/86/99/96/123/120%, text 34/40/88/84/113/106%. Ceilings (solo refs, this run): json 3813/2661/374/82/13/5, text 21866/14941/1052/1021/598/345. The text ceilings jumped with the MT gains (balanced 853→1052, best 477→1021, opt 422→598, ultra 241→345); text.balanced stream-mt8 closed to x1.06 (was 1.52) and text.best ST stream to x0.81 (was 1.40, ours 495→849). The two text ST fast rows carry fresh standalone medians: the pass table read 5240/5118 (x0.33/0.98, a claimed −26..−28% vs 09-19) — re-measured after the pass in fresh processes (two clean runs, spreads ±0.003/±0.011, zstd sides at the pass's own level), the cells land at 6851/6571 (x0.25/0.79): the pass readings were a mid-run transient (a sibling load burst collapses this cell's ours side ~7× harder than zstd's — reproduced deliberately), and the real movement vs 09-19 (7243/6943) is ~−5%, the R23 unpledged-stream screen's documented sampling tax; both remain wins. text.opt/ultra stream-mt8 still beat their printed bulk-mt8 ceilings.

## T6 compression-ratio sweep (`zstdx-bench ratio`)

Full matrix 5 shapes x 6 levels x {bulk,stream} x {st,mt4}, one deterministic pass per cell, checksums off; Δ% = ours/zstd ratio − 1, geo-mean. Wall 119 s. Every cell roundtrip-gated. **Outputs are NOT byte-identical to the 09-19 sweep** — the first byte movement since the 09-16 corpus: every zeros bulk-st cell lost 3 B (ours 1030 vs 1033), text.ultra lost ~20 B on every mode, and the tier-axis cells otherwise reproduce (json/skewed/random byte-exact, text tiers exact to the byte except ultra). The ladder-only chain-row changes are in T7.

Geo-mean Δ over 120 cells **+8.69%** (09-19: +8.67%); per mode: bulk-st **+1.48%** (was +1.42), bulk-mt **+11.08%**, stream-st **+11.43%**, stream-mt **+11.11%**; per shape: json +4.41%, text +40.36%, skewed +1.41%, random ±0, zeros +2.06% (was +1.99). Losing cells (complete): json.fast mt −0.12..−0.14%, text.ultra mt −0.02%, skewed.opt −0.06..−0.07%, skewed.ultra −0.04%, skewed.fast mt −0.01..−0.03%, skewed.fastest −0.00%. Everything else at parity or denser; random ties byte-exact at every cell; text.balanced +1.65..+1.70%; json.opt bulk-st 9 B ahead of zstd-17.

## T7 release gate — full numeric ladder 1-22 (enc-st, per-side budget 500 ms; MiB/s)

Both sides at the same numeric level; `x = ours_time / zstd_time`, <1 = we are faster. Cells whose three minimum rounds already cover the 500 ms budget stop at n=3 (json 6-22, skewed 4 and 13-22), so those absolutes carry the widest drift band; the rest accumulate n≥4 (text ≥4, random ≥36, zeros ≥500).

### Speed (ours/zstd MiB/s, x)

| level | json | text | skewed | random | zeros |
|---|---|---|---|---|---|
| 1 | 592/851 x1.44 | 13632/10485 x0.77 | 2922/1260 x0.43 | 2417/2299 x0.95 | 42246/13282 x0.31 |
| 2 | 578/680 x1.18 | 13379/10244 x0.77 | 2856/865 x0.30 | 2390/2308 x0.97 | 41732/13264 x0.32 |
| 3 | 470/482 x1.02 | 13592/7242 x0.53 | 215/226 x1.05 | 2388/2160 x0.90 | 42339/8708 x0.21 |
| 4 | 450/451 x1.00 | 12203/6943 x0.57 | 152/165 x1.09 | 2374/2184 x0.92 | 42086/8630 x0.21 |
| 5 | 190/265 x1.40 | 7483/4580 x0.61 | 2138/128 x0.060 | 2548/1894 x0.74 | 47752/6353 x0.13 |
| 6 | 125/186 x1.49 | 5920/2740 x0.46 | 2108/107 x0.051 | 2550/1882 x0.74 | 47239/2784 x0.059 |
| 7 | 103/167 x1.61 | 4958/2598 x0.52 | 1798/100 x0.056 | 2530/1799 x0.71 | 46668/2760 x0.059 |
| 8 | 88/127 x1.44 | 4162/1752 x0.42 | 1783/82 x0.046 | 2553/1816 x0.71 | 46163/1774 x0.038 |
| 9 | 139/122 x0.87 | 1226/1732 x1.41 | 1755/75 x0.043 | 2300/1609 x0.70 | 32577/1751 x0.054 |
| 10 | 57/92 x1.63 | 3173/1551 x0.49 | 1264/61 x0.048 | 2454/1217 x0.50 | 43840/1702 x0.039 |
| 11 | 40/66 x1.65 | 2406/1487 x0.62 | 1434/50 x0.035 | 2479/1374 x0.55 | 43634/1702 x0.039 |
| 12 | 34/56 x1.63 | 2325/891 x0.38 | 1137/25 x0.022 | 2417/687 x0.29 | 40684/1014 x0.025 |
| 13 | 26/36 x1.40 | 942/748 x0.79 | 7/14 x2.12 | 2355/265 x0.11 | 42822/839 x0.020 |
| 14 | 22/27 x1.21 | 726/598 x0.83 | 6/12 x1.90 | 2369/119 x0.050 | 41204/729 x0.018 |
| 15 | 20/17 x0.85 | 701/476 x0.68 | 6/8 x1.27 | 2369/114 x0.048 | 40565/643 x0.016 |
| 16 | 7/10 x1.37 | 790/521 x0.66 | 3/9 x2.53 | 2366/21 x0.009 | 42674/1104 x0.026 |
| 17 | 6/7 x1.07 | 778/464 x0.60 | 3/3 x1.02 | 2367/12 x0.005 | 41455/942 x0.023 |
| 18 | 4/5 x1.31 | 464/381 x0.82 | 2/3 x1.51 | 2375/10 x0.004 | 41422/895 x0.022 |
| 19 | 3/3 x1.11 | 395/269 x0.68 | 2/2 x1.08 | 2403/8 x0.003 | 40129/681 x0.017 |
| 20 | 3/2 x0.89 | 275/222 x0.81 | 2/2 x0.86 | 2398/6 x0.003 | 37803/428 x0.011 |
| 21 | 2/2 x0.96 | 268/159 x0.60 | 1/1 x1.02 | 2427/8 x0.003 | 42532/242 x0.006 |
| 22 | 2/1 x0.59 | 254/139 x0.55 | 1/1 x0.99 | 2435/10 x0.004 | 36619/209 x0.006 |

### Ratio (ours/zstd, higher = denser)

| level | json | text | skewed | random | zeros |
|---|---|---|---|---|---|
| 1 | 6.39/6.11 | 309.23/308.94 | 2.00/2.00 | 1.00/1.00 | 32577/32171 |
| 2 | 6.39/5.73 | 312.55/315.95 | 2.00/2.00 | 1.00/1.00 | 32577/32171 |
| 3 | 5.30/5.29 | 333.03/332.90 | 1.92/1.92 | 1.00/1.00 | 32577/32171 |
| 4 | 5.29/5.28 | 333.26/333.23 | 1.81/1.81 | 1.00/1.00 | 32577/32171 |
| 5 | 6.02/5.57 | 356.06/358.32 | 2.00/1.86 | 1.00/1.00 | 32577/32171 |
| 6 | 6.90/5.76 | 367.54/370.17 | 2.00/1.86 | 1.00/1.00 | 32577/32202 |
| 7 | 7.02/5.84 | 372.60/374.93 | 2.00/1.84 | 1.00/1.00 | 32577/32202 |
| 8 | 7.23/5.99 | 375.04/378.25 | 2.00/1.84 | 1.00/1.00 | 32577/32202 |
| 9 | 7.20/5.95 | 384.66/378.41 | 2.00/1.84 | 1.00/1.00 | 32577/32202 |
| 10 | 7.24/6.03 | 378.68/381.88 | 2.00/1.84 | 1.00/1.00 | 32577/32202 |
| 11 | 7.28/6.08 | 380.76/383.83 | 2.00/1.84 | 1.00/1.00 | 32577/32202 |
| 12 | 7.28/6.08 | 380.79/383.85 | 2.00/1.84 | 1.00/1.00 | 32577/32202 |
| 13 | 6.26/6.10 | 386.42/385.90 | 1.84/1.84 | 1.00/1.00 | 32577/32171 |
| 14 | 6.32/6.13 | 388.47/388.11 | 1.84/1.84 | 1.00/1.00 | 32577/32171 |
| 15 | 6.32/6.16 | 389.32/388.95 | 1.84/1.84 | 1.00/1.00 | 32577/32171 |
| 16 | 7.15/7.10 | 409.94/406.09 | 2.00/2.00 | 1.00/1.00 | 32577/32202 |
| 17 | 7.49/7.49 | 410.23/410.11 | 2.00/2.00 | 1.00/1.00 | 32577/32202 |
| 18 | 7.42/7.42 | 413.37/412.91 | 2.00/2.00 | 1.00/1.00 | 32577/32202 |
| 19 | 7.42/7.42 | 414.12/413.98 | 2.00/2.00 | 1.00/1.00 | 32577/32202 |
| 20 | 7.42/7.41 | 414.12/413.98 | 2.00/2.00 | 1.00/1.00 | 32577/32233 |
| 21 | 7.41/7.41 | 414.20/414.09 | 2.00/2.00 | 1.00/1.00 | 32577/32233 |
| 22 | 7.41/7.41 | 414.22/414.12 | 2.00/2.00 | 1.00/1.00 | 32577/32233 |

The 09-19 ratio table no longer reproduces: the LDM campaign densified every json chain row (l5-8 −7..−17% bytes, l10-12 −0.4%) and the text rows 5-8/10-12 (−2..−5%), and shaved 3 B off every zeros row (ours 32483→32577 at all levels; the zstd column is unchanged). Speed structure: the text chain rows flipped to clear wins (l5 x1.21→0.61, l6-8 0.63-0.86→0.42-0.53, l10-12 0.51-0.78→0.38-0.62) and json 10-12 narrowed from x1.82-2.87 to 1.63-1.65; json 5-6 got slower (1.23/1.19→1.40/1.49) buying their density; skewed chain rows pay 12-42% (all still wins, x≤0.06); random l1-4 pay 6-10% and zeros l1-4 13-17% (all still wins). Remaining holes: json 5-8 (x1.40-1.61) and 10-12 (x1.63-1.65), text l9 (x1.41), skewed 13-16 speed (x1.27-2.53 at ratio parity); 19-22 converge to parity or better everywhere except json.l19 (x1.11).

## T8 small-payload encode (1 KiB-1 MiB per-call; checksums off; x = ours/zstd)

| level | shape | 1K | 4K | 64K | 1024K |
|---|---|---:|---:|---:|---:|
| 1 | json | 1.21 | 1.35 | 1.38 | 1.34 |
| 1 | text | 1.65 | 1.51 | 1.66 | 1.48 |
| 3 | json | 1.29 | 1.13 | 0.98 | 1.16 |
| 3 | text | 1.37 | 1.08 | 1.11 | 1.05 |
| 9 | json | 1.13 | 0.95 | 0.64 | 1.09 |
| 9 | text | 1.73 | 1.48 | 2.78 | 1.08 |

The 09-19 picture inverted at level 9: the tier was ahead through 1K-64K (x0.54-0.90) and the 09-27 pass found it behind everywhere (x1.07-2.78, worst at 64K; json-64K re-run standalone reproduced x2.030). Bisect (2026-09-28) pinned the flip on R8's small-src btlazy swap alone (`f0eb106f`: json-64K l9 184→54 MiB/s, zstd's 111 flat) — the R14/R20 suspects were falsified (the R20 screen arms only at 32-64 MiB; the R14 entropy tails measure ±1 B on the stock row). R27 content-gated the swap on an 8 KiB head verdict (`sampled_distinct`, bar 48): json (38-39 distinct) and skewed (16) keep the stock chain/row rows — json l9 now x1.13/0.95/0.64/1.09, skewed-64K l9 41→2063 MiB/s at −188 B — while text (66-79), dll32 (108-213) and random keep the swap and its ratio (text-64K l9 14808 B, −0.9% vs zstd-9; dll32-64K −557 B). Level 1's 09-27 slide (1K/4K both shapes, text-64K; the R14 tiny dense bars plus the fast row's small-src dense bars) and level 3 (0.98-1.29) are unchanged this round — the accepted trades.
