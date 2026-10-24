# zstdx-bench

Benchmark and development-tool harness for the `zstdx` crate. It runs wall-clock A/B against the `zstd` crate, which binds libzstd, and it holds the correctness and validation tools. The crate stays outside the library so development dependencies never leak into it.

This page answers how to run each benchmark, which flags select corpus shapes and levels, and which subcommand gates a given kind of change.

- Build and run with `cargo run --release -p zstdx-bench -- <subcommand> [flags]`. Always use `--release`, because debug builds measure nothing.
- The corpus prerequisite is one run of `bash bench/gen_corpus.sh`. It writes 32 MiB files for five shapes: `json`, `text`, `skewed`, `random` and `zeros`. It also writes `.zst1/3/9` variants, plus `.zst19` for json, text and skewed.
- Timing subcommands share an interleaved A/B harness in `src/common.rs`. Both sides alternate round by round and the per-round ratio is the primary verdict, which is resistant to clock drift. Each side gets a time budget from `--budget-ms`, default 500 ms, or from the `BENCH_BUDGET_MS` environment variable.
- `ZSTDX_DEC_SIMD_TIER=scalar|bmi2` is a library environment override. Unset means runtime detection, and an invalid value panics. It forces the decode SIMD tier, so running the decode subcommands `files`, `mtcheck`, `corrupt` and `prof dec` under it gates the forced tiers on any machine.
- Deterministic, instruction-count based benchmarks under valgrind live in the sibling crate [`zstdx-gungraun`](../zstdx-gungraun/Readme.md).

Level vocabulary is the same everywhere. The six ladder levels are `fastest`, `fast`, `balanced`, `best`, `opt` and `ultra`, paired in benchmarks with libzstd at numeric levels 1, 3, 9, 13, 17 and 19. `--shape` selects corpus shapes and `--level` selects levels from a comma-separated list. Omit either flag to mean all values. `matrix --level` also accepts numeric levels from 1 to 22.

## Subcommands

### `ratio`: compression ratio sweep

One deterministic pass per cell covers every level, both bulk and streaming, and both single-threaded and multi-threaded. The full corpus gives 120 cells, with both zstdx and libzstd measured per cell and no timing loops. Sizes are deterministic, so `diff` two runs or two builds to spot a regression. A geo-mean summary block is printed last, so `tail -n 40 <log>` shows the verdict. Every zstdx cell roundtrip-gates through both decoders before it is reported.

The reported percentage is `zstdx_ratio / zstd_ratio - 1`, so a positive value means zstdx is denser than libzstd.

```bash
cargo run --release -p zstdx-bench -- ratio                  # full sweep (~min, parallel)
cargo run --release -p zstdx-bench -- ratio --shape json,text --level ultra
cargo run --release -p zstdx-bench -- ratio --mode bulk-st,bulk-mt
cargo run --release -p zstdx-bench -- ratio --no-ref         # our sizes only
```

`--shape`, `--level` and `--mode` filter cells. `--mt-workers` sets the worker count both sides use in the mt modes and defaults to 4. `--parallel` sets how many cells compress concurrently and defaults to 8. Cells only measure sizes, so single-threaded cells still run the single-threaded encoder and the pool merely runs many of them at once. Keep `--parallel` within RAM, because each cell transiently holds a few 32 MiB-scale buffers. Higher parallelism is fine on large machines. Progress goes to stderr as `[n/total] …`, and rows and the summary go to stdout.

Run this after every encoder change that may alter output sizes. Use `dump` below when the output must stay byte-identical.

### `matrix`: wide wall-clock A/B

This is the heavyweight tool. Narrow it with `--mode` and filters instead of running `all` while iterating. Sections are `dec-st` for bulk and streaming decode, `dec-mt` for our parallel decode scaling where libzstd has no MT decode, `enc-st` for all ladder levels with size and ratio per cell, `enc-mt` for equal worker counts plus a zstd warm-pool reference, and `enc-stream` for streaming encode ST and MT against libzstd plus the bulk-MT ceiling. Every cell is roundtrip-gated first.

```bash
cargo run --release -p zstdx-bench -- matrix --mode enc-st --shape json
cargo run --release -p zstdx-bench -- matrix --mode dec-st --budget-ms 2000
cargo run --release -p zstdx-bench -- matrix --mode enc-mt --workers 8 --mt-workers 8
cargo run --release -p zstdx-bench -- matrix --mode enc-st --full-ladder --shape json --level 1,7,22
```

`--full-ladder` swaps the `enc-st` level axis to every numeric level from 1 to 22, with both sides at the same number. `--level` accepts tier names too, which then select their numeric level. The mt and stream sections keep their tier subsets. The full 1 to 22 ladder is extremely heavy, so run it once before a release and keep it out of daily iteration. Combine it with `--shape`, `--level` and `--budget-ms` to narrow smoke passes.

