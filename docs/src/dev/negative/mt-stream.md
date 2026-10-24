# Falsified directions: MT and streaming

Every entry here is backed by A/B data. If the premises change, such as the table structure, window, corpus or CPU, re-evaluation is allowed, but new decomposition evidence must come first. The differential-testing discipline is in [benchmarking methodology](../bench/methodology.md), and the rules are in the [index](../negative.md).

## Job model

### Frame-per-job MT encoding

Frame-per-job MT encoding was rejected at design time. Multi-frame output burdens the decode side and is unfriendly to single-frame consumers. Stage-2 single-frame overlap jobs are the right answer.

### Overlap without strip prefill

Enlarging overlap alone, without strip prefill, is ineffective. The insertion paths only insert job-local positions, so the strips never get indexed, and enlarging overlap is mathematically ineffective.

### Fixed-grid prefill for periodic repeats

Fixed-grid prefill to find periodic repeats is mathematically unsolvable. Clustered 5-grams recursively bury twin slots with equal values and phase-alignment traps, and dfast and chain suffer the same burial. Seeding at the job start is mandatory.

### Pledge without a job-size cap

A huge pledge without a `job_size` cap causes a memory blowup. `MAX_JOB_SIZE` is 1 GiB, the same cap as zstdmt.

### Thread-local pool across scope bursts

A `thread_local` pool across `thread::scope` bursts always misses, because the scope threads are fresh each time. The pool must be owned by the encoder and carried across bursts.

## Job strip and LDM state

### Far-dead strip-cap screens

Two screens were tried alone in 2026-09-18: the head engagement screen `ldm_head_parses`, where a narrow alphabet means cap, and the far-lag repeat probe alone with a count bar. The head screen alone caps the corpus text and json fine with identical bytes, because their far leakage is shadowed by near-period repeats. It also kills any far-tiled stream whose repeats start past the head screens: text.raw x19 with a 32 MiB period and the head taken from text.raw's own data moved from 125,724 to 1,637,683 B, 13x larger, because the cap removed exactly LDM's beyond-reach twins. The far-repeat probe alone protects the tiled stream and refuses to cap the corpus text and json, because their collision leakage at a lag of at least 4 MiB clears any count bar at 199 and 362 strided hits, so the strip-bound cells keep their 470 MiB/s with no win.

The landed screen is the AND of both. The head screen runs first and kills the shadowed near-periodic classes, then far-repeat dominance runs with a bar of 1/12 of samples, above the 4.4% leakage ceiling and below any paying class. A head-prefix screen can never see repeats at lags beyond its own span, so the span must be the one the strips actually index. Repeats existing does not mean the far class pays, because shadowed repeats with a nearer twin within reach leak far-lag hits without ever winning a parse.

### Chain head origin in the u32 domain

The chain head origin in the u32 entry domain was measured on 2026-09-23, with `dist = (pos + origin + 1) - entry` mod 2^32 and an explicit empty-sentinel reject, plus a never-zero-links companion where link slots store their own position pack when the threaded head is empty, making every link read check-free. gungraun Ir read json.balanced +3.94% and text +3.68%, with json.balanced bulk-ST wall at -5% and every paired round slower. The links form alone, on top of the checked head read, read json +12.24% and text +14.86%. The write-side select per insert dwarfs the saved per-link compare, because inserts run per position and the saved compares per walk step do not outnumber them until searches run multi-step per position, and the select also broke the insert block's scheduling.

The landed form resolves in the u64 domain, with `q = pos + origin + 1` and `dist = q - entry`, one subtraction per read and one fewer than the origin-free form's sub and add pair. The origin is capped at 2^30 by a real clear once per about 1 GiB, and since the 3.5-GiB negated-floor epoch roll it uses the same no-check property. Live entries stay below 2^32, and the empty sentinel dies by arithmetic, since `q >= pos+1 > reach`, with no check anywhere. json and text Ir read +0.87/+1.10% with walls at parity. The entry-domain choice is itself a hot-loop lever: a modulus that forces an explicit sentinel check, or a write-side branch to keep sentinels out of links, costs 4-12% Ir on walk-heavy shapes, while the u64 domain plus an amortized cap restores the origin-free per-step cost exactly.

### One-bit parity tag

