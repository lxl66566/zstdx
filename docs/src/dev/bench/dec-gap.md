# Decode stream gap vs libzstd — differential attribution (2026-09-20)

> Core-vs-core decomposition of the ST streaming decode gap (todo item 2), measured against the real opponent instead of our own profile: same files through both implementations, per-function Ir, per-symbol cycles, and an instruction-level diff of the two hot loops. All numbers from the bench machine (Zen4), zstd-sys 2.1.0 (libzstd 1.5.7) as linked by the `zstd` crate.

## Method

Our side: `zstdx-bench prof dec` (StreamingDecoder, 64 KiB reads). Reference side: a /tmp twin harness looping `zstd::stream::read::Decoder` with the identical sink shape (the exact matrix `dec-st` stream cell body). Tools: `perf stat` (cycles/instructions/branches/misses/L1d/`ex_ret_ops`), `perf record` per-symbol shares, `valgrind --tool=callgrind` for deterministic per-function Ir, `objdump` of the hot instances — ours `execute_decoded_flat_bmi2` (the WRAP+HEADROOM instantiation is the streaming hot one, confirmed by sample addresses), theirs `ZSTD_decompressSequences_bmi2.constprop.0`. Sequence counts and repcode shares of the reference frames via `seqstats --ref-frame`.

## T1: the gap is instruction volume, not misses or IPC

x = ours/zstd (per 40×32 MiB or 16×100 MiB solo runs):

| shape | cycles | instructions | IPC ours/zstd | branch-miss | L1d-miss |
|---|---:|---:|---:|---:|---:|
| json.zst1 | 1.237 | 1.358 | 3.90/3.56 | 1.21 | 1.02 |
| json.zst3 | 1.326 | 1.359 | 3.63/3.54 | 1.67 | 1.00 |
| json.zst9 | 1.300 | 1.344 | 3.53/3.41 | 1.71 | 1.01 |
| skewed.zst3 | 1.199 | 1.343 | 3.75/3.35 | 1.14 | 0.93 |
| text.zst3 | 1.077 | 0.925 | 2.52/2.94 | 1.93 | 1.02 |
| dll100.zst3 | 1.332 | 1.414 | 3.64/3.43 | 1.68 | 1.04 |

- On every sequence-dense shape we execute 1.34–1.41× the instructions at equal or better IPC. Both loops are throughput-bound; the whole cycle gap is instruction volume. `ex_ret_ops` tracks instructions on both sides (no hidden uop-fusion difference).
- L1d misses are equal to within 4% on every shape — the memory behavior of the two decoders is the same. The extra branch misses on json/dll (+20–33 M per 32 MiB, worth ~0.3–0.4 G cycles at ~15 cyc each) are 8–10% of the cycle gap: secondary.
- text.zst3 is the exception that proves the split: we execute *fewer* instructions (0.92×) yet lose cycles — its reference parse has only 57 K sequences per 32 MiB (avg ml 311, avg ll 273), so the shape is copy- and literal-bound, the per-sequence budget is ~1% of the decode, and the residual is latency exposure (IPC 2.52 vs 2.94), riding the documented >9 GB/s wall-untrustworthy zone.

## T2: per-function Ir (callgrind, json.zst3, one decode ≈ half the totals)

| ours | Ir | zstd | Ir |
|---|---:|---|---:|
| fused loop (`execute_decoded_flat_bmi2`) | 368.8 M | `ZSTD_decompressSequences_bmi2` | 267.3 M |
| xxh64 absorb | 23.1 M | `ZSTD_decompressContinue.part.0` (block dispatch + literals + xxh + staging) | 32.5 M |
| FSE `build_decoding_table` | 13.1 M | `ZSTD_buildFSETable_body_bmi2` | 9.3 M |
| `copy_wrapped_match` (stream wrap) | 12.3 M | (window sliding inside memcpy) | — |
| memcpy | 40.1 M | memcpy | 43.8 M |
| Huffman X2 decode + builds | 12.3 M | HUF read/build + NCount | ~7.3 M |
| `SeqDecoder::new` | 5.6 M | (inside decompressContinue) | — |

