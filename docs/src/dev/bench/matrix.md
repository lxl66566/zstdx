# Bench matrix · fresh raw data (2026-09-16)

> Full re-run of every matrix section at `dedd267d`, two days and 96 commits after the 09-14 pass at `ad57445`. Both speed **and** ratio columns moved by design this time: the DUBT finder + u32 entries + fill-lag clamp (Best/opt/ultra), the cold-start DUBT head (text.Balanced ratio flip), the dense-mode fast scan, the block splitter and the exact-cost entropy merges, the arithmetic FSE transition tables, LDM rows, the stream-mt posted-job queue, and checksum verification on every decode path. `json.raw` was also regenerated on 2026-09-15 (the avg_ml 10.9 shape died, todo 8), so cross-snapshot cells are indicative only. Conclusions live in [snapshot.md](snapshot.md). Earlier archives: 09-14 in git history of this file (pre-`3d163bd3`), 09-12 at `b264842`.

## Provenance

- commit `dedd267d`, date 2026-09-16, branch ext, tree clean.
- CPU: AMD Eng Sample 100-000000870-32_Y (Zen4-class, 32 cores visible, AVX-512/BMI2), max clock 5386 MHz.
- rustc 1.100.0-nightly (8925ea358 2026-08-20), release profile. Harness header prints `libzstd 1.5.7, binding 10507` (zstd crate / zstd-sys, `zstdmt` enabled).
- Corpus: `bench/corpus`, 32MiB x 5 shapes; `json.raw` regenerated 2026-09-15, the other four unchanged since 09-07. Ladder pairs numeric levels (fastest/fast/balanced/best/opt/ultra = 1/3/9/13/17/19).
- Roundtrip verification gates ON for every cell.
- Commands (all via `./target/release/zstdx-bench matrix`), per-side budget 1000 ms (leaner than the 09-14 pass's 1500-2000 ms; interleaved medians, the harness prints per-cell spreads):
  - `--mode dec-st --budget-ms 1000`
  - `--mode enc-st --budget-ms 1000`
  - `--mode enc-mt --workers 8 --mt-workers 8 --budget-ms 1000`
  - `--mode enc-mt --workers 16 --mt-workers 16 --budget-ms 1000`
  - `--mode enc-stream --workers 8 --mt-workers 8 --budget-ms 1000`
  - `--mode dec-mt --budget-ms 1000`
  - plus the full `ratio` sweep (123 s).

## T1 decode ST (bulk + streaming, 64KiB pulls; MiB/s of raw)

`x = ours_time / zstd_time`, <1 = we are faster. Harness spreads ≤±0.006 on every cell.

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

Reproduces the 09-14 column within ±0.03 on every cell: the ST decode core itself is unchanged (the intervening checksum-verification commit is a no-op on these checksum-less frames). The honest core-vs-core gap stays the stream column (zstd's bulk API is a slow wrapper; our own stream-vs-bulk spread ≤2%).

## T2 decode MT scaling (solo; libzstd has no MT decode; MiB/s)

| file | ours ST | zstd stream ST ref | mt2 | mt4 | mt8 | mt16 |
|---|---:|---:|---:|---:|---:|---:|
| json.zst3 | 1413 | 1882 | 1710 | 2004 | 2089 | 2180 |
| text.zst3 | 10514 | 11134 | 10518 | 10523 | 10396 | 10418 |
| skewed.zst9 | 614 | 796 | 734 | 649 | 699 | 746 |
| random.zst3 | 9727 | 8954 | 9534 | 9533 | 9553 | 9541 |

The 09-15 mt-dec lands (wildcopy staged execution, pooled staging buffers, inline stage-B checksum absorb) moved the column: json.zst3 now scales 1.21/1.42/1.48/**1.54×** ST and mt16 sits at **1.16× the zstd stream reference** (09-14: 1.11× ST, 0.83× ref); skewed.zst9 recovered from 0.71× anti-scaling to 1.06-1.22× ST but stays at 0.82-0.94× ref; text stays flat (~1.00× ST, 0.93-0.95× ref); random holds 0.98× ST = 1.06× ref. Serial stage B is still the wall everywhere except json — the decoder-side piece-parallel spend of the ramp guarantee is the open half of todo 1.

## T3 encode ST bulk (checksums off both sides; MiB/s of raw)

| shape.level | ours MiB/s | ours ratio | zstd MiB/s | zstd ratio | x | Δ vs 09-14 x |
|---|---:|---:|---:|---:|---:|---:|
| json.fastest | 596 | 6.39 | 855 | 6.11 | 1.44 | 1.57 |
| json.fast | 457 | 5.30 | 476 | 5.29 | 1.04 | 1.11 |
| json.balanced | 159 | 7.20 | 121 | 5.95 | 0.76 | 1.34 |
| json.best | 26 | 6.26 | 37 | 6.10 | 1.43 | 4.22 |
| json.opt | 6 | 7.49 | 7 | 7.49 | 1.10 | 1.30 |
| json.ultra | 3 | 7.42 | 3 | 7.42 | 1.12 | 1.32 |
| text.fastest | 12413 | 309.23 | 10449 | 308.94 | 0.84 | 0.79 |
| text.fast | 13814 | 333.03 | 7259 | 332.90 | 0.53 | 0.55 |
| text.balanced | 499 | 384.65 | 1730 | 378.41 | 3.47 | 0.59 |
| text.best | 775 | 385.04 | 738 | 385.90 | 0.95 | 1.77 |
| text.opt | 755 | 410.18 | 461 | 410.11 | 0.61 | 1.04 |
| text.ultra | 396 | 414.05 | 274 | 413.98 | 0.69 | 1.05 |
| skewed.fastest | 2966 | 2.00 | 1272 | 2.00 | 0.43 | 0.44 |
| skewed.fast | 219 | 1.92 | 223 | 1.92 | 1.02 | 1.09 |
| skewed.balanced | 1757 | 2.00 | 75 | 1.84 | 0.042 | 0.037 |
| skewed.best | 6 | 2.00 | 14 | 1.84 | 2.15 | 6.22 |
| skewed.opt | 3 | 2.00 | 3 | 2.00 | 1.05 | 1.49 |
| skewed.ultra | 2 | 2.00 | 2 | 2.00 | 1.07 | 1.40 |
| random.fastest | 2568 | 1.00 | 2312 | 1.00 | 0.90 | 0.90 |
| random.fast | 2563 | 1.00 | 2144 | 1.00 | 0.84 | 0.81 |
| random.balanced | 1220 | 1.00 | 1600 | 1.00 | 1.31 | 0.66 |
| random.best | 1901 | 1.00 | 265 | 1.00 | 0.14 | 0.11 |
| random.opt | 2341 | 1.00 | 12 | 1.00 | 0.005 | 0.005 |
| random.ultra | 2345 | 1.00 | 8 | 1.00 | 0.003 | 0.003 |
| zeros.fastest | 50024 | 32483 | 13294 | 32171 | 0.27 | 0.26 |
| zeros.fast | 49631 | 32483 | 8713 | 32171 | 0.18 | 0.17 |
| zeros.balanced | 16080 | 32483 | 1744 | 32202 | 0.108 | 0.038 |
| zeros.best | 8515 | 32483 | 831 | 32202 | 0.098 | 0.019 |
| zeros.opt | 30874 | 32483 | 936 | 32202 | 0.030 | 0.023 |
| zeros.ultra | 30057 | 32483 | 673 | 32202 | 0.022 | 0.017 |

The Δ column is mostly the recorded land waves, not drift: json fastest/fast/balanced carry the dense-mode scan + unconditional flush + reach probe (balanced now **ahead** at x0.76 with +21% density); json/skewed best collapsed 4.22/6.22→1.43/2.15 through the DUBT + u32 finder (the 09-14 table was pre-DUBT); json/text opt/ultra ride the fill-lag clamp + u32 tree entries (text now ahead); text.balanced 0.59→3.47 is the cold-start DUBT head's deliberate trade (ratio −2.78%→**+1.65%**, sizes 87,233 vs 88,673). json.opt bulk-st is 12 B ahead of zstd-17 (4,481,370 vs 4,481,382). random.balanced/best absolutes moved on both sides (ours 2317→1220/2323→1901, zstd 1529→1600/253→265) — x 0.66→1.31 / 0.11→0.14, still far ahead; **balanced attributed 09-17**: the reach probe's `parse_cost` was missing the incompressibility gate, so random's "measurement" fully parsed blocks the frame emits raw, twice per frame (fixed with the probe overhaul — random.balanced back to ~2000+ MiB/s, interleaved); the best-tier sibling stays unattributed. **New observation**: zeros.balanced/best absolutes regressed 2.9×/5.1× vs 09-14 (46,548→16,080 / 43,534→8,515; still 9-10× zstd) — unattributed, prime suspect the rep1 covered-fill chain-linking (`677684c4`) inserting the covered range on RLE-class parses; low priority (todo 12).

Checksum overhead (ours, on/off time ratio): json.fast 1.03, text.fast 0.85.

## T4 encode MT bulk (1 pass; cold pool per call both sides; MiB/s)

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

json.fastest.mt8 stays the one mt cell zstd wins cleanly, narrowed to x1.17 (was 1.27; spread 0.87-1.51 — the noisiest cell, per-core deficit scaled through workers, todo 3). json.balanced.mt8/16 improved to 0.48/0.49 (was 0.71/0.75). text.balanced x2.85/2.81 at an identical 444 MiB/s regardless of width — the DUBT head's serial per-frame cost (head parse + job zeros), not a scheduling artifact. MT ratio preservation vs own ST holds (±0.5% except the documented json.opt mt anomaly). zstd warm-pool json.fast mt16 reference 1593-1598 MiB/s (ours cold-pool 3202).

## T5 encode streaming (64KiB pulls; interleaved medians)

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

The best-tier stream cells followed the core: json.best stream ST 4.05→**1.43**, stream-mt8 3.31→**0.78 (ahead)**; text.best 2.60→1.44 ST, mt8 →1.17; text.opt/ultra stream-mt8 at parity (0.99/1.03). json.fast stream ST crossed ahead (0.94, was 1.00). text.fastest stream-mt8 improved 0.75→0.68 (post the 09-15 posted-job queue the 09-14 headline already carried 1.90→0.75). Remaining behind: json.fastest ST x1.30 (the item-3 per-core deficit), the two best-tier ST cells (x1.43/1.44, capped by the best core), and text.balanced x3.02 (the DUBT head per frame). Stream-mt8 vs own bulk-mt8 ceilings: json 71/73/85/105/123/120% (fastest/fast/balanced/best/opt/ultra — opt/ultra above their printed ceiling refs, ceiling measurement noise), text 27/28/76/97/83/91% — text.fastest/fast stay bounded by the serial 16 KiB read-pump + epoch quiesce (todo 12); ceilings (solo refs): json 3819/2555/427/65/13/5, text 22479/17855/445/319/462/259.

## T6 compression-ratio sweep (`zstdx-bench ratio`, 2026-09-16)

Full matrix 5 shapes x 6 levels x {bulk,stream} x {st,mt4}, one deterministic pass per cell, checksums off; Δ% = ours/zstd ratio − 1, geo-mean. Wall 123 s. Every cell roundtrip-gated.

Geo-mean Δ over 120 cells **+8.66%**; per mode: bulk-st **+1.40%**, bulk-mt **+11.06%**, stream-st **+11.41%**, stream-mt **+11.10%**; per shape: json +4.41%, text +40.28%, skewed +1.41%, random ±0, zeros +1.99%. Worst cell text.best.bulk-mt −0.23%; best text.fastest.stream-st +1594% (libzstd's unknown-size streaming collapse).

Losing cells (Δ < 0, complete list): text.best **−0.15..−0.23%** (all four modes; todo 9b's 512 KiB head parse); skewed.opt −0.06..−0.07% (near-tie, todo 9c). Everything else at parity or denser; random ties byte-exact at every cell; zeros mt wins up to +11%. text.balanced flipped from the 09-14 sweep's −2.73..−2.79% to **+1.65..+1.71%** (cold-start DUBT head); json.opt bulk-st flipped to 12 B ahead (block splitter).

Not cell-comparable to the 09-14 sweep (+9.02%): the encoder's output changed by design on multiple rows (dense-mode scan, block splitter, raw-literals size ladder, exact-cost entropy merges, LDM) and json.raw was regenerated; the 09-14 note's same-code reference-side drift band (±0.8 pt of geo-mean) also still applies.
