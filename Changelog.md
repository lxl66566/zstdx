# Changelog

This document records the changes made between versions, starting with version 0.5.0

# After 0.9.0 (Current)

* The scan loop interleaves two adjacent positions (libzstd's ip0/ip1
  pipeline): hashes and table entries for both are prepared before either
  is probed, overlapping the hash multiply and table load latencies, and a
  fully-missed pair advances by twice the miss step so probe density on
  incompressible data is unchanged. json gains 2% throughput and 6%
  ratio (5.67 -> 6.02, near zstd -1's 6.11) because the pair-step skips
  over short-match starts the way the ml>=6 gate does; text gains 2%
  throughput at -1.3% ratio; other corpora are unchanged.

* The bit writer's 64-bit flush stores one unaligned u64 into the reserved
  output vector instead of calling memcpy for eight bytes, removing a call
  per flushed container from every entropy-coded block. Output is
  bit-identical; json/text/skewed gain 1-2% each.

* The uniform-block detector compares four u64 words per branch instead of
  one, so fully-uniform blocks (zero-filled inputs, padded corpus tails)
  stop paying a branchy scan of the whole block while non-uniform blocks
  still exit after the first batch. zeros +7%, text +1%, other corpora
  neutral.

* Encoder round eight: sequence codes and their add-bit payloads are
  precomputed in a single pass over the sequences (codes packed into one
  u32 stream, add bits pre-merged into one u64 per sequence), replacing the
  three separate code arrays, the per-sequence out-of-line encoder-helper
  calls, and the metadata re-lookups in the bitstream encoder. json +3%
  throughput, all other corpora neutral, output bit-identical.

* Encoder round seven, data-path focused: the match window holds two windows
  plus one block of capacity so compaction copies ~1x data volume instead of
  once per block; block input is read directly into the window tail through
  the new `Matcher::block_tail`/`commit_block` API (replacing the
  `get_next_space`/`commit_space` buffer ping-pong and its pooled-buffer
  zero-fill); sequence FSE tables build from flat arrays (probability list,
  start-state list, packed transition table) instead of 256 per-symbol
  vectors with two sorts each; the scan loop keeps its cursors in locals
  across emissions; and tiny literal runs copy with one unaligned u64 while
  the position hash loads the full u64 masked to five bytes (mathematically
  identical hash values, fewer instructions). text 2.5 -> 3.8 GiB/s, zeros
  4.5 -> 8.2 GiB/s, json 287 -> 334 MB/s, ratios bit-identical.
* Sequence codes are computed once per block and shared between the FSE
  table selection and the bitstream encoder, uniform literal sections
  encode with the one-byte RLE literals mode, and a cheap entropy-bound
  check skips Huffman attempts on near-incompressible literals instead of
  encoding them and discarding the result (random-bytes encode 2.4x
  faster, ratios unchanged).
* The match window widens from 448 KiB to 768 KiB so repository-tile-sized
  repetition periods stay matchable; hash matches now require 6 bytes (a
  5-byte match's sequence overhead roughly equals the literals it covers,
  and rejecting it lets the scan find the longer match that starts next);
  the hash table shrinks to 2^15 heavily-contended slots with half-rate
  miss-path inserts (newest-wins then prefers close, cheap-to-encode
  offsets); and sequence FSE tables gain libzstd's fast-strategy mode
  selection (RLE for single-code blocks, predefined below the dynamic-table
  break-even, otherwise a normalized table built with the ported
  FSE_normalizeCount + optimalTableLog and the last-sequence count
  discount). json ratio 5.22 -> 5.70 at +27% throughput, skewed ratio
  2.00 (= zstd -1) at +81%, text ratio 6.56 -> 239 where the wider window
  unlocks cross-tile matches.
* RLE block detection compares 8 bytes at a time (libzstd `ZSTD_isRLE`
  style) instead of a per-byte closure over an indexed first element, and
  skipped (RLE) blocks index only their first position instead of every
  byte — a uniform run hashes to one table slot, so per-byte indexing just
  rewrote it. zeros encode 7.3x faster.
* The encoder's block emit path is reworked to keep the hot scan loop free
  of allocator traffic and register spills: entropy tables are taken out of
  the compressor state by value for the duration of a block (a raw-block
  fallback puts them back) instead of deep-cloning three FSE tables plus
  the Huffman table per block; the built-in matcher gained a
  `start_matching_into` buffer sink (default-implemented on the `Matcher`
  trait, so custom matchers are unaffected) that appends literals and
  sequences directly instead of routing every emission through a closure
  capture; and the matcher's window reads (hash input, 4-byte probes,
  u64 match extension) are unchecked with caller-guaranteed bounds.
  json +24%, text +39%, skewed +6% end-to-end encode throughput.
