# Falsified directions: entropy, checksum and misc

Every entry here is backed by A/B data. If the premises change, such as the table structure, window, corpus or CPU, re-evaluation is allowed, but new decomposition evidence must come first. The differential-testing discipline is in [benchmarking methodology](../bench/methodology.md), and the rules are in the [index](../negative.md).

## Block decisions

### Block-level pre-gate

The idea was to sample whole-block entropy and emit the block raw when the sample crossed a threshold, skipping the scan. It is infeasible because a byte histogram cannot distinguish pure randomness from randomness containing long-distance repeats. libzstd likewise applies `suspectUncompressible` only to literals.

### Json.opt density gap

The json.opt density gap was hunted in the search and parse. A reference-frame block dump falsified that: the reference emits 447 blocks for our 257, because libzstd's `ZSTD_deriveBlockSplits` cuts nearly every json 128K block at entropy shifts. The gap is a missing feature, block splitting gated on `strategy>=btopt && windowLog>=17`, and not a parameter. The serialization alignment closed everything else, from -0.31% to -0.06%.

### Post-parse splitter expansion

The idea was to expand the landed post-parse splitter beyond its gate, with more rows and shapes and a C-side A/B through a parameter-switch tool over libzstd 1.5.7, run on 2026-09-23. The post-split measurement through `ZSTD_c_splitAfterSequences` reads 0.0000% net on every corpus cell, json, text, skewed and dll100 at L9, L13, L17 and L19, because the sequences-entropy estimator never found a net-positive split beyond the default. C's own splitter is at its corpus optimum, and our remaining json and text deltas are byte-parity residues, not headroom. Do not retune thresholds or widen the gate.

### Superblock splitting

The idea was to port `ZSTD_compressSuperBlock`'s sub-block splitting for the small-data ratio, with several sub-blocks per block: the first carries the entropy tables, the rest repeat or go treeless, and the raw tail block closes it. The experiment ran on 2026-10-01. The CLI's own superblock output loses on the ratio path, with json 64K at +150/+104/+85/+125 B at l1/l3/l9/l19 and text 64K likewise. At 1K and 4K it never splits because `estBlockSize < targetCBlockSize`, and its -7 to -8 B win is the single-sub-block literals layout alone, landed standalone as `LitStreams`.

`targetCBlockSize` serves fixed-size transport chunks with a minimum `ZSTD_n_MIN` of 1340, and not ratio. Per-sub-block entropy decisions cost more than shared tables save on every measured shape. The proposed two-block form of a raw literals block followed by a sequences-only block is strictly dominated by the in-block raw literals section we already emit, which adds 3 B of extra block header and 1 B of empty literals section. Do not port the splitting machinery for ratio. Revisit it only behind an explicit `targetCBlockSize` API.

## Scan and insertion

### Speculative uniform scanning

The idea was to scan uniformly, roll back and rescan on a mismatch. It cost text 8%, because 47% of bytes are long runs at block starts and rollback re-hashing doubles the cost. The correct semantics are absorb and continue.

### AVX-512 insertion loop

The idea was to vectorize the 16-or-fewer full-density insertion loop with AVX-512 vpmullq. The hash microbenchmark ran 2.2x faster with no gain on the full loop, because the bottleneck is random store throughput into the 256 KB table, the store floor. Approaches that only optimize hash computation are all ineffective.

This was re-confirmed on 2026-09-11 on the outlined `insert_covered`, with a vpermb 16 B load, 8 lanes, vpmullq, a stack spill and 8 scalar stores. It cost +3.4% instructions against the scalar loop, because the 64 B stack round-trip plus eight extract-loads costs more than the scalar per-position sequence. It was re-confirmed again with a leaner srlv-built-lane variant using 2 u64 loads, vpsrlvq, vpandq, vpmullq and scalar stores. That variant cost +3.6% Ir and moved json.fastest wall from 532 to 504 MiB/s. With at most 2 batches per call and ml at most 16, the dependent vector chain never amortizes. Do not retry.

### Cheaper exact binning

The idea was cheaper exact binning of the gate histogram through wider lanes, u64-load extract feeds or a deeper unroll. It was falsified on paper after perf-annotate on 2026-09-12. The 4-lane loop already runs at the x86 mem-op floor, with 3 mem-ops per byte from a movzbl load and an RMW load plus store, at 3 mem-ops per cycle, giving the measured 1.04 cycles per byte. u64-load extracts trade byte loads for shift uops that LLVM load-narrowing undoes, and unrolling only drops ALU loop overhead off the port-bound path. An exact per-byte count cannot go below 1 RMW per byte, so the pass can only be skipped through the sticky hold, never cheapened.

## Sequence encoding and table selection

### Transition-row prefetch

Prefetching the `encode_sequences` transition row with prefetchT0 raised instructions 3.5% with no cycle improvement. ILP already covers the latency, and the prefetch only adds noise.

