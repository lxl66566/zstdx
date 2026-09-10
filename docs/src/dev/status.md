# Status Overview

> As of `27b91cf` (2026-09-10). Branch goal (AGENTS.md): feature parity with upstream zstd and performance beyond it; no upstreaming; arbitrary unsafe / SIMD / new instruction sets allowed.

## Capability matrix

### Compression levels

| Level | ≈zstd | Matcher strategy | Window | Notes |
|---|---|---|---|---|
| Uncompressed | 0 | raw block | — | |
| Fastest | 1 | fast (hash5 single-probe table) | 768 KiB | |
| Fast | 3-5 | dfast (hash8+hash5 dual-table single-probe) | 1 MiB | |
| Balanced | 6-9 | hash chain + lazy | 1 MiB | H20 / depth 8 |
| Best | 10-15 | low-spec optimal parser | 1 MiB | 16 compares / targetLength 32 |
| Opt | 16-17 | btopt (full port) | 1 MiB | |
| Ultra | 18-22 | btultra(+2) (full port) | 1 MiB | 2-pass first-block statistics |

`approximate_zstd` maps numbers 1-22 to the nearest tier; the CLI accepts all levels. The fixed window is a **deliberate trade-off**: bare window expansion to 2-4 MiB measured as a double loss (see [falsified directions](dev/negative.md)); expansion must be done together with LDM.

### Codec paths

| Capability | Status |
|---|---|
| slice/bulk codec | ✅ zero-copy encoding + thread_local state pool; decoding writes directly into the flat output |
| streaming codec (read/write Encoder, Decoder) | ✅ streaming output byte-identical to bulk when no flush |
| MT encoding | ✅ bulk (overlap jobs) + streaming (bursts); workers>1; no_std reports Unsupported |
| MT decoding | ✅ restart-point segmentation; serial stage B is the bottleneck, no scalability yet |
| frame checksum | ✅ optional on encode (+sidecar thread offload); decode auto-verifies in-tree xxh64 (MT path does not verify) |
| zstd-crate compat layer `zstdx::compat` | ✅ (dictionary decode works end-to-end) |
| dictionaries | decode ✅; encode ❌; `dict/` training half-done (known bugs) |
| LDM / superblock / adjustable window / C FFI | ❌ |

## Completeness vs libzstd (subjective estimates, for targeting)

| Dimension | Estimate | Basis |
|---|---|---|
| decode features/correctness | ~90% | spec compliance, dictionary decode, corpus+fuzz; missing MT-path checksum |
| decode performance | bulk ahead across the board; streaming ~75-85% | streaming residue is on json/skewed, see [current snapshot](dev/bench/snapshot.md) |
| encode features | ~60% | seven-tier ladder + MT + streaming in place; missing dictionary encode, LDM, superblock, adjustable window |
| encode ratio | matched at every tier | Ultra json 7.52 vs zstd-19 7.49; Opt beats zstd-16; Best beats zstd-12 |
| encode speed | wins and losses split by tier | leading on text/skewed/zeros at multiple tiers; json low tiers behind 1.3-1.8×; Best/Opt/Ultra speed behind |
| API/ecosystem | ~45% | bulk + streaming + compat + CLI; missing C FFI, language bindings, standard CLI argument surface |

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
