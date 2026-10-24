# Decode Stream Gap Against libzstd

This page answers where the single-thread streaming decode gap against libzstd comes from and which explanations the measurements rule out. It decomposes the gap core against core, with both implementations reading the same files, and it separates instruction volume from memory, branch and front-end effects. The x notation is our time divided by the reference time, so values above 1 mean we are slower. All numbers were measured on 2026-09-20 on the bench machine, with zstd-sys 2.1.0 linking libzstd 1.5.7 and the streaming cell taken from the matrix `dec-st` body.

## The gap

The measurement used `zstdx-bench prof dec` on the StreamingDecoder with 64 KiB reads, and the reference is a twin harness looping `zstd::stream::read::Decoder` with the identical sink shape. Tools were `perf stat` for cycles, instructions, branches, misses, L1 data misses and `ex_ret_ops`, `perf record` for per-symbol shares, `valgrind --tool=callgrind` for deterministic per-function instruction counts, and `objdump` of the hot instances. Ours is `execute_decoded_flat_bmi2`, whose WRAP plus HEADROOM instantiation is the streaming hot one, and theirs is `ZSTD_decompressSequences_bmi2.constprop.0`. Sequence counts and repcode shares of the reference frames come from `seqstats --ref-frame`, and zstd's skewed row is the `SplitLitBuffer` variant it selects there with the same loop body.

The table reports the ratio ours over zstd per 40 passes of 32 MiB, or 16 passes of 100 MiB for dll100.

| shape | cycles | instructions | IPC ours/zstd | branch-miss | L1d-miss |
|---|---:|---:|---:|---:|---:|
| json.zst1 | 1.237 | 1.358 | 3.90/3.56 | 1.21 | 1.02 |
| json.zst3 | 1.326 | 1.359 | 3.63/3.54 | 1.67 | 1.00 |
| json.zst9 | 1.300 | 1.344 | 3.53/3.41 | 1.71 | 1.01 |
| skewed.zst3 | 1.199 | 1.343 | 3.75/3.35 | 1.14 | 0.93 |
| text.zst3 | 1.077 | 0.925 | 2.52/2.94 | 1.93 | 1.02 |
| dll100.zst3 | 1.332 | 1.414 | 3.64/3.43 | 1.68 | 1.04 |

On every sequence-dense shape we execute 1.34 to 1.41 times the instructions at equal or better IPC. Both loops are throughput-bound, and the whole cycle gap is instruction volume. `ex_ret_ops` tracks instructions on both sides, so no hidden uop-fusion difference exists. L1 data misses are equal within 4% on every shape, so the two decoders' memory behavior is the same. The extra branch misses on json and dll, +20M to +33M per 32 MiB and worth about 0.3G to 0.4G cycles at roughly 15 cycles each, are 8% to 10% of the cycle gap and secondary.

Text.zst3 is the exception that proves the split: we execute fewer instructions at 0.92 of zstd's yet lose cycles. Its reference parse has only 57K sequences per 32 MiB, so the shape is copy-bound and the per-sequence budget is about 1% of the decode. The residual is latency exposure at IPC 2.52 against 2.94, in the documented above-9-GB/s zone where wall-clock is untrustworthy. A later round corrected the implied literal volume: the reference parse of text.zst3 carries about one literal per sequence and about zero literal bytes per 32 MiB, so there is no literal tail.

## Where the gap comes from

Per-function instruction counts on json.zst3, one decode, about half the totals:

| ours | instructions | zstd | instructions |
|---|---:|---|---:|
| fused loop `execute_decoded_flat_bmi2` | 368.8 M | `ZSTD_decompressSequences_bmi2` | 267.3 M |
| xxh64 absorb | 23.1 M | `ZSTD_decompressContinue.part.0` | 32.5 M |
| FSE `build_decoding_table` | 13.1 M | `ZSTD_buildFSETable_body_bmi2` | 9.3 M |
| `copy_wrapped_match` | 12.3 M | window sliding inside memcpy | none |
| memcpy | 40.1 M | memcpy | 43.8 M |
| Huffman X2 decode and builds | 12.3 M | HUF read, build and NCount | about 7.3 M |
| `SeqDecoder::new` | 5.6 M | inside decompressContinue | none |

The per-shape fused-loop budgets, both harnesses, per decode:

| shape | sequences per 32 MiB | rep share | ours Ir/seq | zstd Ir/seq | difference | loop-Ir ratio |
|---|---:|---:|---:|---:|---:|---:|
| json.zst3 | 2.118 M | 12.9% | 174.1 | 126.2 | +47.9 | 1.38 |
| skewed.zst3 | 1.632 M | 0.0% | 184.8 | 126.9 | +57.9 | 1.46 |
| dll32.zst3 | 1.491 M | 30.7% | 182.4 | 121.7 | +60.7 | 1.50 |