### Package-merge buffer reuse

The idea was to reuse the package-merge back-walk buffer with two swapped u32 Vecs instead of a fresh Vec per level, in borrow-iter and drain-iter variants. It removed 9 malloc and free pairs per table, but text fastest and fast Ir rose +0.08 to +0.14% at 1 MiB and the text-4K wall rose +0.5%, within noise. The by-value fresh-Vec pattern of `for id in active` plus `active = next` codegens measurably better, because the move lets LLVM drop buffer state across iterations. Allocation count is not the walk's cost.

### SeqWord 24 to 16 bytes

The idea was to pack `add_nb` into the codes word's high byte and drop the field. json.fast Ir rose +1.12% and text.fast +0.97%, because the packing cost of a width sum and a shift-or lands in the matcher's emit path, whose per-sequence weight exceeds `encode_sequences`'. The consumer only trades one byte load for one shift. Operand-supply savings must not be bought inside a hotter producer.

### choose_tables_fast residue

The -1 formatted fast-row residue was hunted in `choose_tables_fast`'s dictionary-seeded sequence conversion, covering repeat gating, predefined preemption and the treeless cost model against `ZSTD_selectEncodingType`. A per-block decomposition on 2026-09-18 falsified it: sequence sections were 259 B ahead of the CLI, which used 155 blind `set_repeat`s against our cost-picked 116 Rep, 20 Pre and 15 RLE. The entire +2.0% sat in the literals sections, with 24 raw blocks against the CLI's 51 treeless, behind the gates' flat description term. The cost compare is a favorable deviation from C's fast heuristic of `nbSeq < 1000` leading to `set_repeat`. Do not port the blind repeat. The landed fix is the literals-gate repricing in [encoding](../perf/encoding.md).

### Blind repeat on dictionary frames

The idea was to port C's blind repeat of live FSE tables below 1000 sequences to dictionary frames anyway. It ran on 2026-09-18 with the fast rows' `staticFse_nbSeq_max` shortcut on top of the width-4 parse. Fixture holdout on formatted dicts read -3 +101 B and -5 +93 B summed, or +8/+9 B per file. This encoder's parse distributions fit the dict-seeded tables worse than libzstd's own parses do, and C's blind accept wins because its rows emit the parses the dict tables were trained on. Keep the landed three-way cost compare.

### Exact fresh-table pricing

The idea was exact fresh-table pricing in the dictionary-seeded FSE selection, building the normalized table and costing the histogram through its real probabilities instead of the entropy bound plus description term. No decision flips on the fixture's fast rows on 2026-09-18. On these small histograms the bound understates the fresh bitstream by a near-constant margin, so bound against exact moves both contenders together. The landed compare is decision-equal at a fraction of the cost.

### Unconditional lazy+ selection on multi-block frames

The idea was to port libzstd's unconditional lazy+ exact three-way sequence selection to multi-block no-dict frames at levels 6 and above everywhere, as C runs it. The experiment ran on 2026-09-23. On the 32 MiB corpus, json read l9 +4423, l13 +3179, l17 +2108 and l19 +6037 B, skewed read l13 +33, l17 -9823 and l19 -6926 B in a mixed result, and text l19 +2 B. On 1 MiB json, l9 and l19 read +68/+94 B. This encoder's parses fit repeated tables worse than libzstd's own, the no-dict twin of the dictionary-side finding. The heuristic's rebuild bias is ratio-safe on multi-block frames, and the exact arm stays gated to single-block frames of at most 128 KiB, where only the predefined or fresh choice diverges.

## Literals and Huffman

### Big-Huffman-block residue

The +2 to +3 B per big Huffman block residue was hunted as a Huffman stream or tree divergence on 2026-09-16. Weight-table decoding falsified it: block-level description dumps show the streams within 1-2 B and the length assignments equal-cost. The gap was the weight FSE description built by the legacy normalization, and the aligned port is described in [encoding](../perf/encoding.md). The tree-shape tie residual of about 1 B per fresh table, from package-merge against `HUF_buildCTable` ordering, is accepted. Aligning tie-breaks would fork the length assignment for no measurable corpus gain.

### Blind treeless acceptance for literals

The idea was to port libzstd's `HUF_flags_preferRepeat` blind treeless acceptance for literals, which applies to rows below `ZSTD_lazy` and `srcSize <= 1024` in `ZSTD_compressLiterals` and accepts any live covering table after the histogram heuristics. It is the Huffman counterpart of the two FSE blind-repeat falsifications above. The no-dict corpus was byte-identical everywhere the code can fire, over a 5-shape by level-1-to-22 full-ladder dump and a 120-cell sweep including MT and stream. dll100 read l1 +57 B and l2 +56 B, or +0.0001%, and l3 to l5 were identical. The dict holdout formatted rows were all worse: -1 +5 B, -2 +5 B, -4 +4/+5 B and -5 +5 B, while raw-dict and no-dict rows stayed unchanged. A small band of 1K to 64K at fastest and fast over 3 interleaved rounds read geo-mean -0.21% with mixed signs and no speed change.

