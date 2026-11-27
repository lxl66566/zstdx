# Checksums and streaming
## Snapshots must capture filled state


`build_spf`'s first publication could fire at `upto == 0`, before any `strip_fill_segment` had run. The builder's opening `prefill_window(&[], 0)` only restarts the LDM state — `stale = true`, no fill, table content still the pooled predecessor's — and `LdmState::snapshot` clones that table wholesale. An adopter's `restore` then swaps the stale content in and clears the `stale` flag, so the job's LDM table carried another frame's entries with no clear ever running. The debug assert on `snapshot` caught it once a far-alive regression test produced a finish tail whose first prefix job started inside the publication lookahead; release builds ran the same path silently, and whether the stale entries died by the offset filters was an accident of content.

The rule: a snapshot is only legal over a state that has actually filled past the point it claims. Publication now requires `upto > 0`, which is also the honest schedule — a zero-point snapshot shares no filled span, and the job that would have adopted it takes the stock cold fill, whose restart clear is the only provably residue-free base.
## Single-producer rings


"Single producer" means single thread, not single frame. Nested and overlapping frames, from reentrant compression or two-frame same-thread tests, interleave into the same ring. Any scheme that caches the next sequence starting point at frame creation lets two live frames claim the same sequence and overwrite each other, and the inner reset gets clobbered, the worker panics out of bounds, and the main thread spins forever in `FinishCell` while pegging all cores.

Each post must read the sequence fresh from the shared head and claim it, which is non-reentrant within a single thread, and the producer keeps only the drain threshold locally.
## Worker panics and ordering


After a worker panics, every main-thread wait becomes a permanent spin pegging all cores. When investigating, first look for worker panic output under `--test-threads=1 --nocapture`, then kill the process.

Across the ring, the producer orders the slot payload Relaxed, then kind Release, then head Release. The consumer orders head Acquire, kind Acquire, payload Relaxed. Ring wraparound is guarded by `wait_free_slot`, which is independent of frame identity and naturally supports interleaving.

- A streaming frame's fill cannot know EOF in advance. When streaming fills the tail it must do a second read that triggers 0, and it emits one extra 3-byte raw last block. The slice path must explicitly mirror `trailing_empty` for bit-identical output.
## Drain partial writes


`write_output_to` used `write_all` plus an unconditional `output.clear()`, so a writer that half-accepted a buffer and then errored, through `WouldBlock` or `WriteZero` on a socket or pipe, silently dropped every undelivered encoded byte. The caller's retry continued from a shifted position and the frame shipped with a missing stretch, which surfaced only as a decode failure far away.

The fix is an incremental `write` loop that advances the `out_read` cursor, which `split_output` already had the right shape for, and clears only after the drain completes. Both cores share `encoder_core::write_pending_output`, and the `Ok(0)` verdict mirrors each build's `write_all`: `WriteZero` with std and `WriteAllEof` in no_std.

`write::Encoder::write` still stages its input before the drain, so on `Err` the input may already be accepted. A caller retrying under the io::Write "Err means nothing written" contract would duplicate data. Closing that gap needs input deferral at one copy per write on the hot path, and until a caller needs retry-same-buffer semantics the divergence is documented at the implementation.

The mt queue's `out_bytes` counter had the same class one layer up: it deducted per completed segment, not per delivered byte, so a drain that failed after an `Ok(n)` prefix left `pending_output` over-reporting by n. Any pending-bytes counter must be derived from the delivery cursor on both arms of the drain, the way `split_output` always did.
## Read-side finish and pending output


`read::Encoder::finish` ran `core.finish()`, which emitted the last block and checksum into `core.output`, and then returned the inner reader, dropping `output` with it. The caller had already read only a prefix without the closing block, so the frame was truncated, and the read side has no later drain that could deliver the tail.