Our per-sequence cost is nearly flat at 174 to 185 instructions while libzstd's drops to 122 to 127 on cheaper sequences, so the excess grows exactly where the stream gap is worst at dll x1.30 to 1.37. The fused loop's +101.5M instructions per decode is about 82% of json's +1.24G cycle gap. Non-loop fixed costs are at parity on both sides at about 0.72 to 0.90 Mcycles per MiB: our streaming wrap copy matches their sliding-window memmove, our FSE build matches theirs, and our eight-chain xxh64 is faster than their scalar one.

## Evidence per component

An assembly diff of the two hot loops showed that libzstd's loop is not cleaner at the register level. It also keeps table pointers, FSE states and the bit container on the stack, with three table-pointer and three state reloads per sequence plus spilled next states, and it branches on `of<=1`, `ml_nb==0`, `ll_nb==0` and `nbSeq==1`. Five differences account for the +48 instructions per sequence on json.

Entry handling costs +10. Ours needs one packed u64 load per stream, three loads total, plus 24 unpack shifts from six field extracts twice. Theirs needs 12 single-field loads plus three address computations, so the loads are the unpack. This is an instruction-parity trade that costs us the serial shift chain feeding the sum.

Offset history costs +12, and 87% to 100% of sequences on json, dll and skewed are real offsets. Their real-offset path is three moves plus `of-3`, while our slot and select machinery ran the rotation selects on every sequence even though the rotation is provably rotate-all whenever `of > 3`. Folding in a fast path measured -8.7 instructions per sequence deterministically, but the change was later falsified at the wall.

Stack round-trips cost +15. Ours has the `win` store and load pair, the `consumed` load, add and store, states stored twice in the loop slot and the writeback slot, and `rem` and the output pointer spilled per sequence. Their output pointer and consumed count stay in registers. Read and splits cost +11: our fused read is one `shrx` and `shlx` pair plus consumed bookkeeping, and six field splits of `shrx`, `and`, `bzhi` and add, against their per-field two-shift reads with zero-width branch skips. On json many fields are zero-width, with average literal length 0.63, where they pay two shifts and we pay four. Literals cost +2 because our `ll==0` branch and two stack-reloaded bounds bases compare against their unconditional copy16, whose garbage is overwritten by the match. Making `ll>0` unconditional was falsified because of the TAGE and BTB reordering tax.

## Falsified changes

Both surgical instruction cuts were correct at the instruction level and lost at the wall to the same mechanism. Any machine-code change inside the fused symbol re-rolls its register allocation and whole-binary layout, which taxes the text stream +3% to +5% reproducibly in an A/B/B/A crossover. On text the per-sequence budget is about 1% of the decode, so the tax is displaced non-loop code rather than the loop's own instructions, most plausibly instruction-cache and decode-stream placement of the xxh64 absorb and the copy tails. The data is in the [decode negative results](../negative/decoding.md).

The real-offset constant rotation in `do_offset_history` cut 8.7 instructions per sequence, from 174.1 to 165.4, for json stream instructions -3.8% all inside the fused loop. Wall moved json -1.0% to -1.8% and skewed.zst9 -3.5%, but text.zst3, zst9 and zst19 paid +2.8%, +4.2% and +5.0%, which is net negative across the cell set.

Five-bit nb fields in the packed entry, which kill the six `and $0x3f` bzhi guards, added 1.6% fused-loop instructions on the re-roll, 749.4M against 737.6M, and lost deterministically before reaching the wall.

The read-side fold family was falsified as a class. Carrying the raw container so that the read count folds the consumed offset and reloads shift-free, and folding `consumed` into a single bit cursor, both deliver their predicted uop cuts at json -2.1% instructions and win the throughput-bound shapes at json -1.8% cycles, dll -1.1% and skewed stream -2% to -4%. They lose text reproducibly at +1.4% to +3.5% cycles and +2.7% to +5.0% stream wall across two independent allocation re-rolls. Moving the consumed offset from the window into the count puts the loop-carried cursor in series with the read's serial chain, and the latency-leaning long-sequence mix pays that beyond what the removed shifts buy. The pre-shifted window's count derived from the sequence count is load-bearing.

The entry-struct layout swap was falsified the same day. Libzstd's `{u16 next, u8 nbAdd, u8 nbBits, u32 base}` per stream landed exactly as specified, with 12 field loads, zero unpack shifts and the assembly verified. It cut 15.1 instructions per sequence, from 174.1 to 159.0 and whole-decode -6.8%, but IPC fell about 1:1 with the cut on every shape: json moved 3.61 to 3.35 at 7.4% fewer instructions for -0.1% cycles, and the table base even reloads twice per sequence in the field codegen. Text stream paid +0.6% to +2.4% across all four levels and json.zst3 stream regressed +1.0%. A 2-by-32-bit field-group variant with six loads lost at the instruction level at +2.6% loop instructions because the re-roll re-spilled the bases. The loop is issue and latency saturated rather than uop-count saturated, so uop cuts that do not shorten the serial or issue structure convert about 1:1 into IPC loss. The packed u64 entry is locally optimal on Zen4.

