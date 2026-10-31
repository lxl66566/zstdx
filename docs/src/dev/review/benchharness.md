# Bench harness review

This page records the 2026-10-31 review of `crates/zstdx-bench`, the measurement harness every published number and gate runs through. The scope is the full read of `common.rs`, `corpus.rs`, `main.rs` and the `cmd` modules `matrix`, `small`, `files`, `ratio`, `dump`, `prof`, `emitframe`, `mtcheck`, `corrupt`, `prefill`, `dictmt` and `train`, plus a structural pass over the feature-gated analysis tools `parity`, `seqstats`, `piecepipe` and `jobdecomp` (their designs already live in [diffbench](diffbench.md) and the job-decomposition notes). Each suspicion was re-verified against the code or by running the tool before it was recorded. The method interleaved with the R81 measurement refresh, so several observations come from live cells rather than constructed cases.

## Landed fixes

### Median and MAD average even sample counts

`Stats::from_samples` picked `samples[len/2]` as the median, which is the upper-middle sample whenever the round count is even. The budget rule appends rounds one at a time, so any cell whose budget lands between rounds stops on an even count, and both its median and its MAD (same pattern over the deviations) read one half-sample high or low depending on the draw. Both statistics now take the mean of the two middle values for even counts through a shared `mid_statistic` helper; odd counts are unchanged, which keeps every n=3 cell bit-identical. `median_averages_even_samples` pins the even and odd forms.

### Level flags reject out-of-range numbers

`emitframe --level` and `dictmt --level` passed numbers straight into `Level::from_zstd`, which silently clamps below 1 and above 22. `emitframe` then echoes the requested string in its summary line, so `--level 30` printed "level 30" while encoding at 22. Both parsers now assert the 0-22 range with a message naming the bounds; 0 stays accepted for `emitframe`, where the store-only frame is a meaningful payload. `parse_level_rejects_out_of_range` pins the boundary. The `matrix`/`small`/`ratio` parsers already rejected out-of-range numbers through `LevelSel`, and level 0 stays excluded there deliberately: our level 0 is the store-only frame while libzstd's 0 selects its default level 3, so the pairing has no honest A/B form.

### `prefill` says so when the shape selection is empty

The prefill loop iterates only random, text and json, while `--shape` accepts every corpus shape; `--shape skewed` selected nothing and exited silently. The tool now prints the supported set and returns instead.

## Observations

- The dec-mt section's first-file single-thread solo cell is page-state sensitive. The R81 refresh's opening invocation read `json.zst3.st1` at 1014 MiB/s with every MT cell at its usual level, and the immediate rerun read 1384 against the prior table's 1409. The mt16-over-ST factor inherits that swing (1.59 warm, 2.22 cold), while the zref-anchored factor stayed at 1.17 in both. The zstd-stream reference column is the robust anchor for that table, and a first-run ST reading that disagrees with the bulk decoder's own dec-st cell by more than the noise band warrants one rerun before it is tabulated.
- The interleaved budget rule `rounds >= 3 && a_total >= min_secs && b_total >= min_secs` makes the fast side's budget dominate when the sides are grossly mismatched: random levels 17-19 accumulate 120 to 165 rounds because our side needs that many 4 ms rounds to reach 500 ms while the libzstd side spends 3 to 4 s per round, which is the harness's per-cell worst case at roughly 8 minutes. This is the documented "random at 36 or more" behavior at its extreme and needs no fix, but anyone planning a ladder rerun should budget for it.
- `corpus::load` derives the raw counterpart by splitting on the first dot, so a dotted stem like `dll100.zst3` would resolve to the wrong raw file if it ever lived in the corpus directory. Only the fixed corpus names reach `load`, and the `--file` path resolves through `raw_counterpart`, which splits at the last dot and tries both the bare stem and `.raw`. Unreachable as written.
- `corrupt` flips bytes with `1 + rng % 255`, so every flip changes the byte, and it catches panics per round while leaving the default panic hook printing to stderr. Noisy on a genuine failure, harmless otherwise, and the assert on zero panics is the gate.
- `dump` spawns the system `zstd` CLI once per emitted file as the interop gate, 30 spawns at `--all-levels`, and `mtcheck` spawns it per file per worker count. Both fail loudly through `expect` when the CLI is absent, which is the intended dev-machine assumption.
- `files`' geometric retry doubles a reserve on `TargetTooSmall` and re-decodes from scratch each step, worst case seven extra passes on a 100 MB payload before the buffer fits; bounded and cold-path only.
- `ratio`'s geo-mean averages `ln(1 + delta)` so byte-exact ties (random) contribute zero rather than exploding, matching the documented scale-free form.

## Skipped with cause

The `parity` subcommand's craft-and-mutate machinery was structurally skimmed only: its design, adjudication rules and known asymmetry live in [diffbench](diffbench.md) and parity.md finding 7, and re-reviewing it here without new failing cases would duplicate that record. `seqstats`, `piecepipe` and `jobdecomp` sit behind the `seq_dump`/`job_trace` features, are compiled out of every timing build, and their exec-log assertions (per-record offset sanity and per-match byte verification in `piecepipe`) were read and found sound.


## Harness speedup (2026-11-01)

The 2026-10-30/31 pass family cost about three hours of wall per full run, which priced the ladder and the dump gates out of daily use. This section records where that time went, which harness changes landed, and which levers failed their calibration and stay out. The interleaved A/B, the warmup discard and the median statistics keep their meaning; every landed change trades measured, bounded variance for wall time.

