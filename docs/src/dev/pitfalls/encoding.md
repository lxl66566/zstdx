# Encoding pitfalls

This page collects the encoder bugs, state-contract mistakes and measurement traps that cost real time, so the next change can avoid the same shape. Each lesson keeps its counterexample, and the sections group the lessons by topic: frame format, matcher, entropy coding, memory safety, streaming and output contracts, dictionary training, and measurement.

## Frame format

### Resolve header fields once

The stream-MT capture engagement re-serializes the frame header, and its Dictionary_ID arm read the dictionary directly through `dict.header_id()` instead of the construction-time resolution. `include_dictid(false)` held on the initial header and was silently re-stamped on engagement for a dictionary fast-row stream in the capture band. Default frames never moved, which is why the earlier suites stayed green.

The id now resolves once at core construction and every serialization stamps that field. When a header or any other frame-observable value derives from an option, find every serialization of it, not only the constructor's, because engagement and re-grid paths re-emit what the constructor emitted.

### Multi-block header ownership

The per-block emission contract reserves one 3-byte header per block for the caller to patch. Any path that emits more than one block must take over header ownership explicitly.

The block splitter hit this twice in bring-up. Later partitions computed their header offset as `len - 3` before appending their own reserved bytes, which patched into the previous partition's tail. Worse, the caller then re-patched the first reserved header with the summed size, producing a super-block header that swallowed the partitions' own headers into its bitstream.

Our own decoder roundtripped it permissively while libzstd flagged corruption. Interop through the zstd crate is the mandatory gate for any multi-block-per-parse path. A `SplitOutcome::{Emitted, Single}` return now marks that the headers are already final and size-guarded, so the caller skips the patch and the check.

### Window-capped block size

RFC 8878 sets Block_Maximum_Size to `min(window, 128K)`. The srcSize adjustment can never trigger this, because the window is at least ceil_log2(src) and therefore at least src, so only a forced window log below 128K exposed it. Libzstd rejected even all-raw frames with "Data corruption detected" and error 36, while our permissive decoder accepted them.

Every emission path now reads `Matcher::block_size`, and any new window-shaping knob needs a grep of `MAX_BLOCK_SIZE` in the emission paths first. The pre-split detector's decision window re-hit this. It sized its window by the format's 128K instead of `block_size()`, so tiny-window frames carried over-window blocks and our decoder reported `BlockOutputTooLarge`. A decision input is not exempt from the emission cap.

### Staged-tail anchor

The Read-path pre-split decision asked the matcher for the staged span through `win[idx_of(block_start)..+read]`, but `commit_block` leaves `block_start` pointing at the block it just committed. The staged bytes sit one block later, so every decision after a block read the previous block's bytes.

Output stayed valid and boundaries were silently one block behind. The slice-versus-Read-stream identity test caught it only on inputs whose neighboring blocks decide differently. Any matcher helper over the fill in progress must use `win.len() - slice_size`, not the committed `block_start`.

### Wire-flag size classes

The FCS flag mapping read `3 => 8` where `find_min_size` yields 1, 2, 4 and 8, and the intended arm was `8 => 3`. Every FCS at 2^32 or above hit the panic arm, which covers MT's fixed `frame_content_size: Some(src.len())` for inputs above 4 GiB-1 and a stream pledged at 2^32 or above. All smaller classes behaved, so every ordinary test passed.

Spec-table mappings need boundary tests per class at 255, 256, 64K, 4G-1 and 4G, which pin the descriptor flag, the field width and the decode roundtrip at once. The Dictionary_ID flag had the same shape, where its `_ => panic!()` fired for ids above `u32::MAX` on the publicly constructible `FrameHeader`. The field had no wider wire class at all, so the right fix was the type: `dictionary_id: Option<u32>`, matching the decoder side, makes the panic arm unreachable by construction. A wire field with a finite width belongs in the field's type, not in a match arm's reject case.

## Matcher

### Re-declare frame-scoped matcher modes

`ldm_arming` survives `reset` by contract, like the reach choice, until it is changed. The bulk path re-declares it per frame through `reset_slice_state`, but the ST stream core's `configure` never did, and nothing falsified that while the only reachable value was `Frame`, because the probe's `ProbeKeep` mutation runs on a separate pooled probe driver.

The first mid-frame mutation through `resolve_far_screen` to `FrameScreened` let the next frame on the pooled state arm at configure time with no screen at all, and a random 32 MiB pledged stream silently wrote a W26 single-segment header. Sizes would not have shown it, because random gains nothing from arming and only the header byte moves. The synthetic test's window-descriptor assertion caught it.

Every `set_*` mode on the pooled matcher needs a per-frame re-declaration audit at each pool return path, and byte-contract tests must pin header observables such as the descriptor byte, not only sizes.

### Cursor and anchor invariants

`commit_space` must not set the pos cursor to the block end. Doing so left the matcher fully idle and degraded all blocks to pure literals, while rebuild-style unit tests kept passing. It must be paired with index-validity assertions for skip-matching and with repcode emission assertions.

After backward extension, the offset must use the final position difference `start - cand`.

`anchor_idx = ip_idx` is mandatory after `rep1_chain` returns, in all three emission paths. Otherwise the next sequence counts the chain-covered region's literals once more and decode fails with `TargetTooSmall`. This line was lost once when porting from the old fast code.

Repcode backward extension in the chain loop must keep at least one literal, with `floor = anchor + 1`. At ll=0, `of_value` 1 means rep-swap semantics through the ll==0 branch of `do_offset_history`, and periodic data is guaranteed to blow up without the floor. The chain loop must also probe repcodes when `pos == anchor`, mirroring fast's `probe = pos + 1`. Not probing there loses to our own Fastest by 8x.

### Keep the index domain consistent

The chain-table insert used window indices while the walk used absolute positions, and the two agree only when `win_base == 0`. Once any real driver advances `win_base`, which streaming compaction, bulk per-block adopt and MT job bases all do, the walk runs on truncated, tangled chains at an effective depth of about 2 to 5. The apparent speed lead came from burning fewer probes, the ratio was off too, and the 4x streaming collapse shares the same root cause.

A streaming-versus-bulk output divergence is the smoke alarm for search-state divergence, and the byte-identity assertion had previously covered only Fast and Fastest. The index domain needs the same invariant comment on both the insert and lookup sides.

### u32 table entry arithmetic

For cross-frame stale entries whose value exceeds `pos` with no 4 GiB period to fall back through, `cand - 2^32` must use `checked_sub`, because release wraps into a huge index and reads out of bounds. The first bench run carried that bug for a full round before re-verification caught it.

The other trap is `!u32::MAX as u64`, where unary `!` binds before `as`, yielding 0 and leaving `hi` silently zero. Bit-manipulation helpers need round-trip tests across the 2^32 boundary.

### Clear tables for determinism

Pooled state residue plus a stride-3 grid that writes only one third of the slots made sizes jump between consecutive runs on the same input in the same process, with json.Balanced alternating 5514733 and 5515064. The hazard was latent since the u32 entry change and had been masked by dense table filling.

The fix clears the head table per job like C does. Chain lists may stay uncleared, which is provable by induction: chain slots are read only at candidate positions, and candidates come only from cleared heads or this job's linked values. Frame-epoch clearing was rejected as schedule-dependent, statically binding jobs lost load balancing, and tagging entries paid on the hot path.

### Roll back cross-block state on raw fallback

A raw-block fallback must reconcile cross-block state with what the decoder received. Any reused encoder state, repcodes and reused entropy tables included, has to roll back when a block falls back to raw. Otherwise the next block's Treeless or Repeat reference names a table the decoder never received.

### Matcher scratch geometry

Avoid the `ip1_idx - anchor_idx` subtraction. The new anchor returned by emit can move past `ip1`, which is silent in release, while the debug panic interrupts an MT worker and the join waits forever. Use an exact predicate with no subtraction.

When reproducing MT job scenarios in matcher scratch, keep blocks at 128K or below and adopt only up to `block_end`. A match length above 131074 hits an unreachable path, and an `adopt_window` past `block_end` lets `extend_match` cross the block and yield a huge match length.

### Count mask bytes in hex literals

`0x0000_00ff_ffff_ffff` is three zero bytes plus five `ff` bytes, which is 40 bits and one `ffff` group more than the intended 4-byte mask `0xffff_ffff`. The r13 dictionary-width cut carried exactly this typo, so the 4-byte hash silently stayed 5-byte. Every per-knob A/B on top measured flat, a depth-only residue looked like the whole story, and a comment block full of real numbers pointed at a configuration the binary never ran.