* The encoder's match emitter indexes covered matches sparsely instead of
  hashing every byte: matches up to 16 bytes keep every position (they carry
  most of the alignment coverage on structured data), longer matches fall
  back to a 4-byte grid anchored at the match start plus the final byte.
  json +6% / text +5% encode throughput at -0.2% / -0.6% ratio.
* Sequence decoding extracts each group of bitstream reads (the three
  add-bit fields and the three FSE state transitions) with a single window
  read whose bits are then split in parallel, instead of six serial
  load-shift-store chains per sequence. The mid-sequence reload guard only
  applies to the rare wide-field path (all three add-bit widths sum above
  31). skewed +5-8% (zstd-3/zstd-9), json.zst3 +3-5%, text.zst1 +2-3%,
  rest neutral.
* Huffman literals decoding gains a double-symbol (X2) table for the
  interleaved 4-stream fast path, ported from libzstd: each lookup emits one
  or two literals (a single unaligned u16 store) and consumes the summed bit
  count, halving the serial load-shift chain on skewed distributions. The
  table is chosen per literals section with libzstd's size-based cost model
  and only kept when at least ~80% of the code space can pair two codes into
  the 11-bit lookup window (pairing pays off on skewed tables; on
  long-code tables the wider entries and variable advance lose to the plain
  single-symbol loop). skewed ~+15%/+43% (zstd-3/zstd-1), json +1-4%,
  text +0-2%.
* FSE decoding-table construction precomputes per-symbol spread constants
  (slice baselines/strides/bit counts) so the per-entry fill is a counter,
  compare, and multiply-add; the per-entry `highest_bit_set` scans and the
  division move to a once-per-symbol pass.
* The decoder only computes the xxhash checksum when the frame actually carries
  one (`Content_Checksum` flag set), instead of always hashing every drained byte.
* Sequence decoding reworked into a libzstd-style 64-bit backwards bit reader
  with packed single-load FSE tables; sequences decode ~15% faster.
* Sequence execution reserves the whole block's output up front and appends
  without per-sequence capacity checks; ring buffer wraps with a conditional
  subtract instead of a modulo.
* Interleaved Huffman decoding accesses its tables and output through
  unchecked reads/writes; the loop bounds already guarantee they are in range.
* Fix encoder panics on degenerate single-symbol FSE distributions: the table
  keeps the full weight for the lone symbol instead of redistributing to a
  nonexistent second maximum, and trailing zero probabilities no longer read
  past the end of the symbol array when writing the table description.
* FSE sequence encoding switches states through a flat per-symbol transition
  table instead of a linear scan, and literal/match length codes come from
  constant lookup tables for the dense low ranges.
* The matcher is rewritten as a zstd-fast style single-probe hash matcher over
  one contiguous window: newest-wins hash insertion, u64 chunked forward and
  backward match extension, and escalating probe steps on literal runs.
  Roughly 3x faster matching with better ratios on structured data.
* The matcher now emits repcode sequences: matches at the current repeated
  offset are encoded as offset code 1 instead of a full offset, and a raw-block
  fallback rolls the repeated-offset history back to match the decoder.
* A raw-block fallback now also rolls back the reusable Huffman and FSE tables:
  the decoder never sees the discarded block, so a later block must not reference
  entropy tables only introduced by it.
* Matcher parameters retuned: 448 KiB window and a steeper probe-step ramp on
  literal runs. Incompressible and skewed data compress up to twice as fast with
  slightly better ratios.
* Sequence encoding concatenates the three state-transition bit groups and the
  three extra-bit groups into one bit write each, halving the writer calls in
  the per-sequence loop.
* `StreamingDecoder`'s `read` decodes until the caller's buffer can be filled
  instead of stopping at the first collectible byte, batching block decodes
  under large reads.
* The sequence decode loop carries its FSE tables as raw pointers, caches each
  table entry between the symbol read and the state transition, and writes
  sequences through a raw pointer into pre-reserved capacity, cutting spills
  and redundant loads per sequence.
* Sequence decoding dispatches to a BMI2-compiled copy of its loop at runtime
  when the CPU supports it (x86-64 + std), turning the variable bit shifts
  into single-uop shlx/shrx.
* `decode_all` executes blocks straight into the caller's buffer when no
  dictionary is attached, bypassing the ring buffer and its drain copies
  entirely (flat output path). Slice decoding speeds up 13-35% depending on
  shape; the ring-buffer path remains for dictionaries and streaming.