`--pull <size>` takes a comma-separated list of bytes with an optional k or m suffix and defaults to `64k`. It loops the streaming cells over several reader pull sizes. The `dec-st` streaming cells and the `enc-stream` ST cells, all six ladder tiers, report one cell per size. The 64 KiB cell keeps its unsuffixed name and other sizes get suffixes such as `.4k` and `.1m`. The `enc-stream` MT section always runs at 64 KiB.

`--file <path>` is repeatable and appends payload files outside the corpus. A `.zst*` file adds `dec-st` cells, verified against the raw counterpart found next to it under the stem or `<stem>.raw`. Any other file adds raw-input cells to `enc-st` and to both `enc-stream` sections, ST and MT. Other sections reject `--file`. See "Large payloads" below for the 100 MB and larger recipe.

### Large payloads of 100 MB and more

`bash bench/gen_big.sh` concatenates real system ELF binaries gathered from `/run/current-system/sw/bin`. The mix covers code, data and strings, which resembles a DLL. The output is `bench/big/dll100.raw` at about 100 MB, plus `.zst1/3/9/19` variants and a 32 MiB `dll32.raw` subset. These files are gitignored. Edit the script's `find` root for non-NixOS systems. Any explicit file path works as a `--file` payload, so ad-hoc large binaries need no corpus changes:

```bash
cargo run --release -p zstdx-bench -- matrix --mode enc-st \
    --file bench/big/dll100.raw --level fastest,fast,balanced
cargo run --release -p zstdx-bench -- matrix --mode dec-st \
    --file bench/big/dll100.zst3 --file bench/big/dll100.zst9
```

### `small`: small-payload encode

Measures per-call encode throughput for 1 KiB to 1 MiB payloads, including allocator effects, with zstdx against libzstd at the same numeric level. `--level` defaults to `fastest` and accepts tier names or numeric levels from 1 to 22. The timed zstdx side runs checksum-off, which matches the default of `zstd::bulk::compress`. The crate's `hash`-feature default of checksummed frames plus a sidecar thread above 256 KiB is a real user cost and not a fair A/B. `--size` and `--impl zstdx|zstd` pin one size or implementation for profiler attribution.

```bash
cargo run --release -p zstdx-bench -- small --size 4096
cargo run --release -p zstdx-bench -- small --shape json --level 1,3,9 --size 65536,262144
```

### `files`: decode timing on explicit files

Decodes the given `.zst` files under a time budget and verifies against the raw counterpart next to each file. Both the `z000033.zst` and `json.zst3` naming conventions work. `--threads 4,8,16` additionally times our MT decoder at those worker counts, since zstd has no MT decode counterpart.

```bash
cargo run --release -p zstdx-bench -- files bench/corpus/*.zst3
cargo run --release -p zstdx-bench -- files --threads 4,8,16 frames/*.zst3
```

### `emitframe`: emit one corpus file as a frame

Encodes one raw file to a frame for the downstream analysis tools. It supports bulk and stream paths, single and multithreaded modes, and any level, with an in-process roundtrip gate. It prints the encoder's exact MT job size as `job_size N`, and `piecepipe` must be given the same value. `ZSTDX_MT_RAMP_BYTES=<D>` arms the deep-offset ramp at job starts, see `match_generator::RampGate`.

```bash
ZSTDX_MT_RAMP_BYTES=2097152 cargo run --release -p zstdx-bench -- emitframe bench/corpus/json.raw /tmp/json.zst3 --level 3 --workers 4
```

### `piecepipe`: piece-pipeline critical-path analysis

Simulates parallel stage-B execution on one frame. It decodes the frame through the `seq_dump` executor hook, which records every match's absolute position and resolved offset exactly as executed. It cuts the output into pieces at `--job-size` boundaries and schedules the pieces under exact per-byte source dependencies at rate 1. The unlimited-worker wall is the critical path, so the serial or piece wall upper-bounds what piece-parallel stage B could win. `--depth D` audits an encoder-side depth guarantee for crossing reads shallower than D below a piece start. The subcommand needs `--features seq_dump`, and pieces must be cut at the real encode job boundaries given by `emitframe`'s `job_size` line.

```bash
cargo run --release -p zstdx-bench --features seq_dump -- piecepipe /tmp/json.zst3 --job-size 4194316 --depth 2097152
```

### `jobdecomp`: per-job cost decomposition

