# R115 probe record: hash-partitioned LDM insert staging (joiner insert run)

Date: 2026-11-20. Branch `r115-probe` (worktree `/root/programs/fork/zstd-rs-r115b`, base `5824711a`). Probe-only; no production code touched. Probe crate preserved at `bench/probe-r115/` on the branch.

Postscript from the landing round: the prefetch removal the probe surfaced (finding 4) landed on `r115-staging`, and the in-frame attribution it caveated was re-measured there — the joiner `insert_hits` run is 0.2 percent of job time at dll400 l9 w16 (the lane lease leaves about one leased multi-lane fill per burst, the rest interleave their inserts inside the serial fill), so the removal's frame-wall effect is flat while its solo and instruction effects stand. See the R115 round map in [research](../research.md).

## Question

Would hash-partitioned insert staging cut the joiner's serial insert run (`insert_hits`, 9.6% of the dll400 l9 w16 stream profile after the R114 lane-lease cap)? Reorder inserts bucket-major, preserving each bucket's position-order subsequence so table + `bucket_offsets` stay bit-identical (the landing contract), turning the 8 MiB table's random 128 B-granule scatter into a near-sequential sweep.

## Method

Scratch crate (`bench/probe-r115/`, originally `/tmp/r115probe`): verbatim copies of `GEAR_TAB`, `gear_run` (x86-64 asm), `gear_scan`, `split_fp`, `pack_pos`, `insert`, `prefetch_hit`, `insert_hits`; lane partition arithmetic from `fill_parallel` (lanes=3: part = ceil(span/5), helpers 2x part). Table = 2 MiB-aligned `MADV_HUGEPAGE` 8 MiB mapping (the `HugeBuf` placement), offsets = 64 KiB `Vec<u8>`.

Capture: real fill over a 64 MiB strip of `bench/big/dll100.raw` (content offset 32 MiB; dll400 is dll100 tiled 4x), absolute base 128 MiB, `restart(base)`-equivalent entry state (rolling = `!(u32::MAX as u64)`, arm = base). Staged the full position-ordered `(split, hash, cks)` sequence — the scan_lane form; the split set is position-pure so this equals the lanes' concatenated output.

Captured distribution: 1,044,806 hits (16,325/MiB), 15.94 mean/bucket, std 11.56, p50 13, p99 46, max 1188 (hot tail), 39.6% buckets >=16, 86.7% >=8, 5 empty. Second capture at content offset 4 MiB: 16.00 mean, std 31.4, max 7318 — same class, heavier hot tail.

Arms, all over the SAME captured sequence and real geometry:

- A0: stock per-lane `insert_hits` with `INSERT_PREFETCH_DIST`-16 prefetch (control).
- A0np: A0 without prefetch.
- A1: counting sort by bucket (counts, prefix sum, stable scatter into a 16 MiB bucket-major buffer), then bucket-major walk in per-bucket position order. Prefetch arm = DIST-ahead staged hit's bucket.
- A1np: A1 without prefetch.
- A2: per-lane (3-lane, `fill_parallel` bounds) bucket-major bins, joiner walks buckets in bucket order concatenating lanes in lane order. Prefetch arm = next bucket's lines at each bucket start.
- A2np: A2 without prefetch.

Timing: medians over 16 interleaved rounds (single-thread; order rotated per round) with a 192 MiB evict-scrub before every timed region, plus a no-scrub variant; and 12 interleaved rounds at 16 concurrent instances (16 threads x own 8 MiB table + own staging, barrier-released, makespan measured) approximating the w16 in-job burst. Counters: `perf stat` on single-arm loops (instructions, cycles, `l2_cache_req_stat.dc_access_in_l2`, `ls_rd_blk_x` = store RFO, `ls_any_fills_from_sys.local_ccx/near_cache/all_dram_io` = fill sources, dTLB-load-misses; AMD Zen4-class 16c/32t sample, 2x32 MiB L3, THP madvise). Lock protocol held (`flock -x` timing, `-s` builds).

Contract gate: FNV hash of table+offsets equal across all six arms — PASSED (`21e9a5118387ba1c` for every arm; loop-mode convergence hash `987ad3f4b469b071` shared by A0/A1walk/A2walk). The partitioned orders do preserve the bit-identical final state.

## Numbers

Single-thread medians (ms), scrub on / off:

| arm | net (scrub) | part | walk | net (no scrub) | vs A0 (scrub) |
|---|---|---|---|---|---|
| A0    | 2.637 | —    | 2.637 | 2.460 | — |
| A0np  | 1.276 | —    | 1.276 | 1.161 | -51.6% / -52.8% |
| A1    | 7.132 | 5.508 | 1.574 | 6.999 | +170.5% / +184.5% |
| A1np  | 6.591 | 5.154 | 1.438 | 6.511 | +150.0% / +164.7% |
| A2    | 7.641 | 5.632 | 1.983 | 7.670 | +189.8% / +211.8% |
| A2np  | 7.000 | 4.925 | 1.963 | 6.818 | +165.5% / +177.2% |

16-instance pressure (per-thread p50 ms, 12 interleaved rounds):

| arm | p50 | p90 | vs A0 | part | walk | makespan |
|---|---|---|---|---|---|---|
| A0    | 28.816 | 35.291 | 0     | —      | 28.816 | 32.625 |
| A0np  | 26.497 | 28.613 | -8.0% | —      | 26.497 | 28.487 |
| A1    | 54.384 | 56.446 | +88.7% | 37.925 | 16.821 | 55.814 |
| A1np  | 53.400 | 56.047 | +85.3% | 36.407 | 16.855 | 54.217 |
| A2    | 50.615 | 53.052 | +75.6% | 33.007 | 17.430 | 52.482 |
| A2np  | 50.868 | 52.250 | +76.5% | 33.192 | 17.484 | 51.776 |

