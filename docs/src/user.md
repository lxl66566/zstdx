# User guide

`zstdx` is a pure-Rust implementation of the Zstandard format ([RFC 8878]). It has a complete decoder and an encoder that covers the numeric level ladder from 0 to 22, supports dictionaries, and builds as `no_std`. Frames interoperate with the reference `zstd` implementation in both directions.

This page answers how to call the library, which options control the encoder and decoder, and which flags the CLI accepts.

[RFC 8878]: https://www.rfc-editor.org/rfc/rfc8878.pdf

## Installation

Library:

```toml
[dependencies]
zstdx = "0.2"
```

CLI:

```sh
cargo install zstdx-cli          # from crates.io
cargo install --path crates/zstdx-cli   # or from a checkout
```

## One-shot compression

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

`decompress` takes a capacity hint as its second argument. Prefer `0` over a wrong guess, because the buffer grows as needed. Frames that declare their content size in the header ignore a wrong hint too.

Repeated `compress` calls reuse a per-thread pooled encoder state, so hash and entropy table setup is paid once per thread.

## Streaming

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

`read::Decoder` transparently decodes concatenated frames, and `.single_frame()` stops after the first. The same machinery backs the convenience wrappers [`stream::encode_all`], [`stream::decode_all`], [`stream::copy_encode`] and [`stream::copy_decode`].

`read::Encoder::finish` requires reading the encoder to end of stream first. It refuses with `UnreadOutput` instead of truncating. `try_finish` is the abandon path and hands the reader back together with the error, so a caller dropping a half-read frame keeps the source.

## Porting from the zstd crate

`compat` mirrors the libzstd-bindings crate, `zstd` on crates.io, with numeric `i32` levels, `io::Result`, the same module layout and the same type names. Replace `zstd::` with `zstdx::compat::`:

```rust
use zstdx::compat;

let compressed = compat::bulk::compress(data, 3)?;
let original = compat::bulk::decompress(&compressed, 0)?;
```

`compat::stream` provides `Encoder` and `Decoder`, also re-exported at `compat::` like the zstd crate does, and `copy_encode` and `copy_decode`. `compat::dict` provides prepared `EncoderDictionary` and `DecoderDictionary` with one shared parse per handle. With the `dict_builder` feature it adds the `from_samples`, `from_continuous`, `from_sample_iterator` and `from_files` trainers. The bulk paths declare the frame content size like libzstd's one-shot compressor, and `upper_bound` answers `ZSTD_getFrameContentSize` semantics.

Items that rest on `zstd_safe` have no counterpart: `stream::{raw,zio}`, `with_context`, `set_parameter`, `context_mut`, and the `CDict`/`DDict` re-exports. The experimental items have none either: `include_magicbytes` and the by-reference `dict::*::new` constructors.

Known deviations:

- `compression_level_range` answers 1..=22, with no accelerated levels.
- `read::{Encoder,Decoder}` take `R: Read` directly and wrap it in no `BufReader`, so `get_ref` and `finish` expose `R`.
- `bulk::Decompressor::decompress` treats `capacity` as a growing hint, where the zstd crate errors.
- A prepared dictionary that fails to parse replays the error at use, where the zstd crate panics.
- `set_target_cblock_size` stays unimplemented, because sub-block splitting is falsified for ratio, as recorded in the [todo list](dev/todo.md).

`long_distance_matching(bool)` maps onto `EncoderOptions::long_distance_matching`. A value of true forces the far-match source on with libzstd's window-27 default when no window is forced. A value of false restores the automatic per-level arming, because the upstream bool cannot express an explicit disable. `with_ref_prefix` is the raw-dictionary path under libzstd's prefix name. A refPrefix loads raw content, which brings no entropy tables, no dictionary id and format-default repcodes.

The streaming types materialize their native backend on the first read or write. A failed start hands the wrapped reader or writer back, and `get_mut` and `finish` stay usable, so a retry re-runs the start. The native read decoder's eager first-frame parse may already have consumed bytes before it failed. Those bytes are not replayed, so a retried start keeps reading from wherever the source stands.

## Levels

`Level` is the numeric libzstd level from 0 to 22. [`Level::from_zstd(i32)`] maps exactly: negatives clamp to 1 and values above 22 clamp to 22. Named constants alias representative levels:

| Constant                   | Level | Strategy (libzstd term)     |
| -------------------------- | ----- | --------------------------- |
| `Level::Uncompressed`      | 0     | raw blocks, stored          |
| `Level::Fastest` (default) | 1     | single-probe hash           |
| `Level::Fast`              | 3     | dual-table hash             |
| `Level::Balanced`          | 9     | hash chain, two lazy steps  |
| `Level::Best`              | 13    | binary tree, two lazy steps |
| `Level::Opt`               | 17    | optimal parser              |
| `Level::Ultra`             | 19    | optimal parser, densest     |
| `Level::MAX`               | 22    |                             |

