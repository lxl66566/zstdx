# Matcher
## Re-declare frame-scoped matcher modes


`ldm_arming` survives `reset` by contract, like the reach choice, until it is changed. The bulk path re-declares it per frame through `reset_slice_state`, but the ST stream core's `configure` never did, and nothing falsified that while the only reachable value was `Frame`, because the probe's `ProbeKeep` mutation runs on a separate pooled probe driver.

The first mid-frame mutation through `resolve_far_screen` to `FrameScreened` let the next frame on the pooled state arm at configure time with no screen at all, and a random 32 MiB pledged stream silently wrote a W26 single-segment header. Sizes would not have shown it, because random gains nothing from arming and only the header byte moves. The synthetic test's window-descriptor assertion caught it.

Every `set_*` mode on the pooled matcher needs a per-frame re-declaration audit at each pool return path, and byte-contract tests must pin header observables such as the descriptor byte, not only sizes.

The reach choice was this lesson again, twice removed. R63 stopped a leftover Shrink from collapsing rows that merely share the KEEP_REACH value through a value check in `apply_level`, and the bulk entry re-declares Keep per frame, but the single-threaded stream core's `configure` still carried a probe verdict across frames, and an open stream never re-decides it because the probe's size gate needs a declared length. A pledged json or zeros donor at level 9 left its Shrink on the pooled core and the next open stream collapsed onto the 4 KiB reach: the R87 dump gate caught `text.raw.stream.l9` at 4,662,041 B against the stable 87,238 B, one scheduling decision of the parallel pool away, reproducible deterministically on one thread. `configure` now re-declares both modes together, and the pool round-trip test in `encoder_core` pins the contract.
## Cursor and anchor invariants


`commit_space` must not set the pos cursor to the block end. Doing so left the matcher fully idle and degraded all blocks to pure literals, while rebuild-style unit tests kept passing. It must be paired with index-validity assertions for skip-matching and with repcode emission assertions.

After backward extension, the offset must use the final position difference `start - cand`.

`anchor_idx = ip_idx` is mandatory after `rep1_chain` returns, in all three emission paths. Otherwise the next sequence counts the chain-covered region's literals once more and decode fails with `TargetTooSmall`. This line was lost once when porting from the old fast code.

Repcode backward extension in the chain loop must keep at least one literal, with `floor = anchor + 1`. At ll=0, `of_value` 1 means rep-swap semantics through the ll==0 branch of `do_offset_history`, and periodic data is guaranteed to blow up without the floor. The chain loop must also probe repcodes when `pos == anchor`, mirroring fast's `probe = pos + 1`. Not probing there loses to our own Fastest by 8x.
## Keep the index domain consistent


The chain-table insert used window indices while the walk used absolute positions, and the two agree only when `win_base == 0`. Once any real driver advances `win_base`, which streaming compaction, bulk per-block adopt and MT job bases all do, the walk runs on truncated, tangled chains at an effective depth of about 2 to 5. The apparent speed lead came from burning fewer probes, the ratio was off too, and the 4x streaming collapse shares the same root cause.

A streaming-versus-bulk output divergence is the smoke alarm for search-state divergence, and the byte-identity assertion had previously covered only Fast and Fastest. The index domain needs the same invariant comment on both the insert and lookup sides.
## u32 table entry arithmetic


For cross-frame stale entries whose value exceeds `pos` with no 4 GiB period to fall back through, `cand - 2^32` must use `checked_sub`, because release wraps into a huge index and reads out of bounds. The first bench run carried that bug for a full round before re-verification caught it.

The other trap is `!u32::MAX as u64`, where unary `!` binds before `as`, yielding 0 and leaving `hi` silently zero. Bit-manipulation helpers need round-trip tests across the 2^32 boundary.
## Clear tables for determinism


Pooled state residue plus a stride-3 grid that writes only one third of the slots made sizes jump between consecutive runs on the same input in the same process, with json.Balanced alternating 5514733 and 5515064. The hazard was latent since the u32 entry change and had been masked by dense table filling.

The fix clears the head table per job like C does. Chain lists may stay uncleared, which is provable by induction: chain slots are read only at candidate positions, and candidates come only from cleared heads or this job's linked values. Frame-epoch clearing was rejected as schedule-dependent, statically binding jobs lost load balancing, and tagging entries paid on the hot path.
## Roll back cross-block state on raw fallback


A raw-block fallback must reconcile cross-block state with what the decoder received. Any reused encoder state, repcodes and reused entropy tables included, has to roll back when a block falls back to raw. Otherwise the next block's Treeless or Repeat reference names a table the decoder never received.

