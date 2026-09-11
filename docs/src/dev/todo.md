# Todo list

> Reconciled against the Changelog (as of `27b91cf`, 2026-09-10): items listed in working documents but already landed no longer appear (e.g. json.Best ratio→closed by the opt parser, MT ratio retention→`a37ebaa`, streaming MT→`27b91cf`, table entry u32→`327bc99`). Falsified sub-directions are in [falsified directions](negative.md); do not resurrect them via this list.

## P0 · Structural

1. **Parallelize MT decode stage B** (reachback analysis + rep history prefix scan)
   - Leapfrog dimension: libzstd has no MT decode; done well this jumps from a 0.66-1.0× liability to exclusive leadership.
   - Status: mt2-16 has no scalability (serial stage B accounts for 45-80%; match copies account for 80% on skewed.zst9).
   - Approach: predict per-segment reachback depth to choose split points; replay rep prefixes ahead of time using stage A's sequence stream.
2. **MT decode path checksum verification**: currently explicitly unverified; the decoder already auto-verifies on the ST path (`81d2119`); complete the MT path and re-check mismatch error behavior.
3. **Wide-matrix re-measurement**: encoder-side numbers date from around the tangled-chain fix (matrix §3/§4/§5); since then the opt parser (Best core swap), u32 table entries, MT ratio retention, stride-3 prefill, and streaming MT have landed; the §4 MT table is explicitly void. The zstdx-bench `matrix` tool is ready — run a round and update the [matrix page](../bench/matrix.md) and the [snapshot](snapshot.md).

## P1 · Encoding speed

4. **json.Fastest ~1.78×** (largest single-item speed gap): scan-loop gap vs zstd -1 (zstd uses dual-position ip0..ip3 software pipelining + prefetch + cmov decisions; we use two positions); the encode_sequences backward FSE loop at ~12.6% (87 inst per sequence; compare against `ZSTD_encodeSequences`); the literals huff0 path. Note that select-ification/cmov/context structuring are already done (`29bcb02`/`c528c57`); what remains is not matcher micro-technique — profile first to localize.
5. **json.Fast 1.35× residue**: per-sequence cost candidates — encode_sequences 12.6%, three-channel SIMD for the choose_tables_fast histograms, const-generic log to eliminate the `shr %cl` variable shift.
6. **Balanced speed** (~1.2× vs zstd-9 on json; skewed already overtaken via prefill): shift focus to **per-probe/per-position cost** — instruction count of the chain-walk beat-check/unpack sequence and the emit path (localize with enc_prof + perf). Miss-segment strategy directions (stepping/insertion scheduling) were falsified twice and are closed.
7. **random low tiers 1.18-1.46×**: per-block overhead of raw blocks (zstd raw blocks are near-memcpy); suspects: fixed per-block cost (decision/stage/emit), dfast miss-stepping zeroing between blocks, chain walking full 8-deep garbage chains. Profile before opening a work item.
8. **Best/Opt/Ultra speed**: ratio already ahead/level; speed is the remaining dimension — text Opt 313 vs libzstd ~440; the per-block DP cost behind Best's 12-19 MiB/s; streaming MT per-job fixed cost at the best tier (text.best streaming 0.8× its own ST).
9. **Small/mid payloads (4K-1M)**: json/text mid-size ~2× (per-byte cost of scan+emit+table build, not fixed overhead). Done (2026-09-11): `build_from_weights` counting sort + `package_merge_lengths` scratch rework (u32 Ent, swapped buffers); text-4K +8% cumulative, byte-identical output. Remaining: the scan/emit per-byte cost and the package-merge algorithm itself (still O(11·n log n) compares per table).

## P2 · Ratio and features

10. **Close the dictionary-encoding loop**: encoder support for using dictionaries (frame header dictionary_id always None); fix the `dict/` training bugs (epoch buffer hardcoded `vec![0;100]`, scoring iterating `collection_sample` instead of the current epoch) or just port the C fastCover.
11. **LDM**: gear-hash long-distance matching; decoupled from bare window expansion (bare 2-4 MiB expansion is proven a double loss, but LDM is untested); can reuse existing matcher fusion (C's optLdm long-distance candidate injection is a reference).
12. **Streaming MT residue**: per-burst `thread::scope` spawning of 8 threads (~0.5-2ms/burst; a persistent thread pool is out-of-scope redesign); the full strip-prefill cost for unpledged 1MiB jobs (design trade-off; pledging users can already get bulk-equivalent efficiency); the per-job NT table-clear fixed cost behind zeros.Balanced 0.8× (RLE corpus, low priority); **stream-MT job-boundary ratio penalty** — stream-mt sits up to +0.44% above our own bulk (text.fastest 109007 vs 108531, json.ultra +0.17%, text.balanced/fast +0.16%) because burst jobs split differently than the bulk whole-input splitter; unify the job sizing (2026-09-11 ratio sweep).
13. **Encoder ratio residues** (2026-09-11 ratio sweep, `zstdx-bench ratio`; only cells we lose vs libzstd): text.Balanced **−2.3%** (361.4 vs zstd-6's 370.2, the largest deficit — chain-matcher match selection on long-range-repeat data; compare against zstd-6 lazy2 on the tiled corpus); text.Ultra −0.6% (411.6 vs 413.98, btultra2 price/convergence on huge repeats, marginal); skewed opt/ultra/stream-mt −0.03..−0.09% (16-byte-alphabet fine residue, low priority); everything else at parity or denser. Anomaly worth understanding: json.opt **bulk-mt is 0.44% denser than bulk-st** (4478237 vs 4498011) — job splitting accidentally helps the opt parser on json (per-job entropy reset / window restart aiding price seeding?); if understood, ST opt may mimic it.
14. **Long-term**: superblock (small-data ratio), preSplit content-adaptive chunking, AVX2/SSE2 intermediate SIMD tiers (the stable pattern of nested `#[target_feature]` + non-always `#[inline]` is verified; the risk is codegen interference between the N instantiations), C FFI + zstd CLI argument-surface alignment.

## Shelved (definitive conclusions; do not restart blindly)

- **Aligning frame-header strictness with libzstd**: libzstd rejects nonzero FHD reserved bits (bits 4-3) ("unsupported frame parameter") while zstdx accepts permissively (fuzz crash-b5593b58, second frame FHD=0x38, is exactly this shape). Tightening is a behavior change; run the interop corpus first to confirm no collateral breakage before touching it.
- **Streaming decode decode_step rework** (json/skewed residue 1.05-1.3×): the register wall is proven (~20 live values > 15 GPRs; three structural reworks falsified in a row) — no headroom unless live values shrink first; the next order of magnitude requires an algorithm-level change like libzstd's 8-deep sequence ring-buffer pipeline; assess benefit/risk first.
- **read::Encoder pump double copy**: reading directly into spare capacity would require handing uninitialized buffers to arbitrary Read impls; the trait contract forbids it; evaluated and dropped.
- **BMI2 dispatch Intel measurements**: retained as neutral zero-cost on Zen4; add data when an Intel machine is available.
- **madvise(MADV_HUGEPAGE)**: redundant on THP=always machines; retryable on THP=madvise machines (the extern "C" declaration route with zero new dependencies is verified).