The idea was a 1-bit parity tag replacing the opt tables' epoch, using bit 31 of a u32 entry flipped at every reset point, the u64 form's 16-bit epoch compressed to O(1) invalidation with no clears. All corpus gates passed, including the dump, the 120 cells and dll at st and mt8, but dll32 l16 mt4 grew +332 B deterministically on both builds, while st and mt8 of the same file were identical. The bulk-MT job path resets each pooled state twice, because `take_slice_state` and `run_job_with` each call `reset_slice_state`. The monotone epoch shrugs this off, but a 2-period tag neutralizes it: consecutive jobs on one thread_local state share parity, and the earlier job's entries revive wherever it ran within the later job's window band. The geometry was dll32 l16's 8 MiB jobs over a 4 MiB tree domain, while the corpus cells' same-state job pairs sit window-far apart and mt8 ran one job per state. The revived entries were strip-tail fill-skip interiors whose head slots the tail-half tree fill never rewrites.

A width-2 invalidation domain cannot back a pooled state's unbounded lifetime. The landed form advances a per-state monotone origin instead, with `opt_origin += last_pos + 1` per reset and `+= block_end + 1` at ultra's pass 1. Stale entries rebuild as `cand = q - advance < 0` and die below the window floor, double resets only add extra advance, and output stays independent of the origin value. Any tag narrower than the state's reset count needs the reset count bounded or the tag monotone.

### Halving the opt job strip

Halving the opt rows' MT job strip for both the tree and LDM, with `strip_for_*` at reach/2 for parity with C zstdmt's btopt overlapLog 8, grew dll100 opt mt8 from 20,391,886 to 21,234,397, or +4.13%. dll32 opt mt8 read -0.13% and dll16 -0.27%, which is denser, and corpus json.opt.bulk-mt read -0.17% through job re-split interplay, while json.opt.stream-mt read +0.13%, text and skewed at most +0.021%, and st stayed byte-identical, measured on 2026-09-16. The [4,8) MiB band of job-start history is load-bearing exactly where the far class lives, which for dll100 is the mass twins and LDM is the server there, so shrinking the strip cut LDM's ingestion too. The decomposition's strip-fill cost of 40-73% of job time is attacked through the tree fill only, the tail-half bound in `prefill_job_strip`, leaving the LDM strip whole, as recorded in [mt-stream](../perf/mt-stream.md).

### Job-start seed probes against a stale index

The job-start seed and LDM probes compared candidates against a stale scan index, which caused unpledged row-9 stream-MT frames to corrupt deterministically past 32 MiB on 2026-09-17. text.raw x1.5, x2 and x3 corrupted at w2, w4 and w8, and stayed clean at w1 and at l3, l13 and l17. The rep probe may advance `pos` one byte past the iteration's `idx`, and a seed or LDM offset of exactly that advance resolves the candidate onto `idx` itself. `read4` and the extension then trivially match the position against itself, and the store gate prices the zero offset with `ilog2(0)`, with a release backtrace straight into `pays_for_offset_lit`.

The panic rode two amplifier defects. The stream's `drain_all` returned with the poison unsurfaced, because aborted jobs assemble empty and drain like completions, so the frame shipped missing whole jobs. It decoded short by exactly the poisoned job's span, with the first wrong byte at a block start as later far matches read the shifted history. The bulk pool's workers all exit on poison, leaving unclaimed slots forever `None` and deadlocking the ordered-assembly wait, which produced a 14-minute futex hang with every worker thread gone.

Both layers are fixed. The probes take strictly older candidates only, with `ci < idx`, which is byte-neutral by construction because a self-compare either panicked or never updated best. The full-ladder dump and 5 shapes by 6 tiers of emitframe stream-mt8 plus mt2, mt4 and bulk-mt8 stayed byte-identical against the extension. `drain_all` surfaces the poison once the pending queue drains, and the bulk assembly fills unclaimed slots under poison so the panic resumes instead of hanging. The w1-versus-w2 asymmetry comes from the repcodes: a carried worker state's repcodes are job-tail values that do not trivially match a period-1 run, while a fresh state's format-default `rep[0]=1` does, so parallel jobs on fresh states are the corrupting geometry. The regression is `strip_tail_period_run_roundtrips`, with a 384 KiB word-run head, zeros and a repeat at w2 and w8, and the generator is fixed so the anchor alignment reproduces.

### Chain grid fill uop diet

