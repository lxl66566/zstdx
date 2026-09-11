# Status Overview

> As of `27b91cf` (2026-09-10). Branch goal (AGENTS.md): feature parity with upstream zstd and performance beyond it; no upstreaming; arbitrary unsafe / SIMD / new instruction sets allowed.

## Capability matrix

### Compression levels

| Level | ≈zstd | Matcher strategy | Window | Notes |
|---|---|---|---|---|
| Uncompressed | 0 | raw block | — | |
| Fastest | 1 | fast (hash5 single-probe table) | 768 KiB | |
| Fast | 3-5 | dfast (hash8+hash5 dual-table single-probe) | 2 MiB | W21, aligned with libzstd L3-9 |
| Balanced | 6-9 | hash chain + lazy | 2 MiB | H20 / C20 (aliased beyond 1MiB, like libzstd cLog<wLog) / depth 8 |
| Best | 10-15 | low-spec optimal parser | 4 MiB | W22 / H22 / C22; 16 compares / targetLength 32 |
| Opt | 16-17 | btopt (full port) | 8 MiB | W23 / H22 / C23 (libzstd L17 row) |
| Ultra | 18-22 | btultra(+2) (full port) | 8 MiB | W23 / H22 / C23; 2-pass first-block statistics |

`approximate_zstd` maps numbers 1-22 to the nearest tier; the CLI accepts all levels. Fast/Balanced run W21 and Best/Opt/Ultra W22/W23 (libzstd's own L3-19 large-input windows; landed 2026-09-12 — the 100MB-binary corpus showed the 1MiB window alone cost 26-30% (fast/balanced) to 42-44% (best/opt/ultra) ratio there; same-window outputs match libzstd within 0.03%); Fastest keeps 768KiB (beyond-W21 expansion still measured as a loss — see [falsified directions](dev/negative.md); LDM remains the path for more reach).

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
| encode ratio | matched at every tier | dll: Best/Opt denser than zstd-12/16 (4.86 vs 4.45, 5.01 vs 4.82), Ultra 7.7% behind zstd-19; json: Opt +5%, Best +11%, Ultra parity; worst cell text.balanced.bulk-st −0.61% (json.ultra.bulk-mt was −1.85%, closed to −0.13% by the Ultra job-boundary statistics seeding) |
| encode speed | wins and losses split by tier | leading on text/skewed/zeros at multiple tiers; json low tiers behind 1.3-1.8×; Best/Opt/Ultra speed behind (json Opt 0.46× after W23 — the reach/speed trade mirrors libzstd's own ladder) |
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
