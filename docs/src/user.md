# User Guide

`zstdx` is a pure-Rust implementation of the Zstandard format ([RFC 8878]): a complete decoder, an encoder covering the full level 0–22 ladder, optional dictionaries, `no_std`-capable. Frames are interoperable with the reference `zstd` implementation in both directions.

[RFC 8878]: https://www.rfc-editor.org/rfc/rfc8878.pdf

## Installation

Library:

```toml
[dependencies]
zstdx = "0.9"
```

CLI:

```sh
cargo install zstdx-cli          # from crates.io
cargo install --path crates/zstdx-cli   # or from a checkout
```

## One-shot compression (`zstdx::bulk`)

```rust
use zstdx::{Level, bulk};

let data = b"the quick brown fox jumps over the lazy dog";

let compressed = bulk::compress(data, Level::Fastest);
let original = bulk::decompress(&compressed, 0).unwrap();
assert_eq!(&original[..], data);

// Decode into a caller-owned buffer (no allocation):
let mut buf = vec![0u8; data.len()];
let written = bulk::decompress_to_buffer(&compressed, &mut buf).unwrap();
assert_eq!(&buf[..written], data);
```

`decompress`'s second argument is a capacity hint; prefer `0` over a wrong guess (the buffer grows as needed). Frames that declare their content size in the header ignore a wrong hint too.

Repeated `compress` calls reuse a per-thread pooled encoder state, so they don't re-pay hash/entropy table setup.

## Streaming (`zstdx::stream`)

Writer-shaped and reader-shaped codecs:

```rust
use std::io::{Read, Write};
use zstdx::{Level, stream};

let mut compressed = Vec::new();
{
    let mut enc = stream::write::Encoder::new(&mut compressed, Level::Fast)?;
    enc.write_all(b"some ")?;
    enc.write_all(b"payload")?;
    enc.finish()?;          // writes the end-of-frame marker; auto_finish() defers to Drop
}

let mut dec = stream::read::Decoder::new(&compressed[..])?;
let mut original = Vec::new();
dec.read_to_end(&mut original)?;
```

`read::Decoder` transparently decodes concatenated frames; `.single_frame()` stops after the first. Convenience wrappers over the same machinery: [`stream::encode_all`], [`stream::decode_all`], [`stream::copy_encode`], [`stream::copy_decode`].

## Porting from the `zstd` crate (`zstdx::compat`)

`compat` mirrors the libzstd-bindings crate (`zstd` on crates.io): numeric `i32` levels, `io::Result`, same module layout and type names. Replace `zstd::` with `zstdx::compat::`:

```rust
use zstdx::compat;

let compressed = compat::bulk::compress(data, 3)?;
let original = compat::bulk::decompress(&compressed, 0)?;
```

`compat::stream` provides `Encoder`/`Decoder` and `copy_encode`/`copy_decode`. Not covered: `zstd_safe`, trained-dict generation via `zstd::dict` (use `zstdx::dict`), and `multithread(n > 1)` on streaming encoders (use the native `EncoderOptions::workers`).

## Levels

`Level` is the numeric libzstd level (0–22); [`Level::from_zstd(i32)`] maps exactly (negatives clamp to 1, values above 22 to 22). Named constants alias representative levels:

| Constant | Level | Strategy (libzstd term) |
|---|---|---|
| `Level::Uncompressed` | 0 | raw blocks, stored |
| `Level::Fastest` (default) | 1 | single-probe hash |
| `Level::Fast` | 3 | dual-table hash |
| `Level::Balanced` | 9 | hash chain, two lazy steps |
| `Level::Best` | 13 | binary tree, two lazy steps |
| `Level::Opt` | 17 | optimal parser |
| `Level::Ultra` | 19 | optimal parser, densest |
| `Level::MAX` | 22 | |

Every integer in between selects a real parameter row; the default is `Fastest`.

## Options

Builder-style option sets, validated at construction:

```rust
use zstdx::{EncoderOptions, Level, bulk};

let data = b"the quick brown fox jumps over the lazy dog";

let opts = EncoderOptions::new(Level::Balanced)
    .checksum(true)                          // default on (with the `hash` feature)
    .pledged_size(Some(data.len() as u64))   // declared in the frame header
    .workers(4);                             // >1: multithreaded compression (std builds)
let compressed = bulk::compress_with(data, &opts)?;

let dopts = zstdx::DecoderOptions::new().threads(4);   // parallel decode
let original = bulk::decompress_with(&compressed, 0, &dopts)?;
```