Runs one raw file through the bulk-mt, stream-mt and single-threaded encoders and reports the `job_trace` spans per component. The components are the strip tree-fill, which is the job history the opt tier re-indexes through its binary tree, the in-job block-boundary refills, the hash3 strip ingestion, ultra's job-boundary seed parse, the per-job state reset, the LDM prefill and the per-job head-table clear. `head clear` is a sub-span of `prefill` and reads zero on chain rows since the head origin landed. The caller-side shared-prefix-fill build `spf build` is posting-thread work that the job sums cannot see, including retired and discarded builds. The report also covers the spf build's ldm and chain split, the adopters' remainder fills, and the finish tail's own phases `finish` and `settle` with the parked donation wait, `post` and `drain`. `--pledge` declares the input size on the stream-mt cells. Each component is reported against the summed job time, which exceeds the wall clock on the mt paths because jobs run in parallel. Component shares, not walls, are the verdict, and interleaved A/B remains the wall tool. The subcommand needs the encoder trace hooks from `--features job_trace`, which are compiled out of timing builds like `seq_dump`:

```bash
cargo run --release -p zstdx-bench --features job_trace -- jobdecomp \
    bench/corpus/text.raw --level opt,ultra --workers 8 --iters 3
```

### `prof`: solo profiling loops

Tight single-side loops for `perf` with no reference side, so samples land in one code path. The forms are `prof dec <file.zst> [iters]`, `prof enc <zstd-level> <iters> <file...>` and `prof enc-stream <zstd-level> <iters> <file>`. `RUZ_CKSUM=1` switches the encode path to checksummed bulk.

```bash
cargo run --release -p zstdx-bench -- prof enc 3 20 bench/corpus/text.raw
```

### `dictmt`: dictionary multithreading wall A/B

Times the dictionary ST slice entry against the mt job path, where the frame's first job is seeded from the dictionary, at each worker count on one raw input. Rounds interleave. It prints the deterministic size table first: dict-st, dict-mt with the job-split delta, and plain-frame references. It keeps a plain st-vs-mt cell as the no-regression sanity gate. Every timed cell roundtrips through our decoder with the dictionary before timing. `--dict` takes a formatted dictionary produced by `zstdx-bench train --formatted`.

```bash
cargo run --release -p zstdx-bench -- dictmt bench/corpus/json.raw \
    --dict bench/dict-json --level fastest,fast,balanced,best --workers 4,8
```

### `dump`: byte-exact output snapshots

Compresses every corpus `.raw` into a directory so two builds can be compared byte for byte. Run `dump <dir>` per build, then `diff -r dir_a dir_b`. This is the regression gate for optimizations that must leave the encoder output unchanged. `--all-levels` covers the six ladder tiers at their paired numbers, with files tagged `.l1`, `.l3`, `.l9`, `.l13`, `.l17` and `.l19`. `--workers N` routes compression through the multithreaded job path so the gate covers mt bytes too. Every emitted frame is roundtrip-gated through both in-process decoders and the libzstd CLI before it lands.

`--stream` swaps the bulk encoders for the streaming cores. This is the read-side incremental core that `matrix --mode enc-stream` drives, with one `read_to_end` and no flush. It emits every shape and level as the {st, mt} by {pledged, open} grid: `<shape>.raw.stream[.lN].zst`, `.stream-pledged`, `.stream-mt<W>` and `.stream-mt<W>-pledged`. The mt cells use `--workers`, default 4. One command then becomes the stream differential gate: `dump --stream --all-levels <dir>` per build, then `diff -r` the directories. The run is deterministic between runs, mt cells included, because the bytes carry no wall-clock or thread timing. The unpledged mt cells ride the Growing grid, which is keyed on the worker count by design, so never diff them across `--workers` values. Bulk anchors on the full corpus grid are as follows. Pledged-mt is byte-identical to the `--workers` bulk dump except the Balanced row on skewed and text l9, which is the reach probe's stream and bulk verdict split at -3.5 KB and -12 B. Pledged-st matches the bulk dump modulo the 4-byte declared-size header field, except json l13 where the stream is 721.9 KB smaller and is the better side. The open cells have no byte-equal bulk counterpart because of the undeclared header, the length-keyed bulk rows and the probe-less ST stream cores, which diverge by design.

```bash
cargo run --release -p zstdx-bench -- dump /tmp/dump-a
cargo run --release -p zstdx-bench -- dump /tmp/dump-a --all-levels
cargo run --release -p zstdx-bench -- dump /tmp/dump-a --all-levels --stream
```

### `corrupt`: corruption smoke test

Flips bytes in a valid frame, with `--rounds` setting the number of copies, and asserts that the decoder either errors or succeeds without panicking or hanging. Both the streaming and the flat paths are exercised.

```bash
cargo run --release -p zstdx-bench -- corrupt bench/corpus/json.zst3
```

### `mtcheck`: MT decode validation

Decodes every `.zst*` file in a directory with several worker counts from `--workers`, default `2,4,8,16`, and byte-compares the output against the `zstd` CLI.

```bash
cargo run --release -p zstdx-bench -- mtcheck bench/corpus
```

### `parity`: decode-parity differential against the zstd CLI

