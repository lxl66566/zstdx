# Testing

This page maps the test suites, lists the gate commands, and records the flake, slow-test and fixture policies. The fuzz campaign itself has its own page in the fuzz crate (`crates/zstdx-fuzz/Readme.md`) and its workflow pitfalls in [workflow pitfalls](pitfalls/workflow.md).

## Suite map

| family | location | invariant it guards |
|---|---|---|
| zstdx unit suites | `crates/zstdx/src/tests/` | per-area invariants: decode/encode corpus parity against the reference bytes (`decode_corpus`, `encode_corpus`), format edge shapes (`decode_edges`, `encode_edges`, `multi_frame`), window semantics, dictionary decode (`dict_test`), poison margins (`poison`), fuzz-artifact replay (`fuzz_regressions`), stream/io cores (`read_core`, `bit_reader`, `block_output_cap`) |
| zstdx integration | `crates/zstdx/tests/` | allocator behavior under decode (`decoder_alloc`), the MT piece decoder against sequential decode (`mt_piece_decode`), the multi-GiB epoch paths (`giant_stream`, `#[ignore]`d: run explicitly) |
| FFI Rust suite | `crates/zstdx-ffi/tests/ffi.rs` | the libzstd ABI contract: error codes and names, header introspection, frame-walk semantics, streaming transcripts, dictionary and prefix families, null/bounds edges |
| FFI C harness | `crates/zstdx-ffi/tests/run-c.sh` + `harness.c` | parity against the reference `libzstd.a` itself, including estimate sizes that embed reference struct layouts |
| CLI end-to-end | `crates/zstdx-cli/tests/` | zstd 1.5.7 observable behavior through the real binary: exit codes, diagnostic lines byte for byte, default names, stdin/stdout flows (`cli.rs`), `-l` parity against the system zstd (`list.rs`), `--train` (`train.rs`), `--patch-from` interop (`patch.rs`), `--adapt` (`adapt.rs`); shared harness in `tests/common/mod.rs` |
| bench tool unit tests | next to their code in `crates/zstdx-bench/src/` | the numeric surface the gates read: stats median/MAD (`common.rs`), the ratio-verdict delta and geo-mean math (`cmd/ratio.rs`), level parsing (`cmd/emitframe.rs`), band filters (`cmd/offhist.rs`, feature-gated) |
| fuzz | `crates/zstdx-fuzz/` (own workspace) | ten cargo-fuzz targets over codecs and interop; not part of `cargo test` at all |

The zstdx lib suite is where MT and streaming coverage lives: `stream::tests::mt*`, `encoding::mt`, `decoding::mt` and the `mt_piece_decode` binary are the thread-heavy subset referenced below.

## Gate commands

```sh
# debug suite, whole workspace (gungraun's benches are test = false; excluding
# it avoids building the valgrind-side crate)
cargo test --workspace --exclude zstdx-gungraun

# no_std surface
cargo test -p zstdx --no-default-features

# release codegen (poison margins compiled out)
cargo test --release -p zstdx

# release codegen with every debug wall on: the hardening gate
cargo test --profile paranoid -p zstdx

# doctests (25 in the zstdx lib; nextest does not run them)
cargo test --doc --workspace --exclude zstdx-gungraun
```

CI runs the workspace, doctest and no-default-features gates through nextest plus the doctest companion on ubuntu (`.github/workflows/test.yml`), the paranoid gate on a separate workflow, and the FFI C harness against a pinned reference checkout.

### nextest

`cargo nextest run --workspace --exclude zstdx-gungraun` runs every lib/bin/integration test in its own process, which keeps one panicking test from taking down its binary-mates and reports per-test times. It never runs doctests, so pair it with the `--doc` command above. Configuration lives in `.config/nextest.toml`: tests slower than 60 s are reported as slow, and a test still running after 10 slow periods (600 s) is terminated, which turns a hang into a failure instead of an infinitely stalled run. The full workspace completes in roughly 130 s locally, against roughly 170 s under libtest.

The MT-heavy subset for iteration:

```sh
cargo nextest run -p zstdx -E 'test(mt) or test(pool) or test(thread) or test(worker) or binary(mt_piece_decode)'
```

## Flake policy

A seven-run hunt on 2026-11-17 (five passes over the 177-test MT/thread subset above, two full 802-test workspace runs) produced zero intermittent failures.

Retries are reserved for environmental flakes only: temp-dir races, port or cwd assumptions, runner starvation. A failure caused by a product bug must stay a failure, so no profile in `.config/nextest.toml` sets `retries` above zero. A future flake gets a scoped `[[profile.default.overrides]]` block naming the environmental cause, never a blanket count.

The one known product race is the poison-drain livelock ([todo 39](todo.md), observed twice on 2026-11-15, not reproduced since): a worker panic that leaves a posted job unclaimed spins `wait_progress` forever. It manifests as a hang, which the 600 s termination bound converts into a reported termination, so a run surfaces it instead of stalling silently. Do not add retries for it.

## Slow tests

Slow-but-expected members under nextest, so a slow report is not read as a problem: `stream::tests::write_encoder_matches_oneshot_bytes` (~110 s, the only one past the 60 s mark in a full run), the pledged-stream MT grid pair and `deep_band_open_stream_past_the_buffer_cap` (30-65 s each depending on machine load), and the `mt_piece_decode` ramp (~11-19 s). `giant_stream_keeps_matching_past_4gib` is `#[ignore]`d for its multi-GiB runtime and runs only when invoked explicitly. Adding a test expected to exceed 60 s is fine; consider splitting it if the excess comes from unrelated cells sharing one function.

## Fixtures

- `crates/zstdx/decodecorpus_files/` holds the output of the reference generator `decodecorpus` (the `z0000xx` raw/frame pairs plus `abc.txt`). It is consumed by `tests::decode_corpus` and the window tests.
- `crates/zstdx/dict_tests/` holds a `zstd --train` formatted dictionary plus 207 systemd service files with `.zst` counterparts, consumed by `tests::dict_test`.
- `crates/zstdx/test_fixtures/` holds hand-built window frames (`window_8mib/128mib/256mib.zst`); the provenance of each is documented next to its plaintext constructor in `tests/mod.rs`.
- The fuzz corpora and crash artifacts live under `crates/zstdx-fuzz/` (`artifacts/` plus per-target merged corpora) and replay in-process through `src/tests/fuzz_regressions.rs`.

All fixture directories are excluded from the published crate, and the corpus tests skip themselves when the directory is absent. The CLI and FFI suites need no stored fixtures: CLI tests create self-cleaning scratch directories under the temp dir (`tests/common/mod.rs`), and the interop tests (`list`, `train`, `patch`, `adapt`, `ffi` C harness) skip or are run separately when no system `zstd` / reference checkout is present.

## Coverage

No coverage snapshot exists yet: this host has no `cargo-llvm-cov` and no registry network to install it. When available, the scoped command is:

```sh
cargo llvm-cov nextest -p zstdx --lib --html --ignore-run-fail
```

A least-covered-modules table belongs here once that has been run.
