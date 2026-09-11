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
| lazy-walk cheapening variants (depth-1-only margins; cap 4 steps; skip the second-chance search after an empty probe; rep probe when the incumbent is already a rep) | depth-1 collapses the ratio (json 6.55→6.18, text back to 361.5); cap and empty-skip change nothing / trade 0.05 ratio for 2.5% speed; rep-vs-rep probe never alters selection (output byte-identical without it) | the alternating depth-1/depth-2 margin structure IS the gain; the walk is not deep, it is the 2-searches-per-failure baseline that costs the ~30% json instructions |
| emit skipping positions already inserted by scan pairs (idempotent insertion) | output DIFFERS / instructions +2.6% after the fix | not unconditionally idempotent (hash-collision overwrites during backward extension; redundant reinsertion serves to restore slot values); the cost also exceeds the benefit |
| steady/tail scan-loop split (fold `pair_len` to constant 2 via a full-pairs steady loop + single-position tail epilogue) | fastest json/text Ir −3.3%/−2.7% but text.fastest wall −7% (12201→11347 MiB/s, reproducible in both prof and interleaved matrix) | the third macro instantiation displaces the hot blocks — at 12 GiB/s the loop pays pure layout noise; instruction-count wins that don't survive contact with the layout lottery are not wins |
| dfast position-u64 carry (ride v0 = read8(ip) across the inner-loop back edge; feeds the next short hash, the long/short probes' current-side compares and the rep pre-probe — 4 loads per position saved) | fast json/text/skewed/random Ir +3.6..+6% (json 31.80→33.12M), wall flat | the removed reloads are L1 hits; the pinned register displaces hotter pipeline state into spills — same lesson as SeqWord 24→16B: **operand-supply savings must not be bought with a live value inside a register-saturated loop** (contrast: deriving `cur` from the hash load *within* one iteration, where no new live value crosses the back edge, is a clean win) |
| naive Predefined/Repeat thresholds in choose_table | negligible | later handled by the cost-comparison approach (selectEncodingType port) + repeat mode; do not use naive thresholds |
| store gate priced by average residual literal entropy (per-block feedback of bits/byte, gate `ml*cost ≥ ilog2+7`) | text.balanced 346 (−6.5% vs zstd-6), json +16% | the average anti-predicts: text's residual stream is hyper-skewed (~0.2 bits/B — the matcher hoovers up everything matchable) yet wants the LOOSE gate; the displaced literals' marginal cost (their code length under the block's Huffman table) is the right price and IS the implemented form |

## Encode side · entropy / checksum / misc

| Direction | Result | Conclusion |
|---|---|---|
| block-level pre-gate (sampled whole-block entropy ≥ threshold → raw directly, skipping the scan) | infeasible | **a byte histogram cannot distinguish pure randomness from randomness containing long-distance repeats**; libzstd likewise only applies suspectUncompressible to literals |
| speculative uniform scanning (roll back and rescan on mismatch) | text -8% | 47% of bytes are long runs at block starts; rollback re-hashing doubles the cost; **the correct semantics are absorb-and-continue** |
| encode_sequences transition-row prefetch (prefetchT0) | instructions +3.5%, no cycle improvement | ILP already covers the latency; prefetch only adds noise |
| turning write_bits_64_cold residual bytes into stores | wall clock swings wildly, instructions unchanged | a <0.5% hotspot is pure scheduling noise that wrecks instruction layout |
| histogram counters u64→u32 | flat | x86 `inc qword`/`inc dword` have identical throughput |
| removing checks from the Uniform-4 bulk loop | instructions -19% but wall clock -7% | branch prediction is free; the microarchitectural change degrades instruction addressing and pipeline scheduling |
| AVX-512 vectorization (vpmullq) of the ≤16 full-density insertion loop | hash microbench 2.2× faster, no gain on the full loop | the bottleneck is random store throughput into the 256KB table (the store floor); **approaches that only optimize hash computation are all ineffective**. Re-confirmed 2026-09-11 on the outlined `insert_covered` (vpermb 16B load → 8 lanes → vpmullq → stack spill → 8 scalar stores): +3.4% instructions vs the scalar loop — the 64B stack round-trip plus eight extract-loads costs more than the scalar per-position sequence. Re-confirmed again with a leaner srlv-built-lane variant (2 u64 loads → vpsrlvq/vpandq/vpmullq, scalar stores): +3.6% Ir AND json.fastest wall 532→504 MiB/s — with ≤2 batches per call (ml ≤ 16) the dependent vector chain never amortizes; do not retry |
| PGO (trained on the full corpus) | zero gain | not worth the build-chain complexity |
| integerizing the entropy pre-check | instructions flat | 92% of time is in histogram incq; the f64 entropy computation is near 0% |
| package-merge back-walk buffer reuse (two swapped u32 Vecs instead of a fresh Vec per level; borrow-iter and drain-iter variants) | 9 fewer malloc/free pairs per table, but text fastest/fast Ir +0.08..+0.14% at 1 MiB and text-4K wall +0.5% (within noise) | the by-value fresh-Vec pattern (`for id in active` + `active = next`) codegens measurably better — the move lets LLVM drop buffer state across iterations; allocation count is not the walk's cost |
| SeqWord 24→16 B (pack `add_nb` into the codes word's high byte, drop the field) | json.fast Ir +1.12%, text.fast +0.97% | the packing cost (width sum + shift-or) lands in the matcher's emit path, whose per-sequence weight exceeds encode_sequences'; the consumer only trades one byte load for one shift — **operand-supply savings must not be bought inside a hotter producer** |

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
