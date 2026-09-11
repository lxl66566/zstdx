# Bench matrix · fresh raw data (2026-09-11)

> Full re-run of the matrix sections listed below at `96867c5`. Conclusions live in [snapshot.md](snapshot.md). Pre-2026-09-11 archives (old sections A-E: `b1dd010` matrix, `4ff2b7b` revisions, `27b91cf` stream-mt, ENCPERF, early rounds) were dropped from this page; retrieve them from git history of this file at `96867c5` if needed.

## Provenance

- commit `96867c58b24ec65d9de2d67bd102858672713813`, date 2026-09-11, branch dev, tree otherwise clean.
- CPU: AMD Eng Sample 100-000000870-32_Y (Zen4-class, 32 cores visible, AVX-512/BMI2), max clock 5386 MHz.
- rustc 1.100.0-nightly (8925ea358 2026-08-20), release profile.
- Reference side: zstd crate 0.13.3 (Cargo.lock; Cargo.toml declares 0.13.2) over zstd-sys 2.1.0+zstd.1.5.7 → libzstd 1.5.7, `zstdmt` enabled. Harness header prints `libzstd 1.5.7, binding 10507`.
- Corpus: `bench/corpus` generated 2026-09-10, 32MiB x 5 shapes; decode uses zstd-CLI-precompressed zst1/zst3/zst9.
- Roundtrip verification gates ON for every cell (decode and encode, ST and MT).
- Commands (all via `./target/release/zstdx-bench matrix`):
  - `--mode dec-st --budget-ms 2000` (2 passes)
  - `--mode enc-st --level fastest,fast,balanced --budget-ms 1500` (2 passes)
  - `--mode enc-st --level best,opt,ultra --budget-ms 1500` (2 passes)
  - `--mode enc-mt --workers 8 --mt-workers 8 --level fastest,fast,balanced --budget-ms 1500` (1 pass)
  - `--mode enc-mt --workers 16 --mt-workers 16 --level fastest,fast,balanced --budget-ms 1500` (1 pass)
  - `--mode enc-stream --mt-workers 8 --budget-ms 1500` (1 pass)
  - `--mode dec-mt --budget-ms 1500` (1 pass)
- Budget: per-side ms, interleaved rounds, median-of-ratios verdict; totals ~36 min bench wall time.
- Caveats: ±10% run-to-run noise; sustained-load clocks well below max boost (prior runs ~70-77% of max); single-pass sections (enc-mt/enc-stream/dec-mt) lean on internal duplicate cells and in-run consistency only.

## T1 decode ST (bulk + streaming, 64KiB pulls; 2 passes, speeds are pass medians; MiB/s of raw)

`x = ours_time / zstd_time`, <1 = we are faster. Pass spread ≤2% on all cells except skewed.zst1.stream (1.09-1.13) and json.zst1.bulk (0.67-0.69).

| file | bulk ours | bulk zstd | bulk x | stream ours | stream zstd | stream x |
|---|---:|---:|---:|---:|---:|---:|
| json.zst1 | 1738 | 1186 | 0.68 | 1736 | 2190 | 1.26 |
| json.zst3 | 1428 | 1072 | 0.75 | 1380 | 1872 | 1.36 |
| json.zst9 | 1720 | 1144 | 0.67 | 1646 | 2138 | 1.30 |
| text.zst1 | 5428 | 2007 | 0.37 | 6230 | 7583 | 1.22 |
| text.zst3 | 8226 | 2222 | 0.27 | 10418 | 11105 | 1.07 |
| text.zst9 | 9003 | 2284 | 0.25 | 11588 | 12099 | 1.04 |
| skewed.zst1 | 2268 | 1420 | 0.63 | 2546 | 2824 | 1.11 |
| skewed.zst3 | 1230 | 986 | 0.80 | 1260 | 1526 | 1.21 |
| skewed.zst9 | 636 | 638 | 1.00 | 602 | 788 | 1.31 |
| random.zst3 | 8688 | 2034 | 0.23 | 10872 | 8706 | 0.80 |
| zeros.zst3 | 11157 | 2292 | 0.21 | 12851 | 12896 | 1.00 |

## T2 decode MT scaling (solo; libzstd has no MT decode; 1 pass; MiB/s)