(Run-to-run check: an earlier 12-round pass read A0 29.567, A0np 27.349 (-7.5%), A1 54.985 (+86.0%), A2 51.016 (+72.5%) — consistent.)

Counters per iteration (single-arm loops; the /200 or /100 denominators applied):

| arm | instr | cycles | L2 dc accesses | rd_blk_x (RFO) | L3 fills | DRAM fills | dTLB ld miss |
|---|---|---|---|---|---|---|---|
| A0     | 30.9M | 8.3M  | 2.96M | 105K  | 1.62M | 9.9K  | 2.7K  |
| A0np   | 22.7M | 6.9M  | 2.11M | 316K  | 0.86M | 6.8K  | 2.8K  |
| A1walk | 33.1M | 9.2M  | 449K  | 18K   | 5.3K  | 9.6K  | 5.4K  |
| A2walk | 26.9M | 11.7M | 469K  | 21K   | 9.7K  | 6.9K  | 0.26K |
| A1part | 24.8M | 22.9M | 3.71M | 1.27M | 1.06M | 149K  | 294K  |
| A2part | 26.1M | 22.6M | 3.48M | 1.33M | 0.99M | 162K  | 12.4K |

## Findings

1. The walk does exactly what the hypothesis wanted: bucket-major inserts drop store RFOs ~10-15x (105-316K to 18-21K) and L3 fills ~200x (1.6M to 5-10K) per run. The traffic conversion is real.
2. The reorder pass is the disease it treats. A counting-sort scatter of a uniformly random bucket sequence is itself a random 16 B-granule write over a 2x-larger staging area: 1.27-1.33M RFOs per pass, more than the entire stock insert run generates (105-316K), at 33-38 ms under pressure vs the 11.4 ms of insert-run span the walk saves (17.4 vs 28.8). Net: A1 +88.7%, A2 +75.6% under pressure; worse single-thread. The part/walk cost ratio is ~1.9-2.1x in every regime, so no cache-pressure regime flips the sign.
3. Placement cannot rescue it. Joiner-serial (A1/A2 as measured): the 33-38 ms partition exceeds the 11.4 ms saving outright. Helper-overlapped (A2 bins written during helper scans): the joiner must wait for ALL lanes' bins before the bucket-major walk starts, so the fill's critical path becomes max(lane scan + lane scatter) + walk; at the docs' solo 2.4 GB/s scan rate a helper lane's 25.6 MiB scan is ~10.7 ms and its ~1/3 share of the pressured scatter is ~11 ms, giving ~(10.7 + 11) + 17.4 = ~39 ms against the stock ~5.3 + 28.8 = ~34 ms (joiner start after lane 0's fifth-share scan, inserts overlapping the helpers). The overlapped form is worse on the critical path AND adds ~33 ms of helper CPU per fill inside the burst.
4. Prefetch verdict: partitioning does not make the prefetch dead — it is already a net pessimization on this box in both measured regimes. Removing the stock DIST-16 two-line prefetch reads -7.5/-8.0% of the insert run under 16-instance pressure (-51.6/-52.8% single-thread idle-cache). Mechanism: the T0 prefetches nearly double L3 fill traffic (1.62M vs 0.86M fills) and add ~8M instructions per run, and the loop's misses overlap themselves well enough without help. Caveat: the production falsification map tested DIST retunes (32, 64, one-line) inside noise, never removal; and the real burst runs ~1 leased parallel fill at a time (not 16 simultaneous joiners), so the in-frame effect should be smaller than 8% of the run. A removal A/B on the real encoder is the cheap follow-up, projected at most ~0.8% frame wall (9.6% x 8%) — below this probe's GO bar, a separate micro-lever.
5. A1 vs A2: A2 is uniformly better (net +75.6% vs +88.7% under pressure; per-lane staging is far more TLB-friendly: 12K vs 294K dTLB misses per scatter pass — A1's single 16 MiB 4 KiB-paged staging is the worst case; a hugepage-backed A1 staging would close part of that gap but the deficit is ~20x too large for a TLB fix). Neither is landable.

## Frame-wall projection (dll400 l9 w16, insert run at 9.6% of the stream profile)

- A1 net: +88.7% x 9.6% ≈ +8.5% frame wall (regression).
- A2 net: +75.6% x 9.6% ≈ +7.3% frame wall (regression).
- A2 walk-only with the scatter assumed free (upper bound, not achievable): -39.5% x 9.6% ≈ -3.8% frame wall; the honest critical-path model turns it positive (~+2%). No honest configuration projects a saving.

## Verdict

NO-GO. The GO bar was a projected net >= ~1.5% frame wall; every measured configuration projects a regression (net forms +7 to +9% frame wall; the overlapped form worse on the fill critical path). The mechanism premise survives in isolation — the bucket-major walk's RFO and L3-fill counts drop by an order of magnitude or more — but the reorder is only reachable through a random-granule scatter that costs ~2x the walk's entire span in every cache regime measured, on either thread placement. The hash-partitioned insert staging follow-up from the R114 round map's residue list is falsified by these numbers; the joiner insert run's remaining levers are removal/retune of the two-line prefetch (cheap A/B, sub-1% expected) or shrinking the run itself (fewer inserts), not reordering it.
