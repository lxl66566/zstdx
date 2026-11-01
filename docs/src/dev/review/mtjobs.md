# MT job machinery review

R85 Track B reviewed the multithreaded scheduling and job-state machinery after R80-R84: the bulk worker loop and job claim in `encoding/mt.rs`, the streaming core, accumulate buffer and its pool in `stream/encoder_mt.rs`, the leased thread pool in `stream/mt_pool.rs`, the `job_trace` counters, and the job-state handoff through `encoding/frame_compressor.rs`. The review was read-only against 18b77c19. Finding 1 was reproduced with a scratch test that was reverted after the run, so the tree carries no code change from this round.

## Findings

### 1. The donated prefix can exceed job zero's span on the growing tail re-slice

Severity: critical. Verified live. Fixed in R87 as todo 28 (see the repair note below).

The contract at risk: `compress_job_blocks_inner` takes a `start_cursor` (the donated span, `PROBE_SPAN` = 2 MiB) plus the span's encoded blocks as `prefix`, emits the prefix verbatim and parses from `max(start_cursor, job.start)` to `job.end` (frame_compressor.rs:927-1010). The prefix encodes `[0, PROBE_SPAN)` of the stream, so job zero must end at or past `PROBE_SPAN` whenever it carries a donation. The entry asserts only the lower side, `start_cursor >= job.start` (frame_compressor.rs:998), and the upper side is stated nowhere.

The guard that exists covers one corner. `post_job` discards the donation when `last_frame_block && end <= PROBE_SPAN` (encoder_mt.rs:2669-2674), which handles a final job that stops at the span. The growing grid's tail re-slice can cut job zero below the span without being last: `build_bounds` computes `target = size.min(tail.div_ceil(burst_jobs).max(MIN_JOB_SIZE))` (encoder_mt.rs:2260-2266), and `MIN_JOB_SIZE` is 1 MiB against the span's 2 MiB. The re-slice always produces at least two jobs, because `target` stays below `tail` for every `burst_jobs >= 2`, so job zero is never the last job there and the discard can never fire.

The exposure window: an open balanced stream (unpledged and hint-less) stages the probe at `PROBE_MIN_FRAME` = 8 MiB, a narrow head posts the donation, a Keep verdict parks it (encoder_mt.rs:1727-1763), and the finish or flush tail re-slices from offset zero. The first job lands below the span whenever `tail.div_ceil(burst_jobs) < PROBE_SPAN`, which needs `burst_jobs >= 5`, so every worker count from five up with a total in `[8 MiB, 2 * burst_jobs MiB)` is exposed. At eight workers the band is 8 to 16 MiB. Job zero then emits the donated 2 MiB prefix while job one parses the overlap again, and the assembled frame encodes `[target, PROBE_SPAN)` twice.

Verified live with a scratch test at balanced, workers 8, and an 8 MiB input built from a 16-symbol unit repeated at a 1 MiB lag (narrow head, Keep verdict). The frame fails libzstd's checksum check, while the same input at 128 KiB below the staging gate round-trips byte-identical through both decoders. A mid-stream flush inside the band hits the same re-slice and corrupts the rest of the frame permanently.

Repair (R87, todo 28): `post_job`'s discard dropped the `last_frame_block` conjunct, so any job zero ending at or below the span parses undonated, byte-identical to the stock schedule, and the grid stays a pure function of the offsets. The belt is `debug_assert!(start_cursor <= job.end)` at the continuation entry in `compress_job_blocks_inner`, which covers the bulk and stream donation sites alike. `donated_span_survives_the_growing_tail_reslice` (encoder_mt.rs) pins the corner with this round's fixture at eight workers across the band edges: on the unfixed tree the below-gate control round-tripped while the 8 MiB cell failed decode with `TargetTooSmall`, the duplicated band pushing the frame past the input length, and the suite read green after the repair. The 150-cell dump gate (bulk and stream, all levels) stayed byte-identical.

### 2. The donation span bound lives in arithmetic spread over three constants

Severity: high. Verified in code that no check exists.

