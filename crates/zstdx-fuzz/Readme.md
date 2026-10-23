# zstdx-fuzz

cargo-fuzz targets over the zstdx codecs. cargo-fuzz 0.13, nightly; run
everything from the repo root.

## Targets

| target | surface |
|---|---|
| `encode` | bulk core, multi-payload level ladder + option bits (checksum/pledged/window/LDM), pooled carryover; own decoder + reference both |
| `encode_stream` | streaming cores on the same `[level, opts, k, chunks]` layout |
| `decode`, `decode_dict`, `fse`, `huff0`, `interop`, `ldm_mt_parity` | decode paths, entropy cores, cross-impl interop |

Shared layout (encode family): `[level:u8, opts:u8, k:u8, rest...]`; chunk *i*
compresses at `level (level+i) % 23` through one thread — the pooled
thread-local carryover dimension. The exact walk is duplicated in
`crates/zstdx/src/tests/fuzz_regressions.rs` (artifact walker); keep the two
in lockstep when editing.

## Smoke: full CPU, bounded wall

```bash
crates/zstdx-fuzz/smoke.sh encode 60            # full corpus replay + 60 s fuzz
QUICK=1 crates/zstdx-fuzz/smoke.sh encode 60    # artifacts + 1/16 slice, seconds to INITED
```

Three properties the script encodes that a bare `cargo fuzz run` gets wrong:

- **Parallelism**: libFuzzer is single-process by default. `-jobs=W
  -workers=W` runs W sessions for the same `-max_total_time` — W× the
  mutations per wall second. Workers cap at 16 (ASAN ~200-300 MB RSS each
  on a 13 GB box).
- **Seed-replay time is invisible to `-max_total_time`**: the corpus load
  runs before the clock starts, and every `-jobs` child replays the whole
  corpus itself — a fat corpus makes a "2-minute" smoke take 5. Keep
  corpora merged (below); `QUICK=1` rotates a 1/16 slice per run for the
  iteration loop.
- **No cargo-fuzz corpus auto-append**: `cargo fuzz run` silently appends
  `corpus/<target>` to the libFuzzer args — fine for campaigns, wrong for
  merges (every `-merge=1` then re-processes the full corpus, chunked
  parallel merges collapse to N full merges) and for targeted smokes. The
  script builds with cargo-fuzz, then drives the binary directly.

Avoid running smokes while a `matrix`/`ratio`/wall A/B bench is active —
the workers eat every core.

## Corpus hygiene

Corpora live in `corpus/<target>/` (gitignored). A long campaign leaves
thousands of coverage-redundant seeds; every later smoke pays to replay
them. After a long campaign, merge: hardlink-split the corpus on the SAME
device (a tmpfs `/tmp` cannot hardlink to the SSD corpus), merge chunks in
parallel with the built binary, then merge the minimized sets:

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

Coverage-equivalent by construction (merge only drops seeds that add no
features). Reference shrinks: encode 11653→4807, encode_stream 2518→1667
(2026-10-24).

## Artifacts

Crash inputs land under `artifacts/<target>/` (gitignored; `smoke.sh`
moves anything it finds there). Track real reproducers with `git add -f`
(precedent: the decode and encode artifacts committed with their fixes)
and pin them with the walker test in
`crates/zstdx/src/tests/fuzz_regressions.rs` — pooled-state crashes need
an in-process sequence; a fresh-process artifact replay cannot re-poison
the pool.

## Campaigns (long wall-time, big payloads)

`smoke.sh` bounds wall time but keeps the libFuzzer default `max_len` (4096).
The todo-23b shape — payloads past one 128 KiB block, the window-adoption
edges — needs an explicit campaign:

```bash
# per target: 8 workers x <secs>, seeds from the real corpus (auto-append
# grows it), -len_control low so inputs reach max_len early. Takes the
# bench flock itself (workers eat every core).
<bin> -jobs=8 -workers=8 -max_total_time=1500 -max_len=300000 \
  -len_control=20 -rss_limit_mb=2560 -artifact_prefix=<artifacts>/ <corpus>
```

First pass (2026-10-24, 2 x 25 min over encode + encode_stream): zero
findings, 50k+ units. Honest limit: evolved inputs carry no beyond-window
redundancy (pre-campaign seeds are all <=4096 B; coverage-guided mutation
rarely builds multi-KiB-period structure), so a dict-seeded corpus remains
the lever for cross-window shapes. After a campaign, re-merge the corpus
(see above) — this one grew encode 4807->7103, encode_stream 1667->5250.
