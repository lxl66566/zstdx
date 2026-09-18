# Completed · Features and Infrastructure

> The full performance-optimization list has been merged by topic into the [performance optimization](../perf/decoding.md) pages (each entry carries its commit and measured effect). This page records features, API, correctness milestones, and infrastructure.

## API surface

<!-- prettier-ignore -->
| Item | commit |
|---|---|
| root-level `Level` enum replacing CompressionLevel (removed the dead Default/Better/Best variants that were never implemented) | `55daf71` |
| high-level one-shot API: `zstdx::{compress,decompress}`, `bulk::*`, the `Error/Result` umbrella, Encoder/DecoderOptions | `d35902b` |
| streaming encoder (write/read Encoder, `auto_finish`/`flush`/pledged/checksum/workers) | `51ec2fc` |
| streaming decoder (read/write Decoder, multi-frame + skippable transparency) + `encode_all/decode_all/copy_encode/copy_decode` | `b21d937` |
| zstd-crate compat layer `zstdx::compat` (full bulk/stream suite; dictionary decode verified end-to-end) | `4b60239` |
| CLI: clap rework; all numeric levels accepted | during `c726dfd` |
| errors via thiserror derive (first external dependency, compile-time only, no_std compatible) | `72f3c04` |
| decoder checksum moved in-tree (xxh64 shared by encode/decode; twox demoted to dev-dep; zero external deps at runtime) | `81d2119` |
| dict_builder module renamed `zstdx::dict` (aligned with zstd crate naming) | `7ad9e7a` |
| raw content dictionaries on all codec paths (encode + decode + CLI `-D`; libzstd content-load parity) | `9bea4d3` |
| raw-content dictionary trainer: deterministic fastCover port (shuffled samples, sliding distinct-dmer scoring with zero-out, epoch wrap, size-capped output, k-sweep scored by held-out compression; holdout parity with libzstd's trained content) | this batch |

## Level ladder and encoding features

<!-- prettier-ignore -->
| Item | commit |
|---|---|
| Fast/Balanced/Best tiers landed (hash-chain matcher + clevels-aligned parameters) | `f8cc66d` |
| Fast switched to the dfast matcher | `19077f3` |
| Best switched to a low-spec optimal parser (16 compares / targetLength 32) | `b39a192` |
| Opt/Ultra: full port of btopt/btultra optimal parsing | `c726dfd` |
| boundary package-merge optimal length-limited Huffman | `aa07308` (superseded 2026-09-18 by the two-queue `HUF_buildCTable` port; package-merge remains as the test-side optimality reference) |
| sequence FSE table repeat mode (mode 3) | `7769bf8` |
| frame checksum (hash feature on by default) + pledged_size + workers options | early |
| MT encode ratio retention (overlap prefill + gain gate + periodic seeds) | `a37ebaa` |
| streaming encode MT (burst model, workers>1) | `44e11e5` + `27b91cf` |

## Multithreading

<!-- prettier-ignore -->
| Item | commit |
|---|---|
| bulk MT encoding (overlap jobs, 2/4/8 workers at 2.05×/3.95×/7.55×) | `fd931a1` |
| segment-parallel decoding (restart-point splitting, stage A/B) | `3219947` |
| checksum sidecar offload + bounded-spin worker parking | `df33295` `70b5fd4` |

## Correctness milestones

<!-- prettier-ignore -->
| bug | commit |
|---|---|
| decoder-side frame checksum auto-verification (previously the caller compared via getters) | `81d2119` |
| degenerate single-symbol FSE distribution panic + write_table trailing zero-probability out-of-bounds | `b650cad` |
| raw-block fallback not rolling back rep + reusing entropy tables (later blocks referencing tables the decoder never received) | `61d63a9` |
| usize underflow in overlap_copy8 offsets 5-7 (12 tests failing in debug, release accidentally correct) | `590177e` |
| MT decode literals count validation bug (7 of 11 corpora failed under MT while repo tests were all green) | `3964a62` |
| decode_to_vec_mt serial fallback erroring/hanging on an empty Vec | `21fae62` |
| chain table insert (window index) / walk (absolute position) index-domain split (tangled chains) | `36203c1` |
| nondeterministic MT output (residue in pooled tables; head tables cleared per job) | `a6cf8a6` |
| dfast backfill insertion predicate (the step<4 proxy missed insertions over long-match-covered regions) | `06b67dc` |
| opt.rs debug prints shipped in release via a default feature | `8233b2f` |

## Infrastructure

- **Interleaved A/B harness**: round-by-round alternation, warmup, time budget (`--budget-ms`/BENCH_BUDGET_MS), median/mad statistics; migrated into the zstdx-bench crate together with the tool family (`src/common.rs`). `dc82c29`
- **bench tool family** (now the collection in `crates/zstdx-bench`, subcommands in [methodology](../bench/methodology.md)): matrix (full dec/enc × bulk/stream/MT matrix + roundtrip gate + checksum overhead row + both-side worker scalability, filtered by `--shape/--level/--workers/--mt-workers`), small, files, prof (dec/enc/enc-stream), dump (deterministic snapshots), corrupt, mtcheck, seqstats, prefill. matrix originated in `c922b34` `b1dd010`.
- **Deterministic regression probe**: dump byte-level snapshot comparison (a free A/B signal for "changes that must not alter output") + corrupt (random corruption, 0 panic, covers the flat path). `318d8f7`
- corpus generator tracked in the repo.
- **published-crate test portability**: all fixture access (corpus/dict dirs, single corpus files, fuzz artifacts) goes through `fixture_entries`/`fixture_bytes`, which skip with a note on `NotFound` (and still panic on real IO errors); the `.crate`'s standalone `cargo test` passes.
- ad-hoc profiling tools (enc/dec/enc-stream, mtcheck, prefill, seqstats) promoted to zstdx-bench subcommands (previously untracked examples).
- **lint/fmt toolchain**: workspace lints (clippy `all`+`pedantic`, with explicit allows for codec-inherent noise such as the cast family, `inline_always`, `unreadable_literal`) + `clippy.toml` (msrv 1.89) + nightly `rustfmt.toml` (crate-level import merging, StdExternalCrate grouping, comment wrapping) + `.tombi.toml` (TOML alignment); member crates inherit via `[lints] workspace = true`, fuzz crate inline. Zero clippy/fmt warnings tree-wide (`cargo clippy --workspace --all-targets --all-features`). Same batch: all crates bumped to edition 2024 (rust-version 1.87→1.89, AVX-512 intrinsics stable; explicit `unsafe` blocks inside `unsafe fn` bodies). A later backlog cleanup removed the dead huff0 `encode4x`/`encode4x_with`/`weights` wrappers (`encode4x_only` is the live 4-stream path), gave the hot helpers (parse loops, MT job runners, split/table inserts, staged-block encoder) targeted `too_many_arguments`/`too_many_lines`/`struct_excessive_bools` allows instead of signature churn, moved the misplaced `float_cmp` allow onto `decide_donated_with` (the function that actually compares), replaced the `LDM && false` tautology, and added the single workspace-level allow `assert_is_empty` (its `assert_ne!(x, [] as [T; 0])` rewrite reads worse).
