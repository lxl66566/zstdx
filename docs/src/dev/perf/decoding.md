# Performance Optimization · Decoding

> Everything is landed and verifiable by commit; disproven directions are in [Disproven Directions](../negative.md); open residual work is in [TODO List](../todo.md).

## Output path evolution

1. **ring buffer**: hand-written ringbuffer ("discard from head" and "copy internally to tail" each need half the work; VecDeque/Vec can each do only one efficiently), with modulo-free wrap and hand-written extend_from_within for four layouts. Now used only by dictionary frames.
2. **flat direct write (slice)**: dictionary-free slice decoding writes Raw/RLE/Compressed blocks directly into the caller's target buffer, bypassing the ring and drain copies; block-level budget check + `TargetTooSmall` error variant. slice +13-35% across the board. `77b03b6`
3. **streaming outBuff model**: libzstd outBuff model, steady-state capacity window+2·block+64, two-generation virtual-mapping wrap (`FlatView{origin, prev_origin}` maintains a globally monotonic virtual address), zero sliding copies, flush leaves no window behind. Large-window corpora (8MB window) +85-125%, random/zeros overtake zstd-strm. `8426ba8`

## Fused sequence loop (current core)

- **decode-execute fusion** (libzstd model): sequences execute immediately after decoding, no round-trip through a sequence vector. Note that pure fusion was initially -1.8%; it only turned positive once faking RLE as a single-state table (fake entry self-transition, acc_log=0) eliminated 3 Option registers + 6 branches per sequence, while also deleting 4 variant loops. json.zst1 +10% (overtaking zstd crate slice), json.zst3 +7%. `971b43f`
- state-carrying decode_step + pointer-cursor executor + `HEADROOM` const specialization (when streaming buffers guarantee block size +16B slack, the per-sequence budget gate and wildcopy gate disappear entirely): streaming +4-7.4%. `c6e5fd9`
- the loop carries only three FSE states (not packed entries); `bits`/`src_len` leave the stream state (reload can always rebuild the bit window from memory); the executor cursor becomes raw pointers, folding away base.
- **register wall law**: with ≥15 live values in this loop, LLVM stack-spills the bit window (on the annotated asm, a `shlx` with a memory operand is the tell). All structural rework (splitting out batching, two-stage pipelines, register-resident repcodes) was disproven — the fused structure is locally optimal, see [Disproven Directions](../negative.md).

## Sequence decoding

| Technique | Effect | commit |
|---|---|---|
| 64-bit read-back bitstream (pointer + bits_consumed, inlined reload) + packed u64 FSE entry (base/add_bits folded in at table build, all fields in one load) | sequence decoding ~15% | `0e88ecc` |
| three packed tables merged into one base array + constant slot offsets (LL512/ML512/OF256) | kills the per-iteration table pointer reload | `0d12538` |
| pre-shifted bit window (`win = bits << consumed`) | two shifts left per read | `0d12538` |
| grouped batch extraction: add-bits 3-in-1 + state-transition 3-in-1, one serial window read split in parallel (sum≤31 avoids mid-seq reload) | skewed.zst3 +7-8%, json.zst3 +3-5% | `c79086f` |
| zero-width branchless reads (`wrapping_shr`/bzhi masks) + repcode resolution rotations all via select | skewed.zst3 streaming +4.5% | `1fdcfaa` |
| BMI2 runtime dispatch (shared `#[inline(always)]` impl + `#[target_feature]` wrappers) | neutral on Zen4, benefits Intel | `72d65a1` |
| per-symbol spread precomputation for FSE table build (SymbolSpreadInfo inline arrays) | table-build profile 4.9%→1.9% | `59f6c73` |
| `do_offset_history` slot-indexed (code-1+ll0 single-value slot selection + rotation, backed by exhaustive equivalence tests) | fewer instructions per sequence | `82f3a00` |

## Executor (port of the libzstd wildcopy copy stack)

- literals: ≤16 inline copy16 (`literals_buffer.reserve(16)` guarantees the over-read stays in bounds), \>16 uses a 16B chunk loop.
- match: offset≥16 → copy16/16B loop; [8,16) 8B loop; <8 overlapCopy8 (dec32/dec64 tables write 8B at a time to lift the effective offset to ≥8; **first 4 bytes byte-by-byte** so stores feed loads).
- budget gate `w+ll+ml+16 <= out_len` guarantees overshoot lands in a legal region; the tail takes the exact slow path.
- motivation: json/skewed are masses of 5-6B small copies (LD_PRELOAD measured 32MiB with ~3M PLT calls, ~35% of time). After landing, memmove fell 14.3%→1.6%, json/skewed/text decoding +23-45%. `8ce527a`
- active-segment match source algebraically simplified to `dst - offset` (virtual distance = physical distance, base indirection eliminated); wrapped sources outlined #[cold] to shed 4 view live values; boundary checks merged 3-in-1. `bee924d`
- prev-segment linear fast path: a wrapped previous-segment source physically sits at `dst + seg_a_end - offset` (above the write cursor by ≥MAX_BLOCK, so wildcopy double-sided overshoot is safe); one wildcopy replaces generic segment walking; **placed in the existing out-of-line callee to preserve hot-loop codegen**. skewed.zst9 +4.8%. `6bccc54`

## Huffman literals

- X2 double-symbol table (port of libzstd HUF_decompress4X2) + custom **80% pairing threshold** (the pairable fraction of code space is computed from weights; below it the space is left empty and X1 is used): skewed.zst1 +43%, skewed.zst3 +15%. `0af7fc1`
- X1/X2 stream-state pointerization (ip/op fold base, live values 16→14): inner-loop instructions -10%. **Conclusion: at the practical floor** — libzstd's hand-written asm does 3 dtable loads (memport-bound ~0.7 cyc/B) vs our 1 load + 2 shifts (ALU-bound ~0.75 cyc/B), same magnitude. `5be5f88`

## Miscellaneous

- frames without Content_Checksum skip xxh64 init/update (+2.7%); checksum computation brought in-tree (encode/decode share the 8-chain implementation, twox dropped as a runtime dependency). `5e2c99f` `81d2119`
- consecutive blocks in Predefined mode do not rebuild FSE tables (eliminates per-block allocation, +2.5%). `6d40e4f`
- StreamingDecoder `read` decodes until the caller's buffer is full (removed UptoBlocks(1) granularity, +2%). `26a29da`
- fixed-width copies with allowed overshoot (`copy_bytes_overshooting`, u128/usize whole-word loops bypassing memcpy length dispatch); exponentially doubling overlapping copy `repeat_in_chunks` (log2 memcpys instead of ml/offset).
- interleaved Huffman table access/output unchecked (the bounds check was already absorbed by branch prediction, ≈0 but kept).

## Residual (vs libzstd streaming, json/skewed 1.05-1.3×)

The fused loop is already at the serial-chain limit (IPC ~3.7); the residual is loop instruction quality: entry unpacking 6+ uops/sequence, shlx/shrx serial chains, ~20 live values with residual spilling. The register wall is proven (see negative); compressing further requires reworking decode_step itself; the next order of magnitude requires algorithm-level changes (libzstd-style 8-deep sequence ring-buffer pipeline, SIMD copy scheduling), benefit/risk still to be evaluated.