A uop diet and whole-loop asm for the chain grid fill were re-tested on 2026-09-26 against the 8-way memory-bound framing. Disassembly of the strip-fill inner loop on balanced H21/C20 with a text 4 MiB strip shows 11 instructions per position, register-resident throughout: an unaligned load, a 5-byte mask, imul, a variable shr, a head load, a chain store, a head store, two `add $3`, and a compare and branch. No spill cluster exists to remove. The loop is at its floor, at 0.82 ns per position warm, or about 4.4 cycles. The cold-pool excess was first-touch faults at 2513 per fill and 68% system time, fixed by the state pool and not by loop surgery.

### Position-pure split scan

A position-pure parallel split scan for the chain grid fill, in the `fill_parallel` treatment class, fails on cost structure. The grid fill's scan is 3 of 11 uops, the hash5, and the rest is one random head-table RMW plus one chain stream store per grid position, fixed by the newest-wins and link semantics. The joiner-inserts-in-order form would serialize the exact work the loop already overlaps at 4.4 cycles per position warm. The scan is much cheaper than the store, so the sanctioned parallel form's precondition fails by construction and it is not worth building.

## Finish tail

### Segment-parallel spf build

A segment-parallel spf build at LDM batch-freeze boundaries was the researched far-alive repair, with parallel chain-grid segments and a per-segment head-snapshot handoff. The finish-tail attribution put the serial build inside the donation window on every measured far-alive cell: dll100 unpledged stream-mt8 spends 74.3 ms reaching the verdict, with build 47.7 and 26.6 parked, so the build is already fully hidden and parallelizing its fills moves nothing until the donation wait shrinks. Cheaper or earlier probe parses are byte-affecting by design, and the drain held the redundancy, not the build.

The landed lever is schedule shape, not build parallelism. One serial chain runs to `s_max` and publishes a snapshot per prefix-job start, cutting total fill work from 200 to 87.5 MiB with no nested stock fills. Publication is gated on the far-class screens as a Keep-verdict prediction. Ungated, the clones published on far-pending frames cost text 8 to 13%, the same falsified mechanism of table copies on the verdict's critical path. dll100 wall read 758 to about 690 on cold pools, and -3 to -6% on the pooled tree where pooling claimed the same cold-fill headroom, as recorded in [mt-stream](../perf/mt-stream.md).

### Speculative strip prefill

The finish tail's speculative strip prefill posted each far-dead reach-capped strip fill as a pool task inside the donation wait. It ran the stock `prepare_job_state` and `prefill_job_strip` sequence into a pooled state and handed the state to the job through a skip-prefill run entry, which is byte-exact, and vetoed the fill when the verdict had already drained or the far-class head screen passed. On text.balanced stream-mt8 with 3-iteration jobdecomp, the drain fell from 11.6 to 4.3 ms with jobs parse-only and 56/56 ladder byte-identical, but the donation verdict stretched from about 14 to about 24 ms, because 8 speculative fills' table traffic contends with the two 2 MiB probe parses. Net wall read 37.0 against a 36.5 base, and fanout sweeps at 2, 4 and 7 all landed inside the plus or minus 10% noise band.

The donation wait is the verdict's own critical path, not free idle. Work stuffed into it slows the verdict by roughly the work's memory footprint, and the verdict gates both the tail grid, where a Shrink re-slices from zero, and the strip spans, through the reach choice and the far-class cap. Nothing byte-neutral can start the tail work before it, and the post and drain span is a floor. The change was reverted.

### LDM strip-fill segmentation

Segmenting the LDM strip fill at arbitrary call boundaries, in the shared prefix fill's first form with snapshot and continue at any offset, produced a byte divergence. The asm gear-feed's 4th checkpoint, c4, diverges from the portable reference: the differential harness matches c1 to c3 on 1000 of 1000 random inputs and c4 differs, so the split set depends on where a call boundary lands inside a batch. Segmented and one-shot fills of the same span disagree. Only LDM batch-freeze points are exact cut points, because the batch count is zero by construction and the gear group grid restarts there inside a stock fill too, through `LdmState::fill_to_freeze`. Fixing the asm c4 would change LDM-armed frame bytes and stays a separate, non-byte-neutral follow-up.

### opt and btlazy rows in the shared prefix fill

Admitting the opt and btlazy rows into the shared prefix fill in a release build on unpledged streams of at least 16 MiB at l17 to l19 segfaulted with a core dump. `chain_grid_fill` ran on an opt driver whose `chain` table is empty, so `len - 1` underflowed and the unchecked chain store wrote wild memory. The strategy `debug_assert` is compiled out in release, and `emitframe text.l17 --stream` found the core dump. The fix is an eligibility gate at the entry, `MatchGeneratorDriver::spf_strip_fill`, for chain rows only, plus the adopter prefix bound of `start <= overlap`, since a larger start's strip is mid-stream and adopting diverges. Release builds have no `debug_assert` tripwires, so caller-side gates carry release safety and callee assertions only debug it.

