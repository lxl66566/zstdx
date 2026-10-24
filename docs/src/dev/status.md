# Status overview

This page states what the implementation can do today and how far it is from libzstd. It carries the capability table, the level ladder, the completeness estimates with their basis, the code layout, and the verification discipline each commit follows.

The snapshot is as of `0593f347`, 2026-10-21. The branch goal from `AGENTS.md` is feature parity with upstream zstd and performance beyond it. Upstreaming is out of scope, and arbitrary unsafe code, SIMD and new instruction sets are allowed.

## Capability table

<!-- prettier-ignore -->
| Capability | Status |
|---|---|
| Bulk codec | yes |
| Streaming codec | yes |
| MT encoding | yes |
| MT decoding | yes |
| Frame checksum | yes |
| `zstdx::compat` | yes |
| Dictionaries | yes |
| Forced window log | yes |
| LDM | yes |
| Superblock and C FFI | yes |

Each section below states the behavior behind one row and links to the page that owns the detail.

## Compression levels

<!-- prettier-ignore -->
| Tier | Level | Matcher strategy | Window | Notes |
|---|---|---|---|---|
| Uncompressed | 0 | raw block | none | data stored inside a zstd frame |
| Fastest | 1 | fast, hash5 single-probe table | 768 KiB | row 2: fast H16/W20 |
| Fast | 3 | dfast, hash8 and hash5 dual-table single-probe | 2 MiB | row 4: dfast H18/C18 |
| Balanced | 9 | hash chain with lazy2 | 4 MiB | H21/C20, aliased beyond 1 MiB like libzstd cLog<wLog, depth 8 |
| Best | 13 | btlazy2 over the DUBT tree | 64 MiB | rows 13-15, W22/H22-23, O(1) fill and search-time batch sort |
| Opt | 17 | btopt, full port | 8 MiB | rows 16-17, W23/H22/C22 |
| Ultra | 19 | btultra and btultra2, full port | 8 MiB | rows 18-22, 2-pass first-block statistics |

The ladder has one parameter row per numeric level, modeled on libzstd's `clevels.h` large-source table. The rows live in `LEVEL_PARAMS` in `encoding/match_generator/params.rs`. `Level::from_zstd(n)` maps exactly, and negative levels clamp to 1. The named tiers alias representative rows.

The Fast tier also covers rows 5-12, which use the tagged row matcher with libzstd's own attempts. A narrow-head verdict cuts that budget, and those rows reach json at x1.19 to x1.29 with 4.8% to 16.4% fewer bytes. The Best tier rows 13-15 sit in libzstd's btlazy2 territory with C's exact S4, S5 and S6 ladder, and the O(1) fill plus search-time batch sort live in `dubt.rs` and date from 2026-09-14. The Opt tier uses the L17 row with the ring capped below libzstd's C23, which costs json 19.6% less time and 0.16% more dll bytes.

Ultra rows 20-22 widen to W24-26 with the ring capped at 24 and the hash at 22. The ring carries two u32 link slots per position, where libzstd's chainTable carries one. libzstd's own C27/H25 geometry lands near 640 MiB per encoder and rides every MT worker's pooled state, while the caps hold about 144 MiB. Libzstd runs C25-27/H23-25 u32 in the same rows.

Fastest keeps its 768 KiB window. A beyond-W21 expansion was measured as a loss, as recorded in [falsified directions](negative.md), and LDM covers the longer reach.

### Known-length adjustment

Known-length sources resize their row through a port of libzstd's `ZSTD_adjustCParams`: window at most srcLog, hash at most wlog+1, and chain or ring at most wlog. Bulk paths pass the exact size, streaming passes the pledge, and the CLI passes file metadata. Unknown sizes keep the row. The entry points are `Matcher::set_source_hint` and `FrameCompressor::set_size_hint`.

On adjusted bands from 64 KiB to 4 MiB of json and text, ratio is at parity with libzstd at every probed level, and chain rows are 5% to 15% denser. The 4 KiB band kept a fixed-overhead gap of about 40% on json at that measurement, from block and header entropy coding of tiny payloads. The small-literal huffman fix closed that ratio to ±1-5%. Small calls win on speed: 4 KiB at L19 takes 5.96 ms per call against 16.20 ms, process spawn included.

