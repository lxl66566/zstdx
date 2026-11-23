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

Benchmarks compare against the `zstd` crate (libzstd 1.5.7) on 32 MiB corpus shapes plus a 100 MiB binary payload ([gen_big.sh](https://github.com/lxl66566/zstdx/blob/main/bench/gen_big.sh)). Each cell runs single-threaded with checksums off unless noted, and passes a roundtrip check. Run-to-run noise is ±10%. A speedup of 3.1x means zstdx finishes in about a third of the reference time. The figures cover the streaming paths, and the full tables including bulk are in [snapshot.md](https://github.com/lxl66566/zstdx/blob/main/docs/src/dev/bench/snapshot.md) and [matrix.md](https://github.com/lxl66566/zstdx/blob/main/docs/src/dev/bench/matrix.md).

![Streaming encode: throughput vs compression ratio, zstdx vs libzstd](https://raw.githubusercontent.com/lxl66566/zstdx/main/assets/encode-pareto.svg)

Each marker is one compression level from 1 to 22. The line under a side is the throughput of its fastest level at that density, and the shaded areas are reachable by one side only. A band label gives the throughput gap at equal density. On unknown-size text streams libzstd's level 1 falls to ratio 18.3, which places its marker at the left edge.

Streaming encode makes the smaller frame at 47 of the 66 matched level and payload pairs. On json, level 9 is 1.5x faster at a 21% higher ratio, and between ratios 6.2 and 6.4 the fastest libzstd level runs at 9 MiB/s where zstdx's level 1 runs at 623 MiB/s. On the dll payload, levels 1 to 4 emit 31% to 50% fewer bytes at 1.0x to 1.2x the speed, and levels 13 to 19 emit 13% to 20% fewer bytes. Every text level sits on or above the libzstd frontier. Where a matched level is slower, it still emits the smaller frame or stays within 2% of the libzstd size. Across the full sweep of 5 shapes, 6 levels, both APIs and both threading modes, the geometric mean ratio is 8.7% denser than libzstd at matched levels.

![Streaming encode, 8 workers vs libzstd at 8 workers](https://raw.githubusercontent.com/lxl66566/zstdx/main/assets/encode-stream-mt.svg)

With 8 workers, streaming encode is 1.3x to 4.8x faster at every tier on json and text. On the dll payload it is 1.6x to 2.2x faster at fast, opt and ultra, and 0.5x to 0.9x at the other three tiers.

![Streaming decode speedup over libzstd](https://raw.githubusercontent.com/lxl66566/zstdx/main/assets/decode-speedup.svg)

Streaming decode runs at 0.95x to 1.18x libzstd's speed on compressible shapes and at 1.2x on random data. The 16-worker decoder is 1.9x faster than libzstd's single-threaded decoder on json and the dll payload, and multi-frame inputs scale to up to 3.2x the single-threaded decoder.

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
