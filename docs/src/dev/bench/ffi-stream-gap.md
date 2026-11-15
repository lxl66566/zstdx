# FFI Streaming Gap Against libzstd

This page attributes the gap measured when zstdx runs as a drop-in `zstd.dll` inside 7-Zip-zstd on Windows. The caller shape is a hinted, unpledged streaming feed, which none of the bench sections cover: `enc-st` and the bulk MT cells are one-shot, `enc-stream` drives the Rust pump without a hint, and the FFI write-feed path with `ZSTD_c_srcSizeHint` set never appears anywhere. The field A/B ran on 2026-11-14 on the author's Windows machine: zstdx built as the zstd DLL for mcmilk's 7-Zip-zstd, two game corpora compressed at levels 1/3/9/19 with `-mmt=16` and default parameters (no pinned windowLog), SHA256-verified plus bidirectional cross-backend roundtrips against the stock build, both clean.

## The field measurement

Corpus A is 457 MB over 746 files, corpus B is 3.51 GB over 26269 files, both single solid blocks and therefore single frames.

| corpus | level | ours MB/s | zstd MB/s | ours size / zstd size (MB) | ours / zstd peak MB | decode ours / zstd MB/s |
|---|---|---:|---:|---|---|---|
| A | 1 | 682 | 1015 | 358.8 / 352.3 | 829 / 110 | 682 / 816 |
| A | 3 | 682 | 1015 | 339.5 / 332.7 | 844 / 219 | 682 / 682 |
| A | 9 | 171 | 231 | 328.2 / 321.7 | 1300 / 783 | 682 / 802 |
| A | 19 | 43.9 | 23.3 | 320.4 / 312.9 | 2511 / 1977 | 692 / 682 |
| B | 1 | 759 | 618 | 3283 / 3247 | 1304 / 122 | 421 / 432 |
| B | 3 | 482 | 585 | 3144 / 3080 | 1548 / 267 | 395 / 421 |
| B | 9 | 304 | 387 | 3048 / 2956 | 1905 / 795 | 411 / 443 |
| B | 19 | 46.2 | 42.4 | 2936 / 2825 | 2885 / 2540 | 354 / 325 |

The table is the retest after the strip-floor landing (5676a96e; same corpora, flags and verification, the archives byte-identical to the first run). It moved B's l9 from 166 to 304 MB/s and B's l1 from 640 to 759, with A's cells and both l19 cells inside their spreads. The per-tier directions still match the dll100 stream-mt8 pattern (fastest x2.02, balanced x1.45, best x1.58 behind, fast x0.51, opt x0.50, ultra x0.68 ahead, see the [snapshot](snapshot.md)): l9 reads x1.27 behind on B and x1.35 on A, l1 is ahead on B (x1.23) and behind on A (x1.49), and the fast tier stays behind on both (x1.21 on B, x1.49 on A). Density stays behind 1-4% on both corpora at every level, the second flip against the bench's dll100 lead.

The build side is ruled out as a factor: the workspace pins no `target-cpu` and all SIMD goes through runtime feature detection (`common/simd.rs:53-61`), so the DLL vectorizes the same on any host. Corpus A's decode columns are saturated by the RAMDisk read ceiling (ours reads a constant 682-692 MB/s at every level) and carry no signal; corpus B's decode reads near parity, l3 and l9 about 6 percent behind libzstd and l19 ahead, consistent with the [decode gap](dec-gap.md) floors.

## The caller shape

7-Zip-zstd's `ZstdEncoder.cpp` sets exactly four parameters: `ZSTD_c_compressionLevel`, `ZSTD_c_nbWorkers` (from `-mmt`), `ZSTD_c_contentSizeFlag=1`, and `ZSTD_c_srcSizeHint` clamped to INT_MAX (`SetCoderPropertiesOpt`'s `kExpectedDataSize`, the solid-block total). It never calls `ZSTD_CCtx_setPledgedSrcSize`, never sets jobSize, windowLog or overlapLog, feeds input in `ZSTD_CStreamInSize()` = 128 KiB chunks with `ZSTD_e_continue`, sends one `ZSTD_e_end` at EOF, and never flushes mid-stream.

On the zstdx side this maps to a stream whose `InputShape::len` carries the hint while the pledge stays absent. The hint sizes the window, the tables and the LDM thresholds, but the job grid, the probe's size gate and the mismatch gates stay pledge-keyed by design ("a hinted stream must keep growing its jobs like any open-ended one and may stream past the hint freely", `stream/encoder_mt.rs:1319-1344`), so the frame runs the `JobGrid::Growing` schedule. This is the single largest asymmetry against libzstd: its job size formula does not consult the pledge at all (see below), so the same unpledged caller shape costs it nothing.

