# R95 encode sprint review

Scope: the R93-R95 encode-side and streaming landings since 2026-10-31 — the serial LDM feed (`432d1b5a`), the strip admission stride (`1bad5a30`), the unpledged deep band (`5d001241`), the widened feed window (`7a275c84`), the long-compare strip crossing (`a7f6efac`), the stepped finish drain (`2a8597b0`), the deep-band pins (`900d2d96`), the typed-position refactors (`45ca65a4`, `6798a254`, `c707a272`) and the fast-scan capture hook (`c2e2eff4`). Decode-side commits of the same window belong to another track. This was a quality round: two hygiene fixes landed (see finding 3 and 4), everything else is recorded here for a later gate.

## Method

Each commit was read against the HEAD state, its perf and negative doc sections were checked against the code, and the job-geometry arithmetic behind the arm guards was re-derived from the ladder (`params.rs`) and the job planner (`mt.rs`). The two fixes were gated with the corpus dump ladders (bulk and stream, all levels, diff-empty against the pre-change build) and the zstdx suites.

## Findings

### 1. Medium — the split sweep's window-floor guard disarms the long compare on the late ultra jobs

`run_once`'s arm (the guard at `opt.rs:1799`, above the `update_tree_strip` call) requires `strip_from >= (win_base + block_end).saturating_sub(max_window)`. `block_end` is an absolute position and the floor of an absolute fill target is `target - max_window`, so the `win_base +` term mixes the window-base coordinate into an absolute one and makes the guard stricter than its proof needs. The byte-exactness condition is that the cursor sits at or above the floor of the parse's first search target, which is `block_start - max_window`: a strip job's first search targets exactly `block_start`, and the split sweep then starts where the stock first fill would.

The widened adoption's ultra jobs hold that condition with equality, because the seed rewind pins their cursor at `win_base` and `win_base = block_start - window`. The landed guard instead reduces to `block_end <= max_window` for every ultra strip job, so the jobs whose parse start reaches one window from the frame start run their crossings on the scalar `count_from`. On dll100 ultra (window 2^26, 8 MiB lattice, 12 strip jobs) that is jobs 8-12, exactly the ones whose strips span the full window and pay the widest far-tier crossings. Opt rows hold the arm on the measured shapes: their cursor sits `chain_reach/2` above `win_base`, a 60 MiB margin over the intended floor that the guard's extra `win_base` term stays under on every corpus job. dll32 and the 32 MiB corpus arm everywhere.

Bytes are unaffected either way, since the split sweep is byte-identical to the stock crossing by construction; the loss is silent. Two doc claims overstate the kernel's reach as landed: `perf/matchers.md`'s R95 section names the widened window class as the kernel's buyer, and `perf/mt-stream.md`'s R95 section lists the long-compare kernel as one of the two levers the widened adoption rests on. The far window is indexed either way; on the disarmed jobs the insert descents pay the scalar compare.

The fix is a behavior-affecting change on the hottest path (compare-form selection, a wall question rather than a byte question) and needs its own round: compare against `block_start.saturating_sub(max_window)`, then gate with the dump ladders plus an interleaved Ir and wall A/B on dll100 ultra.

### 2. Low — `run_ldm_feed`'s panic handling is open to a lock-held panic

A panic between `gate.lock()` and `drop(g)` inside the feed's generation loop poisons the gate mutex. The panic handler's `gate.lock().unwrap()` and the waiters' `ready.wait(g).unwrap()` would then panic in cascade, so the parked payload never resumes and the ordered-drain-before-resume contract breaks. No fallible operation sits in the guarded region today — the extend and the publish assignment only, and allocation failure aborts rather than unwinds — so the path is unreachable in the current code. Anyone adding fallible work under the gate lock should recover through `PoisonError` instead.

### 3. Low — `update_tree` and `update_tree_strip` were copy-identical (fixed)