It was diagnosed only by tracing fill and scan hashes on a known 4-byte twin `0a 23 20 74` and recomputing the hash by hand. Width and bitmask constants need a byte-count comment next to the literal, and any "knob does nothing" result needs a does-the-knob-engage probe before it is believed.

### Slice prefix probes at the gate

The reach probe checked `head.len() < PROBE_SPAN` as its engagement gate and then parsed the whole `head`. The probe that was supposed to cost a fixed 36 ms cost 580 ms on a 33 MiB frame, and json solo dropped from 76 to 41 MiB/s. Output stayed byte-correct, so the symptom reads as a feature cost. The probe was a bug.

A prefix-probe pattern must slice `&head[..SPAN]` at the gate, and its cost claim must be re-measured on a large input.

### Keep replay cursors outside loops

Staging probe bytes inside `FrameCompressor::compress`'s block loop reset `staged_read` on each iteration, so the same first block replayed until the frame hung. A 32 MiB json input was stuck for more than 10 minutes at 375 MB RSS. Replay cursors live outside the loop they gate.

### Audit domain conversions at boundaries

Rebasing `win_base` into a local while still reading `self.anchor` and `self.pos` as true-coordinate fields put a whole `shift` of error into the scan cursors, and `ip1_idx - anchor_idx` underflowed on the miss path. That is the recorded underflow class: a debug panic inside an MT worker makes the join wait forever, while release wraps silently and output stays green. One `debug_assert!(pos >= shift && anchor >= shift && block_end >= shift)` at the entry pins what the compiler cannot check.

The enabling property is worth naming. The fast and dfast loops are coordinate-pure, because every absolute derives in-function from `pos` and `win_base`, which is what let the above-4-GiB entry-truncation defect be fixed by rebasing at the parse interface with zero loop changes. The in-loop alternative of u32 wrap-domain arithmetic is falsified on register grounds, so coordinate purity is load-bearing design.

### Defeat repcode carry in test payloads

A 6 GiB probe for the above-4-GiB table collapse passed on both the fixed and the broken trees through three payload iterations. The 64 KiB periodic tiles, and then per-segment repeats with varying distances, all ride `rep[0]` at a constant per-segment distance, and repcodes survive exactly the table collapse the test means to expose.

The payload that bites is a shuffled replay of a 16-byte token vocabulary, where the match distance changes every token so no repcode can carry it, and matches of 16 bytes or less keep every position indexed. Any payload whose matches are rep-carried cannot test a hash-table path at any scale.

### LDM gear arithmetic wraps

The rolling gear hash is `(hash << 1) + GEAR_TAB[b]` over full 64-bit values. C wraps silently, while Rust's debug overflow panic fires as soon as LDM runs on large debug inputs, and release is unaffected.

The same class applies to the `fed` cursor. A shutoff gap must leave `fed` at the arm floor through `gear_rearm` semantics, or the next revival block's `debug_assert_eq!(fed, base)` fires and the canary split lands mid-match, which is a release-mode correctness bug.

### LDM empty-slot bound

`checksum_mask` selects an unwritten slot whenever the query checksum is 0, because the empty encoding also carries checksum 0, and its biased distance resolves to `split + 1`. That is inside the window reach for every split below the window size, so a plain `split - dist` underflows. Debug panics and release wraps, after which `extend_match`'s unchecked reads go out of bounds.

All-zero windows never become splits because the gear over zeros settles at `-GEAR_TAB[0]` and does not trigger, so checksum-0 windows are structured needles the fuzzer must craft. The encode_stream artifact `crash-899d2f24` fingerprints one, and its 64 bytes head a split in any buffer they start because the checkpoint after a window is prior-state independent, which is the regression test's hook. The fix uses `checked_sub`.

### asm lateouts and templates

`gear4_asm`'s template reads `h` after writing `c1` and `c2`, but `lateout` promises only that the output is written after all inputs are consumed. The compiler therefore assigned a lateout to `h`'s register, and `c3` and `c4` were computed from the clobbered value. Dev-profile split sets diverged from the serial gear while the release fuzz build allocated differently from the same source. Any split set yields valid frames, so output gates saw nothing.

The fix uses early `out` for `c1` to `c4`, and `gear_feed_records_serial_splits` locks the inlined instance to the serial split set.

### Empty holdouts skip scoring

The dictionary sweep called `evaluate` even when the train and test split produced no test samples, which happens with a single small source, and the debug assert `!test_samples.is_empty()` fired under `--all-features`. When a sweep can collapse to one candidate, the evaluation step is optional.

### SIMD mask bit domains

The row matcher's packed entry puts the tag in each u32's top byte, and the tag compare was ported as `srli_epi32` plus `cmpeq_epi8` plus `movemask_epi8`. `_mm_movemask_epi8` yields one bit per byte, and with the shift placing the tag at byte 4e of each lane, the entry's bit landed at mask bit 4e. Reading that bit index as a slot index, which is the scalar fallback's contract, searched only slots 0, 4, 8 and 12 and tag-verified only slot 0, so three quarters of every row were invisible while every surviving candidate was still byte-verified.

Correctness gates, roundtrips and MT checks all stayed green. The failure surfaced purely as a ratio and speed tuning puzzle, with text.l5 42% larger and non-monotonic from l6 to l7, and survived several calibration iterations before the mask arithmetic was re-read.

The correct lane-wide form is `cmpeq_epi32` after the shift, where upper bytes are zero on both sides, plus `_mm_movemask_ps`, which gives one bit per entry and needs no BMI2. When porting a compare idiom to a different element width, derive the mask's bit domain from first principles, and treat capacity-shaped ratio anomalies, such as levels that should be monotone in search depth but are not, as a data-structure bug before a parameters bug.

### Slot-choosing state is determinism state

Chain tables' pooled residue is sound because stale entries are only read, range-checked and byte-verified, while live entry placement is a pure function of position. A row matcher's cycling insertion head breaks that shape, because the head chooses where the next insert lands. A residual head makes this job's live-entry placement depend on the pooled state's job history, so two worker-claim orders place different candidates in different slots and the frame bytes diverge run to run.

This is the chain-link landmine's class, reached through new state instead of a missed link. The row heads clear per job with the head table in `fill_window_grid`, and the per-job clear is why the head lives in its own small array and stays out of libzstd's in-row byte 0. A 32 to 64 KiB array clears in microseconds, while in-band state would need re-deriving.

## Entropy coding and bitstream

- Rust operator precedence differs from C's function-call reading. `1u64 << 62 / total` parses as `1 << (62 / total)`, because `/` binds tighter than `<<`, so every normalization step was wrong and the ratio dropped 28%. When porting C's `f(a,b)`-style calls, add the parentheses by hand.
- In the literals 5-bit size format, `size_format` occupies only 1 bit. In the 1-bit form, bit 3 is part of the size, so writing it as 2 bits misaligns the whole rest of the block. For the Compressed type, `sf=2` and `sf=3` are 14-bit and 18-bit sizes with 4-byte and 5-byte headers, a layout different from Raw and RLE.
- Mask packed-bitstream fields per segment. `(packed >> 8)` still carries the OF field's high bits, up to 0xFFFF, so an `& 0xFF` is needed before casting to `usize`. Only a cast to `u8` makes the truncation naturally equivalent.
- The three fields of `SeqWord.codes` are FSE codes. Raw values are a different representation. ML is the ML code, and `ml-3` holds only when `ml < 15`. Decode diagnostics must use the META base or go through the reconstruction path.
- `of_value` semantics differ by match kind. A non-rep match is `offset+3`, and a rep match is in {1, 2, 3}. When diagnostics print offsets, 307203 means 307200+3, and it is not an independent constant.
- Parsing frame structure with Python has its own traps: `nbSeq` bit width, `+` binding tighter than `|`, and sort mixing in kernel symbols. See [engineering methodology](workflow.md).

### Degenerate FSE distributions

The reduce-max-to-half normalization step assumed a second non-max symbol exists to receive the moved weight, and a one-symbol distribution has nowhere to move it. `write_table`'s trailing zero-probability scan also ran unbounded past the table end. Normalization and table-write loops must handle the degenerate case explicitly instead of leaning on another symbol existing.

### The FSE -1 symbol

The positive-probability sum of cross-check inputs must equal `table_size - count(-1)`, or the test generates malformed inputs and false positives.

