# Falsified directions

This page indexes the directions that measurement has falsified, so they are not retried without new evidence. Each area has its own page with the A/B data, and the methodology section records the rules those falsifications follow.

Every entry is backed by A/B data. A change in premises, such as the table structure, the window, the corpus or the CPU, allows re-evaluation, but new decomposition evidence must come first. The differential-testing discipline is in [benchmarking methodology](bench/methodology.md).

- [Decoding](negative/decoding.md): decode and execute splits such as batch loops and the ring pipeline, the staging loops' register budget, wide-copy kernels, and X1, huffman, FSE and xxhash loop attempts.
- [Encoding · Matchers](negative/matchers.md): hash4 and rep probing revivals, window, lazy and insertion variants, DUBT and LDM ports and knobs, scan-loop and emit-path restructures, and the global SA/LCP candidate index.
- [Encoding · Entropy / Checksum / Misc](negative/entropy.md): raw pre-gates, bit-writer and histogram forms, package-merge, PGO, and AVX-512 insertion loops.
- [Dictionary Trainer](negative/dict.md): k-sweep metric level and form variants, grid geometries, bucket hashing, C-side table construction, and stats level and sample-set choices.
- [MT / Streaming](negative/mt-stream.md): frame-per-job, overlap, prefill and seeding, stage-B parallelization, the piece-decode hybrid overlap, and checksum absorb placements.

## Methodology

- Going branchless to cut branch misses needs a stub-attribute A/B first, because halving misses is not the same as a time benefit.
- Chasing byte-identical output against libzstd is unnecessary. Format compatibility suffices, and byte equivalence serves as a regression-testing tool.
- A piece that loses inside a register-saturated loop does not falsify a structure that replaces the loop as a whole. Before closing a gap as a floor, build the reference with the same compiler backend, such as libzstd under clang, to decide whether the gap is codegen or structure.
- A new weak probing source, such as a hash4 side table, a 3-rep probe or a rep2 tail probe, must first be A/B'd on three-corpus ratios. The weak-candidate-steals-positions lesson has been confirmed repeatedly.
