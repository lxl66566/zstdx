# Stage-A worker pool review

Scope: commit `f2df78ca`, "dec: lease stage-A workers from a parked pool across MT decode calls", which added `crates/zstdx/src/decoding/mt_pool.rs` and rewrote `decode_parallel`, `StageAJob`, `serve_stagea` and the `LeaseOut` guard in `crates/zstdx/src/decoding/mt.rs`. Companion landings from the same window are covered in [decloop](decloop.md) round 2. This was a review-only round: no library code changed. `cargo nextest run -p zstdx` is green on the branch, 494 passed.

## Method

`mt_pool.rs` was read end to end and every lease, park, retire and wake transition was enumerated against the two mutexes it uses, the pool mutex and the per-slot state mutex. The lease protocol was then traced through `decode_parallel`, `serve_stagea`, `LeaseOut` and both public entries, including the spawn-failure, caller-panic and worker-panic paths. The scope-based driver at the commit's parent was read side by side, because the rewrite's contract is to preserve that driver's guarantees.

## Safety verdict: sound, one liveness regression

The frozen-borrow lifetime, the drop guard and the retirement protocol are sound on every path that was traced. One lost wakeup on the stage-A panic publish is a regression against the scope-based driver and needs immediate follow-up. It hangs a decode, so it is a liveness bug, not memory unsafety.

### Frozen borrows and `unsafe Send/Sync`

`StageAJob` carries raw pointers to the caller's input slice and the scan plan's segments (`mt.rs:1061`). A worker dereferences them only inside `serve_stagea`, and only after `wait_lease` returned the job, which requires the slot state to be `Serving`. The state leaves `Serving` exclusively through `leave_lease` under the slot state mutex, after `serve_stagea` returned. `wait_leave` (`mt_pool.rs:114`) returns only when the state is no longer `Serving` that exact job, so the guard's return implies every worker's last deref happened before it. The slot state mutex is the happens-before edge that carries the `unsafe impl Send/Sync` at `mt.rs:1080`.

A recycled slot serving a later decode is handled by the pointer comparison inside `wait_leave`'s predicate, and the job `Arc` a worker cloned is only reference-counted after `leave_lease`, never dereferenced.

### The drop guard on every exit path

`LeaseOut::drop` (`mt.rs:1165`) stores `abort`, notifies `ready`, then waits every lease out. Every exit path reaches it. The closure's early returns flow into the captured result before the explicit `drop(out_guard)`. A caller panic unwinds through the guard before the parameter borrows end. A worker panic fills the ordered slot with an error so the consumer terminates and reaches the guard. `lease_workers` panics before the guard exists on a spawn failure, but it marks every secured slot `Retiring` first (`mt_pool.rs:127`), and a retired thread exits without touching the never-assigned job. `wait_leave` polls on a 100 ms bounded wait, so a lost notify delays it but cannot hang it.

The poison payload is taken after the guard, so a decode that both succeeded and carried a later worker panic cannot occur: the consumer waits for every segment slot in order, and poison is only published for a claimed segment, so a successful close implies an empty poison cell.

### Retirement of a panicked thread

`serve_stagea` catches job panics around `decode_segment`, drops the possibly mid-decode scratch, fills the slot, and returns the retire flag. The thread then leaves the lease as `Retiring` and exits, and a thread that has seen an unwind is never reused, matching the encoder pool's rule. A panic in the lease machinery itself is caught one level up in `pool_thread`, which withdraws the slot from the pool, sets `Retiring`, and notifies before resuming, so no waiter blocks on a dead thread. A second panicking worker overwrites the first payload; one panic resumes and the other is dropped silently, which loses a message, never soundness.

### Idle retirement racing a new lease

A parked thread that times out drops the state lock, then removes itself from the pool under the pool lock. A leasing caller pops under the same pool lock, so exactly one of the two wins. If the caller wins, the thread's `unpark_self` finds nothing, re-locks the state, sees the lease already assigned or imminent, and serves it. If the thread wins, the caller can never pop the slot. The state re-check after a failed withdrawal closes the window where the assignment lands between the two locks. No interleaving lets a lease land on a retiring thread, which also upholds `assign_lease`'s `Parked` debug assertion.