The bulk path's eligibility gate checks the stock formula, `job_size_for(len, workers, keep_strip) >= PROBE_SPAN` (mt.rs:587-592), while the frame may run the pinned capture grid, `keep_job_size` (mt.rs:603-616). The two agree with the span bound today through three independent facts: the probe's only row floors the stock formula at `KEEP_REACH` = 4 MiB, the capture band forces `window >= 2^25` and `len >= window/2` so `len/8 >= 2 MiB`, and `PROBE_SPAN` is exactly 2 MiB. The stream's fixed grid relies on a fourth fact, that the Keep overlap is the whole window and therefore at least 8 MiB under the pledge gate. `donate_job_zero_prefix` slices `src[..PROBE_SPAN]` (mt.rs:883) and the continuation relies on `keep_job_size >= PROBE_SPAN`, with neither conclusion asserted at a donation site.

Repair scope: one `debug_assert` per donation consumption site, `job zero end >= PROBE_SPAN` in the bulk worker loop and in `post_job`, plus a comment tying the three constants together. This is the belt that keeps finding 1 fixed and keeps a geometry change from reopening it.

### 3. A reused encoder after a surfaced poison trips the fail-safe panic

Severity: medium. Verified in code.

`surface_poison` takes the payload under the queue lock and never clears the `poisoned` flag (encoder_mt.rs:2947-2975). The method's doc table calls the flag-set-without-payload arm unreachable because the posting thread resumes before any re-entry. A caller that catches the panic and calls `write` or `finish` again on the same encoder violates that assumption, reaches the arm, and panics with the invariant message. The behavior fails fast and cannot corrupt a frame, but the message reports an invariant breach where the actual state is an already-consumed poison.

Repair scope: clear the flag when taking the payload and turn the arm into a plain "encoder poisoned" panic, or leave a marker payload behind. A test that catches the first panic and drives one more call pins it.

### 4. A below-gate flush reserves the cap for a canceled schedule

Severity: low. Verified in code.

`settle_probe` clears `probe_pending` without staging and calls `reserve_decided` on a non-finishing boundary (encoder_mt.rs:1679-1687). On an open stream the growing-grid arm reserves `BUF_CAP_MAX` (encoder_mt.rs:2006-2032), so a flush below the 8 MiB gate reserves 256 MiB of capacity for a schedule that was canceled rather than decided. The organic path would grow lazily through `make_space`. The cost is address space plus a cap-scale buffer retained per thread at `BUF_POOL_KEEP_MAX`, which is consistent with the open-stream policy and invisible in resident memory. Repair scope: gate the re-reserve on a verdict or an engagement having actually resolved, or accept it and record the corner here.

### 5. job_trace notes

Severity: low. All verified in code.

`snapshot()` loads every counter independently under Relaxed (job_trace.rs:120-131), so a harvest racing active jobs can mix totals from different spans. `jobdecomp` resets between iterations, which makes the bench the intended consumer, and a racing harvest is a dev-tool artifact rather than a library defect. The bulk timeline prints absolute `[start, end)` claim lines (job_trace.rs:331-360) while the stream's `TL CLAIM` prints view-relative `(first, len)` pairs (encoder_mt.rs:908-936), both documented at their sites, so a cross-path consumer must know which domain it reads. `add_cmp` is a per-compare atomic on the hot path under the feature, and R84's compare-volume claims rest on it, which is the intended cost of the dev build. Feature hygiene holds: the module and every call site are cfg-gated, the feature pulls `std`, and nothing reaches default builds.

## Verified in code

Each invariant below is enforced by a check, a type or a lock protocol in the tree, with the enforcing site named.

