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
## Closeout (2026-10-23)

All four follow-ups landed or attributed:

- **Pass-through landed**: `--pass-through`/`--no-pass-through` flags plus fileio's default (armed when `-f` and the destination is stdout). The decode path is now fileio's frame loop — peek the next magic, discard skippable frames, decode one data frame per `single_frame` decoder chained over the peeked bytes — so every engagement point matches: bad magic, 1-3-byte tails, and trailing garbage after decoded frames copy through and exit 0; the empty source and non-frame input without pass-through still fail with the same lines. Verified by a 20-case differential against the system zstd 1.5.7 (stdout, stderr normalized, exit codes all equal).
- **The frame loop surfaced a real stream-API bug**: `read::Decoder::single_frame` consulted its flag only after `init_next_frame` had already parsed the next frame's header, so a drained single-frame read left the source 6+ bytes past the frame boundary and the staged bytes died with the decoder (a multi-frame stream decoded its second frame's header into the void and the loop then read garbage as magic). Fixed in `stream/read.rs`; a crate test pins the source position at the next frame's first byte.
- **Skippable-only streams now exit 0** with empty output like zstd (the old whole-stream decoder errored `unknown header`); a skippable frame truncated mid-content maps to zstd's `Read error (39) : premature end`.
- **Foreign-format refusals gained**: the gzip/xz/lzma/lz4 magics refuse with fileio's per-format lines (`gzip file cannot be uncompressed (zstd compiled without HAVE_ZLIB) -- ignored` etc., true of this build) before the pass-through arm — matched against the system zstd byte for byte.
- **train-basic stderr landed**: DiB_train's level-2 small-corpus pair (`!  Warning : data size of samples too small...`, total sample bytes with a 128 KiB per-file cap < 8× maxdict) plus `COVER_warnOnSmallCorpus`'s level-1 line over the trainer's dmer count (`TrainOutcome.dmers`, fastCover's `ctx.nbDmers`; byte-identical to zstd's on a shared corpus, dmers 15545 both sides). `-q` keeps the level-1 line, `-qq` silences both. Still accepted: the `Loading ...`/`Trying N different sets of parameters`/`k=50` progress block (renderer-class).
- **Encoder ratio gap attributed, not a defect**: a level-3 size sweep (json/text/skewed at 64K-32M plus natural source text at 316K/1M) reads −0.44..+0.21% around parity — the CLI review's +0.045% text cell sits inside the dfast row's recorded near-tie band (json 32 MiB ST +0.18%, the 09-12 ladder's "dfast rows +0.2-0.3%" class), not a new regression. No probe opened.

## Commits

- f0ddd5b0 cli: align flag semantics and stderr formats with zstd v1.5.7 (rm/keep, ignore-errors, filelist, NEXT_FIELD, stderr shapes, tzst, collect_input Stat/Raw)
- 75bbb083 cli: zstd exit codes, stdin diagnostics, --train default, unknown-flag and --split rejection
- 1e3bcb2 stream: single_frame stops at the frame boundary
- 1bec87f cli: pass-through mode, frame-loop decode, foreign-format refusals
- b9a1f1a cli,dict: --train small-corpus warnings at zstd parity

## Tests

`cargo test -p zstdx-cli`: 97 green (43 unit + 54 integration; the train suite is now active in the default build — it was feature-gated to zero before). Pins updated: long9/long28/stream-size exit codes and messages, --split rejection, partial-magic `unknown header`, bench smoke `Exit::Success`, train's dibio-shaped report. `cargo test -p zstdx` (debug): green. Closeout adds: the pass-through unit test (copy-through, frame+garbage, skippable positions, truncated skippable, foreign-format lines) and the small-corpus warning test (both lines, `-q`/`-qq` layering); decode walls unchanged (CLI A/B old vs new on json/text/skewed zst3/zst19, within 1 ms) and the `parity --cases 4000` differential stays clean.
