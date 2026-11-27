# MT decoding
## One condvar pairs with one mutex


`StageAJob`'s single `ready` condvar was waited with each slot's own mutex by the consumer and the lanes, and with the poison mutex by the backpressure and lane-poll sleepers, and those waits overlap in real runs (decpool finding 2). POSIX requires one condvar to be waited on with exactly one mutex. glibc tracks no mutex identity, so Linux ran green, while macOS pthread aborts from inside `Condvar::wait` on the second mutex, which is the macOS-only CI failure this class produced. The encode-side bulk pool carried the same shape with four waiter classes (see the encoding page's entry under Checksums and streaming).

Every waited mutex now carries its own condvar through `common::monitor::Monitor`: each slot is a monitor, and the poison mutex's own condvar is the sleep token. A fill notifies the filled slot's monitor plus the poison monitor (the lane polls watch the `seg_sizes` atomics it publishes), the consumer's advance notifies the poison monitor alone, and a poison publish or an abort broadcasts to every monitor so each slot waiter and each poller re-checks and drains. Lane results pair the same way inside `LaneJob`.
## The write limit binds every block arm


The frame lanes hand the caller's buffer bound to `OutRegion::write_limit`, but only the sequence arms consulted it: the Raw and Rle arms copied unconditionally, so a short target on a multi-frame MT decode wrote up to a whole block past the caller's slice and returned `Ok` with a passing checksum. The bound belongs at the executor entry, checked once against the whole block range's output before the first store, so every arm and every executor (lanes, pieces, serial) rejects with the sequential decoder's `TargetTooSmall` instead.
## Piece grids must be fully cut


The incremental piece-cutting loop had no `n_pieces` bound, so after the last piece, where `end` is clamped to the FCS, `end` stayed at the FCS forever. The loop spun pushing empty pieces while holding the driver mutex, and every thread then contended at 0% CPU.

`try_settle` also treated "validated equals currently-cut pieces" as a complete round, so a grid settled mid-staging with its uncut tail's `max_top` all zero. The watermark gate opened, and bytes came out wrong with a checksum mismatch. The fix bounds the loop and requires a grid to be fully cut before it can win.

- Verdict states other than `winner`, such as all-dead and poison or abort, never notified the caller condvar. A plain frame, where all grids fail the lag gate, deadlocked with every thread parked. The fix notifies for each state that satisfies the caller's predicate.
- A `MutexGuard` in a let-else scrutinee lives through the else branch, so re-locking the same mutex there self-deadlocks. Bind the copied value to a plain local first.
## Track the doubled value


`decode_to_vec_mt`'s parallel path grows segment by segment, while the serial fallback reports `TargetTooSmall` on an empty Vec. The doubling retry must track the doubled value, because `reserve(cap)` is a no-op when the spare capacity is at least `cap` and the loop otherwise runs forever.

During debugging this was once misdiagnosed as the flat decoder looping, when the retry loop was re-running the full decode every round.
## MT literals count the delta


ST clears per block, so nothing happened there, but MT stage A accumulates per segment, and every second or later Huffman literals block within a segment was falsely reported. The final validation used the buffer's total length instead of the delta.

Seven of 11 corpus files failed MT decode while the repo tests stayed green, because the textish corpus passed by luck with Raw and RLE literals. The fix uses the delta. The shape of the repo test corpora can systematically mask a class of bugs, and a semi-structured regression test now covers it.

- A symbol with power-of-two probability can yield `nb_d = acc+1` while having no double state and never being used. The assert should read `num_double == 0 || nb_d <= acc`, and the first version was reverted and redone because of this.