Every integer in between selects a real parameter row, and the default is `Fastest`.

## Options

Encoder and decoder options use builder-style sets that are validated at construction:

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

- `workers(n > 1)` works for bulk and the native streaming encoders, with or without a dictionary attached. A dictionary seeds the frame's first job. It falls back to single-threaded for raw-block levels and on single-core machines, and without `std` it reports [`Error::Unsupported`].
- `threads(n > 1)` needs frames with restart points, which this crate's MT encoder and libzstd `-T` output provide. Other frames decode sequentially.
- `InputShape::default().with_window_log(n)` forces the encoder window, clamped to 10..=27, for example to bound decoder memory. `.with_len(n)` feeds a known input size so the window and tables shrink to the source. A pledged size wins for the length when both are set.
- `include_dictid(false)` omits the Dictionary_ID field from the frame header while the dictionary stays fully in effect, which is libzstd's `ZSTD_c_dictIDFlag = 0`. Such frames decode only when the dictionary is handed to the decoder explicitly. On this crate's option-based decode entries the attached dictionary fills that role.
- `DecoderOptions::max_window_size(n)` rejects frames whose window exceeds `n` bytes. The default is `DEFAULT_MAX_WINDOW_SIZE`, which is 128 MiB plus one byte, so set a smaller cap on untrusted input.

### Long-distance matching

`long_distance_matching(LdmMode::Enable)` forces the far-match source on, and `LdmMode::Disable` forces it off. `LdmMode::Auto`, the default, keeps the engine's per-row window-threshold arming, so default frames are byte-identical.

`Enable` raises the window to 27 when no window is forced, using `ZSTD_LDM_DEFAULT_WINDOW_LOG`, ahead of the source-length clamp. `Disable` forces the source off where the automatic bars would arm.

Fidelity notes:

- An explicit enable on a source whose prefix parses to raw blocks keeps the stock behavior below the 64 MiB full window, because skipped blocks never reach the LDM indexer there.
- The tagged row strategies at levels 5-8 and 10-12 carry no LDM candidate feed in their parse, so enable is inert on them.
- On multithreaded frames the switch rides the dispatch for bulk and streaming alike, and every job, donation and shared-prefix build carries the frame's mode.
- `Disable` stands the cross-job captures down: the far screen, the pinned capture grids, the shared prefix fill, the stream core's far-dead strip screens and the fast-row capture staging. It also disarms LDM in every job, including the dictionary seed.
- `Enable` forces the far source on in every job at the raised window. The fast-row screen capture stands down under it, because its grid cannot cover the wider strip that the raised window introduces, while the chain rows' mid-size capture keeps its own bars.
- Two accepted divergences remain on the streaming side. An open-ended stream is unpledged and declares its header before content exists, so `Enable` stamps the unclamped raised window up front, which is the ST unpledged entry's own shape. The chain rows' whole-window strip schedule keeps its pre-existing bulk divergence on the full-window band.
- The job model's own artifacts apply to every mode: repcode gates, per-job entropy resets at job boundaries and non-capture job grids that scale with the worker count.

## Dictionaries

A frame compressed against a dictionary decodes only with the same dictionary. Raw-content and formatted dictionaries, the `zstd --train` shape, both work on either side:

```rust
use zstdx::{DecoderOptions, EncoderOptions, Level, bulk};

let eopts = EncoderOptions::new(Level::Fast).dictionary(&dict);
let compressed = bulk::compress_with(sample, &eopts)?;

let dopts = DecoderOptions::new().dictionary(&dict);
let original = bulk::decompress_with(&compressed, 0, &dopts)?;
```

Training is behind the `dict_builder` feature:

```rust
// All files under a directory as samples, dictionary capped at 110 KiB:
let mut dict = Vec::new();
zstdx::dict::create_raw_dict_from_dir("corpus/", &mut dict, 110 * 1024)?;

// Or in-memory samples, with entropy tables (formatted dictionary):
zstdx::dict::create_formatted_dict_from_samples(&[&s1, &s2], &mut dict, 110 * 1024);
```

Training is deterministic for a given sample set. Dictionaries pay off on short inputs, so keep them small relative to typical payloads.

Parse reuse: `EncoderDictionary::parse` runs the dictionary parse once for any number of compressions through `EncoderOptions::parsed_dictionary`. The compat `bulk::Compressor` caches the parse of a raw-attached dictionary the same way after its first use, and `FrameCompressor::set_parsed_dictionary` shares one parse across frames. Re-attaching a dictionary parses afresh, and so does changing a compat compressor's level, which detaches the dictionary.

## Bounded-memory decoding

