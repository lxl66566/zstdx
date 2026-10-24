# Zstdx

Zstdx is a pure Rust implementation of the Zstandard compression format, built for performance. It started as a fork of [KillingSpark/zstd-rs](https://github.com/KillingSpark/zstd-rs) and rewrote the encoder from scratch. The decoder and the encoder cover the full format and interoperate with the reference C implementation in both directions.

- Decoder: complete. It handles dictionaries, multi-frame and skippable-frame streams, and verifies the checksum on every path.
- Encoder: the full numeric level ladder from 0 to 22, with fast, dfast, chain, btlazy2, btopt and btultra strategy families modeled on libzstd. It supports checksums, raw-content and `zstd --train` formatted dictionaries, long-distance matching, and multithreaded compression.
- Multithreaded decompression.
- `no_std` with `alloc`. The `zstdx::compat` layer mirrors the [`zstd` crate](https://docs.rs/zstd) API shape with numeric levels and `io::Result`, which makes porting straightforward.

Performance is tracked against libzstd in `docs/src/dev/` ([snapshot](https://github.com/lxl66566/zstdx/blob/main/docs/src/dev/bench/snapshot.md)).

## Usage

One-shot:

```rust
let data = b"the quick brown fox jumps over the lazy dog";
let compressed = zstdx::bulk::compress(data, zstdx::Level::Fastest);
let restored = zstdx::bulk::decompress(&compressed, 0).unwrap();
assert_eq!(&restored[..], data);
```

Streaming over `io::Read` and `io::Write`:

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

Checksum, pledged size, worker threads, dictionaries and a forced window are set on the builder-style `zstdx::EncoderOptions` and `zstdx::DecoderOptions`. The [user guide](https://github.com/lxl66566/zstdx/blob/main/docs/src/user.md) walks through the full API. The reference documentation lives at `docs.rs/zstdx`.

## Feature flags

<!-- prettier-ignore -->
| Feature | Default | Effect |
|---|---|---|
| `std` | yes | `std::io` traits, threads, `compat` |
| `hash` | yes | XXH64 frame checksums, written and verified |
| `dict_builder` | no | dictionary training through `zstdx::dict` |

Without `std` the crate builds as `no_std` with `alloc`. Bulk, streaming and the low-level APIs still work, and thread options report `Error::Unsupported`.

## Performance

Benchmarks compare against the `zstd` crate (libzstd 1.5.7) on 32 MiB corpus shapes plus a 100 MB real-binary payload ([gen_big.sh](https://github.com/lxl66566/zstdx/blob/main/bench/gen_big.sh)). Every cell runs single-threaded unless noted, with checksums off and a roundtrip check, and run-to-run noise is ±10%. The figures below cover the streaming paths, which is what `Read` and `Write` users run. Full tables that include bulk are in [snapshot.md](https://github.com/lxl66566/zstdx/blob/main/docs/src/dev/bench/snapshot.md). Speedups are written as 3.1x, which means zstdx finishes in about a third of the reference time.

![Streaming encode: throughput vs compression ratio, zstdx vs libzstd](https://raw.githubusercontent.com/lxl66566/zstdx/main/assets/encode-pareto.svg)

Streaming encode at matched numeric levels is denser than libzstd on every level of every payload. json gains 4.7% density at the fastest tier and 20% at balanced (ratio 7.13 against 5.95). The dll payload is 13% to 17% denser from balanced up, and 49.6% denser at its fastest tier at speed parity.

It is faster at the fast tier on every payload, at the fastest tier on text by 3.1x, and at best, opt and ultra on text. The cells it trails are json at fastest, balanced and best, text at balanced, and dll at balanced, best, opt and ultra. Each of those cells still produces 1.7% to 17% fewer bytes than libzstd. json at best and ultra holds ratio parity or better at somewhat lower speed, and the remaining cells sit on or above the libzstd frontier.

On unknown-size text streams, libzstd's fastest tier emits a 1.84 MB frame at ratio 18.3, while zstdx emits 108 KB at ratio 309. That is 16.9x denser and 3.1x faster, the isolated gray point in the log-x panel. Reader pull sizes from 4 KiB to 1 MiB were swept for every cell, and speedups stay within 5% of the 64 KiB value.

![Streaming encode, 8 workers vs libzstd at 8 workers](https://raw.githubusercontent.com/lxl66566/zstdx/main/assets/encode-stream-mt.svg)

With 8 workers and 64 KiB pulls, streaming encode leads libzstd at every tier on json by 1.6x to 5.8x and at every tier on text by 1.1x to 3.7x. On the dll payload it leads at fast and opt, and trails at fastest, balanced, ultra and best, with best at 0.19x. Those cells run at 52% to 92% of zstdx's own bulk-mt throughput, which the bench documentation records as headroom.

![Streaming decode speedup over libzstd](https://raw.githubusercontent.com/lxl66566/zstdx/main/assets/decode-speedup.svg)

Streaming decode wins on incompressible data, by 1.2x to 1.3x on random and at parity on zeros. It trails on compressible shapes, where libzstd's edge is 1.04x to 1.34x, and the pull-size bands barely move from 4 KiB to 1 MiB reads. Bulk decode wins 17 of 18 cells by up to 4.8x. The 16-worker decoder reaches 1.23x libzstd's single-threaded streaming decode on json and 1.36x on dll100.

Across the full sweep of 5 shapes, 6 levels, both APIs and both threading modes, the geometric mean ratio is 8.7% denser than libzstd at matched numeric levels. Per-level tables for levels 1 to 22 are in [snapshot.md](https://github.com/lxl66566/zstdx/blob/main/docs/src/dev/bench/snapshot.md) and [matrix.md](https://github.com/lxl66566/zstdx/blob/main/docs/src/dev/bench/matrix.md).

## CLI

The companion `zstdx-cli` crate is a flag-compatible `zstd` command line. `zstdx file` writes `file.zst`, `-d` decodes, `-1` to `-19` and `--fast` select the level, `-T0` uses all cores, and `-D DICT` attaches a dictionary.

## Testing

Tests take two forms:

1. Tests on well-formed files must decode correctly and match their originals. Interop roundtrips against the reference implementation run through the dev-dependency on the `zstd` crate.
2. Tests on malformed input generated by the fuzzer. These inputs are garbage, so they may fail to decode. They must never panic the decoder.

## Fuzzing

The fuzz targets live in `crates/zstdx-fuzz`: `decode`, `decode_dict`, `encode`, `encode_stream`, `interop`, `fse` and `huff0`. From that directory, run `cargo +nightly fuzz run decode` or the same command with another target.

On a crash:

- The offending input is saved to the artifacts directory. `cargo test -p zstdx artifacts` reproduces it and shows where the decoder panics.
- Pull requests are welcome. An offending input is useful on its own when you cannot fix the crash.
