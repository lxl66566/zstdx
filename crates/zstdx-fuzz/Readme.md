# zstdx-fuzz

cargo-fuzz targets over the zstdx codecs. They use cargo-fuzz 0.13 on nightly, and every command below runs from the repository root.

This page answers which fuzz targets exist, how to run a bounded smoke test, how to keep the seed corpus small, and how to run a long campaign over large payloads.

## Targets

| target | surface |
|---|---|
| `encode` | bulk core, multi-payload level ladder plus option bits for checksum, pledged size, window and LDM, and pooled carryover. Checks against our decoder and the reference. |
| `encode_stream` | streaming cores on the same `[level, opts, k, chunks]` layout |
| `decode`, `decode_dict`, `fse`, `huff0`, `interop`, `ldm_mt_parity` | decode paths, entropy cores, cross-implementation interop |
| `decode_mt` | MT decode: parallel entry points (segment executor; piece executor via the router override, forced on in the dedicated process) against sequential decode parity, with committed seeds for the job-grid, piece, fallback and restart-poor libzstd-9 epoch classes (the -9 frame parks once per process; the repeated decode probes the staging pool's residue reuse) and, on 1/4 of inputs, a concatenated-frame class (count, sizes, content, checksum, level and encoder workers from the input bytes; a noise frame anchors sub-floor streams above the parallel floor; the decode worker count rides spare bits at 2/4/8/12, so the frame-lane executor runs grouped and per-frame) |
| `dict_pooled` | pooled dictionary encode identity across the raw, parsed, compat re-set and reused-`FrameCompressor` attach surfaces |

The encode family shares the layout `[level:u8, opts:u8, k:u8, rest...]`. Chunk *i* compresses at level `(level+i) % 23` through one thread, which is the pooled thread-local carryover dimension. `k` with bit 7 clear counts length-prefixed chunks ahead of a final remainder chunk; bit 7 set switches `rest` to a structure-mode program (next section). `encode_stream` derives its stream-only drive fields from the same bytes: the worker count from the level byte's spare high bits (`1 + level / 23`, the single-threaded core through mt 12) and the read core's feed entry from `rest`'s first byte and `ph` bits 5..6 — the plain slice, the NT-store `NtSource` or the zero-copy `OwnedEncoder` — so the campaign reaches the mt stream core, its stepped finish drain and both feed entries. The exact walk is duplicated in `crates/zstdx/src/tests/fuzz_regressions.rs`, the artifact walker, which replays the shared layout through the bulk core only, so the stream drive fields stay fuzz-target-specific; keep the two in lockstep when editing either.

## Structure mode

The documented limit of raw mutation is that coverage-guided edits never build multi-KiB-period structure: a 7-byte program event can demand a copy at any distance up to 16 MiB, so inputs stay small while the encoder sees repeats past 128 KiB block boundaries, past a declared window, and into dictionary content. `max_len` bounds the program, not the payload.

With `k` bit 7 set, `rest` is `[ph:u8, events...]`. `ph` bit 0 attaches a synthetic raw dictionary of `1 << (4 + bits 1..4)` bytes, and `encode_stream` reads `ph` bit 6 to pick its core. Every event tag is `op << 6`:

| op | event | operands |
|---|---|---|
| 0 | LIT — verbatim run | `n = (tag & 0x3f) + 1` bytes follow |
| 1 | REP — copy from history | `dist = u24 + 1`, `len = u16 + 1`, `flips = u8 & 0x0f` |
| 2 | NOISE | `n = u24 + 1` pseudo-random bytes |
| 3 | PAD — constant run | `n = u24 + 1` copies of the next byte |

REP copies from the virtual history `dict ++ output`, so a distance past the output echoes dictionary content (the prefill match class) and a distance shorter than the length overlaps like a repcode; `flips` replaces that many pseudo-random bytes inside the copied span, the mutated-copy class. Truncated operands read as zero, every length clamps to the program end and the 256 KiB expansion cap, and an unreachable distance clamps to the virtual history, so garbage programs degrade to fewer events and never panic. Expansion is pure (one xorshift stream seeded over the program bytes), so the walker mirrors it byte for byte: `test_structured_programs` pins the grammar, the far-match property and the dictionary echo, and the artifact pass replays program-mode artifacts.

Both encode targets additionally run committed seed constructors on every input (`cross_window_seed` at small scale, `structured_seed` at block-crossing scale through the program grammar), so the classes run regardless of corpus state.

## Smoke runs

```bash
crates/zstdx-fuzz/smoke.sh encode 60            # full corpus replay + 60 s fuzz
QUICK=1 crates/zstdx-fuzz/smoke.sh encode 60    # artifacts + 1/16 slice, seconds to INITED
```

The script encodes three properties that a bare `cargo fuzz run` gets wrong:

- **Parallelism**: libFuzzer is single-process by default. `-jobs=W -workers=W` runs W sessions for the same `-max_total_time`, which gives W times the mutations per wall second. Workers cap at 16, because ASAN uses about 200 to 300 MB RSS each on a 13 GB box.
- **Seed-replay time is invisible to `-max_total_time`**: the corpus load runs before the clock starts, and every `-jobs` child replays the whole corpus itself. A fat corpus turns a two-minute smoke into five minutes. Keep corpora merged, as described below, and let `QUICK=1` rotate a 1/16 slice per run during the iteration loop.
- **No cargo-fuzz corpus auto-append**: `cargo fuzz run` silently appends `corpus/<target>` to the libFuzzer arguments. That suits campaigns and breaks merges, because every `-merge=1` then re-processes the full corpus and chunked parallel merges collapse to N full merges. It also breaks targeted smokes. The script builds with cargo-fuzz, then drives the binary directly.

Run smokes only while no `matrix`, `ratio` or wall A/B bench is active, because the workers take every core.

## Corpus hygiene

Corpora live in `corpus/<target>/` and are gitignored. A long campaign leaves thousands of coverage-redundant seeds, and every later smoke pays to replay them. After a long campaign, merge the corpus. Hardlink-split it on the same device, because a tmpfs `/tmp` cannot hardlink to the SSD corpus. Merge the chunks in parallel with the built binary, then merge the minimized sets:

```bash
BIN=crates/zstdx-fuzz/target/x86_64-unknown-linux-gnu/release/encode
C=crates/zstdx-fuzz/corpus/encode; T=crates/zstdx-fuzz/corpus/.merge-tmp
mkdir -p $T && ls $C | split -n l/16 - $T/list.
i=0; for f in $T/list.*; do mkdir -p $T/c$i $T/o$i
  while IFS= read -r n; do ln "$C/$n" $T/c$i/; done < "$f"; i=$((i+1)); done
for i in $(seq 0 15); do ("$BIN" -merge=1 -rss_limit_mb=4096 $T/o$i $T/c$i \
  > $T/m$i.log 2>&1) & done; wait
mkdir -p $T/final
"$BIN" -merge=1 -rss_limit_mb=8192 $T/final $T/o*
rm -r $C && cp -r $T/final $C && rm -r $T
```

The result is coverage-equivalent by construction, because a merge only drops seeds that add no features. Reference shrinks from 2026-10-24: encode 11653 to 4807 seeds, encode_stream 2518 to 1667 seeds.

## Artifacts

Crash inputs land under `artifacts/<target>/`, which is gitignored, and `smoke.sh` moves anything it finds there. Track real reproducers with `git add -f`, as the decode and encode artifacts committed with their fixes did, and pin them with the walker test in `crates/zstdx/src/tests/fuzz_regressions.rs`. Pooled-state crashes need an in-process sequence, because a fresh-process artifact replay cannot poison the pool again.

## Campaigns

`smoke.sh` bounds wall time and keeps the libFuzzer default `max_len` of 4096. The todo-23b shape needs an explicit campaign. It covers payloads past one 128 KiB block and the window-adoption edges:

```bash
# per target: 8 workers x <secs>, seeds from the real corpus (auto-append
# grows it), -len_control low so inputs reach max_len early. Takes the
# bench flock itself (workers eat every core).
<bin> -jobs=8 -workers=8 -max_total_time=1500 -max_len=300000 \
  -len_control=20 -rss_limit_mb=2560 -artifact_prefix=<artifacts>/ <corpus>
```

The first pass ran on 2026-10-24 as two 25-minute runs over encode and encode_stream. It found zero crashes over more than 50k units. One limit remained: evolved inputs carried no beyond-window redundancy, so the R73 pass added committed seed constructors, and R74 added structure mode. In program mode the payload does not grow with the input, so a structure campaign takes a modest `-max_len` (65536 is plenty: a useful program is a few dozen bytes, and `-len_control=20` still pushes inputs toward the cap where multi-event programs live) while every expansion still reaches the 256 KiB cap.

Re-merge the corpus after a campaign. Reference growth from the 2026-10-24 first pass: encode 4807 to 7103 seeds, encode_stream 1667 to 5250.

The R99 pass (2026-11-05) drove the frame-lane decode machinery for the first time: `decode_mt`'s committed concatenated-frame class runs 2..=16 frames with sizes from empty to 2 MiB, per-frame checksum, level and encoder-worker mixes, skippable interleave and a noise anchor above the 512-KiB parallel floor, decoding through 2/4/8/12 workers (grouped and per-frame lanes) with an engagement-counter pin per qualifying input. The campaign ran `smoke.sh` at 1200 s x 8 ASAN workers over `decode_mt`, `decode` and `decode_dict` and found zero crashes: 17,189 units on `decode_mt` (constructor-bound, ~1.8 exec/s per worker), about 5.85M on `decode` (~608 exec/s per worker) and the `decode_dict` corpus replayed and fuzzed clean. A smoke seeds a scratch slice and discards new units, so the shared corpora stayed at their pre-campaign counts.
