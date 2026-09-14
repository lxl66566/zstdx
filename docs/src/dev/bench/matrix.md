# Bench matrix · fresh raw data (2026-09-14)

> Full re-run of the matrix sections below at `ad57445` (one day after the 09-12 pass at `0e7044d`; the intervening commits are the six byte-identical-output encoder perf lands — DfastEmit inline `0ef6319`, pack_seq zero-add-bit `d9ac825`, hash dead-mask `42241ac`, chain-walk inline+distance-resolve `69e0ad5`, chain seeded/steady split `2d65d94`, pipelined hash+head `3c07dea` — plus dictionary-trainer work that does not touch codec paths, so ratio columns are unchanged by construction and the deltas are pure speed). Conclusions live in [snapshot.md](snapshot.md). The 2026-09-12 archive is in git history of this file at `b264842`.

## Provenance

- commit `ad57445`, date 2026-09-14, branch ext, tree clean.
- CPU: AMD Eng Sample 100-000000870-32_Y (Zen4-class, 32 cores visible, AVX-512/BMI2), max clock 5386 MHz.
- rustc 1.100.0-nightly (8925ea358 2026-08-20), release profile. Harness header prints `libzstd 1.5.7, binding 10507` (zstd crate / zstd-sys, `zstdmt` enabled).
- Corpus: `bench/corpus` (2026-09-10 generation, unchanged since), 32MiB x 5 shapes; decode uses zstd-CLI-precompressed zst1/zst3/zst9. Ladder pairs numeric levels (fastest/fast/balanced/best/opt/ultra = 1/3/9/13/17/19).
- Roundtrip verification gates ON for every cell.
- Commands (all via `./target/release/zstdx-bench matrix`):
  - `--mode dec-st --budget-ms 2000` (2 passes)
  - `--mode enc-st --level fastest,fast,balanced --budget-ms 1500` (2 passes)
  - `--mode enc-st --level best,opt,ultra --budget-ms 1500` (2 passes)
  - `--mode enc-mt --workers 8 --mt-workers 8 --level fastest,fast,balanced --budget-ms 1500` (1 pass)
  - `--mode enc-mt --workers 16 --mt-workers 16 --level fastest,fast,balanced --budget-ms 1500` (1 pass)
  - `--mode enc-stream --mt-workers 8 --budget-ms 1500` (2 passes + 1 focused `--shape text --level balanced` re-check of the noisiest cell)
  - `--mode dec-mt --budget-ms 1500` (1 pass)
  - plus the full `ratio` sweep (141 s).
- Budget: per-side ms, interleaved rounds, median-of-ratios verdict. Slow-cell absolute MiB/s tracks the 09-12 pass (same sustained-clock regime); the x columns are interleaved same-run ratios. Two-pass sections below are medians; single-pass sections (enc-mt/dec-mt) lean on internal duplicate cells and in-run consistency.

## T1 decode ST (bulk + streaming, 64KiB pulls; 2 passes; MiB/s of raw)

`x = ours_time / zstd_time`, <1 = we are faster. Pass spread ≤2% on all cells except json.zst1.stream (1.25-1.28) and skewed.zst3.stream (1.22-1.25).

| file | bulk ours | bulk zstd | bulk x | stream ours | stream zstd | stream x |
|---|---:|---:|---:|---:|---:|---:|
| json.zst1 | 1742 | 1290 | 0.74 | 1723 | 2185 | 1.27 |
| json.zst3 | 1417 | 1174 | 0.83 | 1385 | 1876 | 1.35 |
| json.zst9 | 1719 | 1247 | 0.73 | 1649 | 2145 | 1.30 |
| text.zst1 | 5540 | 2274 | 0.41 | 6231 | 7561 | 1.21 |
| text.zst3 | 8564 | 2545 | 0.30 | 10413 | 11084 | 1.06 |
| text.zst9 | 9342 | 2602 | 0.28 | 11590 | 12108 | 1.04 |
| skewed.zst1 | 2306 | 1513 | 0.66 | 2583 | 2847 | 1.10 |
| skewed.zst3 | 1243 | 1036 | 0.83 | 1257 | 1535 | 1.22 |
| skewed.zst9 | 635 | 653 | 1.03 | 602 | 794 | 1.32 |
| random.zst3 | 8899 | 2186 | 0.25 | 11108 | 8948 | 0.81 |
| zeros.zst3 | 11591 | 2407 | 0.21 | 12847 | 12883 | 1.00 |

