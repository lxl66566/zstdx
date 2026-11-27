# Entropy coding and bitstream


- Rust operator precedence differs from C's function-call reading. `1u64 << 62 / total` parses as `1 << (62 / total)`, because `/` binds tighter than `<<`, so every normalization step was wrong and the ratio dropped 28%. When porting C's `f(a,b)`-style calls, add the parentheses by hand.
- In the literals 5-bit size format, `size_format` occupies only 1 bit. In the 1-bit form, bit 3 is part of the size, so writing it as 2 bits misaligns the whole rest of the block. For the Compressed type, `sf=2` and `sf=3` are 14-bit and 18-bit sizes with 4-byte and 5-byte headers, a layout different from Raw and RLE.
- Mask packed-bitstream fields per segment. `(packed >> 8)` still carries the OF field's high bits, up to 0xFFFF, so an `& 0xFF` is needed before casting to `usize`. Only a cast to `u8` makes the truncation naturally equivalent.
- The three fields of `SeqWord.codes` are FSE codes. Raw values are a different representation. ML is the ML code, and `ml-3` holds only when `ml < 15`. Decode diagnostics must use the META base or go through the reconstruction path.
- `of_value` semantics differ by match kind. A non-rep match is `offset+3`, and a rep match is in {1, 2, 3}. When diagnostics print offsets, 307203 means 307200+3, and it is not an independent constant.
- Parsing frame structure with Python has its own traps: `nbSeq` bit width, `+` binding tighter than `|`, and sort mixing in kernel symbols. See [engineering methodology](../workflow/toolchain.md).
## Degenerate FSE distributions


The reduce-max-to-half normalization step assumed a second non-max symbol exists to receive the moved weight, and a one-symbol distribution has nowhere to move it. `write_table`'s trailing zero-probability scan also ran unbounded past the table end. Normalization and table-write loops must handle the degenerate case explicitly instead of leaning on another symbol existing.
## The FSE -1 symbol


The positive-probability sum of cross-check inputs must equal `table_size - count(-1)`, or the test generates malformed inputs and false positives.

During SoA construction the -1 symbol is filtered out by `prob <= 0`, so its start state must be written explicitly at allocation time. The baseline packed into transitions uses 9 bits and requires a baseline below 512, so widening `acc_log` must widen the packed width in lockstep.
## BitWriter hot-state handover


A batched writer that takes `hot_state()` and reasons that its adds are at most N bits, with the flush keeping `nb < 48`, must establish that invariant with one conditional flush before the first add. The entry `bits_in_partial` is unbounded from 0 to 63, so the first add can shift bits past the u64 top.

`FSETable::write_table` lost exactly the high bits of its first description entries this way. The symptom surfaced far downstream as a sequence-section underflow in `ramp_frames_decode_byte_exact`, not at the write site.
## Table coverage at every emit site


After the predefined-mode bound went onto `FseTable::max_symbol`, two fallbacks in `select_from_counts` still returned Predefined unconditionally. They were the `nb_seq <= 2` basic shortcut, matching libzstd's `isDefaultAllowed && nbSeq <= 2`, and the normalization-corner fallback.

Libzstd wraps all its `set_basic` returns in `isDefaultAllowed`, and porting a per-site decision without the surrounding gate leaves holes that are unreachable under today's window clamp but corrupt the moment it relaxes. Both fallbacks now check coverage first, and the corner's uncovered arm takes a flat one-state-per-symbol table through `flat_table`, where validity is the contract and quality is irrelevant.
## FSE transition packed width


The u32 entry's baseline keeps only 9 bits, which covers tables of at most 512 states. Production `acc_log` is at most 9, with the huff0 weight table at 6 and the sequence table at 9, so it never overflows. The `fuzz_exports` `round_trip` with `max_log=22` builds a log10 table, makes the baseline at least 512, and silently truncates it, after which the encoder writes diffs above the packed width. Debug asserts blow up under fuzz and release emits a misaligned stream.

The layout is now 12+4+12, `optimal_table_log` already clamps to 5 to 12, and `build_table_from_counts` clamps to 12 with a `build_table_from_probabilities` debug assert as double insurance. Implicit limits in bitfield packing must be enforced at the construction entry, because parameter ranges that production never uses will be stepped on by fuzz or later extensions.
## Pooled scratch state reaches the bytes


`BlockScratch::literals_gate_hold` is a sticky skip flag: once one block's literals pass the sampled entropy gate, later blocks skip the sample. The flag lived in the pooled scratch and no reset entry cleared it — the comment at the fallthrough even said "or the frame resets the scratch", a reset that did not exist. A pooled state reused across MT jobs carried the previous job's hold into the next job's first literals block, which then skipped the sampled reject and took the huffman path when the exact compare won. Ramp-gated dll-class frames keyed their bytes on the worker count: 4786591 bytes at mt4 against 4786978 at mt8 and mt16, identical sequence streams, one job-boundary block flipped between raw and huffman literals.

`frame_compressor::reset_entropy_state` is the documented single reset entry every multi-frame CompressState reuse passes through, so the clear belongs there, and the dictionary path's inline duplicate reset folded into the same entry. The regression test needs a fixture that both pins the level-3 capture grid (recurring 64-byte windows through the far screen, at the 32 MiB mid band) and lands literals in the sampler's flip window — entropy near 7.9 bits per byte, where the sampled reject raws a fresh state's block while the exact huffman compare wins a carried hold's — because a plain pattern head leaves the job grid worker-scaled and a uniform-entropy one never flips, and the equality assert asserts nothing either way.

Sticky decision flags in pooled scratch carry frame provenance into the next frame or job. Size-keyed gates stay green on them, since each worker count is internally consistent; a cross-worker byte-identity assert is the gate that sees the divergence.