For untrusted or huge frames, decode in batches, drain bytes that are no longer needed as match history, and repeat. Memory stays at roughly window size plus batch size regardless of frame size.

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

[`decoding::StreamingDecoder`] wraps this loop behind an `io::Read` for callers that want no manual control.

## Feature flags

| Feature        | Default | Effect                                      |
| -------------- | ------- | ------------------------------------------- |
| `std`          | yes     | `std::io` traits, threads, `compat`         |
| `hash`         | yes     | XXH64 frame checksums (write + verify)      |
| `dict_builder` | no      | dictionary training through `zstdx::dict`, and `zstdx-cli` passes it through for `zstdx --train` |
| `fuzz_exports` | no      | internal: exposes `fse`/`huff0` for fuzzing |

`seq_dump` and `job_trace` are internal instrumentation features for the bench tooling and have no stable interface.

Without `std` the crate builds as `no_std` with `alloc`. Bulk, streaming and the low-level APIs work, and thread options error with [`Error::Unsupported`].

## CLI

The `zstdx-cli` binary mirrors the `zstd` command line with flags and no subcommands. Compression is the default, `-d` decompresses, and with no FILES or with `-` it streams stdin to stdout.

- Default output names are `FILE.zst` when compressing and the name without the `.zst` suffix when decompressing. Inputs are kept unless `--rm` is given.
- Levels are `-1` to `-19`, default 3, and `--fast[=N]` selects negative levels. `--ultra` unlocks 20 to 22.
- `-T N` sets worker threads, `-T0` uses all cores, and `--single-thread` equals `-T1`. `-D DICT` compresses or decompresses against a dictionary.
- `-c` writes to stdout and `-o FILE` sets the output for a single input. `-k` keeps inputs, which is the default. `-f` overwrites without asking, `-r` recurses into directories, `-t` tests archives, and `-q` and `-v` adjust verbosity.
- `--[no-]progress` overrides the auto progress-bar rule, which shows a bar on an interactive stderr with a known size. The bar never pollutes pipes.
- `--ignore-errors` is rejected with `Incorrect parameter: --ignore-errors`, matching zstd 1.5.7, which removed the flag.
- `--long[=N]` forces the window log. The default is 27, the engine's maximum, and the accepted range is 10 to 27. A known source size still shrinks the window to the source, as zstd does. Unlike zstd, `--long` alone leaves the long-distance matcher off, because the engine arms LDM by level row once the effective window reaches 32 MiB.
- `--[no-]check` toggles the frame checksum, default on. Both forms are compression-only. Decoding always validates a present checksum, and the flags are rejected in decode modes instead of ignored.
- `--stream-size=N` pledges the exact stdin size, and a mismatch fails, as in zstd. `--size-hint=N` sizes the encoder row for an approximate stdin size without a header promise. Both are stdin-only and rejected for file inputs, whose sizes are already pledged.
- `-M N` and `--memory=N` bound decode memory by capping the frame window, with zstd's syntax of bare bytes or `K`, `M` and `G` suffixes. Values below 1K reject every frame and are refused, as in zstd. Compression rejects the flag, where zstd silently ignores it.
- `--filelist LIST`, alias `--file`, reads the input list from LIST with one path per line, and `-` reads the list itself from stdin.
- `--sparse` and `--no-sparse` are rejected with an explicit unsupported error, because output is always dense and sparse decode-to-seek is not implemented.
- A failed compression or decompression to a file removes the partial output it created, mirroring zstd.
- Overwrite prompts read from the controlling terminal at `/dev/tty`, never from piped stdin. Without a terminal, existing outputs are refused unless `-f` is given.

### Patch mode

`--patch-from=REF` switches to diff mode. Compression treats REF as match history, which is a raw-content dictionary, so REF needs no dictionary header and no dictID lands in the frame. The window is sized from the source like zstd: the next power of two above it, clamped to 10 to 27 with a warning. `--long`'s value is ignored, as in zstd. Decompression with `-d --patch-from=REF` applies the patch with the same REF and raises `-M`'s bound to cover it. Only one input is allowed, stdin compression requires `--stream-size`, and the flag implies `--ultra`. Frames interoperate with zstd's `--patch-from` in both directions.

### Adaptive levels

`--adapt[=min=N,max=N]` adapts the compression level to I/O conditions during compression. The input compresses as independent 8 MiB chunk frames, which is zstd's `--adapt` window. After each chunk, at most once per zstd's 1/6 s refresh, the level steps by 1. It steps up when input or output cannot keep up with compression, so spare time buys ratio. It steps down when compression lags the input, using zstd's 9/8 and 33/32 thresholds. `min` and `max` bound the walk and default to 1 to 22, the engine's range, because zstd's negative rungs do not exist here. The starting level clamps into the bounds, like zstd. The two keys may appear in either order, and at least one is required.