### Ladder measurements

The first ladder A/B ran 2026-09-12 on 32 MiB json and text against the libzstd CLI at the same level. json is at parity or denser on every row except the dfast rows at +0.2% to +0.3% and the opt rows at +0.1% to +0.4%. Chain rows are 13% to 17% denser and rows 13-15 are 9% to 10% denser. text rows 1-8 are far denser through the window, rows 9-12 are 2.6% to 2.7% behind libzstd's row matcher, rows 13-16 are 0.4% to 5% denser, and rows 17-22 are 0.2% to 0.3% behind.

Two inversions are known. Our deep-chain rows 10-12 sit between our opt rows 13-16 on json, because the chain is strong there, and libzstd's own ladder inverts json levels 1 and 3 by 15% in the same direction. Greedy row 5 trails dfast row 4 on interleaved-random shapes, which mirrors libzstd's 1% to 5% inversion. All 22 levels across 5 corpora roundtrip through the libzstd CLI.

## Codec paths

### Bulk and streaming codecs

Bulk encoding is zero-copy and uses state pools, which are thread-local for ST and cross-thread shared through the stream leases for MT jobs. Decoding writes directly into the flat output. `compress_with` declares a `pledged_size` in the frame header, validated and byte-identical to the pledged stream, measured 2026-10-16, and the FFI one-shots ride it. Streaming output is byte-identical to bulk when no flush is issued. A forced window log through `InputShape::with_window_log` works on all paths, and blocks cap at the window per RFC 8878 Block_Maximum_Size. See [encoding](perf/encoding.md).

### MT encoding

Bulk uses overlap jobs and streaming uses bursts. `workers` above 1 engages the pool, and `no_std` reports `Unsupported`. See [mt-stream](perf/mt-stream.md).

### MT decoding

Decoding is segment-parallel with a stage A worker pool and a serial stage B. Ramp frames additionally get a piece-parallel stage B in `decoding/mt_pieces.rs`, which is env-paired with the encoder ramp and engaged by measured validation. On json.lvl3 at mt8 that reaches 3.2x over serial stage B at +0.00% size. Plain frames reach 1.65x ST on json at mt16, which is 1.23x the zstd stream reference, 1.90x ST and 1.36x the reference on dll100, and 1.25x ST on skewed, which is 0.96x the reference and wanders with the shape's band. text stays flat at 0.97x. The remaining floors are tracked as todo 1 in the [todo list](todo.md). See [mt-stream](perf/mt-stream.md).

The piece verdict is consumed only after staging completes, because an over-pledged frame could otherwise engage pieces off a partial prefix and silently drop the divergence. A handcrafted-frame regression covers that case. The piece gate narrows the pledge through `usize::try_from`, which covers the 32-bit wrap class. A seeded MT mutation smoke and the `decode_mt` fuzz target check MT output against sequential output byte for byte.

A prepared (parsed) dictionary steers decode off the parallel fast path in the same way as a raw dictionary. The parallel fallback builds a fresh frame decoder, which once dropped the dictionary silently. `threads_with_prepared_dictionary_decode` covers it.

### Frame checksum

The checksum is optional on encode and can offload to a sidecar thread. Decode verifies it on every path. ST folds the compare into the trailer read, and MT absorbs it inline in stage B over the hot just-executed ranges. A mismatch reports `ChecksumMismatch`, matching libzstd. See [decoding](perf/decoding.md) and [encoding](perf/encoding.md).

### Compatibility layer

`zstdx::compat` covers the full non-experimental surface after a sweep against zstd 0.13.3 on 2026-10-19. Bulk paths pledge the frame content size, matching the libzstd one-shot, and empty input declares 0. `Compressor::set_compression_level` clears the dictionary, `set_dictionary` takes the level, and `Decompressor::set_dictionary` is present, all matching upstream semantics. `compat::dict` provides `EncoderDictionary::copy` and `DecoderDictionary::copy` with one shared parse per handle, and a failed parse replays as an error where zstd_safe panics. The `dict_builder`-gated trainers are `from_samples`, `from_continuous`, `from_sample_iterator` and `from_files`.