During SoA construction the -1 symbol is filtered out by `prob <= 0`, so its start state must be written explicitly at allocation time. The baseline packed into transitions uses 9 bits and requires a baseline below 512, so widening `acc_log` must widen the packed width in lockstep.

### BitWriter hot-state handover

A batched writer that takes `hot_state()` and reasons that its adds are at most N bits, with the flush keeping `nb < 48`, must establish that invariant with one conditional flush before the first add. The entry `bits_in_partial` is unbounded from 0 to 63, so the first add can shift bits past the u64 top.

`FSETable::write_table` lost exactly the high bits of its first description entries this way. The symptom surfaced far downstream as a sequence-section underflow in `ramp_frames_decode_byte_exact`, not at the write site.

### Table coverage at every emit site

After the predefined-mode bound went onto `FseTable::max_symbol`, two fallbacks in `select_from_counts` still returned Predefined unconditionally. They were the `nb_seq <= 2` basic shortcut, matching libzstd's `isDefaultAllowed && nbSeq <= 2`, and the normalization-corner fallback.

Libzstd wraps all its `set_basic` returns in `isDefaultAllowed`, and porting a per-site decision without the surrounding gate leaves holes that are unreachable under today's window clamp but corrupt the moment it relaxes. Both fallbacks now check coverage first, and the corner's uncovered arm takes a flat one-state-per-symbol table through `flat_table`, where validity is the contract and quality is irrelevant.

### FSE transition packed width

The u32 entry's baseline keeps only 9 bits, which covers tables of at most 512 states. Production `acc_log` is at most 9, with the huff0 weight table at 6 and the sequence table at 9, so it never overflows. The `fuzz_exports` `round_trip` with `max_log=22` builds a log10 table, makes the baseline at least 512, and silently truncates it, after which the encoder writes diffs above the packed width. Debug asserts blow up under fuzz and release emits a misaligned stream.

The layout is now 12+4+12, `optimal_table_log` already clamps to 5 to 12, and `build_table_from_counts` clamps to 12 with a `build_table_from_probabilities` debug assert as double insurance. Implicit limits in bitfield packing must be enforced at the construction entry, because parameter ranges that production never uses will be stepped on by fuzz or later extensions.

## Memory safety

### Reserve against len, not pos

In bulk bit-write loops `pos` often runs ahead of `len` because `set_len` is deferred to the end. `reserve(fixed value)` becomes a no-op once capacity suffices, and later stores run out of bounds and corrupt heap metadata, which explodes later as `realloc(): invalid next size`. Use `reserve(pos + N - len)`.

### Tail guards move with payload width

After the hash payload became u64, the scan tail guard `block_end - pos < 8` and the insert upper bound `len - 8` had to be tightened together to avoid reading the uninitialized tail left by `set_len`.

### Catch-up fill tail margin

`catch_up_insertions` bounded its inserts at `block_start`, the gated block's end, but each insert hashes HASH_READ bytes and the window guarantees coverage only up to that end. A gate hit followed by a sub-8-byte block therefore read past the window. On the bulk and MT paths the window borrows the caller's slice, so it is a real heap overread, which ASAN reports as a read of size 8 right past the input region, while streaming read stale capacity bytes.

Every other fill path already stopped at `len - HASH_READ`, and the bound is now `(block_start - win_base).saturating_sub(HASH_READ)`, one position short of a scan's own last insertion. Output was always valid because garbage entries only steer probes into byte-verified misses, and the trigger needs gate-then-tiny-block sequencing, which is why ordinary roundtrips never saw it.

### Inclusive bounds and empty ranges

`insert_max` is the last insertable position and is inclusive, but `insert_covered` used it as an exclusive loop end and then computed `(end - p) & 1` for the odd-position peel. A match starting past the bound leaves the range empty, the u64 subtraction wraps, and an odd wrapped difference fires the peel, which hashes 8 bytes past the window.

There are two live trigger paths. First blocks below 9 B saturate `insert_max` to `win_base`, and `rep1_chain` tails on ordinary inputs can start up to 4 past the bound because its scan bar is MIN_MATCH of `block_end`. The streaming ring buffer swallows those 1 to 4-byte overreads, and only bulk inputs whose window ends at the allocation end are ASAN-visible, which is why the fuzzer needed an 8-byte reproducer. Release stays silent and output stays valid because the garbage table entry only steers later probes.

The fix-shape A/B measured a guard branch and a `.max(p)` cmov at +0.23% Ir on json.fastest and +0.28% on text, while a subtraction-free peel-after restructure cost +0.54% because the `p + 1 < end` compare does not strength-reduce. Ship the guard, since wall is flat on the branch and latency-bound tier.

### Head inserts carry links

The chain walk reads a candidate's chain slot after resolving it through the head table, so a head insert that skips the link, such as `insert_covered` through `TableEmit::emit` for the rep1 continuation's covered fill, parks a position whose slot still holds whatever a previous job wrote. The prefill documentation's "stale link slots are unreachable" induction holds only if every head insert also links.

The link-less form let a pooled stream-mt worker's job B walk terminate at a slot left by its own job A, which made the frame bytes depend on worker-claim order, about ±25 B on a 1.1 MiB job, run to run nondeterministic and invisible to fresh-process dumps. Bulk MT was affected in principle and only looked stable through fixed claim order.

The fix is `insert_linked_at`: the chain strategies link every covered head insert through `TableEmit::emit_linked`, while the fast strategy's fills stay link-less because it has no chain. The linked fills also improve the chain band, with levels 5-12 shrinking 0.03% to 0.19% on json and text because covered positions become walkable candidates. The regression test is `pooled_state_reuse_is_output_neutral`. Any "stale X is unreachable" proof must be re-validated per insert path, not only per table, because the induction was proven for the prefill grid and assumed for the emit path.

### Exclude distance zero

A stale slot can hold `pack_pos` of the current scan position, because pooled tables reuse residue across frames and an old-cycle slot can alias anything. `dist = pos - entry + 1` then degenerates to 0, `dist <= reach` admits it, and the candidate byte-compares against itself, so `read4` and `extend` trivially match, the walk returns a zero-offset candidate, and the store gate's `(idx - cand).ilog2()` panics.

The old `unpack_pos` wrap check, which stepped one 4 GiB cycle back when `cand >= pos`, rejected this implicitly. The unsigned form `dist.wrapping_sub(1) < reach` admits exactly [1, reach] and restores it at no extra cost, and the subtraction must be wrapping too, because plain `dist - 1` panics in debug exactly at the dist-0 case it exists to reject. Release wraps, so every dump and ratio gate passed while `cargo test` failed.

The bug surfaced only in the interleaved matrix's timing loop with pooled state across rounds, never in fresh-process dumps, so pool residue is a real input dimension that the dump gate cannot see.

### Origin-shifted sentinels

The origin-free distance resolve rejected a zeroed slot for free, since `dist = pos - entry + 1` in u64 always exceeds the reach when `entry = 0`. The direct origin form computes `dist = q - entry` with `q = pos + origin + 1` in the u32 entry domain, and once the origin's accumulated advance crosses a 2^32 boundary, `q` wraps into `[0, reach]`. An empty slot then decodes as a phantom candidate for a whole reach-wide band of scan positions, silently and origin-value-dependently, fresh zeroed tables included, which is the very state a real clear used to guarantee.

Two fixes were measured. An explicit compare and select cost +3.94% Ir on json.balanced through the walk's per-step cmov, and never storing zero in links cost +12.2% through a write-side branch per insert, which is worse.

The landed form keeps the resolve in the u64 domain and caps the origin at 2^30 through a once-per-about-1-GiB real clear, so `q < 2^32` holds, live entries resolve exactly, and the sentinel dies by arithmetic again at the origin-free per-step cost. The cap-clear later became a 3.5-GiB epoch roll with a negated-floor origin, which keeps `q` small the same way without the clear, as described in [matchers](../perf/matchers.md). Arithmetic that used to make the sentinel dead by construction must be re-derived when the coordinate domain gains a shift or a modulus, and the domain choice itself is a hot-loop lever worth an A/B before the check is written.

### Search exit contracts

`dubt::find_best`'s rep early exit returned without inserting the searched position and with `next_update` still at it. The next `fill_to` then performed the insertion, because the `pos >= next_update` guard inserts before searching, but the exit contract had diverged and a long rep match's insertion was deferred.

