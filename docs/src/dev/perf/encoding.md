# Performance Optimization · Encoding (excl. matchers)

> Matcher and level strategy: see [Matchers and Compression Levels](matchers.md); multithreading and streaming pipeline: see [Multithreading and Streaming](mt-stream.md).

## Entry and state management

- **zero-copy slice entry** `compress_slice_to_vec`: the match window directly borrows the input (raw-pointer window + unsafe Send), eliminating the read pass, window compaction, and per-call window allocation; output upfront-reserved, blocks written straight into the final output. text +40%, zeros +60%. `51b019e`
- **thread_local state pool** (hash tables/default FSE tables/BlockScratch, take()-style reuse across calls, nesting-safe): 1KiB payloads 3-7×, 4KiB +30-80%. `cb02c32`
- **emit path rework**: entropy tables take/restore by value instead of deep-cloning 3×FSETable+Huffman per block (the original several hundred allocs per block was the biggest black hole, ~30%); `start_matching_into` buffer direct-write replaced the callback closure (the closure captured 2×Vec + matcher state → register spills, one store sampled at 40%); matcher window reads unchecked. Itemized +2%/+2%/+11%, cumulative json +24%, text +39%. `68ae651` Lesson: **unchecked-read gains (+11%) far outweigh structural rework (+2%×2); bounds checks on byte-wise paths are the prime target**.
- block contents written in place into the frame output (reserve the 3-byte block header first, backfill after compression) + literals/sequence/precomputed-code buffers moved into the BlockScratch pool: random -6.8% instructions. `95b2006`
- zero-sequence blocks skip literal staging (matcher skips the whole-block copy, encoder reads the window directly): random 64K-1M +13%. `15d127d`

## FSE

- SoA flattened tables (probs/start/transitions in contiguous memory) replace 256×Vec + double sorting. `81afece`
- per-symbol spread precomputation; flat transition table `(next<<13)|(nb<<9)|baseline` O(1) state transitions + LL/ML code LUT: json 189→245 MB/s. `5619e80`
- 6 write_bits per sequence merged into 2 u64 spellings (transition bits ≤27bit, add-bits ≤51bit, one each): another +4%. `bbde2ea`
- transition bits and add-bits merged into one hot_push (combined ≤56bit safe cap, falls back to double push beyond it). json -0.4% instructions. `2d18813`
- **table mode selection** (port of the libzstd `selectEncodingType` fast branch): RLE single-code / Predefined threshold / Encoded + normalizeCount M2 normalization + optimalTableLog + last-sequence count decrement. json ratio 5.22→5.37. `1a17db3`
- **repeat mode (mode 3)**: when the previous block's table covers all active codes and the bit-cost estimate (Σ c·log2) ≤ new-table description + entropy lower bound, the whole table is reused with zero description bytes; a Predefined/RLE choice invalidates the remembered table (`PrevTable::{New,Keep,Clear}` three states guard against table-state desync with the decoder). json -2.2% instructions, ratio 5.99→6.00. `7769bf8`
- RLE degenerate single-state FSE table: table_size=1 takes exactly the same path as normal modes, zero special-case branches.

## Huffman

- **boundary package-merge optimal length-limited codes** (Larmore-Hirschberg) replacing the initial rank-weight ladder (the old scheme depended on count order, not magnitude — large losses on skewed histograms): all levels benefit, json Ultra 7.33→7.52, text fastest 196.6→206.0. `aa07308`
- `build_from_weights` counting sort: package-merge caps weights at 11, so 12 stack buckets with symbols scattered in ascending order reproduce the comparison sort's weight-ascending/symbol-ascending order exactly (byte-identical output, gate: full-ladder dump); removes the per-block heap Vec and the O(n log n) sort. text-4K small-payload encode +5% wall-clock, gungraun instruction counts drop on every text-shaped cell, no regressions elsewhere.
- scalar 4-symbol batch accumulation: packed u16 `(code<<4)|nb` code table; flush to <8bit first, then branchlessly accumulate 4 symbols into a u64; a single unaligned store flushes whole bytes. skewed +71%, json +4%.
- **AVX-512VBMI 4-bit code packing kernel** (uniform 9..16-symbol alphabets): 64 symbols/batch, `permutex2var_epi8`×2 indexing a 256-entry LUT + two permutes to un-interleave paired nibbles. skewed instructions -48%, throughput +43%. `2be2207`
- RLE literals mode (all literals the same byte → type1 header + 1 content byte). Gotcha: in the 5-bit size format, size_format occupies only 1 bit (see the pitfalls notes).

