# Measurement and tooling
## ST output depends on process history


The thread-local matcher state pool persists across frames, following the design that leaves u32 tables uncleared, so the same input can produce different but each valid frames under different in-process histories. Level::Fast on a 24-byte input measured 37 B as a streaming raw block against 35 B through bulk. The symptom is a test that passes sometimes and fails sometimes inside one process.

Cross-build byte comparisons must therefore use a fresh process, which is the dump tool's purpose, and the cross-check oracle uses two-sided decode validity rather than byte equality, which is what the encode_stream fuzz target does. The same problem for MT jobs was fixed by per-job table clearing, and the ST variant stays open. If libzstd-style same-input same-output determinism is ever wanted, the options are a reset with table clearing at a speed cost or the frame-epoch scheme recorded with the u32-table pitfall above.
## Measure the rebuilt binary by name


An earlier era's `target/release/zstdx-cli` survived in `target/`, and the binary has been named `zstdx` since. It kept answering shell history's muscle-memory path, so a full fixture sweep ran on a three-day-old binary and reproduced a baseline that no longer existed, complete with the pre-swap `-3` equal to `-4` signature that had already been fixed. The symptom worth memorizing is an unchanged number where a landed change must have moved something.

Verify the artifact with a discriminator level whose output a landed change provably alters before believing any before and after on a rebuilt binary. Also note that `cmd &` in a per-call session shell backgrounds the whole `a && b &` chain unprotected, so a build and sweep pair can die between the two and leave the stale binary answering.
## Trainer cross-check artifacts


`zstd --train -v`'s small-corpus warning prints "the source size" as `nbDmers`, which is the training split's byte count minus 7, the `MAX(d,8)-1` term, and not the split's size. Reconstructing the CLI's train and test sets from it mismatches by a few bytes and sends you hunting for a shuffle divergence that is not there. The shuffle ports exactly: same seed 0xFD2FB528, same LCG and same byte-sorted input give the same 64/22 split and identical byte totals, verifiable against the level-2 "Training on N samples of total size X" line.

`ZDICT_finalizeDictionary` computes the dictID over the untruncated content before shrinking it to capacity, so re-finalizing an extracted content section yields a different but valid ID, and the differing bytes 4 to 7 of a cross-fed header are an artifact rather than a table difference. Compare tables only past the ID.

The trainer's internal `evaluate` scores checksum-less library frames while a CLI referee loop adds a 4-byte checksum per file, so absolute values differ by a per-file constant and only rankings are comparable. A related measurement trap is that two trainer A/B harnesses writing the same output paths in parallel corrupt each other's dictionaries and logs, and the race reads as a nonsensical k pick. Serialize experiments or namespace their outputs.
## Newest-wins tables insert once


`emit_chain`'s covered fill re-inserted the greedy match start and linked the slot to the head it had just become, which is a self-reference. The walk burned its whole depth budget on the loop while the bucket's dictionary twins sat behind the severed link. `seqstats --ref-frame` showed we emitted `ml=5 of=15` where the CLI found `ml=15 of=3552` 8 hops down the same chain, confirmed by a Python replay of the fill and walk.

Skipping the fill below the scan's frontier through `max(start, filled_from)` removed the self-link but regressed every dictionary row on the 3-seed fixture, by 14 to 81 B for chain and 144 to 452 B for opt. The scan's rep-probe advance skips positions the re-inserting fill was silently covering, and opt walks probe far past the emission start, so the skip also dropped the earlier interior fills. A second-order trap is that the corruption silently re-tuned knobs, and depth sweeps showed saturation from 128 up because the severed walks made every depth look saturated. The exact-once shape of filling the rep-advance holes and never re-inserting a head landed on the dictionary-armed greedy row on 2026-11-05, see [encoding](../../perf/encoding.md) and [negative](../../negative/matchers.md). A fixture pinning the severed walk must make its later duplicate share only the searched position's window, because a backward-extension twin reconstitutes the exact-once emission through the advanced position's own bucket: `greedy_dict_rep_advance_inserts_the_searched_position`'s first fixture passed on the corrupted build for exactly that reason.
## Pooled-state replay guards


`seed_gate_probe` guarded `data.len() < GATE_MIN_BLOCK` only inside the `probe.is_empty()` arm. A pooled matcher whose probe table was warm from a previous job then met a zero-length strip at a staging or freeze boundary, such as stream-mt finish with `strip_is_uniform(&[]) == true`, and the uniform branch's `read8(data, 0)` faulted on the empty slice's dangling pointer at address 1.

That was a flaky SIGSEGV, and the parallel ratio sweep hit it in about one run in three while the serial sweep never did, because pooled-probe warmth depends on worker scheduling while the frame bytes stay deterministic. `coredumpctl gdb` on the preserved core pinned the frame to `seed_gate_probe` plus `MtEncoderCore::encode_jobs`, and the faulting `imul (%rbx)` with the gate's multiply constant identified the branch before any source reading. Any guard that licenses an unchecked read must hold on every path that reaches it, warm-state replays included, and the regression test warms the probe through `prefill_job_strip` and then pre-fills an empty strip.
## Walk loop shape


`strided_distinct`'s flat-head diet landed as a two-stage scan. Stage 1 counted 512 strided samples per byte for a `top`-share RLE verdict, and stage 2 resumed through `head.iter().step_by(8).skip(samples)`. Two codegen taxes were invisible to every semantic gate. `Skip<StepBy>` compiles to a per-iteration `spec_nth` call with the iterator state spilled to the stack, measured at 3.4 times the screen's cycles with `far_screen::screen` going from 2.45% to 8.25% of the profile and text.fastest and text.fast losing 13% wall. Slicing from the byte offset through `head[samples*8..].iter().step_by(8)` restores the plain stride induction.