On the stream side, the read side has `with_dictionary`, `with_buffer`, `set_pledged_src_size` and `include_checksum`. Both sides have `window_log`, `include_contentsize` and `with_prepared_dictionary`. A pledge under `include_contentsize(false)` degrades to an input-shape hint. `write::Decoder::on_flush` exists, and `AutoFlushDecoder` flushes on drop, which is the zstd shape. `Encoder` and `Decoder` are re-exported at the root, and `upper_bound` answers `ZSTD_getFrameContentSize` semantics.

The option surface is complete for the non-experimental items. `include_dictid` exists on both stream sides and on `bulk::Compressor` over the new `EncoderOptions::include_dictid` flag. `long_distance_matching(bool)` maps onto `EncoderOptions::long_distance_matching(LdmMode)` with the enable and auto modes, because the upstream bool cannot express disable. `with_ref_prefix` exists on `write::Encoder` and `read::Decoder` as the raw-dictionary load. `set_target_cblock_size` stays excluded by the sub-block-splitting falsification, recorded as todo 18 in the [todo list](todo.md).

Items that rest on `zstd_safe` are deliberately out: `stream::{raw,zio}`, `with_context`, `with_writer`, `with_encoder`, `set_parameter`, `context_mut`, and the experimental items. Prepared-dictionary decode is verified on every shape through bulk, read and write paths and through drop-flush, under a seeded corruption smoke. The `dict_builder`-off import lint is fixed. `Decompressor::set_dictionary`'s empty-slice detach note is exact: raw dictionaries detach, and id-keyed dictionaries stay reachable.

See the [user guide](../user.md) for the API tour and [comparison](comparison.md) for the design-level differences. `decoding::frame` is public for header inspection.

### Dictionaries

Decode handles formatted and raw-content dictionaries through `Dictionary::load`, and an id-0 dictionary applies to frames without a dictID. Encode covers all ST paths through `EncoderOptions::dictionary`, `FrameCompressor::set_dictionary`, the CLI `-D` flag and `--patch-from` with a raw-content reference under libzstd's refPrefix semantics. Frames interoperate in both directions.

A formatted dictionary loads its content as match history, seeds the entropy tables and declares its dictID. Headerless raw content acts as pure match history with default repcodes. Both match libzstd, and libzstd can decode the frames.

MT encode seeds the frame's first job from the dictionary exactly as the ST entries do, through `reset_with_dictionary` and a dedicated owned-window state kit. Later jobs keep the stock strip model like libzstd's zstdmt, with the CDict on job zero only and forceMaxWindow after. With a 110 KiB trained dictionary on 32 MiB json and text, dict-MT is within +12 to +29 B of dict-ST on text (+0.02% to +0.05%), +0.19% and +0.40% on json fastest at mt4 and mt8, and +0.01% on best. Walls for dict-ST to dict-MT4 are x3.0 to x8.1, and mt8 is x3.3 to x7.1 on json and x1.4 to x1.8 or x1.4 to x1.9 on text. Non-dict MT outputs are byte-identical in the st and mt4 all-levels dump differential against main.

At fast and balanced levels, the pre-existing dict-row parse dilution makes dict-ST lose to plain ST on 32 MiB homogeneous payloads, by +840 KB on json L9. Dict-MT confines that loss to job zero and comes out 2.1% and 12.4% smaller than dict-ST there.

Training produces raw content with a deterministic fastCover port: shuffled samples, sliding distinct-dmer scoring, and a k-sweep scored on a held-out split. It runs through `zstdx-bench train`. On the 3-seed fixture, content is ahead of `zstd --train` for every seed and level in raw form, and formatted totals are 1,862 B smaller across 7 of 12 cells, with seed-22 trailing 32 to 52 B per level. Formatted emission ships through `dict/finalize.rs`, the `ZDICT_finalizeDictionary` port, and `zstdx-bench train --formatted`.