## Mechanisms

### The growing-grid ramp

An unpledged stream tiles jobs from `max(MIN_JOB_SIZE, overlap)` in epochs of `burst_jobs` equal jobs, doubling the job size per epoch up to `(BUF_CAP_MAX - overlap) / burst_jobs` (`stream/encoder_mt.rs:2636-2662`). At `-mmt=16` the per-row picture is:

- l9: the chain row's stream strip is the whole 64 MiB window (`stream_overlap_for`, `encoding/match_generator/mod.rs:923-929`), which already exceeds the 12 MiB cap, so the first epoch sits at the cap. No ramp.
- l19: the opt row's stream strip is its 8 MiB reach, cap 15.5 MiB, one doubling from 8 MiB.
- far-dead light frames, e.g. l3 with both far screens rejecting: overlap 2 MiB, cap 15.9 MiB, the jobs ramp 2/4/8/15.9 MiB over four epochs, about 0.5 GiB of stream at w16 before reaching the cap.

The ramp therefore bites a minority of the bytes. Its cost is concentrated exactly where the per-job tax below is worst relative to job size, but it is a secondary mechanism, not the main one.

### The per-job boundary tax

Every job boundary pays three charges:

1. Entropy tables restart self-describing: no Repeat_Mode carries across jobs, so each job's first block re-describes its Huffman and FSE tables (`encoding/mt.rs:10-11`, `reset_entropy_state` in `encoding/frame_compressor.rs:232-251`).
2. The repcode gate: a non-first job may not reference reps until three literal-offset sequences have rewritten the carried rep history (`encoding/mt.rs:1953-1969`; a raw fallback rolls the gate count back, `encoding/levels/fastest.rs:65-70`).
3. The LDM table clears: `restart` marks the table stale and `ensure_fresh` pays a full `table.fill(0)` plus bucket-offset clearing at the first touch (`encoding/ldm.rs:598-612`), 8 MiB at a 64 MiB window (2^(26-6) buckets split over u64 entries, `ldm.rs:575-591`). The clear is deferred, so RLE-only spans never pay it, but real data pays it in full on the first table operation of every job.

More workers shrink the Growing cap linearly, so `-mmt=16` amplifies all three charges relative to the bench's mt8.

### Row geometry against mixed data

The numeric levels are not calibrated against libzstd's rows. At large inputs (`encoding/match_generator/params.rs:607-718` against libzstd `clevels.h`):

| level | zstdx row | libzstd default row |
|---|---|---|
| 1 | fast, hashLog 15, 768 KiB window, LDM on; hint >= 2^26 raises the frame window to the 64 MiB full band (`mod.rs:1620-1646`) | fast, hashLog 14, 512 KiB window, no LDM |
| 3 | dfast, hashLog 17, 2 MiB window, LDM on, same full-band raise | dfast, 2 MiB window, no LDM |
| 9 | chain (lazy2), hashLog 21, 64 MiB window, 4 MiB reach, LDM | lazy2, 4 MiB window, no LDM |
| 19 | opt, 64 MiB window, 8 MiB reach, LDM | btultra2, 8 MiB window, no LDM below wlog 27 |

On dll100 (an ELF concatenation, match-dense with long repeats) the heavier rows win on both axes, which is where the bench's density lead comes from (the fastest tier outputs half the reference's bytes). On game corpora, a mix of audio, precompressed assets and text, the same rows pay for the LDM pass, the 8 MiB per-worker LDM tables and the cache footprint of the wide window while the far classes find little. This data-shape interaction, not the streaming path, is the leading explanation for the fast-tier flip and part of the l9 wall gap.

The level-1 and level-3 rows' hint raise in the table is the frame-continuous arming's (`LdmArming::Frame`, `mod.rs:1620-1646`: bulk and single-threaded paths). The field's streaming-MT feed carries `LdmArming::Job`, and a hint at or past the 64 MiB full band sits outside the fast-row screen's mid band (`fast_row_screen_pending`), so the light rows run their stock 1-2 MiB clamped windows with LDM disarmed by the mid-size bar: measured 2026-11-15, forced `FarClass::Dead` runs are byte-identical and wall-flat at l1 and l3 on the game-like payload under this caller shape. The light rows' far tax exists only where the capture engages, a hint-less stream or a mid-band hint, and there the armed class can hold a real density lead (fastest stream-mt8 on the same payload: +107 percent wall for +1.409 percent size demoted, against a 1.3 percent armed lead over libzstd). Whether the streaming FarClass screen demotes the mixed class effectively closed the same round at the chain row, where the demotion buys +42 percent wall for +0.306 percent size and no keyable paying-ness verdict exists over prefix evidence (see the [negative results](../negative/mt-stream.md)). The bulk path's per-frame adaptation for the balanced tier (the wide-head row-storage verdict, see the [matchers page](../perf/matchers.md)) stands.

