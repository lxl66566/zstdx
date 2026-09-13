# Status Overview

> As of `0e7044d` (2026-09-12). Branch goal (AGENTS.md): feature parity with upstream zstd and performance beyond it; no upstreaming; arbitrary unsafe / SIMD / new instruction sets allowed.

## Capability matrix

### Compression levels

Full 1-22 ladder (one parameter row per numeric level, modeled on libzstd's `clevels.h` large-source table — see `LEVEL_PARAMS` in `encoding/match_generator.rs`); `Level::from_zstd(n)` maps exactly, negative levels clamp to 1. The named tiers alias representative rows:

| Tier (=level) | ≈zstd | Matcher strategy | Window | Notes |
|---|---|---|---|---|
| Uncompressed (0) | 0 | raw block | — | |
| Fastest (1) | 1 | fast (hash5 single-probe table) | 768 KiB | row 2: fast H16/W20 |
| Fast (3) | 3 | dfast (hash8+hash5 dual-table single-probe) | 2 MiB | row 4: dfast H18/C18; rows 5-12: chain family (greedy→lazy→lazy2 via lazy_depth, min_match 5, depths half libzstd's 1<<S — our per-probe walk is dearer; W21→W22, H19→H23) |
| Balanced (9) | 9 | hash chain + lazy2 | 4 MiB | H21 / C20 (aliased beyond 1MiB, like libzstd cLog<wLog) / depth 8 |
| Best (13) | 13 | low-spec optimal parser | 4 MiB | rows 13-15 (libzstd btlazy2 territory); W22 / H22 / C22; searchLog 4-6 / targetLength 32 |
| Opt (17) | 17 | btopt (full port) | 8 MiB | rows 16-17; W23 / H22 / C22 (L17 row, ring capped below libzstd's C23: json −19.6% time for +0.16% dll) |
| Ultra (19) | 19 | btultra(+2) (full port) | 8 MiB | rows 18-22; 2-pass first-block statistics; rows 20-22 widen to W24-26 with the ring capped at 24 and hash at 22 (u64-slot memory guard; libzstd runs C25-27/H23-25 u32 there) |

First ladder A/B (2026-09-12, 32MiB json/text, vs libzstd CLI at the same level): json — every row at parity or denser except dfast rows +0.2-0.3% and opt rows +0.1-0.4%; chain rows -13..-17%, rows 13-15 -9..-10%. text — rows 1-8 far denser (window), rows 9-12 +2.6-2.7% behind (libzstd's row matcher), rows 13-16 -0.4..-5%, 17-22 +0.2-0.3%. Known inversions: our deep-chain rows (10-12) sit between our opt rows 13-16 on json (the chain is that strong there; libzstd's own ladder inverts json -1 vs -3 by 15%), and greedy row 5 trails dfast row 4 on interleaved-random shapes (libzstd's -1..-5 inverts the same way). All 22 levels × 5 corpora roundtrip through libzstd CLI. Fastest keeps 768KiB (beyond-W21 expansion still measured as a loss — see [falsified directions](dev/negative.md); LDM remains the path for more reach).

Known-length sources resize their row (libzstd's `ZSTD_adjustCParams` port: window ≤ srcLog, hash ≤ wlog+1, chain/ring ≤ wlog): bulk paths pass the exact size, streaming passes the pledge, the CLI passes file metadata, unknown sizes keep the row (`Matcher::set_source_hint` / `FrameCompressor::set_size_hint`). Measured on adjusted bands (64KiB-4MiB json/text): ratio parity with libzstd at every probed level (chain rows -5..-15% denser); the 4KiB band keeps a ~+40% fixed-overhead gap on json at this measurement (block/header entropy coding of tiny payloads; ratio since closed to ±1-5% by the small-literal huffman fix, todo 7), while small-call speed wins (4KiB L19 5.96 vs 16.20 ms/call incl. process spawn).

### Codec paths

| Capability | Status |
|---|---|
| slice/bulk codec | ✅ zero-copy encoding + thread_local state pool; decoding writes directly into the flat output |
| streaming codec (read/write Encoder, Decoder) | ✅ streaming output byte-identical to bulk when no flush |
| MT encoding | ✅ bulk (overlap jobs) + streaming (bursts); workers>1; no_std reports Unsupported |
| MT decoding | ✅ restart-point segmentation; serial stage B is the bottleneck, no scalability yet |
| frame checksum | ✅ optional on encode (+sidecar thread offload); decode auto-verifies in-tree xxh64 (MT path does not verify) |
| zstd-crate compat layer `zstdx::compat` | ✅ (dictionary decode works end-to-end) |
| dictionaries | decode ✅ (formatted + raw content: `Dictionary::load`, an id-0 dict applies to frames without dictID); encode ✅ (ST all paths, `EncoderOptions::dictionary`/`FrameCompressor::set_dictionary`/CLI `-D`; formatted dicts load content as match history + seed entropy tables + dictID, headerless raw content as pure match history with default repcodes — libzstd parity, frames decodable by libzstd; MT falls back ST); `dict/` training half-done (known bugs). Dict sizes on the systemd fixture: +13% vs libzstd at -9 (sub-2KB fixed-overhead band, todo 10) |
| forced window log (`InputShape::with_window_log`) | ✅ all paths; blocks cap at the window (RFC 8878 Block_Maximum_Size) |
| LDM / superblock / C FFI | ❌ |

## Completeness vs libzstd (subjective estimates, for targeting)

| Dimension | Estimate | Basis |
|---|---|---|
| decode features/correctness | ~90% | spec compliance, dictionary decode, corpus+fuzz; missing MT-path checksum |
| decode performance | bulk ahead across the board; streaming ~75-85% | streaming residue is on json/skewed, see [current snapshot](dev/bench/snapshot.md) |
| encode features | ~80% | full 1-22 ladder + dictionary encode (ST) + adjustable window + MT + streaming in place; missing dictionary training, LDM, superblock |
| encode speed | wins and losses split by tier | 2026-09-12 ladder speed curve (json 32MiB ST, CLI): chain rows 137/67/25 MiB/s at L6/9/12 vs libzstd 423/165/82 (0.25-0.33×, the known chain/json speed story); opt rows 7/4/2 at L13/17/19 vs 54/6/3 (0.13× at the btlazy2 slot, ~0.7× at btopt+); fastest/fast 447/340 vs 1118/812; small-call side wins (4KiB L19 5.96 vs 16.20 ms incl. spawn); tier-level x in the fresh [snapshot](dev/bench/snapshot.md) (Opt C22 speed cut landed; stream-MT burst-imbalance regression fixed via the epoch grid, same day) |
| encode ratio | matched at every tier | 2026-09-12 full-ladder `ratio` sweep vs libzstd at same numeric levels (1MiB slice, all modes): json geo-mean +4.8% denser (balanced +19.6%, best +10.2%), skewed +2.8%, zeros +2.0%, random 0.00%, text +97.6% geo (window; balanced row -2.7% is the one losing cell — chain-row text residue, todo 9); 4KiB json ±1-5% after the small-literal huffman fix; dll Best/Opt denser than zstd-12/16, Ultra 7.7% behind zstd-19 |
| API/ecosystem | ~60% | bulk + streaming + compat (incl. dictionary constructors) + CLI (levels 1-22, -D dictionaries); missing C FFI, language bindings, standard CLI argument surface |

Note: `COMPARE.md`'s completeness assessment is frozen at the `4ff2b7b` point in time; its conclusions — "encode features ~40%", "json.Best ratio gap (btopt shortfall)", "MT ratio collapse" — have been superseded by the optimal parser (`c726dfd`), package-merge Huffman (`aa07308`), the Best core swap (`b39a192`), and MT ratio retention (`a37ebaa`).

## Code structure (crates/zstdx/src)

| Module | Contents |
|---|---|
| `bit_io/` | bit readers/writers (reverse bit reader BitReader, BitWriter) |
| `blocks/` | block-level parsing |
| `decoding/` | decoder: frame/block/literals/sequence decoding, `sequence_execution.rs` (fused execution), `flat_buffer.rs` (streaming flat window), `ringbuffer.rs` (dictionary path), `mt.rs` (segment-parallel) |
| `encoding/` | encoder: `frame_compressor.rs`, `match_generator.rs` (fast/dfast/chain matchers), `opt.rs` (optimal parsing), `levels/fastest.rs`, `mt.rs` (bulk MT), `async_checksum.rs` (sidecar checksum), `blocks/compressed.rs` (entropy-coded blocks), `seq_codes.rs` |
| `fse/` `huff0/` | entropy codecs |
| `xxh64.rs` | in-tree checksum (shared by encode/decode; zero external deps at runtime) |
| `bulk.rs` `stream/` | one-shot and streaming APIs (`encoder_core.rs` / `encoder_mt.rs`) |
| `compat/` | zstd-crate compat layer |
| `dict/` | dictionary training (feature = "dict_builder", half-done) |
| `level.rs` `options.rs` | level enums and encode/decode options |

## Verification discipline (every commit)

Atomic commits (single-line msg) + fmt + Changelog entry + all-green debug/release/no-default tests + full-corpus interop roundtrip against libzstd (CLI) + corruption smoke (random corruption, 0 panic) + dump differential checks (deterministic output as a free regression probe) + perf A/B as back-to-back same-machine reruns via git stash. Details and counterexamples in [engineering and benchmarking methodology](dev/pitfalls/workflow.md).
