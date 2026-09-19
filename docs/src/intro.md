# Introduction

## What This Handbook Is

Topical knowledge base for zstdx development: current status, todos, performance attribution and comparisons, falsified directions, and pitfalls — organized by topic instead of as running logs. After each batch of work, conclusions are archived into the corresponding topic pages; the goal is **easy to extend, easy to query**.

## Division of Labor Among Documents

<!-- prettier-ignore -->
| Document | Role | Update model |
|---|---|---|
| `docs/` (this handbook) | Topical knowledge base: current status, todos, perf records, comparisons, negative results, pitfalls | Archived after each batch of work |
| Root `README.md` | Published readme (crates.io/docs.rs front page): API quick tour + headline bench table mirroring [bench/snapshot](dev/bench/snapshot.md) | Updated at release or bench refresh |
| `AGENTS.md` / `prompt.md` (local, not in git) | Agent working files (branch rules, batch prompts) | — |

## Reading Conventions

- **Stale round labels** (`r5`, `r7`, `r9-p1`, ...): leftovers from the pre-handbook running logs; ignore the numbering — sequence is preserved only where causality makes it meaningful.
- **Every performance figure carries its measurement timestamp.** The test machine (AMD Zen4 32C) has ±10% noise plus a slow drift phase, so absolute values are not comparable across time windows; only interleaved A/B comparisons are trusted. Numbers generally trace to the wide matrix ([bench/matrix](dev/bench/matrix.md)) or the dated prose on each page.
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
| SeqWord / packed sequence stream | 24B single-stream sequence representation (u32 codes + u64 add-bits + u8 widths) emitted directly by the matcher |
| rep0/rep1/repcode | zstd repeated offset mechanism |
| gain gate | chain store decision `ml*4 ≥ ilog2(offset)+7` |
| Six corpus shapes | json (semi-structured) / text (flattened source code) / skewed (16-letter alphabet) / random / zeros, 32MiB each + dll100 (100 MB concatenated system ELF binaries, `bench/gen_big.sh`) |
| dump cross-check | Byte-level output snapshot comparison across all levels; a free regression probe for "changes that must not alter output" |