- Wide-head stand-down, bulk: the Keep plan is computed before the verdict, a wide head returns with the plan and no donation, and every job posts at once (mt.rs:623-642). Pinned by `head_predicts_keep_splits_on_the_sampled_alphabet`, `wide_head_prediction_matches_the_probe_verdict` and `wide_head_bulk_mt_is_worker_invariant` (mt.rs:1727-1810).
- Wide-head stand-down, stream: `stage_probe` resolves Keep outright, runs the mid-size engagement and the decided reserve inline, and `probe_wait` never exists (encoder_mt.rs:1584-1604). Pinned by `wide_head_stages_keep_without_donation`, including the narrow-head twin at the same mid-stream point.
- `mt_job_size_for` mirrors the prediction so the analysis tools cut at the grid the encoder runs (mt.rs:81-108).
- Poison publish ordering: payload under the queue lock, then the flag, then the abort, with the flag-without-payload arm failing fast (encoder_mt.rs:652-666). Pinned by three tests at encoder_mt.rs:3577, 3620 and 3640.
- Bulk panic containment: `catch_unwind` per job (mt.rs:383-392), the Err path stores the poison, fills the slot and notifies (mt.rs:438-447), the ordered assembly empties unclaimed slots under poison so the drain completes (mt.rs:497-508), and the unwind resumes after the scope (mt.rs:517-519). Lock order is one-sided, share and slot before poison, at every nesting site (mt.rs:316, 362, 501, with the contract in the `wait_prefix_build` doc at 1469-1477).
- Quiesce-gated recycling: `make_space` wraps or grows only at `n_incomplete == 0` (encoder_mt.rs:2856-2891), donation tasks never count (encoder_mt.rs:1606-1676), and `serve_queue` decrements only for encode jobs (encoder_mt.rs:734-736).
- Wait loops re-check their predicates on every wake with a bounded timeout, the historical lost-wakeup fix: `wait_progress` (encoder_mt.rs:2776-2806) and `wait_donation_inner` (encoder_mt.rs:2816-2836).
- `FrozenSrc` views: `freeze` pins the span inside the live extent under debug assertions, and the unsafe `Send`/`Sync` impls carry the quiescence contract (encoder_mt.rs:494-521).
- Worker lifecycle on panic: the panicked state is dropped rather than pooled (encoder_mt.rs:732-741), the thread retires and never leases again, the slot withdraws from the pool under the slot lock (mt_pool.rs:143-186), and `Drop` waits every lease out with the recycled-slot pointer check (mt_pool.rs:135-146, encoder_mt.rs:2984-3005). Pinned by `pool_drop_midstream` and `pool_lease_reuse_deterministic`.
- Pledge contracts: the write gate fails the offending write (encoder_mt.rs:1319-1333), the pump gate fails the read that carried the overrun (encoder_mt.rs:1367-1428), and finish catches the under-run before the closing block (encoder_mt.rs:1430-1437). Pinned by `pledged_mismatch_fails_write_and_finish` and the stream suites.
- The accumulate buffer's ever-initialized extent: the pool carries it as the returned length, growth clamps it to the live bytes, and the direct pump hands out only previously initialized spare (encoder_mt.rs:586-604, 2006-2032, 2856-2891, 2984-3005).
- The capture predicate is canonical: `capture_band` is the one enum-typed decision behind the bulk grid, the plan, the fast-row schedule, the pledged engagement and the stream flips (mt.rs:1183-1251). Pinned by `midsize_capture_window_boundaries` and `capture_schedule_survives_absurd_declared_lens`.
- Donation and capture arming pair: the stream asserts the flip against the donated arming at verdict time (encoder_mt.rs:1775-1782) and the bulk path asserts the pairing at plan time (mt.rs:265). Pinned by `donated_job_zero_resolves_intra_job_far_twin` and `donation_arming_pairs_with_the_capture_plan`.
- `LdmMode` threads into every state the frame runs, jobs, donations, the dictionary seed and both shared-prefix builds, with `Disable` standing the lattice down inside the canonical predicate (mt.rs:1225-1227 and both files' state-creation sites).
- Grid purity: job boundaries derive from absolute offsets alone, the flush re-grid clamp realigns on the epoch floor with its assert (encoder_mt.rs:2267-2269), and the capture lattice is a pure function of the offset (encoder_mt.rs:2048-2071). Pinned by `growing_grid_roundtrip`, `open_capture_arms_deterministically` and the flush-edge tests.
- Shared-prefix adoption domain: `SpfPlan::pick` bounds adoption to whole-prefix strips (encoder_mt.rs:306-315), pinned by `spf_finish_tail_adoption_is_pinned`.
- A failed job poisons no later frame: panicked states drop instead of pooling, retired threads never lease again, and the dictionary and donation kits only ever receive states that completed their runs (mt.rs:781-901, encoder_mt.rs:732-745). Pinned by `pooled_bulk_mt_matches_fresh` and `dict_mt_leaves_plain_mt_bytes_untouched`.

## Documented only

Each item below is sound by reading but enforced by prose or by arithmetic rather than by a check in the tree.

- Finding 2's three-constant agreement (the stock formula's `KEEP_REACH` floor, the capture band's `len/8`, and `PROBE_SPAN`'s 2 MiB) stays arithmetic-only at the donation sites; the R87 repair's continuation-entry assert is the backstop that fires first if a geometry change breaks it.
- `resolve_probe` reads `buf[..PROBE_SPAN]` at verdict time (encoder_mt.rs:1727-1763). The read is safe because `job_start` stays 0 while the verdict is unconsumed, so `movable_lo` saturates to 0 and no wrap can run, and growth preserves the head bytes. The argument is control flow plus the staging asserts at post time (encoder_mt.rs:1606-1610), with no assert at the resolve site.
- The owned window's committed-length to staged-end carry zone: the block loop stages a whole block tail and leaves the uncommitted remainder physically at the window head, where the pre-split detector reads it (frame_compressor.rs:1231-1282). [hardening](hardening.md) records it as caller-live, with the wall waiting on a caller-contract change.
- The buffer pool's keep cap: `BUF_POOL_KEEP_MAX = BUF_CAP_MAX` rests on the measured drop-time re-fault argument (encoder_mt.rs:534-540), with no test retaining a cap-scale buffer across encoders.
- The wide-head prediction equalling the probe's measured verdict on every measured class: the pin covers one fixture class plus synthetic heads (mt.rs:1745-1760), the rest is bench evidence recorded in [mt-stream](../perf/mt-stream.md).
- The growing grid's strip estimate uses `overlap` rather than `strip_span` so the grid stays a pure function of offset before the far-class verdict (encoder_mt.rs:2076-2082). The property is covered indirectly by the grid-purity tests, the reasoning lives in the comment.

