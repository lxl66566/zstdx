# Profiling and attribution
## Rebuilds replace the profiled binary


Recording `prof dec` on the plain release build and then running a `--features seq_dump` tool rebuilds `target/release/zstdx-bench` in place, so every recorded address goes stale. `perf annotate` then attributes the fused loop's samples to `bulk::compress_with`, a neighboring symbol's range, without any warning.

Record and annotate without an intervening cargo invocation, or re-record after any build.
## Cycle counts drift across sessions


The same binary and input measured 713M against 679M cycles per 40 runs of 32 MiB for text.zst3 across sessions, about ±5%. Cycles equal time times frequency per run, but cross-run frequency and interference move the work's cycle count through cache and power state.

For cycle-level A/B on text-class cells, only back-to-back runs inside one script, or the interleaved xslow ratio, are valid. That is exactly what caught the re-roll tax below.
## Machine-code placement in the fused symbol


A strict cut of 8.7 Ir per sequence from the real-offset rotation fold cost text.zst3, text.zst9 and text.zst19 2.8%, 4.2% and 5.0% reproducibly while winning json and skewed. On text the per-sequence budget is about 1% of the decode, so the source is displaced non-loop code such as the xxh64-absorb and copy-tail layout.

Gate every fused-loop change on a text-stream cell, not only on the shape it targets, and expect the tax to dwarf cuts below 10 Ir per sequence. A placement draw can reject a piece, never a structure, as the next entry records.
## Piece verdicts inside a saturated loop


On the batched-read loop, swapping the packed FSE entry for libzstd's per-field layout removed 15 Ir per sequence and bought 0.1% cycles on json, with IPC falling from 3.61 to 3.35. The unconditional literal copy and register-resident repcodes lost the same way. Each change re-rolled register allocation across one saturated loop, so the A/B measured the re-roll rather than the change. The same three pieces are part of the libzstd-shaped loop that won 14% to 51% on 2026-11-02.

When a reference loop is far ahead, first build the reference with our compiler backend to confirm the gap is structure, then port the structure whole and measure that. See [dec-gap](../../bench/dec-gap.md).
## AMD annotate percentages


A single instruction, `add $0x10,%r8` as the copy loop's cursor bump, showed 25.4% of the whole profile inside a symbol holding 23.2%, and the reference's xxh64 region showed line shares summing above its own symbol total. Annotate locates hot regions and does not measure them. Quantify with callgrind Ir plus `perf stat` totals and per-symbol `perf report` shares only.
## Checksum-less pair differential


`zstd -3` and `zstd -3 --no-check` produce the same parse on the same raw, because only the trailer differs. The difference in cycles between the checked and unchecked runs on one side is that side's pure xxh64 absorb.

On text.zst3 it measured ours at 346.0 M against zstd's 345.5 M per 40 runs of 32 MiB, exact parity, while the profile annotation had suggested a 25% difference. Without the split, the checked-wall ratio of x1.071 hides the real residual of x1.15 unchecked.
## Replay harness biases


Overshoot kernels cannot be value-verified on a bare trace, because the executor's correctness invariant is that every read stays behind the write cursor only when the next sequence's literals or match rewrite the overshoot region in address order. A bare match list has no such writes, so a correct kernel mismatches and a mismatching kernel can look fine. Verify exact-tail kernels only.

Interleaving several kernels inside one process also carries order bias, because an NT-store kernel evicts the working set and the first kernel of the next round reads 31.7 ps/B where isolated runs read 23.0. Run one kernel per process and interleave at the process level.

- AMD `ex_ret_brn_misp` samples land in the middle of basic blocks, within about 10 instructions on either side, and cannot be attributed, and `ibs_op` has no miss filtering. Attribution relies on stub bisection plus `perf stat` totals, and annotate or script miss attribution is untrustworthy.
## Stub bisection checks


After stubbing, verify the exit code and output length, because a run once took the half-run leftover values of an out-of-bounds-read segfault as numbers and cycles falsely dropped 13x. Stubs must preserve real offsets, because a `%4096` small offset changes the whole memory-access pattern and makes the wrapped cases vanish entirely, which invalidates the mix.

- Empty-copy execution owns most of the misses. Stubbing the copy loops out one by one decomposes the miss sources, and the decode side holds only 10% while the executor with its copy loops and gating accounts for 88%. That overturns the old assumption that decode-side branches are the bulk.
- Wall clock is untrustworthy for 10 GB/s-class loads. text.zst3 and text.zst9 vary by 0.82x to 1.25x within a run. Look at cycles, or trust only interleaved medians of loads below 9 GB/s.
## The WRAP instantiation


Streaming wraps after 2 MB and carries the full set of view values, about 12 stack round-trips per sequence. Even after cold outlining there can be residual spills, with about 20 live values against 15 general-purpose registers. Before annotating, confirm which instantiation is hot.
## Known hotspots can be limits


After perf showed xxh64 absorb at 8.15%, investigation ruled it out, because the 8-lane version at about 20 GB/s is already faster than zstd's scalar 4-lane and is not the gap source. Compare against known limits before taking a hotspot as work to do.
