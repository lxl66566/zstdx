# Status Overview

> As of `1249735b` (2026-10-01). Branch goal (AGENTS.md): feature parity with upstream zstd and performance beyond it; no upstreaming; arbitrary unsafe / SIMD / new instruction sets allowed.

## Capability matrix

### Compression levels

Full 1-22 ladder (one parameter row per numeric level, modeled on libzstd's `clevels.h` large-source table — see `LEVEL_PARAMS` in `encoding/match_generator/params.rs`); `Level::from_zstd(n)` maps exactly, negative levels clamp to 1. The named tiers alias representative rows:

<!-- prettier-ignore -->
| Tier (=level) | ≈zstd | Matcher strategy | Window | Notes |
|---|---|---|---|---|
| Uncompressed (0) | 0 | raw block | — | |
| Fastest (1) | 1 | fast (hash5 single-probe table) | 768 KiB | row 2: fast H16/W20 |
| Fast (3) | 3 | dfast (hash8+hash5 dual-table single-probe) | 2 MiB | row 4: dfast H18/C18; rows 5-12: tagged row matcher (libzstd's own rows, attempts at parity; a narrow-head verdict cuts the budget — R33, json x1.19-1.29 at −4.8..−16.4% bytes) |
| Balanced (9) | 9 | hash chain + lazy2 | 4 MiB | H21 / C20 (aliased beyond 1MiB, like libzstd cLog<wLog) / depth 8 |
| Best (13) | 13 | btlazy2 over the DUBT tree | 64 MiB | rows 13-15 (libzstd btlazy2 territory, C's exact S4/5/6 ladder); W22 / H22-23; O(1) fill + search-time batch sort (`dubt.rs`, 2026-09-14) |
| Opt (17) | 17 | btopt (full port) | 8 MiB | rows 16-17; W23 / H22 / C22 (L17 row, ring capped below libzstd's C23: json −19.6% time for +0.16% dll) |
| Ultra (19) | 19 | btultra(+2) (full port) | 8 MiB | rows 18-22; 2-pass first-block statistics; rows 20-22 widen to W24-26 with the ring capped at 24 and hash at 22 (u64-slot memory guard; libzstd runs C25-27/H23-25 u32 there) |

First ladder A/B (2026-09-12, 32MiB json/text, vs libzstd CLI at the same level): json — every row at parity or denser except dfast rows +0.2-0.3% and opt rows +0.1-0.4%; chain rows -13..-17%, rows 13-15 -9..-10%. text — rows 1-8 far denser (window), rows 9-12 +2.6-2.7% behind (libzstd's row matcher), rows 13-16 -0.4..-5%, 17-22 +0.2-0.3%. Known inversions: our deep-chain rows (10-12) sit between our opt rows 13-16 on json (the chain is that strong there; libzstd's own ladder inverts json -1 vs -3 by 15%), and greedy row 5 trails dfast row 4 on interleaved-random shapes (libzstd's -1..-5 inverts the same way). All 22 levels × 5 corpora roundtrip through libzstd CLI. Fastest keeps 768KiB (beyond-W21 expansion still measured as a loss — see [falsified directions](negative.md); LDM remains the path for more reach).

Known-length sources resize their row (libzstd's `ZSTD_adjustCParams` port: window ≤ srcLog, hash ≤ wlog+1, chain/ring ≤ wlog): bulk paths pass the exact size, streaming passes the pledge, the CLI passes file metadata, unknown sizes keep the row (`Matcher::set_source_hint` / `FrameCompressor::set_size_hint`). Measured on adjusted bands (64KiB-4MiB json/text): ratio parity with libzstd at every probed level (chain rows -5..-15% denser); the 4KiB band keeps a ~+40% fixed-overhead gap on json at this measurement (block/header entropy coding of tiny payloads; ratio since closed to ±1-5% by the small-literal huffman fix), while small-call speed wins (4KiB L19 5.96 vs 16.20 ms/call incl. process spawn).

### Codec paths

<!-- prettier-ignore -->
| Capability | Status |
|---|---|
| slice/bulk codec | ✅ zero-copy encoding + state pools (thread-local for ST, cross-thread shared with the stream leases for MT jobs — R25); decoding writes directly into the flat output |
| streaming codec (read/write Encoder, Decoder) | ✅ streaming output byte-identical to bulk when no flush |
| MT encoding | ✅ bulk (overlap jobs) + streaming (bursts); workers>1; no_std reports Unsupported |
| MT decoding | ✅ segment-parallel (stage A pool + serial stage B) plus piece-parallel stage B on ramp frames (`decoding/mt_pieces.rs`, env-paired with the encoder ramp and engaged by measured validation — json.lvl3 mt8 3.2× serial-B at +0.00% size; see [mt-stream](perf/mt-stream.md)); plain frames: json 1.60× ST at mt16 (= 1.18× the zstd stream reference), dll100 1.83×/1.31×, skewed 1.19× (0.95× ref — the shape's band wanders), text flat 0.98× — remaining floors in todo 1 |
| frame checksum | ✅ optional on encode (+sidecar thread offload); decode verifies on every path (ST folds the compare into the trailer read, MT absorbs it inline in stage B over the hot just-executed ranges; `ChecksumMismatch` on mismatch — libzstd parity) |
| zstd-crate compat layer `zstdx::compat` | ✅ (dictionary decode works end-to-end) |
| dictionaries | decode ✅ (formatted + raw content: `Dictionary::load`, an id-0 dict applies to frames without dictID); encode ✅ (ST all paths, `EncoderOptions::dictionary`/`FrameCompressor::set_dictionary`/CLI `-D`; formatted dicts load content as match history + seed entropy tables + dictID, headerless raw content as pure match history with default repcodes — libzstd parity, frames decodable by libzstd; MT falls back ST); `dict/` training ✅ raw content (deterministic fastCover port: shuffled samples, sliding distinct-dmer scoring, k-sweep scored on a held-out split; `zstdx-bench train`; content AHEAD of `zstd --train` on the 3-seed fixture — every seed/level raw, formatted totals −1,862 B (7/12 cells; seed-22 trails 32-52 B/level) — after the R13 fix of the k-grid geometry and the finalize stats source/level). Formatted-dict emission ✅ (`dict/finalize.rs`, the `ZDICT_finalizeDictionary` port; `zstdx-bench train --formatted`). Dict frames with a pledged <=16 KiB payload adopt libzstd's <=16 KiB clevels rows (chain 4-8 2026-09-21, opt 11-22 2026-09-22 with fixture-tuned knobs — holdout −2.7..+2.9% raw by level, formatted ahead everywhere), and dict frames at lazy+ levels run libzstd's exact three-way sequence-table selection (2026-09-22 — holdout -4..-9 raw within ±1.8%). Remaining: the greedy -4 row's exact-once insert residue (+13..+96 B/seed, todo 6 — the -11/-12 btopt-row attribution was falsified under same-dict pairing) |
| forced window log (`InputShape::with_window_log`) | ✅ all paths; blocks cap at the window (RFC 8878 Block_Maximum_Size) |
| LDM | ✅ row 9 (W26/reach W22, dll100 −9.75%), ≥32 MiB windows on frame-continuous paths AND bulk/stream mt captures (cross-job history: whole-prefix strips in [32,64) MiB, windowed snapshot-ring jobs ≥64 MiB — dll100-mt ≡ st +0.17% at mt4==mt8), opt-family optLdm + lazy-family gap-parse (`encoding/ldm.rs`) |
| superblock / C FFI | superblock ✅ small-frame literals layout (`LitStreams`, R33 2026-10-01: known-length frames ≤ 64 KiB emit one-stream huffman literals sections — −7..−9 B on the T8 1K/4K cells, several ahead of libzstd including its `--target-compressed-block-size` form; 32 MiB outputs byte-identical; the sub-block splitting machinery falsified for ratio, [negative](negative/entropy.md) — [encoding](perf/encoding.md)); preSplit ✅ (2026-09-23, screened port on the balanced chain row); C FFI ✅ (`crates/zstdx-ffi` builds `libzstd.so`/`libzstd.a`: simple + context + streaming + reset/pledged/compress2 + CDict/DDict + loadDictionary + getDictID + metadata entries, 62 symbols, libzstd interop both directions, the C harness doubles as a reference-parity gate — [ffi](ffi.md)) |

## Completeness vs libzstd (subjective estimates, for targeting)

<!-- prettier-ignore -->
| Dimension | Estimate | Basis |
|---|---|---|
| decode features/correctness | ~90% | spec compliance, dictionary decode, corpus+fuzz |
| decode performance | bulk ahead across the board; streaming ~74-80% | streaming residue is on json/skewed/dll (core-vs-core x1.05-1.36 across compressible shapes; text x1.05-1.21, json x1.19-1.35), see [current snapshot](bench/snapshot.md) |
| encode features | ~90% | full 1-22 ladder + dictionary encode (ST) + formatted-dictionary training/emission + adjustable window + LDM + MT + streaming + preSplit (chain row, screened) + small-frame superblock literals (R33) + C FFI (R34, [ffi](ffi.md)) in place |
| encode speed | wins and losses split by tier | 2026-09-12 ladder speed curve (json 32MiB ST, CLI): chain rows 137/67/25 MiB/s at L6/9/12 vs libzstd 423/165/82 (0.25-0.33×, the known chain/json speed story); opt rows 7/4/2 at L13/17/19 vs 54/6/3 (0.13× at the btlazy2 slot, ~0.7× at btopt+); fastest/fast 447/340 vs 1118/812; small-call side wins (4KiB L19 5.96 vs 16.20 ms incl. spawn); tier-level x in the fresh [snapshot](bench/snapshot.md) (2026-10-01 pre-release pass: json tiers x1.47/1.04/**0.84**/1.28/1.01/1.03 — balanced ahead at +21% density and opt/ultra at parity after R30's hugepage-backed matcher buffers; text best/opt/ultra ahead x0.67/0.57/0.67, text.balanced x1.34 is the DUBT-head ratio trade; skewed best x1.88, opt/ultra ahead x0.95; zeros fastest/fast x0.27/0.18; dll tiers x1.03/0.97/1.31/1.42/1.09/1.13 with −49.6/−31.7% size at fastest/fast and −13..−16% from balanced up; stream-mt8 ahead on every json/text tier (text.balanced x0.98); dll100 stream-mt8 first measured — ahead only at fast/opt, trailing best at x14.27 (todo 17); json l9 small payloads at x1.13/0.93/0.66/1.00 via R27's head verdict) |
| encode ratio | matched at every tier | 2026-10-01 full-corpus `ratio` sweep vs libzstd at same numeric levels (32 MiB, all modes; byte-identical to the 09-28 sweep — R33's ladder diet moves only json rows outside the tier set, R34-R36 touch no encoder output on this corpus): geo-mean +8.69% denser; the text.best −0.15..−0.23% cells were closed 2026-09-18 by the cold-head probe step (now +0.12..+0.20%), leaving json.fast mt −0.12..−0.14% and skewed.opt −0.07% (near-tie) as the recorded losing residues; text.balanced holds +1.65% (cold-start DUBT head); 4KiB json ±1-5%; dll best/opt/ultra −12.8/−16.2/−16.5% size vs the zstd crate (R18 2026-09-24 closed the long-mode gap: the far-class floor lift (opt rows) and the row-9 gap-parse LDM consumer put dll100 balanced/opt/ultra 1.9/4.5/1.4% DENSER than `zstd -9/-17/-19 --long=26` ([matchers](perf/matchers.md))) |
| API/ecosystem | ~80% | bulk + streaming + compat (incl. dictionary constructors) + CLI (levels 1-22, -D, and the zstd 1.5.7 core surface: --long, --[no-]check, --stream-size/--size-hint, -M, --filelist/--file, --single-thread, --[no-]progress, --ignore-errors, -- end-of-flags; `-l/--list` with zstd 1.5.7's byte-identical column layout/error categories; `--train` formatted dictionaries via the `dict_builder` passthrough feature, interop round-trips both directions; unsupported flags such as --sparse are rejected loudly) + C FFI (`crates/zstdx-ffi` builds `libzstd.so`/`.a`: simple + context + streaming + reset/pledged/compress2 + CDict/DDict + loadDictionary + getDictID + metadata entries, 62 symbols, libzstd interop both directions — [ffi](ffi.md)) (R34-R35 2026-10-01; `decoding::frame` made public for header inspection); missing experimental-API entries (deliberate), language bindings, benchmark mode, --patch-from/--adapt |

Note: the [comparison page](comparison.md) is a design-level A/B record — design reasoning and code-symbol indices across both trees, not a current-state snapshot. The authoritative capability/gap assessment is this page.

## Code structure (crates/zstdx/src)

<!-- prettier-ignore -->
| Module | Contents |
|---|---|
| `bit_io/` | bit readers/writers (reverse bit reader BitReader, BitWriter) |
| `blocks/` | block-level parsing |
| `decoding/` | decoder: frame/block/literals/sequence decoding, `sequence_execution.rs` (fused execution), `flat_buffer.rs` (streaming flat window), `ringbuffer.rs` (dictionary path), `mt.rs` (segment-parallel) |
| `encoding/` | encoder: `frame_compressor.rs`, `match_generator/` (driver in `mod.rs`, strategy loops in `parse_*.rs`, level ladder in `params.rs`), `opt.rs` (optimal parsing), `levels/fastest.rs`, `mt.rs` (bulk MT), `async_checksum.rs` (sidecar checksum), `block_enc/compressed.rs` (entropy-coded blocks), `seq_codes.rs` |
| `fse/` `huff0/` | entropy codecs |
| `xxh64.rs` | in-tree checksum (shared by encode/decode; zero external deps at runtime) |
| `bulk.rs` `stream/` | one-shot and streaming APIs (`encoder_core.rs` / `encoder_mt.rs` (+ `mt_pool.rs` worker leases)) |
| `compat/` | zstd-crate compat layer |
| `dict/` | raw-content dictionary training, fastCover port (feature = "dict_builder") |
| `level.rs` `options.rs` | level enums and encode/decode options |

## Verification discipline (every commit)

Atomic commits (single-line msg) + fmt + all-green debug/release/no-default tests + full-corpus interop roundtrip against libzstd (CLI) + corruption smoke (random corruption, 0 panic) + dump differential checks (deterministic output as a free regression probe) + perf A/B as back-to-back same-machine reruns via git stash. Details and counterexamples in [engineering and benchmarking methodology](pitfalls/workflow.md).
