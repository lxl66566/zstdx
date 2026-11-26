# Decode Gap Against libzstd

This page answers where the single-thread decode gap against libzstd came from, what closed it, and what remains. The x notation is our time divided by the reference time, so values above 1 mean we are slower. The reference is libzstd 1.5.7, through the zstd crate in the matrix and as a locally built static library in the solo measurements.

## The reference-compiler differential

Until 2026-11-02 the fused sequence loop read x1.17 to 1.38 on json, skewed and dll. Rounds of single-change experiments had left it marked as a floor, because every instruction cut lost cycles to a register-allocation re-roll. The differential that broke this built libzstd 1.5.7's decoder twice from the same source, once with gcc 16.2 and once with clang 21.1.8, both at `-O3`, and timed both against zstdx on one pinned core. The table reads bulk decode into a reused buffer, best of 25, in MiB/s.

| frame | libzstd gcc | libzstd clang | zstdx before |
|---|---:|---:|---:|
| json.zst3 | 1808 | 1724 | 1500 |
| skewed.zst3 | 1575 | 1528 | 1282 |
| dll100.zst3 | 1645 | 1574 | 1269 |
| json.zst19 | 2622 | 2467 | 2315 |

Clang costs libzstd only 3% to 6%, while zstdx trailed the clang build by 14% to 19%. LLVM therefore compiles libzstd's loop structure well, and the gap belonged to our structure, not to the backend. Per-decode `perf stat` deltas against the clang build put zstdx at 1.24x the cycles and 1.13x the instructions on json.zst3, and at 1.25x and 1.22x on dll100.zst3. Branch misses were 1.78x and 1.72x, about 30% of the cycle gap at roughly 13 cycles each. The largest miss source was a branch on `ll > 0` ahead of the literal copy. json averages 0.63 literals per sequence, so that branch is close to a coin flip, and libzstd has no such branch.

## The libzstd-shaped loop

`execute_decoded_flat_inner` now follows `ZSTD_decompressSequences_body` as one structure:

- Each entry is read field by field through `EntryRef`, in libzstd's `ZSTD_seqSymbol` order.
- Repcodes resolve while decoding, through the `ofBits > 1` real-offset branch, and the history lives in locals for the block.
- Each add-bits field is a separate read behind a zero-width branch. A mid-sequence reload fires at 31 total bits.
- Literals copy 16 bytes unconditionally, and a zero length leaves garbage that the match overwrites.
- One combined check, literals past their end or a sequence end past the wildcopy limit, sends every edge case to the out-of-line exact path `exec_sequence_end`.
- A single compare against the active generation's start guards the match source. The window check and the wrapped-source copy run only behind it.

The batched single-read decode stays in `decode_step` for the staging paths, because MT stage A cannot resolve repcodes and keeps wire values.

Per-decode counts after the port, against the clang build of libzstd:

| frame | cycles | instructions | branch misses |
|---|---:|---:|---:|
| json.zst3 | 82.7M against 97.0M | 322M against 368M | 0.76M against 0.73M |
| dll100.zst3 | 329M against 339M | 1178M against 1215M | 3.08M against 2.64M |
| skewed.zst3 | 112.7M against 112.4M | 422M against 412M | 1.11M against 1.00M |

The `dec-st` matrix moved as follows, with x before and after:

| cell | bulk | stream |
|---|---|---|
| json.zst1 | 1.276 to 1.034 | 1.227 to 1.046 |
| json.zst3 | 1.375 to 0.988 | 1.313 to 1.021 |
| json.zst9 | 1.324 to 0.968 | 1.276 to 1.013 |
| json.zst19 | 1.177 to 0.996 | 1.173 to 1.041 |
| text.zst1 | 1.080 to 0.928 | 1.201 to 1.047 |
| text.zst3 | 1.001 to 0.954 | 1.072 to 1.020 |
| skewed.zst3 | 1.275 to 1.088 | 1.184 to 1.036 |
| skewed.zst9 | 1.362 to 0.906 | 1.283 to 0.961 |
| dll100.zst1 | 1.330 to 1.133 | 1.252 to 1.109 |
| dll100.zst3 | 1.377 to 1.070 | 1.315 to 1.068 |
| dll100.zst9 | 1.362 to 1.061 | 1.298 to 1.067 |
| dll100.zst19 | 1.350 to 1.032 | 1.307 to 1.050 |

Every piece of this structure had been tried alone on the old batched-read loop and recorded as falsified: the unconditional literal copy, the field-per-load entry, register-resident repcodes and libzstd's wildcopy issue order. Each one re-rolled register allocation inside a saturated loop and lost to the text-stream placement gate. The structure won as a whole by 14% to 51% on the dense shapes, with text gaining as well. When a hot loop is far from a reference implementation, the reference-compiler differential decides whether the gap is codegen or structure, before any single-change A/B closes it as a floor.

## The bulk cell's reference handicap