The institutionalized craft-and-mutate differential, tracked as todo 21b and 21c, with design and sweep history in `docs/src/dev/review/diffbench.md`. The corpus is deterministic: 24 real frames from our encoder over 4 payload families at levels 1, 3 and 19 with checksums on and off, plus the handcrafted shapes of `zstdx::parity`, which are the same bytes the lib tests pin. Mutations are an exhaustive truncation ladder, rotating single-byte XOR flips, OR scans, strided deletes and strided inserts. The OR scans apply `|=` on the 18 header bytes with 4 masks, and `|= 0x03` at every position, which is the class that found the modes-byte reserved-bits hole that XOR misses. Every case is decoded by zstdx on a serial arm plus the `--mt` arms, and by `zstd -dcq` with one CLI spawn per case, which is the reference by design. Accept or reject verdicts and output bytes are compared. Any zstdx panic or any verdict or output divergence exits nonzero, so the mode can gate. The one documented CLI-versus-one-shot asymmetry, on window-violating offsets, is parity.md finding 7. It is adjudicated by libzstd's own one-shot `ZSTD_decompress` and reported as `oneshot-asymmetry` rather than a failure. A full sweep is multi-core work, so wrap it in `flock /root/programs/fork/zstd-bench.lock`.

```bash
cargo run --release -p zstdx-bench -- parity                 # full sweep (~38k cases)
cargo run --release -p zstdx-bench -- parity --cases 2000     # deterministic stride sample
cargo run --release -p zstdx-bench -- parity --mt 2,4,8       # more MT arms
cargo run --release -p zstdx-bench -- parity --dict bench/dict-json
```

`--seed` reseeds the payload RNG. `--cases N` stride-samples to an approximate budget and always keeps clean replays. `--dict <path>` attaches the dictionary to both decoders and encodes the real seeds with it, and the dict-frame sweep itself is todo 21a. `--mt` selects the worker-count arms and defaults to `4`, and with `--dict` they run the sequential dictionary path. `--dump <dir>` writes every divergent case's bytes for offline replay. `--decode <file>` prints one file's verdicts from zstdx, libzstd one-shot and the CLI, for replaying a dumped case.

### `seqstats`: matcher sequence statistics

Reports sequence statistics and the entropy lower bound of our matcher on one corpus file, with `--level` selecting the level. `--ref-frame <file.zst>` adds the differential mode. The reference frame, for example `bench/corpus/text.zst9`, is decoded through the `seq_dump` decoder hook and its (ll, ml, of-wire) triples are diffed against our encoder's parse of the same raw data. The report covers same-position divergences, matches that only the reference found bucketed by ml and offset-log with our covering sequence as context, per-side literal, match and repcode aggregates with entropy bounds, and the cold-start literal split over the first 768 KiB. `--divergences N` caps the printed events, which also count toward the missed-match context lines. `--dict <file>` compresses against a raw-content or formatted dictionary and decodes the reference frame with it, so the differential mode works on dictionary frames. The dictionary frame must come from the same dictionary the reference was compressed with. The hook sits in the decode hot loop, so `seq_dump` is a bench-crate feature and never enabled by default, because it would slow every measured decode by about 30 to 40 percent. Build it explicitly:

```bash
cargo run --release -p zstdx-bench --features seq_dump -- seqstats ...
```

### `prefill`: `prefill_window` micro-benchmark

Times the matcher's window prefill per strategy on window-sized corpus strips. `--shape` defaults to `random,text,json`, and `--level` selects strategies.

### `train`: dictionary trainer tooling

Trains a dictionary with the in-tree trainer from files or directories, with `--size` capping the output. The default output is raw content. `--formatted` produces a formatted dictionary with entropy tables in the `zstd --train` shape, which libzstd accepts verbatim. `--content-of <dict>` dumps the content section of a formatted dictionary so content selection and entropy-table seeding can be A/B'd in isolation against `zstd --train`. `--format-content <content.bin>` finalizes an arbitrary content file, such as the CLI trainer's extracted content, with the training split's stats, which isolates table quality from content quality. `-v` prints the sweep's per-k metric and the winner.

Trainer knob overrides for A/B isolation, all defaulting to the library's values:

- `--grid compact|cli|<k>` sets candidate segment sizes. `cli` is libzstd's optimizer ladder.
- `--metric raw|formatted` sets what the sweep scores.
- `--hash exact|buckets` sets the frequency-table key, using exact fingerprints or libzstd's colliding f-bit buckets.
- `--boundary cross|stop` sets whether sample-boundary-spanning k-mers count.
- `--stats-level <n>` and `--stats-samples train|all` control the finalize stats parse.

```bash
cargo run --release -p zstdx-bench -- train bench/dict_files --out dict.bin --size 16384
cargo run --release -p zstdx-bench -- train bench/dict_files --out dict.bin --size 16384 --formatted
```
