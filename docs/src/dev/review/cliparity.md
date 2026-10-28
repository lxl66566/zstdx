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

## Flag surface audit (R75)

The audit compared every flag zstd 1.5.7 accepts against the zstdx parser. The reference is the system zstd 1.5.7 binary (probed flag by flag for acceptance, diagnostics and exit codes) plus `zstdcli.c`, `fileio.c` and `util.c` for the semantics behind each acceptance. The source checkout at `/root/programs/fork/zstd` is 1.6.0-dev, so every flag it gained after 1.5.7 (`--patch-apply`, `--jobsize`, `--split`, the `--priority=` forms beyond `rt`) stays in the unknown-flag path here, matching 1.5.7's own `Incorrect parameter` answer.

### Implemented this round

| Flag | Behavior |
|---|---|
| `-C` | Short form of `--check`. |
| `--no-dictID` | Omit the header Dictionary_ID through `EncoderOptions::include_dictid(false)`. The dictionary stays fully in effect. Inert in decode modes, like the reference. |
| `--[no-]content-size` | `--no-content-size` keeps the size out of the header and moves it into the input-shape hint, so the row still sizes to the source. A suppressed `--stream-size` pledge is validated in the CLI by a read counter, keeping the error-11 line. |
| `--exclude-compressed` | fileio's per-input skip against the verbatim `compressedFileExtensions` table (98 suffixes), compression only, silent, exit 0. |
| `--memlimit`, `--memlimit-decompress` | Aliases of `--memory`. |
| `--output-dir-flat=DIR` | Derived output names relocate under DIR. The directory must exist, and a missing one fails with `FIO_openDstFile`'s `zstd: <dst>: <strerror>` line, which now also covers every other destination-open failure. |
| `--auto-threads={physical,logical}` | Selects the `-T0` basis. Physical is the default and counts distinct `/proc/cpuinfo` (physical id, core id) pairs like `UTIL_countPhysicalCores`, announced at `-v` with the `Note: N physical core(s) detected` line. |
| `-n`, `--[no-]asyncio`, `--[no-]mmap-dict`, `--format=zstd` | Identity flags: the reference accepts them and this build's observable behavior is identical by construction, so they parse and change nothing. `--format` rejects the other values as `Incorrect parameter`, which is exactly what a zstd built without zlib/lzma/lz4 does. |
| `-V`, `-h`, `-H`, `--help` | Answered during an argv scan before clap, at the verbosity seen so far: `-q -V` prints the bare version, `-V -v` adds `*** supports: zstd`. The text is zstdx's own (see deviations). |
| repeated value flags | zstdcli.c overwrites each value variable, so the last occurrence wins. `normalize_args` now drops earlier occurrences together with their separated value, covering `-D a -D b`, `-T4 -T8`, `-o a -o b` and every other repeatable knob. |
| empty expansion | Explicit inputs that expand to nothing (empty directories under `-r`) decline with `please provide correct input file(s) or non-empty directories -- ignored ` at display level 1 and exit 0, like zstdcli.c's post-expansion check. |
| `-D` with `--patch-from` | zstdcli.c's own `error : can't use -D and --patch-from=# at the same time ` line, exit 1. |

### Rejected loudly, engine knob needed

Each parses and then rejects with `zstdx: <flag> is not supported: <reason>`, exit 1. The reasons live in `unsupported_flag`.

- `--rsyncable` - no rsync-friendly block splitting. The reference itself errors with `Rsyncable mode is not compatible with single thread mode` only when `--single-thread` is set; our engine has no rsyncable mode at any thread count, so the rejection is unconditional and the divergence is deliberate.
- `--[no-]compress-literals` - literal entropy mode is per block.
- `--[no-]row-match-finder` - matchfinder selection is per level row.
- `--target-compressed-block-size=#` - no block-size target exists.
- `--zstd=PARAMS`, `--max` - raw compression-parameter forcing is not exposed.
- `--trace=FILE`, `--trace-file-stat` - no engine trace hooks.
- `--show-default-cparams` - the level table is not exposed for printing.
- `--priority=rt` - no scheduler control; in 1.5.7 the flag only affects bench mode anyway.
- `-s#` (dict selectivity), `-S` (bench separates files), `-p` (pause), `-P#` (synthetic corpus compressibility) - trainer and bench knobs that do not exist here.
- `--fake-{stdin,stdout,stderr}-is-console` - zstd's own console-detection test hooks.
- `--output-dir-mirror=DIR` - mirrored output trees need walk-relative path tracking; the only rejection that is fileio work rather than engine work.

### Still out, deliberate