The rollback set is the full repcode state, not just the three offsets. A multithreaded job after the first gates repcode references until three literal-offset sequences have shipped (`gate_repcodes` counts them down in the parse). The compressed attempt of a raw-fallback block emits those sequences, so its parse drains the counter; restoring only the offset array left the gate open against a decoder that had received nothing. The next repcode emission then resolved against the decoder's own history and the frame decoded wrong, four bytes deep in an otherwise clean 457 MB stream (level 1 and level 3, 16 workers, the fast rows' far-window capture). The snapshot is [`RepcodeState`][crate::encoding::RepcodeState]: the offsets plus the counter, restored together in `compress_fastest`'s raw arm.
## Matcher scratch geometry


Avoid the `ip1_idx - anchor_idx` subtraction. The new anchor returned by emit can move past `ip1`, which is silent in release, while the debug panic interrupts an MT worker and the join waits forever. Use an exact predicate with no subtraction.

When reproducing MT job scenarios in matcher scratch, keep blocks at 128K or below and adopt only up to `block_end`. A match length above 131074 hits an unreachable path, and an `adopt_window` past `block_end` lets `extend_match` cross the block and yield a huge match length.
## Count mask bytes in hex literals


`0x0000_00ff_ffff_ffff` is three zero bytes plus five `ff` bytes, which is 40 bits and one `ffff` group more than the intended 4-byte mask `0xffff_ffff`. The r13 dictionary-width cut carried exactly this typo, so the 4-byte hash silently stayed 5-byte. Every per-knob A/B on top measured flat, a depth-only residue looked like the whole story, and a comment block full of real numbers pointed at a configuration the binary never ran.

It was diagnosed only by tracing fill and scan hashes on a known 4-byte twin `0a 23 20 74` and recomputing the hash by hand. Width and bitmask constants need a byte-count comment next to the literal, and any "knob does nothing" result needs a does-the-knob-engage probe before it is believed.
## Slice prefix probes at the gate


The reach probe checked `head.len() < PROBE_SPAN` as its engagement gate and then parsed the whole `head`. The probe that was supposed to cost a fixed 36 ms cost 580 ms on a 33 MiB frame, and json solo dropped from 76 to 41 MiB/s. Output stayed byte-correct, so the symptom reads as a feature cost. The probe was a bug.

A prefix-probe pattern must slice `&head[..SPAN]` at the gate, and its cost claim must be re-measured on a large input.
## Keep replay cursors outside loops


Staging probe bytes inside `FrameCompressor::compress`'s block loop reset `staged_read` on each iteration, so the same first block replayed until the frame hung. A 32 MiB json input was stuck for more than 10 minutes at 375 MB RSS. Replay cursors live outside the loop they gate.
## Audit domain conversions at boundaries


Rebasing `win_base` into a local while still reading `self.anchor` and `self.pos` as true-coordinate fields put a whole `shift` of error into the scan cursors, and `ip1_idx - anchor_idx` underflowed on the miss path. That is the recorded underflow class: a debug panic inside an MT worker makes the join wait forever, while release wraps silently and output stays green. One `debug_assert!(pos >= shift && anchor >= shift && block_end >= shift)` at the entry pins what the compiler cannot check.

The enabling property is worth naming. The fast and dfast loops are coordinate-pure, because every absolute derives in-function from `pos` and `win_base`, which is what let the above-4-GiB entry-truncation defect be fixed by rebasing at the parse interface with zero loop changes. The in-loop alternative of u32 wrap-domain arithmetic is falsified on register grounds, so coordinate purity is load-bearing design.
## Defeat repcode carry in test payloads


A 6 GiB probe for the above-4-GiB table collapse passed on both the fixed and the broken trees through three payload iterations. The 64 KiB periodic tiles, and then per-segment repeats with varying distances, all ride `rep[0]` at a constant per-segment distance, and repcodes survive exactly the table collapse the test means to expose.

The payload that bites is a shuffled replay of a 16-byte token vocabulary, where the match distance changes every token so no repcode can carry it, and matches of 16 bytes or less keep every position indexed. Any payload whose matches are rep-carried cannot test a hash-table path at any scale.
## LDM gear arithmetic wraps


The rolling gear hash is `(hash << 1) + GEAR_TAB[b]` over full 64-bit values. C wraps silently, while Rust's debug overflow panic fires as soon as LDM runs on large debug inputs, and release is unaffected.

The same class applies to the `fed` cursor. A shutoff gap must leave `fed` at the arm floor through `gear_rearm` semantics, or the next revival block's `debug_assert_eq!(fed, base)` fires and the canary split lands mid-match, which is a release-mode correctness bug.
## LDM empty-slot bound


