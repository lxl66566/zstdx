# Profiling and attribution
## Record with ASLR off


Use `setarch -R perf record` plus `perf script` to extract an IP histogram, then objdump the corresponding address ranges and read them manually. `perf annotate --stdio` output mixes in kernel disassembly and is untrustworthy.
## Keep annotate windows small


A +0x1000 window once made `FSETable::clone` look like instructions inside `emit_match`. Get exact addresses with `nm` first, then use a small window.
## The instruction-count criterion


Late in optimization, ±5% to ±25% wall-clock swings from single-line changes are usually layout or scheduling noise, and only a drop in `perf stat` instruction count is a real gain. Flat instructions with a wall swing count as ineffective disturbance. Use instruction counts for micro changes and interleaved wall-clock medians for end-to-end changes.

- AMD branch-miss event sampling has a large skid. Attribute through stub bisection plus `perf stat` totals, with the details on the decoding pitfall page.
- LD_PRELOAD counting of memcpy and memmove is the sharp tool for small-copy disasters, which is how json 32 MiB showed about 3M PLT calls.
## Register-pressure law


At 15 or more live values LLVM keeps the bit window on the stack, while 12 values can stay in registers, and a `shlx` with a memory operand in annotate means you are hit.

Removing a single memory operand is nearly useless at +0.2%, because the out-of-order engine already hides memory latency, and shortening dependency chains gives only +1%. Cutting serial dependencies through grouped batch extraction pays the most.
## Per-job spans and caller phases


jobdecomp's spans are job-side, and the stream spf build traced only its final snapshot clone while the fill loop ran through `chain_grid_fill` spanless. The 2026-09-23 decomposition booked "grid_fill 66% of summed job time" against the below-median jobs' fills, while the wall's actual whale was the caller's built-then-dropped prefix build at 18 to 33 ms on text, invisible to every span.

Before booking a per-job share as the cell's lever, time the caller phases of one finish or encode call: build, probe settle, post and drain. A summed-job-time share is a wall attribution only when no serial caller phase dominates and the phase's own output is kept.