The fix makes the early exit write the fill-form entry of head, chain link and unsorted mark, and set `next_update = max(next_update, pos + 1)`. That is table-state equivalent to the deferred fill, but not output-equivalent, because the LDM skip consumes `next_update`'s exact value and the shifted value can flip that heuristic on rare shapes and move the frame bytes while keeping them legal. A multi-exit search owns one postcondition: the searched position is inserted and `next_update` is past it. Each new early exit must restate it instead of relying on the next fill.

### Row matcher twin contracts

`RowEmit::emit`'s covered fill wraps exactly like `insert_covered`'s old bug, where a match starting inside the final HASH_READ span meets an `insert_max` already behind it, `end_abs - from` wraps, and the peel runs about 96 out-of-window hash reads in release. `rep1_row`'s tail compares a fourth byte past the window at exactly MIN_MATCH bytes of block left, the `rep1_chain` twin, and row-inserts the probe, where `row_hash_at` reads the full 8-byte HASH_READ past the buffer.

Two of the three fuzz findings in that round are the row twins of recorded chain pitfalls. Any fill, emit or tail change owes a pass over both families.

### Table geometry pairs move together

`resize_tables`' Row branch keyed its keep on the row count alone. Two row levels share a row count at different hash logs once the source-length clamp shrinks the head table: level 10's rowLog 5 at srcLog 11 and level 5's rowLog 4 at srcLog 10 both give 128 rows. The kept-rows branch resized `tables` while `second` still named the old span, and the per-thread bulk pool carried the stale pair into a later Fast frame, whose heads-keep branch truncates but cannot grow. `parse_fast` then sliced `tables[..second]` past the buffer.

The crash input passes standalone because the poisoner and the victim are different inputs in one thread, so the artifact walker's fresh process per input cannot see the class. Pin it with an in-process call sequence through `encode_edges::pooled_row_geometry`. Any future kept branch that resizes one field of a pair must update its twin in the same branch.

- Guard the small-literal u64 copy on the read side with `anchor_idx + 8 <= win.len()`. Write-side overflow is harmless, while a read-side out-of-bounds read intrudes into illegal memory.
- Use `saturating_sub` for loop guards. Miss stepping can cross `block_end`, and unsigned subtraction wraps into a huge value.
- `insert_max` subtraction underflows on a first block below 5 bytes, where `win_base + win.len() - MIN_HASH` goes negative. Compute it lazily or use `saturating_sub`.

### Merged add width over the accumulator cap

`pack_seq` 把三个字段的 add bits 合并成一个 payload，宽度 `ln + mn + on`。ln 上界 16（LL code 35，base 65536），mn 上界 16（ML code 52，base 65539），on 上界 27（`encode_offset` 取 wire 值的 ilog2，window log 被 clamp 在 27，wire 值 dist+3 不超过 2^27+2）。ln=16 与 mn=16 无法共存：两个 base 之和 131075 已超过 MAX_BLOCK_SIZE 的 131072，装不进一个块。可行组合的 ln+mn 上界是 31（16+15），因此 add_nb 最高到 58。

`hot_push_raw` 的合同是 flush 后 bits < 8、任何 nb ≤ 56 都安全。nb=58 且 pending bits=7 时 `acc |= v << 7` 丢掉 payload 最高位，bits 变成 65；下一次 flush 的 `acc >>= 64` 在 debug 和 paranoid 下是 shift-overflow panic，在 release 下回绕成 `>> 0`，累加器从此失步，序列段的整个尾部损坏。绕过 56 上界的调用有两处：最后一个序列的首 push 完全没有宽度检查；split 分支的 guard 只检查了合并形态 `trans_nb + add_nb <= 56`，进入该分支后单独 push `add` 却没有再检查 add_nb 自身。

触发条件苛刻，需要 w27 窗口加一条 ll+ml 不小于 98307 的序列，所以全部门禁都是绿的：bench 语料的窗口不超过 25，on ≤ 25、add_nb ≤ 56。修复是 `push_add` 在 add_nb 超过 56 时按字段拆分 push，payload 布局 `la | ma << ln | oa << (ln + mn)`，三个宽度从 code 字节恢复（LL 与 ML 走 LL_META/ML_META，offset 的 code 就是自身宽度）。拆分 push 与单次 push 的位序完全一致，先低字段后高字段，所以 add_nb ≤ 56 的所有输入保持 byte-identical，merged 快路径不动。配套的墙有三层：`seq_codes.rs` 的 `const _` 网格证明所有 base_ll + base_ml ≤ MAX_BLOCK_SIZE 的 (lc, mc) 组合满足 ln + mn ≤ 31，`hot_push_raw` 入口 debug 断言 nb ≤ 56，`encode_offset` debug 断言 log ≤ 27。容量合同写在注释里不会约束调用方，要么在每个调用点检查，要么在入口断言。

## Checksums and streaming

### Single-producer rings

"Single producer" means single thread, not single frame. Nested and overlapping frames, from reentrant compression or two-frame same-thread tests, interleave into the same ring. Any scheme that caches the next sequence starting point at frame creation lets two live frames claim the same sequence and overwrite each other, and the inner reset gets clobbered, the worker panics out of bounds, and the main thread spins forever in `FinishCell` while pegging all cores.

Each post must read the sequence fresh from the shared head and claim it, which is non-reentrant within a single thread, and the producer keeps only the drain threshold locally.

### Worker panics and ordering

After a worker panics, every main-thread wait becomes a permanent spin pegging all cores. When investigating, first look for worker panic output under `--test-threads=1 --nocapture`, then kill the process.

Across the ring, the producer orders the slot payload Relaxed, then kind Release, then head Release. The consumer orders head Acquire, kind Acquire, payload Relaxed. Ring wraparound is guarded by `wait_free_slot`, which is independent of frame identity and naturally supports interleaving.

- A streaming frame's fill cannot know EOF in advance. When streaming fills the tail it must do a second read that triggers 0, and it emits one extra 3-byte raw last block. The slice path must explicitly mirror `trailing_empty` for bit-identical output.

### Drain partial writes

`write_output_to` used `write_all` plus an unconditional `output.clear()`, so a writer that half-accepted a buffer and then errored, through `WouldBlock` or `WriteZero` on a socket or pipe, silently dropped every undelivered encoded byte. The caller's retry continued from a shifted position and the frame shipped with a missing stretch, which surfaced only as a decode failure far away.

The fix is an incremental `write` loop that advances the `out_read` cursor, which `split_output` already had the right shape for, and clears only after the drain completes. Both cores share `encoder_core::write_pending_output`, and the `Ok(0)` verdict mirrors each build's `write_all`: `WriteZero` with std and `WriteAllEof` in no_std.

`write::Encoder::write` still stages its input before the drain, so on `Err` the input may already be accepted. A caller retrying under the io::Write "Err means nothing written" contract would duplicate data. Closing that gap needs input deferral at one copy per write on the hot path, and until a caller needs retry-same-buffer semantics the divergence is documented at the implementation.

### Read-side finish and pending output

`read::Encoder::finish` ran `core.finish()`, which emitted the last block and checksum into `core.output`, and then returned the inner reader, dropping `output` with it. The caller had already read only a prefix without the closing block, so the frame was truncated, and the read side has no later drain that could deliver the tail.

`finish` now fails with `Error::UnreadOutput { bytes }` unless the encoder was read to end of stream first, where the final read's `Ok(0)` both finishes the core through the pump and drains the output. This is the read-side twin of the drain pitfall. A `finish` that hands back the wrapped resource can only succeed when nothing is in flight, so the pending state must be an explicit, countable error rather than a documented loss.

### Bound the write_all reserve

The mt stream write path handed `make_space` the entire remaining input as its growth target once the buffer filled, so a single `write_all` of a multi-hundred-MiB slice reserved exactly that much. `BUF_CAP_MAX`'s "past the cap the wrap alone recycles" bound never engaged on this path, while the read-side `pump_direct` was bounded per pull and unaffected.

The full-buffer write step now asks for one bounded increment through `PUMP_STEP_MAX`, the same bound as a direct read pull, and the loop refills and recycles. Each step's posts keep completed jobs widening the wrappable dead prefix, so capped steps progress instead of spinning, because a bounded want of at least 1 at a full buffer always wraps or grows. The proactive doubling ladder below the cap is untouched, so steady-state buffer sizes and frame bytes do not change, since the job grid is a pure function of stream offsets.

### Pledged sizes are contracts