The real-offset rotation later re-landed on the newer codebase. Once the codebase re-rolled its placement text-benign, solo perf measured text.zst3 instructions -0.98% at cycles +0.05% over four repetitions, json.zst3 instructions -4.2% and cycles -1.2% to -1.7%, and skewed.zst9 instructions -4.7% and cycles -3.4% to -3.9%. Stream A/B/B/A moved json -1.1% to -1.6% at all four levels, skewed.zst3 and zst9 -2.4% and -3.0%, and dll100 +1.0% and +1.4%, with text inside ±0.8% in both directions. The earlier record stands as layout-lottery proof rather than a mechanism veto, and the text-stream gate stays mandatory for every fused-symbol machine-code change.

## What remains open

The 2026-09-20 rounds left no open instruction-volume lever for the fused loop. The read-side folds and both entry layouts are falsified, and text's copy tail is latency-class. Later re-lands of individually sound cuts, such as the real-offset rotation, are arbitrated per roll by the text-stream gate rather than by a mechanism argument.

The text exception was re-attributed. A dedicated decomposition falsified the copy-tail latency mechanism and showed that the text residual is the same instruction-volume class as the dense shapes. A seq_dump execution trace of the reference frames over 32 MiB shows text.zst3 has 57,443 sequences and 17.9 MB of match bytes at average match length 311. Of those match bytes, 94.3% sit in 151 matches of about 112 KB each at offsets from 256K to 2M, the flattened-tree tile copies, and zst9 and zst19 are the same at 97%. Offsets below 32 carry 1.5% of match bytes, and about 47% of output is about 15.6 MB per decode of 128 KiB RLE fills from corpus padding, which are data-inherent and equal on both sides at ours 16.0M and theirs 15.7M instructions per decode.

A checksum-less pair differential, comparing `zstd -3` against `zstd -3 --no-check` on text.raw over 40 passes and reproducing twice, gave ours 676.3M cycles checked and 330.3M unchecked for an absorb of 346.0M, and zstd 631.2M and 285.7M for an absorb of 345.5M. The xxh64 absorb is exact parity at 8.65M cycles per decode and 51% and 55% of the two checked decodes, so it contributes nothing to the gap and only dilutes the ratio: x1.071 checked and x1.151 to 1.156 unchecked. On the unchecked pair the per-function split is the json picture again, with their sequence loop at 10.7M instructions against our fused 14.0M per decode, and drain memcpy at ours 34.2M against theirs 36.7M, where ours is about 0.4M cycles faster because our buffer is contiguous against their sliding window.

A kernel replay on the recorded trace replayed the exact position, match length and offset triples over a frame-sized buffer with one kernel per process and four interleaved rounds, over the offset-32-and-above domain that carries 99.2% of match bytes.

| kernel | picoseconds per byte | versus current |
|---|---:|---|
| current 16B loop | 23.0 | none |
| libzstd shape, first-16 peel plus 2x16B per iteration, overshoot 32 | 22.2 | -3.5% copy-only, about -0.8% decode |
| 2x16B per iteration, exact backward tail | 30.2 | +31% |
| 4x16B per iteration, exact backward tail | 30.5 | +33% |
| AVX2 256-bit chunks | 48.3 | +110% |
| AVX-512 512-bit chunks | 32.0 | +39% |
| libc memcpy per match | 27.7 | +20% |
| NT streaming stores, 16, 32 and 64B | 157-162 | about 7x |

In situ the loop runs at IPC 3.57, from callgrind 14.0M instructions against perf 3.92M cycles per decode, which is about 10 bytes per cycle. That is the per-core L3-bandwidth ceiling for a 2.3 MiB streaming window, not a latency chain and not an issue bound, because the ALU ceiling would be about 21 bytes per cycle. No copy-side lever exists for text, and the earlier AVX2 copy32 and SIMD copy scheduling falsifications hold under the long-match premise. The one shape that wins on copy time alone, libzstd's peel schedule, sits below the documented fused-symbol re-roll tax of +3% to +5% on text stream. The unchecked text gap at x1.15 belongs to the instruction-volume and fixed-cost family of the dense shapes plus their cheaper non-loop staging.