Before 2026-11-01 the matrix bulk cell's zstd closure called `zstd::bulk::decompress`, which allocates a fresh output vector per iteration, while our side reused its decoder and buffer. A fresh 33.55 MB allocation costs about 7,920 minor faults per decode, and the fault handlers read as reference-side time: 34% on skewed, 40% on json, 69% on text and random and 76% on zeros. The cell now reuses the reference's decoder and buffer through `decompress_to_buffer`, the same form the gungraun benches use, and its bulk and stream columns agree.

## What remains

- skewed.zst1 bulk read x1.30 against libzstd and lost to our own stream path, because the frame is 246 pure-literals blocks of 256 with 13 sequences in 32 MiB: decode was the X2 Huffman loop at 69%, the staging-to-output literals copy at 17%, the block-body staging copy at 7% and xxh64 at 11%. A zero-sequence peek now decodes such a block's literals straight into the flat target, the X2 loop dispatches through the BMI2 tier, and the one-shot path parses block bodies in place off the input slice. The bulk cell reads x1.09, skewed.zst3 bulk x1.00, json.zst3 bulk x0.90 and text.zst1 bulk x0.90, and gungraun l1 skewed reads 6.359M against libzstd's 6.362M Ir, from 8.680M.
- skewed.zst19 bulk read x1.25 after the 2026-11-03 table-build round. The 2026-11-04 decomposition split the gap per 32 MiB round into cycle deltas against libzstd: the X2 Huffman loop +5.3M cycles (ours 47.5M against 42.2M, x1.13), the sequence executor with its staged literals copy +5.0M (10.5M against 5.5M, x1.9, with 81 percent of our executor's samples in the 16-byte literal chunk loop), xxh64 at parity and the FSE builds diffuse. The loop and the executor each held about half the gap. The X2 loop's residue was codegen volume, not its dependency chain: the Rust body ran 243 instructions per iteration against the amd64 asm loop's about 200, at about 4.5 IPC for both, and a two-load Rust form that reached the asm's chain length re-rolled the register allocator and measured flat. The BMI2 tier now runs the loop as an inline-asm stretch in the asm loop's shape (`x2_inner_asm`), 202 instructions per iteration with the count byte feeding shlx straight off its load, and the Rust body stays as the scalar tier and the reference. Gungraun reads skewed l1 -14.5 percent Ir and l9 -1.55, and the locked matrix moved skewed.zst19 bulk to x1.137, skewed.zst19 stream to x0.978, skewed.zst1 bulk to x0.972 and skewed.zst9 bulk to x0.914, with json and text cells unchanged. The zst19 bulk residue was the staged literals copy, closed on 2026-11-04 by staging the literals inside the output buffer. The decomposition first pinned the mechanism: the executor's literal chunk loop ran at 4.0 B/c against libzstd's 14.2 B/c at equal whole-decode instruction counts (229.6M against 228.6M per round), the frame's 30.8K literal runs average 1084 B with a 1 to 2 KiB median bucket, and a kernel replay over the recorded run list held every schedule from the landed 1x16 loop through libzstd's 2x16 wildcopy, 32-byte chunks, memcpy forms and an AVX2 kernel inside 44 to 59 ps/B, so the loop shape was not the binding resource. The binding resource was the staging address: a separate 128 KiB staging buffer leaves every output line absent, and the copy pays a write-allocate DRAM fill per 64 B at the about 4 B/c per-core fill ceiling, while libzstd's `ZSTD_in_dst` arrangement decodes each block's literals into the output buffer past the block's write limit, which makes every output line cache-present before the executor writes it and moves the remaining fills into the X2 loop, which has about four times the cycles to hide them.

The flat one-shot path now ports that arrangement: blocks carrying at least 64 KiB of literals decode them into `out[written + block_out_max + 16 ..]` when the target holds the region with its pad and over-read tail, and the block executes through an out-of-line twin of the block body so the staged call site keeps its register allocation. The MT port of the same arrangement was measured on 2026-11-04 and falsified, because the staging core and the executing core differ there and the warmth crosses cores; the placement contract and the A/B are in [negative](../negative/decoding.md). The HEADROOM streaming paths refuse the placement, because the windowed buffer's previous generation physically sits above the write cursor where the staging region would, the same refusal as libzstd's `not_streaming` gate. json.zst3's 2.1M runs all sit at or below 32 B, so its executor keeps the scratch path at instruction parity, and a 2x16 chunk schedule that won skewed 3 percent was withdrawn after it re-rolled the fused loop and cost json 4.5 percent wall (the falsification is in [negative](../negative/decoding.md)).

The locked matrix batch, four position-balanced invocations per side against base d1b9f1aa, moved skewed.zst19 bulk from x1.135 to x1.045 with our absolute side reading 2629 to 2855 MiB/s at a reference side that drifted about 18 percent across the batch; a pinned-core interleaved pair read base 2602 to 2606 against branch 2750 to 2754, a 6 to 7 percent win. json.zst3, json.zst19, text.zst3 and skewed.zst1, zst3 and zst9 read flat on our absolute side, with the batch's x drift on those cells tracking the reference draws, and gungraun reads skewed l3 -1.05 percent Ir and l9 -0.54 with json and text within plus or minus 0.004 percent. The loop itself sits at asm parity.
- dll100 reads x1.02 to 1.05 on both paths after the 2026-11-03 table-build round. The 2026-11-02 attribution sampled `prof dec bench/big/dll100.zst3` (40 iters, one sample per 4003 misses) and a solo one-shot loop over the same frame against the gcc-built libzstd at `/root/programs/fork/zstd/lib`, and read 3.17M against 2.61M misses per decode, 1.22x. The same recipe re-run around the rank fill reads 3.29M before and 3.07M after against 2.70M for libzstd in that session, 1.22x to 1.14x. The lumps decompose by sampled symbol, with shares of each side's own total:

| miss source | before | after | libzstd |
|---|---:|---:|---:|
| Huffman table build | 0.22M (6.6%) | 0.03M (0.9%) | 0.018M (0.7%) |
| FSE sequence table builds | 0.27M (8.3%) | 0.26M (8.5%) | 0.17M (6.4%) |
| match copy loops, 16-byte chunk plus overlap8 | 0.56M | 0.56M | 0.50M |
| wrapped-generation streaming copy | 0.07M (2.3%) | 0.07M (2.4%) | none |

The Huffman lump closed the same round: `build_table_from_weights` now fills rank by rank, the port of libzstd's `HUF_readDTableX1_wksp` fill, and the build's own misses dropped from about 270 to 35 per rebuild over the frame's about 800 rebuilds, with the structure and the equivalence gate on the [decoding](../perf/decoding.md) page. The dll100 bulk cells moved from x1.03-1.07 to x1.02-1.03 and the stream cells from x1.05-1.07 to x1.04-1.05. The item is closed as [todo 30](../todo.md).

The X1 rebuild chain took the same round's diet (2026-11-08, R107). `build_decoder` carried two redundant passes: the `Entry` view duplicating every packed u16 slot, and a full-table `resize(dummy)` plus `resize(0)` that the rank fill overwrote slot for slot. The table now keeps `packed` only, `decode_symbol` and `next_state` unpack it, and the fill sizes the vector with `set_len` behind the Kraft equality, so each rebuild writes half the slots and no pre-fill. Callgrind Ir over a full streaming decode of dll100 reads -1.205 percent at level 1, -1.051 at 3, -1.017 at 9 and -0.614 at 19, with json -0.046, text -0.087 and skewed, random and zeros flat at -0.001 percent. The remaining excess sits in passes the reference also runs: the weights-stat pass reads 3265 Ir per build against 295 for the code-length materialization and 504 for the rank counts, out of about 29.4K per rebuild, so fusing the count into the stat pass saves about what the deleted rank pass costs, and the row closes as parity rather than waste.

The FSE builds keep a +0.09M excess on the same algorithm. The spread's data-dependent trip counts are the wire format's own position sequence, so no regrouping exists, and the low-probability arm's slice-width division became a shift on 2026-11-03 because `num_state_slices` is a power of two by construction, pinned by a division-reference test. The residue was structural: the spread wrote symbols one field at a time into an 8-byte `Entry` where libzstd writes a 4-byte one, and a second pass behind it computed the baselines the decode loop's multiply-add entry form consumes, with a third pass packing the entries into the slots. The reference's two-pass build landed on 2026-11-08 and deletes the `Entry` table, its zero fill and the pack pass: the spread stages each state's symbol in the packed slot's own `base` field and the second pass reads it back, computes the baseline and folds the code pair in. A full `dll100.zst3` decode reads 2,781,496,177 against 2,742,330,731 Ir, −1.41 percent, with the sequence-table chain 167.0M to 137.9M: `build_packed_table` 110.08M plus the outlined `hoist_spread_info` 12.15M against the base's 107.87M build pass, the pack pass and `SeqDecoder::new`'s glue 43.81M to 0.56M, and `__memset_avx2_unaligned_erms` 48.50M to 35.01M. The single-pass fusion that carried the code fold in the spread is the falsified shape above it, and the attribution and the equivalence gate are in [negative](../negative/decoding.md).

The hoist residue behind that split closed on 2026-11-18 against 829da5e0, in the reference's own per-slot form. The spread loops of `build_packed_table` now start each live symbol's running FSE state at its probability, one u16 slot in a 256-wide counter, and the second pass derives both baseline fields from the running state through one `highest_bit_set` plus a shift and a subtract, reading the same slot it increments. The per-symbol spread constants and the pass that computed them keep only the `Entry` view's fill, and the `symbol_info` field stays in `FSETable`, so no `FSEScratch` or `FrameDecoderState` field moved. A first candidate that rebuilt `prob + count` per slot out of the distribution returned most of the deleted hoist to `build_packed_table`, 5.47M Ir against the 11.08M the sequence side of the hoist paid on a full `dll100.zst3` decode; carrying the sum in the counter deletes that give-back and lands the fill pass itself below the constants form.

A full `dll100.zst3` streaming decode reads 2,713,703,208 against 2,695,773,235 Ir, −0.66 percent: `build_packed_table` 110.08M to 103.29M and `hoist_spread_info` 12.15M to zero, while every decode loop, Huffman build and memop keeps its exact instruction count. The gungraun dll cells read −0.702 percent at level 1, −0.660 at 3, −0.543 at 9 and −0.559 at 19, with json −0.46 to −0.79, text −0.70 to −0.79, skewed l1 exactly flat and l3, l9 at −0.45 and −0.24, zeros −0.53, and random exactly unchanged at every level. An exhaustive test pins the running-state derivation against the constants form over every table size, probability and count, a reuse test drives pooled tables through shrinking and growing wire descriptions, and the dual-path equivalence gate over the 150 pseudo-random distributions, the spread classes and the RFC defaults passes unchanged.

The copy loops hold the largest single executor lump, 18.4% of the executor's samples in the 16-byte match chunk loop against libzstd's 12.6%, with the offset-class split at 16 and the `ml mod 16` back-edge trip both data-dependent per sequence. The copy-kernel family is falsified as a whole, so no lever exists there. The wrapped-generation copy is the two-generation streaming model's own cost, which the reference's one-shot loop never pays. The sequence loop's decode-side branches no longer hold a top lump, which matches json.zst3 already reading 0.76M against 0.73M: the port closed the loop, and the residue moved to the per-block table builds. The remainder, about 0.14M, is diffuse executor traffic that sampling with skid cannot assign to one side.
- The stream cells read x1.01 to 1.05 on json and text, where the bulk cells win. The difference is the streaming pair's drain copy, which both sides pay.
- Text's long matches run the 16-byte copy loop at about 10 bytes per cycle, the per-core L3 bandwidth for its 2.3 MiB window. A trace replay of its 151 tile copies of about 112 KB each found no faster kernel: libzstd's 2x16 schedule read -3.5% copy-only, and AVX2, AVX-512, libc memcpy and non-temporal stores all lost.

## The textreal decode band

The real axis where decode reads a lead: textreal x0.88 to 0.99 against libzstd, both paths, from the 2026-11-25 release pass. The 2026-11-26 R123 decomposition put callgrind Ir on both sides of the full 33.6 MB frames at zst1/zst3/zst9 through a throwaway two-implementation driver — the same reused-decoder bulk shape and 64 KiB-pull streaming shape the matrix cells time, ours and the reference through the same buffer protocol — reading each cell at one and three iterations and taking per-function 3-minus-1 diffs so the fixed input cost drops out. The band reproduced on the base tree at bulk x0.977/0.890/0.888 and stream x0.985/0.907/0.907.

The census first, because both sides must pay the same wire work before the split means anything: 834/786/996 entropy-coded blocks at zst1/zst3/zst9, every block carrying a fresh Huffman description, 831/738/36 X2-mode and 3/48/957 X1-mode literals sections, and three explicit sequence-table descriptions per block (2,502/2,358/2,988) with no Repeat-mode stream. The reference reads the same description bytes on the same layout.

Per-decode Ir, ours against libzstd, with the component splits read off the same diffs:

| component | zst1 | zst3 | zst9 |
|---|---:|---:|---:|
| sequence executor | 124.0M vs 132.9M | 207.8M vs 225.2M | 186.8M vs 202.8M |
| X2 literal loop | 40.5M vs 38.0M | 11.8M vs 11.1M | — |
| X1 literal loop | — | — | 9.0M vs 7.7M |
| sequence FSE chain | 7.9M vs 5.4M | 7.3M vs 5.2M | 8.4M vs 5.9M |
| Huffman rebuild chain | 7.3M vs 3.2M | 7.0M vs 2.8M | 4.0M vs 2.1M |
| whole decode | 195.5M vs 196.6M | 249.8M vs 261.8M | 224.1M vs 236.0M |

The premise inverted: whole-frame Ir sits 4 to 5 percent below the reference at zst3/zst9 (parity at zst1), so the x0.89 band is cycle- and memory-side, and the executor's port win carries it. The literal loops sit at their recorded parities (the X2 asm stretch, the X1 cycle limit), and the two our-side excesses are both per-block setup. The sequence FSE chain reads 3.2/3.1/2.8K Ir per table against the reference's 2.15/2.2/2.0K — the two-pass form the 2026-11-18 fold landed re-priced on textreal's wider table logs, where dll100 read 1.1x and textreal reads 1.4x. The Huffman rebuild chain reads 8.7/8.9/4.0K per literals section against 3.8/3.7/2.1K, because every compressed-literals section pays the full X1 `build_decoder` fill — 4.1K per section at zst3 — before `build_x2_table` converts it, where `HUF_readDTableX2_wksp` builds the double-symbol table straight off the weights and rank counts. The weights-FSE decode loop inside the chain prices about 0.15 percent of the frame in outlining candidates and stays.

The X2 fill diet (2026-11-26, R123) took the chain's largest dietable share. `build_x2_table` read 50.8K Ir per build against `HUF_readDTableX2_wksp`'s 18.4K, the excess being per-symbol-pair slice bounds, iterator machinery and a per-build heap sort vector. The fill now derives its rank statistics off the `bit_ranks` the X1 fill already counts, keeps the sorted symbols in a stack array, hoists the span width, continuation offset and packed bit fields once per (rank, rank) pair, and writes through a raw cursor in the reference's own span-switched shape — 1/2/4/8 paired stores as unaligned 64-bit writes with the doubling copy `HUF_fillDTableX2` uses. A per-symbol-pair reference stays in the source behind `cfg(test)`, and an equivalence test builds both ways over 409 weight distributions. The fill reads 37.8K per build, and the full frames read −0.69/−0.48/−0.02 percent Ir at zst1/zst3/zst9 bulk (−0.59/−0.41/−0.02 stream).

Gungraun against the pre-change baseline credits text −0.687/−0.350/−0.192 percent Ir at l1/l3/l9, dll l1 −0.262 with l3 and l9 exactly flat, own_fastest and own_balanced text −0.512/−0.400, skewed −0.03 to −0.09, and json +0.05 to +0.28 percent on cells the X2 path never touches (codegen drift, sub-gate). The locked interleaved wall A/B — position-balanced driver rounds, the base tree at 3b649be2 — moved textreal.zst1 bulk +1.68 percent and zst3 bulk +0.47/+0.49 percent across two independent runs, jsonreal +0.47 to +0.66 percent on both paths, with textreal.zst9 flat, the stream cells flat inside the ±0.8 percent noise band, and the text, json and skewed controls flat within ±1 percent. The locked matrix moved every textreal cell: bulk x0.977 to 0.975, 0.890 to 0.895, 0.888 to 0.897 and stream 0.985 to 0.982, 0.907 to 0.910, 0.907 to 0.912, with every control inside its recorded band.

The X2-direct build (2026-11-26, R126) closed the chain's structural excess. `prepare_section_table` builds only the table the section decodes through, through `HuffmanTable::build_decoder_selected`: an X2-selected description builds the double-symbol table alone, skipping the X1 materialization exactly as `HUF_readDTableX2_wksp` does, and a pairing-rejected or X1-armed description builds the single-symbol table alone. The decode paths that read the X1 table regardless of the selection — the slow 4-stream fallback and the single-stream path, both through `HuffmanDecoder` — re-derive it lazily through `materialize_x1_table`, an idempotent rank fill over the retained `bits` and `bit_ranks`; on textreal every section passes the interleaved fast gate, so the lazy path never runs there. `reinit_from` copies `bit_ranks` so a pooled clone of an X2-built table can still materialize, and `build_decoder` keeps its two-table contract for the dictionary loader and the huff0 round-trips. The per-decode chain reads 7.28M to 3.87M at zst1 and 6.70M to 3.56M at zst3, both −47 percent, with the whole frame −1.50/−1.05/−0.24 percent Ir at zst1/zst3/zst9 bulk; the zst9 control stays near flat because 957 of its 996 sections are X1. Gungraun against the pre-change baseline credits text −0.861/−0.421/−0.235 percent Ir at l1/l3/l9, json −0.19 to −0.31, dll −0.03, skewed −0.02 to −0.07, with every non-text control inside 0.38 percent. The locked interleaved wall A/B moved textreal.zst1 bulk +1.28 percent and zst3 bulk +0.84 percent, and every X2-bearing control improved — text.zst3 stream +2.59, json.zst3 bulk +1.76, jsonreal bulk +1.19 — with the X2-poor cells flat inside overlapping noise bands and no cell regressed. The locked matrix moved every textreal cell again: bulk x0.975 to 0.956, x0.895 to 0.870 and x0.897 to 0.877 at zst1/zst3/zst9, stream x0.982 to 0.977, x0.910 to 0.900 and x0.912 to 0.906, and jsonreal.zst3 bulk crossed to x0.997, with the core controls inside their recorded bands.

The band stays open as a cycle-side lead with one named Ir excess, tracked as todo 44: the widened two-pass FSE residue, 1.4x per table on textreal's wide table logs where dll100 reads 1.1x.

## The jsonreal decode band

The real axis where decode trails: jsonreal reads x1.01 to 1.05 bulk and 1.03 to 1.08 stream against libzstd on every cell and pull, the widest unattributed decode deficit on record, while textreal reads ahead at x0.88 to 0.99, [matrix](matrix.md) and [snapshot](snapshot.md). The 2026-11-26 R127 decomposition prices it with the textreal band's method, at jsonreal.zst1, zst3 and zst9, with textreal measured as the contrast through the same driver.

The harness is a throwaway two-implementation driver: the reused-decoder bulk shape (`FrameDecoder::decode_all` into one reused output slice against `zstd::bulk::decompress_to_buffer`), the 64 KiB-pull streaming shape, each cell at one and three iterations under callgrind with per-function 3-minus-1 diffs so the fixed input cost drops out, plus `perf stat` over cycles, instructions, branch misses and cache misses at forty bulk and twenty-five stream iterations and `perf record` lumps on the same cells at 20 kHz pinned to one core. The driver takes the raw output length from its caller, so no calibration decode of either implementation lands inside a measured process. It reproduces the matrix cells, which verify the checksum on both sides: jsonreal x1.04/1.01/1.01 bulk and 1.05/1.02/1.03 stream against the recorded 1.05/1.01/1.02 and 1.07/1.03/1.04, textreal x0.97/0.88/0.89 and 0.97/0.90/0.91 against 0.977/0.870/0.877 and 0.985/0.900/0.906.

The census first, because both sides must pay the same wire work before the split means anything. Every jsonreal frame is per-block unique work: not one sequence description and not one Huffman tree description repeats inside a frame at any level, so the keyed table cache's whole ceiling on this axis is the weights-FSE table, whose repeats run 15 to 34 percent and price at about 0.3 to 0.5 percent of the frame's instructions. The Huffman population splits 43/240 X1/X2 at zst1, 114/156 at zst3 and 297/57 at zst9, off the same cost model the reference runs.

| frame | blocks | sequence descriptions (LL/OF/ML) | seq repeats | Huffman rebuilds (X1/X2) | Huffman repeats | weights-FSE rebuilds (repeats) |
|---|---:|---:|---:|---:|---:|---:|
| jsonreal.zst1 | 284 | 284/284/284 | 0 (0.0%) | 283 (43/240) | 0 (0.0%) | 283 (96, 33.9%) |
| jsonreal.zst3 | 271 | 271/271/271 | 0 (0.0%) | 270 (114/156) | 0 (0.0%) | 270 (66, 24.4%) |
| jsonreal.zst9 | 379 | 322/290/175 | 0 (0.0%) | 354 (297/57) | 0 (0.0%) | 354 (52, 14.7%) |
| textreal.zst1 | 278 | 278/278/278 | 0 (0.0%) | 278 (1/277) | 0 (0.0%) | 278 (169, 60.8%) |
| textreal.zst3 | 262 | 262/262/262 | 0 (0.0%) | 262 (17/245) | 0 (0.0%) | 262 (134, 51.1%) |
| textreal.zst9 | 343 | 342/311/309 | 0 (0.0%) | 332 (321/11) | 0 (0.0%) | 332 (70, 21.1%) |

The description counts below the block count at zst9 are the streams that carry no explicit description: 57 blocks of jsonreal.zst9 hold no sequence section, and the OF and ML streams of the rest fall to RLE or predefined modes at the deep levels.

The instruction split per decode, ours against libzstd, with the components read off the same diffs:

| component | jsonreal zst1 | jsonreal zst3 | jsonreal zst9 |
|---|---|---|---|
| sequence executor | 115.3M vs 122.5M | 121.2M vs 128.8M | 106.8M vs 112.9M |
| X2 literal loop | 19.1M vs 17.6M | 8.2M vs 7.5M | 2.4M vs 2.3M |
| X1 literal loop | 1.9M vs 1.6M | 5.0M vs 4.3M | 8.5M vs 7.3M |
| sequence FSE chain | 13.6M vs 9.6M | 13.4M vs 9.5M | 10.6M vs 7.4M |
| Huffman rebuild chain | 12.6M vs 5.2M | 9.8M vs 4.3M | 9.3M vs 4.2M |
| FSE description parse | 4.3M vs 1.5M | 4.1M vs 1.4M | 3.8M vs 1.4M |
| checksum | 23.1M vs 32.5M | 23.1M vs 32.5M | 23.1M vs 32.5M |
| whole decode | 192.4M vs 190.9M (x1.008) | 186.7M vs 188.6M (x0.990) | 165.8M vs 168.3M (x0.985) |

The checksum row is not a like-for-like column: ours is its own symbol where the reference's sits inlined in its frame loop, so its number is the reference's no-checksum frame delta, 0.97 Ir/B against our 0.69 over the same 33.55 MB of output. The premise inverts as it did on textreal: whole-frame Ir sits at parity or below the reference in bulk, so no instruction lever closes a bulk cell. The stream cells read +4.4 to +5.2 percent for the mirror reason: the same +14.1M of rebuild chains against the checksum's -9.6M, the executor's +3.0M of wrapped-generation copies that the reference's one-shot loop never pays, and 2.7M more drain memcpy instructions over the same sinks, 41.5M against 38.8M.

Per-decode cycles, `perf stat` medians over five interleaved draws with the checksum on, in the x notation:

| cell | bulk cycles | bulk instructions | bulk branch misses | stream cycles | stream instructions | stream branch misses |
|---|---|---|---|---|---|---|
| jsonreal.zst1 | 1.042 | 0.997 | 1.158 | 1.051 | 1.047 | 1.132 |
| jsonreal.zst3 | 1.014 | 0.981 | 1.120 | 1.022 | 1.042 | 1.075 |
| jsonreal.zst9 | 1.013 | 0.980 | 1.178 | 1.032 | 1.044 | 1.115 |
| textreal.zst1 | 0.966 | 0.980 | 1.184 | 0.975 | 1.029 | 1.145 |
| textreal.zst3 | 0.882 | 0.945 | 1.085 | 0.899 | 1.013 | 0.931 |
| textreal.zst9 | 0.892 | 0.944 | 1.071 | 0.908 | 1.017 | 0.917 |

The band is cycle-side, +1 to +5 percent, and the branch-miss column is the one ratio the two shapes share, +7 to +18 percent on every checksummed cell. Cache misses run the other way, 0.84 to 1.00 of the reference's, so no memory-side residue exists either.

The checksum is no part of it, priced by running checksum-free frames of the same content through both sides: the absorb costs 8.5M cycles against the reference's 8.7M at zst1 bulk, 8.2 to 8.9M against 7.6 to 9.1M across the six cells, 0.24 to 0.26 cycles per byte on both sides, with our loop carrying 23.1M instructions against the reference's 32.5M.

The cycle shares on jsonreal.zst1 bulk put the split on one component: the sequence executor reads 59.2 percent against the reference's 65.3 (36.5M cycles against 38.6M), the literal loops 10.9 against 11.4 (6.70M against 6.71M), the three entropy rebuild chains 14.5 against 8.3 (8.95M against 4.90M), and the checksum with its frame loop 14.5 against 14.2 (8.9M against 8.4M). The chains' +4.1M cycles exceed the +2.5M band, and the executor's -2.1M is what masks it down to the recorded x1.04. Per build, from the census's rebuild counts:

| rebuild | jsonreal zst1 | ratio | jsonreal zst3 | ratio | jsonreal zst9 | ratio |
|---|---|---|---|---|---|---|
| sequence FSE build | 15.9K vs 11.3K | 1.41x | 16.3K vs 11.6K | 1.40x | 13.3K vs 9.4K | 1.42x |
| X2 Huffman fill | 28.9K vs 13.0K | 2.23x | 25.4K vs 12.6K | 2.01x | 23.3K vs 11.8K | 1.97x |
| X1 Huffman fill | 6.6K vs 4.2K | 1.59x | 7.2K vs 4.2K | 1.70x | 6.3K vs 4.2K | 1.49x |
| weights-FSE decode | 7.9K vs 3.7K | 2.15x | 7.8K vs 3.7K | 2.12x | 7.2K vs 3.5K | 2.05x |
| FSE description parse | 3.8K vs 1.1K | 3.51x | 3.8K vs 1.1K | 3.51x | 3.4K vs 0.9K | 3.62x |

The sequence FSE build's 1.41x is todo 44's named residue, the two-pass form the 2026-11-18 fold landed, now priced on a second real shape at the ratio textreal records; the X2 fill's 2.0 to 2.2x is the R123 diet's remainder; the weights-FSE decode and the NCount reader are the same class of Rust-body volume against compact C loops. None of the five is wire-unique work, so the R107 census class does not describe this band.

The copy lumps next, because the corpus residue sat in them. The flat executor's own trace, over the `seq_dump` hook, prices jsonreal's copies as lighter than textreal's at every level: 856K sequences averaging 35.6 match bytes and 3.5 literals in 2.29M 16-byte chunk trips at 13.3 B per trip at zst1, against textreal's 2.00M sequences at 10.6 match bytes and 6.2 literals in 2.21M trips at 9.5 B per trip. The sub-16 offset class, the 8-byte `overlap_copy8` path, is 0.14 percent of jsonreal's match bytes against 0.7 percent of textreal's, and its literals are 9 percent of its output against textreal's 37, which locates the two shapes' executor difference in the work between the copies rather than in the copy kernels the record already falsifies as a family, [negative](../negative/decoding.md). The executor's instruction count agrees: 134.7 Ir per sequence against the reference's 143.1 on jsonreal, and 124.3 against 133.1 on textreal.

What the split leaves is the shape difference, and it is arithmetic on the cycle totals rather than a second mechanism. The per-frame setup is the same absolute cost on both shapes, the three rebuild chains at 8.7M cycles on textreal against 8.95M on jsonreal and the checksum at 8.4M on both, while jsonreal's reference-side frame costs 42 percent fewer cycles, 59.1M against 101.8M at zst1 bulk. The chains' excess, +4.1M cycles on jsonreal and +3.2M on textreal, is therefore 6.9 percent of the former's frame against 3.1 percent of the latter's, which is why one reads x1.01 to 1.05 and the other x0.88 to 0.99 on the same absolute excess.

The band closes as a floor attribution rather than a lever row. Its one named component is the per-block entropy rebuild machinery, +4.1M cycles and +14.1M instructions on jsonreal.zst1, and every one of its five rebuild rates lands in a residue class the record already carries: todo 44's two-pass FSE build at 1.41x, the R123 X2 fill's remainder at 2.0 to 2.2x, and the description-parse and weights-FSE readers at 3.5x and 2.1x, all passes the reference also runs rather than wire work. The avenues the probe's recipe named are closed by its own numbers: the keyed table cache's reopen condition counts zero repeated sequence and Huffman descriptions in every jsonreal frame, the copy-kernel family is falsified and the executor already reads below the reference, and the checksum prices at parity on both sides. The reopening condition is the one todo 44 names, a build-side diet landing those five rates inside the record's per-build bands without reopening the falsified single-pass fusion, the R124 dispatch classes or the entropy-side closures, [negative](../negative/decoding.md).

## The per-block entropy table census

The table machinery prices at about 5.5 percent of the dll100 decode wall, and one dll100.zst3 decode pays about 3.9K sequence-table and 1.2K X1 Huffman rebuilds, so a per-decoder cache of built tables keyed on the description bytes looked like it could cut repeated work. A throwaway build instrumented every rebuild site of the one-shot single-threaded path, hashed the description bytes each rebuild consumed together with the table kind and its accuracy log or table log, and counted per frame how many rebuilds receive a description byte-identical to an earlier one of the same kind. The share is 3 percent of the sequence rebuilds and 0.4 percent of the Huffman rebuilds: the descriptions are per-block unique work, and the cache's whole ceiling is 0.20 percent of the frame's instructions, about 0.1 percent of its wall.

| frame | blocks | seq rebuilds | seq repeats | X1 rebuilds | X1 repeats | weights FSE rebuilds | weights FSE repeats |
|---|---:|---:|---:|---:|---:|---:|---:|
| dll100.zst1 | 1171 | 3487 | 111 (3.2%) | 1146 | 18 (1.6%) | 1146 | 465 (40.6%) |
| dll100.zst3 | 1756 | 4028 | 123 (3.1%) | 1178 | 5 (0.4%) | 1178 | 289 (24.5%) |
| dll100.zst9 | 1883 | 3289 | 100 (3.0%) | 1057 | 0 (0.0%) | 1057 | 185 (17.5%) |
| dll100.zst19 | 2494 | 5352 | 142 (2.7%) | 1138 | 0 (0.0%) | 1138 | 449 (39.5%) |
| json.zst3 | 257 | 771 | 1 (0.1%) | 229 | 27 (11.8%) | 229 | 200 (87.3%) |
| json.zst9 | 257 | 478 | 6 (1.3%) | 222 | 3 (1.4%) | 222 | 185 (83.3%) |
| text.zst3 | 257 | 33 | 6 (18.2%) | 8 | 0 (0.0%) | 8 | 0 (0.0%) |
| skewed.zst3 | 256 | 768 | 180 (23.4%) | 4 | 3 (75.0%) | 4 | 3 (75.0%) |
| skewed.zst9 | 256 | 768 | 302 (39.3%) | 2 | 1 (50.0%) | 2 | 1 (50.0%) |
| random.zst3 | 256 | 0 | 0 | 0 | 0 | 0 | 0 |
| zeros.zst3 | 256 | 3 | 0 (0.0%) | 0 | 0 | 0 | 0 |

dll100.zst3 holds 1756 blocks over its 104.9 MB of output, not the about 800 the 128 KiB block cap suggests, because entropy-coded blocks average 60 KB; the census counts 3928 explicit descriptions plus 100 predefined installs, where the R106 mapping recorded 3,903 for this side and 3,928 for libzstd, so the census reproduces the mapping's second column exactly and the repeat share holds either way. Every dll100.zst3 block re-describes all three sequence tables, with no Repeat-mode stream in the frame at all; dll100.zst9 and zst19 do carry the wire's own reuse, 91/139/145 and 86/184/129 Repeat-mode streams, and still repeat only 3.0 and 2.7 percent of their explicit descriptions. The dll100.zst3 repeats split 51 of 1339 LL, 32 of 1350 OF and 40 of 1339 ML, and they sit far apart: an LRU of 32 entries catches 26, 12 and 26 of them, and most land 64 or more blocks after their twin, so even the recurring descriptions are not recent.

The one class with a large repeat share is the weights FSE table that decodes a Huffman description, 289 of 1178 on dll100.zst3 and 200 of 229 on json.zst3. It is the table of at most 64 entries that the weights header builds, about 3K Ir per build and 0.3 percent of the frame, so its 289 repeats buy 0.07 percent; the Huffman description that owns it repeats 5 times.

Priced on the three frames the build was profiled on, callgrind Ir split by function: dll100.zst3 reads 17.9K Ir per sequence rebuild and 44.1K per Huffman rebuild of 1187.1M total, json.zst3 22.6K and 11.4K of 329.3M, skewed.zst3 21.4K of 387.2M.

| frame | seq repeats | X1 repeats | ceiling Ir | share of the frame's Ir |
|---|---:|---:|---:|---:|
| dll100.zst3 | 123 | 5 | 2.42M | 0.20% |
| json.zst3 | 1 | 27 | 0.33M | 0.10% |
| skewed.zst3 | 180 | 3 | 3.85M | 1.00% |

skewed is the best shape the census found, its 180 ML repeats giving 23.4 percent of the sequence rebuilds, and it prices at 1.0 percent of the frame's instructions on a shape outside the dll band. The wire's own reuse already costs nothing: a Repeat-mode stream skips the rebuild, and the predefined installs collapse to 100 builds per dll100.zst3 frame behind the existing flag. What the census leaves is the per-block build itself, 3928 distinct descriptions over 1756 blocks, which no cache can remove. The falsification and its reopening condition are on the [negative](../negative/decoding.md) page.
