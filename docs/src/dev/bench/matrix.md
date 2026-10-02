# Bench matrix · fresh raw data (2026-10-01)

> Pre-release pass at `1249735b` (zstdx / zstdx-cli 0.1.0): every matrix section re-run plus the full numeric 1-22 ladder, one continuous locked session. Nine commits after the 09-28 pass — R30-R32 (hugepage-backed matcher buffers, dfast extend-loop icache fix, stream copy-tax maps), R33 (row-band head-gated attempts diet + small-frame one-stream literals), R34/R35 (C FFI crate, CLI surface alignment), R36 (AVX2 entropy kernels: uniform4 pack + small-alpha histogram, plus the `ZSTDX_SIMD_FORCE`/`ZSTDX_DEC_SIMD_TIER` overrides). **Encoder outputs on the 32 MiB corpus are byte-identical to the 09-28 sweep** (all 120 ratio cells size-equal, geo-mean +8.69% unchanged); the ladder's json l5-8/10-12 rows carry R33's diet (already recorded there); dll100 tier sizes identical. Speed movement concentrates on the big-table tiers — R30's hugepage-backed matcher buffers (landed 2026-09-30, after the 09-28 pass; the DUBT heads+ring, `opt_table`/`bt`, the LDM and fused tables all ride `HugeBuf` now): json best/opt/ultra x1.41/1.10/1.12 → **1.28/1.01/1.03**, text best 0.84 → **0.67** and balanced 1.42 → **1.34**, skewed best/opt/ultra 2.11/1.05/1.06 → **1.88/0.95/0.95** (opt/ultra now wins), dll balanced/opt/ultra 1.45/1.16/1.20 → **1.31/1.09/1.13**. R36's AVX2 entropy kernels are dispatch-gated off on this AVX-512 machine (`!wide_ok`) — byte-identical and performance-inert here. Decode reproduces 09-28 within noise everywhere; skewed MT scaling reads better (mt16 1.19× ST vs 1.04× — the shape whose absolute band is known to wander). Conclusions live in [snapshot.md](snapshot.md). Earlier archives: 09-28/09-27/09-19/09-18/09-16 passes in this file's git history.

## Provenance

- commit `1249735b`, date 2026-10-01, tree clean, single continuous session (no split-commit carry-overs).
- CPU: AMD Eng Sample 100-000000870-32_Y (Zen4-class, 32 cores visible, AVX-512/BMI2), max clock 5386 MHz — same machine as every pass since 09-12.
- rustc 1.100.0-nightly (8925ea358 2026-08-20), release profile (codegen-units=1). Harness header prints `libzstd 1.5.7, binding 10507` (zstd crate / zstd-sys, `zstdmt` enabled).
- Corpus unchanged since the 09-19 pass (json.raw 2026-09-15, the other four 2026-09-07); `bench/big/dll100.*` the same 2026-09-19 02:08 set (dll100.raw = 104857600 B).
- Workspace tests green before the pass (debug / release / no-default-features). Roundtrip verification gates ON for every cell. All runs under `flock /root/programs/fork/zstd-bench.lock`.
- Commands (all via `cargo run --release -p zstdx-bench`; walls): `ratio` 110 s; `matrix --mode dec-st --budget-ms 1000 --file bench/big/dll100.zst1 --file …zst3 --file …zst9 --file …zst19` 1m42; `matrix --mode dec-mt --budget-ms 1000` 25 s; `matrix --mode enc-st --budget-ms 1000` chunked per shape × tier group exactly as the 09-28 pass (dll100 via `--shape zeros --file bench/big/dll100.raw`, its deterministic tier sizes re-captured at `--budget-ms 1`); `matrix --mode enc-mt --workers 8 --mt-workers 8 --budget-ms 1000` 46 s and `--workers 16 --mt-workers 16` 58 s; `matrix --mode enc-stream --workers 8 --mt-workers 8 --budget-ms 1000` 3m48, plus a standalone `--shape text --level fastest,fast` rerun for the T5 standalone-median protocol; `small --level 1,3,9 --shape json,text` 30 s; `files --budget-ms 1000 --threads 2,4,8,16 bench/big/dll100.zst3` 6 s; `matrix --mode enc-st --full-ladder --budget-ms 500` chunked per shape × level span as the 09-28 pass (comma-list `--level`, 23 chunks, ≈1 h).
- Same-session pull-sweep addendum (README figures, bench gained `--pull` and a full-ladder `enc-stream` ST axis mid-pass — code-only change, zero library impact): `matrix --mode dec-st --budget-ms 1000 --pull 4k,16k,64k,256k,1m` + the four dll100 files (4m27); `matrix --mode enc-stream --budget-ms 1000 --pull 4k,16k,64k,256k,1m --file bench/big/dll100.raw` (33m, gates included). Results in the T5 addendum below; the `enc-stream` MT rerun reproduced the morning cells within spread.