The front end was measured directly, because the issue and latency saturation had been inferred from IPC conversion rather than from front-end events. Zen4 has no Intel DSB or LSD events, so the equivalents are the op cache, the DSB counterpart that can deliver up to eight uops per cycle, and the loop buffer, the LSD counterpart. The measurement used `perf stat` event groups on the two solo harnesses, pinned and interleaved with medians, plus IBS fetch sampling through `perf record -R -e ibs_fetch//` with per-sample fetch decode of latency, cache and TLB miss bits, symbolized through the PIE map. Shapes were json.zst3, dll100.zst3 and text.zst3, since skewed shares json's loop structure. Events included `de_src_op_disp.{op_cache,decoder,loop_buffer}`, `de_no_dispatch_per_slot.*` with its per-slot share over the six-wide dispatch, `de_op_queue_empty`, `ex_no_retire.empty`, `op_cache_hit_miss.*`, `ic_tag_hit_miss.*`, `bp_l1_tlb_miss_*`, `bp_de_redirect`, `ex_ret_ucode_ops` and `de_dis_dispatch_token_stalls1/2.*`.

| shape | side | IPC | ops dispatched/cycle | op-cache share | loop-buffer share | FE-stall %cy | op-queue-empty %cy | nothing-to-retire %cy | backend-stall %cy |
|---|---|---:|---:|---:|---:|---:|---:|---:|---:|
| json.zst3 | ours | 3.67 | 4.22 | 99.50% | 0 | 11.97 | 9.23 | 3.39 | 17.7 |
| json.zst3 | zstd | 3.59 | 4.11 | 99.45% | 0 | 11.56 | 8.59 | 2.26 | 19.7 |
| dll100.zst3 | ours | 3.63 | 4.08 | 99.54% | 0 | 12.35 | 9.29 | 4.61 | 19.6 |
| dll100.zst3 | zstd | 3.45 | 3.97 | 99.19% | 0 | 10.61 | 7.67 | 3.82 | 23.1 |
| text.zst3 | ours | 2.55 | 2.58 | 99.22% | 0 | 5.32 | 2.94 | 1.37 | 51.2 |
| text.zst3 | zstd | 3.04 | 3.02 | 99.38% | 0 | 2.92 | 2.22 | 0.88 | 46.2 |

Miss and issue-side sources are small or favorable. Op-cache miss rate reads 0.55 against 0.49 for json, 0.50 against 0.76 for dll and about 0.9 against 0.9 for text within ±0.1 percentage point of round noise. Instruction-cache miss rate reads 27.0% against 28.0% for json at 0.32 misses per 1000 instructions or fewer on both sides, so both are L2-resident, and ITLB misses and fetch redirects stay at 60K or fewer per 60 passes of 32 MiB, effectively zero. Microcode ops are 0.06% of instructions for json and 0.10% for dll, equal on both sides. Issue-resource stalls are lower for us on every counter: json integer-scheduler-token stalls read 4.6% against 7.8% of cycles, integer physical-register-file 1.5% against 3.8%, and store-queue 2.1% against 2.6%.

IBS fetch attribution on the hot loops gives ours 85.9%, 74.4% and 42.3% of samples on json, dll and text, against libzstd's 85.5%, 74.4% and 28.9%. Mean fetch latency reads 0.01, 0.00 and 0.04 cycles for us against 0.00, 0.00 and 0.09 for them, instruction-cache misses are 0.011% to 0.053% of tagged fetches, op-cache misses 0.077% to 0.105%, and there are zero incomplete fetches and zero in-loop ITLB events. On text our loop fetches strictly cleaner than theirs.

The verdict is that no front-end lever exists on any shape. Both loops dispatch about 99.5% of ops from the op cache and 0% from the loop buffer, because neither loop fits it, so loop-buffer occupancy is a non-factor by construction. The front-end stall differential of +0.4, +1.7 and +2.4 cycle-equivalent percentage points on json, dll and text is not concentrated in the fused loop, whose own fetch sits at about zero latency and on text is cleaner than the reference. The maximal fantasy bound, where our front-end stall cycle-equivalents per dispatched op dropped exactly to libzstd's, buys json nothing because the per-op figures are equal at 0.0283-0.0286 against 0.0280-0.0296 across rounds. It buys dll +1.4% of cycles at 0.0302 against 0.0268 and text +2.8% that sits in the memory-ceiling copy and absorb tails, where backend stalls are 51% of cycles and the loop is 11% to 21%, not in fetch. Every case is below the 3% wall bar, and none is addressable by code layout. We dispatch more ops per cycle than libzstd on the dense shapes at 4.22 against 4.11 for json and 4.08 against 3.97 for dll from the same occupancy structure, and the cycle gap tracks the dispatched-op ratio, with json ops at 1.369x and cycles at 1.333x. That is the instruction-volume inference, now measured at the front end. Layout-class levers such as loop alignment and hot-cold outlining have no measured loss to recover, and the remaining decode-stream gap is op volume through execution ports.
