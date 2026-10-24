# zstdx-gungraun

Deterministic performance comparison between `zstdx` and the reference `zstd` crate (libzstd), built on [gungraun](https://github.com/gungraun/gungraun), the successor of iai-callgrind. Each benchmark runs once under the Valgrind simulator and reports executed instruction counts through the `Instructions` metric, also called `Ir`. This page answers when instruction counts beat wall-clock timing, what the crate measures, and how to gate a change against a baseline.

The wall-clock benchmarks in `zstdx-bench` interleave two sides and take medians to fight clock drift. This crate reports instruction counts instead. An instruction count is a property of the binary and its input, and it stays independent of the machine's temperature, frequency scaling and background load. Results are reproducible to seven or more significant digits and comparable across machines. On a host with heavy CPU drift, where perf offsets reach 35% or more, instruction counts are the dependable way to detect a regression or to prove that an optimization removed work. Gungraun also disables ASLR and clears the environment for the measured child processes, which removes two further sources of nondeterminism.

Wall-clock time and multithreaded paths stay outside this crate. Use `zstdx-bench` for wall time. Thread scheduling makes instruction counts depend on the schedule, so only single-threaded code is measured here.

## Requirements

- Linux with Valgrind 3.20 or newer, and 3.22 or newer recommended. On Windows, use WSL2 with any distribution, for example Arch with `pacman -S valgrind`.
- The `gungraun-runner` binary, at the same version as the `gungraun` library dependency, currently 0.19.4:

  ```bash
  cargo install --version 0.19.4 gungraun-runner
  ```

  The runner is looked up in `PATH` at benchmark runtime through `Command::new("gungraun-runner")`. The directory that `cargo install` writes to, usually `~/.cargo/bin`, must be on `PATH` in the shell that runs `cargo bench`. The `GUNGRAUN_RUNNER` environment variable is an alternative. It is baked in with `option_env!` at compile time, so setting it at runtime has no effect unless the benches are rebuilt.
- `cargo` must also be on `PATH`, because the runner shells out to `cargo metadata` to find the workspace root. Run `cargo bench` from inside the workspace, which is the normal case, and this works.
- The benchmark corpus in `bench/corpus` at the repository root. Generate it once with `bash bench/gen_corpus.sh`. The benches panic with an error that points at the missing file. Each benchmark works on the first 1 MiB slice of a shape, which keeps a full valgrind sweep in the minutes range.

The crate itself, library and bench targets included, compiles on any host including Windows, so `cargo check --workspace` and `cargo clippy --workspace --all-targets` remain usable there. The bench targets set `test = false`, so `cargo test --workspace` never executes them on hosts without valgrind.

## What is measured

Four bench binaries pair the two implementations on identical inputs. `zx*` functions measure `zstdx` and `zc*` functions measure the `zstd` crate. Every group enables `compare_by_id`, so the output of a `zc*` benchmark includes a `Comparison with zx*` block per corpus shape.

| File | Groups | What it measures |
|------|--------|------------------|
| `benches/decode.rs` | `l1`, `l3`, `l9`, `own_fastest`, `own_balanced`, `own_ultra` | Bulk decode into a caller-provided buffer (`FrameDecoder::decode_all` against `zstd::bulk::decompress_to_buffer`). The `l*` groups decode frames produced by libzstd at that level, which is the interop case. The `own_*` groups decode frames produced by the zstdx encoder at the matching level. |
| `benches/encode.rs` | `fastest`, `fast`, `balanced`, `best`, `opt`, `ultra` | One-shot bulk encode (`zstdx::bulk::compress_with` against `zstd::bulk::compress`) at the six ladder levels, paired with libzstd levels 1, 3, 6, 12, 16 and 19. Context setup and output allocation are counted on both sides equally. |
| `benches/small.rs` | `small_fastest` | Repeated one-shot bulk encode of 4 KiB to 64 KiB payloads, with 256 calls and a warm state pool. This is the fixed-cost band where allocator churn and per-block entropy builds rival the scan. The cells are json and text at 4 KiB, 16 KiB and 64 KiB, plus random at 4 KiB, 8 KiB and 16 KiB, paired zx and zc per cell. |
| `benches/stream.rs` | `stream_decode`, `stream_encode_fastest`, `stream_encode_balanced` | Streaming decode with a 64 KiB sink (`StreamingDecoder` against `zstd::stream::read::Decoder`) and streaming encode with 64 KiB writes into an in-memory sink (`stream::write::Encoder` against `zstd::stream::write::Encoder`). |

The corpus shapes are shared with `zstdx-bench`. `json` holds semi-structured records, `text` holds tiled source with long-range repeats, `skewed` uses a 16-byte alphabet, `random` is incompressible, and `zeros` is maximally redundant.

Setup work happens in the `#[bench]` argument expressions, which run outside the measured region. It covers reading the corpus slice, producing the compressed frames, and the cross-implementation correctness gates that decode every produced frame with the other implementation before handing it over. A broken encoder or decoder therefore fails the benchmark loudly instead of silently producing numbers for garbage.

## Running

From the repository root, or from this crate's directory, on a Linux host:

```bash
cargo bench -p zstdx-gungraun                    # everything (~100 benchmarks)
cargo bench -p zstdx-gungraun --bench decode     # one file
cargo bench -p zstdx-gungraun --bench encode -- --list
```

Arguments after `--` go to the gungraun runner. The position filter is an anchored wildcard pattern matched against the full benchmark id `<file>::<group>::<function>::<id>`, so anchor it with a leading `*` unless you spell out the whole path:

```bash
cargo bench --bench decode -- '*l1::*'                    # the whole l1 group (both implementations)
cargo bench --bench decode -- '*l1::zc_l1::skewed'        # one benchmark
cargo bench --bench encode -- '*ultra::zx_ultra::json'    # zstdx ultra encode on the json shape
cargo bench -- '*'                                         # everything, explicitly
```

Useful runner flags:

- `--parallel=auto` runs benchmarks in parallel, while benchmarks within a group still run serially.
- `--nocapture` shows the benchmark output.
- `--output-format=json` prints one JSON object per benchmark on stdout.
- `--save-summary=json` writes a JSON summary.

Callgrind dumps per benchmark land under `target/gungraun/` and can be inspected with `callgrind_annotate` or `kcachegrind` for function-level attribution.

### Reading the output

```
decode::l1::zc_l1 skewed:(zstd_frame(Shape :: Skewed, 1))
  Instructions:                     6362624|N/A                  (*********)
  Comparison with zx_l1 skewed:(zstd_frame(Shape :: Skewed, 1))
  Instructions:                     9659350|6362624              (+51.8139%) [+1.51814x]
```

The first row is this benchmark's own fresh measurement. `|N/A` means there is no baseline to compare against yet. The `Comparison with zx_l1` row shows `zstdx | libzstd` instruction counts side by side. Here zstdx needed 51.8% more instructions than libzstd to decode the same frame. The other metrics come from the simulator's cache model: `L1 Hits`, `LL Hits`, `RAM Hits` and `Total read+write`. `Estimated Cycles` is a weighted combination of those events and is a relative cost signal. Wall-clock prediction belongs to `zstdx-bench`. For cross-implementation verdicts and regression gating, use `Instructions`.

### Baselines and regression gating

```bash
cargo bench -p zstdx-gungraun -- --save-baseline=main    # record the current state
# ...apply a change...
cargo bench -p zstdx-gungraun -- --baseline=main         # every benchmark prints new|old with deltas
cargo bench -p zstdx-gungraun -- --baseline=main --callgrind-limits='ir=5%'   # exit code 3 on >5% regressions
```

Limits can also be declared in code through `Callgrind::soft_limits` and `Callgrind::hard_limits` in a `LibraryBenchmarkConfig`, and CLI limits override them. The exit code of 3 on regression makes the command directly usable as a CI gate.

Identical binaries and inputs produce identical counts across runs. A change to the compiler version, flags, dependency versions, benchmark slice size or corpus regeneration changes absolute numbers. The corpus is gitignored and partly generated from `/dev/urandom`, which produces `random.raw`, so keep it unchanged between recording a baseline and comparing against it.

## WSL2 notes

- The host machine runs a customized WSL kernel whose 9p/drvfs mount returns `EIO` for `statx` with a full mask. That breaks every Rust `fs::metadata` call and therefore all of cargo on `/mnt/c`. The symptom is `error: could not find Cargo.toml in /mnt/c/...` while `ls` shows the file. Work around it by copying the working tree to the Linux filesystem and running there, since instruction counts do not depend on where the code was built:

  ```bash
  cd /mnt/c/programs/fork/zstd-rs-pure
  tar --exclude=./target --exclude=./.git -cf - . | (mkdir -p ~/zstdx && cd ~/zstdx && tar xf -)
  # re-run this sync after editing files on the Windows side
  cd ~/zstdx && PATH="$HOME/.cargo/bin:/usr/sbin:/usr/bin:/sbin:/bin" cargo bench -p zstdx-gungraun
  ```

  The explicit `PATH` covers both runtime lookups: `gungraun-runner` from `~/.cargo/bin` and `cargo` for the runner's workspace-metadata query.
- A pitfall that produces silently empty benchmark runs: `main!` must be invoked at item position, because it expands to the `fn main` itself. Wrapping it as `fn main() { main!(...) }` compiles without error, but the expansion becomes a nested, never-called function and the benchmark binary exits 0 doing nothing. All bench files here invoke it correctly, so keep it that way.