Both stream cores wrote `options.pledged_size` straight into the frame header and never compared it with what was fed. A stream pledged 10 bytes but fed 20, or pledged 100 and fed 20, produced a normal-looking frame declaring FCS=n over content not equal to n, which every decoder rejects. Libzstd checks the pledge mid-compress and at the frame tail, and the CLI pledges the stat size and then copies, so a file changing under it silently shipped a broken frame.

The fix follows libzstd's split. The first write whose bytes push the fed total past the pledge fails that write itself with `srcSize_wrong`, nothing staged and deterministic, and both core finishes catch any residual mismatch before emitting the closing block. Finish cannot promise that no frame byte was emitted, because `write::Encoder::write` drains eagerly and the sink may already hold the header and completed blocks.

The surfaced failure is a truncated, unclosed frame, which is the expected outcome. The first version of the fix claimed byte-level prevention, which must not be re-claimed. Anything written into a frame header as fact must be verified at the earliest boundary that can observe the lie, which is the write that crosses it, not only the finish that closes it.

### Windowed one-byte FCS class

Flag 0 means the Frame_Content_Size field is absent unless the single-segment flag is set, but the header serializer mapped every width-1 size from 0 to 255 to flag 0 and appended the 1-byte field anyway. Any pledged stream or bulk-mt input below 256 bytes, and the CLI pledges every non-empty file's stat size, serialized a stray byte that shifted the first block header. Both this crate's decoder and libzstd rejected the frame as an incomplete frame or data corruption. All boundary classes at 256 and above worked, so every ordinary roundtrip test passed.

`FrameHeader::serialize` now normalizes: a windowed frame below 256 bytes drops the declaration, the pledge keeps shaping the encode, and only the header promise is lost. Descriptor-flag tables need a test per flag class crossed with the flags that redefine them, because `single_segment` redefines flag 0.

### Poison flag ordering

The stream-mt worker's panic path aborted its job with an empty slot, then flipped the `poisoned` mirror flag, then stored the payload under the queue lock. That is a two-step publish, and the posting thread could observe flag-set and payload-absent in between. `surface_poison` surfaced nothing, `drain_all` assembled the aborted job's empty slot like any completion and returned a frame missing the poisoned job's bytes as `Ok`, and the payload was stranded in the worker. The bulk path is immune because it stores the payload under its mutex first and then the slot.

`QueueShared::publish_poison` now orders payload, then flag, then abort, and the abort must stay after the flag or the assemblable-empty job drains ahead of the payload it implies. The drain's "pending empty, surface, return" arm is safe only because a set flag now guarantees `inner.poison` holds the payload. `surface_poison`'s flag-set and payload-absent arm is unreachable and now panics instead of returning, because a silent return would re-ship the exact corruption the ordering fixed. A fast-path mirror flag of mutex-guarded state is a second publication and needs its own happens-before story.

### Refuse partially honored knobs

`prepare_job_state` armed the deep-offset ramp from `ZSTDX_MT_RAMP_BYTES` on every MT job, but only the fast, dfast and chain parses read the gate. The btlazy2 rows 13-15 and the opt rows 16 and above silently produced unconstrained frames under the environment variable, so a piece-decode experiment on those levels measured nothing. The frames are legal, but their crossing reads have unbounded shallowness, which fails the piece executor's validation, or on shapes whose reads happen to run deep slips past it and fakes engagement.

`MatchGeneratorDriver::arm_ramp` now panics on `BtLazy` and `Opt`. The tree rows' wiring points are enumerated in its doc comment: `dubt::find_best`'s rep loop and sorted descent, `btlazy::run_block_lazy`'s two rep probes, the literal-offset backward extension floor, the LDM far-candidate check, and the opt DP walk. The panic must be unconditional rather than a `debug_assert`, because release is where the experiment runs.

### Arm donated spans as job zero

The reach probe's donation hard-armed `LdmArming::Job`, but a Keep verdict can re-grid the frame onto the mid-size prefix-LDM capture through bulk `plan_prefix_ldm` or stream `engage_midsize_capture`, whose jobs arm `JobPrefix`. The capture's source-clamped window in the W25 band sits below the `Job` bar, so the donated state's LDM never armed while jobs 1 and above armed. Job zero lost the far candidates inside its own span and the frame diverged from the undonated schedule: a 32 MiB tiled row-9 frame at w2 measured 7.94 MB against 4.76 MB, which is 67% larger. The divergence is invisible at w4 and above because a job at the W22 strip floor has no beyond-reach distances.

The arming is decidable before the verdict, because the capture engages exactly when a Keep verdict would run it, so the donation gate computes the capture plan first and arms the donation with its arming. The Shrink verdict pays the span's split pass for nothing, which is the band's price, and debug asserts pair the frame's capture with the donation's arming on both paths. A contract that says the donated span is job zero's parse must arm the donation from the job's final schedule, never from the pre-verdict default.

### Initial reserve is a scheduling scale

The mt stream's initial and probe-settle reserves sized to `workers * initial_job + overlap`. With the CLI always pledging the file size, a 16 GiB file at `-T8` with the job size at the `MAX_JOB_SIZE` 1 GiB ceiling reserved about 8 GiB up front and held that much input resident for the whole stream, because the pump recycles only at a full buffer.

Both reserves are now capped at `BUF_CAP_MAX`, the same ceiling the unpledged growth ladder uses. The pump grows into the live need through `make_space`, whose wrap recycles at grid lines where quiescence is reachable, and `make_space`'s growth arm still exceeds the cap when the live set demands it, such as a pledged grid's held-back final job. One floor remains: the fixed grid's first job cannot post until a full job-size is buffered, so the buffer still grows to about one job plus the strip mid-stream, which is the grid's own semantics rather than the reserve.

### Growing-grid epochs stay bounded

`post_due` holds a whole epoch of `burst_jobs` jobs plus the next job's strip before anything posts, so the accumulate buffer's live span is `strip + burst_jobs * job_size`. The doubling ladder's `MAX_JOB_SIZE` 1 GiB ceiling let that scale with the worker count toward workers times 1 GiB on long unpledged streams, which is the CLI stdin path. VmHWM measured 1.4 GiB at w8 with 3 GiB L3, 1.5 GiB at w2 with 3 GiB, and 1.6 GiB at L9 w8 with 2 GiB, which is OOM scale on small machines.

`growing_job_cap` now ties the Growing grid's job-size ceiling to `(BUF_CAP_MAX - overlap) / burst_jobs` and clamps the first epoch's `overlap` floor too, so the staged epoch stays at the buffer's own scale for any stream length. L3 w8 reads 361 MiB VmHWM at 1 GiB and 3 GiB alike, and L9 w8 reads 552 MiB, which is the capped buffer plus the level's fixed per-worker tables. The strip estimate is `overlap` and never `strip_span`, because the far-class screen resolves only at the first post and the grid must stay a pure function of offset.

The costs are that jobs below the whole-window strip pay strip-fill per job, measured at 7.5% slower wall for L9 w8 on a 2 GiB unpledged stream and 12% to 25% faster for L3 because smaller jobs pipeline better, and that long unpledged streams' frame bytes move to a denser job grid. The 32 MiB bench corpus is byte-identical, with zero deltas across the full 120-cell ratio A/B. Reclaiming the L9 redundancy wants the finish tail's shared prefix fill extended to streaming posts. An epoch-staged posting gate makes the job-size ceiling a memory bound, and a ceiling shared with the bulk formula is not one for the streaming grid.

### LDM limitTableUpdate floor

The btlazy gap-parse set `next_update = next - 512` unconditionally once an LDM emission cleared the 1024 bar, but libzstd's `ZSTD_ldm_limitTableUpdate` also caps the backstep at `curr - nextToUpdate - 1024` through `MIN(512, ...)`, so the update point never lands below `next_update + 1024`. The floor raises the update point, which makes the tree fill skip more, and does not add insertions. Our bare `-512` indexed up to 512 extra positions whenever the emission only just cleared the bar, with a gap in the 1024 to 1536 range.

The aligned form is `backstep = 512.min(next - next_update - 1024)`, and the guard makes the subtraction positive, so u64 needs no C-underflow care. Measured dll100 at L13 moves the frame by 287 B on 19.59 MB, which is +0.0015% and noise. The 32 MiB ratio corpus never hits the band, with a byte-identical 120-cell A/B and geo-mean +8.67% on both sides, and wall is unchanged at dll100 L13 over 3 iterations, 10.684 against 10.654 s. The opt family is deliberately untouched, because C's btopt and above consume LDM as candidates with no `limitTableUpdate`, and `opt.rs`'s `match_end - 8` is the tree search's own interior skip.

