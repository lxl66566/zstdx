# CLI parity

This page records the comparison of the `zstdx` command line with zstd v1.5.7. It answers how arguments, exit codes, diagnostics and the self-contained modes were matched, which deviations remain accepted, and how the closeout pass-through work changed the decode loop. The comparison covers `crates/zstdx-cli`: argument semantics, exit statuses, stderr shapes, and the self-contained modes `-l` list, `-b` bench, `--train`, `--patch-from`, `--adapt` and the size flags. Reference: the `zstd` v1.5.7 CLI, namely `zstdcli.c`, `fileio.c`, `dibio.c` and `benchzstd.c` in `/root/programs/fork/zstd/programs`.

## Method

A differential harness ran matched invocations of `zstd` and `zstdx` in mirrored sandboxes, with a fresh template per case. The templates covered text, binary, empty, zero and random payloads, multi-frame, skippable-prefix, dictionary frame, w30 frame, corrupted, truncated, magic4 and badmagic streams, and 20 trainer samples plus a reference-trained dict. The harness compared exit code, stdout, stderr with only `zstdx: ` normalized to `zstd: `, and the artifacts. `.zst` outputs were compared by decoding both sides, and everything else was compared byte-wise.

The harness ran two case sets: 57 compression, decompression and flag cases, and 58 cases for list, bench, train, patch, adapt, size and collect. The harness lives outside the repo as a scratch tool. Its shape is a casefile of `NAME :: COMMAND` lines with `$ZSTD` bound per side, a sandbox copy, a comparison of exit code, stdout, normalized stderr and file set, and a per-file sha1 plus decode-compare of `.zst` files. That description is enough for a future round to replay it.

## Flag semantics

`--rm` and `-k` follow last-flag-wins through a raw-argv scan, because clap occurrence indices count default values. `--ignore-errors` is a hard parser rejection in 1.5.7. `--filelist -` has no stdin form, so a separated value is an error and `=-` is a literal path. Value flags reject a following `-` entry with `command cannot be separated...` or `missing command argument`.

An unknown flag now prints `Incorrect parameter: <arg> ` and exits 1 like zstd. clap used to print its usage block and exit 2, so `UnknownArgument` is intercepted in main.

Only `-B` exists in zstd 1.5.7, and `--split=64K` is an unknown argument there. The long alias is removed, so the argument falls into the `Incorrect parameter` path, with the first line and the exit code byte-matching.

## Exit codes and diagnostics

A missing dictionary or a missing patch reference matched the message but exited 1, where zstd exits 31 because fileio's EXM_THROW carries the error number out. The `Exit` enum now threads the code, and `Diagnostic` carries it.

A pledged-size mismatch now reports `error 11 : Src size is incorrect ` and exits 11. The previous behavior was a long engine-flavored message and exit 1. `Error::PledgedSizeMismatch` maps both error-chain shapes to a code-11 `Diagnostic`.

`--long=9` now reports `error 11 : Parameter is out of bound ` and exits 11, because zstd rejects values below ZSTD_WINDOWLOG_MIN. For `--long=31`, zstd clamps silently, so the clamp warning was removed.

