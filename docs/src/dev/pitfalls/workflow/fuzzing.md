# Fuzz campaign

## Fuzz oracles and reference APIs


The decode target's inherited `streamed == flat` assertion was false by construction, because the paths mirror different libzstd APIs, streaming generation model against one-shot whole history, and they diverge by design on wrapped-ring aliasing frames. Each path must be pinned against its own reference counterpart, never against the sibling.

Even the per-path byte compare is ill-defined on window-invalid aliased frames, because libzstd's accepted bytes then depend on the caller's buffer geometry. The zstd crate's `copy_decode` resolved a stale-ring tail `28 cb d5 fa` where the CLI and zstdx streaming both produce `00 00 00 00`, byte-identical over the whole 360 KB output, so a cross-implementation byte assert can fire on a defect-free decoder.

This happened only inside a long-lived worker process, because the reference's aliased read returns its own buffer's stale heap bytes. Fresh-process single runs by 20 and a full one-process corpus replay of the same artifact were all clean.

The adjudicator that keeps the oracle sound runs the streamed byte compare only when the reference's own streaming and one-shot outputs agree, which every valid frame does because validity is exactly layout-independence. Accept and reject agreement stays unconditional. The campaign report is `/root/scratch-r73-fuzz`.
## MT fuzzing and router floors


The parallel decoder engages only at 512 KiB or more of compressed input, or a pledged single frame in the 2 to 64 MiB piece regime. libFuzzer's default 4 KiB `max_len` therefore only fuzzes the sequential fallback behind the MT entry points.

Run with `-max_len` around 2 MiB and seed both regimes. Ramp frames from the CLI through `ZSTDX_MT_RAMP_BYTES=2M zstdx -T4`, whose MT headers carry the pledge, exercise the piece executor. Plain `-T4` frames above the floor exercise segment-parallel decode, and a sample of the decode corpus fills in. The piece-router override is safe to flip inside a fuzz target because it runs in a dedicated process, the same reason the piece tests live in their own binary. Since the R102 round the encode-family targets also generate the ramp class in-process (`ramp_seed` on half the inputs: one D of the 512 KiB to 2 MiB lattice over the gate-honoring scan rows, the armed frame at workers 4 and plain controls at workers 0 and 1, the override stored around the armed frame because `arm_ramp` refuses the tree rows), so a campaign reaches the gate paths and the pooled-state cycling without the env. The decode-side piece class stays on the Fastest row, and the constraint is structural: the piece validator's lag bar reads 3/8 of the true job size, and a deeper row's strip floors its jobs past the bar — a level-3 frame at the 2 MiB window runs one 2 MiB job, and D must reach 768 KiB before its crossings clear the bar at all. Every D of the lattice clears the bar at the 1 MiB job floor. The piece engagement counter pins each qualifying frame, so an encoder guarantee violation surfaces as a crash artifact instead of a silent serial fallback.

The strongest property is directional parity: MT must agree with sequential byte for byte and never accept what sequential rejects. Large inputs cut execution to about 30 per second per worker, so budget by coverage, with exec count as a secondary signal.
## Cross-surface fuzz empty cases


The first crash in the `dict_pooled` target triaged to a target bug. The engine behaved correctly. A derived empty raw dictionary parses fine through `EncoderOptions::dictionary` and gives a windowed dictionary frame, while compat `set_dictionary(_, &[])` means detach and gives a no-dictionary single-segment frame, so the cross-surface byte-identity assertion was vacuously wrong at the boundary.

Pin every surface's documented behavior on the degenerate inputs the fuzzer can derive: empty, 1-byte and all-same. The same campaign's engine find was that a reused pooled state only reproduces in-process with the exact call order and a warm pool, because fresh-process reruns of the artifact came back clean and libFuzzer carries state across execs from the empty init input. Keep the reproducer as a scripted call sequence as well as the artifact.
## The pooled-state fuzz targets


The `dict_pooled` target checks pooled dictionary encode identity across bulk raw and parsed attachment, compat re-set, and `FrameCompressor` displacement across interleaved frames. It asserts no panic, a dictionary roundtrip, second-call bytes equal to first-call bytes, and the cached path decoding as the fresh parse.

The `ldm_mt_parity` target checks LdmMode by workers decode parity over {Auto, Enable, Disable} by {0, 1, 4} by pledged and open by bulk and stream, with three decoders each: sequential, parallel and reference, as a window-declaration proxy. A campaign of 4 jobs of 241 s per target under the bench flock ran 41.5K execs for `dict_pooled`, with its corpus growing from 141 to 3677 units at about 48 exec/s/worker and 388 MiB RSS, and a clean `ldm_mt_parity` at 3290 units, with zero crashes after the empty-dictionary target fix.

The campaign's one engine bug was that `FrameCompressor` with a dictionary and no input shape folded the unknown length into a dictionary-sized span. That clamped the window to the dictionary while the source streamed arbitrary input past it. The blocks then exceeded `min(window, 128 KiB)`, both decoders rejected the frame, and the fuzzer minimized it to a 30-byte reproducer. It was fixed by the stream core's rule that an unpledged stream keeps the unknown length, gated by `unpledged_frame_compressor_dict_keeps_full_window`.
## Fuzz campaign mechanics


With nightly as the default toolchain, `cargo fuzz run --fuzz-dir crates/zstdx-fuzz -s address -O <target>` works directly. libFuzzer's `-jobs` and `-workers` mode writes `fuzz-N.log` into the current directory, so clean them after the run or launch from a scratch directory. `-max_total_time` is per job, so a 4 by 4 wave takes wall time close to one job's budget.

The campaign must hold the bench flock, four workers is plenty, and per-worker RSS is about 430 MiB with ASAN on decode targets, so set `-rss_limit_mb` below the free-RAM floor. Seed corpora from `crates/zstdx/decodecorpus_files` using every third `.zst`, from `dict_tests` using slices of the formatted dictionary at many offsets and widths for headers, truncated tails and table regions, and from the raw corpus for interop, where the target compresses seeds with the reference crate itself.

The current smokes run through `crates/zstdx-fuzz/smoke.sh`. It drives the built binary directly because `cargo fuzz run` silently appends `corpus/<target>` to the arguments, which collapses chunked parallel `-merge=1` into N full merges. It splits the seed replay over W single-shot `-runs=0` processes, because letting every `-jobs` child replay the whole corpus under contention drops per-worker execution 3x at 16 workers and `-max_total_time` never sees the replay at all. It then fuzzes W-wide from a rotating 1/W slice, holds the bench flock, and archives crashes to `artifacts/`.

The split loop created slice directories per file, so a corpus with fewer files than workers left the tail slices missing and phase 2's seed directory failed to load; an empty corpus died earlier still, with the unmatched `"$corpus"/*` glob handed to `ln` literally under `set -e`. The R112 round hit both shapes bootstrapping `ldm_mt_parity`, the one target without a corpus. `smoke.sh` now creates all W slice dirs up front and sets `nullglob`, so an empty corpus is a valid smoke input.

Corpora stay merged after campaigns, with encode going from 11653 to 4807 and encode_stream from 2518 to 1667. The recipe in the fuzz Readme needs hardlink splits on the corpus device, because a tmpfs `/tmp` cannot hardlink to it.