An early `return` inside the walk's body for the 128-distinct cap keeps the loop un-rotated where the cap-free body unrolls 2-wide, so a head that never reaches the bar pays the scalar shape for nothing. The final form is a SIMD `is_uniform` probe on the first 4 KiB for the exact-RLE zeros reject, then a bounded cap hunt over the first 4096 samples where wide-alphabet heads leave for the twin scan early on random's leg, then a cap-free remainder walk for text and json with the old-shape unroll.

It is verdict-exact against the pre-diet walk for every non-exact-RLE head, with a full-ladder dump byte-identical against the base build, which is stronger than the counting stage whose 99% top-share reject also miss-armed near-RLE openings. text.fastest and text.fast recovered to 13399 and 13317 against 13470 and 13427 in the same locked window, and zeros and random keep the diet's wins. Iterator-adapter stacks on hot loops get disassembled before landing, and a per-element branch that only some payload classes take belongs in a bounded prefix loop, not the shared body.
## Residue gates key on role


The reach-probe shrink's apply gate matched `chain_reach == Some(KEEP_REACH)`, a value the btlazy tree rows carry as their stock search domain. A pooled stream core's leaked row-9 Shrink verdict collapsed row 13's 4 MiB tree domain to 4 KiB and made the frame's bytes depend on what the core had compressed before. json l13 stream was 13.5% off bulk, both decode-valid, and the new `dump --stream` grid was the only gate that caught it, because every size-keyed and fresh-process gate was green and the matrix execution order never triggers it.

Residue-class gates belong to the same predicate that declares the state's owner, here `reach_probe_eligible`'s Chain-only rule, never to an incidental value equality. The diagnosis used a pooled-versus-fresh process replay of the exact in-process level sequence, and the poison needed the full l1, l3 and l9 warmup while an l9-only prefix was clean. Instrumentation that makes a bug disappear is a layout or UB signal to switch tools, and a probe-print build un-poisoned this one before valgrind memcheck became moot through the replay.
## Inline job views start at the strip head


`run_inline_job` sliced `buf[..hi - buf_base]` while its `first` index counts from `strip_lo`, which is the same convention as every posted job's frozen view, whose pointer `post_job` builds from `strip_lo`. The two align only while `strip_lo == buf_base`, which holds until a flush re-grids `job_start` past the buffer's head under a window shorter than it. Forced window_log 9 or 10 with a flush at 2051 of 4095 gives `strip_lo` 1027 and `buf_base` 0.

The inline path then handed the runner a view offset by 1027 bytes and the job re-encoded stale earlier input as the post-flush span. The frame decodes fine, so only the checksum compare or a byte compare catches it, as `ChecksumMismatch` with the checksum on and a silently wrong roundtrip without. It reproduced identically on the pre-wiring base, so it is pre-existing since the inline path's introduction. A view's index convention is a contract shared by every builder of that view, and the inline twin of a worker path must construct the same view the worker gets, not merely call the same runner with a differently anchored slice.
## Flush-regridded epoch ends


`post_ready` computed `end = job_start + epoch_size` but gated the post on `pos >= growing_epoch_end(job_start)`. On the untouched lattice the two agree because a job never straddles an epoch. A mid-stream flush parks `job_start` at an arbitrary mid-epoch offset, where the lattice-relative `end` can lie past the epoch boundary. The gate then opens exactly at the epoch end while `end` still exceeds `pos`: on a 3 MiB stream with a flush at 2622464, the job `[2622464, 3671040)` was due at pos 3145728.

`post_job`'s `debug_assert!(pos >= end)` caught it in debug, while release's frozen view read `end - strip_lo` bytes that were never staged and emitted a corrupted frame. The fix clamps the post step to the epoch end, the same clamp `build_bounds`' walk applies, and it is a no-op on the untouched grid, so existing frames keep their bytes. A posting gate keyed on a coarser boundary licenses every finer step under it, and the gate and the boundary function must share the clamp, not just the arithmetic.
## Arm poison walls at construction


The walls' contract is exit-fill and entry-assert: the producer poisons the slack past the logical length on its way out, and the consumer asserts it intact on its way in before the legitimate overwrite. A freshly constructed Vec's spare capacity is uninitialized, so the first entry-assert reads indeterminate bytes and trips on noise. The MT core's first test run was a false positive until the constructor learned to arm.

Any buffer joining an entry-assert protocol gets `fill_margin` at allocation or reset, not just after the first write. The protocol covers only slack that nobody legitimately touches, so a span the read pump drains into, a bitwriter's deliberate past-end flush inside its reservation, or a caller-live carry zone such as the owned window's committed-length to staged-end span under the pre-split convention cannot be walled without changing that contract, and must be recorded as uncovered instead of silently skipped.
## Byte-granularity cadences dominate debug sweeps


The 2026-11-17 audit found `write_encoder_matches_oneshot_bytes` at 111 s and `raw_encoder_matches_write_encoder_bytes` at 22 s with the encodes themselves nearly free: the cost was the 1-byte arms — `chunks(1)` writes and 1-byte `split_output` takes over multi-hundred-KiB shapes, at roughly 40 µs per call in debug builds. Both dropped under 0.25 s once the byte cadence ran only on the small shapes, whose sub-block accumulation path it shares with any sub-block-sized cadence (7 KiB writes, 977-byte takes).

A sweep dimension that multiplies per-call work (single-byte writes, one-byte deliveries) belongs on the smallest shape that exercises the accumulation state, never on the large ones; the large shapes keep the coarse cadences that reach the same code.