Per-shape fused-loop budgets (callgrind, both harnesses, per decode; zstd's skewed row is the `SplitLitBuffer` variant it selects there, same loop body):

| shape | nseq / 32 MiB | rep share | ours Ir/seq | zstd Ir/seq | Δ | loop-Ir ratio |
|---|---:|---:|---:|---:|---:|---:|
| json.zst3 | 2.118 M | 12.9% | 174.1 | 126.2 | +47.9 | 1.38 |
| skewed.zst3 | 1.632 M | 0.0% | 184.8 | 126.9 | +57.9 | 1.46 |
| dll32.zst3 | 1.491 M | 30.7% | 182.4 | 121.7 | +60.7 | 1.50 |

Our per-sequence cost is nearly flat (174–185) while libzstd's drops with cheaper sequences (122–127) — the excess grows exactly where the stream gap is worst (dll x1.30-1.37). The fused loop's +101.5 M Ir/decode is ~82% of json's +1.24 G cycle gap. Non-loop fixed costs are at parity on both sides (≈0.72–0.90 Mcyc/MiB each; our streaming wrap copy ≈ their sliding-window memmove; our FSE build ≈ theirs; our 8-chain xxh64 is faster than their scalar).

## T3: where the +48 Ir/seq sit (assembly diff of the two hot loops)

libzstd's loop is *not* cleaner at the register level — it also keeps table pointers, FSE states and the bit container on the stack (three table-pointer and three state reloads per sequence, spilled next-states), and it branches on `of<=1`, `ml_nb==0`, `ll_nb==0` and `nbSeq==1`. The differences that matter:

1. **Entry handling, +10**: our one packed u64 load per stream (3 loads) + 24 unpack shifts (6 field extracts × 2 groups) vs their 12 single-field loads + 3 leas — the loads ARE the unpack. Ir-parity trade that costs us the serial `shr` chain feeding `sum`.
2. **Offset history, +12 (87–100% of sequences on json/dll/skewed are real offsets)**: their real-offset path is 3 moves + `of-3`; our slot/select machinery ran the rotation selects on every sequence even though the rotation is provably rotate-all whenever `of > 3`. A fast-path fold measured −8.7 Ir/seq deterministically — but see the falsification below.
3. **Stack round-trips, +15**: our `win` store/load pair, `consumed` load+add+store, states stored twice (loop slot + writeback slot), `rem` and `op` spilled per sequence. Their op stays in a register; their consumed stays in a register.
4. **Read + splits, +11**: our fused read (one shrx/shlx pair + consumed bookkeeping) + 6×(shrx+and+bzhi+add) field splits vs their per-field two-shift reads with zero-width branch skips. On json many fields are zero-width (avg ll 0.63), where they pay 2 and we pay 4.
5. **Literals, +2**: our `ll==0` branch + two stack-reloaded bounds bases vs their unconditional copy16 (garbage overwritten by the match). Making ll>0 unconditional is falsified (negative) — the TAGE/BTB reordering tax.

## Falsified this round (data in [negative](../negative/decoding.md))

Both surgical Ir cuts below are correct at the instruction level and lose at the wall to the same mechanism: **any machine-code change inside the fused symbol re-rolls its register allocation and whole-binary layout, taxing text-stream +3–5% reproducibly** (A/B/B/A crossover; on text the per-seq budget is ~1% of the decode, so the tax is *displaced non-loop code*, not the loop's own instructions — most plausibly I$/DSB placement of xxh64-absorb and the copy tails).

- Real-offset constant rotation in `do_offset_history`: −8.7 Ir/seq (174.1→165.4, json stream Ir −3.8%, all inside the fused loop), wall json −1.0..−1.8%, skewed.zst9 −3.5%, but text.zst3/9/19 +2.8/+4.2/+5.0% — net negative across the cell set.
- 5-bit nb fields in the packed entry (kills the six `and $0x3f` bzhi guards): the re-roll *added* +1.6% fused-loop Ir (749.4 M vs 737.6 M) — lost deterministically before wall.

## Remaining lever ranking (differential evidence)

1. Read-side serial-chain folds (reload window rebuild into the next read; `consumed` into `ip`) — the family with a proven conversion record (fused bit-read), cuts the T3-3 stack round-trips AND chain segments; but every such change pays the same re-roll tax, so each must carry its own A/B/B/A including text.
2. Entry-struct layout swap (libzstd's `{u16 next, u8 nbAdd, u8 nbBits, u32 base}` per-stream, loads as extracts): −10 Ir/seq equivalent, kills the serial shr-unpack chain, and shifts the port mix (12 L1 loads vs 24 ALU) — the one remaining lever that changes the loop's *resource* profile rather than shaving uops; needs the full table-build/decode rework and the same lottery hedge. Ir-neutral load-port risk: 15 loads/seq at the target 4+ seq/cycle sits at the 3-load-port ceiling (libzstd lives exactly there at IPC 3.4–3.5).
3. Offset-history fold (the falsified candidate's −8.7 Ir/seq) is worth revisiting only bundled with a lever big enough to dominate the tax (≥5% expected), or if a placement-stabilizing trick appears — none is known (outlining variants are falsified).
4. ~~text's residual is latency exposure in the copy/literal tails~~ — falsified by T4 below: the copy tail sits at the per-core L3 ceiling with a healthy-loop IPC 3.57 and no kernel lever; the unchecked text gap is the same instruction-volume family as the dense shapes, diluted to x1.07 on the checked wall by an exactly-parity absorb.

## T4: the text exception re-attributed (2026-09-20, follow-up round)

T1 read text as "latency exposure in the copy tails" from the IPC 2.52/2.94 contrast. A dedicated decomposition round falsifies that mechanism: the text residual is the same instruction-volume class as the other shapes, and the checked-wall ratio is diluted by a checksum cost that is *exactly* parity. Also corrected: T1's "avg ll 273" is wrong — the reference parse of text.zst3 carries ~1 literal per sequence (~0 literal bytes per 32 MiB); there is no literal tail at all.

**Copy-domain shape** (seq_dump exec trace of the reference frames, per 32 MiB): text.zst3 = 57,443 sequences, 17.9 MB match bytes, avg ml 311; **94.3% of match bytes sit in 151 matches of avg ~112 KB at offsets 256K–2M** (the flattened-tree tile copies); zst9/zst19 the same at 97%. Offsets <32 carry 1.5% of match bytes. The other ~47% of the output is ~15.6 MB/decode of 128 KiB RLE fills (memset; corpus padding) — data-inherent, equal Ir on both sides (ours 16.0 M, theirs 15.7 M per decode).

**Checksum-less pair differential** (`zstd -3` vs `zstd -3 --no-check` on text.raw; 40×32 MiB solo, back-to-back, reproduced twice): ours 676.3 M / 330.3 M cycles (checked/unchecked) → absorb = 346.0 M; zstd 631.2 M / 285.7 M → absorb = 345.5 M. The xxh64 absorb is exact parity (8.65 M cyc/decode; 51%/55% of the two checked decodes) — it contributes nothing to the gap, it only dilutes its ratio: **x1.071 checked, x1.151–1.156 unchecked**. On the unchecked pair the per-function Ir split is the json picture again: their sequence loop 10.7 M Ir vs our fused 14.0 M per decode; drain memcpy ours 34.2 M vs theirs 36.7 M Ir (ours ~0.4 M cyc faster — our contiguous flat buffer vs their sliding window).

**Kernel replay on the recorded trace** (the exact (pos, ml, offset) triples replayed over a frame-sized buffer; one kernel per process, 4 interleaved rounds; ps/B over the offset≥32 domain = 99.2% of match bytes):

| kernel | ps/B | vs current |
|---|---:|---|
| current 16B loop | 23.0 | — |
| libzstd shape (first-16 peel + 2×16B/iter, overshoot 32) | 22.2 | −3.5% copy-only ≈ −0.8% decode |
| 2×16B/iter, exact backward tail | 30.2 | +31% |
| 4×16B/iter, exact backward tail | 30.5 | +33% |
| AVX2 256-bit chunks | 48.3 | +110% |
| AVX-512 512-bit chunks | 32.0 | +39% |
| libc memcpy per match | 27.7 | +20% |
| NT streaming stores (movntdq 16/32/64B) | 157–162 | ~7× |

In situ the loop runs at IPC 3.57 (callgrind 14.0 M Ir vs perf 3.92 M cycles per decode) at ~10 B/cycle — the per-core L3-bandwidth ceiling for a 2.3 MiB streaming window, not a latency chain and not issue-bound (the ALU ceiling would be ~21 B/cycle). No copy-side lever exists for text: the earlier "AVX2 copy32 / SIMD copy scheduling" falsifications hold unchanged under the long-match premise, and the one shape that wins on copy time alone (libzstd's peel schedule) is below the documented fused-symbol re-roll tax (+3–5% text-stream). The unchecked text gap (x1.15) belongs to the instruction-volume/fixed-cost family of the dense shapes — entry layout, offset history, stack round-trips — plus their cheaper non-loop staging, not to the executor's copy tail.