`checksum_mask` selects an unwritten slot whenever the query checksum is 0, because the empty encoding also carries checksum 0, and its biased distance resolves to `split + 1`. That is inside the window reach for every split below the window size, so a plain `split - dist` underflows. Debug panics and release wraps, after which `extend_match`'s unchecked reads go out of bounds.

All-zero windows never become splits because the gear over zeros settles at `-GEAR_TAB[0]` and does not trigger, so checksum-0 windows are structured needles the fuzzer must craft. The encode_stream artifact `crash-899d2f24` fingerprints one, and its 64 bytes head a split in any buffer they start because the checkpoint after a window is prior-state independent, which is the regression test's hook. The fix uses `checked_sub`.
## asm lateouts and templates


`gear4_asm`'s template reads `h` after writing `c1` and `c2`, but `lateout` promises only that the output is written after all inputs are consumed. The compiler therefore assigned a lateout to `h`'s register, and `c3` and `c4` were computed from the clobbered value. Dev-profile split sets diverged from the serial gear while the release fuzz build allocated differently from the same source. Any split set yields valid frames, so output gates saw nothing.

The fix uses early `out` for `c1` to `c4`, and `gear_feed_records_serial_splits` locks the inlined instance to the serial split set.
## Empty holdouts skip scoring


The dictionary sweep called `evaluate` even when the train and test split produced no test samples, which happens with a single small source, and the debug assert `!test_samples.is_empty()` fired under `--all-features`. When a sweep can collapse to one candidate, the evaluation step is optional.
## SIMD mask bit domains


The row matcher's packed entry puts the tag in each u32's top byte, and the tag compare was ported as `srli_epi32` plus `cmpeq_epi8` plus `movemask_epi8`. `_mm_movemask_epi8` yields one bit per byte, and with the shift placing the tag at byte 4e of each lane, the entry's bit landed at mask bit 4e. Reading that bit index as a slot index, which is the scalar fallback's contract, searched only slots 0, 4, 8 and 12 and tag-verified only slot 0, so three quarters of every row were invisible while every surviving candidate was still byte-verified.

Correctness gates, roundtrips and MT checks all stayed green. The failure surfaced purely as a ratio and speed tuning puzzle, with text.l5 42% larger and non-monotonic from l6 to l7, and survived several calibration iterations before the mask arithmetic was re-read.

The correct lane-wide form is `cmpeq_epi32` after the shift, where upper bytes are zero on both sides, plus `_mm_movemask_ps`, which gives one bit per entry and needs no BMI2. When porting a compare idiom to a different element width, derive the mask's bit domain from first principles, and treat capacity-shaped ratio anomalies, such as levels that should be monotone in search depth but are not, as a data-structure bug before a parameters bug.
## Slot-choosing state is determinism state


Chain tables' pooled residue is sound because stale entries are only read, range-checked and byte-verified, while live entry placement is a pure function of position. A row matcher's cycling insertion head breaks that shape, because the head chooses where the next insert lands. A residual head makes this job's live-entry placement depend on the pooled state's job history, so two worker-claim orders place different candidates in different slots and the frame bytes diverge run to run.

This is the chain-link landmine's class, reached through new state instead of a missed link. The row heads clear per job with the head table in `fill_window_grid`, and the per-job clear is why the head lives in its own small array and stays out of libzstd's in-row byte 0. A 32 to 64 KiB array clears in microseconds, while in-band state would need re-deriving.
## Hashes are defined over the byte sequence


The matcher hash primitives loaded their words with the target's byte order. Every hash stayed self-consistent within one process, so frames decoded correctly, but the values differed from libzstd's `MEM_readLE*` definitions: on s390x the row matcher lost most of its matches and level 3 output ran 78% over the reference size, `c_fixture_parity` failed, and pre-split's fingerprint diverged from its recorded fixtures. The same native read also broke the min-match-3 rep gate, whose `<< 8` intends to drop the fourth byte of a little-endian read; on big-endian it dropped byte 0 instead, admitted rep candidates whose first three bytes were never verified, and emitted matches that decoded to single wrong bytes at template repeat boundaries.

The fix defines every hash input load, the rep gate and the fingerprint hash over little-endian words on every target, matching libzstd. Match decisions then match the reference and match the other architectures, which turns the byte-identical cross-target dump into a free parity gate: dump the same corpus on two endiannesses and `diff` the frames. A hash that only needs to agree with itself may use any byte order; a hash calibrated against the reference or against fixtures must agree with the reference's definition.