No decoder commits since the 09-12 pass — cells reproduce it within noise. Our own stream-vs-bulk spread is ≤2% on every shape; the honest core-vs-core gap is the stream column (zstd's bulk API is a slow wrapper).

## T2 decode MT scaling (solo; libzstd has no MT decode; 1 pass; MiB/s)

| file | ours ST | zstd stream ST ref | mt2 | mt4 | mt8 | mt16 |
|---|---:|---:|---:|---:|---:|---:|
| json.zst3 | 1402 | 1877 | 1254 | 1439 | 1544 | 1551 |
| text.zst3 | 10633 | 11179 | 10684 | 10709 | 10703 | 10672 |
| skewed.zst9 | 635 | 794 | 450 | 456 | 452 | 456 |
| random.zst3 | 9788 | 9035 | 9574 | 9583 | 9610 | 9596 |

Still no scaling anywhere (stage-B serial fraction); skewed.zst9 anti-scales to 0.71x ST at every width; json.zst3 best case mt16 = 1.11x ST (0.83x of the zstd ST reference); random.zst3 holds 0.98x ST = 1.06x the zstd reference. See todo item 1 (stage-B parallelization) — unchanged verdict, fresh numbers.

## T3 encode ST bulk (checksums off both sides; 2 passes; MiB/s of raw)

Output sizes deterministic, identical across passes and identical to the 09-12 tables (byte-identical-output perf lands). Slow cells (json/skewed best/opt/ultra, n=3 rounds per pass) agreed within 2% between passes; all fast cells within 3%.

| shape.level | ours MiB/s | ours ratio | zstd MiB/s | zstd ratio | x | Δ vs 09-12 x |
|---|---:|---:|---:|---:|---:|---:|
| json.fastest | 548 | 6.14 | 859 | 6.11 | 1.57 | 1.67 |
| json.fast | 431 | 5.29 | 478 | 5.29 | 1.11 | 1.21 |
| json.balanced | 90 | 7.11 | 121 | 5.95 | 1.34 | 1.51 |
| json.best | 9 | 6.75 | 37 | 6.10 | 4.22 | 4.10 |
| json.opt | 5 | 7.46 | 7 | 7.49 | 1.30 | 1.32 |
| json.ultra | 2 | 7.42 | 3 | 7.42 | 1.32 | 1.33 |
| text.fastest | 13225 | 309.17 | 10502 | 308.94 | 0.79 | 0.83 |
| text.fast | 13295 | 332.97 | 7271 | 332.90 | 0.55 | 0.60 |
| text.balanced | 2890 | 367.89 | 1700 | 378.41 | 0.59 | 0.65 |
| text.best | 416 | 404.78 | 733 | 385.90 | 1.77 | 1.78 |
| text.opt | 451 | 408.76 | 469 | 410.11 | 1.04 | 1.04 |
| text.ultra | 265 | 412.58 | 274 | 413.98 | 1.05 | 1.05 |
| skewed.fastest | 2877 | 2.00 | 1259 | 2.00 | 0.44 | 0.44 |
| skewed.fast | 209 | 1.92 | 229 | 1.92 | 1.09 | 1.22 |
| skewed.balanced | 2041 | 2.00 | 75 | 1.84 | 0.037 | 0.041 |
| skewed.best | 2 | 2.00 | 14 | 1.84 | 6.22 | 6.10 |
| skewed.opt | 2 | 2.00 | 3 | 2.00 | 1.49 | 1.50 |
| skewed.ultra | 1 | 2.00 | 2 | 2.00 | 1.40 | 1.40 |
| random.fastest | 2335 | 1.00 | 2092 | 1.00 | 0.90 | 0.88 |
| random.fast | 2328 | 1.00 | 1892 | 1.00 | 0.81 | 0.80 |
| random.balanced | 2317 | 1.00 | 1529 | 1.00 | 0.66 | 0.61 |
| random.best | 2323 | 1.00 | 253 | 1.00 | 0.11 | 0.09 |
| random.opt | 2329 | 1.00 | 12 | 1.00 | 0.005 | 0.004 |
| random.ultra | 2341 | 1.00 | 8 | 1.00 | 0.003 | 0.003 |
| zeros.fastest | 50333 | 32483 | 13309 | 32171 | 0.26 | 0.26 |
| zeros.fast | 50045 | 32483 | 8701 | 32171 | 0.17 | 0.17 |
| zeros.balanced | 46548 | 32483 | 1754 | 32202 | 0.038 | 0.037 |
| zeros.best | 43534 | 32483 | 831 | 32202 | 0.019 | 0.019 |
| zeros.opt | 41904 | 32483 | 936 | 32202 | 0.023 | 0.022 |
| zeros.ultra | 40703 | 32483 | 673 | 32202 | 0.017 | 0.017 |

The low-tier speed lands moved: json fastest/fast/balanced +7/+8/+13%, text fastest/fast/balanced +5/+11/+10%, skewed fast/balanced +12/+13% vs the 09-12 absolutes. best/opt/ultra tiers and all ratio columns flat (no commits touched them; the random cells' absolutes drifted −5..−9% with zstd's own reference −3% — clock state, x columns unchanged within +0.02).

## T4 encode MT bulk (1 pass; cold pool per call both sides; MiB/s)

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

MT ratio preservation vs own ST unchanged (every cell within ±0.5%). zstd warm-pool json.fast mt16 reference: 1580-1615 MiB/s (ours cold-pool 3340). json.fastest.mt8 remains the one mt cell zstd wins cleanly (x1.27, was 1.35); json.balanced.mt8 improved to 0.71 (was 0.86).

## T5 encode streaming (64KiB pulls; ST = 2-pass medians, MT8 = 2 passes + focused re-check)

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

json.fast stream ST is now at parity (was x1.11). The text fast/balanced/best stream-mt8 cells are bimodal across passes (per-round burst-schedule variance; text.balanced measured 485, 1171, then 1485 on a focused re-run — the focused 1485/x0.66 matches the 09-12 value, so no regression, just variance to keep in mind when reading single passes). Bulk-mt8 ceilings (this run's T4): json fastest/fast/balanced/best = 3403/2551/294/19, text = 19712/15034/1937/228. Stream-MT8 reaches 43/45/64/79% (json) and 12/12-15/25-77/71-84% (text) of its own bulk ceiling; text.fastest (12%) and json.fastest (43%) stay bounded by the burst model's serialized accumulate/spawn/barrier (persistent-pool redesign, todo 12).

## T6 compression-ratio sweep (`zstdx-bench ratio`, 2026-09-14)

Full matrix 5 shapes x 6 levels x {bulk,stream} x {st,mt4}, one deterministic pass per cell, checksums off; Δ% = ours/zstd ratio − 1, geo-mean. Wall 141 s. Every cell byte-count identical to the 09-12 sweep (byte-identical-output lands). Geo-mean Δ over 120 cells **+9.02%**; per mode: bulk-st **+1.73%**, bulk-mt **+11.42%**, stream-st **+11.81%**, stream-mt **+11.46%**; per shape: json +4.78%, text +40.22%, skewed +2.75%, random ±0, zeros +1.99%.

> 2026-09-14 later note: a same-code re-run (753136d, ours-side sizes byte-identical by construction) prints geo-mean +8.23% / json +3.49% / skewed +1.41% — the drift is on the reference side of the Δ or its cell set, unresolved; build-vs-build cell diffs (the sweep's actual regression gate) stay exact, which is what the 09-14 prefill-cap/seed lands were gated on.

Losing cells (Δ < 0, complete list): text.balanced **−2.73..−2.79%** (all four modes — the one sizable deficit); text.opt −0.30..−0.35%; text.ultra −0.29..−0.34%; json.opt −0.18..−0.31% (vs the much-denser zstd-17); json.ultra mt −0.13%; json.fast mt −0.17..−0.19%; skewed fast/opt/ultra −0.01..−0.07%. random ties byte-exact at every cell; zeros mt wins up to +11%.

The mt/stream margins are libzstd's losses, not our gains: zstd-mt on text collapses (fastest 39.8 vs our 309, fast 189 vs 333) while our mt stays within ~0.1% of our ST; zstd's unknown-size streaming at level 1 emits 1.84 MB (ratio 18.3) where our streaming matches our bulk (309).
