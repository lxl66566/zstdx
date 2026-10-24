# zstdx-fuzz

cargo-fuzz targets over the zstdx codecs. They use cargo-fuzz 0.13 on nightly, and every command below runs from the repository root.

This page answers which fuzz targets exist, how to run a bounded smoke test, how to keep the seed corpus small, and how to run a long campaign over large payloads.

## Targets

| target | surface |
|---|---|
| `encode` | bulk core, multi-payload level ladder plus option bits for checksum, pledged size, window and LDM, and pooled carryover. Checks against our decoder and the reference. |
| `encode_stream` | streaming cores on the same `[level, opts, k, chunks]` layout |
| `decode`, `decode_dict`, `fse`, `huff0`, `interop`, `ldm_mt_parity` | decode paths, entropy cores, cross-implementation interop |

The encode family shares the layout `[level:u8, opts:u8, k:u8, rest...]`. Chunk *i* compresses at level `(level+i) % 23` through one thread, which is the pooled thread-local carryover dimension. The exact walk is duplicated in `crates/zstdx/src/tests/fuzz_regressions.rs`, the artifact walker, so keep the two in lockstep when editing either.

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

The first pass ran on 2026-10-24 as two 25-minute runs over encode and encode_stream. It found zero crashes over more than 50k units. One limit remains: evolved inputs carry no beyond-window redundancy. Pre-campaign seeds are all 4096 B or smaller, and coverage-guided mutation rarely builds multi-KiB-period structure, so a dict-seeded corpus remains the lever for cross-window shapes. Re-merge the corpus after a campaign. This one grew encode from 4807 to 7103 seeds and encode_stream from 1667 to 5250 seeds.
