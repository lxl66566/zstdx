# Decode parity differential

This page describes the `zstdx-bench parity` subcommand, which turns the craft-and-mutate differential from the [decode error paths](parity.md) review into a permanent tool. It answers what the tool compares, which mutation classes it runs, how it adjudicates a mismatch, and what the first full sweep found. The earlier harness lived in `/tmp`. This is its permanent form.

## Design

### Corpus

The corpus holds 24 real frames produced by our encoder over 4 payload families at levels 1, 3 and 19, with the checksum on and off. `--seed` reseeds the payload RNG. It also holds the crafted shapes from `zstdx::parity`, which are the same builders the lib tests pin. The crafted set is 36 shapes, including the header and sequence frames from the [decode error paths](parity.md) review and the literals-tail frames described below.

### Mutation classes

Each base frame passes through an exhaustive truncation ladder. It also passes through single-byte XOR flips with masks 0xFF, 0x01, 0x80, 0xC0 and 0x60 rotating by position. An OR scan applies `|= 0x03` at every position, which is the class that found the modes-byte reserved-bits hole, because OR keeps the mode bits untouched where XOR rewrites them. A second OR scan runs on the 18 header bytes with masks 0x01, 0x03, 0x80 and 0xFF. Strided deletes and inserts run as well.

Every case is decoded by zstdx through the serial arm and the `--mt` arms, and by `zstd -dcq` with one CLI spawn per case, because the CLI is the reference by design. The tool compares accept or reject, output length and output bytes. A zstdx panic counts as a hard divergence.

### Adjudication

An accept or reject mismatch is re-checked against libzstd's one-shot `ZSTD_decompress` through the zstd crate. When zstdx matches the one-shot API and only the CLI differs, the case is the documented API-pair asymmetry, and the tool reports it as `oneshot-asymmetry`. The two asymmetry classes are empty input, where `ZSTD_decompress` returns `Accept(0)` and the CLI reports an unknown header, and window-violating offsets, described in the [decode error paths](parity.md) review.

Since the binding move to zstd-sys 2.2.0 (libzstd 1.6.0), the adjudicator keeps classifying window-violating offsets as `oneshot-asymmetry`, because 1.6.0's one-shot still accepts whole-history offsets. 1.6.0's one-shot also newly rejects Raw and RLE blocks over `min(window, 128 KiB)`, a cap zstdx's flat path lifts, so a mutant that inflates such a block flips from `oneshot-asymmetry` to a hard divergence. The mutation space reaches the class, verified by a stride sample of 2877 cases: a crafted `multi-frame-cross-offset` frame with an XOR flip on the second frame's RLE block header inflating its claim from 1000 to 5096 bytes against the 1 KiB window. The tool now classifies those cases as a third kind, `block-cap permissiveness`, after a walker proves the stream really holds the over-cap claim, printed in the summary and not failing the gate; the sample reports 0 hard, 76 `oneshot-asymmetry`, 1 `block-cap`. Anything else a zstdx accept disagrees with stays a hard divergence.

### Sweep control

The full sweep is about 41k cases, each with one CLI spawn and N+1 zstdx decodes, and it runs in parallel. It must be wrapped in `flock -x /tmp/myflock.lock`. `--cases N` stride-samples deterministically. `--dict` attaches a dictionary to both decoders and encodes the seeds with it; the completed dictionary-frame sweep is recorded in [dictparity](dictparity.md). `--dump <dir>` writes the bytes of divergent cases for replay, and `--decode <file>` decodes one file with all verdicts printed.

## Bugs fixed

The first full sweep ran 41,320 cases and found 42 hard divergences in the Huffman literals tail. They came from two real bugs, both fixed.

The 4-stream tail produced 37 rejects-what-accepts cases. On x86_64, libzstd never runs the old-body 4X decoder for regular frames. `HUF_DecompressFastArgs_init` engages the fast loop when the tables are rescaled to `HUF_DECODER_FAST_TABLELOG` = 11, all streams are at least 8 bytes and the output is at least 6 bytes. The fast loop's tail validates only the output length and the `ip >= stream_start - 8` window bound, and it has no `BIT_endOfDStream`. An under-running stream is filled from zero-padded or rotated window lookups, and the frame is accepted. zstdx's `finish_streams` enforced the fallback body's exact-consumption check and treated stream crossing as corruption, while the reference treats it as a loop exit. `finish_streams` is now an exact mirror of `HUF_decodeStreamX1` on a `BIT_DStream` floored at the region start, which is the reference's `ilowest` jump table. It reproduces the reference's garbage bytes byte for byte, because the reference accepts that output and users observe it. Streams shorter than 8 bytes still take the strict slow path, because the reference falls back to the old body there.