`finish` now fails with `Error::UnreadOutput { bytes }` unless the encoder was read to end of stream first, where the final read's `Ok(0)` both finishes the core through the pump and drains the output. This is the read-side twin of the drain pitfall. A `finish` that hands back the wrapped resource can only succeed when nothing is in flight, so the pending state must be an explicit, countable error rather than a documented loss.
## Bound the write_all reserve


The mt stream write path handed `make_space` the entire remaining input as its growth target once the buffer filled, so a single `write_all` of a multi-hundred-MiB slice reserved exactly that much. `BUF_CAP_MAX`'s "past the cap the wrap alone recycles" bound never engaged on this path, while the read-side `pump_direct` was bounded per pull and unaffected.

The full-buffer write step now asks for one bounded increment through `PUMP_STEP_MAX`, the same bound as a direct read pull, and the loop refills and recycles. Each step's posts keep completed jobs widening the wrappable dead prefix, so capped steps progress instead of spinning, because a bounded want of at least 1 at a full buffer always wraps or grows. The proactive doubling ladder below the cap is untouched, so steady-state buffer sizes and frame bytes do not change, since the job grid is a pure function of stream offsets.
## Pledged sizes are contracts


Both stream cores wrote `options.pledged_size` straight into the frame header and never compared it with what was fed. A stream pledged 10 bytes but fed 20, or pledged 100 and fed 20, produced a normal-looking frame declaring FCS=n over content not equal to n, which every decoder rejects. Libzstd checks the pledge mid-compress and at the frame tail, and the CLI pledges the stat size and then copies, so a file changing under it silently shipped a broken frame.

The fix follows libzstd's split. The first write whose bytes push the fed total past the pledge fails that write itself with `srcSize_wrong`, nothing staged and deterministic, and both core finishes catch any residual mismatch before emitting the closing block. Finish cannot promise that no frame byte was emitted, because `write::Encoder::write` drains eagerly and the sink may already hold the header and completed blocks.

The surfaced failure is a truncated, unclosed frame, which is the expected outcome. The first version of the fix claimed byte-level prevention, which must not be re-claimed. Anything written into a frame header as fact must be verified at the earliest boundary that can observe the lie, which is the write that crosses it, not only the finish that closes it.

The read-side core held the mirror-image bug for hints. `FrameCompressor::compress` ended the frame once the fed total reached `InputShape::len`, so a hint below the real input silently dropped the tail while the header declared nothing. The hint now sizes the row only. The frame ends at EOF, and a grid-exact source closes with the 3-byte empty last block like the unshaped Read path always did. Only a pledge may close a frame early.
## Caller size hints fold with saturation


`InputShape::len` is caller data, so every fold that adds a real length to it (the dictionary content length, on six sites across the bulk slice, the MT job seeding and both stream cores) must use `saturating_add`. The plain `+` panicked under overflow checks on a hint of u64::MAX, and u64::MAX - 8 + dict_len wrapped to a few KB in release, clamping the window to ~1 KiB while the frame still decoded. Saturated, the absurd hint keeps the unknown/huge shape: the same window a merely-large hint (1 << 40) gets on the same path. Folds over real lengths (a source span plus the dictionary) cannot overflow and stay plain.
## Windowed one-byte FCS class


Flag 0 means the Frame_Content_Size field is absent unless the single-segment flag is set, but the header serializer mapped every width-1 size from 0 to 255 to flag 0 and appended the 1-byte field anyway. Any pledged stream or bulk-mt input below 256 bytes, and the CLI pledges every non-empty file's stat size, serialized a stray byte that shifted the first block header. Both this crate's decoder and libzstd rejected the frame as an incomplete frame or data corruption. All boundary classes at 256 and above worked, so every ordinary roundtrip test passed.

`FrameHeader::serialize` now normalizes: a windowed frame below 256 bytes drops the declaration, the pledge keeps shaping the encode, and only the header promise is lost. Descriptor-flag tables need a test per flag class crossed with the flags that redefine them, because `single_segment` redefines flag 0.
## Poison flag ordering


