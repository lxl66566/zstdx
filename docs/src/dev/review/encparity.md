# R68 review — encoder edges (wire audit + fuzz)

Scope: the encode emit path had no dedicated round (R64 covered the entropy cores and bit I/O at the block level, R65-R67 the decode/FFI/CLI surfaces). Two halves: a wire-format audit of the emit chain — `seq_codes` (LL/ML code ladders, merged add-bits packing), `frame_header` (FCS/dictID field classes, the <256 FCS drop on windowed frames), `block_header` (21-bit size caps), `block_enc/compressed.rs` (literals size formats, FSE table-mode selection, backward-interleaved sequence encode, hot-path reserves), `levels/fastest.rs` (raw fallback, block-size guard), `frame_compressor.rs` (single_segment rule, emit split, reach-probe donation) and `do_offset_history` (re-verified ≡ `ZSTD_updateRep`, incl. the repCode==3 pseudo-slot) — all clean, no findings. The empirical half: the `encode_edges` differential suite plus the extended `encode` fuzz target, which found three real defects within ~25 minutes of fuzzing.

## Findings

| # | Site | Finding | Fix |
|---|---|---|---|
| 1 | `RowEmit::emit` (parse_row.rs) | `insert_max` can clamp below a match's start (the one-ahead rep probe and the rep continuation both start matches inside the final HASH_READ span): `end_abs - from` wraps — debug panics, release runs the peel/fill loops over ~96 positions whose row hash reads 8 bytes past the window. The chain family's `insert_covered` had this guard since its own fuzz fix; the row emit is a separate implementation and missed it. | Guard `from >= end_abs` → return the post-match cursor (no fill — those positions leave no table entries, and the scan tail never searches one). 67442a5b |
| 2 | `rep1_row` / `rep1_chain` | At exactly MIN_MATCH bytes of block left, `read4`'s fourth compare byte lies past the window (the next block's first byte mid-frame — accepted-superset; foreign memory at the frame tail). `rep1_row` additionally row-inserted the probe: `row_hash_at`'s masked u64 load reads HASH_READ bytes, a full 8-byte overread at unaligned tails. | Bounded 3-byte head compare at the tail (both); the row insert skipped when `pidx + HASH_READ > win.len()`. 67442a5b |
| 3 | `resize_tables` Row branch (mod.rs) | The Row family keys its keep on the row count alone, but two row levels share a row count at different hash logs once the source-length clamp shrinks the head table (10's rowLog 5 at srcLog 11, 5's rowLog 4 at srcLog 10 — both 128 rows; at stock logs 7/8 vs 11 collide the same way). The kept-rows branch resized `tables` while `second` still named the old span; the per-thread pooled bulk state carried the stale pair into a later Fast frame, whose heads-kept truncate cannot grow, and `parse_fast` sliced `tables[..second]` past the buffer (`range end index 4096 ... length 2048`). | `second` follows the head span in that branch; `debug_assert!(second <= tables.len())` at resize exit. e88a0755 |

All three are reliability bugs (panics or out-of-bounds *reads* steering byte-verified candidates), never silent corruption: every emitted frame was decodable before and after.

## Verification

- Reproducers: each crash input replayed clean under ASAN post-fix; #3's artifact passes standalone (see the pitfall below) and is pinned by the in-process sequence instead.
- Post-fix fuzz: 10-minute ASAN session on the extended target, 1,256,670 runs, zero findings (the earlier stop at `crash-6532` was rss-limit creep at 4096 MB — the artifact passes on replay; long sessions want `-rss_limit_mb=8192`).
- Byte-differential: `dump --all-levels` and `dump --stream --all-levels`, pre-fix vs post-fix builds — both grids byte-identical. The tail guards only drop fills/compares the scan never reuses, and #3 corrects bookkeeping whose consumers are internal.
- Gates: zstdx debug 407 / release 406 / no-default-features 234, zstdx-cli 91 — all green.

## Pitfalls recorded

- **A crash artifact whose input passes standalone indicts pooled state, not the input** (#3): the per-thread bulk pool carries `tables`/`second` across calls, so the poisoner and the victim are *different* inputs in one thread. The artifact walker replays each artifact in a fresh process and cannot see the class — pin it with an in-process call sequence (`encode_edges::pooled_row_geometry`: level 10 @1500 B → level 5 @750 B → level 1 @1500 B reproduces the exact 4096/2048 panic pre-fix). Same discipline as the earlier pooled-residue rounds (the stream-mt link-slot and dist-0 lessons in [pitfalls](../pitfalls/encoding.md)).
- **The row matcher re-implements every chain fill/emit contract**: the chain's tail-margin fixes (HASH_READ fills, the inclusive-`insert_max` empty-range guard) must be mirrored in `RowEmit`/`rep1_row` — two of the three findings are exactly the row twins of recorded chain pitfalls. Any future fill-path change owes a pass over both families.

## Remaining

- The `encode_stream` fuzz target still runs the old option surface; extending it like `encode` (ladder + options through the streaming cores) is the natural next sweep.
- The fuzz ladder compresses ≤4096-byte payloads (libFuzzer default `max_len`); a `--` `-max_len=` campaign or a dict-seeded one would reach the multi-block window/adoption edges the corpus grid covers only via dump.

## Commits

- b7964afb fuzz: encode target walks the level ladder and option surface
- 67442a5b enc: bound row-tail rep reads, guard row emit's clamped span
- e88a0755 enc: keep row head-table span in lockstep on rows-kept resizes
- eb812386 test: encoder edge differential against the reference libzstd
