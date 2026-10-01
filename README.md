# Zstdx (a pure rust zstd format implementation)

A pure Rust implementation of the Zstandard compression format, focus on performance. Forked from [KillingSpark/zstd-rs](https://github.com/KillingSpark/zstd-rs) with a from-scratch encoder; the decoder and encoder both cover the full format and interoperate with the reference C implementation in both directions.

- Decoder: complete, including dictionaries, multi-frame/skippable-frame streams, and checksum verification on every path.
- Encoder: the full numeric level ladder 0–22 (fast/dfast/chain/btlazy2/btopt/btultra strategy families modeled on libzstd), checksums, dictionaries (raw-content and `zstd --train`-style formatted), long-distance matching, and multithreaded compression.
- Multithreaded _decompression_.
- `no_std` + `alloc` capable; a `zstdx::compat` layer mirrors the [`zstd` crate](https://docs.rs/zstd)'s API shape (numeric levels, `io::Result`) for easy porting.

Performance is tracked against libzstd in `docs/src/dev/` ([snapshot](https://github.com/lxl66566/zstdx/blob/master/docs/src/dev/bench/snapshot.md)).

## Usage

One-shot:

```rust
let data = b"the quick brown fox jumps over the lazy dog";
let compressed = zstdx::bulk::compress(data, zstdx::Level::Fastest);
let restored = zstdx::bulk::decompress(&compressed, 0).unwrap();
assert_eq!(&restored[..], data);
```

Streaming over `io::Read`/`io::Write`:

```rust
use zstdx::io::{Read, Write};

let mut compressed = Vec::new();
let mut enc = zstdx::stream::write::Encoder::new(&mut compressed, zstdx::Level::Fast).unwrap();
enc.write_all(b"some payload").unwrap();
enc.finish().unwrap();

let mut restored = Vec::new();
zstdx::stream::read::Decoder::new(&compressed[..])
    .unwrap()
    .read_to_end(&mut restored)
    .unwrap();
assert_eq!(restored, b"some payload");
```

Options (checksum, pledged size, worker threads, dictionaries, forced window) are builder-style: `zstdx::EncoderOptions` / `zstdx::DecoderOptions`. See the [user guide](https://github.com/lxl66566/zstdx/blob/master/docs/src/user.md) for the full tour, and `docs.rs/zstdx` for the API reference.

## Feature flags

<!-- prettier-ignore -->
| Feature | Default | Effect |
|---|---|---|
| `std` | ✓ | `std::io` traits, threads, `compat` |
| `hash` | ✓ | XXH64 frame checksums (write + verify) |
| `dict_builder` | — | dictionary training (`zstdx::dict`) |

Without `std` the crate builds as `no_std` + `alloc`; bulk, streaming and the low-level APIs work, thread options report `Error::Unsupported`.

## Performance

Benchmarked against the `zstd` crate (libzstd 1.5.7) on 32 MiB corpus shapes plus a 100 MB real-binary payload (dll, concatenated system ELF binaries via [gen_big.sh](bench/gen_big.sh)); single-threaded unless noted, checksums off, every cell roundtrip-verified, ±10% run-to-run noise. The figures use the **streaming** paths — what `Read`/`Write` users actually run; full tables incl. bulk: [snapshot.md](https://github.com/lxl66566/zstdx/blob/master/docs/src/dev/bench/snapshot.md).

![Streaming encode: throughput vs compression ratio, zstdx vs libzstd](https://raw.githubusercontent.com/lxl66566/zstdx/master/assets/encode-pareto.svg)

At matched numeric levels zstdx is denser at **every** streaming-encode level on every payload — +4.7% bytes on json at the fastest tier, +20% on json balanced (ratio 7.13 vs 5.95), −13..−17% on the dll payload from balanced up, −49.6% at its fastest tier at speed parity. Speed: zstdx leads the working tiers — fast on every payload, fastest on text at 3.2×, best/opt/ultra on text — while the cells it trails (json fastest/balanced/best, text balanced, dll balanced/best/opt/ultra) all carry 1.7–20% byte wins; json best/ultra hold ratio parity-or-denser at somewhat less speed, everything else sits on or above the libzstd frontier. On unknown-size text streams libzstd's fastest tier emits a 1.84 MB frame (ratio 18.3) vs our 108 KB (ratio 309) — 16.9× denser *and* 3.2× faster (the isolated gray point in the log-x panel). Reader pull sizes 4 KiB–1 MiB were swept for every cell: speedups stay within 5% of the 64 KiB value.

![Streaming encode, 8 workers vs libzstd at 8 workers](https://raw.githubusercontent.com/lxl66566/zstdx/master/assets/encode-stream-mt.svg)

With 8-worker multithreaded streaming (64 KiB pulls), zstdx stays ahead of libzstd on json at every tier (1.6–5.7×) and on text at every tier (1.0–3.4×). On the dll payload it leads at fast and opt but trails at fastest, balanced, ultra and sharply at best (0.07×) — those cells also run at 46–93% of zstdx's own bulk-mt throughput, so the streaming-MT path on that payload has recorded headroom in the bench docs.

![Streaming decode speedup over libzstd](https://raw.githubusercontent.com/lxl66566/zstdx/master/assets/decode-speedup.svg)

Streaming decode — the honest gap — wins incompressible data (random 1.2–1.3×, zeros at parity) and trails on compressible shapes (dots left of the parity line; libzstd's edge is 1.04–1.36×), and the pull-size bands show it barely moves from 4 KiB to 1 MiB reads. Bulk decode, for contrast, wins 17/18 cells up to 4.8×. Our 16-worker multi-threaded decoder (diamonds) reaches ×1.18 (json) / ×1.31 (dll100) of libzstd's single-threaded streaming decode. Compression-ratio geo-mean over the full sweep (5 shapes × 6 levels × bulk/stream × ST/MT): **+8.7% denser than libzstd** at matched numeric levels.

Full per-level tables (1-22), methodology and noise caveats: [snapshot.md](https://github.com/lxl66566/zstdx/blob/master/docs/src/dev/bench/snapshot.md) and [matrix.md](https://github.com/lxl66566/zstdx/blob/master/docs/src/dev/bench/matrix.md).

## CLI

The companion `zstdx-cli` crate is a flag-compatible `zstd` command line (`zstdx file` → `file.zst`, `-d` to decode, `-1`..`-19`/`--fast`, `-T0`, `-D DICT`).

## Testing

Tests take two forms:

1. Tests using well-formed files that have to decode correctly and are checked against their originals, plus interop roundtrips against the reference implementation (dev-dependency on the `zstd` crate).
2. Tests using malformed input generated by the fuzzer; these don't have to decode (they are garbage) but must never panic the decoder.

## Fuzzing

The fuzz targets live in `crates/zstdx-fuzz` (`decode`, `decode_dict`, `encode`, `encode_stream`, `interop`, `fse`, `huff0`). From that directory, use `cargo +nightly fuzz run decode` (or another target) to run the fuzzer.

If the fuzzer finds a crash it will be saved to the artifacts dir by the fuzzer. Run `cargo test -p zstdx artifacts` to run the artifacts tests. This will tell you where the decoder panics exactly. If you are able to fix the issue please feel free to do a pull request. If not please still submit the offending input and I will see how to fix it myself.
