# Benchmark tooling
## Instrumentation must be opt-in


The `seq_dump` decoder hook, a TLS `Vec::push` per sequence inside `decode_step`, was enabled unconditionally in zstdx-bench's zstdx dependency for the `seqstats` tool. Every measured decode and every ratio-sweep roundtrip gate silently ran with it, and json stream fell from 1733 to 1224 MiB/s, 29% slower and x1.26 to x1.79 against libzstd, while the zstd side was unchanged, so the A/B harness reported a phantom regression.

The interleaved A/B protects against clock drift, but it cannot protect against a one-sided build difference. Instrumenting features in the measured crate's hot loop are bench-crate opt-in features behind `--features seq_dump`, and after landing any instrumentation hook, re-run one timing cell at HEAD as a canary before trusting later numbers.
## Bench harness self-wounds


The MiB/s formula once forgot to multiply by rounds, so `raw.len()/elapsed` was used where `raw.len()*n/elapsed` belonged. A filter-argument substring mismatch silently runs an empty process, with utime near 0 as the signal.

The x factor's `%.3f` format is unreadable at extreme values, so look at the MiB/s column. zeros is a free noise-calibration cell. seqstats hardcodes `Level::Fastest`, which makes it useless for non-Fastest work, and a `--level` flag is the real fix.
## Parameter sweeps must print back


A regex replacement dropped one comma so a subpattern never matched, and one "unchanged" run yielded a "changed" conclusion masked by another configuration's data. A sed margin sweep `s/+A/+B/` silently no-ops when the pattern is absent, so an entire sweep step, and any test rerunning the same value, measures the unchanged file.

Verify with grep or Python asserts, as set_lvl.py does with its echoes and asserts. Two bogus identical-output conclusions were drawn before detection.
## Pin both sides of a ref-frame A/B


The seqstats "ours" parse always comes from the current build, so two runs in the 2026-09-18 A/B whose reference side was rebuilt from the same tree once proved a bit-identical parse against what was believed to be the base. Freeze one side as a frame file with `--ref-frame`, plus `--dict` and `--level`, and diff parses against that file while rebuilding only the moving side.

After a base build through `git checkout --`, re-verify that the patch is applied before the next new measurement, since a stale binary silently reverts the experiment.

A baseline worktree carries its own untracked `Cargo.lock`, and drift between it and the main repo's lock silently rebuilds dependency crates on one side only. The 2026-11-02 gungraun A/B read a 13% Ir shift on the libzstd reference side from a zstd 0.13.4 versus 0.13.3 lock drift before either side of the change under review was measured. Copy the main repo's lock into the baseline worktree and re-verify both sides' dependency versions whenever a worktree joins an Ir comparison.
## Long locked benches run detached


A follow-up command in the same shell bucket blocks until the locked bench finishes, and the queued command's client has long timed out, so its output is lost while the command still executes. A queued `rm` once deleted a finished matrix run's only output copy.

Launch long locked benches detached with `nohup flock ... &`, poll from a separate session, and never queue cleanup commands into a bucket that might be blocked.
## Tier-only gates hide off-tier rows


`ratio` and `dump --all-levels` walk levels 1, 3, 9, 13, 17 and 19, so every off-tier row at 16, 18, 20 to 22 and the numeric-only rows generally is invisible to both gates. Row-16 and row-22 knob A/Bs initially looked byte-identical because the grids never compress those levels.

Off-tier experiments must gate on `matrix --mode enc-st --full-ladder --level <n>` for sizes and ratio per cell, or on `emitframe` per level. A knob change that reports no cell moved under the standard gates has not been measured.

## Port fidelity against the reference binary


The `-i0` scheduler shape looked tested, because a zero budget still measures one round, yet `zstd -b1 -i0` against our binary showed 10 ms against 2.2 s. That exposed the missed `run_ms > total_ms` clamp in `BMK_resetTimedFnState`. Timing the two CLIs side by side on an edge flag is a cheap probe that no unit shape replaces.

Conversely, `--patch-from` under `-b` is silently ignored by the reference itself, because zstdcli.c aliases `dictFileName = patchFromDictFileName` after the bench dispatch and the `-D` conflict and multi-file checks sit after it too. A port that ignores it too is at parity, so verify the quirk against the binary before fixing it into a rejection or a dictionary bench.
