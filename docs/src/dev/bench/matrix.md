# Bench matrix · fresh raw data (2026-09-12)

> Full re-run of the matrix sections listed below at `0e7044d` (includes the Opt-tier bt ring C23→C22 landing). Conclusions live in [snapshot.md](snapshot.md). The 2026-09-11 archive (old sections A-E and the pre-ladder tables) is in git history of this file at `96867c5`.

## Provenance

- commit `0e7044d`, date 2026-09-12, branch ext, tree otherwise clean.
- CPU: AMD Eng Sample 100-000000870-32_Y (Zen4-class, 32 cores visible, AVX-512/BMI2), max clock 5386 MHz.
- rustc 1.100.0-nightly (8925ea358 2026-08-20), release profile. Harness header prints `libzstd 1.5.7, binding 10507` (zstd crate / zstd-sys, `zstdmt` enabled).
- Corpus: `bench/corpus` (2026-09-10 generation), 32MiB x 5 shapes; decode uses zstd-CLI-precompressed zst1/zst3/zst9.
- **Pairing change since the 2026-09-11 tables**: the ladder now pairs numeric levels (fastest/fast/balanced/best/opt/ultra = 1/3/9/13/17/19, `e11d07e`); the 09-11 tables paired 1/3/6/12/16/19. balanced/best/opt cells are therefore NOT cell-for-cell comparable to the old pages (both ratio and x columns move with the reference level).
- Roundtrip verification gates ON for every cell.
- Commands (all via `./target/release/zstdx-bench matrix`; enc-mt/enc-stream and the second best/opt/ultra pass ran as per-`--shape` splits with identical flags):
  - `--mode dec-st --budget-ms 2000` (2 passes)
  - `--mode enc-st --level fastest,fast,balanced --budget-ms 1500` (2 passes)
  - `--mode enc-st --level best,opt,ultra --budget-ms 1500` (2 passes)
  - `--mode enc-mt --workers 8 --mt-workers 8 --level fastest,fast,balanced --budget-ms 1500` (1 pass)
  - `--mode enc-mt --workers 16 --mt-workers 16 --level fastest,fast,balanced --budget-ms 1500` (1 pass)
  - `--mode enc-stream --mt-workers 8 --budget-ms 1500` (1 pass)
  - `--mode dec-mt --budget-ms 1500` (1 pass)
- Budget: per-side ms, interleaved rounds, median-of-ratios verdict; totals ~45 min bench wall time.
- Caveats: ±10% run-to-run noise; slow-cell absolute MiB/s ran ~25% below the 09-11 passes (sustained-load clocks) — the x columns are interleaved same-run ratios and stay comparable; single-pass sections (enc-mt/enc-stream/dec-mt) lean on internal duplicate cells and in-run consistency only. dec-ST/enc-ST speeds below are the two-pass medians.

## T1 decode ST (bulk + streaming, 64KiB pulls; 2 passes; MiB/s of raw)

`x = ours_time / zstd_time`, <1 = we are faster. Pass spread ≤2% on all cells except json.zst1.stream (1.26-1.33) and skewed.zst3.stream (1.22-1.26).

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

## T2 decode MT scaling (solo; libzstd has no MT decode; 1 pass; MiB/s)

| file | ours ST | zstd stream ST ref | mt2 | mt4 | mt8 | mt16 |
|---|---:|---:|---:|---:|---:|---:|
| json.zst3 | 1415 | 1868 | 1224 | 1442 | 1548 | 1559 |
| text.zst3 | 10676 | 11168 | 10725 | 10624 | 10630 | 10634 |
| skewed.zst9 | 613 | 791 | 447 | 443 | 446 | 441 |
| random.zst3 | 9705 | 9013 | 9503 | 9497 | 9439 | 9465 |

Still no scaling anywhere (stage-B serial fraction); skewed.zst9 anti-scales to 0.72x ST at every width; json.zst3 best case mt16 = 1.10x ST (0.83x of the zstd ST reference); random.zst3 holds 0.98x ST = 1.05x the zstd reference. See todo item 1 (stage-B parallelization) — unchanged verdict, fresh numbers.

## T3 encode ST bulk (checksums off both sides; 2 passes; MiB/s of raw)

