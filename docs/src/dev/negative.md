# Falsified directions (do-not-retry list)

> Every entry is backed by A/B data. If premises change (table structure, window, corpus, CPU), re-evaluation is allowed, but new decomposition evidence must come first; differential-testing discipline in [benchmarking methodology](bench/methodology.md).

- [Decoding](negative/decoding.md) — fused-loop restructures (batch splits, ring pipeline, SIMD copy scheduling), repcode/register work, X1/huffman/FSE/xxhash loop attempts.
- [Encoding · Matchers](negative/matchers.md) — hash4/rep probing revivals, window/lazy/insertion variants, DUBT/LDM ports and knobs, scan-loop and emit-path restructures.
- [Encoding · Entropy / Checksum / Misc](negative/entropy.md) — raw pre-gates, bit-writer/histogram forms, package-merge, PGO, AVX-512 insertion loops.
- [MT / Streaming](negative/mt-stream.md) — frame-per-job, overlap/prefill/seeding, stage-B parallelization, checksum absorb placements.

## Methodology

- blindly going branchless to cut branch-misses: stub-attribute first; halving misses ≠ time benefit.
- chasing byte-identical output vs libzstd: format compatibility suffices; byte equivalence is only a regression-testing tool.
- the fused loop's register budget is saturated: default any "optimization" that adds live values to failure.
- any new weak probing source (hash4 side table / 3-rep / rep2 tail probe) must first be A/B'd on three-corpus ratios — the "weak candidate steals positions" lesson has been confirmed repeatedly.