### Attribution

Each timed cell cost gates, one round pair, plus warmup, one round pair, plus `n` timed pairs with `n = max(3, ceil(budget per side))`. Applying that model to the 2026-10-31 tables, anchored by the two measured walls of `ratio` at 123 to 141 s and the stream dump at 175 to 280 s, puts the old full pass at roughly 170 minutes of section walls: the numeric ladder about 74 minutes, enc-st tier mode with dll100 about 13, enc-stream with the dll100 tail about 11, the two dumps together 5 to 8, ratio about 2, and the decode, mt and small sections about 3 combined. The ladder's random tail alone was about 32 minutes, because the budget rule paired our 4 ms rounds against libzstd's 3 to 4.6 s rounds and the fast side's own 500 ms budget accumulated 139 pairs while the slow side already had its three.

### Landed changes

The asymmetric-cell cap stops a cell once one side spent the full budget and the other an eighth of it, which bounds the random tail at three to eighteen pairs per cell. Encode cells gate on their own last timed round instead of a separate gate encoding, one round pair cheaper per cell. enc-st and the enc-stream ST cells run on a bounded cell pool, default width 2, each cell keeping its interleaved A/B on one pool thread. dump frames emit on the same pool at width 6, where the bytes carry no timing and a 120-frame stream grid read 90 s against 175 s serial with every frame byte-identical by `diff -r`. `bench/pass.sh` drives the whole pass against one build under the shared lock with per-section walls.

### Calibration verdicts

Width calibration on enc-st json, serial against parallel: width 2 held the calibrated cells within 5 percent of serial x, with balanced at 0.762 against 0.768 and ultra at 1.015 against 1.033, and cut the section from 119 s to 80 s. Width 3 read json.ultra at x1.648 with MAD 0.570 and width 6 inflated balanced by 9 percent, so the timing default is 2 and wider widths are smoke-only. The pass then showed the caveat the json calibration could not: cells in the 10 GB/s class distort further. text.fastest read x0.901 in the pass and x0.797 on an immediate serial rerun, against the 0.797 to 0.83 recorded band, so the pairing inflated it by 13 percent. Cells of that class keep the existing cycles-over-wall doctrine, and a verdict on them needs a `--parallel 1` rerun. dec-st stays serial: decode cells are bandwidth-bound, the bulk pair is asymmetric because our slice path streams memory while libzstd's wrapper is CPU-bound, and text.zst3.bulk read 7 percent x-inflated at width 3 while its streaming cell stayed clean.

Two levers failed calibration outright. Counting the first round pair as samples on slow cells, tried to save the warmup pair, biased the deep tiers upward: json best read x1.345 against 1.237 with the discard restored and opt 1.087 against 1.020, because the encoders' table allocations fault again during the second round, so the unconditional warmup discard returned and the attempt is recorded here. Parallelizing dec-st and raising ratio's `--parallel` default above 8 were rejected without code: the first by the decode calibration above, the second because the machine carries about 6 GiB free with 5.3 GiB of swap in use while each cell transiently holds a few 32 MiB-scale buffers, and the section is already 123 s where the estimate says width 12 buys about 40 s.

### Pass budget

The 2026-11-01 fast pass (`bench/pass.sh all`, on the bench code of commit 3e5db479) measured the sections below. The before column is the attribution model anchored as above; the after column is the pass summary.

| section | before | after |
|---|---|---|
| dec-st | ~40 s | 30.8 s |
| dec-mt + files-dll | ~35 s | 16.7 s |
| enc-st tier + dll100 | ~13 min | 7.2 min |
| enc-mt8 + enc-mt16 | ~2 min | 0.4 min |
| enc-stream + dll100 | ~11 min | 7.6 min |
| small | ~35 s | 22.7 s |
| ratio | 123-141 s | 122.2 s |
| full ladder json | ~17 min | 6.9 min |
| full ladder text | ~0.5 min | 0.2 min |
| full ladder skewed | ~21 min | 9.6 min |
| full ladder random | ~33 min | 1.7 min |
| full ladder zeros | ~0.4 min | 0.6 min |
| total timing sections | ~170 min | 37.5 min |
| dump bulk / stream | ~2-3 min / 175-280 s | ~30 s / 90 s |

Spot cells against the 2026-10-31 tables, fast pass against recorded: dec-st text.zst3.bulk 0.279 against 0.28 and text.zst3.stream 1.090 against 1.07, enc-st json.balanced 0.770 against 0.773 with best 1.254 against 1.264 and opt 1.008 against 1.032, skewed.balanced 0.042 against 0.041, enc-mt json.fast.mt8 0.397 to 0.430 against 0.42 and text.balanced.mt8 0.863 against 0.85, enc-stream json.fastest 1.253 against 1.23 with text.ultra 0.641 against 0.65 and dll100.balanced 1.278 against 1.27, small text.64K level 9 at 2.834 against 2.80, ladder json l13 1.315 against 1.28 with l17 1.041 against 1.04 and l22 0.527 against 0.51, and the ratio geo-mean reproduced +8.70 percent byte-for-byte over the same 120 cells. The n=3 deep cells wander inside their documented band, with json.ultra at 1.108 against 1.019 and json.l19 at 0.863 against 1.02 as the widest reads. The asymmetric-cap cells reproduce their verdicts at a fraction of the rounds: random l17 to l22 read x0.002 to 0.004 against the recorded 0.001 to 0.005 at n of 5 to 8, where the old rule accumulated 35 to 139 pairs on the same cells.