## Test gaps

- The bulk poison paths have no test: the Err-path slot fill, the assembly's empty-the-unclaimed branch and the post-scope resume. The stream core carries three poison tests, the bulk core none.
- The buffer pool's retention behavior has no test.
- `stage_probe`'s decided reserve at staging has no coverage: `huge_pledge_caps_initial_reserve` pins the constructor and an empty finish, since nothing is fed and the staging gate never fires, and it does not control the head class.

## Impact on the planned follow-ups

The deep-tier job-geometry work is blocked by finding 1: any change that re-slices jobs multiplies the short-job-zero class the donation contract breaks on, so the repair and the finding 2 assert belong in that round's base. `growing_job_cap` ties the job ceiling to `(BUF_CAP_MAX - overlap) / burst_jobs` and is pinned by `growing_grid_caps_job_size`, so a geometry change that moves burst semantics must re-derive the cap with it.

The ultra fill-overlap prefill pipeline meets its constraint surface in the queue's counting. Encode jobs hold buffer views and gate recycling through `n_incomplete`, donation tasks own their bytes and never gate it, and a prefill task that both holds a view and must overlap the pump needs a third counting class, otherwise the pump serializes behind the prefill exactly as the falsified fast-row LDM prefix chain did. The per-transfer second live worker state has its substrate in the kit pools, `DONATION_KIT` and `DICT_KIT` (mt.rs:781-901), whose depth-one and depth-four shapes are the precedent to copy.

## What was checked and held

The bulk claim loop, the ordered assembly and the checksum overlap were checked against the poisoning protocol and hold. The no-build capture paths, the `SelfFill` split and `FastCapture::Engaged`, were checked for wait-freedom and worker invariance and hold. The shared prefix fill's publication gate, its far-class prediction and the adoption exactness were checked and hold. The stream pump boundary, `write`, `pump_direct`, `finish`, `flush_block` and both output drains, was checked against the retry and pledged-size contracts and holds. Duplicated state was hunted: the poison flag mirror, the three pending flags and the `JobSpfStream` to `JobSpf` bridge each carry an ordering or bridging story, and no dead field was found.