## Reach-probe donation

### Stream-MT donation variants

Three donation variants were measured on 2026-09-17. Variant (a) ran one donation task doing keep and shrink serially on a pool worker. Variant (b) held a `FrozenSrc` into the accumulate buffer and counted in `n_incomplete` like an encode job. Variant (c) pre-reserved the keep-side buffer scale at 64 MiB, poolable, at the gate alongside the donation. Variant (a) was a wash against the stock probe on text at 372-392 against 366 MiB/s over 10 matrix rounds, because keep at 16.3 ms plus shrink at 15.3 ms serial lands after the about 8 ms pump either way. The keep side is real work job zero owed, and only the shrink parse is exposed time, so one worker cannot beat the about 29 ms stock dual parse enough to matter. Variant (b) read json 349 to 296 MiB/s, because the pump fills the about 10 MiB pending-scale buffer and then `make_space` waits for quiescence on the counted donation and stalls behind a verdict a shrink-class stream will not deliver until the whole keep parse finishes. The pump must stay recycling-independent of the donation. Variant (c) read text 446 to 405 with 0.28 spread, and best rounds above 800, because a fresh 64 MiB mapping per stream re-introduces the first-touch fault bimodality the pooled-buffer fix removed, worse than the incremental growth ladder it replaced.

The landed form uses two parallel donation tasks on owned head bytes, uncounted and holding no views, with a lazy verdict and the kit pool, as recorded in [mt-stream](../perf/mt-stream.md). Buffer growth stays incremental through `make_space`.

## Streaming buffer and pump

### Reading into Read spare capacity

Reading directly into `Read`'s spare capacity, beyond the read-pump double copy, was dropped. Handing uninitialized buffers to arbitrary `Read` implementations violates the trait contract.

### Ping-pong buffer recycling

The idea was to regenerate the accumulate buffer into a spare when the dead prefix is not enough, retiring the old allocation behind an in-order assembled watermark, as the pump-never-waits-on-workers form. An interim interleaved A/B on 2026-09-16 measured it equal to the simpler quiesce-gated wrap and grow on every fastest and fast stream-mt8 cell. The audit found three lifecycle defects in the WIP form. The spare was reused without clearing the retired buffer's length, which gives stale-prefix corruption plus runaway capacity at the 256 MiB ceiling. The poison abort completed unclaimed jobs without setting `done` or decrementing the posted count, which gives an infinite spin in the quiesce wait. The in-buffer wrap is a move of the live tail whose destination overlaps the views of still-running jobs, so it cannot run ahead of quiescence without ring-buffer job views, zstdmt's model with a different job contract. The measured overlap value on the epoch-sized buffer is zero, because the pump laps the workers about once per epoch and the quiesce wait is bounded by the in-flight encode span. The quiesce-gated form landed.

### Larger pump pulls

Larger read-side pump pulls of 64 or 128 KiB instead of a 16 KiB staging chunk were measured in an interleaved A/B on the mt8 read path over text and json at fastest and fast on 2026-09-16. The 64 KiB median ratio against 16 KiB read 0.89-1.00, below 1 on 5 of 6 cells, and 128 KiB read 0.85-1.00, below 1 on 5 of 6. A pull larger than the grid's posting granularity overshoots the epoch boundary inside one append, so the buffer pressure shifts to mid-epoch recycle points, which are quiesce-gated, instead of boundary-aligned ones. The 16 KiB chunk stays, as a persistent per-encoder buffer, because the zeroing and not the size was the cost.

### Incremental last-epoch posting

Posting the growing grid's last epoch incrementally as its bytes arrive was rejected at design time. The read-pump round exposed json.fast's two-wave floor, where epoch N's jobs post only when fully buffered and epoch N+1's tail only at `finish`, so a 32 MiB stream runs ramp-read, wave and wave with about 1.5 ms of read unhidden. The stream-end tail re-slice is a function of where `job_start` sits at EOF. Incremental posts advance it, for example 4 MiB grid jobs posted at 20, 24 and 28 MiB against today's 8x2 MiB re-slice of [16,32), and the frame bytes change, which the byte-identity gate of dump and 120 cells vetoes. A pledge gets the pipelined fixed grid instead.

