# Bench matrix · fresh raw data (2026-09-28)

> Post-release-fix pass at `b213d1b8` (zstdx / zstdx-cli 0.1.0): every matrix section re-run plus the full numeric 1-22 ladder. Four commits after the 09-27 pass — the release pass's three beyond-noise drops were bisected and resolved: **R27** content-gates R8's small-src btlazy swap on an 8 KiB head verdict (json/skewed keep the stock row, text/dll/random keep the swap), **R28** reverts the fused bit-read decoder commit (`b889d6d2`, json mt2 −33%), and **R29** puts `strided_distinct` on a sampling diet with a same-day codegen fix (a `Skip<StepBy>` second stage plus an in-loop cap kept the walk un-rotated — text fastest/fast paid −13% until caught by this pass's first leg). Encoder outputs on the 32 MiB corpus are **byte-identical to the 09-27 pass** (full-ladder dump gate); the only byte movement anywhere is the small-payload band's json l9 rows (T8), which now walk the stock row. Net movement: decode MT restored (json mt16 1957→2224, back to 1.58× ST), zeros/random fast rows recovered (zeros fastest x0.31→0.27), text ST stream fast cells re-measured clean (the 09-27 −26..−28% reading was a pass transient — it reproduced in this pass's table too and again does not reproduce standalone). Conclusions live in [snapshot.md](snapshot.md). Earlier archives: 09-27/09-19/09-18/09-16 passes in this file's git history.

## Provenance

- commit `b213d1b8`, date 2026-09-28, tree clean. The decode sections, the ratio sweep and the dll `files` row ran at `c7399bd2` (pre the same-day far_screen fix): the encoder bytes are dump-gated identical across both commits and the fix touches no decode path, so those readings carry over; every encode-side section and the ladder re-ran at `b213d1b8`.
- CPU: AMD Eng Sample 100-000000870-32_Y (Zen4-class, 32 cores visible, AVX-512/BMI2), max clock 5386 MHz — same machine as every pass since 09-12.
- rustc 1.100.0-nightly (8925ea358 2026-08-20), release profile. Harness header prints `libzstd 1.5.7, binding 10507` (zstd crate / zstd-sys, `zstdmt` enabled).
- Corpus unchanged since the 09-19 pass (json.raw 2026-09-15, the other four 2026-09-07); `bench/big/dll100.*` the same 2026-09-19 02:08 set (dll100.raw = 104857600 B).
- Roundtrip verification gates ON for every cell. All runs under `flock /root/programs/fork/zstd-bench.lock`. Long sections were chunked by `--shape`/`--level` to bound wall time; the zeros cells that ride along in the dll chunks are duplicates of the dedicated zeros run and were discarded.
- Commands (all via `cargo run --release -p zstdx-bench`):
  - `ratio` — wall 118 s
  - `matrix --mode dec-st --budget-ms 1000 --file bench/big/dll100.zst1 --file bench/big/dll100.zst3 --file bench/big/dll100.zst9 --file bench/big/dll100.zst19` — 1m41
  - `matrix --mode dec-mt --budget-ms 1000` — 25 s
  - `matrix --mode enc-st --budget-ms 1000`, chunked: `--shape json` split `fastest,fast,balanced` / `best,opt,ultra` (11 s / 2m51), `--shape skewed` split `fastest,fast,balanced` / `best,opt` / `ultra` (32 s / 2m30 / 2m44), `--shape text` whole (18 s), `--shape random` split `fastest,fast,balanced` / `best,opt` / `ultra` (7 s / 3m43 / 5m29), `--shape zeros` whole (3m04), dll via `--shape zeros --file bench/big/dll100.raw` at `fastest,fast,balanced` / `best` / `opt` / `ultra` (46 s / 1m23 / 2m05 / 3m03)
  - `matrix --mode enc-mt --workers 8 --mt-workers 8 --budget-ms 1000` — 48 s; `--workers 16 --mt-workers 16` — 58 s
  - `matrix --mode enc-stream --workers 8 --mt-workers 8 --budget-ms 1000` — 3m59
  - `matrix --mode enc-st --full-ladder --budget-ms 500`, chunked per shape × level span: text 1-22 (36 s); json 1-14 / 15-17 / 18-19 / 20 / 21 / 22 (1m03 / 1m46 / 3m12 / 2m03 / 2m24 / 4m35); skewed 1-11 / 12-15 / 16-17 / 18-19 / 20 / 21 / 22 (1m36 / 2m36 / 3m03 / 5m08 / 3m14 / 3m38 / 3m39); random 1-13 / 14-16 / 17-18 / 19 / 20 / 21-22 (22 s / 1m24 / 3m59 / 2m52 / 3m26 / 5m05); zeros 1-14 / 15-19 / 20-22 (2m36 / 2m08 / 3m32) — 23 invocations, ≈64 min total
  - `small --level 1,3,9 --shape json,text`
  - `files --budget-ms 1000 --threads 2,4,8,16 bench/big/dll100.zst3` — the T2 dll row (solo tool; the zstd-ref number is T1's stream cell)
  - Beyond-noise cells are standalone-confirmed per the workflow pitfall: the text ST stream fast cells (see T5) and, during the pass's first leg at `c7399bd2`, text/json/skewed fast bulk rows (the R29 codegen tax, fixed same day — the fix's A/B used locked same-window runs).

## T1 decode ST (bulk + streaming, 64KiB pulls; MiB/s of raw)

`x = ours_time / zstd_time`, <1 = we are faster. Harness spreads ≤±0.006 on every cell.

| file | bulk ours | bulk zstd | bulk x | stream ours | stream zstd | stream x |
|---|---:|---:|---:|---:|---:|---:|
| json.zst1 | 1751 | 1272 | 0.73 | 1727 | 2169 | 1.26 |
| json.zst3 | 1446 | 1152 | 0.80 | 1377 | 1851 | 1.34 |
| json.zst9 | 1741 | 1239 | 0.71 | 1649 | 2117 | 1.28 |
| json.zst19 | 2170 | 1325 | 0.61 | 2193 | 2577 | 1.18 |
| text.zst1 | 6029 | 2244 | 0.37 | 6265 | 7541 | 1.20 |
| text.zst3 | 8765 | 2500 | 0.29 | 10421 | 11091 | 1.06 |
| text.zst9 | 9443 | 2543 | 0.27 | 11617 | 12100 | 1.04 |
| text.zst19 | 9494 | 2539 | 0.27 | 11588 | 12120 | 1.05 |
| skewed.zst1 | 2287 | 1524 | 0.67 | 2590 | 2842 | 1.10 |
| skewed.zst3 | 1246 | 1045 | 0.84 | 1266 | 1529 | 1.21 |
| skewed.zst9 | 647 | 654 | 1.01 | 607 | 786 | 1.30 |
| skewed.zst19 | 2191 | 1481 | 0.68 | 2399 | 2686 | 1.12 |
| random.zst3 | 8840 | 2306 | 0.26 | 11020 | 8820 | 0.80 |
| zeros.zst3 | 12327 | 2621 | 0.21 | 12869 | 12896 | 1.00 |
| dll100.zst1 | 1152 | 1012 | 0.88 | 1164 | 1504 | 1.29 |
| dll100.zst3 | 1236 | 1087 | 0.88 | 1254 | 1699 | 1.36 |
| dll100.zst9 | 1537 | 1235 | 0.80 | 1542 | 2068 | 1.34 |
| dll100.zst19 | 1336 | 1089 | 0.82 | 1348 | 1797 | 1.33 |

The R28 revert gives back the fused bit reads' ST win where it was real: the text stream column tightened to 1.04-1.06 (was 1.07-1.09), json zst9 stream to 1.28 (was 1.39) and dll bulk zst9 to 0.80 (was 0.81) — the documented +1.7-2.8% trade for restored MT. Bulk still wins 17/18 (skewed.zst9 1.01 the near-tie exception). Conclusions: [snapshot.md](snapshot.md).

## T2 decode MT scaling (solo; libzstd has no MT decode; MiB/s)

The dll row comes from `files --threads` (solo tool, same budget); its zstd-ref column is T1's stream cell.

| file | ours ST | zstd stream ST ref | mt2 | mt4 | mt8 | mt16 |
|---|---:|---:|---:|---:|---:|---:|
| json.zst3 | 1404 | 1873 | 1589 | 1992 | 2192 | 2224 |
| text.zst3 | 11034 | 11161 | 10919 | 10898 | 10888 | 10847 |
| skewed.zst9 | 648 | 795 | 739 | 733 | 652 | 674 |
| random.zst3 | 9732 | 8817 | 9498 | 9530 | 9543 | 9529 |
| dll100.zst3 | 1217 | 1699 | 1809 | 2060 | 2220 | 2240 |

The R28 revert restores the 09-19 scaling: json mt16 = 1.58× ST = 1.19× ref (the 09-27 pass's 1.40×/1.04× regression was the fused read body — mt2 back at 1.13× ST from 0.91×), dll mt16 = 1.84× ST = 1.32× ref (strongest row, on the real-binary payload). skewed mt16 674 = 1.04× ST still trails its zstd ref 795 (the ref itself drifted 682→793 since 09-19); text flat 0.98×. Scaling verdicts: [snapshot.md](snapshot.md).

## T3 encode ST bulk (checksums off both sides; MiB/s of raw; pairs 1/3/9/13/17/19)

dll = `bench/big/dll100.raw` (100 MB, `bench/gen_big.sh`); its ratios are payload sizes, not the 32 MiB corpus.

| level | shape | ours MiB/s | ours ratio | zstd MiB/s | zstd ratio | x |
|---|---|---:|---:|---:|---:|---:|
| fastest | json | 590 | 6.39 | 852 | 6.11 | 1.44 |
| fastest | text | 13473 | 309.23 | 10439 | 308.94 | 0.78 |
| fastest | skewed | 2912 | 2.00 | 1273 | 2.00 | 0.44 |
| fastest | random | 2411 | 1.00 | 2216 | 1.00 | 0.92 |
| fastest | zeros | 49454 | 32577 | 13320 | 32171 | 0.27 |
| fastest | dll | 532 | 4.35 | 535 | 2.19 | 1.01 |
| fast | json | 451 | 5.30 | 472 | 5.29 | 1.05 |
| fast | text | 13346 | 333.03 | 7249 | 332.90 | 0.54 |
| fast | skewed | 212 | 1.92 | 224 | 1.92 | 1.06 |
| fast | random | 2418 | 1.00 | 2040 | 1.00 | 0.85 |
| fast | zeros | 49030 | 32577 | 8688 | 32171 | 0.18 |
| fast | dll | 447 | 4.97 | 426 | 3.40 | 0.96 |
| balanced | json | 138 | 7.20 | 120 | 5.95 | 0.87 |
| balanced | text | 1225 | 384.66 | 1703 | 378.41 | 1.42 |
| balanced | skewed | 1726 | 2.00 | 74 | 1.84 | 0.043 |
| balanced | random | 2284 | 1.00 | 1579 | 1.00 | 0.69 |
| balanced | zeros | 31077 | 32577 | 1751 | 32202 | 0.056 |
| balanced | dll | 104 | 5.31 | 152 | 4.62 | 1.45 |
| best | json | 26 | 6.26 | 36 | 6.10 | 1.41 |
| best | text | 845 | 386.42 | 712 | 385.90 | 0.84 |
| best | skewed | 6 | 1.84 | 13 | 1.84 | 2.11 |
| best | random | 2347 | 1.00 | 253 | 1.00 | 0.11 |
| best | zeros | 41481 | 32577 | 839 | 32171 | 0.020 |
| best | dll | 28 | 5.35 | 37 | 4.67 | 1.31 |
| opt | json | 6 | 7.49 | 7 | 7.49 | 1.10 |
| opt | text | 761 | 410.23 | 460 | 410.11 | 0.61 |
| opt | skewed | 3 | 2.00 | 3 | 2.00 | 1.05 |
| opt | random | 2364 | 1.00 | 12 | 1.00 | 0.005 |
| opt | zeros | 40346 | 32577 | 938 | 32202 | 0.023 |
| opt | dll | 11 | 6.04 | 13 | 5.06 | 1.16 |
| ultra | json | 3 | 7.42 | 3 | 7.42 | 1.12 |
| ultra | text | 395 | 414.12 | 271 | 413.98 | 0.69 |
| ultra | skewed | 2 | 2.00 | 2 | 2.00 | 1.06 |
| ultra | random | 2392 | 1.00 | 8 | 1.00 | 0.003 |
| ultra | zeros | 39561 | 32577 | 673 | 32202 | 0.017 |
| ultra | dll | 7 | 6.32 | 9 | 5.28 | 1.20 |

R29's diet shows on the flat rows: zeros fastest/fast x0.27/0.18 (was 0.31/0.21), random fastest/fast 0.92/0.85 with the shortened bar leg; the same-day codegen fix holds text fastest/fast at the 09-27 level (13473/13346 vs 13696/13667 — inside the run band) after the broken first leg read 11866/11963. Every other tier reproduces 09-27 within noise. Conclusions: [snapshot.md](snapshot.md).

## T4 encode MT bulk (1 pass; cold pool per call both sides; MiB/s)

| cell | ours mt8 | zstd mt8 | x8 | ours mt16 | zstd mt16 | x16 | ratio ours mt16 | ratio zstd mt16 |
|---|---:|---:|---:|---:|---:|---:|---:|---:|
| json.fastest | 3633 | 4224 | 1.17 | 5203 | 1808 | 0.35 | 6.34 | 6.11 |
| json.fast | 2533 | 1048 | 0.41 | 3289 | 1038 | 0.32 | 5.30 | 5.31 |
| json.balanced | 356 | 203 | 0.57 | 418 | 205 | 0.49 | 7.19 | 5.95 |
| text.fastest | 20682 | 10782 | 0.53 | 18089 | 2268 | 0.13 | 309.08 | 39.78 |
| text.fast | 12086 | 2240 | 0.19 | 12112 | 2200 | 0.18 | 332.94 | 189.16 |
| text.balanced | 1032 | 1258 | 1.21 | 1054 | 1253 | 1.18 | 384.59 | 378.30 |
| skewed.fastest | 4212 | 3245 | 0.77 | 3651 | 1528 | 0.42 | 2.00 | 2.00 |
| skewed.fast | 1284 | 639 | 0.50 | 1649 | 639 | 0.39 | 1.92 | 1.92 |
| skewed.balanced | 830 | 122 | 0.15 | 826 | 121 | 0.15 | 2.00 | 1.84 |

json.fastest.mt8 x1.17 remains the one clean zstd mt win (ours 3633, zstd 4224; the cell is noisy 0.95-1.65). Our cold-pool mt16 json.fast (3289) still beats zstd's warm-pool reference (1643 this run); MT ratio preservation vs own ST holds within ~0.8%. Conclusions: [snapshot.md](snapshot.md).

## T5 encode streaming (64KiB pulls; interleaved medians)

| cell | ST ours | ST zstd | ST x | MT8 ours | MT8 zstd | MT8 x |
|---|---:|---:|---:|---:|---:|---:|
| json.fastest | 595 | 776 | 1.31 | 3406 | 1922 | 0.56 |
| json.fast | 467 | 442 | 0.95 | 2272 | 403 | 0.18 |
| json.balanced | — | — | — | 352 | 112 | 0.31 |
| json.best | 25 | 35 | 1.36 | 80 | 52 | 0.66 |
| json.opt | — | — | — | 16 | 7 | 0.40 |
| json.ultra | — | — | — | 6 | 3 | 0.50 |
| text.fastest | 6990 | 1735 | 0.25 | 7560 | 4144 | 0.55 |
| text.fast | 6702 | 5250 | 0.78 | 5512 | 1614 | 0.29 |
| text.balanced | — | — | — | 949 | 975 | 1.03 |
| text.best | 853 | 655 | 0.77 | 841 | 363 | 0.44 |
| text.opt | — | — | — | 666 | 380 | 0.58 |
| text.ultra | — | — | — | 363 | 244 | 0.67 |

The two text ST fast cells carry standalone medians: the in-pass table read 5163/5084 (x0.335/0.986) — the SAME collapse the 09-27 pass recorded (5240/5118), now twice-reproduced in full continuous passes and still never standalone: fresh narrow invocations read 6990/6702 (x0.249/0.783, spreads ±0.003/±0.006; the 09-28 pre-fix binary's standalone 6584/6309 and the t5b lane's four runs 6571-6871/6309-6614 all agree). The in-pass ceilings sit in the same depressed window (text 21938/15031 vs the standalone section's own mt8 cells at 7560/5512 scaling normally), so the effect is a sustained-load state specific to long continuous runs — ours drops ~25%, zstd's side ~2-4%. Mechanism open (recorded in the workflow pitfalls); the recorded table uses the standalone medians per that protocol. The real residual vs 09-19 (7243/6943) is ~-4%, the R23 unpledged-stream screen's sampling tax. Stream-mt8 vs own ceilings: json 90/85/96/99/133/120%, text 34/37/92/90/109/110%; text.opt/ultra stream-mt8 still beat their printed bulk-mt8 ceilings.

## T6 compression-ratio sweep (`zstdx-bench ratio`)

Full matrix 5 shapes x 6 levels x {bulk,stream} x {st,mt4}, one deterministic pass per cell, checksums off; Δ% = ours/zstd ratio − 1, geo-mean. Wall 118 s. Every cell roundtrip-gated. **Outputs are byte-identical to the 09-27 sweep** — R27's gate moves bytes only below 128 KiB declared (T8), R28/R29 touch no encoder output on this corpus; the sweep re-confirms it cell for cell.

Geo-mean Δ over 120 cells **+8.69%**; per mode: bulk-st **+1.48%**, bulk-mt **+11.08%**, stream-st **+11.43%**, stream-mt **+11.11%**; per shape: json +4.41%, text +40.36%, skewed +1.41%, random ±0, zeros +2.06%. Losing cells (complete): json.fast mt −0.12..−0.14%, text.ultra mt −0.02%, skewed.opt −0.06..−0.07%, skewed.ultra −0.04%, skewed.fast mt −0.01..−0.03%, skewed.fastest −0.00%. Everything else at parity or denser; random ties byte-exact at every cell; text.balanced +1.65..+1.70%; json.opt bulk-st 9 B ahead of zstd-17.

## T7 release gate — full numeric ladder 1-22 (enc-st, per-side budget 500 ms; MiB/s)

Both sides at the same numeric level; `x = ours_time / zstd_time`, <1 = we are faster. Cells whose three minimum rounds already cover the 500 ms budget stop at n=3 (json 6-22, skewed 4 and 13-22), so those absolutes carry the widest drift band; the rest accumulate n≥4 (text ≥13, random ≥38, zeros ≥500).

### Speed (ours/zstd MiB/s, x)

| level | json | text | skewed | random | zeros |
|---|---|---|---|---|---|
| 1 | 588/854 x1.45 | 13420/10441 x0.78 | 2889/1267 x0.44 | 2415/2269 x0.94 | 49327/13319 x0.27 |
| 2 | 554/654 x1.18 | 13134/10207 x0.78 | 2830/861 x0.31 | 2395/2297 x0.96 | 49246/13290 x0.27 |
| 3 | 457/477 x1.05 | 13296/7235 x0.54 | 213/228 x1.08 | 2412/2140 x0.89 | 48944/8678 x0.18 |
| 4 | 449/451 x1.01 | 11635/6886 x0.59 | 152/166 x1.09 | 2399/2169 x0.91 | 48369/8602 x0.18 |
| 5 | 189/267 x1.42 | 7198/4558 x0.63 | 2171/127 x0.059 | 2543/1900 x0.75 | 46768/6355 x0.14 |
| 6 | 123/188 x1.52 | 5749/2731 x0.48 | 2138/107 x0.050 | 2543/1879 x0.74 | 46147/2786 x0.060 |
| 7 | 101/166 x1.65 | 4929/2602 x0.53 | 1792/100 x0.056 | 2561/1794 x0.70 | 45385/2763 x0.061 |
| 8 | 86/126 x1.47 | 4081/1753 x0.43 | 1799/82 x0.046 | 2530/1802 x0.72 | 44761/1774 x0.040 |
| 9 | 138/119 x0.87 | 1216/1734 x1.43 | 1728/74 x0.043 | 2279/1597 x0.70 | 30607/1750 x0.057 |
| 10 | 56/91 x1.63 | 3110/1546 x0.50 | 1262/61 x0.048 | 2431/1211 x0.50 | 42775/1704 x0.040 |
| 11 | 39/66 x1.66 | 2423/1499 x0.61 | 1436/46 x0.032 | 2460/1347 x0.55 | 42971/1702 x0.040 |
| 12 | 33/55 x1.65 | 2321/884 x0.38 | 1180/24 x0.021 | 2386/669 x0.28 | 40481/1007 x0.025 |
| 13 | 25/36 x1.44 | 943/745 x0.79 | 6/13 x2.10 | 2341/252 x0.11 | 42186/831 x0.020 |
| 14 | 21/27 x1.26 | 812/608 x0.75 | 6/11 x1.89 | 2309/116 x0.050 | 40843/722 x0.018 |
| 15 | 19/16 x0.84 | 768/481 x0.63 | 6/7 x1.28 | 2332/111 x0.047 | 40327/637 x0.016 |
| 16 | 7/10 x1.41 | 791/533 x0.68 | 3/8 x2.58 | 2338/20 x0.009 | 42330/1092 x0.026 |
| 17 | 6/7 x1.10 | 778/463 x0.60 | 3/3 x1.05 | 2355/11 x0.005 | 40874/935 x0.023 |
| 18 | 3/4 x1.30 | 458/376 x0.82 | 2/3 x1.46 | 2345/10 x0.004 | 40854/883 x0.022 |
| 19 | 3/3 x1.11 | 394/271 x0.68 | 2/2 x1.06 | 2381/7 x0.003 | 39883/668 x0.017 |
| 20 | 3/2 x0.89 | 274/220 x0.80 | 2/1 x0.86 | 2373/6 x0.003 | 37500/419 x0.011 |
| 21 | 2/2 x0.97 | 266/159 x0.60 | 1/1 x0.99 | 2411/8 x0.003 | 35220/239 x0.007 |
| 22 | 2/1 x0.59 | 253/138 x0.55 | 1/1 x0.98 | 2412/9 x0.004 | 34200/205 x0.006 |

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

The ratio table reproduces 09-27 byte-for-byte (the dump gate again). Speed: zeros l1-4 recovered (x0.27/0.27/0.18/0.18, was 0.31/0.32/0.21/0.21) and random l1-4 tightened (0.89-0.96, was 0.90-0.97); text l1-4 held by the same-day fix (x0.78/0.78/0.54/0.59, was 0.77/0.77/0.53/0.57 — inside the band); the rest of the ladder reproduces 09-27 within noise. Remaining holes unchanged: json 5-8 (x1.42-1.65) and 10-12 (x1.63-1.66) carrying their byte wins, text l9 (x1.43), skewed 13-16 speed; 19-22 converge to parity or better everywhere except json.l19.

## T8 small-payload encode (1 KiB-1 MiB per-call; checksums off; x = ours/zstd)

| level | shape | 1K | 4K | 64K | 1024K |
|---|---|---:|---:|---:|---:|
| 1 | json | 1.25 | 1.35 | 1.42 | 1.35 |
| 1 | text | 1.64 | 1.53 | 1.73 | 1.49 |
| 3 | json | 1.31 | 1.14 | 1.03 | 1.16 |
| 3 | text | 1.40 | 1.14 | 1.03 | 1.02 |
| 9 | json | 1.16 | 0.95 | 0.66 | 1.09 |
| 9 | text | 1.73 | 1.47 | 2.79 | 1.08 |

R27's verdict lands exactly where the bisect said it should: json l9 keeps the stock row's speed shape (x1.16/0.95/0.66/1.09 — the 09-27 pass read 1.39/1.40/2.04/1.07 through the unconditional swap; skewed-64K l9 outside this table went 41→2063 MiB/s), text l9 keeps the swap's ratio at x1.73/1.47/2.79/1.08 (bytes unchanged). Level 1 (x1.25-1.73) and level 3 (x1.02-1.40) reproduce the accepted trades (the R14 tiny dense bars plus the fast row's small-src dense bars). The json l9 1K/4K rows carry the stock row's ±1 B seq-selection arm (follow-up noted in [matchers](../perf/matchers.md)).