| file | ours ST | zstd stream ST ref | mt2 | mt4 | mt8 | mt16 |
|---|---:|---:|---:|---:|---:|---:|
| json.zst3 | 1359 | 1879 | 1323 | 1648 | 1549 | 1747 |
| text.zst3 | 10681 | 11181 | 10285 | 10250 | 9955 | 10314 |
| skewed.zst9 | 637 | 788 | 438 | 441 | 441 | 438 |
| random.zst3 | 9720 | 8777 | 9511 | 9523 | 9507 | 9445 |

No scaling anywhere; skewed.zst9 anti-scales to 0.69x ST at every worker count; json.zst3 best case mt16 = 1.29x ST (still 0.93x of the zstd ST reference).

## T3 encode ST bulk (checksums off both sides; 2 passes, speeds are pass medians; MiB/s of raw)

> 2026-09-12: the fast/balanced rows below predate the W21 window change — json.fast is now ~387 MiB/s at libzstd-parity ratio, text.fast ~12045, json.balanced ~87-92 at +23.7% density (5516920→4711529), text.balanced ~2847; run `matrix --mode enc-st` for current numbers.

Output sizes and ratios are deterministic and identical across passes. Slowest cells (json/skewed best/opt/ultra, n=3 rounds per pass) showed pass-to-pass x spread up to ~10% (json.ultra 0.69/0.73, skewed.best 3.68/3.36, random.best 30.8/27.6); all fast cells within 3%.

| shape.level | ours MiB/s | ours ratio | zstd MiB/s | zstd ratio | x |
|---|---:|---:|---:|---:|---:|
| json.fastest | 477 | 6.15 | 852 | 6.11 | 1.78 |
| json.fast | 371 | 5.36 | 480 | 5.29 | 1.29 |
| json.balanced | 149 | 6.08 | 185 | 5.76 | 1.24 |
| json.best | 12 | 6.83 | 54 | 6.08 | 4.52 |
| json.opt | 8 | 7.46 | 10 | 7.10 | 1.15 |
| json.ultra | 4 | 7.44 | 3 | 7.42 | 0.71 |
| text.fastest | 11279 | 309.17 | 10455 | 308.94 | 0.93 |
| text.fast | 10821 | 332.97 | 7189 | 332.90 | 0.67 |
| text.balanced | 3168 | 361.45 | 2748 | 370.17 | 0.87 |
| text.best | 358 | 404.78 | 872 | 383.85 | 2.44 |
| text.opt | 470 | 408.76 | 528 | 406.09 | 1.12 |
| text.ultra | 290 | 411.58 | 266 | 413.98 | 0.92 |
| skewed.fastest | 2665 | 2.00 | 1210 | 2.00 | 0.45 |
| skewed.fast | 164 | 1.92 | 225 | 1.92 | 1.37 |
| skewed.balanced | 1935 | 2.00 | 105 | 1.86 | 0.055 |
| skewed.best | 6 | 2.00 | 24 | 1.84 | 3.52 |
| skewed.opt | 7 | 2.00 | 8 | 2.00 | 1.10 |
| skewed.ultra | 4 | 2.00 | 2 | 2.00 | 0.44 |
| random.fastest | 1482 | 1.00 | 1966 | 1.00 | 1.33 |
| random.fast | 1291 | 1.00 | 1841 | 1.00 | 1.43 |
| random.balanced | 1284 | 1.00 | 1639 | 1.00 | 1.27 |
| random.best | 22 | 1.00 | 632 | 1.00 | 29.2 |
| random.opt | 22 | 1.00 | 21 | 1.00 | 0.96 |
| random.ultra | 16 | 1.00 | 8 | 1.00 | 0.46 |
| zeros.fastest | 49130 | 32483 | 13278 | 32171 | 0.27 |
| zeros.fast | 48518 | 32483 | 8650 | 32171 | 0.18 |
| zeros.balanced | 46475 | 32483 | 2779 | 32202 | 0.060 |
| zeros.best | 42649 | 32483 | 959 | 32202 | 0.023 |
| zeros.opt | 43128 | 32483 | 1050 | 32202 | 0.024 |
| zeros.ultra | 40880 | 32483 | 649 | 32202 | 0.016 |

