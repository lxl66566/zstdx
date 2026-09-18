# Introduction

## What This Handbook Is

Development-log handbook for the `ext` branch (the deep-optimization fork of zstdx): the dozen-odd process documents at the repo root were merged by topic, deduplicated, and proofread into this structured knowledge base. The goal is **easy to extend, easy to query** — after each batch of optimizations, conclusions are archived into the corresponding topic page instead of spawning yet another running log document.

## Division of Labor Among Documents

<!-- prettier-ignore -->
| Document | Role | Update model |
|---|---|---|
| `docs/` (this handbook) | Topical experience: technical designs, completed work, todos, negative results, pitfalls, performance comparisons | Archived after each batch of work |
| Root `README.md` | Published readme (crates.io/docs.rs front page): API quick tour + headline bench table mirroring [bench/snapshot](dev/bench/snapshot.md) | Updated at release or bench refresh |
| `AGENTS.md` / `prompt.md` (local, not in git) | Agent working files (branch rules, batch prompts) | — |

## Reading Conventions

- **Round numbering is ignored**. The original documents' "Round N / rN / Batch N / M1 / D6 / E9" labels overlap across documents and are non-contiguous; this handbook is organized by topic and preserves sequence only where causality makes it meaningful.
- **Every performance figure carries its measurement timestamp**. The test machine (AMD Zen4 32C) has ±10% noise plus a slow drift phase, so absolute values are not comparable across time windows; only interleaved A/B comparisons are trusted. Numbers generally trace to the wide matrix ([bench/matrix](dev/bench/matrix.md)) or the dated prose on each page.
- Commit short hashes can be inspected with `git show <hash>`.

## Glossary

<!-- prettier-ignore -->
| Term | Meaning |
|---|---|
| Fastest / Fast / Balanced / Best / Opt / Ultra | Compression-level ladder (≈zstd 1 / 3-5 / 6-9 / 10-15 / 16-17 / 18-22) |
| fast / dfast / chain / opt(bt) | Matcher strategies: single probe table / dual tables / hash chain + lazy / optimal parse (binary tree + DP) |
| ST / MT | Single-thread / multi-thread |
| bulk(slice) / streaming(st) | One-shot in-memory API / streaming API |
| xslow | Our time ÷ zstd time; >1 = we are slower |
| Interleaved A/B | Both sides alternate per round and medians are taken; amortizes machine drift |
| restart point | Block boundary whose entropy state is self-describing (MT decode segmentation points = zstd `-T` job boundaries) |
| wildcopy / overlapCopy8 | libzstd-style fixed-width overwriting copy / bit tricks for overlapping copies with offset<8 |
| SeqWord / packed sequence stream | 16B single-stream sequence representation emitted directly by the matcher |
| rep0/rep1/repcode | zstd repeated offset mechanism |
| gain gate | chain store decision `ml*4 ≥ ilog2(offset)+7` |
| Five corpus shapes | json (semi-structured) / text (flattened source code) / skewed (16-letter alphabet) / random / zeros, 32MiB each |
| dump cross-check | Byte-level output snapshot comparison across all levels; a free regression probe for "changes that must not alter output" |

## Source Document → Handbook Mapping

<!-- prettier-ignore -->
| Original document | Content | Archived into |
|---|---|---|
| Early-Optimization.md | Combined summary of rounds 1-3 (mostly decoding + encoder matcher rewrite) | perf/decoding, perf/matchers, pitfalls, negative |
| Early-Encoding-Optimization.md | Encoder rounds 4-6 | perf/matchers, perf/encoding, pitfalls |
| ENCPERF.md | Encoder rounds r11-r15 + final wrap-up | perf/encoding, bench, pitfalls, negative |
| 0909-Micro-Optimizations.md / PERF2.md | Second extreme-performance round (redundant pair; the fuller PERF2 was used) | perf/decoding, perf/mt-stream, pitfalls |
| PERF3.md | Third extreme-performance round (branch miss / chain-break fixes) | perf/decoding, negative, pitfalls |
| Multithreaded-Optimization.md | MT codec + level ladder | perf/matchers, perf/mt-stream, completed |
| PLAN-mt-stream.md | Streaming MT design and data | perf/mt-stream, bench/matrix |
| WORK.md | Latest batch (MT prefill / u32 table entries / streaming MT etc.) | perf/*, pitfalls, todo |
| BENCH-MATRIX.md | Wide perf matrix | bench/matrix |
| COMPARE.md | Implementation-level comparison against libzstd | status, bench, todo |