The stdin name in every diagnostic is the literal `/*stdin*\` from fileio's stdinmark. Zero bytes read gives `unexpected end of file`, and 1 to 3 bytes give `unknown header`. A `STDIN_NAME` const and a `CountingReader` feed `read_bytes` into `decode_failure_lines`, so files and stdin behave uniformly.

Decode failures map to zstd error-name lines for checksum, content-size, dictionary mismatch, premature end and unsupported format, plus the FIO window help block. The diagnostics cover the dictionary stat line `error 31 : Stat failed...`, an unknown suffix, the overwrite prompt including the `-q` decline, the can't-stat, is-a-directory and not-regular-file collection diagnostics, and the `.tzst` to `.tar` name change.

## Self-contained modes

`--train` was a feature-gated stub, so the release binary printed "training mode not available" and exited 1. `dict_builder` is now a default feature of `zstdx-cli`, and the trained dict is byte-identical to ZDICT's, as the harness FILES check confirms.

Small sample counts now match dibio.c, which prints `Error 14 : nb of samples too low` and exits 14 when there are fewer than 5 samples. The rule and dibio's no-prefix EXM_THROW shape are aligned.

A bench run with missing files now matches benchzstd.c's bare `Error loading files`, with no prefix, no name and no trailing newline, and exits 15. A `BenchFailure::Load` variant carries it, and stat, open and read failures in `gather_files` and `load_corpus` all map to it.

## Pass-through and frame loop

Closeout pass-through landed on 2026-10-23. `--pass-through` and `--no-pass-through` exist, plus fileio's default, which arms when `-f` is given and the destination is stdout. The decode path is now fileio's frame loop: peek the next magic, discard skippable frames, and decode one data frame per `single_frame` decoder chained over the peeked bytes. Every engagement point matches: bad magic, 1 to 3-byte tails, and trailing garbage after decoded frames copy through and exit 0. The empty source and non-frame input without pass-through still fail with the same lines. A 20-case differential against the system zstd 1.5.7 confirmed this, with stdout, normalized stderr and exit codes all equal.

The frame loop surfaced a real stream-API bug. `read::Decoder::single_frame` consulted its flag only after `init_next_frame` had already parsed the next frame's header, so a drained single-frame read left the source 6 or more bytes past the frame boundary and the staged bytes died with the decoder. A multi-frame stream decoded its second frame's header into the void, and the loop then read garbage as magic. The fix is in `stream/read.rs`, and a crate test pins the source position at the next frame's first byte.

Skippable-only streams now exit 0 with empty output like zstd. The old whole-stream decoder errored with `unknown header`. A skippable frame truncated mid-content maps to zstd's `Read error (39) : premature end`.

Foreign-format refusals were added for the gzip, xz, lzma and lz4 magics. They refuse with fileio's per-format lines before the pass-through arm, for example `gzip file cannot be uncompressed (zstd compiled without HAVE_ZLIB) -- ignored`, which is true of this build. The lines match the system zstd byte for byte.

Train-basic stderr also landed. DiB_train's level-2 small-corpus pair reports `!  Warning : data size of samples too small...` with the total sample bytes and a 128 KiB per-file cap below 8 times maxdict. `COVER_warnOnSmallCorpus`'s level-1 line reports the trainer's dmer count through `TrainOutcome.dmers`, which is fastCover's `ctx.nbDmers`, and is byte-identical to zstd's on a shared corpus at 15545 dmers on both sides. `-q` keeps the level-1 line, and `-qq` silences both. The `Loading ...`, `Trying N different sets of parameters` and `k=50` progress block stays accepted as a renderer-class difference.

The encoder ratio gap was attributed and is not a defect. A level-3 size sweep over json, text and skewed at 64K to 32M, plus plain source text at 316K and 1M, reads -0.44 to +0.21% around parity. The CLI review's +0.045% text cell sits inside the dfast row's recorded near-tie band, together with json 32 MiB ST at +0.18% and the 09-12 ladder's dfast rows at +0.2 to +0.3%. It is not a new regression, and no probe was opened.

## Accepted deviations

- `-l` with `-v` or `-vv` would print zstd's banner `*** Zstandard CLI (64-bit) v1.5.7, by Yann Collet ***`, and at `-vv` the line `Compressing with N worker threads` with a machine-dependent N. Printing zstd's banner from zstdx would claim false provenance, and the thread line depends on the MT configuration.
- `--progress` at `-q` forces zstd's summary line and its `Read: ... ==> N%` progress drawing with raw carriage-return animations. Our progress runs through indicatif with different bytes, and matching zstd's renderer is not worth it.
- The bench progress redraw differs in one spinner glyph, `|` against `/`, and in number-width spacing. The difference is timing-dependent even between two zstd runs.
- After the Incorrect-parameter line, zstd prints its full usage block at displayLevel of 2 or more. Our usage text is clap-generated, so it differs by construction.

The final differential before the closeout landed at cases1 57/57 and cases2 50/58, with 8 residual divergences recorded under accepted deviations above. `b-decode` passed on the rerun, which confirms the spinner glyph difference is timing-flaky.

## Tests

`cargo test -p zstdx-cli` is green with 97 tests, 43 unit and 54 integration. The train suite is now active in the default build, where the feature gate previously left it at zero. Pins were updated for the long9, long28 and stream-size exit codes and messages, the `--split` rejection, the partial-magic `unknown header`, the bench smoke `Exit::Success`, and train's dibio-shaped report. `cargo test -p zstdx` in debug is green. The closeout adds a pass-through unit test for copy-through, frame plus garbage, skippable positions, a truncated skippable frame and the foreign-format lines, and a small-corpus warning test for both lines and the `-q` and `-qq` layering. Decode walls are unchanged in a CLI A/B of old against new on json, text and skewed at zst3 and zst19, within 1 ms. The `parity --cases 4000` differential stays clean.

## Commits

- f0ddd5b0 cli: align flag semantics and stderr formats with zstd v1.5.7 (rm/keep, ignore-errors, filelist, NEXT_FIELD, stderr shapes, tzst, collect_input Stat/Raw)
- 75bbb083 cli: zstd exit codes, stdin diagnostics, --train default, unknown-flag and --split rejection
- 1e3bcb2 stream: single_frame stops at the frame boundary
- 1bec87f cli: pass-through mode, frame-loop decode, foreign-format refusals
- b9a1f1a cli,dict: --train small-corpus warnings at zstd parity