Dict frames with a pledged payload of 16 KiB or less adopt libzstd's small clevels rows: chain at rows 4-8, measured 2026-09-21, and opt at rows 11-22, measured 2026-09-22, with fixture-tuned knobs. Holdout raw size lands within -2.7% to +2.9% by level, and formatted output is ahead everywhere. At lazy levels and above, dict frames run libzstd's exact three-way sequence-table selection, with holdout raw output 4 to 9 B smaller and within ±1.8%.

The width-4 dict-row semantics disarm above a 256 KiB span of payload plus dictionary, and the span key now rules every dict-row knob after the swap and unknown-arm holes were closed on 2026-09-23. One residue remains: the greedy -4 row's exact-once insert adds 13 to 96 B per seed, tracked as todo 6 in the [todo list](todo.md). The -11/-12 btopt-row attribution was falsified under same-dict pairing.

Review findings in this area are covered by tests. Stream-MT capture engagement once re-stamped a suppressed dictID on the armed fast-row header when `include_dictid(false)` met a dictionary, because re-serialization read the dictionary instead of the construction-time resolution. `dict_mt_capture_keeps_suppressed_dictid` covers it. The FFI streaming decoder once registered a ref'd dictionary by id only, so a dictIDFlag-suppressed frame decoded without it and failed with "Data corruption detected". It now registers the options-style null id, and `suppressed_dictid_stream_decodes_with_loaded_dict` covers it. The option tests had broken `--no-default-features` through `std::io::Write` imports where the encoder implements the crate's own io trait, and through an ungated workers arm. Both are fixed and the suite is re-gated.

Prepared-dictionary call economics closed as well. The parse resolves once per attachment. Pooled ST dictionary states restore a captured post-seed image of tables, filled trees and entropy, keyed to the dictionary identity, the level, the LDM mode and the payload size's seeding projection. Size-varying records inside one projection class stay on the hit path. See [encoding](perf/encoding.md).

### Long-distance matching

LDM arms on row 9, with W26, reach W22 and 9.75% smaller dll100 output. It also arms on windows of 32 MiB and more for frame-continuous paths and for bulk and streaming MT captures, which carry cross-job history. Whole-prefix strips cover windows in [32, 64) MiB, and windowed snapshot-ring jobs cover windows of 64 MiB and more. dll100-mt equals ST +0.17% at mt4 equal to mt8. The opt family uses optLdm, the lazy family uses the gap-parse, and both live in `encoding/ldm.rs`. See [matchers](perf/matchers.md).

### Small frames and C FFI

The superblock layout handles small-frame literals through `LitStreams`. Known-length frames of 64 KiB or less emit one-stream huffman literals sections, which measured 2026-10-01 saves 7 to 9 B on the small-payload 1K and 4K cells and lands several bytes ahead of libzstd, including its `--target-compressed-block-size` form. 32 MiB outputs stay byte-identical. Sub-block splitting was falsified for ratio, as recorded in [negative](negative/entropy.md) and [encoding](perf/encoding.md). preSplit is a screened port on the balanced chain row.

`crates/zstdx-ffi` builds `libzstd.so` and `libzstd.a` with the simple, context, streaming, reset, pledged, compress2, CDict, DDict, loadDictionary, getDictID and metadata entries. That is 62 symbols, with libzstd interop in both directions. The C harness doubles as a reference-parity gate. See [ffi](ffi.md).

## Completeness against libzstd

These are subjective estimates used for targeting.