Output sizes and ratios are deterministic and identical across passes. Slow cells (json/skewed best/opt/ultra, n=3 rounds per pass) agreed within 2% between passes; all fast cells within 3%.

| shape.level | ours MiB/s | ours ratio | zstd MiB/s | zstd ratio | x |
|---|---:|---:|---:|---:|---:|
| json.fastest | 514 | 6.14 | 856 | 6.11 | 1.67 |
| json.fast | 400 | 5.29 | 484 | 5.29 | 1.21 |
| json.balanced | 80 | 7.11 | 122 | 5.95 | 1.51 |
| json.best | 9 | 6.75 | 36 | 6.10 | 4.10 |
| json.opt | 5 | 7.46 | 7 | 7.49 | 1.32 |
| json.ultra | 2 | 7.42 | 3 | 7.42 | 1.33 |
| text.fastest | 12547 | 309.17 | 10474 | 308.94 | 0.83 |
| text.fast | 12053 | 332.97 | 7267 | 332.90 | 0.60 |
| text.balanced | 2630 | 367.89 | 1703 | 378.41 | 0.65 |
| text.best | 417 | 404.78 | 742 | 385.90 | 1.78 |
| text.opt | 453 | 408.76 | 471 | 410.11 | 1.04 |
| text.ultra | 264 | 412.58 | 276 | 413.98 | 1.05 |
| skewed.fastest | 2913 | 2.00 | 1272 | 2.00 | 0.44 |
| skewed.fast | 186 | 1.92 | 227 | 1.92 | 1.22 |
| skewed.balanced | 1808 | 2.00 | 75 | 1.84 | 0.041 |
| skewed.best | 2 | 2.00 | 14 | 1.84 | 6.10 |
| skewed.opt | 2 | 2.00 | 3 | 2.00 | 1.50 |
| skewed.ultra | 1 | 2.00 | 2 | 2.00 | 1.40 |
| random.fastest | 1678 | 1.00 | 2182 | 1.00 | 1.30 |
| random.fast | 1675 | 1.00 | 2148 | 1.00 | 1.28 |
| random.balanced | 1658 | 1.00 | 1660 | 1.00 | 1.00 |
| random.best | 1576 | 1.00 | 255 | 1.00 | 0.16 |
| random.opt | 1585 | 1.00 | 12 | 1.00 | 0.007 |
| random.ultra | 1592 | 1.00 | 8 | 1.00 | 0.005 |
| zeros.fastest | 50414 | 32483 | 13296 | 32171 | 0.26 |
| zeros.fast | 49967 | 32483 | 8701 | 32171 | 0.17 |
| zeros.balanced | 46916 | 32483 | 1754 | 32202 | 0.037 |
| zeros.best | 43500 | 32483 | 834 | 32202 | 0.019 |
| zeros.opt | 41949 | 32483 | 937 | 32202 | 0.022 |
| zeros.ultra | 40780 | 32483 | 676 | 32202 | 0.017 |

Checksum overhead row (ours, off/on time ratio, 2 passes): json.fast 1.00/1.00, text.fast 0.90/0.90 (checksum ON is faster on text — sidecar path).

vs the 2026-09-11 tables (beyond the pairing change): json.fastest 1.78→1.67 (scan-loop work), json.fast 1.29→1.21 at ratio parity with zstd-3 (W21), json.balanced now trades speed for density (x1.24→1.51 at +19.5% density vs zstd-9, W21+row), text.balanced denser (361→368) and faster (0.87→0.65), json.opt 1.15→1.32 but vs the denser zstd-17 reference and after the C22 ring cut (solo −19.6%), random.best/opt/ultra leapfrog 6-200x (incompressibility gate, unmeasured wall until now).

## T4 encode MT bulk (checksums off; cold pool per call both sides; 1 pass; MiB/s)

Ratio preservation: our mt8/mt16 ratios sit within 0.5% of our ST on every cell; zstd-mt collapses on text (see zstd ratio column). Duplicate cells (sweep vs fixed loop) agree within 2%.

### mt8

