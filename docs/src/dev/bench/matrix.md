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

## Coverage gaps (not run 2026-09-11, or harness does not expose)

- dec-st: random/zeros only at zst3 (harness curated set omits zst1/zst9 as redundant).
- dec-mt: json.zst1/zst9, text.zst1/zst9, skewed.zst1/zst3, zeros; worker counts beyond 16.
- enc-mt: harness fixed-worker loop is hardcoded to json/text/skewed x fastest/fast/balanced — random/zeros and best/opt/ultra MT cells do not exist; mt32 not run.
- enc-stream: harness covers json/text only (no skewed/random/zeros); ST streaming has no balanced cell; MT streaming only at mt8 (no mt16).
- Streaming decode exercised only via the 64KiB-pull read path; no write-path (`io::Write`) decode bench in the matrix.
- dec-mt / enc-mt / enc-stream ran once (no second pass); dec-st and enc-st ran twice.
- Unchanged methodology-level gaps: dictionaries, small-payload matrix, MT decode of our own MT-encoded output, >32MB inputs.