### Zero-copy slice source

A zero-copy `&[u8]` source for the read pump was rejected at design time. `read::Encoder<R>`'s pump is generic over `R: Read`, and detecting that the source is a slice inside the generic needs specialization, which is unstable, or an `Any` downcast. The bench's `&raw[..]` borrows a local, so it is not `'static` and the downcast cannot fire either. The single copy is the Read-trait floor for an opaque source.

### Deleting the input copy chain

The streaming-encode input copy chain's deletion was the borrowed-window, direct-fill and ring family, prioritized on 2026-09-30. The idea was to fill the owned window straight from the Read or write path so that raw to read-buffer to staged to win collapses to raw to win. The ceiling was probed by gathering the whole stream into a reused thread-local buffer and running the bulk direct-window core, env-flipped inside one binary with position-balanced alternating rounds.

Healthy cells were flat at every copy count that keeps at least one input copy. text.fast stock with 3 copies and compaction read 6644-6742 against 6712-6903 MiB/s for gather plus bulk with 1 copy. text.fastest read 7083-7114 against 7094, text.best 1022 against 1021, json.fast 432-446 against 448-464, and json.best 29-30 on both, while the zstd sides stayed flat at 5248-5355 throughout. The zero-copy bulk core under `prof enc 3` measures 10973 MiB/s on the same shape, +63% over the stock stream.

The marginal input copy is not linear. One serial copy per byte already saturates the resource the scan needs, so deleting two of the three copies buys nothing, and every owned-window streaming mechanism is bounded at the gather level, which is stock. Such a mechanism necessarily keeps at least one fill copy, plus the compaction schedule whose `win_base` timeline is byte-load-bearing, because every parse floors candidates at `reach = min(pos−win_base, max_window)`. The remaining 60% sits only behind a zero-copy borrowed window, which a true stream cannot have, since the Read-spare-capacity contract was falsified above and caller memory is not contiguous with the history. The collapse reproducer's degraded phase, with in-pass text.fast at 4960-5030 and the zstd side at -5%, ran 5/5 rounds in one about 4-minute window and then 0/36 across the interleaved series. That is a machine-wide phase and not an orderable per-process state, so no collapsed-phase ceiling sample exists, and the direction closes on the healthy-state bound.

## Allocation

### MADV_HUGEPAGE on the opt tables

`MADV_HUGEPAGE` over the opt and dubt tables' fresh `alloc_zeroed` allocations followed the accumulate-buffer precedent, aiming the fresh-state Fast to Opt build's first-touch faults at the stream cells' reset spans. The jobdecomp json stream-mt reset spans rose from about 45 to about 95 ms summed, and ultra rose from 133 to 294, on this 59-day-uptime fragmented box with the advice in. The same build without the advice sat below the old u64 baseline from the u32 halving alone, and the walls stayed dominated by the fill and moved -16 to -22% regardless. Huge-page allocation under fragmentation pays direct-compaction stalls that exceed the 4 KiB faults they replace. The advice helps dense steady-state buffers, as with the accumulate buffer, and hurts once-per-encoder cold mappings, so it was dropped.

## Checksum

### Streaming-MT checksum off the calling thread

Four placements for the streaming-MT checksum absorb were measured on 2026-09-15. XXH64 has no chunk-level merge, because the spec's `mergeAccumulator` is a finalize fold, so per-job digests cannot combine and one serial state must absorb in order, as the [decoding page's merge proof](decoding.md) shows. zstdmt's SerialState is an ordered per-job `XXH64_update` under a mutex and not a merge. The variants were (a) pool-job pickup like zstdmt, where our bursts freeze the whole range at post so no ordering is even needed, (b) a sidecar thread through the existing `AsyncChecksum`, (c) the hash fused into the pump memcpy through `write_appending`, and (d) the baseline of absorb on the calling thread at fire time.

In-process interleaved 3-way and 4-way A/B on 32 MiB at w8, on both the write path and the read path with 64 KiB chunks, read as follows. Caller absorb is free, with write text.fastest at x1.005-1.041 and every read-path cell at x0.958-1.013 on and off. The sidecar read x0.96-1.25, never beating the caller and reaching x1.09-1.25 at text.fastest. Pool pickup read x1.15-1.23 at text.fastest and x1.05-1.11 at text.fast. The fused pump read x1.08-1.21 on the text cells. The caller's absorb cost shows only in slow machine states, with the first session at on/off 1.19-1.20 at text.fastest and `Xxh64::absorb` at 16.65% of whole-process cycles.

