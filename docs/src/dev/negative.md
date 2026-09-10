# Falsified directions (do-not-retry list)

> Every entry is backed by A/B data. If premises change (table structure, window, corpus, CPU), re-evaluation is allowed, but new decomposition evidence must come first; differential-testing discipline in [benchmarking methodology](../bench/methodology.md).

## Decode side

| Direction | Result | Conclusion |
|---|---|---|
| splitting the fused loop into batch processing (decode-batch→exec-batch dual loops, 3 variants) | -15~-21% | the intermediate Vec round-trip is not the bottleneck; batch buffering breaks LLVM register residency and loses fused out-of-order overlap |
| two-stage batch pipeline (batch 16, codegen fully on target) | cycles +9-15%, miss +14-30% | the interleaved decode/exec branch stream inside the fused loop carries the TAGE/BTB load; splitting it into two pure streams destroys history correlation — **the fused structure is a local optimum** |
| repcode registerization + fully branchless parsing (inside the fused loop) | json -2%/miss -22% but skewed +8-11% | register budget saturated: +3 live values necessarily evict hotter ones; branch-elimination gains < spill cost |
| purely branchless repcode (cmov chain + `%3` fold pseudo-slot) | json regresses 5-6% | the repcode branch predicts surprisingly well; **removing branches ≠ speedup**; final version = 1 cmov on the parse side + a single branch on the update side |
| packing 3 FSE states into one u32/u64 | regression | pack/unpack lands on the serial FSE chain, costlier than the spills it saves |
| BMI2 conversion of the 4streams X1 loop | instructions -12.7% but json/skewed -2~3% | front-end/code-layout effects eat the gains; this loop at 1.42 cyc/symbol ≈ the theoretical limit |
| copy-loop 32B pairing rework + AVX2 copy32 | miss drops hard (json -22%) but wall clock regresses | **halving misses ≠ time benefit**; json-class workloads are not miss-dominated |
| copy_inline hand-rolled inline copy (large-copy corpus) | memmove 33%→1.15% but zero end-to-end gain | large copies are bandwidth-bound necessary work; no conflict with wildcopy succeeding on small-copy corpora |
| writing literals directly into flat out | premise does not hold | literals and matches interleave in the output; contiguous layout cannot no-op; direct writing only saves the staging, not the number of writes |
| further X2/X1 huffman loop optimization | closed | ~0.75 cyc/B is the same order as libzstd's hand-written asm; no conventional path for SIMD bit-serial huffman |
| further FSE table-build optimization | closed | after 4.9%→1.9% every individual item is <4% |
| xxhash AVX-512 vpmullq vector core | worse instead | vpmullq latency (≈4cyc) breaks the serial chain; twox's 4 chains already hug the scalar machine limit; **the right answer = 8-chain scalar interleaving** |
| madvise(MADV_HUGEPAGE) | within noise | redundant on THP=always machines (rolled back; retryable on THP=madvise machines) |
| select-ifying the wrapped branch | miss unmoved, json -2.9% | this branch (74% taken, drifting with pos) already predicts well; **stub-attribute before select-ifying** |
| making ll>0 unconditional (copy16 even at ll==0, garbage gets overwritten) | skewed miss -62% but text +9.3% | the reordering effect of removing the branch itself hurts critical files (TAGE/BTB history) |
| inlining the fast path into the fused loop | json -1.2% | hot-loop layout disturbed; **new code always goes into an existing out-of-line callee** |
| instantiation slimming (shrinking the HEADROOM=false build) | no mechanism to help | the streaming hot path only instantiates HEADROOM=true; outlining error paths risks homing |
| AVX2 const threaded dispatch (8 instantiations) | feasible but regresses | the nested target_feature pattern works; codegen risk is high (kept on the intermediate-tier todo) |

## Encode side · matchers