## T1 decode ST (bulk + streaming, 64KiB pulls; MiB/s of raw)

`x = ours_time / zstd_time`, <1 = we are faster. Harness spreads ≤±0.009 on every cell.

| file | bulk ours | bulk zstd | bulk x | stream ours | stream zstd | stream x |
|---|---:|---:|---:|---:|---:|---:|
| json.zst1 | 1744 | 1239 | 0.72 | 1738 | 2189 | 1.26 |
| json.zst3 | 1422 | 1132 | 0.80 | 1384 | 1873 | 1.35 |
| json.zst9 | 1722 | 1223 | 0.71 | 1643 | 2146 | 1.31 |
| json.zst19 | 2177 | 1293 | 0.59 | 2204 | 2624 | 1.19 |
| text.zst1 | 5951 | 2176 | 0.37 | 6253 | 7593 | 1.21 |
| text.zst3 | 8734 | 2409 | 0.28 | 10415 | 11142 | 1.07 |
| text.zst9 | 9399 | 2456 | 0.26 | 11570 | 12133 | 1.05 |
| text.zst19 | 9446 | 2447 | 0.26 | 11572 | 12140 | 1.05 |
| skewed.zst1 | 2311 | 1496 | 0.65 | 2578 | 2843 | 1.10 |
| skewed.zst3 | 1241 | 1022 | 0.82 | 1267 | 1534 | 1.21 |
| skewed.zst9 | 640 | 657 | 1.03 | 604 | 796 | 1.32 |
| skewed.zst19 | 2197 | 1446 | 0.66 | 2428 | 2695 | 1.11 |
| random.zst3 | 8896 | 2258 | 0.26 | 11096 | 8916 | 0.80 |
| zeros.zst3 | 12236 | 2534 | 0.21 | 12856 | 12874 | 1.00 |
| dll100.zst1 | 1144 | 992 | 0.87 | 1165 | 1512 | 1.30 |
| dll100.zst3 | 1227 | 1070 | 0.87 | 1254 | 1704 | 1.36 |
| dll100.zst9 | 1511 | 1213 | 0.80 | 1548 | 2079 | 1.34 |
| dll100.zst19 | 1320 | 1107 | 0.84 | 1356 | 1801 | 1.33 |

