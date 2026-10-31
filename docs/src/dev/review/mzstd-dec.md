# mzstd decode-side review

This page records the R86 Track 4 audit of [mzstd](https://github.com/M-Compressors/mzstd), a safe-Rust zstd reimplementation with byte-identical frame output and a reported decode median of 1.5x the C library. Our streaming decode runs at 1.17 to 1.32x C on the dense shapes, so the audit looked for transferable techniques rather than a roadmap, against the standing attribution that the whole streaming gap is fused-loop op volume with the wrapper at parity, in [dec-gap](../bench/dec-gap.md). Five techniques from their decode side were checked against our code and measurements. None transfers: two are already implemented here in stronger form, one maps onto a falsified family, one is falsified-equivalent by our own transplant data, and one has no exposure in our build. The review source was read at their repository's tip of 2026-11-01.

## History and Sink delivery

Their streaming decoder regenerates every block into a self-owned `History` vector that doubles as the window, delivers bytes to the caller through a `Sink` trait instantiated per destination type, and supports uninitialized caller memory through `BorrowedBuf` (`src/decompressor/core/history.rs`, `stream.rs`). The buffer grows without bound between compactions and drops all but the last window only after growing by another window past it, so each byte moves at most once on average and the history never wraps.

This is their safe-Rust substitute for a sliding window, and our output path is ahead of it. Our streaming model ports libzstd's outBuff design with a two-generation virtual-mapping wrap that keeps a globally monotonic address, so there are zero sliding copies and a flush leaves no window behind, while dictionary-free slice decoding writes blocks directly into the caller's target (see the output path section in [decoding](../perf/decoding.md)). The wrapper's residual cost against our own bulk path measured 2.2% on json.zst3, 1408 against 1439 MiB/s, and the wrapper sits at parity with libzstd's in the dec-gap decomposition. Nothing in the Sink structure applies.

## 32-bit index masking

Every index their copies see is masked through `idx`, which ANDs with `u32::MAX`, under a compile-time assertion that `2^window_log + 8*block_max < 2^32` (`src/decompressor/core/copy.rs`). The mask changes no value; it lets the compiler prove `index + 32` cannot wrap, leaving one bounds compare per access instead of two, with zero unsafe code.

Our executor has no per-access bounds check left to fold. It runs raw pointers under the per-sequence budget gate `w+ll+ml+16 <= out_len`, with the three boundary checks merged into one, and wrapped sources outlined cold (see the executor section in [decoding](../perf/decoding.md)). The masking trick buys safe slice code the property our pointer code already has by construction, so it is superseded here.

## Sentinel bit reader

Their Huffman fast loops read through a `Sentinel` container that keeps unread bits at the top of a u64 and a `1` marker below them, shifted up with every read, so the consumed count is the marker's `trailing_zeros` position and a lookup is one shift (`src/decompressor/core/bits.rs`). The refill retreats by the consumed whole bytes and clamps the position to a precomputed limit, which turns the bounds proof into a compare and a conditional move, and the loop counts the affordable rounds up front so refills carry no branch.

Our X1 and X2 interleave loops already run this exact structure, ported from the reference's C loops: the 1-marker window with `trailing_zeros` as the consumed count, the five-lookups-per-round schedule, and the round bound precomputed from the input and output margins (`decompress_4streams_interleaved` and `decompress_4streams_interleaved_x2` in `crates/zstdx/src/decoding/literals_section_decoder.rs`). The clamp is their safe-code substitute for what our reload buys by reading through raw pointers under the iteration bound. Equivalent, already in place.

## BitCount as a 32-variant enum

Their sequence table row carries `extra_bits` and `nb_bits` as a `#[repr(u8)]` enum with one variant per count from 0 to 31 (`src/decompressor/core/fse.rs`). A load of the field then carries range information to the compiler, so reads shifted by the count need no mask on the shift amount.

The row carrying four separately loaded fields is libzstd's `ZSTD_seqSymbol` layout, which is the shape our entry-struct layout swap measured: minus 15.1 fused-loop instructions per sequence and minus 6.8% whole-decode instructions on json, yet cycles flat to worse everywhere because IPC fell about 1:1 with the cut, and the text-stream gate failed on every roll (see the entry-layout entries in [negative decoding](../negative/decoding.md)). The 2xu32 field-group variant lost deterministically at the instruction level, and the 5-bit nb mask variant, which kills the same `bzhi` guards by a different route, lost at plus 1.6% loop instructions to an allocation re-roll. The BitCount enum is a fourth route into the same mechanism space, killing shift-amount masks inside the fused loop, and that space is measured out on Zen4: the loop is issue and latency saturated, so uop cuts that leave the issue structure intact convert into IPC loss. Falsified-equivalent by existing data; no new measurement was warranted inside a closed family.

## AVX-512 downgraded to AVX2 for block decode

Their backend runs the block decoder's loops at AVX2 even when the host reports AVX-512 (`src/backend.rs`, `block_decode_level`), because the cheap gathers of AVX-512 lead the compiler to vectorize the four Huffman streams' independent lookups across lanes, lengthening every stream's dependency chain, at a measured 101.8 against 87.2 microseconds on their benchmark.

Our decode path has no auto-vectorization surface to protect. Every decode dispatch site is an explicit BMI2-gated `#[target_feature]` wrapper over a shared inlined body, with no SIMD abstraction layer in between, and the decode tier switch knows only scalar and bmi2. A disassembly sweep of the release bench binary over the zstdx decode region on 2026-11-01 found zero gather instructions in any decode symbol; the five hot decode functions, both Huffman interleave loops, the fused executor, its bmi2 instantiation, `finish_streams` and `copy_wrapped_match`, contain only the SSE `movups` pairs of the wildcopy kernels; and the sole AVX-512 in the swept region is the encoder's explicit `uniform4_pack_avx512` table-packing kernel. No exposure.

## Verdict

mzstd's loop bodies are ports of libzstd's shapes, the field-per-load sequence rows, the split read-plus-execute structure and the mid-stream reload, and every one of those shapes has a transplant measurement in this repository already. Their streaming and output structure is a safe-Rust design that our outBuff port with the two-generation wrap supersedes on measured ground. The audit closed with no transferable lever, and the standing dec-gap attribution stands: the streaming gap is fused-loop dispatched-op volume, with every instruction-volume, structural and layout lever falsified or measured out.