The fire-time caller absorb already rides the fire-to-drain window that overlaps the encode span on a thread that would otherwise wait at the barrier. Every offload pays the same 1.6 ms of hash plus a new concurrent memory stream competing with 8 encoders on the bandwidth-saturated fastest-tier cell. The sidecar extends the burst, pool pickup starts after the first job finishes and pokes past the last, and the fused copy runs slower than memmove and pays on the pump directly. The residual on/off tax at text.fastest is a resource tax, with the serial hash sum about equal to the pump memcpy sum and the encode span, and no thread placement can remove it. Only a cheaper hash or de-saturating the cell through the pump and burst work can. Nothing landed, and the pre-change placement stands.

## Decode scheduling

### Caller validation jobs

The calling thread claiming validation jobs while it pooled staging during the fused phase was a sub-variant of the landed caller-staging, where the caller runs `claim_validation` between staging segments, measured on 2026-09-17. Three-round interleaved medians against the caller-staged-only form read json mt8 5252 to 5365-5392, which helps, but dll32 mt8 read 4298 to 4246, or -1.2%, and dll32 mt16 read 4192 to 4069, or -3% consistently. The staged-only form beat it on every cell, at json mt8 5411 and dll32 mt8 4411. A validating caller reads the staged set while workers still write it, which is the falsified hybrid execution's shared-resource asymmetry at smaller scale. The caller pools staging only, since it writes the same streams the workers write and adds no reader of the staged set.

### Hybrid overlap

The piece-decode hybrid overlap ran optimistic serial stage B on the caller under a cap, leaving a quarter plus a 2 MiB floor for the pieces, and switched mid-flight to piece execution at the validation verdict. It was the no-regression form sketched for the staging barrier, implemented and measured fully on 2026-09-17, with `place(0, fcs)` up front, `SerialExec` collect and execute interleaved into the staging and settle waits, straddle-piece re-execution, and serial-prefix checksum handoff. On 32 MiB ramp frames at D=2 MiB with `files` medians, json mt4/mt8/mt16 read 3259/5073/5417 against 2359/2369/2222, dll32 mt8 read 4211 against 2051, and skewed mt8 was at parity at 5595 against 5589, because staging is short and the verdict precedes any meaningful caller work.

Two losses compound. The caller's streaming execution evicts the workers' L3-resident staged set, so per-piece validation goes from 0.37 to 2.5 ms and the whole round from 0.63 to 7.25 ms, settling exactly when the caller hits its cap, and staging itself stretches nondeterministically, with worker completion at 2.19 ms once and 9-12 ms in other runs. Even a cap of 8 MiB only reaches 4233. The interleaved segments also delay the round posting and the verdict, so the switch lands at about 5.4 ms against the barrier's about 2.9 ms. The serially executed bytes run at about 1/5 of piece-parallel speed and postpone the parallel phase. The caller idling about 3 ms of the about 6 ms wall is true, but the memory system is the shared resource, and filling caller idle with stage-B bytes is not free. The staging barrier is not a defect to engineer around, because the barrier form is optimal among all tested variants on this box. Remaining worker-side levers only are to fuse validation into staging per piece as covering segments complete, and to shave the stage to validate to switch transition wakes. The debug run also produced a concurrency lesson: interleaving unlocked work between a predicate check and its `cv.wait` loses wakeups, so every wait must be preceded by a fresh predicate evaluation under the same lock hold. The corrupt-frame test hung exactly there, twice.

### Restart-position grid discovery

Discovering the encoder job grid from restart-block positions was measured on 2026-09-16. Job starts are restart blocks by construction, so their output positions should pin the job size, and the attempt seeded candidates from the first restarts and kept a seed whose every multiple is a restart. On json.lvl3 at 32 MiB, 92 of 128 blocks are restart points because this encoder re-sends entropy tables densely, so the restart lattice carries no job-grid signal. The seeds all fail the multiples check except a coincidental 1.75 MiB periodic alignment whose pieces carry near-boundary crossing reads at a lag of 0-8 B, and the true 4 MiB grid never enters the seed set.

Restart density destroys the signal. Grid candidates for the piece executor come from the job-size formula's lattice instead, with `ceil(fcs/2w)` over worker counts plus the overlap-raised sizes, cross-checked by measured per-piece lag. It landed with `mt_pieces`, recorded in [mt-stream](../perf/mt-stream.md).