### Price the real serialization in selection

The sequence-table selection priced a fresh table's description as a flat `(max_symbol+1)*6+8` bits while running the fast-row heuristic thresholds everywhere. Libzstd switches `ZSTD_selectEncodingType` arms by strategy, and everything at or above `ZSTD_lazy` compares predefined, repeat and fresh on exact integer costs with the real NCount bytes.

On a small LL alphabet the flat estimate charged about 12 B where the acc-5 NCount serializes to 5 B, so the compare leaned predefined on exactly the small-histogram blocks a dictionary frame is made of. That cost 17 B of bitstream per flipped block and affected 30 to 35 of 54 holdout files.

The fingerprint for this class is entropy bounds in our favor while the frame loses bytes, with a section diff showing same-mode bitstreams differing. That means the leak is in the decision, not the stream. Any cost read off a pooled table, including probs or tt tails past `nsym`, must bound the alphabet first, because the stale tail reads as plausible costs and can return a repeat verdict for a table that does not cover the histogram.

## Dictionary trainer

### Sorted concatenation poisons selection

The fastCover epochs partition the concatenated training body. With a sorted body, ordered alphabetically by glob, every epoch is a cluster of near-identical files, each epoch's best segment describes its cluster instead of the collection, and the positional 75/25 train and test split lands on a distribution shift. On the systemd fixture this alone was worth about 20% dictionary quality, with holdout 7,371 to 6,030 B at matched k against a libzstd -9 referee.

The fix is a fixed-seed Fisher-Yates shuffle over sample order before concatenation, a port of libzstd's `DiB_shuffle`, which is deterministic and representative. A related trap is that `zstd --train-fastcover=k=K,d=D` with forced parameters, which is the non-optimizer path, trains on all samples, because the 75/25 split exists only in the optimizer. Forced-k dictionaries therefore leak the test set in A/B comparisons, and a leaked k=537 dictionary scored 5,685 against the unleaked 6,085.

### Trainer test noise must be aperiodic

The `(i*131 + j*17) as u8` noise repeats its 8-grams about 30 times across files, because all files land on the same 256 k-mers under mod-256 arithmetic. A 61-byte pattern repeated 8 times also lets a shared or noise-straddling window cover all distinct shared k-mers plus free noise coverage. Both made the noise out-score the shared block. LCG noise with per-file seeds and a non-repetitive shared block keep the assertion meaningful.

### Entropy preloading versus heuristics

The seeded tables must compete on measured costs, and the tuned heuristics starve them three ways, measured on the systemd holdout through cross-fed dictionary decomposition. The Huffman treeless gate `diff > 5`, a sum of code-length differences, rejects dictionary tables that would win by tens of bytes, and the right comparison is stream bits against stream bits plus the exact serialized description in the `HUF_estimateCompressedSize` form. The FSE predefined thresholds, with `nb_seq < 72` selecting predefined, fire before the repeat check, so dictionary tables never reach the small blocks where their statistics matter, while libzstd's lazy and above paths compare predefined, repeat and fresh purely on costs. Treeless blocks below 1 KiB should take the single-stream literals form, with no 6-byte jump table and one padding tail instead of four, which libzstd emits whenever `repeat_valid && lhSize == 3`.

Landing all three took zstdx -9 with our formatted dictionary from +11.6% to +3.0% against libzstd on the holdout, and flipped the formatted-versus-raw delta from +3.1%, worse than raw, to -5.7%. All of it is gated on a per-stream "still dictionary statistics" flag, and every no-dict path keeps the stock selection with a byte-identical full-ladder dump.

### Cross-fed dictionary decomposition

Train both trainers on the same corpus, then run {our encoder, libzstd} against {our raw content, libzstd raw content, libzstd formatted} over the holdout. Same-encoder raw-content pairs isolate trainer quality, same-dictionary encoder pairs isolate the encoder, and formatted-versus-raw per encoder isolates the entropy-preload handling.

On this fixture the split showed trainer parity, an encoder-side +7.8% small-band residue, and an entropy component that was actively harmful, which is three different levers from one measurement.

### Dictionary fill follows load semantics

`load_dictionary` had routed through `prefill_window`, inheriting the strip's stride-3 grid and Fast's retention-tail cap, which exist because a window-long strip cannot afford a dense fill. A dictionary is at most one window and the payload parses mostly against it, so coverage dominates over fill cost. Libzstd fills chain rows densely through `ZSTD_insertAndFindFirstIndex`, and the CLI's small files go through CDict attach, whose tables carry the `dtlm_full` backfill. Reusing a strip-tuned prefill for another consumer of the same tables silently dropped two thirds of the dictionary's candidates for chain or the whole dictionary head for Fast's cap.

The level fingerprint was a chain-row-only end-to-end gap, worst at -9, with -3 and dfast at parity because both sides stride-3 there. A corollary is that the dictionary chain path needs no `acquire_seed` once dense, measured byte-identical, because the seed's period-repeat compensation is for single-candidate head tables and dense links already reach every position.

A second knob in the same class is that our `search_depth` table halves libzstd's `1 << searchLog` attempt counts as a big-payload speed tuning. Parity holds there, but a dictionary parse walks a long dense chain where the full budget converts, worth +2.1% to +1.9% raw control at -9 on top of the fill fix.

### Filter seeding is phase-critical

Seeding the incompressibility gate's repeat probe from a job strip with one whole-strip stride hits a modular lottery. A period-P repeat probes a hit only for blocks whose sample grid happens to align with the seed stride modulo P/gcd. On a synthetic 5 MiB-period corpus, four of five jobs lost the far class while the one job whose strip length picked a coprime stride kept it: a 12 MiB strip derived stride 385, which is 5*7*11 and shares factors with the block stride 33, while an 8 MiB strip gave 257, a prime, which was lucky.

The fix replays the consumer itself, with the same block-sized segments, the same per-segment stride and the same phase restart, so a repeat at a block-multiple displacement lands on the segment grid a scan would have planted. A future "pre-plant a sampled filter from history" must copy the consumer's exact sampling geometry, and matching its density is not enough.

## Guard and codegen follow-ups

### Sentinels through contracted helpers

Btlazy's depth-0 rep probe priced the incumbent `best` even when the tree search returned nothing. `lazy_value`'s `debug_assert!(len >= MIN_MATCH)` fired on the `{off: 0, len: 0}` sentinel, and `full_ladder_roundtrip` failed in every debug run. Release passed because `s*0/4 - lazy_price(0) == 0`, which is accidentally the correct no-candidate baseline.

The sentinel's semantics must be explicit at the call site, with `incumbent = 0` when `best.len == 0`, instead of relying on len-0 arithmetic degenerating to the right value. The release behavior was never wrong, but nothing tied the two together.

### Keep the btlazy2 rep off-base

Collapsing the longest-rep candidate to `off_base 1` because it is a rep emits the wrong offset whenever the winner is the rep1 or rep2 form. The roundtrip fails only on rep-dense data and far into the stream, found at about 4 MiB on json. Off-base 1, 2 and 3 each name a different decoder action at ll==0, which are swap, rep2 and rep3, and a different plain repcode at ll > 0.

### opt.rs port traps

A debug `eprintln!` under `#[cfg(feature="std")]` is compiled into release builds with default features and once silently flooded stderr. `ZSTD_count` returns a pointer difference, so continuing a count must replace the value and never accumulate it.

Rep history is updated once per series by the path walk, and emitter-side per-sequence updates are the decoder-equivalent semantics, so pick one, because duplicate updates always blow up. The insert-into-tree counting cap belongs in the DP window of 4096 slots, or regions skipped by the parser get re-counted from stale heads, which is the lesson behind text moving from 45 to 313 MiB/s.

### Gain gate meaning

C's +7 is the margin for whether a later candidate replaces the already-held match, not a store gate. Putting the gate on the store decision is stricter than C and deliberate, because our tables are denser, but the cost is that short-distance matches in text get falsely killed.

### Wrap-dependent fused checks

The DUBT diet's fused checks `cand - (floor + 1) >= span` rely on the unsigned wrap of a below-floor candidate resolving to huge, which is the intended out-of-range verdict. The same wrap trips the debug profile's overflow checks, and the whole library suite had been red since that commit with 16 tests while every release gate stayed green.

