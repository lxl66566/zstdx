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
