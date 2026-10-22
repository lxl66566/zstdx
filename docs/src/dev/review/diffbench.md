# R66 review — the decode-parity differential (zstdx-bench parity)

Institutionalizes the R65 craft+mutate differential ([parity](parity.md)) as the `zstdx-bench parity` subcommand (todo 21b/21c): one command, deterministic corpus, both decoders, nonzero exit on divergence. The R65 harness lived in /tmp; this is the permanent form.

## Design

- Corpus: 24 real frames (our encoder over 4 payload families × levels 1/3/19, checksum on/off — `--seed` reseeds the payload RNG) + the crafted shapes of `zstdx::parity` (the same builders the lib tests pin; 36 shapes incl. the R65 header/sequence frames and the R66 literals-tail frames below).
- Mutation classes per base frame: exhaustive truncation ladder; single-byte XOR flips (masks 0xFF/0x01/0x80/0xC0/0x60 rotating by position); OR scan `|= 0x03` at every position (the class that found the modes-byte reserved-bits hole — OR keeps the mode bits untouched where XOR rewrites them); OR scan on the 18 header bytes with masks {0x01, 0x03, 0x80, 0xFF}; strided deletes/inserts. Every case is decoded by zstdx (serial arm + `--mt` arms) and by `zstd -dcq` (one CLI spawn per case — the reference is the CLI by design); accept/reject, output length and output bytes are compared. A zstdx panic counts as a hard divergence.
- Adjudication: accept/reject mismatches are re-checked against libzstd's one-shot `ZSTD_decompress` (via the zstd crate). If zstdx matches the one-shot and only the CLI (streaming) differs, the case is the documented API-pair asymmetry (empty input → Accept(0) vs the CLI's "unknown header"; window-violating offsets per parity.md finding 7) and reported as `oneshot-asymmetry`, not a failure.
- Full sweep ≈ 41k cases (1 CLI spawn + N+1 zstdx decodes each), parallelized — wrap in `flock /root/programs/fork/zstd-bench.lock`. `--cases N` stride-samples deterministically, `--dict` attaches a dictionary to both decoders and encodes the seeds with it (the dict-frame sweep itself is todo 21a), `--dump <dir>` writes divergent case bytes for replay, `--decode <file>` decodes one file with all verdicts printed.

## R66 sweep findings

The first full sweep (41,320 cases) found 42 hard divergences in the Huffman literals tail — two real bugs, both fixed:

1. **4-stream tail: rejects-what-accepts (37 cases).** libzstd on x86_64 never runs the "old body" 4X decoder for regular frames: `HUF_DecompressFastArgs_init` (tables rescaled to `HUF_DECODER_FAST_TABLELOG` = 11, all streams ≥ 8 bytes, output ≥ 6) engages the fast loop whose tail validates only output length and the `ip ≥ stream_start − 8` window bound — there is **no `BIT_endOfDStream`**. An under-running stream is filled from zero-padded/rotated window lookups and the frame is accepted. zstdx's `finish_streams` enforced the fallback body's exact-consumption check (and treated stream crossing as corruption; the reference treats it as a loop exit). Fixed: `finish_streams` is now an exact mirror of `HUF_decodeStreamX1` on a `BIT_DStream` floored at the region start (the reference's `ilowest` — the jump table), reproducing the reference's garbage bytes byte for byte, because the reference ACCEPTS that output and users observe it. Streams shorter than 8 bytes still take the strict slow path (the reference falls back to the old body there).
2. **single-stream: accepts-what-rejects (5 cases).** The 1X path has no fast variant: `HUF_decompress1X1_usingDTable_internal_body` decodes count-driven to `dstEnd`, then requires `BIT_endOfDStream`. zstdx's loop was bits-driven with only a decoded-count check — no exact-consumption gate — so streams with leftover bits whose symbol count happened to match were accepted. Fixed: count-driven decode + the exact-consumption check.

Post-fix full sweep: 41,867 cases (24 real seeds + 36 crafted), 0 hard divergences, 60 oneshot-asymmetries (all `truncate->0` empty-input cases: `ZSTD_decompress` on empty input succeeds with 0 bytes, the streaming CLI rejects; zstdx matches the one-shot API pair it implements). Both arms (serial + mt4) clean. Pins: `tests/parity.rs` (`four_stream_literals_underflow_is_accepted_with_reference_garbage`, `single_stream_tight_literals_are_accepted`, `single_stream_leftover_bits_are_rejected`), builders shared via `zstdx::parity` (`fuzz_exports`-gated).

## Residuals

- The X2 (double-symbol) interleaved path shares the mirrored tail, but the reference's X2 tail (`HUF_decodeStreamX2`) has a different guarded-phase cadence (5-symbol batches, `pEnd-9` bound); if a sweep ever reports an X2 byte-level asymmetry on an accepted corrupt frame, the tail needs an X2-specific mirror. No case found in 41k.
- The vendored zstd crate's libzstd may differ from the system CLI version; the adjudicator uses it only to classify asymmetries, and every hard divergence in this round was confirmed against a locally built reference libzstd 1.5.7 (asm and no-asm).

## How to run

```bash
flock /root/programs/fork/zstd-bench.lock \
  cargo run --release -p zstdx-bench -- parity            # full sweep (~41k cases)
cargo run --release -p zstdx-bench -- parity --cases 2000 # stride-sampled
cargo run --release -p zstdx-bench -- parity --dict bench/dict-json
cargo run --release -p zstdx-bench -- parity --decode <case.bin>  # replay one case
```

Exit 0 = clean; nonzero with per-case bytes (`--dump <dir>` writes them) = divergence.