- `workers(n > 1)` works for bulk and the native streaming encoders. Falls back to single-threaded for raw-block levels, on single-core machines, or when a dictionary is attached. Without `std`: [`Error::Unsupported`].
- `threads(n > 1)` needs frames with restart points (this crate's MT encoder or libzstd `-T` output); other frames decode sequentially.
- `InputShape::default().with_window_log(n)` forces the encoder window (clamped to 10..=27), e.g. to bound decoder memory; `.with_len(n)` feeds a known input size so window and tables shrink to the source. A pledged size wins for the length when both are set.
- `DecoderOptions::max_window_size(n)` rejects frames whose window exceeds `n` bytes (default cap: 128 MiB) — use it on untrusted input.

## Dictionaries

A frame compressed against a dictionary decodes only with the same dictionary. Raw-content and formatted (`zstd --train`-style) dictionaries both work on either side:

```rust
use zstdx::{DecoderOptions, EncoderOptions, Level, bulk};

let eopts = EncoderOptions::new(Level::Fast).dictionary(&dict);
let compressed = bulk::compress_with(sample, &eopts)?;

let dopts = DecoderOptions::new().dictionary(&dict);
let original = bulk::decompress_with(&compressed, 0, &dopts)?;
```

Training, behind the `dict_builder` feature:

```rust
// All files under a directory as samples, dictionary capped at 110 KiB:
let mut dict = Vec::new();
zstdx::dict::create_raw_dict_from_dir("corpus/", &mut dict, 110 * 1024)?;

// Or in-memory samples, with entropy tables (formatted dictionary):
zstdx::dict::create_formatted_dict_from_samples(&[&s1, &s2], &mut dict, 110 * 1024);
```

Training is deterministic for a given sample set. Dictionaries should stay small relative to typical payloads — they pay off on short inputs.

## Bounded-memory decoding (low-level `FrameDecoder`)

For untrusted or huge frames: decode in batches, drain bytes that are no longer needed as match history, repeat. Memory stays at roughly window size + batch size regardless of frame size.

```rust
use zstdx::decoding::{BlockDecodingStrategy, FrameDecoder};

let mut dec = FrameDecoder::new();
dec.init(&mut source)?;
while !dec.is_finished() {
    dec.decode_blocks(&mut source, BlockDecodingStrategy::UptoBytes(64 * 1024))?;
    if let Some(ready) = dec.collect() {
        consume(ready);
    }
}
if let Some(rest) = dec.collect() {
    consume(rest);
}
```

[`decoding::StreamingDecoder`] wraps this loop behind an `io::Read` if you don't need the control.

## Feature flags

| Feature | Default | Effect |
|---|---|---|
| `std` | ✓ | `std::io` traits, threads, `compat` |
| `hash` | ✓ | XXH64 frame checksums (write + verify) |
| `dict_builder` | — | dictionary training (`zstdx::dict`) |
| `fuzz_exports` | — | internal: exposes `fse`/`huff0` for fuzzing |

Without `std` the crate builds as `no_std` + `alloc`; bulk, streaming, and the low-level APIs work, thread options error with [`Error::Unsupported`].

## CLI

```
zstdx-cli compress <FILE> [OUTPUT] [-l LEVEL] [-D DICT]
zstdx-cli decompress <FILE.zst> [OUTPUT] [-D DICT]
```

- Default output names: `FILE.zst` when compressing, the archive name without extension when decompressing.
- `-l` is the numeric level 0–22, default 1 (0 stores uncompressed).
- `-D` compresses against a dictionary; decompressing such a frame requires the same `-D`.
- Shows a progress bar and the final size ratio.

```sh
zstdx-cli compress trace.log                    # → trace.log.zst, level 1
zstdx-cli compress -l 19 data.csv out.zst
zstdx-cli compress -D api.dict -l 9 req.json    # dictionary-compressed
zstdx-cli decompress out.zst                    # → out
```

## Current limitations

- Encoder speed trails libzstd on most levels (bulk decode is ahead; measured numbers in [Current Status](dev/status.md)).
- No superblock, preSplit, or C FFI.
- `compat` streaming encoders reject multithreading (the native streaming API supports it).

[`Level::from_zstd(i32)`]: https://docs.rs/zstdx/latest/zstdx/level/struct.Level.html#method.from_zstd
[`stream::encode_all`]: https://docs.rs/zstdx/latest/zstdx/stream/fn.encode_all.html
[`stream::decode_all`]: https://docs.rs/zstdx/latest/zstdx/stream/fn.decode_all.html
[`stream::copy_encode`]: https://docs.rs/zstdx/latest/zstdx/stream/fn.copy_encode.html
[`stream::copy_decode`]: https://docs.rs/zstdx/latest/zstdx/stream/fn.copy_decode.html
[`decoding::StreamingDecoder`]: https://docs.rs/zstdx/latest/zstdx/decoding/struct.StreamingDecoder.html
[`Error::Unsupported`]: https://docs.rs/zstdx/latest/zstdx/enum.Error.html#variant.Unsupported