| cell | ours | ours ratio | zstd-mt | zstd-mt ratio | x |
|---|---:|---:|---:|---:|---:|
| json.fastest.mt8 | 3181 | 6.14 | 4282 | 6.11 | 1.35 |
| json.fast.mt8 | 2264 | 5.30 | 1039 | 5.31 | 0.46 |
| json.balanced.mt8 | 235 | 7.09 | 203 | 5.95 | 0.86 |
| text.fastest.mt8 | 18829 | 308.95 | 10899 | 39.78 | 0.58 |
| text.fast.mt8 | 13207 | 332.47 | 2255 | 189.16 | 0.17 |
| text.balanced.mt8 | 1964 | 367.74 | 1260 | 378.30 | 0.64 |
| skewed.fastest.mt8 | 4351 | 2.00 | 3288 | 2.00 | 0.76 |
| skewed.fast.mt8 | 1164 | 1.92 | 644 | 1.92 | 0.56 |
| skewed.balanced.mt8 | 776 | 2.00 | 121 | 1.84 | 0.16 |

### mt16

| cell | ours | ours ratio | zstd-mt | zstd-mt ratio | x |
|---|---:|---:|---:|---:|---:|
| json.fastest.mt16 | 4459 | 6.13 | 1827 | 6.11 | 0.41 |
| json.fast.mt16 | 3190 | 5.30 | 1044 | 5.31 | 0.33 |
| json.balanced.mt16 | 211 | 7.09 | 202 | 5.95 | 0.96 |
| text.fastest.mt16 | 15978 | 308.54 | 2294 | 39.78 | 0.14 |
| text.fast.mt16 | 13333 | 332.47 | 2184 | 189.16 | 0.16 |
| text.balanced.mt16 | 1913 | 367.74 | 1254 | 378.30 | 0.66 |
| skewed.fastest.mt16 | 3793 | 2.00 | 1586 | 2.00 | 0.42 |
| skewed.fast.mt16 | 1541 | 1.92 | 641 | 1.92 | 0.42 |
| skewed.balanced.mt16 | 767 | 2.00 | 123 | 1.84 | 0.16 |

zstd warm-pool reference (context reused, json.fast mt16): 1604 MiB/s — still ~2x slower than our cold-pool mt16 (3190).

## T5 encode streaming (64KiB pulls, checksums off; 1 pass; MiB/s of raw)

> **REGRESSION found by this re-run, bisected to `946dd2f`, fixed same day** (quantized epoch grid + finish-tail re-slice + epoch-sized MADV_HUGEPAGE buffer reservation + cursor-based output serving; see [mt-stream](../perf/mt-stream.md)): the growing grid sized jobs ~1.25x apart inside one 8-job burst, so the burst barrier idled at utilization ≈ sum/(workers x max) ≈ 0.52 and every stream-MT cell ran at ~0.5x its 09-11 speed (json.fastest 961, json.fast 724, text.fastest 2049, json.best 17). The MT8 table below is the fixed re-pass (2026-09-12, budget 700ms — absolute MiB/s ±10% vs the 1.5-2s rows around it); ST streaming cells were never affected. Ratio columns shifted ≤0.1% (stream-mt only; mt8 json.ultra −0.32% denser, mt4 json/skewed balanced/best +0.05-0.09% sparser).

### ST (json/text only — harness scope)

| cell | ours | ours size | zstd | zstd size | x |
|---|---:|---:|---:|---:|---:|
| json.fastest.stream | 503 | 5459976 | 777 | 5495184 | 1.55 |
| json.fast.stream | 398 | 6339383 | 441 | 6389264 | 1.11 |
| json.best.stream | 8 | 4972828 | 35 | 5500578 | 4.17 |
| text.fastest.stream | 6812 | 108531 | 1774 | 1838306 | 0.26 |
| text.fast.stream | 6700 | 100772 | 5587 | 100794 | 0.83 |
| text.best.stream | 268 | 82895 | 690 | 87011 | 2.58 |

Note: unknown-size streaming text.fastest — zstd ratio collapses to 18.3 (1.84MB output) while we hold 309 (108KB), a 17x ratio + 3.8x speed double win; json.best.stream matches its bulk speed (9), i.e. bounded by the Best core, not the pipeline.

### MT8 (json/text only — harness scope)

