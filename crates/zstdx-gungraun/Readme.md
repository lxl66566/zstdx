# zstdx-gungraun

Deterministic, machine-independent performance comparison between `zstdx` and the reference `zstd` crate (libzstd), built on [gungraun](https://github.com/gungraun/gungraun) (the successor of iai-callgrind).

Unlike the wall-clock benchmarks in the `zstdx-bench` crate, which interleave two sides and take medians to fight clock drift, this crate runs each benchmark exactly once under the Valgrind simulator and reports **executed instruction counts** (the `Instructions`/`Ir` metric). Instruction counts are a property of the binary and its input, not of the machine's temperature, frequency scaling or background load, so results are reproducible to seven or more significant digits and comparable across machines. On a host with heavy CPU drift (35%+ perf offsets) this is the only trustworthy way to detect a regression or to prove that an optimization actually removed work. Gungraun additionally disables ASLR and clears the environment for the measured child processes, removing two further sources of nondeterminism.

What is deliberately **not** covered here: wall-clock time (use `zstdx-bench`), and multithreaded paths — thread scheduling makes instruction counts scheduling-dependent, so only single-threaded code is measured.

## Requirements

- Linux with Valgrind >= 3.20 (>= 3.22 recommended). On Windows, use WSL2 (any distro; Arch works with `pacman -S valgrind`).
- The `gungraun-runner` binary, at the same version as the `gungraun` library dependency (currently 0.19.4):

  ```bash
  cargo install --version 0.19.4 gungraun-runner
  ```

  The runner is looked up in `PATH` **at benchmark runtime** (`Command::new("gungraun-runner")`), so `~/.cargo/bin` (or wherever `cargo install` puts it) must be on `PATH` in the shell that runs `cargo bench`. The `GUNGRAUN_RUNNER` environment variable is an alternative, but note it is baked in with `option_env!` **at compile time** — setting it at runtime has no effect unless the benches are rebuilt.
- `cargo` must be on `PATH` as well: the runner shells out to `cargo metadata` to find the workspace root. Run `cargo bench` from inside the workspace (the normal case) and this just works.
- The benchmark corpus in `bench/corpus` (repository root). Generate it once with `bash bench/gen_corpus.sh`; the benches panic with a pointing error message when files are missing. Each benchmark works on the first 1 MiB slice of a shape so a full valgrind sweep stays in the minutes range.

The crate itself (library and bench targets) compiles on any host including Windows, so `cargo check --workspace` and `cargo clippy --workspace --all-targets` remain usable there. The bench targets set `test = false`, so `cargo test --workspace` never executes them on hosts without valgrind.

## What is measured

Three bench binaries, each pairing the two implementations on identical inputs. `zx*` functions measure `zstdx`, `zc*` measure the `zstd` crate. Every group enables `compare_by_id`, so the output of a `zc*` benchmark includes a `Comparison with zx*` block per corpus shape.

| File | Groups | What it measures |
|------|--------|------------------|
| `benches/decode.rs` | `l1`, `l3`, `l9`, `own_fastest`, `own_balanced`, `own_ultra` | Bulk decode into a caller-provided buffer (`FrameDecoder::decode_all` vs `zstd::bulk::decompress_to_buffer`). `l*` groups decode frames produced by libzstd at that level (the interop case); `own_*` groups decode frames produced by the zstdx encoder at the matching level. |
| `benches/encode.rs` | `fastest`, `fast`, `balanced`, `best`, `opt`, `ultra` | One-shot bulk encode (`zstdx::bulk::compress_with` vs `zstd::bulk::compress`) at the six ladder levels, paired with libzstd levels 1/3/6/12/16/19. Context setup and output allocation are counted on both sides equally. |
| `benches/stream.rs` | `stream_decode`, `stream_encode_fastest`, `stream_encode_balanced` | Streaming decode with a 64 KiB sink (`StreamingDecoder` vs `zstd::stream::read::Decoder`) and streaming encode with 64 KiB writes into an in-memory sink (`stream::write::Encoder` vs `zstd::stream::write::Encoder`). |

The corpus shapes (shared with `zstdx-bench`): `json` (semi-structured records), `text` (tiled source, long-range repeats), `skewed` (16-byte alphabet), `random` (incompressible), `zeros` (maximally redundant).

Setup work (reading the corpus slice, producing the compressed frames, the cross-implementation correctness gates that decode every produced frame with the *other* implementation before handing it over) happens in the `#[bench]` argument expressions, which run outside the measured region. A benchmark therefore fails loudly if the encoder or decoder is broken, instead of silently producing numbers for garbage.

## Running

From the repository root (or this crate's directory) on a Linux host:

```bash
cargo bench -p zstdx-gungraun                    # everything (~100 benchmarks)
cargo bench -p zstdx-gungraun --bench decode     # one file
cargo bench -p zstdx-gungraun --bench encode -- --list
```

Arguments after `--` go to the gungraun runner. The position filter is an **anchored** wildcard pattern matched against the full benchmark id `<file>::<group>::<function>::<id>` — anchor it with a leading `*` unless you spell out the whole path:

```bash
cargo bench --bench decode -- '*l1::*'                    # the whole l1 group (both implementations)
cargo bench --bench decode -- '*l1::zc_l1::skewed'        # one benchmark
cargo bench --bench encode -- '*ultra::zx_ultra::json'    # zstdx ultra encode on the json shape
cargo bench -- '*'                                         # everything, explicitly
```

Useful runner flags: `--parallel=auto` (benchmarks within a group still run serially), `--nocapture`, `--output-format=json` (one JSON object per benchmark on stdout), `--save-summary=json`. Output artifacts (callgrind dumps per benchmark) land under `target/gungraun/` and can be inspected with `callgrind_annotate` or `kcachegrind` for function-level attribution.

### Reading the output

```
decode::l1::zc_l1 skewed:(zstd_frame(Shape :: Skewed, 1))
  Instructions:                     6362624|N/A                  (*********)
  Comparison with zx_l1 skewed:(zstd_frame(Shape :: Skewed, 1))
  Instructions:                     9659350|6362624              (+51.8139%) [+1.51814x]
```

The first row is this benchmark's own fresh measurement (`|N/A` means no baseline to compare against yet). The `Comparison with zx_l1` row shows `zstdx | libzstd` instruction counts side by side; here zstdx needed 51.8% more instructions than libzstd to decode the same frame. The other metrics are derived from the simulator's cache model (`L1 Hits`, `LL Hits`, `RAM Hits`, `Total read+write`); `Estimated Cycles` is a weighted combination of those events — a relative cost signal, not a wall-clock prediction. For cross-implementation verdicts and regression gating, use `Instructions`.

### Baselines and regression gating

```bash
cargo bench -p zstdx-gungraun -- --save-baseline=main    # record the current state
# ...apply a change...
cargo bench -p zstdx-gungraun -- --baseline=main         # every benchmark prints new|old with deltas
cargo bench -p zstdx-gungraun -- --baseline=main --callgrind-limits='ir=5%'   # exit code 3 on >5% regressions
```

Limits can also be declared in code via `Callgrind::soft_limits`/`hard_limits` in a `LibraryBenchmarkConfig`; CLI limits override them. The exit code of 3 on regression makes this directly usable as a CI gate.

Determinism note: identical binaries and inputs produce identical counts across runs; but any change to the compiler version, flags, dependency versions, benchmark slice size or corpus regeneration changes absolute numbers. The corpus is gitignored and partially generated from `/dev/urandom` (`random.raw`) — do not regenerate it between recording a baseline and comparing against it.

## WSL2 notes (this repository's host setup)

- The host machine runs a customized WSL kernel whose 9p/drvfs mount returns `EIO` for `statx` with a full mask, which breaks every Rust `fs::metadata` call and therefore all of cargo on `/mnt/c`. Symptom: `error: could not find Cargo.toml in /mnt/c/...` while `ls` shows the file. Workaround: copy the working tree to the Linux filesystem and run there (instruction counts do not depend on where the code was built):

  ```bash
  cd /mnt/c/programs/fork/zstd-rs-pure
  tar --exclude=./target --exclude=./.git -cf - . | (mkdir -p ~/zstdx && cd ~/zstdx && tar xf -)
  # re-run this sync after editing files on the Windows side
  cd ~/zstdx && PATH="$HOME/.cargo/bin:/usr/sbin:/usr/bin:/sbin:/bin" cargo bench -p zstdx-gungraun
  ```

  The explicit `PATH` covers both runtime lookups: `gungraun-runner` from `~/.cargo/bin`, and `cargo` for the runner's workspace-metadata query.
- A pitfall that produces silently empty benchmark runs: `main!` must be invoked at **item position** (it expands to the `fn main` itself). Wrapping it as `fn main() { main!(...) }` compiles without error, but the expansion becomes a nested, never-called function — the benchmark binary exits 0 doing nothing. All bench files here invoke it correctly; keep it that way.
