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

- skewed.zst1 and skewed.zst19 bulk read x1.32 and did not move with the loop. Our own bulk path runs slower than our stream path there, at 2346 against 2623 MiB/s on zst1, so the residue sits outside the sequence loop, on the literals side of the block decode.
- dll100 reads x1.03 to 1.13 on both paths, with branch misses still 1.17x the clang build's.
- The stream cells read x1.01 to 1.05 on json and text, where the bulk cells win. The difference is the streaming pair's drain copy, which both sides pay.
- Text's long matches run the 16-byte copy loop at about 10 bytes per cycle, the per-core L3 bandwidth for its 2.3 MiB window. A trace replay of its 151 tile copies of about 112 KB each found no faster kernel: libzstd's 2x16 schedule read -3.5% copy-only, and AVX2, AVX-512, libc memcpy and non-temporal stores all lost.
