# Bench matrix · fresh raw data (2026-10-18)

> Pre-release pass at `add0b0be` (zstdx / zstdx-cli / zstdx-ffi 0.2.0): every matrix section re-run plus the full numeric 1-22 ladder, one continuous locked session. Seventeen library commits after the 10-01 pass — R37/R38 (fused bit-read re-land as per-callsite `const FUSED` + the offset rotation, both already gate-measured 2026-10-07), R39-R41 (fast/dfast coordinate epoch, the reach-probe's concurrent shrink/stock parse), R42 (dict stock-row exact-selection exemption), R43 (chain-heads epoch past 3.5 GiB — inert on this corpus by design), the MT `count_from_long` SIMD compare, `bulk` pledged_size, and the decode-hardening quartet (RLE output bound, oversized block claims, FHD reserved bit, LDM quiet count). **Encoder outputs are byte-identical to the 10-01 pass**: all 120 ratio cells size-equal (geo-mean +8.69% unchanged), all 110 ladder ratio cells equal, dll100 tier sizes identical. Speed moves concentrate where the commits point — text.balanced ST x1.34 → **1.10** and its ladder l9 row x1.34 → 1.11 (R40/R41, the recorded 1.10-1.23 band), json.balanced 0.84 → **0.771**, json.best 1.28 → 1.23, json.ultra 1.03 → 1.00, skewed.opt 0.95 → 0.92, dll.best 1.42 → **1.26**; decode stream cells drift a few % better (json x1.19-1.35 → 1.13-1.33, dll zst9 bulk 0.80 → 0.742 — the R38 direction), dec-mt json mt16 1.60 → **1.65× ST**, and text.balanced stream-mt8 closes to **x0.947, the section's first balanced-tier win**. dll100 stream cells reproduce the 10-02 addendum (best x5.39 vs the recorded 5.4). Conclusions live in [snapshot.md](snapshot.md). Earlier archives: 10-01/09-28/09-27/09-19 passes in this file's git history.

## Provenance

- commit `add0b0be`, date 2026-10-18, tree clean, single continuous session.
- CPU: AMD Eng Sample 100-000000870-32_Y (Zen4-class, 32 cores visible, AVX-512/BMI2), max clock 5386 MHz — same machine as every pass since 09-12.
- rustc 1.100.0-nightly (8925ea358 2026-08-20), release profile (codegen-units=1). Harness header prints `libzstd 1.5.7, binding 10507` (zstd crate / zstd-sys, `zstdmt` enabled).
- Corpus unchanged since the 09-19 pass (json.raw 2026-09-15, the other four 2026-09-07); `bench/big/dll100.*` the same 2026-09-19 02:08 set (dll100.raw = 104857600 B).
- Workspace tests green before the pass (debug / release / no-default-features). Roundtrip verification gates ON for every cell. All runs under `flock /root/programs/fork/zstd-bench.lock`.
- Commands (all via `cargo run --release -p zstdx-bench`): `ratio` 115 s; `matrix --mode dec-st --budget-ms 1000` + the four dll100 `.zst*` files via `--file`; `matrix --mode dec-mt --budget-ms 1000`; `matrix --mode enc-st --budget-ms 1000` chunked per shape × tier group exactly as the 09-28/10-01 passes (dll100 via `--shape zeros --file bench/big/dll100.raw`); `matrix --mode enc-mt --workers 8 --mt-workers 8` and `--workers 16 --mt-workers 16`, both `--budget-ms 1000`; `matrix --mode enc-stream --workers 8 --mt-workers 8 --budget-ms 1000` (json/text), a standalone `--shape text --level fastest,fast` rerun for the T5 standalone-median protocol, and a `--file bench/big/dll100.raw` rerun for the dll stream cells; `small --level 1,3,9 --shape json,text`; `files --budget-ms 1000 --threads 2,4,8,16 bench/big/dll100.zst3` for the T2 dll row; `matrix --mode enc-st --full-ladder --budget-ms 500` chunked per shape × level span as the 09-28 pass (23 chunks). Full-session wall ≈75 min.

## T1 decode ST (bulk + streaming, 64KiB pulls; MiB/s of raw)

`x = ours_time / zstd_time`, <1 = we are faster. Harness spreads ≤±0.005 on every cell.

| file | bulk ours | bulk zstd | bulk x | stream ours | stream zstd | stream x |
|---|---:|---:|---:|---:|---:|---:|
| json.zst1 | 1733 | 1227 | 0.71 | 1778 | 2186 | 1.23 |
| json.zst3 | 1434 | 1109 | 0.77 | 1415 | 1874 | 1.33 |
| json.zst9 | 1724 | 1184 | 0.69 | 1677 | 2146 | 1.28 |
| json.zst19 | 2158 | 1269 | 0.59 | 2226 | 2510 | 1.13 |
| text.zst1 | 5948 | 2126 | 0.36 | 6284 | 7554 | 1.20 |
| text.zst3 | 8612 | 2338 | 0.27 | 10449 | 11094 | 1.06 |
| text.zst9 | 9286 | 2387 | 0.26 | 11612 | 12053 | 1.04 |
| text.zst19 | 9331 | 2388 | 0.26 | 11607 | 12128 | 1.05 |
| skewed.zst1 | 2334 | 1467 | 0.63 | 2632 | 2843 | 1.08 |
| skewed.zst3 | 1271 | 1016 | 0.80 | 1292 | 1547 | 1.20 |
| skewed.zst9 | 643 | 642 | 1.00 | 621 | 797 | 1.28 |
| skewed.zst19 | 2183 | 1410 | 0.65 | 2461 | 2697 | 1.10 |
| random.zst3 | 8796 | 2181 | 0.25 | 11068 | 8888 | 0.80 |
| zeros.zst3 | 11690 | 2449 | 0.21 | 12865 | 12874 | 1.00 |
| dll100.zst1 | 1157 | 973 | 0.84 | 1183 | 1514 | 1.28 |
| dll100.zst3 | 1236 | 1011 | 0.82 | 1280 | 1709 | 1.34 |
| dll100.zst9 | 1536 | 1137 | 0.74 | 1575 | 2081 | 1.32 |
| dll100.zst19 | 1326 | 1048 | 0.79 | 1379 | 1819 | 1.32 |

Bulk holds 17/18 wins (skewed.zst9 x1.00 the near-tie, now exactly at parity); every stream cell reads at or a few % inside its 10-01 value (json 1.19-1.35 → 1.13-1.33, skewed.zst9 1.32 → 1.28, dll 1.30-1.36 → 1.28-1.34) — the R37/R38 re-land was gate-measured at −1..−3% on these cells on 2026-10-07 and the pass reads the same direction. Conclusions: [snapshot.md](snapshot.md).

## T2 decode MT scaling (solo; libzstd has no MT decode; MiB/s)

The dll row comes from `files --threads` (solo tool, same budget); its zstd-ref column is T1's stream cell.

| file | ours ST | zstd stream ST ref | mt2 | mt4 | mt8 | mt16 |
|---|---:|---:|---:|---:|---:|---:|
| json.zst3 | 1399 | 1875 | 1927 | 2117 | 2276 | 2308 |
| text.zst3 | 11132 | 11179 | 10832 | 10852 | 10843 | 10818 |
| skewed.zst9 | 608 | 788 | 783 | 756 | 782 | 759 |
| random.zst3 | 9678 | 8928 | 9458 | 9479 | 9476 | 9471 |
| dll100.zst3 | 1225 | 1709 | 1872 | 2102 | 2241 | 2322 |

json mt16 = 1.65× ST = 1.23× ref (10-01: 1.60×/1.18×), dll100 mt16 = 1.90× ST = 1.36× ref (10-01: 1.83×/1.31×) — both scaling rows strengthened. skewed 1.25× ST / 0.96× ref (the shape's band wanders ±15% across passes; 10-01 read 1.19×/0.95×), text flat 0.97×, random flat 0.98× ST at 1.06× ref. Scaling verdicts: [snapshot.md](snapshot.md).

## T3 encode ST bulk (checksums off both sides; MiB/s of raw; pairs 1/3/9/13/17/19)

dll = `bench/big/dll100.raw` (100 MB, `bench/gen_big.sh`); its ratios are payload sizes, not the 32 MiB corpus.

| level | shape | ours MiB/s | ours ratio | zstd MiB/s | zstd ratio | x |
|---|---|---:|---:|---:|---:|---:|
| fastest | json | 590 | 6.39 | 852 | 6.11 | 1.45 |
| fastest | text | 13231 | 309.23 | 10494 | 308.94 | 0.79 |
| fastest | skewed | 2832 | 2.00 | 1253 | 2.00 | 0.44 |
| fastest | random | 2286 | 1.00 | 2052 | 1.00 | 0.90 |
| fastest | zeros | 49317 | 32577 | 13305 | 32171 | 0.27 |
| fastest | dll | 501 | 4.35 | 529 | 2.19 | 1.06 |
| fast | json | 456 | 5.30 | 472 | 5.29 | 1.03 |
| fast | text | 13512 | 333.03 | 7264 | 332.90 | 0.54 |
| fast | skewed | 211 | 1.92 | 225 | 1.92 | 1.07 |
| fast | random | 2283 | 1.00 | 1911 | 1.00 | 0.84 |
| fast | zeros | 49217 | 32577 | 8709 | 32171 | 0.18 |
| fast | dll | 439 | 4.97 | 424 | 3.40 | 0.97 |
| balanced | json | 157 | 7.20 | 121 | 5.95 | 0.77 |
| balanced | text | 1580 | 384.66 | 1732 | 378.41 | 1.10 |
| balanced | skewed | 1839 | 2.00 | 75 | 1.84 | 0.041 |
| balanced | random | 2280 | 1.00 | 1471 | 1.00 | 0.65 |
| balanced | zeros | 32822 | 32577 | 1750 | 32202 | 0.053 |
| balanced | dll | 116 | 5.31 | 150 | 4.62 | 1.30 |
| best | json | 30 | 6.26 | 37 | 6.10 | 1.23 |
| best | text | 1141 | 386.42 | 692 | 385.90 | 0.61 |
| best | skewed | 7 | 1.84 | 14 | 1.84 | 1.91 |
| best | random | 2190 | 1.00 | 251 | 1.00 | 0.11 |
| best | zeros | 42873 | 32577 | 816 | 32171 | 0.019 |
| best | dll | 31 | 5.35 | 39 | 4.67 | 1.26 |
| opt | json | 7 | 7.49 | 7 | 7.49 | 1.01 |
| opt | text | 816 | 410.23 | 452 | 410.11 | 0.55 |
| opt | skewed | 3 | 2.00 | 3 | 2.00 | 0.92 |
| opt | random | 2217 | 1.00 | 12 | 1.00 | 0.005 |
| opt | zeros | 41220 | 32577 | 900 | 32202 | 0.022 |
| opt | dll | 13 | 6.04 | 14 | 5.06 | 1.09 |
| ultra | json | 3 | 7.42 | 3 | 7.42 | 1.00 |
| ultra | text | 402 | 414.12 | 258 | 413.98 | 0.64 |
| ultra | skewed | 2 | 2.00 | 2 | 2.00 | 0.95 |
| ultra | random | 2221 | 1.00 | 8 | 1.00 | 0.003 |
| ultra | zeros | 39748 | 32577 | 634 | 32202 | 0.016 |
| ultra | dll | 8 | 6.32 | 9 | 5.28 | 1.12 |

Sizes equal the 10-01 pass byte for byte on every cell. Speed: the balanced/best band carries the R40/R41 reach-probe parse (text.balanced 1.34 → 1.10, json.balanced 0.84 → 0.77) and the tree tiers read at or inside their bands (json.best 1.28 → 1.23, dll.best 1.42 → 1.26, skewed.opt 0.95 → 0.92, json.ultra 1.03 → 1.00); fastest/fast reproduce (json.fastest 1.47 → 1.45 in the cell's known spread, dll.fastest 1.03 → 1.06 likewise). Checksum on/off (ours): json.fast 1.00, text.fast 0.86. Conclusions: [snapshot.md](snapshot.md).

## T4 encode MT bulk (1 pass; cold pool per call both sides; MiB/s)

| cell | ours mt8 | zstd mt8 | x8 | ours mt16 | zstd mt16 | x16 | ratio ours mt16 | ratio zstd mt16 |
|---|---:|---:|---:|---:|---:|---:|---:|---:|
| json.fastest | 3443 | 4046 | 1.19 | 5175 | 1758 | 0.34 | 6.34 | 6.11 |
| json.fast | 2375 | 990 | 0.42 | 3307 | 1004 | 0.30 | 5.30 | 5.31 |
| json.balanced | 427 | 201 | 0.48 | 548 | 202 | 0.38 | 7.19 | 5.95 |
| text.fastest | 19538 | 10572 | 0.55 | 17237 | 2228 | 0.13 | 309.08 | 39.78 |
| text.fast | 11700 | 2144 | 0.18 | 12020 | 2127 | 0.18 | 332.94 | 189.16 |
| text.balanced | 1116 | 1223 | 1.08 | 1144 | 1216 | 1.09 | 384.59 | 378.30 |
| skewed.fastest | 4072 | 3216 | 0.79 | 3610 | 1527 | 0.42 | 2.00 | 2.00 |
| skewed.fast | 1193 | 631 | 0.54 | 1633 | 635 | 0.39 | 1.92 | 1.92 |
| skewed.balanced | 681 | 120 | 0.18 | 750 | 122 | 0.16 | 2.00 | 1.84 |

json.fastest.mt8 x1.19 (spread 0.95-1.68) remains the one clean zstd mt win — the cell's known noise band (0.95-1.65 across passes); every other cell ahead or (text.balanced, 1.08/1.09 inside its 0.99-1.47 spread) at band. Our cold-pool mt16 json.fast (3307) still beats zstd's warm-pool reference (1593 this run). Conclusions: [snapshot.md](snapshot.md).

## T5 encode streaming (64KiB pulls; interleaved medians)

| cell | ST ours | ST zstd | ST x | MT8 ours | MT8 zstd | MT8 x |
|---|---:|---:|---:|---:|---:|---:|
| json.fastest | 588 | 776 | 1.32 | 3265 | 1877 | 0.57 |
| json.fast | 467 | 439 | 0.94 | 2273 | 394 | 0.17 |
| json.balanced | 87 | 106 | 1.22 | 389 | 113 | 0.29 |
| json.best | 30 | 35 | 1.16 | 82 | 51 | 0.62 |
| json.opt | 7 | 7 | 0.97 | 17 | 7 | 0.40 |
| json.ultra | 3 | 3 | 0.98 | 7 | 3 | 0.47 |
| text.fastest | 6960* | 1761 | 0.25 | 7575 | 4131 | 0.55 |
| text.fast | 6695* | 5260 | 0.79 | 6306 | 1676 | 0.27 |
| text.balanced | 1427 | 1612 | 1.13 | 1002 | 947 | **0.95** |
| text.best | 1026 | 646 | 0.63 | 853 | 351 | 0.41 |
| text.opt | 750 | 414 | 0.55 | 669 | 371 | 0.55 |
| text.ultra | 389 | 247 | 0.63 | 366 | 236 | 0.64 |

*The two text ST fast cells carry standalone medians per the established protocol: the in-pass table read 5601/5502 (x0.310/0.909) — the same-process page-dispersity collapse, now reproduced in **four** consecutive full passes (09-27 through this one) and still never standalone (this pass's solo rerun: 6960/6695; mechanism and reproducer in the [workflow pitfalls](../pitfalls/workflow.md)). Unknown-size text.fastest streaming: zstd emits 1.84MB (ratio 18.3) vs our 108KB (ratio 309). text.balanced stream-mt8 closes to **x0.947** (was 0.98) — every stream-mt cell including it now wins. Stream-mt8 vs own bulk-mt8 ceilings: json 92/88/86/100/131/140%, text 39/48/90/102/112/111%.

dll100 stream cells (same-session `--file` rerun): ST fastest 506/500 (x0.99), fast 448/395 (x0.88), balanced 119/138 (x1.16), best 32/38 (x1.20), opt 13/14 (x1.05), ultra 8/9 (x1.08); stream-mt8 fastest 587/2047 (x3.52), fast 537/428 (x0.80), balanced 154/176 (x1.15), best 15/84 (**x5.39**), opt 26/25 (x0.95), ultra 15/18 (x1.21) — reproducing the 10-02 addendum cell for cell (best carries the `count_from_long` fix), at 52-92% of the bulk-mt8 ceilings (1064/966/168/29/36/22 MiB/s). **R61 (2026-10-22) re-run after the buffer-reserve fix** (byte-identical outputs; the decided reserve now covers the stream, killing the mid-stream growth barrier — [mt-stream](../perf/mt-stream.md)): stream-mt8 fastest 897/2154 (**x2.36**, 86% of the 1048 ceiling), balanced 161/199 (x1.25, 93%), best 27/84 (**x3.12**, 93% of 29), ultra 18/18 (**x0.98, libzstd-mt parity**, 82% of 22); corpus stream cells flat. The libzstd-MT fastest/best losses stay the open item ([todo](../todo.md) 17).

## T6 compression-ratio sweep (`zstdx-bench ratio`)

Full matrix 5 shapes x 6 levels x {bulk,stream} x {st,mt4}, one deterministic pass per cell, checksums off; Δ% = ours/zstd ratio − 1, geo-mean. Wall 115 s. Every cell roundtrip-gated. **Outputs are byte-identical to the 10-01 sweep** (and therefore to 09-28/09-27): R37-R43 and the decode-hardening commits touch no encoder output on this corpus — the sweep re-confirms it cell for cell.

Geo-mean Δ over 120 cells **+8.69%**; per mode: bulk-st **+1.48%**, bulk-mt **+11.08%**, stream-st **+11.43%**, stream-mt **+11.11%**; per shape: json +4.41%, text +40.36%, skewed +1.41%, random ±0, zeros +2.06%. Losing cells (complete, identical to 10-01): json.fast mt −0.12..−0.14%, text.ultra mt −0.02%, skewed.opt −0.06..−0.07%, skewed.ultra −0.04%, skewed.fast mt −0.01..−0.03%, skewed.fastest −0.00%. Everything else at parity or denser; random ties byte-exact at every cell; text.balanced +1.65..+1.70%; json.opt bulk-st 9 B ahead of zstd-17.

## T7 release gate — full numeric ladder 1-22 (enc-st, per-side budget 500 ms; MiB/s)

Both sides at the same numeric level; `x = ours_time / zstd_time`, <1 = we are faster. Cells whose three minimum rounds already cover the 500 ms budget stop at n=3 (json 6-22, skewed 4 and 13-22), so those absolutes carry the widest drift band; the rest accumulate n≥4 (text ≥5, random ≥36, zeros ≥504).

### Speed (ours/zstd MiB/s, x)

| level | json | text | skewed | random | zeros |
|---|---|---|---|---|---|
| 1 | 556/850 x1.53 | 13254/10478 x0.79 | 2848/1261 x0.44 | 2373/2125 x0.90 | 49629/13310 x0.27 |
| 2 | 544/655 x1.21 | 13127/10276 x0.78 | 2768/865 x0.31 | 2353/2168 x0.92 | 49487/13290 x0.27 |
| 3 | 464/475 x1.02 | 13529/7245 x0.54 | 212/225 x1.06 | 2327/2028 x0.87 | 49268/8726 x0.18 |
| 4 | 423/433 x1.02 | 12231/6898 x0.56 | 153/164 x1.08 | 2328/2050 x0.88 | 48905/8658 x0.18 |
| 5 | 207/268 x1.30 | 7477/4603 x0.62 | 1947/125 x0.064 | 2439/1803 x0.74 | 47312/6353 x0.13 |
| 6 | 148/188 x1.27 | 5902/2746 x0.47 | 1955/105 x0.054 | 2452/1782 x0.73 | 45679/2782 x0.061 |
| 7 | 125/165 x1.31 | 4954/2612 x0.53 | 1615/99 x0.061 | 2443/1725 x0.71 | 44991/2759 x0.061 |
| 8 | 101/126 x1.25 | 4128/1756 x0.42 | 1605/81 x0.051 | 2441/1752 x0.72 | 44614/1771 x0.040 |
| 9 | 157/120 x0.76 | 1562/1734 x1.11 | 1836/75 x0.041 | 2320/1541 x0.66 | 33079/1750 x0.053 |
| 10 | 74/90 x1.23 | 3139/1550 x0.49 | 1361/62 x0.045 | 2341/1200 x0.51 | 42543/1688 x0.040 |
| 11 | 53/65 x1.24 | 2479/1419 x0.57 | 1482/48 x0.033 | 2365/1297 x0.55 | 42728/1687 x0.039 |
| 12 | 45/56 x1.23 | 2353/859 x0.36 | 1196/25 x0.021 | 2335/673 x0.29 | 40790/985 x0.024 |
| 13 | 30/37 x1.23 | 1137/693 x0.62 | 7/14 x1.91 | 2266/250 x0.11 | 42675/821 x0.019 |
| 14 | 25/27 x1.10 | 1006/573 x0.57 | 7/12 x1.70 | 2272/119 x0.052 | 41333/710 x0.017 |
| 15 | 22/17 x0.76 | 947/454 x0.48 | 7/8 x1.17 | 2273/113 x0.050 | 40266/628 x0.016 |
| 16 | 8/10 x1.26 | 815/509 x0.62 | 4/9 x2.27 | 2262/21 x0.009 | 43031/1078 x0.025 |
| 17 | 7/7 x1.01 | 798/448 x0.57 | 3/3 x0.96 | 2297/12 x0.005 | 41310/916 x0.022 |
| 18 | 4/5 x1.24 | 472/371 x0.79 | 2/3 x1.37 | 2305/10 x0.004 | 41219/875 x0.021 |
| 19 | 3/3 x1.01 | 401/261 x0.65 | 2/2 x0.94 | 2282/8 x0.003 | 39634/661 x0.017 |
| 20 | 3/2 x0.82 | 266/208 x0.78 | 2/2 x0.77 | 2345/6 x0.003 | 39159/407 x0.010 |
| 21 | 2/2 x0.88 | 258/148 x0.57 | 2/1 x0.89 | 2307/8 x0.003 | 36110/237 x0.007 |
| 22 | 2/1 x0.52 | 246/129 x0.53 | 2/1 x0.87 | 2301/9 x0.004 | 40911/203 x0.005 |

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

The ratio table is byte-identical to 10-01 (all 110 ladder size cells re-verified equal). Speed: the R40/R41 reach-probe parse moves text l9 x1.34 → **1.11** and json l9 0.83 → 0.76; json l13/14 read 1.23/1.10 (was 1.25/1.12), text l13-15 0.48-0.62 (was 0.50-0.67); json l5-8/10-12 hold the R33 diet band at x1.23-1.31, the skewed 13-16 chain rows their x1.17-2.27, and every high-tier row stays ≤1.01 on json 17-22 and ≤0.96 on skewed 17-22 — the 10-01 holes (text l9 the headline) all narrow or hold. Remaining holes unchanged: text l9 (now x1.11) and skewed 13-16 speed at ratio parity.

## T8 small-payload encode (1 KiB-1 MiB per-call; checksums off; x = ours/zstd)

| level | shape | 1K | 4K | 64K | 1024K |
|---|---|---:|---:|---:|---:|
| 1 | json | 1.24 | 1.40 | 1.41 | 1.37 |
| 1 | text | 1.73 | 1.57 | 1.78 | 1.54 |
| 3 | json | 1.28 | 1.16 | 0.83 | 1.15 |
| 3 | text | 1.34 | 1.13 | 0.90 | 1.03 |
| 9 | json | 1.11 | 0.90 | 0.64 | 0.99 |
| 9 | text | 1.73 | 1.49 | 2.46 | 1.00 |

json l9 stays on the stock row (x1.11/0.90/0.64/0.99 vs 10-01's 1.13/0.93/0.66/1.00); text l9 keeps the swap's bytes (x1.73/1.49/2.46/1.00 vs 1.73/1.48/2.76/1.01 — the 64K cell reads faster this pass, in its drift band); level 3's 64K cells improved (json 1.01 → 0.83, text 1.09 → 0.90); level 1's accepted trades unchanged. R42's stock-row exemption shows only in dict fixtures (bytes), not here.