| cell | ours | ours size | zstd | zstd size | x |
|---|---:|---:|---:|---:|---:|
| json.fastest.stream-mt8 | 1423 | 5461337 | 1761 | 5488119 | 1.26 |
| json.fast.stream-mt8 | 1248 | 6334849 | 400 | 6324934 | 0.32 |
| json.balanced.stream-mt8 | 247 | 4734402 | 114 | 5636749 | 0.46 |
| json.best.stream-mt8 | 16 | 4970453 | 52 | 5498839 | 3.2 |
| text.fastest.stream-mt8 | 2363 | 108968 | 4258 | 843554 | 1.80 |
| text.fast.stream-mt8 | 2279 | 100921 | 1732 | 177384 | 0.76 |
| text.balanced.stream-mt8 | 1546 | 91244 | 998 | 88698 | 0.65 |
| text.best.stream-mt8 | 208 | 82935 | 365 | 86976 | 1.75 |

Bulk-mt8 ceilings (ours, this run's T4): json fastest/fast/balanced/best = 3257/2336/264/19 MiB/s, text = 20150/15210/2204/226. Stream-MT8 reaches 44/53/94/84% (json) and 12/15/70/92% (text) of its own bulk ceiling — pre-regression (09-11) json was 61/61/80 and text 24/19/46/…; the json fast/balanced and text balanced/best tiers now sit at or above their 09-11 ceiling fractions, while text.fastest (12%) and json.fastest (44%) are bounded by the burst model's serialized accumulate/spawn/barrier (the overlapped producer-consumer pool is the persistent-pool redesign, todo 12).

## Compression-ratio sweep (`zstdx-bench ratio`; sizes, not speeds)

Re-run 2026-09-12 at `0e7044d` (includes W21/W22-23 windows, the level ladder, and the Opt C22 ring). Conclusions in [snapshot.md](snapshot.md).

- Command: `./target/release/zstdx-bench ratio` at defaults — parallel 8, mt-workers 4, checksums off on both sides, streaming without a pledged size. Wall 141 s; one deterministic pass per cell.
- Same machine/toolchain/corpus as the provenance above; cells 5 shapes x 6 ladder levels (paired with libzstd **1/3/9/13/17/19**, numeric) x `bulk-st`/`bulk-mt`/`stream-st`/`stream-mt` = 120; every zstdx output roundtrip-gated through both decoders before being reported.
- Δ% = ours ratio ÷ zstd ratio − 1 (+ = denser than libzstd); summary geo-means average the (1+Δ) factors. Geo-mean Δ over 120 cells **+9.02%** (modes: bulk-st +1.73 / bulk-mt +11.42 / stream-st +11.81 / stream-mt +11.46; shapes: json +4.79 / text +40.20 / skewed +2.76 / random +0.00 / zeros +2.00); worst cell text.balanced.bulk-mt −2.79%.

| cell | zstdx | ratio | zstd | ratio | Δ% |
|---|---:|---:|---:|---:|---:|
| json.fastest.bulk-st | 5466381 | 6.138 | 5495185 | 6.106 | +0.53% |
| json.fastest.bulk-mt | 5466721 | 6.138 | 5488123 | 6.114 | +0.39% |
| json.fastest.stream-st | 5459976 | 6.146 | 5495184 | 6.106 | +0.64% |
| json.fastest.stream-mt | 5460551 | 6.145 | 5488119 | 6.114 | +0.50% |
| json.fast.bulk-st | 6339383 | 5.293 | 6339220 | 5.293 | -0.00% |
| json.fast.bulk-mt | 6337229 | 5.295 | 6324938 | 5.305 | -0.19% |
| json.fast.stream-st | 6339383 | 5.293 | 6389264 | 5.252 | +0.79% |
| json.fast.stream-mt | 6337087 | 5.295 | 6324934 | 5.305 | -0.19% |
| json.balanced.bulk-st | 4720677 | 7.108 | 5639335 | 5.950 | +19.46% |
| json.balanced.bulk-mt | 4734315 | 7.088 | 5636753 | 5.953 | +19.06% |
| json.balanced.stream-st | 4720677 | 7.108 | 5639351 | 5.950 | +19.46% |
| json.balanced.stream-mt | 4730731 | 7.093 | 5636749 | 5.953 | +19.15% |
| json.best.bulk-st | 4972828 | 6.748 | 5500580 | 6.100 | +10.61% |
| json.best.bulk-mt | 4970393 | 6.751 | 5498843 | 6.102 | +10.63% |
| json.best.stream-st | 4972828 | 6.748 | 5500578 | 6.100 | +10.61% |
| json.best.stream-mt | 4967862 | 6.754 | 5498839 | 6.102 | +10.69% |
| json.opt.bulk-st | 4495372 | 7.464 | 4481382 | 7.488 | -0.31% |
| json.opt.bulk-mt | 4489412 | 7.474 | 4481379 | 7.488 | -0.18% |
| json.opt.stream-st | 4495372 | 7.464 | 4482274 | 7.486 | -0.29% |
| json.opt.stream-mt | 4489410 | 7.474 | 4481375 | 7.488 | -0.18% |
| json.ultra.bulk-st | 4522377 | 7.420 | 4522548 | 7.419 | +0.00% |
| json.ultra.bulk-mt | 4528524 | 7.410 | 4522546 | 7.419 | -0.13% |
| json.ultra.stream-st | 4522377 | 7.420 | 4524530 | 7.416 | +0.05% |
| json.ultra.stream-mt | 4528521 | 7.410 | 4522542 | 7.419 | -0.13% |
| text.fastest.bulk-st | 108531 | 309.169 | 108613 | 308.936 | +0.08% |
| text.fastest.bulk-mt | 108568 | 309.064 | 843555 | 39.777 | +676.98% |
| text.fastest.stream-st | 108531 | 309.169 | 1838306 | 18.253 | +1593.81% |
| text.fastest.stream-mt | 109126 | 307.483 | 843554 | 39.777 | +673.01% |
| text.fast.bulk-st | 100772 | 332.974 | 100795 | 332.898 | +0.02% |
| text.fast.bulk-mt | 100809 | 332.852 | 177385 | 189.162 | +75.96% |
| text.fast.stream-st | 100772 | 332.974 | 100794 | 332.901 | +0.02% |
| text.fast.stream-mt | 100894 | 332.571 | 177384 | 189.163 | +75.81% |
| text.balanced.bulk-st | 91208 | 367.889 | 88673 | 378.406 | -2.78% |
| text.balanced.bulk-mt | 91245 | 367.740 | 88699 | 378.295 | -2.79% |
| text.balanced.stream-st | 91208 | 367.889 | 88722 | 378.197 | -2.73% |
| text.balanced.stream-mt | 91232 | 367.792 | 88698 | 378.300 | -2.78% |
| text.best.bulk-st | 82895 | 404.782 | 86951 | 385.900 | +4.89% |
| text.best.bulk-mt | 82936 | 404.582 | 86977 | 385.785 | +4.87% |
| text.best.stream-st | 82895 | 404.782 | 87011 | 385.634 | +4.97% |
| text.best.stream-mt | 82914 | 404.690 | 86976 | 385.790 | +4.90% |
| text.opt.bulk-st | 82088 | 408.762 | 81819 | 410.106 | -0.33% |
| text.opt.bulk-mt | 82125 | 408.578 | 81840 | 410.000 | -0.35% |
| text.opt.stream-st | 82088 | 408.762 | 81841 | 409.995 | -0.30% |
| text.opt.stream-mt | 82121 | 408.597 | 81839 | 410.005 | -0.34% |
| text.ultra.bulk-st | 81329 | 412.576 | 81054 | 413.976 | -0.34% |
| text.ultra.bulk-mt | 81312 | 412.663 | 81075 | 413.869 | -0.29% |
| text.ultra.stream-st | 81329 | 412.576 | 81076 | 413.864 | -0.31% |
| text.ultra.stream-mt | 81308 | 412.683 | 81074 | 413.874 | -0.29% |
| skewed.fastest.bulk-st | 16782205 | 1.999 | 16782141 | 1.999 | -0.00% |
| skewed.fastest.bulk-mt | 16782788 | 1.999 | 16782365 | 1.999 | -0.00% |
| skewed.fastest.stream-st | 16782177 | 1.999 | 16782140 | 1.999 | -0.00% |
| skewed.fastest.stream-mt | 16782763 | 1.999 | 16782364 | 1.999 | -0.00% |
| skewed.fast.bulk-st | 17467569 | 1.921 | 17471754 | 1.920 | +0.02% |
| skewed.fast.bulk-mt | 17473074 | 1.920 | 17470648 | 1.921 | -0.01% |
| skewed.fast.stream-st | 17467569 | 1.921 | 17471753 | 1.920 | +0.02% |
| skewed.fast.stream-mt | 17473088 | 1.920 | 17470647 | 1.921 | -0.01% |
| skewed.balanced.bulk-st | 16782213 | 1.999 | 18263376 | 1.837 | +8.83% |
| skewed.balanced.bulk-mt | 16785794 | 1.999 | 18258359 | 1.838 | +8.77% |
| skewed.balanced.stream-st | 16782213 | 1.999 | 18263380 | 1.837 | +8.83% |
| skewed.balanced.stream-mt | 16785078 | 1.999 | 18258358 | 1.838 | +8.78% |
| skewed.best.bulk-st | 16810658 | 1.996 | 18210558 | 1.843 | +8.33% |
| skewed.best.bulk-mt | 16816630 | 1.995 | 18207355 | 1.843 | +8.27% |
| skewed.best.stream-st | 16810658 | 1.996 | 18210568 | 1.843 | +8.33% |
| skewed.best.stream-mt | 16814663 | 1.996 | 18207354 | 1.843 | +8.28% |
| skewed.opt.bulk-st | 16812215 | 1.996 | 16802359 | 1.997 | -0.06% |
| skewed.opt.bulk-mt | 16813574 | 1.996 | 16802359 | 1.997 | -0.07% |
| skewed.opt.stream-st | 16812215 | 1.996 | 16802357 | 1.997 | -0.06% |
| skewed.opt.stream-mt | 16813570 | 1.996 | 16802358 | 1.997 | -0.07% |
| skewed.ultra.bulk-st | 16814597 | 1.996 | 16807639 | 1.996 | -0.04% |
| skewed.ultra.bulk-mt | 16813905 | 1.996 | 16807639 | 1.996 | -0.04% |
| skewed.ultra.stream-st | 16814597 | 1.996 | 16807649 | 1.996 | -0.04% |
| skewed.ultra.stream-mt | 16813901 | 1.996 | 16807638 | 1.996 | -0.04% |
| random.fastest.bulk-st | 33555209 | 1.000 | 33555210 | 1.000 | +0.00% |
| random.fastest.bulk-mt | 33555210 | 1.000 | 33555210 | 1.000 | +0.00% |
| random.fastest.stream-st | 33555209 | 1.000 | 33555209 | 1.000 | +0.00% |
| random.fastest.stream-mt | 33555209 | 1.000 | 33555209 | 1.000 | +0.00% |
| random.fast.bulk-st | 33555209 | 1.000 | 33555210 | 1.000 | +0.00% |
| random.fast.bulk-mt | 33555210 | 1.000 | 33555210 | 1.000 | +0.00% |
| random.fast.stream-st | 33555209 | 1.000 | 33555209 | 1.000 | +0.00% |
| random.fast.stream-mt | 33555206 | 1.000 | 33555209 | 1.000 | +0.00% |
| random.balanced.bulk-st | 33555209 | 1.000 | 33555210 | 1.000 | +0.00% |
| random.balanced.bulk-mt | 33555210 | 1.000 | 33555210 | 1.000 | +0.00% |
| random.balanced.stream-st | 33555209 | 1.000 | 33555209 | 1.000 | +0.00% |
| random.balanced.stream-mt | 33555206 | 1.000 | 33555209 | 1.000 | +0.00% |
| random.best.bulk-st | 33555209 | 1.000 | 33555210 | 1.000 | +0.00% |
| random.best.bulk-mt | 33555210 | 1.000 | 33555210 | 1.000 | +0.00% |
| random.best.stream-st | 33555209 | 1.000 | 33555209 | 1.000 | +0.00% |
| random.best.stream-mt | 33555206 | 1.000 | 33555209 | 1.000 | +0.00% |
| random.opt.bulk-st | 33555209 | 1.000 | 33555210 | 1.000 | +0.00% |
| random.opt.bulk-mt | 33555210 | 1.000 | 33555210 | 1.000 | +0.00% |
| random.opt.stream-st | 33555209 | 1.000 | 33555209 | 1.000 | +0.00% |
| random.opt.stream-mt | 33555206 | 1.000 | 33555209 | 1.000 | +0.00% |
| random.ultra.bulk-st | 33555209 | 1.000 | 33555210 | 1.000 | +0.00% |
| random.ultra.bulk-mt | 33555210 | 1.000 | 33555210 | 1.000 | +0.00% |
| random.ultra.stream-st | 33555209 | 1.000 | 33555209 | 1.000 | +0.00% |
| random.ultra.stream-mt | 33555206 | 1.000 | 33555209 | 1.000 | +0.00% |
| zeros.fastest.bulk-st | 1033 | 32483 | 1043 | 32171 | +0.97% |
| zeros.fastest.bulk-mt | 1034 | 32451 | 1148 | 29229 | +11.03% |
| zeros.fastest.stream-st | 1033 | 32483 | 1042 | 32202 | +0.87% |
| zeros.fastest.stream-mt | 1034 | 32451 | 1147 | 29254 | +10.93% |
| zeros.fast.bulk-st | 1033 | 32483 | 1043 | 32171 | +0.97% |
| zeros.fast.bulk-mt | 1034 | 32451 | 1064 | 31536 | +2.90% |
| zeros.fast.stream-st | 1033 | 32483 | 1042 | 32202 | +0.87% |
| zeros.fast.stream-mt | 1030 | 32577 | 1063 | 31566 | +3.20% |
| zeros.balanced.bulk-st | 1033 | 32483 | 1042 | 32202 | +0.87% |
| zeros.balanced.bulk-mt | 1034 | 32451 | 1049 | 31987 | +1.45% |
| zeros.balanced.stream-st | 1033 | 32483 | 1041 | 32233 | +0.77% |
| zeros.balanced.stream-mt | 1030 | 32577 | 1048 | 32018 | +1.75% |
| zeros.best.bulk-st | 1033 | 32483 | 1043 | 32171 | +0.97% |
| zeros.best.bulk-mt | 1034 | 32451 | 1050 | 31957 | +1.55% |
| zeros.best.stream-st | 1033 | 32483 | 1042 | 32202 | +0.87% |
| zeros.best.stream-mt | 1030 | 32577 | 1049 | 31987 | +1.84% |
| zeros.opt.bulk-st | 1033 | 32483 | 1042 | 32202 | +0.87% |
| zeros.opt.bulk-mt | 1034 | 32451 | 1042 | 32202 | +0.77% |
| zeros.opt.stream-st | 1033 | 32483 | 1041 | 32233 | +0.77% |
| zeros.opt.stream-mt | 1030 | 32577 | 1041 | 32233 | +1.07% |
| zeros.ultra.bulk-st | 1033 | 32483 | 1042 | 32202 | +0.87% |
| zeros.ultra.bulk-mt | 1034 | 32451 | 1042 | 32202 | +0.77% |
| zeros.ultra.stream-st | 1033 | 32483 | 1041 | 32233 | +0.77% |
| zeros.ultra.stream-mt | 1030 | 32577 | 1041 | 32233 | +1.07% |

## Coverage gaps (not run 2026-09-12, or harness does not expose)

- dec-st: random/zeros only at zst3 (harness curated set omits zst1/zst9 as redundant).
- dec-mt: json.zst1/zst9, text.zst1/zst9, skewed.zst1/zst3, zeros; worker counts beyond 16.
- enc-mt: harness fixed-worker loop is hardcoded to json/text/skewed x fastest/fast/balanced — random/zeros and best/opt/ultra MT cells do not exist; mt32 not run.
- enc-stream: harness covers json/text only (no skewed/random/zeros); ST streaming has no balanced cell; MT streaming only at mt8 (no mt16).
- Streaming decode exercised only via the 64KiB-pull read path; no write-path (`io::Write`) decode bench in the matrix.
- dec-mt / enc-mt / enc-stream ran once (no second pass); dec-st and enc-st ran twice.
- The gaps above are speed-coverage only: output sizes for every shape x level x mt/stream cell (including random/zeros and best/opt/ultra) are covered by the ratio sweep section.
- Unchanged methodology-level gaps: dictionaries, small-payload matrix, MT decode of our own MT-encoded output, >32MB inputs (dll100 lives in [matchers](../perf/matchers.md) A/Bs), full 1-22 ladder (release-only `matrix --full-ladder`).