Divergence: zstd re-levels per worker job inside one frame and requires `-T`, while zstdx re-levels per chunk frame because the library fixes encoder options at construction. A stream under one chunk never steps, and history does not carry across chunk boundaries. With any `-T`, workers apply within each chunk. `-vv` traces the decisions, where zstd prints its own at `-vvvv`. `--stream-size` and `--size-hint` are rejected with `--adapt`.

### Listing frames

`-l` and `--list` print frame information without decompressing: frame and skip counts, compressed and decompressed sizes, ratio, checksum and dictID. Headers are parsed and block headers walked only. The column layout, the `-v` field block, the `-vv` raw byte counts, the grouped totals and the error categories for not-zstd, truncated and frame errors match zstd 1.5.7 byte for byte. stdin is refused. `-l` conflicts with the compression and decompression modes and with `-o`.

### Benchmark mode

`-b#` benchmarks files at compression level #, default 3, as a port of zstd 1.5.7's `benchzstd` down to the result-line formats. Whole files load into memory. `-B#` and `--split=#` cut them into independent chunks. Each level spends `-i#` seconds per phase, default 3, and reports the fastest measured round. `-e#` extends to a level range. `-T#` multithreads the benchmarked compression. `-d` or `-bd` selects the decode-only benchmark. With no FILES a synthetic 10 MB "Lorem ipsum" corpus runs and is byte-identical to zstd's generator. Results and `\r` progress go to stdout, and `-q` prints the one-line-per-level summary instead. Benchmark frames carry no checksum, which matches libzstd's library default and differs from normal CLI compression. Level 0 is the engine's raw-store level, while libzstd's level 0 selects its default row.

### Dictionary training

`--train` builds a dictionary from sample files through the `dict_builder` feature. It takes a plain file list, and `-r` recurses into directories. stdin is unsupported. `-o` sets the output, default `dictionary`, and `--maxdict=#` caps its size with zstd suffix syntax, default 112640 and floor 256. The output is a formatted dictionary with magic, dictID and entropy tables, and zstd itself can use it. Built without the feature, the flag fails with a clear error. `--dictID`, `--train-cover`, `--train-fastcover` and `--train-legacy` are rejected loudly, because the trainer derives the dictID from content and implements one algorithm.

```sh
zstdx trace.log                          # → trace.log.zst, level 3
zstdx -19 -o data.zst data.csv
zstdx -D api.dict -9 req.json            # dictionary-compressed
zstdx --long=26 -T0 archive.tar          # 64 MiB window, all cores
zstdx -d -M 128M out.zst                 # decode within a memory cap
zstdx -d out.zst                         # → out
zstdx -T0 -c big.bin | ssh host 'zstd -d > big.bin'
zstdx -l -v archive.tar.zst              # frame info without decompressing
zstdx --train -r logs/ --maxdict=64K -o logs.dict
zstdx -b1 -e3 -i5 trace.log             # zstd-style benchmark, levels 1-3
zstdx -bd -i5 trace.log.zst             # decode-only benchmark
zstdx -D logs.dict -9 app.log             # trained-dictionary compression
zstdx --patch-from=v1.tar -19 v2.tar      # diff-style: v2 compressed against v1
zstdx -d --patch-from=v1.tar v2.tar.zst   # apply the patch
zstdx --adapt=min=3,max=12 -T4 backup.tar # level walks 3-12 with I/O conditions
```

## Current limitations

- Encoder speed is split by tier and corpus shape. It is ahead on several tiers and behind on the json fastest scan core and the best-tier tree core. Bulk decode is ahead across the board. Measured numbers are in [Current Status](dev/status.md).
- Dictionaries ride the match-history and entropy-seed path, without dedicated dictionary-search matcher templates. Dictionary encode is itself multithreaded, because `-T` seeds the frame's first job from the dictionary.

[`Level::from_zstd(i32)`]: https://docs.rs/zstdx/latest/zstdx/level/struct.Level.html#method.from_zstd
[`stream::encode_all`]: https://docs.rs/zstdx/latest/zstdx/stream/fn.encode_all.html
[`stream::decode_all`]: https://docs.rs/zstdx/latest/zstdx/stream/fn.decode_all.html
[`stream::copy_encode`]: https://docs.rs/zstdx/latest/zstdx/stream/fn.copy_encode.html
[`stream::copy_decode`]: https://docs.rs/zstdx/latest/zstdx/stream/fn.copy_decode.html
[`decoding::StreamingDecoder`]: https://docs.rs/zstdx/latest/zstdx/decoding/struct.StreamingDecoder.html
[`Error::Unsupported`]: https://docs.rs/zstdx/latest/zstdx/enum.Error.html#variant.Unsupported