The stream-mt worker's panic path aborted its job with an empty slot, then flipped the `poisoned` mirror flag, then stored the payload under the queue lock. That is a two-step publish, and the posting thread could observe flag-set and payload-absent in between. `surface_poison` surfaced nothing, `drain_all` assembled the aborted job's empty slot like any completion and returned a frame missing the poisoned job's bytes as `Ok`, and the payload was stranded in the worker. The bulk path is immune because it stores the payload under its mutex first and then the slot.

`QueueShared::publish_poison` now orders payload, then flag, then abort, and the abort must stay after the flag or the assemblable-empty job drains ahead of the payload it implies. The drain's "pending empty, surface, return" arm is safe only because a set flag now guarantees `inner.poison` holds the payload. `surface_poison`'s flag-set and payload-absent arm is unreachable and now panics instead of returning, because a silent return would re-ship the exact corruption the ordering fixed. A fast-path mirror flag of mutex-guarded state is a second publication and needs its own happens-before story.
## One condvar pairs with one mutex


The bulk-mt pool shared a single `ready` condvar across four waiter classes: the ordered assembly waiting on each job slot's own mutex (N mutexes by that class alone), the LDM feed gate, the whole-strip prefix build's share and the windowed chain's share. POSIX requires one condvar to be waited on with exactly one mutex. glibc tracks no mutex identity, so Linux ran this shape for its whole life, while macOS pthread detects the second mutex and aborts the process from inside `Condvar::wait`. The abort then poisoned a mutex on the unwinding path and the encode died at a `PoisonError` unwrap in the slot fill, so the CI read as a checksum-suite failure with an unrelated-looking cascade (the decode-side twin of the same shape is under MT decoding below).

The rule is structural: every waited mutex carries its own condvar, paired in `common::monitor::Monitor` with `lock`/`wait`/`notify_all` as the only entry points, so a bare shared condvar cannot come back. The wake discipline moves with the split. A notify targets the monitor whose guarded state changed, and a poison publish broadcasts to every monitor, because each waiter class re-checks the poison between its own waits and would otherwise sleep through the drain. The slot fill notify stays on the filled slot's monitor alone: the assembly is the single waiter there, and the other classes' predicates are untouched by a completion, which is exactly what the old shared broadcast's extra wakes resolved to.
## Refuse partially honored knobs


`prepare_job_state` armed the deep-offset ramp from `ZSTDX_MT_RAMP_BYTES` on every MT job, but only the fast, dfast and chain parses read the gate. The btlazy2 rows 13-15 and the opt rows 16 and above silently produced unconstrained frames under the environment variable, so a piece-decode experiment on those levels measured nothing. The frames are legal, but their crossing reads have unbounded shallowness, which fails the piece executor's validation, or on shapes whose reads happen to run deep slips past it and fakes engagement.

`MatchGeneratorDriver::arm_ramp` now panics on `BtLazy` and `Opt`. The tree rows' wiring points are enumerated in its doc comment: `dubt::find_best`'s rep loop and sorted descent, `btlazy::run_block_lazy`'s two rep probes, the literal-offset backward extension floor, the LDM far-candidate check, and the opt DP walk. The panic must be unconditional rather than a `debug_assert`, because release is where the experiment runs.
## Arm donated spans as job zero


The reach probe's donation hard-armed `LdmArming::Job`, but a Keep verdict can re-grid the frame onto the mid-size prefix-LDM capture through bulk `plan_prefix_ldm` or stream `engage_midsize_capture`, whose jobs arm `JobPrefix`. The capture's source-clamped window in the W25 band sits below the `Job` bar, so the donated state's LDM never armed while jobs 1 and above armed. Job zero lost the far candidates inside its own span and the frame diverged from the undonated schedule: a 32 MiB tiled row-9 frame at w2 measured 7.94 MB against 4.76 MB, which is 67% larger. The divergence is invisible at w4 and above because a job at the W22 strip floor has no beyond-reach distances.

