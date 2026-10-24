# Introduction

This page explains what the zstdx handbook covers, which document holds which material, and how to read the measurements inside it.

## What this handbook is

A topical knowledge base for zstdx development. It holds the current status, the remaining work, performance attribution and comparisons, falsified directions, and engineering pitfalls. Conclusions are archived into the topic page they belong to after each batch of work.

## Document roles

<!-- prettier-ignore -->
| Document | Role | Update model |
|---|---|---|
| `docs/` (this handbook) | Topical knowledge base: current status, todos, performance records, comparisons, negative results, pitfalls | Archived after each batch of work |
| Root `README.md` | Published readme and the crates.io / docs.rs front page: capability overview, API quick tour, headline figures against libzstd, with the full tables in [bench/snapshot](dev/bench/snapshot.md) | Updated at release or bench refresh |
| `AGENTS.md` and the local `prompt.md` | Agent working files with branch rules and batch prompts. `prompt.md` stays out of git | Updated per working batch |

## Reading conventions

- Every performance figure carries its measurement timestamp. The test machine (AMD Zen4 32C) has ±10% noise plus a slow drift phase, so absolute values are not comparable across time windows. Only interleaved A/B comparisons are trusted.
- Numbers generally trace to the wide matrix ([bench/matrix](dev/bench/matrix.md)) or to the dated prose on each page.
- Commit short hashes can be inspected with `git show <hash>`.

## Glossary

<!-- prettier-ignore -->
| Term | Meaning |
|---|---|
| Fastest, Fast, Balanced, Best, Opt, Ultra | Named level tiers. They alias numeric levels 1, 3, 9, 13, 17 and 19. |
| fast, dfast, chain, row, btlazy, opt | Matcher strategies. `fast` uses a single-probe table, `dfast` two tables, `chain` a hash chain with a lazy search, `row` tagged rows, `btlazy` a lazy search over a binary tree, and `opt` an optimal parse over a binary tree with dynamic programming. |
| ST, MT | Single-thread and multi-thread. |
| bulk, streaming | The one-shot in-memory API and the streaming API. |
| x | Ratio notation, written as x1.45. Our time divided by the reference time, so values below 1 mean we are faster. |
| Interleaved A/B | Both sides alternate per round and medians are taken, which amortizes machine drift. |
| restart point | A block boundary whose entropy state is self-describing. MT decode cuts segments there, and these points match zstd `-T` job boundaries. |
| wildcopy, overlapCopy8 | A libzstd-style fixed-width overwriting copy, and bit tricks for overlapping copies with an offset below 8. |
| SeqWord, packed sequence stream | A 24-byte single-stream sequence representation with u32 codes, u64 add-bits and u8 widths, emitted directly by the matcher. |
| rep0, rep1, repcode | The zstd repeated offset mechanism. |
| gain gate | The chain store decision `ml*4 ≥ ilog2(offset)+7`. |
| Six corpus shapes | json (semi-structured), text (flattened source code), skewed (16-letter alphabet), random and zeros, 32 MiB each, plus dll100 (100 MB of concatenated system ELF binaries from `bench/gen_big.sh`). |
| dump cross-check | A byte-level output snapshot comparison across all levels. It is a free regression probe for changes that must leave the output unchanged. |
