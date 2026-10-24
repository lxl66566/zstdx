# Encoder edges

This page records the audit of the encoder's emit path and the defects that the `encode_edges` differential suite and the extended `encode` fuzz target found. It answers which emit contracts were checked clean, what the three defects were, and how the fixes were verified. The entropy cores and bit I/O had dedicated reviews, and so did the decode, FFI and CLI surfaces, but the encode emit path had none.

## Scope

The work had two halves. A wire-format audit covered the emit chain. `seq_codes` was checked for its LL and ML code ladders and merged add-bits packing. `frame_header` was checked for the FCS and dictID field classes and the under-256 FCS drop on windowed frames. `block_header` was checked for its 21-bit size caps. `block_enc/compressed.rs` was checked for the literals size formats, FSE table-mode selection, backward-interleaved sequence encode and hot-path reserves. `levels/fastest.rs` was checked for the raw fallback and block-size guard. `frame_compressor.rs` was checked for the single_segment rule, emit split and reach-probe donation. `do_offset_history` was re-verified against `ZSTD_updateRep`, including the repCode==3 pseudo-slot. The audit found nothing. The empirical half was the `encode_edges` differential suite plus the extended `encode` fuzz target, which found three real defects within about 25 minutes of fuzzing.

## Defects fixed

`RowEmit::emit` in `parse_row.rs` could clamp `insert_max` below a match's start. The one-ahead rep probe and the rep continuation both start matches inside the final HASH_READ span, so `end_abs - from` wrapped. Debug builds panicked, and release builds ran the peel and fill loops over about 96 positions whose row hash reads 8 bytes past the window. The chain family's `insert_covered` has had this guard since its own fuzz fix, but the row emit is a separate implementation and missed it. A guard now returns the post-match cursor when `from >= end_abs`, with no fill, because those positions leave no table entries and the scan tail never searches one.

`rep1_row` and `rep1_chain` read past the window at exactly MIN_MATCH bytes of block left. `read4`'s fourth compare byte lies past the window, which is the next block's first byte mid-frame and foreign memory at the frame tail. This was an accepted superset. `rep1_row` additionally row-inserted the probe, and `row_hash_at`'s masked u64 load reads HASH_READ bytes, a full 8-byte overread at unaligned tails. Both now use a bounded 3-byte head compare at the tail, and the row insert is skipped when `pidx + HASH_READ > win.len()`.

The `resize_tables` Row branch in `mod.rs` lost the head-table span. The Row family keys its keep decision on the row count alone, but two row levels share a row count at different hash logs once the source-length clamp shrinks the head table. Level 10's rowLog 5 at srcLog 11 and level 5's rowLog 4 at srcLog 10 both give 128 rows, and at stock logs 7 and 8 against 11 they collide the same way. The kept-rows branch resized `tables` while `second` still named the old span. The per-thread pooled bulk state then carried the stale pair into a later Fast frame, whose heads-kept truncate cannot grow, and `parse_fast` sliced `tables[..second]` past the buffer with `range end index 4096 ... length 2048`. `second` now follows the head span in that branch, and `debug_assert!(second <= tables.len())` guards the resize exit.

All three are reliability bugs. They cause panics or out-of-bounds reads that steer byte-verified candidates, and every emitted frame was decodable before and after, so there was no silent corruption.

## Verification

Each crash input replayed clean under ASAN after the fix. The third defect's artifact passes standalone, which the pitfall below explains, so the in-process sequence pins it instead.

A 10-minute ASAN session on the extended target ran 1,256,670 runs and found nothing. The earlier stop at `crash-6532` was rss-limit creep at 4096 MB, and the artifact passes on replay. Long sessions want `-rss_limit_mb=8192`.

The byte differential compared `dump --all-levels` and `dump --stream --all-levels` between the pre-fix and post-fix builds, and both grids are byte-identical. The tail guards only drop fills and compares that the scan never reuses, and the third fix corrects bookkeeping whose consumers are internal.

The gates were zstdx debug at 407, release at 406, no-default-features at 234 and zstdx-cli at 91, all green.

## Pitfalls

- A crash artifact whose input passes standalone points to pooled state. The per-thread bulk pool carries `tables` and `second` across calls, so the poisoner and the victim are different inputs in one thread. The artifact walker replays each artifact in a fresh process and cannot see this class. Pin it with an in-process call sequence: `encode_edges::pooled_row_geometry` with level 10 at 1500 B, then level 5 at 750 B, then level 1 at 1500 B reproduces the exact 4096/2048 panic before the fix. This follows the same discipline as the earlier pooled-residue work, recorded with the stream-mt link-slot and dist-0 lessons in [pitfalls](../pitfalls/encoding.md).
- The row matcher re-implements every chain fill and emit contract. The chain's tail-margin fixes, namely the HASH_READ fills and the inclusive-`insert_max` empty-range guard, must be mirrored in `RowEmit` and `rep1_row`. Two of the three defects are exactly the row twins of recorded chain pitfalls. Any future fill-path change needs a pass over both families.

## Open items

- The `encode_stream` fuzz target still runs the old option surface. Extending it like `encode`, with the ladder and options through the streaming cores, is the next sweep to run.
- The fuzz ladder compresses payloads of at most 4096 bytes, the libFuzzer default `max_len`. A `-max_len=` campaign or a dict-seeded one would reach the multi-block window and adoption edges that the corpus grid covers only through `dump`.

## Commits

- b7964afb fuzz: encode target walks the level ladder and option surface
- 67442a5b enc: bound row-tail rep reads, guard row emit's clamped span
- e88a0755 enc: keep row head-table span in lockstep on rows-kept resizes
- eb812386 test: encoder edge differential against the reference libzstd