The arming is decidable before the verdict, because the capture engages exactly when a Keep verdict would run it, so the donation gate computes the capture plan first and arms the donation with its arming. The Shrink verdict pays the span's split pass for nothing, which is the band's price, and debug asserts pair the frame's capture with the donation's arming on both paths. A contract that says the donated span is job zero's parse must arm the donation from the job's final schedule, never from the pre-verdict default.
## A donated span must end inside job zero


The reach probe's donated prefix encodes the first `PROBE_SPAN` bytes as job zero's own blocks, so every schedule that hands job zero to a worker must keep the job's end past the span. The growing grid's tail re-slice floors its jobs at `MIN_JOB_SIZE`, half the span, and the continuation entry asserted the contract's lower side alone (`start_cursor >= job.start`). An open balanced stream at five workers or more with a total in `[PROBE_MIN_FRAME, 2 x burst_jobs MiB)` then shipped a frame with the band between job one and the span encoded twice, and the checksum rejected it.

The repair widened the posting-side gate, `post_job`'s discard, to any job zero ending at or below the span, and asserted the upper side at the continuation entry. A donated-prefix contract has one consumer-side entry and one posting-side gate: wall both, because every path that re-slices job zero is a new way to move its end under the span.
## Pin the donation post, not its parked wait


The wide-head stand-down test asserted `probe_wait.is_some()` right after the staging write. `post_ready`'s loop polls the two donated parses for a landed verdict before every posting decision, and a landed verdict is consumed for free there. When the posting thread stalls past the parses' completion, the write itself resolves the probe and the wait is gone at the assert. The window is the poll's own span in code and stretches to the parses' runtime under a loaded scheduler. The paranoid suite hit it once across many runs, while every isolated and locally loaded rerun stayed green.

The pinned fact is the post, which leaves two deterministic traces: the parked wait, or the donated carry a Keep verdict installs in `resolve_probe`. The assertion now reads the pair, the same one `donated_span_survives_the_growing_tail_reslice` already reads. An assertion over scheduler-dependent intermediate state fails as a flake while the code is correct, so the test pins the state's cause.
## Initial reserve is a scheduling scale


The mt stream's initial and probe-settle reserves sized to `workers * initial_job + overlap`. With the CLI always pledging the file size, a 16 GiB file at `-T8` with the job size at the `MAX_JOB_SIZE` 1 GiB ceiling reserved about 8 GiB up front and held that much input resident for the whole stream, because the pump recycles only at a full buffer.

Both reserves are now capped at `BUF_CAP_MAX`, the same ceiling the unpledged growth ladder uses. The pump grows into the live need through `make_space`, whose wrap recycles at grid lines where quiescence is reachable, and `make_space`'s growth arm still exceeds the cap when the live set demands it, such as a pledged grid's held-back final job. One floor remains: the fixed grid's first job cannot post until a full job-size is buffered, so the buffer still grows to about one job plus the strip mid-stream, which is the grid's own semantics rather than the reserve.
## Growing-grid epochs stay bounded


`post_due` holds a whole epoch of `burst_jobs` jobs plus the next job's strip before anything posts, so the accumulate buffer's live span is `strip + burst_jobs * job_size`. The doubling ladder's `MAX_JOB_SIZE` 1 GiB ceiling let that scale with the worker count toward workers times 1 GiB on long unpledged streams, which is the CLI stdin path. VmHWM measured 1.4 GiB at w8 with 3 GiB L3, 1.5 GiB at w2 with 3 GiB, and 1.6 GiB at L9 w8 with 2 GiB, which is OOM scale on small machines.

`growing_job_cap` now ties the Growing grid's job-size ceiling to `(BUF_CAP_MAX - overlap) / burst_jobs` and clamps the first epoch's `overlap` floor too, so the staged epoch stays at the buffer's own scale for any stream length. L3 w8 reads 361 MiB VmHWM at 1 GiB and 3 GiB alike, and L9 w8 reads 552 MiB, which is the capped buffer plus the level's fixed per-worker tables. The strip estimate is `overlap` and never `strip_span`, because the far-class screen resolves only at the first post and the grid must stay a pure function of offset.