<!-- prettier-ignore -->
| Dimension | Estimate | Basis |
|---|---|---|
| decode features and correctness | ~90% | Spec compliance, dictionary decode, and corpus plus fuzz coverage. Parity rounds on 2026-10-23 swept entropy headers, sequence execution, literals tails and dictionary paths against libzstd. Decode window semantics match libzstd: streaming enforces the buffer generation model, one-shot decodes the whole history by design, and probes pin each decode path. See the [parity review](review/parity.md) and [window semantics review](review/winsem.md). |
| decode performance | bulk ahead across the board, streaming about 75-81% | The streaming residue sits on json, skewed and dll. Core-to-core x is 1.04 to 1.34 across compressible shapes, text is x1.04 to 1.20 and json is x1.13 to 1.33. See [snapshot](bench/snapshot.md). |
| encode features | ~90% | The full 1-22 ladder, dictionary encode on ST and MT, formatted-dictionary training and emission, adjustable window, LDM, MT, streaming, preSplit on the chain row, small-frame superblock literals, and the 62-symbol C FFI. |
| encode speed | wins and losses split by tier | The ladder speed curve and the current per-tier x are below. |
| encode ratio | matched at every tier | The full-corpus ratio sweep measured 2026-10-18 is below. |
| API and ecosystem | ~85% | Bulk, streaming, the full non-experimental compat surface, and the CLI's zstd 1.5.7 core surface. The C FFI ships 62 `libzstd` symbols. The missing entries are the experimental APIs, deliberately, and language bindings. |

### Encode speed measurements

The 2026-09-12 ladder speed curve on json 32 MiB ST through the CLI gives chain rows 137, 67 and 25 MiB/s at L6, L9 and L12 against libzstd's 423, 165 and 82, which is 0.25x to 0.33x. Opt rows give 7, 4 and 2 at L13, L17 and L19 against 54, 6 and 3, which is 0.13x at the btlazy2 slot and about 0.7x at btopt and above. Fastest and fast give 447 and 340 against 1118 and 812. Small calls win: 4 KiB at L19 takes 5.96 ms including spawn against 16.20 ms.

The fresh 2026-10-18 snapshot reads json tiers at x1.45, 1.03, 0.77, 1.23, 1.01 and 1.00. Balanced is ahead at 21% more density and ultra is at parity. text reads x0.61, 0.55 and 0.64 at best, opt and ultra, and x1.10 at balanced. skewed reads x1.91 at best, with opt and ultra ahead at x0.92 and x0.95. zeros reads x0.27 and x0.18 at fastest and fast. dll reads x1.06, 0.97, 1.30, 1.26, 1.09 and 1.12, with 49.6% and 31.7% smaller output at fastest and fast and 13% to 16% smaller from balanced up.

Stream-mt8 wins every json and text tier, including text.balanced at x0.95. On dll100, stream-mt8 is ahead at fast and opt, and ultra is at libzstd-mt parity at x0.98. Fastest at x2.36 and best at x3.12 carry the capture's 49.6%-smaller density and are not iso-ratio, tracked as todo 17 in the [todo list](todo.md). json small payloads at l9 read x1.11, 0.90, 0.64 and 0.99 through the head verdict.

### Encode ratio measurements

The 2026-10-18 full-corpus `ratio` sweep runs all modes on 32 MiB at the same numeric levels as libzstd. Outputs are byte-identical to the earlier sweeps, and the geometric mean is 8.69% denser.

The text.best cells read +0.12% to +0.20% after the cold-head probe step, measured 2026-09-18. The recorded losing residues are json.fast mt at -0.12% to -0.14% and skewed.opt at -0.07%, which is a near-tie. text.balanced holds +1.65% from the cold-start DUBT head, and 4 KiB json sits at ±1-5%.

Against the zstd crate, dll best, opt and ultra are 12.8%, 16.2% and 16.5% smaller. The far-class floor lift on opt rows and the row-9 gap-parse LDM consumer, measured 2026-09-24, put dll100 balanced, opt and ultra 1.9%, 4.5% and 1.4% denser than `zstd -9/-17/-19 --long=26`. See [matchers](perf/matchers.md).

### API and ecosystem detail

The CLI covers levels 1-22, `-D`, and the zstd 1.5.7 core surface: `--long`, `--[no-]check`, `--stream-size`, `--size-hint`, `-M`, `--filelist` and `--file`, `--single-thread`, `--[no-]progress`, `--` end-of-flags, and `-l/--list` with zstd 1.5.7's byte-identical column layout and error categories. Unsupported flags such as `--sparse` are rejected loudly, and `--ignore-errors` is rejected with zstd 1.5.7's own diagnostic because that version removed the flag.