Wrap-by-design arithmetic must spell `wrapping_sub` even where the release semantics are the point, and a red debug suite on a landing branch is a gate failure, not noise.

### Cost table -1 markers

`ZSTD_crossEntropyCost` maps `norm == -1`, the low-probability NCount wire form, to probability 1 before the `kInverseProbabilityLog256` lookup. The port read the raw i32, and `(u32::MAX << shift)` indexed 4 GiB past the 256-entry table.

It is reachable only through the exact-cost arm for DictEntropy frames, where a literal run of 2051 bytes or more hits the LL default distribution's -1 tail at codes 32 to 35. One holdout file in the rebuilt fixture carried it, and the earlier fixture had that file in train, so the roundtrip gate never compressed it. A dictionary-frame-only panic at a table-indexing site is a wire-form artifact, not memory unsafety.

### Gate dict-cursor updates

The catch-up insert cursor initialized to `u64::MAX` on no-dict frames but advanced `next_ins = pos + 1` after every probe unconditionally. One probe in, the cursor went live, the `pos > next_ins` guard fired on no-dict frames, and json and skewed l9 bytes changed.

The full-ladder dump caught it, and the dictionary-fixture sweeps cannot see the leak by construction. Per-frame feature state that decides fills belongs in the guard itself.

### Full-width mask shifts

The row search's head rotation masked its rotated candidate mask with `(1u64 << entries) - 1`, which is correct for the 16-entry rows it shipped with. At rowLog 6, with `entries` at 64, the shift equals the register width. Debug builds panic, while release x86 masks the count to 6 bits and shifts by 0, so the mask collapses to 0 and every search silently returns no match. No assert fires, and the encoder just emits literals, while the tests that passed with rowLog 4 did not cover the wider instantiation.

The full-row arm now uses `rotate_right`, which is total for any count. `(1 << N) - 1` and `x << N` are total only for N strictly below the type width, so a const-generic width needs its full-width arm written explicitly, and every new instantiation of a width-parameterized path needs at least one debug-mode roundtrip at that width.

A companion edge from the same widening is that an array length cannot key off a const generic through arithmetic, because `[0u32; 1 << ROW_LOG]` wants `generic_const_exprs` and a `const` item in the function cannot see outer generics. Materialize the shipped widths through a compile-time dispatch onto a second const-generic function, `row_search_cap::<ROW_LOG, 16/32/64, _>`, which also keeps the pre-existing width's stack layout byte for byte.

### Live window length is not source size

The small-frame tree-hash screen keyed on `win.len() <= 128 KiB`, but the window slice is ring or buffer capped near one block, so the condition held on every block of every large text frame. It switched the DUBT tree to 4-byte hashing frame-wide, costing 32 MiB text 30% at the Best tier, and only the full 120-cell sweep caught it while every small-payload gate stayed green. Size-keyed policy must read the declared input shape through `params.small_src`, never the accumulated window.

The dual bug is that a row replacement through `return adjust_params(btlazy(...))` silently resets constructor-defaulted flags, so `small_src` came back false and disarmed the same screen for the frames it was built for, costing 115 B on text-16K l9 with all gates green. The symptom class in both cases is per-cell ratio drift with no test failure, and only the sweep's byte accounting catches it.

### Screens are O(samples)

At the fastest tier a 1 KiB to 128 KiB call is 6 to 40 µs, and one extra window pass costs 20% to 50% of it, measured at 47% slower on skewed-16K. The same lesson bit twice inside the screen.

A `[u64; 4]` bitmap indexed as `bitmap[b >> 6] |= ...` compiles to a store-forwarding chain at about 2.8 cycles per sample, and keeping the four words in registers through a `match` fixes it. A runtime-variable bar in the scan's hot loop, with `ml >= min_ml` and `miss >> shift`, costs 22% wall, where the same value as an instantiation constant folds away.

The frame-parser tooling for section diffs has its own map: literals size formats `00`, `01`, `10` and `11` mean 10-bit one-stream, 10-bit four-stream, 14-bit and 18-bit. `read_ncount`'s `nbBits` and `threshold` shrink dynamically with `remaining`, and the reader prefetches, so each table needs a fresh reader before its walk. The NCount header's accuracy field stores `log - 5`.

### Hot loop body shape

Restructuring `rep1_chain`'s tail into the clamp-first form cost balanced json +1.39% Ir while the restructured body is never executed by the bench. A semantically identical revert measured +3.0%, and the landed post-emit `pos.min(block_end)` clamp that kept the base loop shape recovered to +0.44%, with all of it register-allocation placement around the once-per-parse accessor chain.

This is the walk-shape entry's class, and semantic gates such as dumps and tests are blind to it while only gungraun Ir sees it. When hardening edits share a file with a hot loop, keep the loop's instruction shape untouched and attach the guard before or after the loop. Bisect with gungraun per hunk rather than per round, because placement effects do not compose linearly.

### Clamp search domains to the final window

The tree rows' domain was `chain_reach`, the ring's reach, not `window`. A forced `window_log` on an unpledged stream updated `p.window` while `adjust_params`' unknown-length early return left `chain_reach` at the stock 1<<22 or 1<<23, so rows 9, 13-15 and 16-19 emitted offsets past the declared window. Libzstd rejects those, and at w16 its ring silently decodes wrong bytes, gated only by the checksum.

C's legality invariant is `ZSTD_getLowestMatchIndex` at the final windowLog, unconditionally in every search loop, while `adjustCParams` log clamps run only under a known srcSize because they are memory clamps, not legality. The zstdx mirror is the unconditional `clamp_geometry_to_window` with chain, row and bt at most wlog, hash and dfast short at most wlog+1, and the bt ring's two-link-slot layout makes `bt_log` exactly C's cycleLog, with the length-gated downsize kept separate.

Repro shapes for beyond-window redundancy need no near twins, and the `farfrags` and `twinhalves` classes in `encode_edges` cover them, while near-periodic shapes with a period at or below the window never catch the class. The audit is in [winsem](../review/winsem.md).

### Hoist every cursor of a moved field

C derives `nextToUpdate3 = ms->nextToUpdate` at block entry. The eager dictionary seed moves `next_update` to `keep - OPT_NUM` without moving hash3's coverage, so the frame's first block inherits a hash3 fill hole over `[0, keep-OPT_NUM)` where the lazy flow fills contiguously from 0.

It was probed unreachable on the current ladder, because every hash3 user row from 18 to 22 has `mls == 3` and the 22-bit main tree hashes the same 3-grams and re-finds any candidate the 17-bit slot would offer. Cached-versus-fresh output is byte-identical on ordinary and adversarial rep-suppressed fixtures. The unreachability is a property of the ladder, not of the code.

A seed-time hoist of a field that other cursors derive from owes a fill, or a reachability proof, for every derived cursor. The proof must cite what makes the gap unreachable, here the mls shadowing, rather than a byte-exact by construction argument over the tables that happened to be audited.

## Measurement and tooling

### ST output depends on process history

The thread-local matcher state pool persists across frames, following the design that leaves u32 tables uncleared, so the same input can produce different but each valid frames under different in-process histories. Level::Fast on a 24-byte input measured 37 B as a streaming raw block against 35 B through bulk. The symptom is a test that passes sometimes and fails sometimes inside one process.

Cross-build byte comparisons must therefore use a fresh process, which is the dump tool's purpose, and the cross-check oracle uses two-sided decode validity rather than byte equality, which is what the encode_stream fuzz target does. The same problem for MT jobs was fixed by per-job table clearing, and the ST variant stays open. If libzstd-style same-input same-output determinism is ever wanted, the options are a reset with table clearing at a speed cost or the frame-epoch scheme recorded with the u32-table pitfall above.

### Measure the rebuilt binary by name

An earlier era's `target/release/zstdx-cli` survived in `target/`, and the binary has been named `zstdx` since. It kept answering shell history's muscle-memory path, so a full fixture sweep ran on a three-day-old binary and reproduced a baseline that no longer existed, complete with the pre-swap `-3` equal to `-4` signature that had already been fixed. The symptom worth memorizing is an unchanged number where a landed change must have moved something.

Verify the artifact with a discriminator level whose output a landed change provably alters before believing any before and after on a rebuilt binary. Also note that `cmd &` in a per-call session shell backgrounds the whole `a && b &` chain unprotected, so a build and sweep pair can die between the two and leave the stale binary answering.

### Trainer cross-check artifacts

