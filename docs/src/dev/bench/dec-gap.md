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