After the far tier landed, the two fills differed only in the `insert_bt1` instantiation and their doc comments. They now share one `Finder::fill_sweep::<LONG>` body behind two one-line entries, with the common contract (the window-floor clamp, the one-call ascending sweep, the compare-form switch) on the shared body. Fixed in `afac680a`.

### 4. Low — the far admission tier had no unit pin (fixed)

`strip_crossing_split_matches_stock` ran with `strip_base: 0`, so the 256-position tier never executed under test. The pin now crosses both tier shapes: per shape the split sweep must equal one stock call, and the far-tier shape must move the tree against the strip-tier shape, so the configuration is live rather than a dead fixture. Fixed in `7a9eed2d`.

### 5. Low — `insert_bt1`'s trace cfg blocks duplicate the LONG branch (reported)

`a7f6efac` widened a pre-existing one-line duplication: the `#[cfg(feature = "job_trace")]` and `#[cfg(not(...))]` blocks now carry the same four-line compare-form branch, and only `add_cmp` differs. Hoisting the branch would read better, but it touches the descent loop's codegen surface and stays behind an Ir check for a later round.

## Verified without action

- The feed's retain parity holds: `take_feed_span` filters `offset > overlap` with the stock strip overlap, which equals the driver's own `chain_reach` retain, and `ldm_generate`'s feed slice keys on split positions inside `[pos, block_end)`. The publication is final once `published` crosses the job end, `install_ldm_feed` parks the driver's own LDM, and `reset` clears both the feed and the parked state.
- The feed's panic protocol covers the lost-wakeup class: the feed parks its payload in the pool poison slot and wakes every waiter, waiters that see `panicked` release their slots as empty and retire, the assembly drains, and the scope end resumes the panic.
- The unpledged deep band's staging rule is coherent: the gate holds every post while the frame is a pending open subject, `cancel_deep_staging` flips the verdict off and re-runs `post_ready` so deferred posts land at the stock cadence bounds, and `resolve_deep_ldm` re-checks the span so no other call site can arm below it. The `DEEP_LDM_STAGE_SPAN` arithmetic (2^24+1 reaching the 2^25 clamp) matches the docs.
- The stepped finish drain keeps its poison-wall pairing: the handoff return re-arms the output wall, `finish_end` re-arms at its exit, and `finish_step` carries no entry assert because `finish_begin`'s tail posts append between the pump's entry check and the first step. The write path still drains inside one call.
- `6798a254`'s NonZero offset narrowing is sound: the admission compare bounds the distance to `[1, window]`, the row window log clamps at 27 so the u32 narrowing is lossless, and the debug assert and SAFETY comment sit at the single unsafe site.
- `45ca65a4` and `c707a272`'s WinPos conversions construct every abs/base pair at one point per site, with the searches destructuring to raw locals at entry as their messages record.
- `900d2d96`'s two pins share one mutex over the process-global switch, and `c2e2eff4`'s capture hook is feature- and env-gated and single-threaded only. The hook's entry block recomputes the window slice that the parse body computes again right after, a trace-only duplication left as is.

## Test gaps

- The stepped drain has no pin of its own contracts (the handoff return with undelivered output, the `close_empty` closing through the step path). The read-side roundtrips exercise the path end to end, but only as an aggregate.
- The serial feed's panic path has no injection test. The stage-A tests already serialize panic injection elsewhere; the feed thread needs the same machinery.
- The strip admission stride's size bar (+0.056% worst cell) lives in the dev sweeps only. An in-repo pin would need a fixed fixture and a size assertion, which is a ratio question rather than a unit one; the split-sweep equality pin covers the exactness half.

## Doc consistency

`perf/mt-stream.md`'s R91, R92 and R93 sections and `negative/mt-stream.md`'s R92 and R95 entries match the code, checked item by item (admission ceiling at the parse start, feed engagement keys, staging span arithmetic, cancel rule, far-tier boundary at the prefill's fill bound). The two R95 kernel-carriage claims named in finding 1 are the only mismatches; they should be re-worded or the guard fixed, whichever round picks the finding up first.