- `--sparse`/`--no-sparse`, `--ignore-errors`, `--dictID=`, `--train-cover`, `--train-fastcover`, `--train-legacy` - unchanged from the earlier rounds.
- The 1.6.0-dev-only flags stay unknown arguments, which is byte-matching with 1.5.7.

### Audit deviations (accepted)

- `-V` prints zstdx's own banner and version instead of zstd's `*** Zstandard CLI (64-bit) v1.5.7 ... ` line: printing zstd's banner from zstdx would claim false provenance, the same reasoning as the `-l -v` banner deviation. `-h` and `-H` both print the single clap help text; zstd splits them into a short and a long usage, and reproducing zstd's texts would document flags this build does not have.
- The extra spellings zstd 1.5.7 lacks were removed: `--file` (filelist alias), `--dict` (long form of `-D`) and `--recursive` (long form of `-r`) now fall into `Incorrect parameter` like the reference. `--file` had been pinned as accepted, so the pin flipped.
- Reference parser corners that stay ours: a separated `-M 512K`, `-T 4`, `-e 5`, `-i 1` or `-B 1M` value is consumed by clap, while zstd's inline-only `readU32FromChar` reads 0 and treats the value as a file name. The audit kept the clap shapes because matching would mean inventing input errors nobody asks for. `--memlimit-decompress=1M` is unreachable in the reference (its `--memlimit` prefix match swallows the flag first and then errors on the next entry); here it is a plain alias.
- `-T` defaults to 1 where the reference defaults to an auto-sized MT worker pool (physical cores), because the default thread count changes the emitted frame bytes. `-T0` resolves the basis like the reference but the default stays single-threaded. Decode with `-T>1` also stays silent where zstd warns `decompression does not support multi-threading`, because MT decode is real here.
- A trailing-slash directory argument produces reference names like `t//f.txt` (its strcat walk); the Rust join collapses the double slash. The file sets are identical.

One pre-existing engine observation surfaced during the audit interop pass: at small source sizes with a dictionary attached, our frame header picks single-segment off while libzstd picks it on, so dict frames differ in header layout at those sizes (plain and `--no-content-size` frames are byte-identical on the same corpus). Both forms decode identically in both directions. Byte-pinning the dict-frame header is engine work under item 21a's dictionary surface, not a CLI defect.

## Accepted deviations

- `-l` with `-v` or `-vv` would print zstd's banner `*** Zstandard CLI (64-bit) v1.5.7, by Yann Collet ***`, and at `-vv` the line `Compressing with N worker threads` with a machine-dependent N. Printing zstd's banner from zstdx would claim false provenance, and the thread line depends on the MT configuration.
- `--progress` at `-q` forces zstd's summary line and its `Read: ... ==> N%` progress drawing with raw carriage-return animations. Our progress runs through indicatif with different bytes, and matching zstd's renderer is not worth it.
- The bench progress redraw differs in one spinner glyph, `|` against `/`, and in number-width spacing. The difference is timing-dependent even between two zstd runs.
- After the Incorrect-parameter line, zstd prints its full usage block at displayLevel of 2 or more. Our usage text is clap-generated, so it differs by construction.

The final differential before the closeout landed at cases1 57/57 and cases2 50/58, with 8 residual divergences recorded under accepted deviations above. `b-decode` passed on the rerun, which confirms the spinner glyph difference is timing-flaky.

## Tests

`cargo test -p zstdx-cli` is green with 116 tests, 48 unit and 68 integration. The train suite is now active in the default build, where the feature gate previously left it at zero. Pins were updated for the long9, long28 and stream-size exit codes and messages, the `--split` rejection, the partial-magic `unknown header`, the bench smoke `Exit::Success`, and train's dibio-shaped report. `cargo test -p zstdx` in debug is green. The closeout adds a pass-through unit test for copy-through, frame plus garbage, skippable positions, a truncated skippable frame and the foreign-format lines, and a small-corpus warning test for both lines and the `-q` and `-qq` layering. Decode walls are unchanged in a CLI A/B of old against new on json, text and skewed at zst3 and zst19, within 1 ms. The `parity --cases 4000` differential stays clean.

## Commits

- f0ddd5b0 cli: align flag semantics and stderr formats with zstd v1.5.7 (rm/keep, ignore-errors, filelist, NEXT_FIELD, stderr shapes, tzst, collect_input Stat/Raw)
- 75bbb083 cli: zstd exit codes, stdin diagnostics, --train default, unknown-flag and --split rejection
- 1e3bcb2 stream: single_frame stops at the frame boundary
- 1bec87f cli: pass-through mode, frame-loop decode, foreign-format refusals
- b9a1f1a cli,dict: --train small-corpus warnings at zstd parity