Checksum overhead row (ours, A/B = off/on time ratio, 2 passes): json.fast 1.00/1.03, text.fast 0.93/0.91 (checksum ON is faster on text — sidecar path).

Historical context (labeled, from the 2026-09-10 matrix @ `4ff2b7b`, not from this run): Best-level speed used to lead (json.Best x0.69, text.Best x0.48, skewed.Best x0.67, random.Best x0.49); the opt-parser Best core (`b39a192`) inverted that to the x2.4-29 losses above while buying ratio (json.Best 5.70→6.83, text.Best 363→405). skewed.Balanced went 44→1935 MiB/s (x2.40→0.055) via the `a37ebaa` prefill/gain-gate work; json.Balanced x1.41→1.24.

## T4 encode MT bulk (checksums off; cold pool per call both sides; 1 pass; MiB/s)

Ratio preservation: our mt8/mt16 ratios sit within 0.5% of our ST on every cell; zstd-mt collapses on text (see zstd ratio column). Duplicate cells (sweep vs fixed loop) agree within 2%.

### mt8

| cell | ours | ours ratio | zstd-mt | zstd-mt ratio | x |
|---|---:|---:|---:|---:|---:|
| json.fastest.mt8 | 3025 | 6.14 | 4121 | 6.11 | 1.37 |
| json.fast.mt8 | 2306 | 5.36 | 1042 | 5.31 | 0.45 |
| json.balanced.mt8 | 560 | 6.08 | 541 | 5.76 | 0.98 |
| text.fastest.mt8 | 18218 | 308.68 | 11109 | 39.78 | 0.61 |
| text.fast.mt8 | 16014 | 332.72 | 2040 | 189.16 | 0.13 |
| text.balanced.mt8 | 2691 | 361.16 | 1640 | 212.55 | 0.61 |
| skewed.fastest.mt8 | 3904 | 2.00 | 3027 | 2.00 | 0.77 |
| skewed.fast.mt8 | 1055 | 1.92 | 618 | 1.92 | 0.58 |
| skewed.balanced.mt8 | 1656 | 2.00 | 345 | 1.86 | 0.21 |

### mt16

| cell | ours | ours ratio | zstd-mt | zstd-mt ratio | x |
|---|---:|---:|---:|---:|---:|
| json.fastest.mt16 | 4414 | 6.14 | 1642 | 6.11 | 0.38 |
| json.fast.mt16 | 3203 | 5.37 | 985 | 5.31 | 0.31 |
| json.balanced.mt16 | 532 | 6.09 | 531 | 5.76 | 1.00 |
| text.fastest.mt16 | 16392 | 307.82 | 1980 | 39.78 | 0.12 |
| text.fast.mt16 | 13751 | 332.43 | 1921 | 189.16 | 0.14 |
| text.balanced.mt16 | 1229 | 360.85 | 1582 | 212.55 | 1.28 |
| skewed.fastest.mt16 | 3365 | 2.00 | 1378 | 2.00 | 0.41 |
| skewed.fast.mt16 | 1569 | 1.92 | 613 | 1.92 | 0.39 |
| skewed.balanced.mt16 | 762 | 2.00 | 345 | 1.86 | 0.46 |

zstd warm-pool reference (context reused, json.fast mt16): 1608 / 1597 MiB/s in the mt8/mt16 runs — still ~2x slower than our cold-pool mt16 (3203).

## T5 encode streaming (64KiB pulls, checksums off; 1 pass; MiB/s of raw)

### ST (json/text only — harness scope)

| cell | ours | ours size | zstd | zstd size | x |
|---|---:|---:|---:|---:|---:|
| json.fastest.stream | 475 | 5459976 | 776 | 5495184 | 1.64 |
| json.fast.stream | 374 | 6262491 | 440 | 6389264 | 1.18 |
| json.best.stream | 12 | 4912296 | 48 | 5517459 | 4.00 |
| text.fastest.stream | 6010 | 108531 | 1767 | 1838306 | 0.29 |
| text.fast.stream | 6481 | 100772 | 5463 | 100794 | 0.84 |
| text.best.stream | 340 | 82895 | 799 | 87486 | 2.36 |

