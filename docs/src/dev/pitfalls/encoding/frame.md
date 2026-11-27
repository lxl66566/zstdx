# Frame format
## Resolve header fields once


The stream-MT capture engagement re-serializes the frame header, and its Dictionary_ID arm read the dictionary directly through `dict.header_id()` instead of the construction-time resolution. `include_dictid(false)` held on the initial header and was silently re-stamped on engagement for a dictionary fast-row stream in the capture band. Default frames never moved, which is why the earlier suites stayed green.

The id now resolves once at core construction and every serialization stamps that field. When a header or any other frame-observable value derives from an option, find every serialization of it, not only the constructor's, because engagement and re-grid paths re-emit what the constructor emitted.
## Multi-block header ownership


The per-block emission contract reserves one 3-byte header per block for the caller to patch. Any path that emits more than one block must take over header ownership explicitly.

The block splitter hit this twice in bring-up. Later partitions computed their header offset as `len - 3` before appending their own reserved bytes, which patched into the previous partition's tail. Worse, the caller then re-patched the first reserved header with the summed size, producing a super-block header that swallowed the partitions' own headers into its bitstream.

Our own decoder roundtripped it permissively while libzstd flagged corruption. Interop through the zstd crate is the mandatory gate for any multi-block-per-parse path. A `SplitOutcome::{Emitted, Single}` return now marks that the headers are already final and size-guarded, so the caller skips the patch and the check.
## Window-capped block size


RFC 8878 sets Block_Maximum_Size to `min(window, 128K)`. The srcSize adjustment can never trigger this, because the window is at least ceil_log2(src) and therefore at least src, so only a forced window log below 128K exposed it. Libzstd rejected even all-raw frames with "Data corruption detected" and error 36, while our permissive decoder accepted them.

Every emission path now reads `Matcher::block_size`, and any new window-shaping knob needs a grep of `MAX_BLOCK_SIZE` in the emission paths first. The pre-split detector's decision window re-hit this. It sized its window by the format's 128K instead of `block_size()`, so tiny-window frames carried over-window blocks and our decoder reported `BlockOutputTooLarge`. A decision input is not exempt from the emission cap.

The read-side `FrameCompressor<M>` loop re-hit it a third way. `block_size` existed as an inherent method on `MatchGeneratorDriver` and the trait impl never overrode it, so the generic loop resolved the `Matcher` default of 128K while every concrete-typed caller got the clamp. A method that exists as both an inherent twin and a trait default must be overridden in the trait impl. Otherwise generic contexts silently keep the default.
## Staged-tail anchor


The Read-path pre-split decision asked the matcher for the staged span through `win[idx_of(block_start)..+read]`, but `commit_block` leaves `block_start` pointing at the block it just committed. The staged bytes sit one block later, so every decision after a block read the previous block's bytes.

Output stayed valid and boundaries were silently one block behind. The slice-versus-Read-stream identity test caught it only on inputs whose neighboring blocks decide differently. Any matcher helper over the fill in progress must use `win.len() - slice_size`, not the committed `block_start`.
## Pre-split carry across window compaction


The Read-path pre-split leftover once stayed physically in the matcher's uncommitted tail, and the next fill read its fresh bytes in behind it. That zero-copy contract only holds while the `block_tail` reservation leaves the window in place: past `2 * window + slice_size` committed bytes the reservation compacts the owned window, and the compaction's `copy_within` moves committed bytes only. The block staged right after a compaction spliced stale pre-compaction window bytes in front of its fresh ones. The frame stayed structurally valid with a self-consistent checksum over the wrong payload, so no decoder-side error can catch this class; only payload comparison can.

The carry is encoder state and rides an encoder-owned buffer across the reservation, the same ownership the stream core's `staged` keeps. The invariant: nothing survives a `block_tail` reservation in the window's uncommitted tail. It needed a frame window below ~1.5 MiB plus input past the compaction trigger to surface (first firing at 1703936 committed bytes on the wl18 level-5 row), so the corpus-sized tests never crossed it.
## Wire-flag size classes


The FCS flag mapping read `3 => 8` where `find_min_size` yields 1, 2, 4 and 8, and the intended arm was `8 => 3`. Every FCS at 2^32 or above hit the panic arm, which covers MT's fixed `frame_content_size: Some(src.len())` for inputs above 4 GiB-1 and a stream pledged at 2^32 or above. All smaller classes behaved, so every ordinary test passed.

Spec-table mappings need boundary tests per class at 255, 256, 64K, 4G-1 and 4G, which pin the descriptor flag, the field width and the decode roundtrip at once. The Dictionary_ID flag had the same shape, where its `_ => panic!()` fired for ids above `u32::MAX` on the publicly constructible `FrameHeader`. The field had no wider wire class at all, so the right fix was the type: `dictionary_id: Option<u32>`, matching the decoder side, makes the panic arm unreachable by construction. A wire field with a finite width belongs in the field's type, not in a match arm's reject case.
## Word loads follow the format's byte order


A bare unaligned `read_unaligned`/`write_unaligned` compiles to the target's native word order, and the s390x cross job turned every such load that feeds the frame into a byte-reversed one. The backward bitstreams consumed words through the decoder reload macros, so Huffman and FSE decode read garbage; the checksum loops absorbed native words, so every frame failed `ChecksumMismatch` on both our decoder and the reference CLI; the Huffman X2 pair store reversed its two symbols; the sequence emitter's literal fast path copied bytes through an LE store of a native load, which reordered each 8-byte literal group.

The fix routes each such primitive through `bit_io`'s little-endian helpers, because the byte order of these words belongs to the format, not to the target: the bitstreams and the checksums are defined low-byte-first on every platform. Two pairing rules keep the remaining paths honest. A byte-preserving copy pairs an LE load with an LE store; a native load pairs with a native store, and mixing the two reverses bytes on big-endian. A byte-index compare over a native load's xor follows the load's byte order, so `first_diff_byte` scans from the low byte on little-endian and the high byte on big-endian.

The diagnosis tool worth keeping: an env-gated check inside `push_seq_packed` that re-derives each sequence's effective offset from the rep history and byte-compares the claimed match span against the window. It printed the exact invalid sequences and their first differing byte while the frame still decoded to almost-correct output, which pinned the rep-gate bug below in one run.