The single-stream path produced 5 accepts-what-rejects cases. The 1X path has no fast variant. `HUF_decompress1X1_usingDTable_internal_body` decodes count-driven to `dstEnd` and then requires `BIT_endOfDStream`. zstdx's loop was bits-driven with only a decoded-count check and no exact-consumption gate, so it accepted streams with leftover bits whose symbol count happened to match. The decode is now count-driven and performs the exact-consumption check.

The X2 interleave had a same-shape residue that only a malformed jump table reaches (review-a, `f2`): flipping the table's first byte slides the stream boundaries while every stream stays at 8 bytes or more, so both decoders keep the fast loop and both accept the block with full-length garbage, but their bytes differed from position 245 on. Two causes. The X2 loop handed its window state to `finish_streams`, the X1 mirror, while the reference's fast path finishes through `HUF_decodeStreamX2` with the pair table: a pair lookup consumes both symbols' bits in one step, so once the tail's window freezes at the reload floor, the rotation lookups pick different symbols than two single-symbol lookups, and the accepted garbage itself diverges. And the BMI2 asm stretch ran a fixed precomputed round count, while the reference's asm loop ends each stretch at its op[3] `olimit` compare, so the two entered the tail with different window states when a stream ran dry mid-stretch. The X2 loop now has its own `finish_streams_x2` (an exact mirror of `HUF_decodeStreamX2`'s three phases plus `HUF_decodeLastSymbolX2`'s consumption clamp), the asm stretch carries the same `olimit` bottom compare, and a stream crossing below its predecessor exits to the tail like the reference's `goto _out` instead of erroring. `tests/parity.rs::four_stream_jump_table_corruption_matches_libzstd_bytes` pins byte equality with libzstd on the mutant and byte-identical legal-frame decode.

## Post-fix sweep

The post-fix full sweep ran 41,867 cases, from 24 real seeds and 36 crafted shapes. It found 0 hard divergences and 60 `oneshot-asymmetry` cases, all of them `truncate->0` empty-input cases. In those cases `ZSTD_decompress` on empty input succeeds with 0 bytes and the streaming CLI rejects, and zstdx matches the one-shot API pair it implements. Both arms, serial and mt4, were clean.

The pins are in `tests/parity.rs`: `four_stream_literals_underflow_is_accepted_with_reference_garbage`, `single_stream_tight_literals_are_accepted` and `single_stream_leftover_bits_are_rejected`. The builders are shared through `zstdx::parity`, gated behind `fuzz_exports`.

## Residuals

The X2 double-symbol interleaved path shares the mirrored tail, but the reference's X2 tail in `HUF_decodeStreamX2` has a different guarded-phase cadence, with 5-symbol batches and a `pEnd-9` bound. If a sweep ever reports an X2 byte-level asymmetry on an accepted corrupt frame, the tail needs an X2-specific mirror. No case turned up in the 41k sweep.

The vendored zstd crate's libzstd may differ from the system CLI version. The adjudicator uses it only to classify asymmetries, and every hard divergence in this round was confirmed against a locally built reference libzstd 1.5.7, with and without asm.

## Platform-dependent reference verdicts

The reference's verdict on the accepted-garbage classes is a property of its build, not of the format. `HUF_DecompressFastArgs_init` answers "use the fallback" when `MEM_32bits() || !MEM_isLittleEndian()`, so the 4-stream fast loop and its length-only tail validation exist only on 64-bit little-endian builds. Every other build takes the old body, whose per-stream `BIT_endOfDStream` exact-consumption check rejects a slid or under-running stream as corruption.

The cross CI surfaced this through the jump-table mutant, with identical frame bytes on every platform. libzstd 1.5.7 accepts the mutant on x86_64 and rejects it on i686 and s390x. zstdx ports the fast loop as portable Rust, so it accepts on every platform with byte-identical output, verified on x86_64, i686 and qemu-s390x. zstdx keeps this behavior instead of mirroring the 32-bit and big-endian rejection, because the same frame would then decode or fail depending on the build target, and those builds would lose the interleaved loop to the slow strict tail. `four_stream_jump_table_corruption_matches_libzstd_bytes` now pins libzstd's verdict per pointer width and endianness, keeps byte equality with libzstd on 64-bit LE, and pins the accepted garbage by FNV-1a on every platform, so a word-size or endianness divergence inside zstdx still fails the test.

## How to run

```bash
flock -x /tmp/myflock.lock \
  cargo run --release -p zstdx-bench -- parity            # full sweep (~41k cases)
cargo run --release -p zstdx-bench -- parity --cases 2000 # stride-sampled
cargo run --release -p zstdx-bench -- parity --dict bench/dict-json
cargo run --release -p zstdx-bench -- parity --decode <case.bin>  # replay one case
```

Exit status 0 means clean. A nonzero exit with per-case bytes, written by `--dump <dir>`, means a divergence.
