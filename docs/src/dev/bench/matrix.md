# Bench matrix · fresh raw data (2026-09-19)

> Release pass at `e0b6078b` (zstdx / zstdx-cli 0.1.0): every matrix section re-run plus the full numeric 1-22 ladder. 67 commits after the 09-18/09-19 passes — almost entirely correctness/review fixes (decoder output caps and wildcopy bounds, stream error paths, MT-stream reserve/poison caps, compat/CLI), plus two decoder-stream perf commits (`read_to_end` probe-buffer growth, `write::Decoder` bounded-cursor staging) and the btlazy LDM `limitTableUpdate` clamp. Encoder outputs are byte-identical to 09-18 on every corpus cell (T6 reproduces exactly), so the speed columns moved only where those commits touch (decode stream) and the dll rows are new. **dll100 is promoted from side payload to a formal shape**: the full six-tier encode row set, all four decode levels and an MT-decode scaling row are integrated into the shape axes below. Conclusions live in [snapshot.md](snapshot.md). Earlier archives: 09-18/09-16 passes in this file's git history.

## Provenance

- commit `e0b6078b`, date 2026-09-19, tree clean.
- CPU: AMD Eng Sample 100-000000870-32_Y (Zen4-class, 32 cores visible, AVX-512/BMI2), max clock 5386 MHz — same machine as every pass since 09-12.
- rustc 1.100.0-nightly (8925ea358 2026-08-20), release profile. Harness header prints `libzstd 1.5.7, binding 10507` (zstd crate / zstd-sys, `zstdmt` enabled).
- Corpus unchanged (json.raw regenerated 2026-09-15, the other four since 09-07); `bench/big/dll100.*` the same 2026-09-19 02:08 set the `a1176a9c` additions used (dll100.raw = 104857600 B).
- Roundtrip verification gates ON for every cell. All runs under `flock /root/programs/fork/zstd-bench.lock`. Long sections were chunked by `--shape`/`--level` to bound wall time; the zeros cells that ride along in the dll chunks are duplicates of the dedicated zeros run and were discarded.
- Commands (all via `./target/release/zstdx-bench`):
  - `ratio` — wall 119 s
  - `matrix --mode dec-st --budget-ms 1000 --file bench/big/dll100.zst{1,3,9,19}`
  - `matrix --mode dec-mt --budget-ms 1000`
  - `matrix --mode enc-st --budget-ms 1000`, chunked: `--shape json|skewed` split `--level fastest,fast,balanced` / `--level best,opt,ultra` (11/34 s and 2m46/5m03), `--shape text|random|zeros` whole-shape (19 s / 9m16 / 3m07), dll via `--shape zeros --file bench/big/dll100.raw` at the same two tier groups (47 s / 6m26)
  - `matrix --mode enc-mt --workers 8 --mt-workers 8 --budget-ms 1000` — 47 s; `--workers 16 --mt-workers 16` — 57 s
  - `matrix --mode enc-stream --workers 8 --mt-workers 8 --budget-ms 1000` — 3m57
  - `matrix --mode enc-st --full-ladder --budget-ms 500`, chunked per shape × level span (17 invocations, ≈70 min total; slowest chunks random 20-22 8m29, skewed 16-19 7m46, json 21-22 7m00, skewed 20-21 6m51, random 17-19 6m35)
  - `small --level 1,3,9 --shape json,text`
  - `files --budget-ms 1000 --threads 2,4,8,16 bench/big/dll100.zst3` — the T2 dll row (solo tool; the zstd-ref number is T1's stream cell)

## T1 decode ST (bulk + streaming, 64KiB pulls; MiB/s of raw)

`x = ours_time / zstd_time`, <1 = we are faster. Harness spreads ≤±0.007 on every cell.

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

Corpus cells reproduce the 09-18/09-19 tables within noise except where the two decoder-stream staging commits land: **text stream narrowed to x1.04-1.07** (was 1.09-1.10 on zst3/9/19) and json.zst19 stream 1.20→1.19; json zst9/zst19 bulk also ticked up (0.73→0.71, 0.62→0.60). dll rows: bulk wins every level (x0.82-0.89, 1144-1527 MiB/s ours); stream x1.30-1.37 like every compressible shape.

## T2 decode MT scaling (solo; libzstd has no MT decode; MiB/s)

The dll row comes from `files --threads` (solo tool, same budget); its zstd-ref column is T1's stream cell.

| file | ours ST | zstd stream ST ref | mt2 | mt4 | mt8 | mt16 |
|---|---:|---:|---:|---:|---:|---:|
| json.zst3 | 1402 | 1884 | 1585 | 2014 | 2160 | 2199 |
| text.zst3 | 11188 | 11183 | 10885 | 10866 | 10918 | 10921 |
| skewed.zst9 | 613 | 795 | 739 | 725 | 667 | 682 |
| random.zst3 | 9749 | 8977 | 9532 | 9561 | 9567 | 9560 |
| dll100.zst3 | 1220 | 1708 | 1774 | 2004 | 2192 | 2217 |

**dll100.zst3 is the strongest scaling row: mt16 = 1.82× our ST = 1.30× the zstd stream reference** — the restart-point parallel decode pays off best on the large real-binary payload. json.zst3 mt16 = 1.57× ST = 1.17× ref (09-18: 1.60×/1.21×, inside the noise band). skewed.zst9 1.09-1.21× ST / 0.84-0.93× ref; text flat 0.97-0.98× on both; random 0.98× ST = 1.06-1.07× ref.

## T3 encode ST bulk (checksums off both sides; MiB/s of raw; pairs 1/3/9/13/17/19)

dll = `bench/big/dll100.raw` (100 MB, `bench/gen_big.sh`); its ratios are payload sizes, not the 32 MiB corpus.

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

Corpus cells sit within ±0.03 of the 09-18 table (text.balanced 1.51→1.47 the largest move, still +1.65% denser than zstd-9). dll: fastest x1.36 gives up 1.8% size; from fast up every tier is denser — fast −2.2%, balanced −8.4%, best −12.8%, opt −10.3%, ultra −10.5% — while balanced stays the open speed cell (x1.49); best/opt/ultra (x1.39/1.10/1.19) are first measured this pass.

Checksum overhead (ours, on/off time ratio): json.fast 1.03, text.fast 0.84.

## T4 encode MT bulk (1 pass; cold pool per call both sides; MiB/s)

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

json.fastest.mt8 stays the one mt cell zstd wins, x1.18 (spread 0.92-1.57, still the noisiest cell); mt16 is ours x0.34. text.balanced improved to x1.48 both widths (was 1.51-1.54). Our cold-pool mt16 json.fast (3402) still beats zstd's warm-pool reference (1636/1652 across the two runs). MT ratio preservation vs own ST holds within ±0.5%.

## T5 encode streaming (64KiB pulls; interleaved medians)

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

text.balanced stream-mt8 x1.52 remains the last behind cell; text.opt/ultra (0.59/0.68) still beat their printed bulk-mt8 ceilings (the shared finish-tail fill is streaming-only; ceiling refs are stale by design). Stream-mt8 vs own ceilings: json 84/82/99/102/123/120%, text 33/38/78/110/153/147%. Ceilings (solo refs, this run): json 3796/2671/337/70/13/5, text 23820/16083/853/477/422/241.

## T6 compression-ratio sweep (`zstdx-bench ratio`)

Full matrix 5 shapes x 6 levels x {bulk,stream} x {st,mt4}, one deterministic pass per cell, checksums off; Δ% = ours/zstd ratio − 1, geo-mean. Wall 119 s. Every cell roundtrip-gated. **Outputs are byte-identical to the 09-18 sweep** — the 67 commits in between moved no corpus cell's bytes — so every number reproduces exactly:

Geo-mean Δ over 120 cells **+8.67%**; per mode: bulk-st **+1.42%**, bulk-mt **+11.08%**, stream-st **+11.43%**, stream-mt **+11.11%**; per shape: json +4.41%, text +40.36%, skewed +1.41%, random ±0, zeros +1.99%. Losing cells (complete): json.fast mt −0.12..−0.14%, text.ultra mt −0.04%, skewed.opt −0.06..−0.07%, skewed.ultra −0.04%, skewed.fast mt −0.01..−0.03%, skewed.fastest −0.00%. Everything else at parity or denser; random ties byte-exact at every cell; text.balanced +1.65..+1.70%; json.opt bulk-st 9 B ahead of zstd-17.

## T7 release gate — full numeric ladder 1-22 (enc-st, per-side budget 500 ms; MiB/s)

Both sides at the same numeric level; `x = ours_time / zstd_time`, <1 = we are faster. Cells whose three minimum rounds already cover the 500 ms budget stop at n=3 (every json row ≥6, skewed 3-4 and 13-22, text 18-22 — the multi-second compressions), so those absolutes carry the widest drift band; the rest accumulate n≥28 (random ≥36, zeros ≥500).

### Speed (ours/zstd MiB/s, x)

| level | json | text | skewed | random | zeros |
|---|---|---|---|---|---|
| 1 | 602/857 x1.43 | 14260/10494 x0.74 | 2946/1277 x0.43 | 2575/2235 x0.87 | 50356/13335 x0.26 |
| 2 | 563/658 x1.17 | 14042/10259 x0.73 | 2886/865 x0.30 | 2586/2233 x0.87 | 50429/13314 x0.26 |
| 3 | 475/482 x1.02 | 14344/7251 x0.51 | 213/224 x1.05 | 2564/2056 x0.80 | 49943/8701 x0.17 |
| 4 | 447/446 x1.00 | 12155/6929 x0.57 | 155/166 x1.08 | 2559/2101 x0.82 | 49469/8618 x0.17 |
| 5 | 216/265 x1.23 | 3773/4550 x1.21 | 2431/126 x0.052 | 2570/1837 x0.71 | 48846/6315 x0.13 |
| 6 | 157/185 x1.19 | 3470/2729 x0.79 | 2399/106 x0.044 | 2566/1826 x0.71 | 48363/2779 x0.057 |
| 7 | 102/165 x1.62 | 3001/2571 x0.86 | 2265/100 x0.044 | 2544/1756 x0.69 | 47513/2758 x0.058 |
| 8 | 92/126 x1.37 | 2777/1738 x0.63 | 2231/82 x0.037 | 2538/1755 x0.69 | 47105/1775 x0.038 |
| 9 | 140/122 x0.87 | 1176/1707 x1.45 | 1831/75 x0.041 | 2313/1597 x0.69 | 32119/1753 x0.055 |
| 10 | 42/91 x2.16 | 1963/1526 x0.78 | 2006/62 x0.031 | 2470/1333 x0.54 | 44995/1704 x0.038 |
| 11 | 23/66 x2.87 | 1900/1420 x0.75 | 2003/48 x0.024 | 2468/1383 x0.56 | 44982/1703 x0.038 |
| 12 | 31/56 x1.82 | 1770/902 x0.51 | 1961/25 x0.013 | 2423/686 x0.28 | 43014/1009 x0.024 |
| 13 | 25/37 x1.46 | 786/721 x0.92 | 7/14 x2.12 | 2331/256 x0.11 | 44122/829 x0.019 |
| 14 | 22/27 x1.22 | 683/593 x0.88 | 6/12 x1.88 | 2243/117 x0.052 | 41874/722 x0.017 |
| 15 | 19/16 x0.87 | 616/476 x0.77 | 6/8 x1.25 | 2287/112 x0.049 | 40990/640 x0.016 |
| 16 | 7/10 x1.39 | 784/519 x0.66 | 3/9 x2.58 | 2301/21 x0.009 | 43707/1092 x0.025 |
| 17 | 6/7 x1.09 | 765/459 x0.60 | 3/3 x1.05 | 2317/12 x0.005 | 41830/933 x0.022 |
| 18 | 3/5 x1.33 | 464/386 x0.83 | 2/3 x1.51 | 2307/10 x0.004 | 42502/881 x0.021 |
| 19 | 3/3 x1.14 | 392/270 x0.69 | 2/2 x1.09 | 2324/8 x0.003 | 40304/669 x0.017 |
| 20 | 3/2 x0.89 | 257/224 x0.87 | 2/2 x0.86 | 2404/6 x0.003 | 42245/413 x0.010 |
| 21 | 2/2 x0.96 | 250/161 x0.64 | 1/1 x1.00 | 2422/8 x0.003 | 36798/245 x0.007 |
| 22 | 2/1 x0.58 | 238/140 x0.59 | 1/1 x0.99 | 2437/10 x0.004 | 40393/211 x0.005 |

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

Ratio table byte-identical to 09-18 (deterministic outputs). Speed structure unchanged: every delta sits inside the drift band and no cell crossed a win/lose boundary except near-ties (json l4 0.99→1.00, skewed l3 1.04→1.05, skewed l17 0.99→1.05, skewed l21 1.02→1.00). The standing inversions: json 5-8 (chain rows, x1.19-1.62) and 10-12 (x1.82-2.87) lose on both axes while our level-9 row wins (x0.87 at +21% density); text only loses speed at rows 5/9 (x1.21/1.45) and ratio at rows 5-12; skewed 13-16 (btlazy2) stay behind on speed (x1.25-2.58) at ratio parity; 19-22 converge to parity or better everywhere except json.l19 (x1.14).

## T8 small-payload encode (1 KiB-1 MiB per-call; checksums off; x = ours/zstd)

| level | shape | 1K | 4K | 64K | 1024K |
|---|---|---:|---:|---:|---:|
| 1 | json | 1.14 | 1.22 | 1.41 | 1.36 |
| 1 | text | 1.30 | 1.25 | 1.41 | 1.48 |
| 3 | json | 1.31 | 1.10 | 0.97 | 1.14 |
| 3 | text | 1.28 | 1.07 | 0.96 | 1.02 |
| 9 | json | 0.90 | 0.81 | 0.61 | 1.06 |
| 9 | text | 0.69 | 0.54 | 0.68 | 1.04 |

Same picture as the 09-18 spot checks: the fastest tier trails through the whole 64K-1M band (allocator + fixed costs), level 3 sits near parity, level 9 is ahead except the two 1M cells.