| Direction | Result | Conclusion |
|---|---|---|
| hash4 revivals (@2^13 short-table side table / 16KB distance cap / @2^14 single table / rep0 fallback; 4 attempts) | all failed (json 4.18-4.93, all worse) | table pollution from high-frequency 4-byte patterns is irreversible; **hash5 is settled** |
| hash4 near-distance side table (ml≥6 gate; cures pollution) | speed -7~-14% across the board, ratio flat | ml≥6 does cure the pollution, but the gains cannot cover the dual-table cost |
| 3-rep probing (probe rep1/rep2 as well) | hit rate 7.6%→26% but json/text/skewed ratios all worsen | weak rep matches steal positions from longer hash matches; libzstd-fast probing only rep0 is right; a trailing rep2 probe also degrades slightly |
| rep0 threshold 4→5 | neutral | ml=4 rep matches are too few |
| HASH_LOG 17 | json +0.6% but speed -8%, skewed ratio worsens | |
| tiered MIN_MATCH (large offsets require ml≥5) | zero effect | far ml=4 matches are rare anyway |
| Fastest window <448K or 2MB | 256K/384K catastrophic (text ratio 301→7.88); 2MB worse | cutting the tile period is catastrophic; 512K workable but 0x70000 is more cache-friendly (768K later unlocked cross-tile) |
| dfast window 2MB | json ratio -1.1% and 4% slower | far candidates evict near-and-better matches from the single-probe slot |
| chain window 4MB (W4M) | double loss on every shape, skewed 14 MiB/s | finds farther but not longer; offset code-position loss > length gain; window expansion must come with LDM |
| porting C's anchor-distance miss formula to fast/chain | regression across the board (skewed scan description ~80× slower) | we use stride-1 dense insertion + newest-wins single-candidate tables; per-byte probing overwrites good far entries with garbage; the C formula's premises (sparse table + large hLog) do not hold. Holds for dfast only (`0b46f21`) |
| full lazySkipping port (chain) | flat over 11 interleaved rounds (skewed +3.1%, text -0.7%) | the C mechanism's two levers (dense→sparse insertion, slow→fast stepping) are already spent in our design; **miss-segment strategy directions are closed** |
| pure dual-anchor table insertion (libzstd fast default) | json 5.71-5.90, text 275-290 | severe ratio loss; ≤16 dense insertion must be kept |
| in-match insertion step 1→2 | json ratio 6.02→5.97 | not worth it for json +2% speed |
| zero-slope full-density probing / slope relaxed to mc>>3 | json/skewed/random all collapse | insertion volume↑ → 2^15 table aging↑ → far matches die; the dynamic miss slope is essential |
| hLog16 (512KB table, 2^15 era) | speed -8-14%, ratio no gain | beyond-L2 locality |
| full u64 hashing (no 40-bit truncation) | json wins both ways but text ratio -7% | the 40-bit mask keeps mathematical equivalence; global ratio takes priority |
| table entry u64→u32 @Fastest 2^15 (2-bit epoch scheme) | output identical but instructions +0.09% | entry width is not a lever while the table lives in L2; contrast: at Balanced's 16MiB working set, u32 is a big win (`327bc99`) |
| emit skipping positions already inserted by scan pairs (idempotent insertion) | output DIFFERS / instructions +2.6% after the fix | not unconditionally idempotent (hash-collision overwrites during backward extension; redundant reinsertion serves to restore slot values); the cost also exceeds the benefit |
| naive Predefined/Repeat thresholds in choose_table | negligible | later handled by the cost-comparison approach (selectEncodingType port) + repeat mode; do not use naive thresholds |

## Encode side · entropy / checksum / misc

| Direction | Result | Conclusion |
|---|---|---|
| block-level pre-gate (sampled whole-block entropy ≥ threshold → raw directly, skipping the scan) | infeasible | **a byte histogram cannot distinguish pure randomness from randomness containing long-distance repeats**; libzstd likewise only applies suspectUncompressible to literals |
| speculative uniform scanning (roll back and rescan on mismatch) | text -8% | 47% of bytes are long runs at block starts; rollback re-hashing doubles the cost; **the correct semantics are absorb-and-continue** |
| encode_sequences transition-row prefetch (prefetchT0) | instructions +3.5%, no cycle improvement | ILP already covers the latency; prefetch only adds noise |
| turning write_bits_64_cold residual bytes into stores | wall clock swings wildly, instructions unchanged | a <0.5% hotspot is pure scheduling noise that wrecks instruction layout |
| histogram counters u64→u32 | flat | x86 `inc qword`/`inc dword` have identical throughput |
| removing checks from the Uniform-4 bulk loop | instructions -19% but wall clock -7% | branch prediction is free; the microarchitectural change degrades instruction addressing and pipeline scheduling |
| AVX-512 vectorization (vpmullq) of the ≤16 full-density insertion loop | hash microbench 2.2× faster, no gain on the full loop | the bottleneck is random store throughput into the 256KB table (the store floor); **approaches that only optimize hash computation are all ineffective** |
| PGO (trained on the full corpus) | zero gain | not worth the build-chain complexity |
| integerizing the entropy pre-check | instructions flat | 92% of time is in histogram incq; the f64 entropy computation is near 0% |

## MT / streaming

| Direction | Result | Conclusion |
|---|---|---|
| frame-per-job MT encoding | rejected at design time | multi-frame output burdens the decode side (unfriendly to single-frame consumers); stage-2 single-frame overlap jobs are the right answer |
| enlarging overlap only, without strip prefill | ineffective | insertion paths only insert job-local positions; strips never get indexed; enlarging overlap is mathematically ineffective |
| fixed-grid prefill to find periodic repeats | mathematically unsolvable | clustered 5-grams recursively bury twin slots with equal values / phase-alignment traps; dfast/chain suffer the same burial; **seeding at job start is mandatory** |
| huge pledge without a job_size cap | memory blowup | MAX_JOB_SIZE=1GiB (same as zstdmt) |
| thread_local pool across thread::scope bursts | always misses | scope threads are fresh each time; the pool must be owned by the encoder and carried across bursts |
| reading directly into Read's spare capacity (beyond the read-pump double copy) | dropped | handing uninitialized buffers to arbitrary Read impls violates the trait contract |

## Methodology

- blindly going branchless to cut branch-misses: stub-attribute first; halving misses ≠ time benefit.
- chasing byte-identical output vs libzstd: format compatibility suffices; byte equivalence is only a regression-testing tool.
- the fused loop's register budget is saturated: default any "optimization" that adds live values to failure.
- any new weak probing source (hash4 side table / 3-rep / rep2 tail probe) must first be A/B'd on three-corpus ratios — the "weak candidate steals positions" lesson has been confirmed repeatedly.
