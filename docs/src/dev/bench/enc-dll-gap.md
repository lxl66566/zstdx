# Encode dll100 gap vs libzstd — differential attribution (2026-09-20)

> Core-vs-core decomposition of the dll100 low-tier encode gap (snapshot deficit #8), measured against the real opponent: whole-file callgrind Ir (100 MB, one pass) plus per-symbol breakdowns of both encoders. All numbers from the bench machine (Zen4), zstd crate 0.13.2 (libzstd 1.5.7) via a /tmp twin harness calling `zstd::bulk::compress` — the exact matrix `enc-st` cell body; our side `zstdx-bench prof enc <lvl> 1` (bulk slice, checksums off). Literal volumes from `seqstats --ref-frame` against the zstd CLI frames.

## T1: totals — instruction volume only below balanced

| tier | ours Ir | zstd Ir | ours/zstd | ours size | zstd size | nseq ours/ref | lit ours/ref |
|---|---:|---:|---:|---:|---:|---:|---:|
| l1 fastest | 3,792,480,451 | 2,969,259,712 | 1.277 | 48,802,511 | 47,922,691 | 4.85 M / 5.00 M | 46.5 MB / 45.4 MB |
| l3 fast | 3,238,547,300 | 2,959,509,856 | 1.094 | 30,183,932 | 30,874,173 | 5.51 M / 6.01 M | 19.8 MB / 21.8 MB |
| l9 balanced | 10,903,348,910 | 11,269,595,186 | **0.967** | 20,782,718 | 22,701,602 | 3.39 M / 4.76 M | — |

- **Balanced is not an instruction gap at all**: we execute 3.3% FEWER instructions than libzstd at l9 while running x1.49 the wall. The chain walk's documented latency-bound regime (link chase at ~66 cyc/step, [matchers](../perf/matchers.md)) is the whole story on this tier; libzstd's l9 runs `ZSTD_RowFindBestMatch` (16-entry contiguous rows, one cache line per bucket) against our position-indexed chain (H21 heads 8 MB + C20 links 4 MB of random accesses). Closing it is a matcher-architecture question (row matcher), not an attrition one.
- fastest and fast ARE instruction-volume gaps (1.28×/1.09×), and the per-symbol split below shows where.

## T2: per-function Ir (callgrind, one dll100 pass)

l1 (fastest): ours — scan `start_matching_fast` 2,118.4 M (55.9%), literals huff stream `write_packed_codes_dual_impl` 516.2 M (13.6%), `encode_sequences` 432.0 M (11.4%), `insert_covered` 425.0 M (11.2%), `histogram_literals` 128.8 M, `choose_tables_fast` 52.8 M, libm `__log2_fma` 17.2 M. zstd — `ZSTD_compressBlock_fast` 1,704.8 M (57.4%, storeSeq inline), `ZSTD_encodeSequences_bmi2` 529.8 M, `HUF_compress1X_usingCTable` 328.7 M, `HIST_count_parallel_wksp` 187.4 M, `ZSTD_seqToCodes` 95.2 M.

Gap 823 M decomposes: **scan loop +414 M (50%), `insert_covered` +425 M (52%), literals huff stream +187 M (23%)**, offset by seq-encode −193 M (ours ahead), literals histogram −59 M, table builds −15 M, plus the entropy-bound `log2` calls +17 M (ours only).

l3 (fast): ours — `start_matching_dfast` 2,327.4 M (71.9%), `encode_sequences` 490.3 M, huff stream 217.3 M; zstd — `ZSTD_compressBlock_doubleFast` 1,789.1 M, `encodeSequences` 602.0 M, `seqToCodes` 108.0 M, HUF 145.8 M, `ZSTD_splitBlock`+fingerprint 73.8 M. Gap 279 M ≈ scan loop +538 M minus emit/histogram/split wins (−259 M): **the dfast scan excess is the whole l3 gap** (x1.30 on the scan symbol).

l9 (balanced): ours — `start_matching_codes` 8.70 G (79.8%), LDM `gear_feed` 0.62 G, DUBT head `find_best` 0.50 G, `ldm_generate` 0.23 G, emit+entropy 0.49 G; zstd — `RowFindBestMatch_noDict_5_4` 8.54 G + `lazy2_row` 1.32 G + `ZSTD_count` 0.18 G. Matcher-side Ir is a wash (ours 10.05 G vs theirs 10.04 G counting the LDM machinery we carry for the −8.4% size).

## T3: the literals huff stream was the one emit-side gap — closed 2026-09-20

Per literal byte (volumes near-identical both sides): ours 11.1 Ir/sym vs zstd 7.2 at l1, 11.0 vs 6.7 at l3 — the stream loop paid an nb-extract (`mov`+`and $0xf`), a `shr %cl` (the BMI2 dispatch wrapper only tail-jumped the baseline impl, so shrx never materialized — a delegation is not a feature-compiled copy) and a separate count add per symbol. libzstd's `HUF_CElt` discipline lands all three with one whole-element op each: the shift count IS the element (x86 masks to 6 bits = nb; element bits 4..7 are zero since codes sit above bit 53), the OR payload is the raw element, and the bit counter accumulates the WHOLE element — its low byte is exactly Σnb because carries propagate only upward. Our table already had that layout (nb in the low nibble ⇒ low byte = nb), so adopting the discipline was arithmetic-only. Result ([encoding](../perf/encoding.md)): dll100 Ir l1 −4.0% / l3 −2.0% / l9 −0.4%, byte-identical everywhere, gungraun encode json fastest −1.6%/text −1.0%, wall dll100 l1 +0.8% interleaved. The stream loop now sits at libzstd's per-symbol floor; what remains of the +23%-of-gap item is gone.

## T4: emit-side verdict on the "live-set shrink" hypothesis

Summing the serialization side at every tier — ours (`encode_sequences` + huff stream + histograms + builds) vs theirs (`encodeSequences` + `seqToCodes` + HUF + HIST + builds): l1 1,156 M vs 1,140 M (parity), l3 823 M vs 969 M (ahead), l9 538 M vs 695 M (ahead). **The snapshot's "emit live-set shrink" lever was not the dll gap** — the emit path was at parity or ahead before this round; the huff-stream loop (T3) was the single exception and is closed. The remaining excess is matcher-side: the fast/dfast scan loops (+414/+538 M, whose register-budget levers are measured out — see [negative](../negative/matchers.md) const-log/OR-fold/restructure entries) and `insert_covered` (+425 M at l1, uop-volume-bound at ~9-14 Ir/insert; every fill-thinning policy costs size on json/text, and the density-gated stride-2 form already shipped).

## Remaining lever ranking (differential evidence)

1. balanced: row-based candidate storage (libzstd's `ZSTD_RowFindBestMatch`) — the only lever that touches the latency mechanism; a matcher rearchitecture, not an attrition item.
2. fastest/fast: scan-loop instruction volume — the loop's register budget re-rolls on every added live value; only register-NEGATIVE changes convert (landed examples: const-log instantiation, fused table allocation). No identified removable live value remains.
3. `insert_covered` fill density — the speed-priority knob (dual-anchor everywhere, +7.3% wall at l1 for +0.46% dll size) stays rejected on the size axis; a const-log instantiation of the outlined body would save ~1 Ir/insert (~0.7% of l1), below the noise floor.