`zstd --train -v`'s small-corpus warning prints "the source size" as `nbDmers`, which is the training split's byte count minus 7, the `MAX(d,8)-1` term, and not the split's size. Reconstructing the CLI's train and test sets from it mismatches by a few bytes and sends you hunting for a shuffle divergence that is not there. The shuffle ports exactly: same seed 0xFD2FB528, same LCG and same byte-sorted input give the same 64/22 split and identical byte totals, verifiable against the level-2 "Training on N samples of total size X" line.

`ZDICT_finalizeDictionary` computes the dictID over the untruncated content before shrinking it to capacity, so re-finalizing an extracted content section yields a different but valid ID, and the differing bytes 4 to 7 of a cross-fed header are an artifact rather than a table difference. Compare tables only past the ID.

The trainer's internal `evaluate` scores checksum-less library frames while a CLI referee loop adds a 4-byte checksum per file, so absolute values differ by a per-file constant and only rankings are comparable. A related measurement trap is that two trainer A/B harnesses writing the same output paths in parallel corrupt each other's dictionaries and logs, and the race reads as a nonsensical k pick. Serialize experiments or namespace their outputs.

### Newest-wins tables insert once

`emit_chain`'s covered fill re-inserted the greedy match start and linked the slot to the head it had just become, which is a self-reference. The walk burned its whole depth budget on the loop while the bucket's dictionary twins sat behind the severed link. `seqstats --ref-frame` showed we emitted `ml=5 of=15` where the CLI found `ml=15 of=3552` 8 hops down the same chain, confirmed by a Python replay of the fill and walk.

Skipping the fill below the scan's frontier through `max(start, filled_from)` removed the self-link but regressed every dictionary row on the 3-seed fixture, by 14 to 81 B for chain and 144 to 452 B for opt. The scan's rep-probe advance skips positions the re-inserting fill was silently covering, and opt walks probe far past the emission start, so the skip also dropped the earlier interior fills. A second-order trap is that the corruption silently re-tuned knobs, and depth sweeps showed saturation from 128 up because the severed walks made every depth look saturated. The exact-once shape of filling the rep-advance holes and never re-inserting a head is the open path, recorded in [negative](../negative/matchers.md).

### Pooled-state replay guards

`seed_gate_probe` guarded `data.len() < GATE_MIN_BLOCK` only inside the `probe.is_empty()` arm. A pooled matcher whose probe table was warm from a previous job then met a zero-length strip at a staging or freeze boundary, such as stream-mt finish with `strip_is_uniform(&[]) == true`, and the uniform branch's `read8(data, 0)` faulted on the empty slice's dangling pointer at address 1.

That was a flaky SIGSEGV, and the parallel ratio sweep hit it in about one run in three while the serial sweep never did, because pooled-probe warmth depends on worker scheduling while the frame bytes stay deterministic. `coredumpctl gdb` on the preserved core pinned the frame to `seed_gate_probe` plus `MtEncoderCore::encode_jobs`, and the faulting `imul (%rbx)` with the gate's multiply constant identified the branch before any source reading. Any guard that licenses an unchecked read must hold on every path that reaches it, warm-state replays included, and the regression test warms the probe through `prefill_job_strip` and then pre-fills an empty strip.

### Walk loop shape

`strided_distinct`'s flat-head diet landed as a two-stage scan. Stage 1 counted 512 strided samples per byte for a `top`-share RLE verdict, and stage 2 resumed through `head.iter().step_by(8).skip(samples)`. Two codegen taxes were invisible to every semantic gate. `Skip<StepBy>` compiles to a per-iteration `spec_nth` call with the iterator state spilled to the stack, measured at 3.4 times the screen's cycles with `far_screen::screen` going from 2.45% to 8.25% of the profile and text.fastest and text.fast losing 13% wall. Slicing from the byte offset through `head[samples*8..].iter().step_by(8)` restores the plain stride induction.

An early `return` inside the walk's body for the 128-distinct cap keeps the loop un-rotated where the cap-free body unrolls 2-wide, so a head that never reaches the bar pays the scalar shape for nothing. The final form is a SIMD `is_uniform` probe on the first 4 KiB for the exact-RLE zeros reject, then a bounded cap hunt over the first 4096 samples where wide-alphabet heads leave for the twin scan early on random's leg, then a cap-free remainder walk for text and json with the old-shape unroll.

It is verdict-exact against the pre-diet walk for every non-exact-RLE head, with a full-ladder dump byte-identical against the base build, which is stronger than the counting stage whose 99% top-share reject also miss-armed near-RLE openings. text.fastest and text.fast recovered to 13399 and 13317 against 13470 and 13427 in the same locked window, and zeros and random keep the diet's wins. Iterator-adapter stacks on hot loops get disassembled before landing, and a per-element branch that only some payload classes take belongs in a bounded prefix loop, not the shared body.

### Residue gates key on role

The reach-probe shrink's apply gate matched `chain_reach == Some(KEEP_REACH)`, a value the btlazy tree rows carry as their stock search domain. A pooled stream core's leaked row-9 Shrink verdict collapsed row 13's 4 MiB tree domain to 4 KiB and made the frame's bytes depend on what the core had compressed before. json l13 stream was 13.5% off bulk, both decode-valid, and the new `dump --stream` grid was the only gate that caught it, because every size-keyed and fresh-process gate was green and the matrix execution order never triggers it.

Residue-class gates belong to the same predicate that declares the state's owner, here `reach_probe_eligible`'s Chain-only rule, never to an incidental value equality. The diagnosis used a pooled-versus-fresh process replay of the exact in-process level sequence, and the poison needed the full l1, l3 and l9 warmup while an l9-only prefix was clean. Instrumentation that makes a bug disappear is a layout or UB signal to switch tools, and a probe-print build un-poisoned this one before valgrind memcheck became moot through the replay.

### Inline job views start at the strip head

`run_inline_job` sliced `buf[..hi - buf_base]` while its `first` index counts from `strip_lo`, which is the same convention as every posted job's frozen view, whose pointer `post_job` builds from `strip_lo`. The two align only while `strip_lo == buf_base`, which holds until a flush re-grids `job_start` past the buffer's head under a window shorter than it. Forced window_log 9 or 10 with a flush at 2051 of 4095 gives `strip_lo` 1027 and `buf_base` 0.

The inline path then handed the runner a view offset by 1027 bytes and the job re-encoded stale earlier input as the post-flush span. The frame decodes fine, so only the checksum compare or a byte compare catches it, as `ChecksumMismatch` with the checksum on and a silently wrong roundtrip without. It reproduced identically on the pre-wiring base, so it is pre-existing since the inline path's introduction. A view's index convention is a contract shared by every builder of that view, and the inline twin of a worker path must construct the same view the worker gets, not merely call the same runner with a differently anchored slice.

### Flush-regridded epoch ends

`post_ready` computed `end = job_start + epoch_size` but gated the post on `pos >= growing_epoch_end(job_start)`. On the untouched lattice the two agree because a job never straddles an epoch. A mid-stream flush parks `job_start` at an arbitrary mid-epoch offset, where the lattice-relative `end` can lie past the epoch boundary. The gate then opens exactly at the epoch end while `end` still exceeds `pos`: on a 3 MiB stream with a flush at 2622464, the job `[2622464, 3671040)` was due at pos 3145728.

`post_job`'s `debug_assert!(pos >= end)` caught it in debug, while release's frozen view read `end - strip_lo` bytes that were never staged and emitted a corrupted frame. The fix clamps the post step to the epoch end, the same clamp `build_bounds`' walk applies, and it is a no-op on the untouched grid, so existing frames keep their bytes. A posting gate keyed on a coarser boundary licenses every finer step under it, and the gate and the boundary function must share the clamp, not just the arithmetic.

### Arm poison walls at construction

The walls' contract is exit-fill and entry-assert: the producer poisons the slack past the logical length on its way out, and the consumer asserts it intact on its way in before the legitimate overwrite. A freshly constructed Vec's spare capacity is uninitialized, so the first entry-assert reads indeterminate bytes and trips on noise. The MT core's first test run was a false positive until the constructor learned to arm.

Any buffer joining an entry-assert protocol gets `fill_margin` at allocation or reset, not just after the first write. The protocol covers only slack that nobody legitimately touches, so a span the read pump drains into, a bitwriter's deliberate past-end flush inside its reservation, or a caller-live carry zone such as the owned window's committed-length to staged-end span under the pre-split convention cannot be walled without changing that contract, and must be recorded as uncovered instead of silently skipped.
