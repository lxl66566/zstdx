# Streaming and multi-frame semantics
## Streaming window semantics use a generation model


Libzstd streaming never compares an offset against the declared window. The per-sequence rule is `offset > oLitEnd - virtualStart`, which tests the generation, and the ring geometry is tuned so the two almost always coincide. The cap is the window floored at 1 KiB plus 2*blockMax plus 2*WILDCOPY, capped by the FCS, and the ring wraps when `outStart + blockMax > cap`.

The two rules differ at the edges in both directions. `fa_w16_o100000`, with an offset past the 65536 window but inside the 180009-byte generation, accepts under streaming, while an in-window offset flips to reject once the origin wraps and the previous generation is dropped. A single-origin simplification of the model wrongly rejects `ctrl_w10_o900`, whose offset 900 is below the 1024 window.

Never collapse the generation bookkeeping into a window-distance check, and pin decode tests per path. One-shot decode is whole-history by design, and C's `ZSTD_decompress` accepts any in-history offset, while streaming is generational, so the two disagree by design on the same frame.

A raw dictionary is a virtual generation-0 prefix that expires at the first wrap, with reach equal to dictSize plus the bytes produced until then. The semantics map and probe table are in [winsem](../../review/winsem.md).
## Folded bases and virtual addresses


The flat executor folds the generation floor into its per-block bases, `folded_base = virt_base - prev_origin` and `virtual_start` derived from it, so the floor never rides the loop as a live value. One helper once reconstructed `virt_base` from the folded base without un-folding, which equals `virt_base - prev_origin`. From the second wrap on, every previous-generation address was shifted, producing wrong bytes, a bogus `OffsetTooBig`, or a debug underflow, with four different test failures from one root cause.

When a base carries a folded term, every consumer that rebuilds an address from it must un-fold through the same view. Structural values such as the 5-word `FlatView` never enter the fused loop by value, and per-block scalars are derived in the prologue instead.
## Multi-frame agreement


After the first frame's `is_finished`, `StreamingDecoder::read` returned `Ok(0)` while `decode_all` looped over all frames, so the same multi-frame input gave different results on the two paths. A new cross-check assertion in the fuzz decode target caught it, because the old target only called `read_to_end`.

The semantics follow libzstd and the zstd crate: both paths decode all frames, skip skippable frames transparently, error on any trailing garbage of 1 byte or more including a 1 to 3-byte partial magic, and never treat an empty content frame as EOF.

The fix pushes the frame-tail continuation primitive, which pre-reads 4 magic bytes to distinguish clean EOF from a partial tail, into `decoding::frame_source`, shared by `stream::read::Decoder` and `StreamingDecoder`. The continuation check must live inside the read fill loop, because when an empty frame ends the current read, `can_collect == 0` falsely reports EOF.

That crash's second frame had FHD=0x38 setting a reserved bit, and the header-strictness divergence it exposed is closed under Divergences.
## Next-frame hunt keeps peeked bytes


`start_frame` built a fresh `PrefixedReader` per attempt, so any `Err` after the magic was consumed, most naturally `WouldBlock` on a non-blocking socket, dropped the staged bytes with the local. The retry then read the payload as a magic number, the stream was permanently misaligned with `BadMagicNumber`, and a transient error became permanent corruption.

The hunt state now rides on the `FrameDecoder` through `NextFrameStaging`, so staged header bytes replay on retry. A failed skippable-frame skip keeps the remaining-content count and resumes the discard, and skippable content is never staged because it is unbounded. The reader pulls at most the magic into the stage, because anything over-read past the header would be lost when the hunt ends, since block decoding reads from the raw source.

Follow-up: staging is stream-scoped state on a reusable decoder, so a decoder abandoned mid-hunt and rebound to a new source replayed the stale magic into the new stream's head and was misparsed as "Reserved block occured". `init_first_frame` and `FrameDecoder::reset` now drop the staging, and the hunt itself is unaffected because `start_frame` lifts the staging out for each attempt before any reset runs.
## A skippable frame is a frame for EOF purposes


A skippable-only stream died at `StreamingDecoder::new` with `MagicNumberReadError(UnexpectedEof)`: the hunt consumed the skippable frames, probed the next magic, and treated the clean end of the source as a truncation because the position marker still said "no data frame decoded yet". The reference CLI decodes a skippable-only input to empty output with exit 0, while the empty input stays an error ("unexpected end of file"), so the marker must track whether any frame at all was consumed, not whether a data frame was. Pinned by `tests/decode_edges.rs::skippable_only_stream_decodes_to_nothing`.