Every cell reproduces 09-28 inside ±0.03 x (no decode-path commits since R28; R36's tier audit touched dispatch sites only). Bulk still wins 17/18 — skewed.zst9 x1.03 the near-tie. Conclusions: [snapshot.md](snapshot.md).

## T2 decode MT scaling (solo; libzstd has no MT decode; MiB/s)

The dll row comes from `files --threads` (solo tool, same budget); its zstd-ref column is T1's stream cell.

| file | ours ST | zstd stream ST ref | mt2 | mt4 | mt8 | mt16 |
|---|---:|---:|---:|---:|---:|---:|
| json.zst3 | 1379 | 1879 | 1802 | 2038 | 2170 | 2212 |
| text.zst3 | 11132 | 11204 | 10933 | 10919 | 10947 | 10918 |
| skewed.zst9 | 635 | 796 | 760 | 756 | 757 | 754 |
| random.zst3 | 9776 | 8912 | 9525 | 9539 | 9553 | 9532 |
| dll100.zst3 | 1219 | 1704 | 1826 | 2041 | 2183 | 2234 |

json mt16 = 1.60× ST = 1.18× ref (09-28: 1.58×/1.19×), dll mt16 = 1.83× ST = 1.31× ref (strongest row, on the real-binary payload). skewed reads 1.19× ST / 0.95× ref this pass (09-28 read 1.04×/0.85× with the same binary behavior class — this shape's ST/mt absolutes and its zstd ref have wandered ±15% across passes with no decode commit in range; recorded, not attributed). text flat 0.98×. Scaling verdicts: [snapshot.md](snapshot.md).

## T3 encode ST bulk (checksums off both sides; MiB/s of raw; pairs 1/3/9/13/17/19)

dll = `bench/big/dll100.raw` (100 MB, `bench/gen_big.sh`); its ratios are payload sizes, not the 32 MiB corpus.

| level | shape | ours MiB/s | ours ratio | zstd MiB/s | zstd ratio | x |
|---|---|---:|---:|---:|---:|---:|
| fastest | json | 580 | 6.39 | 852 | 6.11 | 1.47 |
| fastest | text | 13511 | 309.23 | 10456 | 308.94 | 0.77 |
| fastest | skewed | 2848 | 2.00 | 1260 | 2.00 | 0.44 |
| fastest | random | 2344 | 1.00 | 2099 | 1.00 | 0.90 |
| fastest | zeros | 49162 | 32577 | 13260 | 32171 | 0.27 |
| fastest | dll | 520 | 4.35 | 536 | 2.19 | 1.03 |
| fast | json | 453 | 5.30 | 472 | 5.29 | 1.04 |
| fast | text | 13630 | 333.03 | 7291 | 332.90 | 0.54 |
| fast | skewed | 211 | 1.92 | 224 | 1.92 | 1.07 |
| fast | random | 2334 | 1.00 | 1989 | 1.00 | 0.85 |
| fast | zeros | 49086 | 32577 | 8698 | 32171 | 0.18 |
| fast | dll | 439 | 4.97 | 427 | 3.40 | 0.97 |
| balanced | json | 146 | 7.20 | 120 | 5.95 | 0.84 |
| balanced | text | 1292 | 384.66 | 1731 | 378.41 | 1.34 |
| balanced | skewed | 1730 | 2.00 | 74 | 1.84 | 0.043 |
| balanced | random | 2272 | 1.00 | 1538 | 1.00 | 0.68 |
| balanced | zeros | 31547 | 32577 | 1747 | 32202 | 0.055 |
| balanced | dll | 114 | 5.31 | 150 | 4.62 | 1.31 |
| best | json | 28 | 6.26 | 36 | 6.10 | 1.28 |
| best | text | 1091 | 386.42 | 727 | 385.90 | 0.67 |
| best | skewed | 7 | 1.84 | 14 | 1.84 | 1.88 |
| best | random | 2263 | 1.00 | 248 | 1.00 | 0.11 |
| best | zeros | 42597 | 32577 | 824 | 32171 | 0.019 |
| best | dll | 28 | 5.35 | 39 | 4.67 | 1.42 |
| opt | json | 7 | 7.49 | 7 | 7.49 | 1.01 |
| opt | text | 811 | 410.23 | 463 | 410.11 | 0.57 |
| opt | skewed | 3 | 2.00 | 3 | 2.00 | 0.95 |
| opt | random | 2277 | 1.00 | 12 | 1.00 | 0.005 |
| opt | zeros | 41136 | 32577 | 919 | 32202 | 0.022 |
| opt | dll | 13 | 6.04 | 14 | 5.06 | 1.09 |
| ultra | json | 3 | 7.42 | 3 | 7.42 | 1.03 |
| ultra | text | 402 | 414.12 | 271 | 413.98 | 0.67 |
| ultra | skewed | 2 | 2.00 | 2 | 2.00 | 0.95 |
| ultra | random | 2280 | 1.00 | 8 | 1.00 | 0.003 |
| ultra | zeros | 39479 | 32577 | 660 | 32202 | 0.017 |
| ultra | dll | 8 | 6.32 | 9 | 5.28 | 1.13 |

Checksum on/off (ours): json.fast 1.04, text.fast 0.85.

R30's hugepage-backed buffers are the whole story of the tier movement (R36's AVX2 kernels never dispatch on this AVX-512 box): every best/opt/ultra cell improves vs 09-28 (json 1.41/1.10/1.12 → 1.28/1.01/1.03; text 0.84/0.61/0.69 → 0.67/0.57/0.67; skewed 2.11/1.05/1.06 → 1.88/0.95/0.95; dll 1.31/1.16/1.20 → 1.42/1.09/1.13 — dll.best the one cell reading worse, its zstd side 37→39 with ours flat at n=3, in-band), text.balanced 1.42 → 1.34, dll.balanced 1.45 → 1.31 — exactly the tiers whose tables ride `HugeBuf` (DUBT heads+ring, `opt_table`/`bt`, LDM, fused). Ratios byte-identical everywhere (sizes lines equal cell for cell). The fastest/fast rows reproduce 09-28 (json.fastest 1.44→1.47, zeros 0.27/0.18 held). Conclusions: [snapshot.md](snapshot.md).

## T4 encode MT bulk (1 pass; cold pool per call both sides; MiB/s)

| cell | ours mt8 | zstd mt8 | x8 | ours mt16 | zstd mt16 | x16 | ratio ours mt16 | ratio zstd mt16 |
|---|---:|---:|---:|---:|---:|---:|---:|---:|
| json.fastest | 3393 | 4044 | 1.23 | 5135 | 1774 | 0.35 | 6.34 | 6.11 |
| json.fast | 2506 | 1021 | 0.41 | 3379 | 1017 | 0.30 | 5.30 | 5.31 |
| json.balanced | 360 | 201 | 0.56 | 434 | 201 | 0.46 | 7.19 | 5.95 |
| text.fastest | 19658 | 10492 | 0.54 | 17353 | 2247 | 0.13 | 309.08 | 39.78 |
| text.fast | 11748 | 2198 | 0.19 | 11961 | 2165 | 0.18 | 332.94 | 189.16 |
| text.balanced | 1046 | 1241 | 1.18 | 1049 | 1208 | 1.14 | 384.59 | 378.30 |
| skewed.fastest | 4119 | 3246 | 0.79 | 3596 | 1544 | 0.43 | 2.00 | 2.00 |
| skewed.fast | 1187 | 635 | 0.54 | 1660 | 639 | 0.39 | 1.92 | 1.92 |
| skewed.balanced | 720 | 121 | 0.18 | 718 | 121 | 0.17 | 2.00 | 1.84 |

json.fastest.mt8 x1.23 (spread 1.03-1.57) remains the one clean zstd mt win — the cell's known noise band (0.95-1.65 across passes); every other cell ahead or (text.balanced) inside its own 0.99-1.47 spread. Our cold-pool mt16 json.fast (3379) still beats zstd's warm-pool reference (1597 this run). Conclusions: [snapshot.md](snapshot.md).

## T5 encode streaming (64KiB pulls; interleaved medians)

| cell | ST ours | ST zstd | ST x | MT8 ours | MT8 zstd | MT8 x |
|---|---:|---:|---:|---:|---:|---:|
| json.fastest | 579 | 777 | 1.34 | 3215 | 1967 | 0.61 |
| json.fast | 470 | 440 | 0.94 | 2260 | 399 | 0.18 |
| json.balanced | 85 | 104 | 1.23 | 381 | 115 | 0.30 |
| json.best | 29 | 35 | 1.21 | 84 | 52 | 0.61 |
| json.opt | 7 | 7 | 1.01 | 17 | 7 | 0.40 |
| json.ultra | 3 | 3 | 0.98 | 7 | 3 | 0.49 |
| text.fastest | 7063* | 1767 | 0.25 | 7767 | 3972 | 0.51 |
| text.fast | 6776* | 5274 | 0.78 | 5738 | 1573 | 0.28 |
| text.balanced | 1423 | 1617 | 1.14 | 984 | 967 | 0.98 |
| text.best | 1001 | 651 | 0.65 | 824 | 358 | 0.44 |
| text.opt | 744 | 424 | 0.57 | 692 | 379 | 0.55 |
| text.ultra | 387 | 260 | 0.67 | 373 | 237 | 0.64 |

*The two text ST fast cells carry standalone medians per the established protocol: the in-pass table read 5643/5566 (x0.307/0.901) — the same-process page-dispersity collapse, now reproduced in **three** consecutive full passes (09-27, 09-28, this one) and still never standalone (this pass's solo rerun: 7063/6776, spreads ±0.002; mechanism and reproducer in the [workflow pitfalls](../pitfalls/workflow.md)). Unknown-size text.fastest streaming: zstd emits 1.84MB (ratio 18.3) vs our 108KB (ratio 309). text.balanced stream-mt8 closed to x0.98 (was 1.03) — no stream-mt cell loses. Stream-mt8 vs own bulk-mt8 ceilings: json 92/90/104/102/131/140%, text 40/43/98/90/108/107% — text.opt/ultra stream-mt8 still beat their printed bulk-mt8 ceilings.

**Same-session addendum (pull sweep, `--pull 4k,16k,64k,256k,1m`, the README-figure runs).** The ST section's axis is now the full six tiers and the cells loop over pull sizes, so the previously unmeasured ST cells landed: json.balanced 85/104 (x1.23, ratio 7.13 vs 5.95 — a +19.8% byte win at a speed loss), json.opt x1.01, **json.ultra x0.98 (a win)**, text.balanced 1423/1617 (x1.14 at +1.7% bytes), text.opt x0.57, text.ultra x0.67; dll100 joined via `--file` (its ST 64 KiB cells: fastest 516/509, fast 437/394, balanced 120/138, best 28/37, opt 13/13, ultra 8/9 — every loss carrying −13..−16% bytes). Pull-size verdict: **both stream directions are pull-size-flat** — every tier's ST encode speedup and every decode cell's speedup stays within 5% of its 64 KiB value from 4 KiB to 1 MiB pulls (json.best's 1.22-1.28 the widest encode band, random's 0.78-0.82 the widest decode band; 4 KiB pulls slightly favor us on decode). The unknown-size stream caveat shows in the sizes: json.balanced stream ruz 4707627 (r 7.13) vs bulk 4661008 (r 7.20) — no pledge, no row resize, on both sides.

**dll100 joined the streaming-MT section (2026-10-02)** — T5b now iterates the same payload list as ST, so raw `--file` payloads get MT cells. First measurement, stream-mt8 vs libzstd-mt8: ahead only at fast (537/437, x0.81) and opt (26/25, x0.96); trailing fastest 585/2051 (x3.56), balanced 157/180 (x1.15), ultra 15/18 (x1.22) and best 6/83 (**x14.27**). Against our own bulk-mt8 ceilings (1073/950/169/13/36/22 MiB/s) the dll stream cells run 46-93% — the same stream-vs-ceiling class as text's fast rows (40-43%, recorded above) — but the libzstd-MT losses are new: its dll stream-mt is genuinely strong (fastest 2051 MiB/s ≈ 4× its own ST stream). json/text MT cells re-measured in the same run reproduce the recorded table within spread. Investigation opened as [todo](../todo.md) 17.

**Same-day follow-up (2026-10-02, `count_from_long` landed)**: the best cell's x14.27 was a matcher long-compare collapse, not scheduling (attribution in [mt-stream](../perf/mt-stream.md)); with the SIMD compare the cell reads **best 15-16/83 (x5.4)**, the bulk-mt8 best ceiling 13 → 29 MiB/s, all other cells unchanged, every cell's output byte-identical. [todo](../todo.md) 17.

## T6 compression-ratio sweep (`zstdx-bench ratio`)

Full matrix 5 shapes x 6 levels x {bulk,stream} x {st,mt4}, one deterministic pass per cell, checksums off; Δ% = ours/zstd ratio − 1, geo-mean. Wall 110 s. Every cell roundtrip-gated. **Outputs are byte-identical to the 09-28 sweep** (and therefore to 09-27): R33's diet moves only ladder rows outside the tier set, R34-R36 touch no encoder output on this corpus — the sweep re-confirms it cell for cell.

Geo-mean Δ over 120 cells **+8.69%**; per mode: bulk-st **+1.48%**, bulk-mt **+11.08%**, stream-st **+11.43%**, stream-mt **+11.11%**; per shape: json +4.41%, text +40.36%, skewed +1.41%, random ±0, zeros +2.06%. Losing cells (complete, identical to 09-28): json.fast mt −0.12..−0.14%, text.ultra mt −0.02%, skewed.opt −0.06..−0.07%, skewed.ultra −0.04%, skewed.fast mt −0.01..−0.03%, skewed.fastest −0.00%. Everything else at parity or denser; random ties byte-exact at every cell; text.balanced +1.65..+1.70%; json.opt bulk-st 9 B ahead of zstd-17.

## T7 release gate — full numeric ladder 1-22 (enc-st, per-side budget 500 ms; MiB/s)

Both sides at the same numeric level; `x = ours_time / zstd_time`, <1 = we are faster. Cells whose three minimum rounds already cover the 500 ms budget stop at n=3 (json 6-22, skewed 4 and 13-22), so those absolutes carry the widest drift band; the rest accumulate n≥4 (text ≥5, random ≥36, zeros ≥504).

### Speed (ours/zstd MiB/s, x)

| level | json | text | skewed | random | zeros |
|---|---|---|---|---|---|
| 1 | 575/853 x1.48 | 13488/10462 x0.77 | 2877/1257 x0.44 | 2334/2220 x0.95 | 49484/13272 x0.27 |
| 2 | 532/658 x1.24 | 13303/10285 x0.77 | 2792/853 x0.31 | 2352/2183 x0.93 | 49543/13282 x0.27 |
| 3 | 462/475 x1.03 | 13657/7291 x0.53 | 208/221 x1.07 | 2342/2071 x0.89 | 49009/8697 x0.18 |
| 4 | 444/446 x1.01 | 12168/6984 x0.58 | 150/164 x1.09 | 2323/2117 x0.91 | 48748/8635 x0.18 |
| 5 | 210/266 x1.26 | 7388/4562 x0.62 | 2116/128 x0.061 | 2453/1836 x0.75 | 47118/6350 x0.14 |
| 6 | 150/187 x1.25 | 5851/2735 x0.47 | 2106/107 x0.051 | 2454/1815 x0.74 | 46041/2783 x0.060 |
| 7 | 127/166 x1.31 | 4911/2595 x0.53 | 1781/101 x0.056 | 2438/1770 x0.73 | 45471/2758 x0.061 |
| 8 | 101/127 x1.25 | 4132/1738 x0.42 | 1778/82 x0.046 | 2435/1769 x0.73 | 44629/1771 x0.040 |
| 9 | 148/122 x0.83 | 1290/1734 x1.34 | 1750/75 x0.043 | 2228/1624 x0.73 | 31921/1742 x0.055 |
| 10 | 75/92 x1.22 | 3147/1553 x0.49 | 1375/61 x0.044 | 2337/1224 x0.53 | 42910/1688 x0.039 |
| 11 | 54/66 x1.22 | 2467/1411 x0.57 | 1514/47 x0.030 | 2395/1349 x0.56 | 42264/1687 x0.040 |
| 12 | 45/56 x1.23 | 2374/878 x0.37 | 1227/25 x0.020 | 2338/667 x0.29 | 41008/984 x0.024 |
| 13 | 29/36 x1.25 | 1092/728 x0.67 | 7/14 x1.89 | 2271/259 x0.12 | 42864/823 x0.019 |
| 14 | 24/27 x1.12 | 977/586 x0.60 | 7/12 x1.68 | 2267/119 x0.052 | 41153/714 x0.017 |
| 15 | 22/16 x0.75 | 918/459 x0.50 | 6/8 x1.17 | 2294/114 x0.050 | 40069/626 x0.016 |
| 16 | 8/10 x1.28 | 813/514 x0.63 | 4/8 x2.22 | 2264/21 x0.009 | 42688/1075 x0.025 |
| 17 | 7/7 x1.02 | 801/455 x0.57 | 3/3 x0.95 | 2299/12 x0.005 | 41007/914 x0.022 |
| 18 | 4/5 x1.22 | 475/383 x0.81 | 2/3 x1.31 | 2288/10 x0.004 | 41178/869 x0.021 |
| 19 | 3/3 x1.01 | 404/271 x0.67 | 2/2 x0.95 | 2280/8 x0.003 | 39609/658 x0.017 |
| 20 | 3/2 x0.81 | 286/217 x0.76 | 2/2 x0.76 | 2312/6 x0.003 | 45696/398 x0.009 |
| 21 | 2/2 x0.90 | 277/156 x0.56 | 2/1 x0.91 | 2324/8 x0.003 | 42074/234 x0.006 |
| 22 | 2/1 x0.52 | 262/136 x0.52 | 2/1 x0.89 | 2337/10 x0.004 | 41450/202 x0.005 |

### Ratio (ours/zstd, higher = denser)

| level | json | text | skewed | random | zeros |
|---|---|---|---|---|---|
| 1 | 6.39/6.11 | 309.23/308.94 | 2.00/2.00 | 1.00/1.00 | 32577/32171 |
| 2 | 6.39/5.73 | 312.55/315.95 | 2.00/2.00 | 1.00/1.00 | 32577/32171 |
| 3 | 5.30/5.29 | 333.03/332.90 | 1.92/1.92 | 1.00/1.00 | 32577/32171 |
| 4 | 5.29/5.28 | 333.26/333.23 | 1.81/1.81 | 1.00/1.00 | 32577/32171 |
| 5 | 5.86/5.57 | 356.06/358.32 | 2.00/1.86 | 1.00/1.00 | 32577/32171 |
| 6 | 6.63/5.76 | 367.54/370.17 | 2.00/1.86 | 1.00/1.00 | 32577/32202 |
| 7 | 6.75/5.84 | 372.60/374.93 | 2.00/1.84 | 1.00/1.00 | 32577/32202 |
| 8 | 7.16/5.99 | 375.04/378.25 | 2.00/1.84 | 1.00/1.00 | 32577/32202 |
| 9 | 7.20/5.95 | 384.66/378.41 | 2.00/1.84 | 1.00/1.00 | 32577/32202 |
| 10 | 7.18/6.03 | 378.68/381.88 | 2.00/1.84 | 1.00/1.00 | 32577/32202 |
| 11 | 7.25/6.08 | 380.76/383.83 | 2.00/1.84 | 1.00/1.00 | 32577/32202 |
| 12 | 7.25/6.08 | 380.79/383.85 | 2.00/1.84 | 1.00/1.00 | 32577/32202 |
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

The ratio table is byte-identical to 09-28 outside json l5-8/10-12, which carry R33's recorded diet (json 5.86/6.63/6.75/7.16 and 7.18/7.25/7.25 vs zstd 5.57-6.08 — still −4.8..−16.4% bytes). Speed: json l5-8/10-12 hold the diet's x1.22-1.31; the hugepage win shows across the btlazy/opt band (skewed 17-22 now x0.76-1.31, was 0.86-1.46; json 19-22 all ≤1.01 — json.l19's old x1.11 exception is gone, json.l18 x1.22 the band's residue); everything else reproduces 09-28 within noise. Remaining holes unchanged: text l9 (x1.34) and skewed 13-16 speed (x1.17-2.22 at ratio parity).

## T8 small-payload encode (1 KiB-1 MiB per-call; checksums off; x = ours/zstd)

| level | shape | 1K | 4K | 64K | 1024K |
|---|---|---:|---:|---:|---:|
| 1 | json | 1.23 | 1.35 | 1.39 | 1.39 |
| 1 | text | 1.62 | 1.52 | 1.66 | 1.49 |
| 3 | json | 1.28 | 1.13 | 1.01 | 1.14 |
| 3 | text | 1.38 | 1.11 | 1.09 | 1.03 |
| 9 | json | 1.13 | 0.93 | 0.66 | 1.00 |
| 9 | text | 1.73 | 1.48 | 2.76 | 1.01 |

Reproduces the 09-28 table cell for cell (json l9 x1.13/0.93/0.66/1.00 on the stock row; text l9 keeps the swap's ratio at x1.73/1.48/2.76/1.01; level 1 x1.23-1.66 and level 3 x1.01-1.38 the accepted trades — the R14 tiny dense bars plus the fast row's small-src dense bars). R33's one-stream literals show only in bytes (−7..−9 B on the 1K/4K cells, recorded in [encoding](../perf/encoding.md)), not in these speed columns.