## Histogram and incompressibility detection

- literals histogram **4-way sub-tables** (split by position mod 4, avoiding store-forwarding serialization on the same counters): skewed cycles -7%, throughput +10%.
- ≤16 alphabet **AVX-512 exact histogram**: 16-slot `cmpeq_epi8_mask` + popcount accumulation; the popcount sum doubles as 17th-symbol detection (scalar fallback only when uncontaminated). skewed instructions -18%, +28%. `e76bb60`
- **Miller-Madow entropy sampling rejection gate**: strided sampling (~1024 points) + Miller-Madow bias correction + distinct>208 pre-screen + sticky `gate_hold`; rejects only at ≥8 bits/byte. random instructions -43%, +43% (1450→2060, overtaking libzstd L1). `cdc70c0`
- literals entropy lower-bound pre-check (exact histogram + Shannon bound + 8% margin; skip the Huffman attempt when it cannot beat raw; histogram and table build share one scan): random 455→1178 (+159%). `a32d2c1`
- three-table histogram fused into a single pass (one traversal of packed codes updates the ll/ml/of counters). `6690a05`
- log2 under no_std uses a linear-mantissa approximation (error <0.086, swallowed by the 8% margin). `9121215`

## Checksum (xxh64)

- **in-house 8-chain XXH64**: 2×32B per iteration, 8 accumulation chains in flight (4 chains +8%); shared with the decoder, twox-hash demoted to dev-dep, zero external runtime dependencies. `3c0bf97` `81d2119` Note: the AVX-512 vpmullq vector core was a negative result (serial dependency chain); 8 interleaved scalar chains is the right answer.
- **pass fusion**: the RLE uniformity scan runs XXH64 rounds while comparing (uniform-block hashing completed inside the single pass; on mismatch it returns a 32B-aligned resume offset to continue); raw-block copies absorb while copying; the zero-sequence-block gate rejects with early exit. zeros +22%, random +10%. `3c0bf97`
- **sidecar thread offload** (std+hash, input ≥256KiB, ≥2 cores): a thread_local SPSC ring (512 slots) posts block bytes, consumed by a dedicated worker; the `BlockChecksum` trait unifies inline/offload. random +13%, text +26%, skewed +14%, zeros +2%, json +4%. `df33295`
- worker **bounded-spin parking**: parks on a condvar after a ~300µs budget (80µs was not enough — the raw-block pipeline paces at worker saturation, so wakeup latency lands directly on the critical path); the head flip is published under the lock to prevent lost wakeups. Saturated runs see ~1 voluntary switch per frame, 0 ticks while idle. `70b5fd4`
- on the MT path the checksum is absorbed in parallel, inline in the calling thread; during streaming bursts the main thread hashes.

## Bit writing and miscellaneous

- BitWriter 64-bit flush uses a single unaligned u64 store instead of an 8-byte memcpy: all corpora +1-2%.
- sequence codes computed once and shared (choose_table and encode_sequences consume the same set of code arrays, META table lookup split from add-bits): identical output bits, cleaner code. `b0c2653`
- sequence code/add-bits single-pass precomputation (codes packed into a u32 stream, add-bits pre-merged into u64): json +3%. `d9a840b`
- the matcher pushes the packed sequence stream directly (`Matcher::start_matching_codes`, json -3.5% instructions) — see the matchers page for details. `6faba75`
- RLE detection chunked by u64 (the original closure's byte-wise indexing could not be vectorized); RLE blocks' skip_matching inserts only the first position (all 5-byte windows of a uniform run land in the same slot): zeros 7.3×. `6abb9ae` `d9f078a`
- after match emission, short-literal overrun-overwrite copy (≤8B literal run: `reserve(8)` + unaligned read/write + set_len; the read runs into the following match, write overflow ≤7B is overwritten by subsequent pushes).
- uniform block detection: 4×u64 batched compares + deliberate `#[inline(never)]` (inlining into large callers made the loop lose its data pointer and reload it from the stack every 32B, halving throughput — counterintuitive but measured).
