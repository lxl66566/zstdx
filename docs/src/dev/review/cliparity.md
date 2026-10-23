# R67 review — CLI parity (zstdx vs zstd v1.5.7)

Scope: `crates/zstdx-cli` — argument semantics, exit statuses, stderr shapes, and the self-contained modes (`-l` list, `-b` bench, `--train`, `--patch-from`, `--adapt`, size flags). Reference: `zstd` v1.5.7 CLI (zstdcli.c / fileio.c / dibio.c / benchzstd.c in /root/programs/fork/zstd/programs).

Method: a differential harness ran matched invocations of `zstd` and `zstdx` in mirrored sandboxes (fresh template per case: text/binary/empty/zero/random payloads, multi-frame, skippable-prefix, dict frame, w30 frame, corrupted/truncated/magic4/badmagic streams, 20 trainer samples + a reference-trained dict), comparing exit code, stdout, stderr (only `zstdx: ` → `zstd: ` normalized) and artifacts (`.zst` outputs compared by decoding both, everything else byte-wise). Two case sets: 57 (de)compression/flags cases, 58 phase-2 cases (list/bench/train/patch/adapt/size/collect). The harness lives outside the repo (scratch); its shape — casefile lines `NAME :: COMMAND` with `$ZSTD` bound per side, sandbox copy, cmp of code/stdout/normalized-stderr/file-set, per-file sha1 + decode-compare of `.zst` — is fully described here so a future round can replay it.

## Divergences fixed

| # | Case | Finding | Fix |
|---|---|---|---|
| 1 | rm-keep / ignore-errors / filelist / NEXT_FIELD | `--rm`/`-k` last-flag-wins (raw-argv scan; clap occurrence indices count default values); `--ignore-errors` is a hard parser rejection in 1.5.7; `--filelist -` has no stdin form (separated-value error, `=-` a literal path); value flags reject a following `-` entry (`command cannot be separated...`, `missing command argument`) | f0ddd5b0 |
| 2 | stderr per-site shapes | decode failures map to zstd error-name lines (checksum/content-size/dict-mismatch/premature-end/unsupported-format) + FIO window help block; dict stat (`error 31 : Stat failed...`), unknown suffix, overwrite prompt incl. `-q` decline, can't-stat / is-a-directory / not-regular-file collection diagnostics; `.tzst` → `.tar` | f0ddd5b0 |
| 3 | c-dict-missing, patch-from-missing-ref | message matched but exit code was 1; zstd exits 31 (fileio EXM_THROW carries the error number out) | `Exit` enum threading; `Diagnostic` carries the code. 75bbb083 |
| 4 | c-stream-size-wrong | pledged-size mismatch must be `error 11 : Src size is incorrect ` + exit 11 (was a long engine-flavored message + exit 1) | map `Error::PledgedSizeMismatch` (both error-chain shapes) to a code-11 Diagnostic. 75bbb083 |
| 5 | c-long9 / c-long31 | `--long=9`: `error 11 : Parameter is out of bound ` + exit 11 (zstd rejects below ZSTD_WINDOWLOG_MIN). `--long=31`: zstd clamps silently — our clamp warning removed | 75bbb083 |
| 6 | d-stdin-empty | zstd's stdin name is the literal `/*stdin*\` (fileio.h stdinmark) in every diagnostic; zero bytes read = `unexpected end of file`, 1-3 bytes = `unknown header` | `STDIN_NAME` const + `CountingReader` feeding `read_bytes` into `decode_failure_lines`; works for files and stdin uniformly. 75bbb083 |
| 7 | train-basic | `--train` was a feature-gated stub: the release binary printed "training mode not available" and exited 1 | `dict_builder` is now a default feature of zstdx-cli; the trained dict comes out byte-identical to ZDICT's (harness FILES check passes). 75bbb083 |
| 8 | train-few-samples | zstd (dibio.c): `Error 14 : nb of samples too low` + exit 14 when samples < 5 | aligned rule + dibio's no-prefix EXM_THROW shape. 75bbb083 |
| 9 | unknown-flag | zstd: `Incorrect parameter: <arg> ` + exit 1; clap printed its usage block and exited 2 | clap `UnknownArgument` intercepted in main. 75bbb083 |
| 10 | b-split | zstd 1.5.7 has `-B` only — `--split=64K` is an unknown argument (rejected), our CLI accepted it as a long alias | alias removed; falls into the `Incorrect parameter` path (first line + exit code byte-match). 75bbb083 |
| 11 | b-missing | benchzstd.c: bare `Error loading files` (no prefix, no name, no trailing newline) + exit 15 | `BenchFailure::Load` variant; stat/open/read failures in gather_files/load_corpus all map to it. 75bbb083 |

Final differential: cases1 57/57 OK; cases2 50/58 OK, 8 residual divergences (below; b-decode passed on the rerun, confirming the spinner glyph is timing-flaky).

## Accepted deviations

- **l-verbose / l-vv**: zstd prints `*** Zstandard CLI (64-bit) v1.5.7, by Yann Collet ***` (and at -vv `Compressing with N worker threads`, N machine-dependent). Printing zstd's banner from zstdx would be false provenance; the thread line is MT-config dependent.
- **progress-flags**: `--progress` at `-q` forces zstd's summary line and its `Read: ... ==> N%` progress drawing (raw \r animations). Our progress runs through indicatif with different bytes; matching zstd's renderer is not worth it.
- **b-multi-levels / b-blocksize / b-decode**: bench progress redraw differs in one spinner glyph (`|` vs `/`) and number-width spacing — timing-dependent even between two zstd runs.
- **b-split residual**: after the Incorrect-parameter line zstd prints its full usage block at displayLevel >= 2; our usage text is clap-generated and differs anyway.
- **pass-through** (found by probing, not in the case sets): `zstd -d -f -c <not-a-frame>` copies the input through and exits 0 (legacy pass-through when force + stdout); zstdx rejects as unsupported format. A mode, not a stderr shape — follow-up below.

## Follow-ups

- **train-basic stderr**: zstd prints `COVER_warnOnSmallCorpus` (`WARNING: The maximum dictionary size ...`) at displayLevel 1 when dmers/maxDict < 10. Needs the trainer to expose its dmer count (cross-crate API, feature-gated).
- **Encoder ratio gap**: our level-3 frame of the 316 KiB text corpus is 69575 B vs zstd's 69544 B (+0.045%; --no-check 69571 vs 69540). Invisible to artifact-compare-by-decode, caught by `-l`'s ratio column (4.650 vs 4.652). Engine-side item (level-3 parameter/match-finding parity), not CLI.
- **Pass-through mode** (above).
- **unknown header edge**: 1-3-byte *file* inputs now map like zstd; `zstd -q -d -f -c` on them engages pass-through (see above), which we do not implement.

## Commits

- f0ddd5b0 cli: align flag semantics and stderr formats with zstd v1.5.7 (rm/keep, ignore-errors, filelist, NEXT_FIELD, stderr shapes, tzst, collect_input Stat/Raw)
- 75bbb083 cli: zstd exit codes, stdin diagnostics, --train default, unknown-flag and --split rejection

## Tests

`cargo test -p zstdx-cli`: 97 green (43 unit + 54 integration; the train suite is now active in the default build — it was feature-gated to zero before). Pins updated: long9/long28/stream-size exit codes and messages, --split rejection, partial-magic `unknown header`, bench smoke `Exit::Success`, train's dibio-shaped report. `cargo test -p zstdx` (debug): green.