### The strip-to-job imbalance

The bulk job strip is the row's search domain (`strip_for_level` = chain_reach, `mod.rs:912-915`), but the streaming strip is the whole window for the chain rows at row geometry (`stream_overlap_for`, `mod.rs:923-929`) so the LDM far class can ride cross-job through the adopted window; the fast rows reach the whole window only where their capture engages (a hint-less stream or a hint inside the [32, 64) MiB mid band — a hint at or past the full band runs the fast rows Stock, see the row-geometry section above). At w16 on l9 that pairs a 64 MiB strip with 12 MiB Growing jobs, a 5.3x ratio, unless the `FarClass` screen resolves Dead and caps every strip at the 4 MiB reach with job LDM disarmed (`stream/encoder_mt.rs:199-220`). The grid itself stays overlap-keyed ("the grid must stay a pure function of offset"), so a far-dead l9 frame keeps the 12 MiB cap computed from the 64 MiB estimate even though its strips are 4 MiB.

Measured on 2026-11-14: the streaming path refills per job. The far-alive (and Pending) frame's mid-stream Growing jobs all run `JobSpfStream::None` — `build_spf`'s snapshot chain exists only inside the finish tail — so every job re-runs `prefill_job_strip` over the whole window strip. The gungraun attribution lives in `bench/scratch/stripfill-measure.md` and the committed `zstdx-gungraun` `stripfill` bench (strip-length sweeps plus whole-stream far-alive/far-dead arms over dll100 at the FFI caller shape):

- A 64 MiB l9 strip prefill reads 9.98 G Ir unfloored; the chain grid is 27% of it, the LDM ingest 11.5%, the seed scan 59% in valgrind's scalar form (the window's tail has no seed twin, so the scan walks the whole strip; the AVX-512 form on hardware is bandwidth-cheap) and the gate replay 0.8%. The LDM `ensure_fresh` clear is ~1M u64 stores per job — an order below the gear scan.
- The landed fix floors the chain grid at the reach tail (`chain_strip_floor`): the walk admits candidates only within `chain_reach`, so below-floor heads and links are unobservable and the bytes cannot move — `dump --stream --all-levels` and `dump --all-levels` diff empty against the pre-change build. The m64 arm reads -25.3% Ir and -39.3% estimated cycles (LL hits -78%), and the chain grid's function share drops to ~1.5%.
- Whole-stream dll100 at this caller shape (w8, unpledged, full-size hint): far-alive 1.259 G Ir against far-dead's 182 M — the far machinery is 85.5% of the far-alive stream. After the floor, the remaining per-job whole-strip costs are the LDM ingest (the next round's theme, per-job clear and fill) and the O(strip) seed scan.

The same round fixed a latent pooled-residue defect the new regression test surfaced: `build_spf`'s first publication could snapshot the LDM state at `upto == 0`, where the builder's empty-strip `prefill_window` has only restarted it — the clone captured the pooled predecessor's table and an adopter's `restore` swapped it in without a clear. Publication now requires `upto > 0`.

### FFI feed and delivery copies

The FFI boundary copies the data on the way in: input arrives in 128 KiB `FEED_CHUNK` slices into the accumulate buffer (`zstdx-ffi/src/cctx.rs:29`, the feed loop at `cctx.rs:801-818`), the caller's contract shape. The delivery half landed as write-through on 2026-11-15: the streaming session drives the new pull encoder (`zstdx::stream::raw::Encoder`), and `deliver` (`cctx.rs:344-382`) moves the core's pending span straight into the caller's slice whenever the slice covers it, staging through the sink only when a burst (a multithreaded job's output) exceeds the slice. A split-first variant that always wrote the slice's worth direct and spilled the remainder measured x0.99 at w8, so the burst case keeps the whole-span spill. Measured through `ZSTD_compressStream2` on 200 MB urandom (128 KiB continue feed, `ZSTD_CStreamOutSize` slices, interleaved A/B between the two builds' DLLs): the single-threaded cells read 23.3 to 20.7 ms at l1 and 24.5 to 21.3 ms at l3 (x1.13 to x1.15), and the w8 cells read parity at 131 KiB and 4 MiB slices alike — the delivery copies there hide behind the workers' encode, so the multithreaded wall belongs to the strip machinery of direction 3, not to delivery. The bench's bulk and owned paths are zero-copy on the input side by construction, so this cost is invisible there.

### Memory anatomy