Note: unknown-size streaming text.fastest — zstd ratio collapses to 18.3 (1.84MB output) while we hold 309 (108KB), a 17x ratio + 3.4x speed double win; json.best.stream matches its bulk speed (12), i.e. bounded by the Best core, not the pipeline.

### MT8 (json/text only — harness scope)

| cell | ours | ours ratio/size | zstd | zstd ratio/size | x |
|---|---:|---:|---:|---:|---:|
| json.fastest.stream-mt8 | 1865 | 5461734 | 1921 | 5488119 | 1.03 |
| json.fast.stream-mt8 | 1433 | 6253606 | 391 | 6324934 | 0.27 |
| json.balanced.stream-mt8 | 450 | 5511666 | 222 | 5821602 | 0.50 |
| json.best.stream-mt8 | 32 | 4906807 | 82 | 5515957 | 2.58 |
| text.fastest.stream-mt8 | 4459 | 109007 | 4481 | 843554 | 1.00 |
| text.fast.stream-mt8 | 3676 | 100936 | 1543 | 177384 | 0.42 |
| text.balanced.stream-mt8 | 1273 | 92985 | 1213 | 157867 | 0.96 |
| text.best.stream-mt8 | 275 | 82994 | 631 | 87441 | 2.30 |

Bulk-mt8 ceilings (ours, solo reference over the same bytes): json fastest/fast/balanced/best = 3058/2333/566/41 MiB/s; text = 18839/19805/2744/437. Stream-MT8 reaches 61/61/80/78% (json) and 24/19/46/63% (text) of its own bulk ceiling.

## Compression-ratio sweep (`zstdx-bench ratio`; sizes, not speeds)

First full sweep 2026-09-11, landed together with the tool in this file's introducing commit (see `git log -- docs/src/dev/bench/matrix.md`). Conclusions in [snapshot.md](snapshot.md).

- Command: `./target/release/zstdx-bench ratio` at defaults — parallel 8, mt-workers 4, checksums off on both sides, streaming without a pledged size. Wall 94 s; one deterministic pass per cell; a control re-run was byte-identical on every size.
- Same machine/toolchain as the provenance above (Zen4-class 32C, rustc 1.100.0-nightly, zstd crate 0.13.3 / libzstd 1.5.7 `zstdmt`); corpus 32 MiB × 5 shapes (json raw 33554523, others 33554432).
- Cells: 5 shapes × 6 ladder levels (paired with libzstd 1/3/6/12/16/19) × `bulk-st`/`bulk-mt`/`stream-st`/`stream-mt` = 120; every zstdx output roundtrip-gated through both decoders before being reported.
- Δ% = ours ratio ÷ zstd ratio − 1 (+ = denser than libzstd); summary geo-means average the (1+Δ) factors.