`--train` emits formatted dictionaries through the `dict_builder` passthrough feature, with interop round trips both directions and zstd's small-corpus warning pair: DiB's level-2 lines plus `COVER_warnOnSmallCorpus` over the trainer's dmer count, byte-identical.

`--patch-from` provides diff mode. The reference goes through the `-D` load path as raw content, the window is the next power of two above the source clamped to 10-27 per fileio.c, the decode bound is raised to the reference, and the flag implies `--ultra`. It interops with libzstd in both directions.

`--adapt` is the fileio.c adaptation engine translated from per-MT-job frame-progression counters to per-chunk phase timings. It runs 8 MiB chunk frames with ±1 steps, using the reference's 9/8 output-bound and 33/32 encode-bound thresholds, a bare input-bound compare and a 1/6 s refresh gate. `min=` and `max=` parse per parseAdaptParameters with the engine's 1-22 defaults, and the starting level clamps per zstdcli.c. It works with any `-T`, because re-leveling is per frame. `-vv` traces the decisions. The multi-frame output lists under `-l` and interops both directions.

Pass-through decode is controlled by `--pass-through` and `--no-pass-through`, and defaults on with `-f` to stdout. It applies gzip `-df` semantics over a fileio-style frame-granular decode loop, which also brought skippable-only streams to zstd's clean exit 0 and the foreign-format magic refusals to their exact lines.

`-b#` is a `benchzstd.c` port with the `BMK_benchTimedFn` timing core, byte-identical result lines, `-i/-e/-B/--split/-bd`, and a byte-identical `lorem.c` synthetic corpus for the no-FILES case. Level 0 maps to the engine's raw-store level 0. The `-i0` budget takes benchfn.c's run_ms clamp, so a zero budget reports its first measured round instead of spinning to the half-second floor. `--patch-from` under `-b` is ignored, matching the reference, which aliases the patch reference into dictFileName only after the bench dispatch.

See the [user guide](../user.md) for flags and options.

## Code structure

<!-- prettier-ignore -->
| Module | Contents |
|---|---|
| `bit_io/` | bit readers and writers: reverse bit reader `BitReader`, `BitWriter` |
| `blocks/` | block-level parsing |
| `decoding/` | decoder: frame, block, literals and sequence decoding, `sequence_execution.rs` for fused execution, `flat_buffer.rs` for the streaming flat window, `ringbuffer.rs` for the dictionary path, `mt.rs` for segment-parallel decode |
| `encoding/` | encoder: `frame_compressor.rs`, `match_generator/` with the driver in `mod.rs`, strategy loops in `parse_*.rs` and the level ladder in `params.rs`, `opt.rs`, `levels/fastest.rs`, `mt.rs` for bulk MT, `async_checksum.rs` for the sidecar checksum, `block_enc/compressed.rs` for entropy-coded blocks, `seq_codes.rs` |
| `fse/`, `huff0/` | entropy codecs |
| `xxh64.rs` | in-tree checksum, shared by encode and decode, with zero external runtime dependencies |
| `bulk.rs`, `stream/` | one-shot and streaming APIs, with `encoder_core.rs` and `encoder_mt.rs` plus `mt_pool.rs` worker leases |
| `compat/` | zstd-crate compat layer |
| `dict/` | raw-content dictionary training and the fastCover port, behind the `dict_builder` feature |
| `level.rs`, `options.rs` | level values and encode and decode options |

## Verification per commit

Each commit is atomic with a single-line message, formatted with `cargo fmt`, and passes debug, release and no-default-feature tests. It also passes a full-corpus interop roundtrip against the libzstd CLI, a corruption smoke test with zero panics, and the dump differential checks that use deterministic output as a regression probe. Decoder-touching changes add the decode parity differential, `zstdx-bench parity`, which runs the craft-and-mutate corpus against the zstd CLI and exits nonzero on divergence. Performance work uses back-to-back same-machine A/B reruns through `git stash`. Details and counterexamples live in [engineering and benchmarking methodology](pitfalls/workflow.md).

The [comparison page](comparison.md) is a design-level A/B record with design reasoning and code-symbol indices across both trees. It is not a current-state snapshot. The authoritative capability and gap assessment is this page.
