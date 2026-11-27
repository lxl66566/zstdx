---
name: zstdx-encoder-invariants
description: Invariants and gates for the zstdx encoder hot paths. Read before editing match_generator, stream, ldm, block_enc or frame_compressor, adding search/fill/emit paths, adjusting the level ladder or table geometry, wiring poison walls, or writing pooled-state regression tests.
---

# zstdx encoder invariants

Walk the contracts below before changing an encoding hot path. Every one comes from a real bug, with the full mechanism recorded in docs/src/dev/pitfalls/encoding.md. These are not theoretical rules.

## Cursor and span arithmetic

- Same-domain cursor differences (`end - start` and friends) go through `IdxRange` in `match_generator/range.rs`. Construction asserts the ordering, and `new(lo, hi.max(lo))` yields an empty span instead of a wrap. A naked `a - b` is allowed in two forms only: inside a per-byte scan loop backed by a loop-bar invariant comment (the bar guarantees `cursor <= end - margin`), or with a one-line comment proving the wrap cannot happen.
- `insert_max` is an **inclusive** bound, the last insertable position. Any loop that treats it as an exclusive end must guard with `.max(p)` or an empty-span check first. Every fill stops at `len - HASH_READ`, the shared conservative margin with HASH_READ=8, and a new fill path must not claim a wider bound.
- Distance-resolve u32 table entries must exclude dist 0 through the `dist.wrapping_sub(1) < reach` form. The subtraction has to wrap, because the debug overflow check panics on exactly the dist-0 case it exists to reject.
- Keep the coordinate domains apart. The chain heads' origin/epoch (u64-domain resolve, rolled by `CHAIN_EPOCH_ROLL`), the fast strategy's `fast_shift` epoch cycle, and a job's strip coordinates each form their own domain. Adding a shift or modulus to a domain obliges you to re-derive the proof that its sentinel is dead by arithmetic (the origin sentinel lesson).

## Table state

- `tables`/`second` are written only through the `HeadTables` mutation entries. The single entry per resize shape keeps the pair in lockstep, and the R68 desync bug came from exactly this. A new resize shape means a new entry method, not a new field.
- Ladder and table geometry edits are caught at **compile time** by the `const _` grids in `params.rs`: the full level × src_log mesh plus `LADDER_ROW_COLLISIONS == 132` as the pair collision count. When a ladder change fails the build, work out the geometry first instead of bending the assert to fit.
- Every search exit owns the same postcondition: the searched position is inserted and `next_update` is past it. A new early exit must restate it by writing the fill-form entry, and cannot rely on the next fill to finish the job. Every chain-family head insert carries a chain link, because the walk induction depends on it.
- The repcode `off_base` values 1/2/3 each name a different decode action under ll==0, and must not be folded into "any rep counts as 1".
- Avoid magic zero sentinels such as a len-0 candidate. Use an explicit `Option` or enum. Arithmetic that happens to be right for len 0 in release is not a contract.

## Views and pooling

- Job view indices anchor at `strip_lo`, the convention every posted job and the inline path share. R69 was an inline anchor mistake that silently corrupted output. When adding an inline twin of a worker path, build the same view the worker gets.
- The post and gate boundary functions and the stepping must share the same clamp ("a job may not cross an epoch"), not only the arithmetic.
- Pooled state (the thread-local bulk pool, the stream core's cross-frame residue) is a real input dimension that fresh-process dump and fuzz runs cannot see. Regression tests covering pooling must be in-process sequences: `pooled_row_geometry`, `pooled_ladder_sweep`, and the artifact walker's forward and reverse passes.

## Poison wall protocol (debug detection layer)

- Contract: the producer calls `fill_margin(buf, len)` on exit, and the consumer calls `assert_margin(buf, len, ctx)` on entry before it legally overwrites. The lifetime start (allocation, reset, construction) must fill first, because a fresh Vec's spare capacity is uninitialized and the first entry assert would fire a false positive.
- Wall only the slack that nobody may legally touch. The known untouchable regions are on record, so do not retry them: the MT accumulate buffer (the read pump's documented destination), the reserved segment inside bitwriter `hot_push_raw` that is overwritten on purpose, the owned window's committed-len to staged-end span (the pre-split carry convention, caller-live), exactly sized tables, and output borrowed from the caller.
- Wire the walls like `stream/encoder_core.rs` (12 cfg blocks) and `ldm.rs`. Everything sits under `#[cfg(debug_assertions)]` and costs zero code in release.

## Gates after a change

1. `cargo test -p zstdx` with every wall on, plus `--no-default-features` and `--release`.
2. `cargo test --profile paranoid -p zstdx`. This is release codegen with every wall, and the wrap classes only surface under release optimization.
3. Semantics-preserving refactors: `diff -r` of `dump --all-levels` and `dump --stream --all-levels` against the baseline must be empty (corpus: bench/corpus).
4. Refactors of a hot loop in the same file: bisect gungraun Ir **hunk by hunk**. Placement effects do not add up linearly, and even an unexecuted loop body can move ±3% Ir. Keep the loop body's instruction shape stable and put the guards outside the loop.
5. Pooling or boundary fixes: feed the multi-payload fuzz targets for `encode` and `encode_stream` (layout `[level, opts, k, chunks]`, chunk i at `(level+i)%23`). For a smoke run use `crates/zstdx-fuzz/smoke.sh <target> <seconds>` (saturates every core, splits the replay across workers, QUICK=1 skips the full replay; see that directory's Readme).

## Further reading

- docs/src/dev/pitfalls/encoding.md: the full mechanism behind each bug. Skim the Memory safety section before editing.
- docs/src/dev/review/hardening.md: the coverage of each R69 wall layer and its known blind spots.
- docs/src/dev/review/encparity.md: the root-cause chain of the three R68 bugs.