| cell | zstdx | ratio | zstd | ratio | Δ% |
|---|---:|---:|---:|---:|---:|
| json.fastest.bulk-st | 5459976 | 6.146 | 5495185 | 6.106 | +0.64% |
| json.fastest.bulk-mt | 5460123 | 6.145 | 5488123 | 6.114 | +0.51% |
| json.fastest.stream-st | 5459976 | 6.146 | 5495184 | 6.106 | +0.64% |
| json.fastest.stream-mt | 5461734 | 6.144 | 5488119 | 6.114 | +0.48% |
| json.fast.bulk-st | 6262491 | 5.358 | 6339220 | 5.293 | +1.23% |
| json.fast.bulk-mt | 6260342 | 5.360 | 6324938 | 5.305 | +1.03% |
| json.fast.stream-st | 6262491 | 5.358 | 6389264 | 5.252 | +2.02% |
| json.fast.stream-mt | 6253606 | 5.366 | 6324934 | 5.305 | +1.14% |
| json.balanced.bulk-st | 5516920 | 6.082 | 5829475 | 5.756 | +5.67% |
| json.balanced.bulk-mt | 5516167 | 6.083 | 5821606 | 5.764 | +5.54% |
| json.balanced.stream-st | 5516920 | 6.082 | 5829477 | 5.756 | +5.67% |
| json.balanced.stream-mt | 5511666 | 6.088 | 5821602 | 5.764 | +5.62% |
| json.best.bulk-st | 4912296 | 6.831 | 5517458 | 6.082 | +12.32% |
| json.best.bulk-mt | 4908568 | 6.836 | 5515961 | 6.083 | +12.37% |
| json.best.stream-st | 4912296 | 6.831 | 5517459 | 6.082 | +12.32% |
| json.best.stream-mt | 4906807 | 6.838 | 5515957 | 6.083 | +12.41% |
| json.opt.bulk-st | 4498011 | 7.460 | 4725236 | 7.101 | +5.05% |
| json.opt.bulk-mt | 4478237 | 7.493 | 4689383 | 7.155 | +4.71% |
| json.opt.stream-st | 4498011 | 7.460 | 4660887 | 7.199 | +3.62% |
| json.opt.stream-mt | 4501099 | 7.455 | 4689379 | 7.155 | +4.18% |
| json.ultra.bulk-st | 4509801 | 7.440 | 4522548 | 7.419 | +0.28% |
| json.ultra.bulk-mt | 4500007 | 7.457 | 4522546 | 7.419 | +0.50% |
| json.ultra.stream-st | 4509801 | 7.440 | 4524530 | 7.416 | +0.33% |
| json.ultra.stream-mt | 4517412 | 7.428 | 4522542 | 7.419 | +0.11% |
| text.fastest.bulk-st | 108531 | 309.169 | 108613 | 308.936 | +0.08% |
| text.fastest.bulk-mt | 108568 | 309.064 | 843555 | 39.777 | +676.98% |
| text.fastest.stream-st | 108531 | 309.169 | 1838306 | 18.253 | +1593.81% |
| text.fastest.stream-mt | 109007 | 307.819 | 843554 | 39.777 | +673.85% |
| text.fast.bulk-st | 100772 | 332.974 | 100795 | 332.898 | +0.02% |
| text.fast.bulk-mt | 100809 | 332.852 | 177385 | 189.162 | +75.96% |
| text.fast.stream-st | 100772 | 332.974 | 100794 | 332.901 | +0.02% |
| text.fast.stream-mt | 100936 | 332.433 | 177384 | 189.163 | +75.74% |
| text.balanced.bulk-st | 92833 | 361.449 | 90647 | 370.166 | -2.35% |
| text.balanced.bulk-mt | 92870 | 361.305 | 157868 | 212.547 | +69.99% |
| text.balanced.stream-st | 92833 | 361.449 | 90766 | 369.681 | -2.23% |
| text.balanced.stream-mt | 92985 | 360.859 | 157867 | 212.549 | +69.78% |
| text.best.bulk-st | 82895 | 404.782 | 87416 | 383.848 | +5.45% |
| text.best.bulk-mt | 82917 | 404.675 | 87442 | 383.734 | +5.46% |
| text.best.stream-st | 82895 | 404.782 | 87486 | 383.541 | +5.54% |
| text.best.stream-mt | 82994 | 404.299 | 87441 | 383.738 | +5.36% |
| text.opt.bulk-st | 82088 | 408.762 | 82629 | 406.085 | +0.66% |
| text.opt.bulk-mt | 82113 | 408.637 | 82655 | 405.958 | +0.66% |
| text.opt.stream-st | 82088 | 408.762 | 82653 | 405.968 | +0.69% |
| text.opt.stream-mt | 82182 | 408.294 | 82654 | 405.963 | +0.57% |
| text.ultra.bulk-st | 81525 | 411.585 | 81054 | 413.976 | -0.58% |
| text.ultra.bulk-mt | 81548 | 411.468 | 81075 | 413.869 | -0.58% |
| text.ultra.stream-st | 81525 | 411.585 | 81076 | 413.864 | -0.55% |
| text.ultra.stream-mt | 81617 | 411.121 | 81074 | 413.874 | -0.67% |
| skewed.fastest.bulk-st | 16782177 | 1.999 | 16782141 | 1.999 | -0.00% |
| skewed.fastest.bulk-mt | 16782569 | 1.999 | 16782365 | 1.999 | -0.00% |
| skewed.fastest.stream-st | 16782177 | 1.999 | 16782140 | 1.999 | -0.00% |
| skewed.fastest.stream-mt | 16784123 | 1.999 | 16782364 | 1.999 | -0.01% |
| skewed.fast.bulk-st | 17466858 | 1.921 | 17471754 | 1.920 | +0.03% |
| skewed.fast.bulk-mt | 17471115 | 1.921 | 17470648 | 1.921 | -0.00% |
| skewed.fast.stream-st | 17466858 | 1.921 | 17471753 | 1.920 | +0.03% |
| skewed.fast.stream-mt | 17487109 | 1.919 | 17470647 | 1.921 | -0.09% |
| skewed.balanced.bulk-st | 16782199 | 1.999 | 18045761 | 1.859 | +7.53% |
| skewed.balanced.bulk-mt | 16782942 | 1.999 | 18039364 | 1.860 | +7.49% |
| skewed.balanced.stream-st | 16782199 | 1.999 | 18045765 | 1.859 | +7.53% |
| skewed.balanced.stream-mt | 16785257 | 1.999 | 18039363 | 1.860 | +7.47% |
| skewed.best.bulk-st | 16810482 | 1.996 | 18197895 | 1.844 | +8.25% |
| skewed.best.bulk-mt | 16814015 | 1.996 | 18196519 | 1.844 | +8.22% |
| skewed.best.stream-st | 16810482 | 1.996 | 18197907 | 1.844 | +8.25% |
| skewed.best.stream-mt | 16820649 | 1.995 | 18196518 | 1.844 | +8.18% |
| skewed.opt.bulk-st | 16810482 | 1.996 | 16805362 | 1.997 | -0.03% |
| skewed.opt.bulk-mt | 16814015 | 1.996 | 16806021 | 1.997 | -0.05% |
| skewed.opt.stream-st | 16810482 | 1.996 | 16805361 | 1.997 | -0.03% |
| skewed.opt.stream-mt | 16820649 | 1.995 | 16806020 | 1.997 | -0.09% |
| skewed.ultra.bulk-st | 16812693 | 1.996 | 16807639 | 1.996 | -0.03% |
| skewed.ultra.bulk-mt | 16812106 | 1.996 | 16807639 | 1.996 | -0.03% |
| skewed.ultra.stream-st | 16812693 | 1.996 | 16807649 | 1.996 | -0.03% |
| skewed.ultra.stream-mt | 16817228 | 1.995 | 16807638 | 1.996 | -0.06% |
| random.fastest.bulk-st | 33555209 | 1.000 | 33555210 | 1.000 | +0.00% |
| random.fastest.bulk-mt | 33555210 | 1.000 | 33555210 | 1.000 | +0.00% |
| random.fastest.stream-st | 33555209 | 1.000 | 33555209 | 1.000 | +0.00% |
| random.fastest.stream-mt | 33555209 | 1.000 | 33555209 | 1.000 | +0.00% |
| random.fast.bulk-st | 33555209 | 1.000 | 33555210 | 1.000 | +0.00% |
| random.fast.bulk-mt | 33555210 | 1.000 | 33555210 | 1.000 | +0.00% |
| random.fast.stream-st | 33555209 | 1.000 | 33555209 | 1.000 | +0.00% |
| random.fast.stream-mt | 33555209 | 1.000 | 33555209 | 1.000 | +0.00% |
| random.balanced.bulk-st | 33555209 | 1.000 | 33555210 | 1.000 | +0.00% |
| random.balanced.bulk-mt | 33555210 | 1.000 | 33555210 | 1.000 | +0.00% |
| random.balanced.stream-st | 33555209 | 1.000 | 33555209 | 1.000 | +0.00% |
| random.balanced.stream-mt | 33555209 | 1.000 | 33555209 | 1.000 | +0.00% |
| random.best.bulk-st | 33555209 | 1.000 | 33555210 | 1.000 | +0.00% |
| random.best.bulk-mt | 33555210 | 1.000 | 33555210 | 1.000 | +0.00% |
| random.best.stream-st | 33555209 | 1.000 | 33555209 | 1.000 | +0.00% |
| random.best.stream-mt | 33555209 | 1.000 | 33555209 | 1.000 | +0.00% |
| random.opt.bulk-st | 33555209 | 1.000 | 33555210 | 1.000 | +0.00% |
| random.opt.bulk-mt | 33555210 | 1.000 | 33555210 | 1.000 | +0.00% |
| random.opt.stream-st | 33555209 | 1.000 | 33555209 | 1.000 | +0.00% |
| random.opt.stream-mt | 33555209 | 1.000 | 33555209 | 1.000 | +0.00% |
| random.ultra.bulk-st | 33555209 | 1.000 | 33555210 | 1.000 | +0.00% |
| random.ultra.bulk-mt | 33555210 | 1.000 | 33555210 | 1.000 | +0.00% |
| random.ultra.stream-st | 33555209 | 1.000 | 33555209 | 1.000 | +0.00% |
| random.ultra.stream-mt | 33555209 | 1.000 | 33555209 | 1.000 | +0.00% |
| zeros.fastest.bulk-st | 1033 | 32483 | 1043 | 32171 | +0.97% |
| zeros.fastest.bulk-mt | 1034 | 32451 | 1148 | 29229 | +11.03% |
| zeros.fastest.stream-st | 1033 | 32483 | 1042 | 32202 | +0.87% |
| zeros.fastest.stream-mt | 1033 | 32483 | 1147 | 29254 | +11.04% |
| zeros.fast.bulk-st | 1033 | 32483 | 1043 | 32171 | +0.97% |
| zeros.fast.bulk-mt | 1034 | 32451 | 1064 | 31536 | +2.90% |
| zeros.fast.stream-st | 1033 | 32483 | 1042 | 32202 | +0.87% |
| zeros.fast.stream-mt | 1033 | 32483 | 1063 | 31566 | +2.90% |
| zeros.balanced.bulk-st | 1033 | 32483 | 1042 | 32202 | +0.87% |
| zeros.balanced.bulk-mt | 1034 | 32451 | 1063 | 31566 | +2.80% |
| zeros.balanced.stream-st | 1033 | 32483 | 1041 | 32233 | +0.77% |
| zeros.balanced.stream-mt | 1033 | 32483 | 1062 | 31596 | +2.81% |
| zeros.best.bulk-st | 1033 | 32483 | 1042 | 32202 | +0.87% |
| zeros.best.bulk-mt | 1034 | 32451 | 1049 | 31987 | +1.45% |
| zeros.best.stream-st | 1033 | 32483 | 1041 | 32233 | +0.77% |
| zeros.best.stream-mt | 1033 | 32483 | 1048 | 32018 | +1.45% |
| zeros.opt.bulk-st | 1033 | 32483 | 1042 | 32202 | +0.87% |
| zeros.opt.bulk-mt | 1034 | 32451 | 1049 | 31987 | +1.45% |
| zeros.opt.stream-st | 1033 | 32483 | 1041 | 32233 | +0.77% |
| zeros.opt.stream-mt | 1033 | 32483 | 1048 | 32018 | +1.45% |
| zeros.ultra.bulk-st | 1033 | 32483 | 1042 | 32202 | +0.87% |
| zeros.ultra.bulk-mt | 1034 | 32451 | 1042 | 32202 | +0.77% |
| zeros.ultra.stream-st | 1033 | 32483 | 1041 | 32233 | +0.77% |
| zeros.ultra.stream-mt | 1033 | 32483 | 1041 | 32233 | +0.77% |

## Coverage gaps (not run 2026-09-11, or harness does not expose)

- dec-st: random/zeros only at zst3 (harness curated set omits zst1/zst9 as redundant).
- dec-mt: json.zst1/zst9, text.zst1/zst9, skewed.zst1/zst3, zeros; worker counts beyond 16.
- enc-mt: harness fixed-worker loop is hardcoded to json/text/skewed x fastest/fast/balanced — random/zeros and best/opt/ultra MT cells do not exist; mt32 not run.
- enc-stream: harness covers json/text only (no skewed/random/zeros); ST streaming has no balanced cell; MT streaming only at mt8 (no mt16).
- Streaming decode exercised only via the 64KiB-pull read path; no write-path (`io::Write`) decode bench in the matrix.
- dec-mt / enc-mt / enc-stream ran once (no second pass); dec-st and enc-st ran twice.
- The gaps above are speed-coverage only: output sizes for every shape × level × mt/stream cell (including random/zeros and best/opt/ultra) are covered by the ratio sweep section.
- Unchanged methodology-level gaps: dictionaries, small-payload matrix, MT decode of our own MT-encoded output, >32MB inputs.
