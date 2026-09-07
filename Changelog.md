# Changelog

This document records the changes made between versions, starting with version 0.5.0

# After 0.9.0 (Current)

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