* The corruption smoke example also fuzzes the flat `decode_all` path and no
  longer panics itself when corruption hits the frame magic (a legitimate
  header error).
* The matcher probes the second repeated offset immediately after every
  emitted match (zstd fast's rep_offset2 loop); alternating-period data now
  chains repcode matches with zero literals (json ratio +6%).
* Dictionary-free streaming decode executes blocks into a flat windowed
  buffer (libzstd's outBuff model) instead of the ring buffer: blocks decode
  straight into the buffer, flushes hand out bytes without retaining a
  window, and a full buffer wraps to its start with the previous segment's
  tail serving as the match window. Streaming speeds up 3% (small windows)
  to 125% (8MB windows); on large-window data ruzstd now matches or beats
  the zstd crate's streaming decoder.
* The sequence decode loop reads its bitstream through a pre-shifted window
  kept in a register (two dependent shifts per read instead of a
  consumed-counter shift chain), and the three packed FSE tables live in one
  fixed-slot array addressed through a single base pointer with constant
  offsets, removing the per-iteration table-pointer reloads and bit-container
  memory operands from the loop.
* Sequence decoding and flat-path sequence execution are fused into one loop
  (the libzstd model): each sequence is executed the moment it is decoded
  instead of round-tripping it through the sequence vector. RLE streams now
  decode through a one-state fake table packed like any FSE table, so the
  loop carries no RLE branches at all and the separate RLE/non-RLE decode
  loops collapse into one `SeqDecoder`. Slice decoding of sequence-heavy
  data speeds up 5-10% (json +10% at level 1, where ruzstd now decodes
  faster than the zstd crate's slice decoder).

# After 0.8.3

* Avoid emitting compressed blocks when the compressed payload is not smaller
  than the raw block.
* Fix Dictionary decoding. It should not panic on invalid inputs.
* Make the decode window size limit configurable via `FrameDecoder::set_max_window_size`/`max_window_size`, `StreamingDecoder::new_with_max_window_size`, and the `DEFAULT_MAX_WINDOW_SIZE` constant. The default stays 100mb.
* Apply the window size limit to the first frame of a stream, not just later frames.
* **Breaking** `FrameDecoderError::WindowSizeTooBig` gained a `max` field and now reports the effective limit.

# After 0.8.2
* Introduce the `rust-version` field
* Fix checksum generation when repeatedly using the encoder
* Expose decoding::Dictionary as public
* Add Debug derive to CompressionLevel enum
* Make RLE and Raw block decoding more efficient and not use intermediary buffer on the stack

# After 0.8.1
* The CLI has been refactored to use `clap`
* The MatchDriverGenerator has been made public so users can name it as `M` in `FrameCompressor<R,W,M>`

# After 0.8.0
* The compressor now includes a `content_checksum` when the `hash` feature is enabled
* Dictionary generation has been added

# After 0.7.3
* Add initial compression support
* **Breaking** Refactor modules to reflect that this is now also a compression library

# After 0.7.2
* Soundness fix in decoding::RingBuffer. The lengths of the diferent regions where sometimes calculated wrongly, resulting in reads of heap memory not belonging to that ringbuffer
    * Fixed by https://github.com/paolobarbolini
    * Affected versions: 0.7.0 up to and including 0.7.2

* Added convenience functions to FrameDecoder to decode multiple frames from a buffer (https://github.com/philipc)

# After 0.7.1

* Remove byteorder dependency (https://github.com/workingjubilee)
* Preparations to become a std dependency (https://github.com/workingjubilee)

# After 0.7.0
* Fix for drain_to functions into limited targets (https://github.com/michaelkirk)

# After 0.6.0
* Small fix in the zstd binary, progress tracking was slighty off for skippable frames resulting in an error only when the last frame in a file was skippable
* Small performance improvement by reorganizing code with `#[cold]` annotations
* Documentation for `StreamDecoder` mentioning the limitations around multiple frames (https://github.com/Sorseg)
* Documentation around skippable frames (https://github.com/Sorseg)
* **Breaking** `StreamDecoder` API changes to get access to the inner parts (https://github.com/ifd3f)
* Big internal documentation contribution (https://github.com/zleyyij)
* Dropped derive_more as a dependency (https://github.com/xd009642)
* Small improvement by removing the error cases from the reverse bitreader (and making sure invalid requests can't even happen)

# After 0.5.0
* Make the hashing checksum optional (thanks to [@tamird](https://github.com/tamird))
    * breaking change as the public API changes based on features
* The FrameDecoder is now Send + Sync (RingBuffer impls these traits now)