The unpledged decided stream reserves straight to the 256 MiB `BUF_CAP_MAX` (`reserve_decided`, `stream/encoder_mt.rs:2568-2588`), the thread-local buffer pool retains up to two cap-sized buffers per thread (`encoder_mt.rs:628-644`), the worker-state pool holds depth 32 (`frame_compressor.rs:262-291`) and the thread pool parks workers for 5 s idle (`stream/mt_pool.rs:66-68`). Together with the 8 MiB per-worker LDM tables this is level-independent, which is why l1 commits 1.3 GB where libzstd's l1 needs ~70 MB total, while l19 lands at rough parity (2.88 GB against 2.54 GB) because both sides are window-and-table bound there. Measured 2026-11-14: the cap reserve itself costs address space only on Linux (the untouched tail never faults; a hint-extent reserve measured as an RSS no-op, see the [negative results](../negative/mt-stream.md)). That reading is platform-specific: the field's peak-memory metric on Windows counts commit, and a malloc-backed reserve commits its whole extent there, so the straight-to-cap reserve and the pool's retained cap buffers join the touched per-worker state as attribution candidates on the field platform.

## Why libzstd pays almost none of this in the same shape

With jobSize unset, zstdmt derives the job size from the level alone: `2^MAX(20, windowLog+2)`, i.e. 2 MiB at l1, 8 MiB at l3, 16 MiB at l9, 32 MiB at l19 (`lib/compress/zstdmt_compress.c:1186-1198`), regardless of any pledge. Its overlap strip is `window >> (9 - overlapLog_default(strategy))` (same file, lines 1200-1245): 64 KiB at l1, 256 KiB at l3, 1 MiB at l9, the full 8 MiB window at l19. It also drops cross-job far matches (it carries no LDM at these levels at all) and rewrites table headers at job starts, so the qualitative losses match ours. Its per-byte boundary tax is an order of magnitude lower because the jobs are fixed and large while the strips are small, and its rows carry no long-distance machinery that could lose far matches in the first place.

## Directions

1. Closed 2026-11-14 on both forms: the seed regressed every light cell at field scale, and the finish-tail split already exists as the growing grid's tail rule (`build_bounds` re-slices the unposted remainder into `burst_jobs`-filling jobs), measured wall-optimal under the per-job strip cost — a 16 MiB l9 w16 tail bursts sixteen 1 MiB jobs for 191 ms, about 10% of the cell's wall (see the [negative results](../negative/mt-stream.md)).
2. Measured a no-op 2026-11-14: a hint-extent reserve moves no RSS (the cap reserve's tail is address space only) and the field's solid-block hints sit at or above the cap. The open memory work is attributing the l1 field peak on Windows, where the metric counts commit and the reserve commits in full (todo 35).
3. The per-job far machinery, quantified 2026-11-14 at l9 w16 on dll400: 149 ms of fixed cost per 12 MiB job of 484 ms — LDM ingest 84.5 ms over the 64 MiB strip, chain grid 36.1 after the reach-tail floor, gate 21.9, seed 6.9 — table-traffic into the strip-scale tables and invariant to the job span. It is the steady-state form of the finish-tail burst (direction 1) and the main body of the zero-tail l9 gap (250 MB/s against libzstd's 479). The chain-grid floor landed byte-identical (see the imbalance section above); every remaining form shares or shrinks the strip, whose falsified map the [negative results](../negative/mt-stream.md) carry (todo 33). The verdict arm measured and closed 2026-11-15: on a game-like mixed payload the demotion buys +42 percent wall for +0.306 percent size at l9 feed-mt16, the light rows carry no far machinery at this caller shape at all, and every keyable verdict form over prefix evidence is falsified (the [negative results](../negative/mt-stream.md) carry the A/B); the cost arm, the per-job LDM ingest and the O(strip) seed scan, stays the open surface.
4. Demoted 2026-11-14: the LDM per-job clear measured at ~1M u64 stores per restart, an order below the strip's seed scan (the stripfill bench), so it is not the next lever; the per-job LDM ingest and the O(strip) seed scan are.
5. Carry entropy tables across job boundaries with Repeat_Mode (format-legal, libzstd does not do it) as the structural density fix; the cheap interim is a larger minimum job on unpledged streams (todo 34).
6. Landed 2026-11-15: FFI output delivery writes through into the caller's slice via the pull encoder, staging only bursts larger than the slice; the single-threaded incompressible cells read about x1.14 wall and the multithreaded cells parity (see the feed and delivery section above).
7. Landed 2026-11-14: the `enc-feed` bench section reproduces this caller shape (128 KiB continue feed, hint set, no pledge, mt8/mt16, the mixed shapes and `--file` payloads).
8. Consider a libzstd-parity level mapping switch for the FFI compat layer, since drop-in callers expect `-mx9` to anchor near libzstd 9 on the speed and density axes (todo 37).