### Stale jobs after park

The `Serving` state is only written by `assign_lease` under the state mutex, by a caller that exclusively owns the slot through a pool pop or a fresh spawn, and a fresh spawn's first `wait_lease` checks the state under the same mutex before sleeping. A worker therefore wakes into exactly the lease the state carries, and a parked worker holds no job reference at all.

### The small-input cutoff

`engage` (`mt.rs:1276`) requires two or more requested workers, two or more available cores, and either a 512 KiB input or a pledged frame in the piece regime. Below that the public entries fall back to the sequential `FrameDecoder`, so the pool is never leased for small inputs and an accidental small decode cannot strand leases. The piece executor still runs `thread::scope` and was untouched by this commit.

## Findings

1. High, needs immediate follow-up. Lost wakeup on the stage-A panic publish. `serve_stagea`'s panic arm at `mt.rs:1139` fills `slots[id]` and returns at `mt.rs:1149` without `job.ready.notify_all()`, which only runs on the `Ok` arm at `mt.rs:1152`. The consumer's in-order wait at `mt.rs:1219` is an unbounded `Condvar::wait`. The breaking interleaving: the consumer parks waiting for slot `id`, the worker staging `id` panics and fills the slot without notifying, and every other worker has already exhausted its claims or returned at the loop-top poison check, so no later `Ok` completion notifies either. The consumer sleeps forever, workers parked in backpressure keep polling on their 50 ms timeout, and the decode never returns. The scope-based driver this commit replaced notified after every claim iteration on both outcomes, so this is a regression, and the panic arm's comment, which says the slot is released so the ordered consumer can run to completion, is now false in this window. The injected-panic test misses the window because its four workers on a many-segment frame almost always deliver an `Ok` notification after the consumer parks. The fix is to move the notify above the match, restoring the old placement. Reaching this path in release needs a worker panic, which means an invariant failure inside `decode_segment`; the injection hooks exist precisely to exercise that class, and the pool's stated guarantee is to survive it.
2. Low, pre-existing, next-round. One `Condvar` driven by many mutexes. `job.ready` (`mt.rs:1068`) is waited with each `slots[id]` mutex (`mt.rs:1219`) and with the poison mutex (`mt.rs:1119`), and those waits are concurrent, because backpressured workers sleep on the poison lock while the consumer waits on a slot lock. std documents `Condvar::wait` as allowed to panic when used with more than one mutex over time, and the pthread-backed std implementation used on macOS panics outright with "attempted a condition variable with two mutexes". The Linux futex and Windows implementations do not track mutex identity, so the current targets run correctly. The scope-based driver had the same pairing, so this commit inherited rather than introduced it. A single job-state mutex carrying the slots, the poison cell and the consumed counter would remove the ambiguity, the same shape `mt_pieces`' driver already uses with its one driver mutex. The backpressure predicate could then also observe the poison cell, which it omits today, leaving backpressured workers polling after a panic until abort fires.
3. Informational. `serve_stagea` loads `abort` with `Relaxed`. The memory-safety chain does not depend on that load, because `leave_lease` under the slot mutex provides the ordering that ends the frozen borrows, and the flag only steers liveness, which the notify and the bounded waits cover.

## Verified clean

- The pool mutex and a slot state mutex are never held together anywhere in `mt_pool.rs`, so the park, assign, repark and retire paths cannot deadlock.
- The per-slot `wake` condvar is only ever paired with that slot's own state mutex, so finding 2 does not apply to it.
- `wait_lease`'s timeout withdrawal, `wait_leave`'s bounded predicate re-check and the backpressure 50 ms poll each survive a lost notify by construction.
- Two concurrent `decode_parallel` calls build separate jobs and lease disjoint slots, because every pool pop and every retirement runs under the pool lock.
- `lease_workers`' fresh spawns are assigned only after every slot exists, and a spawn failure retires the secured slots before the panic, so no orphan lease outlives the call.