The costs are that jobs below the whole-window strip pay strip-fill per job, measured at 7.5% slower wall for L9 w8 on a 2 GiB unpledged stream and 12% to 25% faster for L3 because smaller jobs pipeline better, and that long unpledged streams' frame bytes move to a denser job grid. The 32 MiB bench corpus is byte-identical, with zero deltas across the full 120-cell ratio A/B. Reclaiming the L9 redundancy wants the finish tail's shared prefix fill extended to streaming posts. An epoch-staged posting gate makes the job-size ceiling a memory bound, and a ceiling shared with the bulk formula is not one for the streaming grid.
## Arming bars read the clamped window


`ldm_min_window`'s `LdmArming::Job` arm demanded the row's full 64 MiB window, a bar written against the row parameter. The bulk MT path declares the input length, and `adjust_params` clamps the window to the next power of two of the source, so every deep-row job on a [32,64) MiB source evaluated the bar against 32 MiB and disarmed LDM, while the same row's ST frame passes an unknown length, keeps the 64 MiB window and arms at the 32 MiB midsize bar. dll32 best bulk-mt8 paid +25.2% size with the DUBT tree carrying the whole far class at 458 GiB of compares, and the defect hid behind the twin-whale attribution for a round because the grid sweep's single-job cell, not any per-job span, was what isolated it.

Any bar keyed on `params.window` must be evaluated against the window the caller's shape actually produced, and a job-only bar stricter than its frame's needs a source-clamp audit. The isolation pattern generalizes: when a defect is attributed to the job split, run the one-job cell first, because it removes every boundary at once and costs one jobdecomp invocation.
## LDM limitTableUpdate floor


The btlazy gap-parse set `next_update = next - 512` unconditionally once an LDM emission cleared the 1024 bar, but libzstd's `ZSTD_ldm_limitTableUpdate` also caps the backstep at `curr - nextToUpdate - 1024` through `MIN(512, ...)`, so the update point never lands below `next_update + 1024`. The floor raises the update point, which makes the tree fill skip more, and does not add insertions. Our bare `-512` indexed up to 512 extra positions whenever the emission only just cleared the bar, with a gap in the 1024 to 1536 range.

The aligned form is `backstep = 512.min(next - next_update - 1024)`, and the guard makes the subtraction positive, so u64 needs no C-underflow care. Measured dll100 at L13 moves the frame by 287 B on 19.59 MB, which is +0.0015% and noise. The 32 MiB ratio corpus never hits the band, with a byte-identical 120-cell A/B and geo-mean +8.67% on both sides, and wall is unchanged at dll100 L13 over 3 iterations, 10.684 against 10.654 s. The opt family is deliberately untouched, because C's btopt and above consume LDM as candidates with no `limitTableUpdate`, and `opt.rs`'s `match_end - 8` is the tree search's own interior skip.
## Price the real serialization in selection


The sequence-table selection priced a fresh table's description as a flat `(max_symbol+1)*6+8` bits while running the fast-row heuristic thresholds everywhere. Libzstd switches `ZSTD_selectEncodingType` arms by strategy, and everything at or above `ZSTD_lazy` compares predefined, repeat and fresh on exact integer costs with the real NCount bytes.

On a small LL alphabet the flat estimate charged about 12 B where the acc-5 NCount serializes to 5 B, so the compare leaned predefined on exactly the small-histogram blocks a dictionary frame is made of. That cost 17 B of bitstream per flipped block and affected 30 to 35 of 54 holdout files.

The fingerprint for this class is entropy bounds in our favor while the frame loses bytes, with a section diff showing same-mode bitstreams differing. That means the leak is in the decision, not the stream. Any cost read off a pooled table, including probs or tt tails past `nsym`, must bound the alphabet first, because the stale tail reads as plausible costs and can return a repeat verdict for a table that does not cover the histogram.
