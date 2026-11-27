# Build and cargo


- A/B always runs back-to-back on the same machine through `git stash`. After a stash round-trip, `target/` is not rebuilt automatically, and one session spent half a day objdump-ing the stale BASE binary while analyzing why BMI2 was not generated.
## Shared stash stack


All worktrees of this repository push onto one `refs/stash`, so a `git stash pop` after any delay can restore and drop another session's work in progress. This was observed live, where a pop restored a foreign match_generator and opt.rs diff and dropped its entry.

Before popping, verify the top entry's content with `git stash show --stat`. On a wrong pop the dropped commit usually still exists, and `git stash store <sha>` re-registers it exactly. For worktree-local parking, prefer scratch commits on the branch with `git add -A && git commit -m wip` and reset later.
## Gitignored AGENTS.md is absent from fresh worktrees


`AGENTS.md` is ignored and untracked, so a worktree checked out for the first time carries no copy. An agent session that starts with reading it dies on the first step. Copy the main checkout's file into a new worktree before handing it over; the ignore rule keeps the worktree clean.
## Per-package cargo binaries


`cargo build -p zstdx` does not refresh `target/release/zstdx-bench`, so rebuild the bench binary after every edit. A ratio run once showed byte-identical output because it was a stale binary.
## No-default caching illusion


Touch sources to confirm recompilation before counting warnings. `cargo build --no-default-features` also recompiles examples as no-std and overwrites the default build under `target/release`, with AVX-512 kernels cfg'd out and xxhash gone, which invalidates every subsequent measurement.

Before measuring, rebuild the default build and verify with `nm ... | grep <kernel symbol>`. The no-default check must carry `-p zstdx`, because a root build lets the CLI bring the hash feature back. The examples have since moved out of zstdx, so that last risk is gone.
## Cargo test's piped exit code


With `cargo test | tail`, the cargo failure is swallowed by the `&&` chain while the success marker still prints. The test gate must be `cargo test > log; echo $?`, persisting to disk and checking both the exit code and the FAILED count.

`2>&1 | tail -N` also truncates the output file to its last N lines and wastes a re-run round.
## The debug test suite is mandatory


A green release suite does not mean a bug-free tree. If the suite seems hung, first suspect that a panic poisoned the thread pool, because an MT worker panic makes the join wait forever, which once meant 17 minutes of zero output. Localize with `--test-threads 1` and by running the test binary directly.

A suite that dies on `signal: 15, SIGTERM` with every visible result ok and no failing test named is the host's earlyoom, not the code: this box runs `earlyoom -m5,3`, which SIGTERMs the largest process the moment free memory crosses 5 percent, and the paranoid profile's parallel MT tests (each pooling 256 MiB accumulate buffers) are the usual victim. The 2026-11-18 run died three times at different tests this way. Rerun with `-- --test-threads=1`, which bounds the concurrent buffers and finished the whole 570-test lib suite in 68 s. Since R116 the admission gate in `tests::mem_gate` bounds the suite's peak (see dev/testing.md, Paranoid-gate memory shape); the serial rerun remains the fallback for a genuinely exhausted box.

- Running the test binary directly requires the crate root, because 13 corpus tests use relative paths.
- The bench corpus is not in git, being an untracked generated artifact, so copy it before benching in a worktree.
- Delete debug panic and eprintln instrumentation before committing. One was once left behind, and an eprintln under `#[cfg(feature="std")]` compiles into release with the default feature.
- On this repository's git version, `git stash push --staged` only copies without restoring the scene. For splitting atomic commits, use `git restore --staged` plus `git commit <path>`.
- `cargo fmt` touches historically unformatted files. Before committing, run `git checkout --` on the unrelated files, because it nearly dragged six of them into an atomic commit.
## Un-CI'd configuration drifts


`--no-default-features` drifted uncompilable across two full optimization rounds, through a std-gated re-export and std-only float methods in newly no_std-path code, while every agent verified default features only. A one-line `cargo check --no-default-features` in the pre-commit ritual catches this class for free, and the same applies to `--features std`-minus combinations on crates that promise them.

`--all-features` is the same trap on the lint side, where `#[cfg(any(test, feature = "fuzz_exports"))]` huff0 helpers sat as `dead_code`-invisible under default features for several rounds and the bench's `seq_dump`-only helpers were only unused-but-compiled without the feature. The lint gate must be `cargo clippy --workspace --all-targets --all-features`. The trap fires in reverse too, where a `#[cfg(feature = "dict_builder")]`-only import in compat/dict.rs warned on every default-feature build but was invisible under the all-features gate for a full round, so a default-features clippy pass belongs in the ritual alongside the all-features one.

The no-default-features pass also carried 11 pre-existing warnings, 4 mechanical in `io_nostd.rs`, 5 `unused_self` on the checksum-free implementation's symmetry API and 2 feature-gated dead-code functions in opt.rs and match_generator, and none blocked the gate's purpose of catching new drift. Every profile now builds and lints warning-free, and the gate is CI-enforced through `.github/workflows/lint.yml` with `cargo fmt --all -- --check`, `cargo clippy --workspace --all-targets` and `cargo clippy -p zstdx --no-default-features --all-targets`. The manual ritual had drifted again when poison-test files landed with pedantic lints, which motivated wiring it in.

Clippy itself drifted a third way on 2026-10-24: a newer toolchain fired `manual_div_ceil` and `manual_ilog2` on frame-crafting helpers older than those lints, and `opt.rs`'s `prefill_hash3` turned dead-code under no-default-features behind its `std`-gated caller. Both CI commands run clean again after the sweep. A clippy bump can reopen old code, so re-read the gate output after toolchain updates.