The blind accept fires exactly where the landed exact treeless-against-fresh comparison already chose a fresh table, so overriding it keeps a worse table. No threshold in the family wins, because a smaller one shrinks toward zero overrides. The exact compare is the family's optimum wherever it is affordable, and it is affordable: our fresh build runs at 0.57x C's own, and the regime is rare without a dict, since 32 MiB shapes produce no block with literals of at most 1 KiB and a live table at any level. The -1 and -2 entropy-side dictionary residue is not the treeless acceptance semantics, as established on 2026-09-21.

### Small-payload gap

The small-payload gap of text 1-64K at +7.5 to +13.7% on l1 and l9 was hunted in the entropy sections: FSE tableLog scaling at low nseq, the predefined-against-fresh bar, the literals reject-floor margins in `compress_literals` and `sampled_gate_rejects`' flat description term, window and table clamping at wLog 10-12, and checksum-flag parity. Frame-parser section diffs on 2026-09-21 falsified it. Sequence sections were at parity on every diffed cell, the reject margins moved at most 10 B, both CLIs default the checksum on with a 4 B constant on both sides, and `adjust_params` already matches `ZSTD_adjustCParams`' clamp. The gap decomposed onto frame shape at +4 B per frame, the srcSize-keyed cParams row, and the l1 parse bars. The entropy side keeps only the tail: literal Huffman descriptions at +6 to +10 B, where the `HUF_optimalTableLog` cap landed, and the json l1 1-4K residue at +29 to +112 pm, from description bytes at tiny sizes, recorded in [encoding](../perf/encoding.md).

## Bit I/O, histograms and dispatch

### Residual byte stores in write_bits_64_cold

Turning the residual bytes of `write_bits_64_cold` into stores made wall clock swing wildly with instructions unchanged. A hotspot below 0.5% is scheduling noise, and the change wrecks instruction layout.

### Histogram counter width

Changing histogram counters from u64 to u32 is flat. On x86, `inc qword` and `inc dword` have identical throughput.

### Removing checks from the Uniform-4 loop

Removing the checks from the Uniform-4 bulk loop cut instructions 19% but cost wall clock 7%. Branch prediction is free, and the microarchitectural change degrades instruction addressing and pipeline scheduling.

### PGO

Profile-guided optimization trained on the full corpus gave zero gain. It is not worth the build-chain complexity.

### Integerizing the entropy pre-check

Integerizing the entropy pre-check left instructions flat. 92% of the time is in the histogram `incq`, and the f64 entropy computation is near 0%.

### Entropy-scratch pooling on random cells

The effect of entropy-scratch pooling on the raw-heavy random cells was measured on 2026-09-16. random-8K wall fell 5% with flat Ir at -0.09%, while random-4K read +0.9% Ir and wall within plus or minus 2%. The movement is a codegen layout shift and not work, because the change touches none of random's paths and raw blocks build no tables. Gate this class of change on Ir plus the json, text and skewed band, and not on single raw-cell wall numbers.

### AVX2 tier for the LDM checksum mask

The idea was an AVX2 tier for the LDM `checksum_mask`, with four quarter-bucket loads and `vpsrlq`, `vpcmpeqq` and `vmovmskpd` per quarter, run on 2026-10-01. On dll100.fast the cell was flat: forced AVX2 read 448, forced scalar 450 and default AVX-512 453 MiB/s, as medians of 6 interleaved same-binary env-flip rounds with overlapping ranges. Ir read -0.30%, from 15.912G to 15.864G per 5 passes, against a total tier gap of -0.83% on the cell. The inherited perf-record attribution of +2.3% of cycles inside `ldm_generate` does not survive to end-to-end wall. The cell's whole SIMD tier spectrum is worth at most 0.8% Ir and 0.7% wall, so no AVX2 kernel lands, and the decode-side sibling reached the same no-tier verdict for its sites. The kernel's unit test matched the scalar mask exactly, so the falsification is cost-side and not correctness-side.

## Residual attribution

### Text Opt and Ultra parser residue

The text Opt and Ultra ratio residue was hunted as a parser-side divergence in the opt price model, match finder, rep injection and backtracking against `zstd_opt.c`. It was falsified by decomposition on 2026-09-16. `seqstats --ref-frame` at l17 and l19 showed 12 sequences differing out of 27k, with the entropy bound 12 B better for us. Block-aligned payload diffs showed the sequence sections byte-identical with +2 B on every near-empty literals block. The residue was `raw_literals`'s hardcoded 20-bit size format, and the landed ladder is in [encoding](../perf/encoding.md). The opt parse is parity-verified against C at the event and byte level, so do not reopen the price-model or finder audit space for this symptom.
