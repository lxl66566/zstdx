# Changelog

This document records the changes made between versions, starting with version 0.5.0

# After 0.9.0 (Current)

* Multithreaded compression now holds the single-threaded ratio. The
  overlap strip between jobs is the full level window and its positions
  are actually indexed into the search tables (`prefill_window`; before,
  the strip was borrowed as window but never indexed, so no sequence
  could ever resolve into it). The fast strategy fills the strip on
  libzstd's `fastHashFillStep` grid, and the chain store decision gained
  libzstd's replacement-margin gate (`ml*4 >= ilog2(offset) + 7`):
  the densely pre-indexed strips otherwise flood the stream with
  five-byte matches a window back — measured on the 32 MiB corpus,
  skewed Balanced went 1.86 -> 2.00 while its speed rose 42 -> 2139
  MiB/s single-threaded (51x; json 5.66 -> 6.08, text 363.5 -> 361.5,
  all MT16 cells within ~0.5% of single-thread and ahead of zstd-mt).
  Periodic repeats still escaped every table: on clumped data a
  period-old twin is always buried under newer same-hash recurrences,
  and a coarser grid only trades burial for phase misalignment, so job
  starts now probe a seed offset directly — the distance of the nearest
  56+ byte backward repeat of the strip tail, found by one backward
  scan at prefill. Three seed matches encode as plain literal offsets
  (legal under the repcode gate) and rotate the repeated-offset history
  until it holds the period, which the repcode probes then ride; a seed
  that stops matching retires on a probe budget. Periodic corpus
  (300 KiB period, 8 jobs): Fastest mt/st 2.70 -> 1.00, Fast 2.80 ->
  1.00, Balanced 2.70 -> 1.00; on the 32 MiB corpus every level and
  shape stays within +0.16% of single-threaded.

* New levels `Level::Opt` (≈zstd 16-17, btopt) and `Level::Ultra`
  (≈zstd 18-22, btultra/btultra2): a full port of libzstd's optimal
  parser (`zstd_opt.c`) — the lazy-filled binary match tree
  (insertBt1 / insertBtAndGetAllMatches with epoch-tagged absolute
  positions), the forward DP over stretches with adaptive
  fractional-bit price statistics persisting across blocks
  (rescaleFreqs seeding/downscaling, updateStats), the btultra
  match+1-literal recheck, and the 2-pass first-block statistics
  seeding (libzstd rewinds its window limits between passes; the epoch
  bump invalidates the pass-1 tree instead). `approximate_zstd` maps
  numeric levels 16-17 to Opt and 18+ to Ultra, and the CLI now
  accepts every level instead of `unimplemented!()` past 4. Two port
  subtleties were correctness-critical: `ZSTD_count` returns the
  delta from its start pointers (a resumed count must replace, not
  add to, the carried prefix), and libzstd's rep history is updated
  exactly once per series by the path traversal — emission-time
  sequence updates (`push_seq_packed`'s history fold) are the
  decoder-equivalent and replace it. Insert-side tree counts cap at
  the 4096-position DP window: re-filling regions the parser skipped
  (long matches at far offsets) otherwise re-counts hundreds of KB
  per candidate against stale hash heads — text went 45 -> 313 MiB/s
  (level Opt) with byte-identical output. 32 MiB corpus, ratio =
  raw/compressed: json Opt 7.57 vs zstd-16 7.28, json Ultra 7.52 vs
  zstd-19 7.49 (and zstd-19 forced to the same 1 MiB window: 7.53);
  text Opt 271.5 vs 272.0, text Ultra 275.9 vs 276.7; skewed and
  random at parity. Speed: json Opt ≈ zstd-16 (7.8 MiB/s), text Opt
  313 MiB/s vs libzstd's ~440.

* The literals Huffman table now gets optimal length-limited code
  lengths (boundary package-merge, Larmore-Hirschberg) instead of the
  rank-only weight ladder: the old scheme depended on count order
  but not magnitude, losing heavily on skewed literal histograms.
  Every level benefits; 32 MiB corpus ratios before -> after:
  json fastest 6.05 -> 6.20, json ultra 7.33 -> 7.52, text fastest
  196.6 -> 206.0, text best 242.0 -> 245.5.

* `Level::Best` (≈zstd 10-15) now runs the optimal parser in its
  cheapest setting (16 tree compares, targetLength 32) instead of a
  depth-24 hash-chain search: the chain matcher could not reach the
  tier's ratio at any depth. json Best 5.77 -> 6.88 (zstd-12: 6.14),
  text Best 245.5 -> 270.8 (zstd-12: 256.1), skewed Best 1.87 ->
  1.996, at 12-19 MiB/s depending on shape.

* The chain search now beat-checks candidates (libzstd's "potentially
  better" read): the 4 bytes ending at best_len+1 decide whether a
  candidate can strictly improve, so hash collisions reject on one load
  instead of a full extend. With the walks following real chains since
  the indexing fix, the search depths the ladder claimed are finally
  paid in full — so the chain levels are retuned against the honest
  cost: Balanced H17/d16/W1MiB -> H20/d8/W1MiB and Best
  H18/d64/W4MiB -> H21/d24/W1MiB (the wider window only ever found
  farther, not longer, matches and lost ratio and speed on every shape;
  hash growth shortens per-slot chains, which real walks need to
  terminate). 32 MiB corpus, ST bulk: json Best 23->83 MiB/s (ratio
  5.66->5.70), skewed Best 12->36 (1.817->1.867), text Best 1665->1652
  (364.5->363.3); json Balanced 98->134 (5.68->5.64, trading
  0.7% ratio for 37% speed), skewed Balanced 24->44 (1.848->1.865). Streaming Best reaches bulk parity (18 -> 78
  MiB/s on json, was 4.3x slower than bulk).

* The chain strategies' table links diverged from their walks: inserts
  keyed the chain by the window-relative index while walks resolved
  candidates by absolute position, and the two only coincide while
  `win_base == 0`. Every real driver advances `win_base` — the owned
  window compacts after two windows in streaming, the bulk path adopts a
  `[block_start - window, block_end]` window per block, MT jobs adopt
  job-relative bases — so from the first advance the walks followed links
  from unrelated chains: read4 gating kept the output valid but the
  search effectively ran on truncated scrambled chains, burning full
  search depth on rejected candidates. All three insert sites
  (`emit_chain`, the scan's own insert, RLE blocks' `skip_matching`) now
  key by absolute position like the walk. Consequences: streaming Best
  recovers bulk parity (18 -> ~23 MiB/s on json with the old ladder, and
  its output becomes byte-identical to bulk bar the frame header), and
  the bulk chain levels' speed/ratio points were artifacts of the broken
  search (the retune follows separately).

* A match source below the active segment (a wrapped-away previous-segment
  reference) resolves as one linear in-buffer copy instead of the generic
  segment walk: the previous segment physically sits `seg_a_end - offset`
  above the destination, and the wrap margin keeps every in-window source
  at least MAX_BLOCK_SIZE above the write cursor (never overlapping, ml
  never exceeding the distance) and its bytes at least 16 under the buffer
  end (wildcopy overshoot safe). Only sources straddling the wrap boundary
  or past the window bound keep the generic path. The fast path lives at
  the top of the out-of-line `copy_wrapped_match`, leaving the fused loop's
  codegen untouched — an inline version shifted its layout and cost
  json.zst1 ~1%. Interleaved A/B streaming: skewed.zst9 +4.8% (wrapped
  matches are 99.99% of its far-offset references; the walk was 15.6% of
  cycles), json.zst1/skewed.zst3 +0.9%, others flat.

* The fused sequence decoder carries the three FSE states instead of the
  packed table entries, and drops `bits` and `src_len` from its carried
  stream state entirely: the reload always rebuilds the bit window straight
  from memory (the `nb == 0` clamp path is provably idempotent on the
  window, and `ip >= 1` implies the stream spans a full 8-byte read), so
  the raw container never crosses a sequence boundary. The executor's
  output/literal cursors become raw pointers with folded bases — the
  virtual-address check reads `op + vbase_op`, the wrapped-source check
  `offset + wrap_base > op` — and the sequence countdown replaces the
  idx/nseq pair. The clamped reload inlines as a cold block instead of a
  call, which had forced every loop-carried value into a stack home.
  Together with a `HEADROOM` instantiation for targets that guarantee a
  block's size plus 16 bytes of slack (the flat streaming/MT buffers, via
  `ensure_block_space`; exactly-sized slice targets keep the checks), the
  per-sequence budget check and wildcopy gate vanish from the streaming
  loop. Interleaved A/B on the streaming path: json.zst1/3/9 +4.7/+6.3/
  +4.7%, text.zst1/3/9 +5.8/+7.4/+4.0%, skewed.zst9 +5.5%, others flat.
  State-packing the three FSE states into one u32 was tried and reverted:
  the pack/unpack ops land on the serial FSE chain and cost more than the
  spills they remove.

* `overlap_copy8` computed its post-spread source as `s2 + (8 - dec64)`
  in usize; for spread offsets 5-7 the adjustment is negative and the
  subtraction underflowed — a debug-build panic (12 corpus tests fail)
  and a wrapped-but-accidentally-correct pointer in release. The table
  is now the signed `8 - dec64` applied via `offset`, matching libzstd's
  `*ip -= dec64table[offset]` directly.
* The fused sequence loop's bitstream reads and repcode resolution lost
  their data-dependent branches. Zero-width reads (the `sum == 0` /
  `sum_t == 0` fast cases, taken constantly on rep0-heavy structured
  input) now run through a branchless `read` whose `wrapping_shr` turns
  `n == 0` into a hardware-masked shift by zero, returning the raw window;
  every field extraction gains a bzhi mask that is zero exactly when the
  field's width is zero, so the garbage never escapes. The offset history
  resolves and rotates entirely through selects (cmov) instead of the
  `<= 3` test and the rotation match, keeping the history load in bounds
  for real offsets by masking the index. Interleaved A/B on the streaming
  path: skewed.zst3 +4.5%, random.zst3 +1.9%, others flat (json.zst1
  branch-misses -4%, text -1..3%); the decoder-side FSE branches turn out
  to be a minority of the loop's mispredictions — the copy executor holds
  most of them (see PERF3), which a chunk-pair restructure halved but
  could not convert to wall time on this machine.
* New `bench_matrix` example: the head-to-head comparison widened to the
  full decode/encode × bulk/streaming × single-/multi-thread matrix.
  Five modes (`dec-st`, `dec-mt`, `enc-st`, `enc-mt`, `enc-stream`) run
  interleaved A/B against the zstd crate over the corpus ladder, with
  per-cell roundtrip gates, size/ratio reporting, a checksum-overhead
  row, worker-count scaling for the multithreaded encoder on both sides
  (fresh contexts per call, plus a warm reused zstd context as a
  reference line), and solo scaling rows for our parallel decoder, which
  libzstd has no counterpart for. The zstd dev-dependency gains the
  `zstdmt` feature so the multithreaded columns compile.
  First results (32 MiB corpus, libzstd 1.5.7): bulk decode leads on all
  11 files, streaming decode trails 1.05-1.40x on compressed shapes,
  encode Best leads every shape, json.Fastest/Fast trail 1.78x/1.35x,
  multithreaded encode is 2-4.5x ahead at >=8 workers, and the parallel
  decoder currently scales to 1.0x at best — the serial execution stage
  caps it. Along the way the correctness gates exposed two multithreaded
  decode bugs (see the two fixes below).

* The sequential fallback of `decode_to_vec_mt` retries with doubling
  capacity like `bulk::decompress`, honoring the same append contract as
  the parallel path for a fresh output vec (a plain
  `FrameDecoder::decode_all_to_vec` errors with `TargetTooSmall` on a
  zero-capacity vec because the frame size cannot fit). The doubling is
  tracked explicitly: re-reserving the current capacity is a no-op once
  spare capacity covers it, which would spin. Adds a regression test that
  multithreaded-decodes libzstd single-thread output of semi-structured
  records (huffman literals in back-to-back blocks, the shape whose
  accumulated-buffer staging the old check rejected) at levels 1/3/9.

* `decompress_literals` validates the number of literals it appended
  against the section's regenerated size instead of the output buffer's
  total length. The absolute comparison assumed an empty target, which
  holds for the sequential block decoder (it clears the buffer per block)
  but not for the parallel segment stager, whose literals buffer
  accumulates across a segment's blocks — every huffman-coded section
  after the first in a segment was rejected as a count mismatch. This
  made multithreaded decode fail on 7 of the 11 corpus files (all
  json/skewed variants and text.zst1; only shapes whose blocks use raw or
  RLE literals survived), silently falling back was not possible because
  the error aborted the whole decode.

* The interleaved 4-stream huffman fast loops (X1 and X2) keep their per
  stream state as raw pointers instead of region/output offsets, folding
  the region and out base pointers into `ip[]`/`op[]`. This drops the two
  base pointers from the loop's live set (14 hot values, fitting the GPR
  file) and removes the indexed addressing on every table lookup and
  output write. The X2 inner loop drops from 290 to 259 instructions and
  the X1 loop from 217 to 200; throughput is equal to slightly ahead
  (json.zst1 +0.5-1%, text +0.5-1%, rest within noise), and the loop sits
  at its practical floor — the remaining gap to libzstd's handwritten asm
  is an ALU-bound vs load-port-bound split of the same work (~0.75 vs
  ~0.7 cycles per byte on this core).

* The async checksum worker spins under a bounded budget (~300 µs) and then
  parks on a condvar instead of burning a core for the whole process
  lifetime. The head flip is published under the wake mutex so a parked
  worker cannot miss a post; the budget sits above the cadence of a
  worker-saturated pipeline (a 128 KiB raw block hashes in 50-100 µs), so
  the engaging payloads never pay a wake — verified by the worker's
  voluntary context switches staying at ~1 per frame during a saturated
  run and zero CPU during idle stretches. Throughput on the corpus is
  unchanged within noise (json.Fast 367 MiB/s, random.Fast interleaved A/B
  inside machine-drift variance).

* The fused flat decode loop addresses active-segment match sources as a
  plain `dst - offset` pointer (virtual distances are physical distances
  inside the linear active segment — the buffer-base indirection cancels),
  and moves the wrapped-history copy — sources at or below the segment
  boundary — into a cold outlined function, dropping the four-segment
  mapping state from the hot loop's live set. The per-sequence bounds
  checks merge into one budget: `w + ll + ml` against the target plus one
  `end + 16 <= out_len` gate shared by both wildcopy overshoots. Streaming
  decode on the 32 MiB corpus: json.zst1 1656 → ~1690 MiB/s (+2%),
  skewed.zst3 1183 → ~1220 (+3%), other shapes unchanged within noise.

* `do_offset_history` resolves and updates the repcode history from a
  single slot index (`code - 1 + ll0`, where slot 3 is the `rep0 - 1`
  pseudo-slot folded onto `scratch[0]`), replacing the two chained match
  trees with one branch; behavior is byte-for-byte identical (exhaustively
  tested against the old implementation). Throughput is neutral within
  noise on the corpus; the win is fewer instructions per sequence on
  repcode-dense streams.

* The single-table emit helpers (`emit_seq`, `emit_seq_chain`,
  `rep1_chain`) move from twelve-to-fourteen-argument free functions into
  a shared `TableEmit` context (head table, output streams, per-block
  constants), mirroring the dfast emit context, so their arguments stop
  spilling through the stack on every call. The fast and chain scan
  loops keep their own table accesses on a raw pointer, which holds the
  pointer in a register instead of reloading the context field per
  access. Output is byte-identical at every level; interleaved A/B
  against zstd -1: json at Fastest gains a further 466 → 481-488 MiB/s
  (1.85× → ~1.8×, 6-7% cumulative with the select probes), text ~1%
  faster, Fast/Balanced unchanged within noise.

* The Fastest level's scan loop resolves probe validity through a select
  (libzstd's selectAddr trick) instead of a three-comparison chain: stale
  hash-table entries alias the scanning position itself, and the byte
  compare plus one `cand != ip` branch rejects them. The repcode
  pre-probe folds its checked subtraction and window-base comparison into
  a single `probe >= win_base + rep[0]` bound computed once per scan
  iteration. Output is byte-identical at every level; interleaved A/B
  against zstd -1 on the 32 MiB corpus: json 455 → 466 MiB/s
  (1.90× → 1.85×), skewed 2724 → 2915 MiB/s, text ~1% faster.

* The Balanced level pins its chain-table log to its window (W20 + C20 +
  H17, was W21 + C20): the chain table is position-indexed, so its log is
  also the match reach, and the old pairing silently dropped links past
  1 MiB inside the 2 MiB window. With reach equal to the window every
  in-window position stays linked, and matches beyond 1 MiB — whose offset
  codes cost more than they save — disappear from the output. Interleaved
  A/B against zstd -6 on the 32 MiB corpus: json 165 → 177 MiB/s
  (1.14× → 1.06×) and skewed 110 → 152 MiB/s (0.97× → 0.70×, ahead on
  both speed and ratio now); compressed sizes shrink slightly on both
  (json −0.8%, skewed −0.4%), text unchanged in size and ~1% slower.
  Smaller tables in zstd-6's shape (C18/H19) were measured and rejected:
  reach truncation collapses skewed to 2.2×. Only Balanced output bytes
  change; other levels are byte-identical.

* The matcher's per-sequence emit path pays one buffer push instead of
  three: the packed code triple, merged add-bits payload and payload width
  now live in one `SeqWord` word per sequence (`Matcher::start_matching_codes`
  and the block encoder consume the single stream). The dfast strategy's
  emit helpers take a shared context (tables, streams, per-block constants)
  instead of a fourteen-argument signature, the push helper and code packer
  are always-inlined, zero-literal sequences skip the literal copy outright,
  and probe validity (epoch, window age, window buffer) resolves through a
  select that aliases invalid candidates to the scanning position — the byte
  compare then rejects them with one predictable branch instead of a
  three-comparison chain (libzstd's selectAddr trick). All levels emit
  byte-identical output; json at Fast gains a further 17% on top of the
  dfast matcher (314 → 366 MiB/s, 0.57× → 0.76× of zstd -3 per standalone
  A/B; bench ratio 1.57 → 1.32) and skewed.Fast 152 → 161 MiB/s.

* The Fast level now uses a port of libzstd's double-fast (dfast) matcher:
  two single-probe tables — an 8-byte long hash (2^17 slots) and a 5-byte
  short hash (2^16) — with a two-position pipeline, short-hit upgrades by
  the next position's long probe, libzstd's complementary four-anchor
  insertion after each match, an immediate rep-offset2 chain that
  re-seeds both tables, and a miss step that only grows every 256 skipped
  positions. This replaces the hash-chain + lazy search, which paid a
  per-byte chain insertion (2.5 MiB of hot tables and double searching)
  at what is libzstd level 3's dfast algorithm class. json at Fast gains
  80% (175 → 314 MiB/s, ratio 5.47 → 5.33, above zstd-3's 5.31), highly
  repetitive text 154% (3.1 → 7.9 GiB/s, now faster than zstd -3), skewed
  10%; other levels are byte-for-byte unchanged. The strategy selection
  is now an enum (`Fast`/`Dfast`/`Chain`) instead of an optional
  chain-table log.

* The flat decoder's sequence executor copies literals and matches with
  inline 16/8-byte chunks (libzstd's wildcopy scheme, budget-gated 16 bytes
  before the output end with an exact-copy tail path) instead of one libc
  memcpy/memmove call per sequence: sequence-dense payloads paid millions
  of ~5-byte PLT calls per 32 MiB. Sub-16 offsets go through a port of
  ZSTD_overlapCopy8 (dec32/dec64 tables) whose leading four bytes copy one
  at a time so each store feeds the next load. Offsets below 16 in the
  wrapped streaming view run 8-byte chunks when the source stays inside the
  active segment and exact copies across the segment boundary. Decode of
  json/skewed/text at levels 1-9 gains 23-45% (e.g. skewed.zst9 481 →
  673 MiB/s, json.zst1 streaming 0.61× → 0.76× of the zstd crate), with
  incompressible and run-length payloads unchanged.

* The chain levels grow their probe step on long literal runs (the fast
  strategy's miss-acceleration policy), so incompressible data no longer
  pays a full chain walk per byte: random 32 MiB compresses at ~2.1 GiB/s
  on every level, with `Best` faster than libzstd's level 12 (2118 vs
  918 MiB/s) at identical (stored) ratios.

* Compression levels beyond `Fastest`: `Level::Fast` (≈ zstd 3-5), `Level::Balanced`
  (≈ 6-9) and `Level::Best` (≈ 12-15) join the ladder, each backed by a real
  hash-chain matcher inside `MatchGeneratorDriver` (per-level hash-log,
  window, chain-table size, search depth and lazy depth, following
  libzstd's `clevels.h`). The chain walk prefers repcode candidates,
  extends matches backwards into pending literals (keeping one literal
  pending for repcode emissions — a zero-literal repcode resolves to a
  repcode swap on the decoder side), defers emission across lazy steps
  when a longer match starts nearby, and fully chain-indexes covered
  ranges (a coarse grid inside long matches). `Fastest` keeps its
  byte-identical single-probe fast loop; `Level::approximate_zstd` maps
  numeric levels (1-2 → Fastest, 3-5 → Fast, 6-9 → Balanced, 10-22 →
  Best) and the compat layer now uses it instead of flattening every
  level to Fastest. On 32 MiB of repetitive text the ladder reaches
  0.0043/0.0041/0.0041 (zstd 3/6/12: 0.0045/0.0041/0.0039) at
  2300/1600/1000 MiB/s; on short-match synthetic data it trails zstd's
  corresponding levels by 5-25% (no optimal parser yet). Multithreaded
  jobs overlap chain levels at window/4 to hold the ratio within 2% of
  the single-thread path.

* Parallel decoding of complete in-memory inputs: `bulk::decompress_with`
  and `bulk::decompress_to_buffer_with` with `DecoderOptions::threads(n)`
  engage a segment-parallel decoder on std builds. A pre-scan walks the
  block headers and splits the input at *restart points* — blocks whose
  entropy state is fully self-describing (literals not Treeless, no FSE
  stream in Repeat mode). Job-based encoders emit exactly those at every
  job boundary (libzstd `-T` output and this crate's multithreaded
  compressor alike), and frame starts are restart points by definition, so
  concatenated multi-frame inputs parallelize as well. Stage A (worker
  pool) decodes each segment's literals and FSE sequences into staging and
  computes its exact output size; stage B (calling thread, in input order)
  executes them into the output, carrying the repcode history across
  segments — repcode resolution is the only cross-sequence state and it
  never touches stage A. Output buffers are sized segment by segment (the
  Vec variants grow exactly), staged segments are bounded to the worker
  count, and dictionary frames, malformed input, single-restart inputs and
  single-core processes fall back to the sequential decoder, which also
  owns error reporting. 64 MiB of previously-encoded data decodes 2.06x
  faster with 4 threads (2.1x at 8; the serial execution stage bounds the
  speedup).

* Multithreaded one-shot compression: `bulk::compress_with(source,
  &EncoderOptions)` engages a job-parallel path on std builds when
  `workers > 1`. The input splits into jobs (twice the worker count, 1 MiB
  floor); each job compresses through the per-thread pooled slice state
  while borrowing a `window/8` strip of the preceding job as match history,
  and the assembled output stays a single regular zstd frame with the total
  size pledged in the header. Two invariants mirror libzstd's zstdmt and
  keep jobs independent: every job starts from reset entropy tables (no
  Repeat modes across a boundary) and from the second job on the matcher
  gates repcode references until three literal-offset sequences have
  rewritten the repeated-offset history. The frame checksum is hashed on
  the calling thread while the jobs run, and finished job bytes append in
  order as they land (condvar handshake; a worker panic resumes on the
  caller after the assembly drains). Small inputs, single workers, raw
  levels and single-core processes fall back to the byte-identical
  single-thread path. Throughput on 64 MiB of text-like data scales near
  linearly (2 workers 2.05x, 4 workers 3.95x, 8 workers 7.55x) at a ratio
  cost below 0.1%. `compress_slice_to_vec` gained a checksum-aware sibling
  `compress_slice_opts` used by the new entry point.

* The `dict_builder` feature's raw-dictionary builder module moved from
  `ruzstd::dictionary` to `ruzstd::dict`, matching the zstd crate's naming
  and the new top-level module layout (`decoding::Dictionary` stays where
  it is; it parses dictionaries rather than building them).

* The benchmark examples now measure through a shared interleaved A/B
  harness (`examples/common/mod.rs`, pulled in via `#[path]`): both sides
  alternate round by round so slow machine drift (thermal, clocks,
  background load) hits them equally and the per-round time ratio is the
  primary output; each side runs after one warmup round and until a time
  budget (500 ms default, `BENCH_BUDGET_MS` overrides), with median/min/max
  and median-absolute-deviation stats instead of bare means. `bench_compare`
  reworks its decode cells into slice/stream pairs and its encode cells
  against zstd levels 1 and 3; `bench_small` batches ~4 MiB of calls per
  round so small payloads don't measure timer overhead (keeping the IMPL and
  SIZE env knobs for profiling); `bench_encode` measures against zstd -1.
  A full `bench_compare` pass stays under roughly two minutes at the
  default budget.

* A zstd-crate compatibility layer at `ruzstd::compat` (std builds): the
  `zstd` crate's module layout, type names, numeric levels and `io::Result`
  signatures on top of the pure-Rust implementation, so `zstd::` imports
  swap to `ruzstd::compat::` with minimal churn. Covered: `bulk::{compress,
  compress_to_buffer, decompress, decompress_to_buffer, Compressor,
  Decompressor}`, `stream::{read::{Encoder, Decoder}, write::{Encoder,
  AutoFinishEncoder, Decoder, AutoFlushDecoder}, encode_all, decode_all,
  copy_encode, copy_decode}` and `DEFAULT_COMPRESSION_LEVEL` /
  `compression_level_range`. The compat encoders defer stream start so zstd's
  post-construction setters (`set_pledged_src_size`, `include_checksum`,
  `window_log_max`, ...) work before the first write and fail afterwards;
  the compat read decoder likewise defers its first-frame init. Dictionary
  decoding works end to end (a libzstd-trained dictionary plus a
  libzstd-compressed dict frame round-trips through it, verified in tests);
  dictionary encoding and multithread(>1) return errors. Every numeric level
  compresses with the fast strategy. `DecoderOptions` stores dictionaries
  as raw bytes now (parsed when a decoder takes the options), which makes
  the option set `Clone`.

* Streaming decoders: `ruzstd::stream::read::Decoder` decompresses while
  reading and — unlike `decoding::StreamingDecoder`, which is documented to
  stop after one frame — is transparent over concatenated frames and
  skippable frames (`single_frame()` restores the one-frame behavior; a
  4-byte magic peek distinguishes a clean frame boundary from a truncated
  magic). `ruzstd::stream::write::Decoder` decodes compressed bytes written
  to it into an underlying writer (`auto_flush()` wraps it to flush the
  writer per write). The write decoder stages input and only hands a block
  to the FrameDecoder once the block header, body and — behind the last
  block of a checksummed frame — the 4-byte trailer are fully staged,
  because a starved block read would poison the decoder state. One-shot
  conveniences over the same machinery: `ruzstd::stream::{encode_all,
  decode_all, copy_encode, copy_decode}`. A `crate::error::into_io` helper
  sidesteps the no_std io::Error's inherent `from(ErrorKind)` shadowing the
  `From<crate::Error>` impl at `.map_err` call sites.

* Streaming encoders: `ruzstd::stream::write::Encoder` (io::Write in,
  compressed out, with `auto_finish`/`on_finish`/`finish`/`try_finish`/
  `do_finish`, and `flush` emitting the staged partial block early) and
  `ruzstd::stream::read::Encoder` (io::Read over a compressed reader). Both
  share an incremental core built from the same matcher/block-encoder
  building blocks as `FrameCompressor`; without intermediate flushes the
  output is byte-identical to `encoding::compress` over the same bytes (the
  byte-equality is asserted by tests across write chunkings of 1 B, 7 KiB,
  block-size and everything-at-once). `EncoderOptions::{pledged_size,
  checksum, workers}` are honored: pledged sizes land in the frame header,
  checksum(false) omits the 4-byte trailer, and workers > 1 fails with
  `Error::Unsupported` until the multithreaded backend lands. The pooled
  slice fast path is untouched; release assembly of all pre-existing symbols
  is unchanged.

* New high-level one-shot API: `ruzstd::{compress, decompress}` and
  `ruzstd::bulk::{compress, decompress, decompress_to_buffer}`.
  `bulk::compress` forwards to the pooled slice fast path unchanged;
  `bulk::decompress` starts from a capacity hint (0 = auto) and doubles until
  the decoded data fits, transparently consuming concatenated and skippable
  frames. Configuration moves into builder option sets:
  `EncoderOptions::{checksum, pledged_size, workers}` (more than one worker
  currently fails with `Error::Unsupported` until the multithreaded backend
  lands) and `DecoderOptions::{max_window_size, dictionary}`. The new
  `ruzstd::{Error, Result}` umbrella converts frame corruption, dictionary
  and io errors via `?`, and implements `From<Error> for std::io::Error`.
  `Dictionary` gains a compact `Debug` (id + content length; the entropy
  tables are omitted).

* All hand-written `Display`/`From`/`std::error::Error` impls in
  `decoding::errors` are now derived with thiserror (the crate's first
  external dependency; compile-time only, and `default-features = false`
  keeps the no_std and `rustc-dep-of-std` builds working — without `std` the
  derive emits `core::error::Error` impls instead). Variant names, payloads
  and message texts are byte-identical to the previous impls; the file drops
  from 1163 to ~330 lines. `io_nostd::Error` and `GetBitsError` now implement
  `core::error::Error` unconditionally (std re-exports the same trait) so they
  can stay error-chain sources under no_std.

* `CompressionLevel` is replaced by a root `Level` enum
  (`ruzstd::Level::{Uncompressed, Fastest}`, `#[non_exhaustive]`,
  `Level::DEFAULT = Fastest`). The `Default`/`Better`/`Best` variants never had
  implementations and panicked at runtime when reached, so they are gone;
  further variants arrive as their strategies land. The CLI previously
  defaulted to the panicking `Default` variant (level 2) and now maps 1..=4 to
  `Fastest` with `Fastest` as the default. Codegen is unchanged except that
  the per-block level dispatch loses its dead `unimplemented!()` arm (verified
  by instruction-stream diff of the release assembly: 249 global symbols
  compared, only `compress_slice_to_vec` differs, at exactly that dispatch).

* The decoder now verifies frame checksums with the same in-tree XXH64 as
  the encoder (the module moved from `encoding` to the crate root).
  `twox-hash` drops from runtime dependency to dev-dependency (kept purely
  as the test reference), leaving ruzstd without any external runtime
  dependencies; the `hash` feature no longer pulls in external code and
  now also builds under `rustc-dep-of-std`. Standalone the two
  implementations measure equal (~20 GiB/s bulk on the dev box, four
  accumulator chains already saturate round latency; 32 B - 8 MiB inputs
  within noise), and the checksummed decode corpus A/B confirms parity:
  15 shapes x 2 back-to-back rounds, slice deltas +3% to -6% with a mean
  of +0.4%, all inside the machine's noise band. `DecodeBuffer::hash`
  narrows from `pub` to `pub(crate)` since the hasher type is no longer a
  public dependency.

* Sequence FSE tables can now be repeated across blocks (mode 3): when the
  previous block's table covers every live code and its estimated bit cost
  for the current histogram stays within a fresh table's description cost
  plus entropy bound, the block reuses it and writes no table description.
  The comparison is libzstd's cost-based selection from the lazy
  strategies (the fast-strategy shortcut only engages with dictionary
  tables, which this encoder never has). Predefined and RLE table choices
  invalidate the remembered table, so a later block can never repeat
  against a decoder whose table was replaced. json: -2.2% instructions at
  32 MiB, ratio 5.99 -> 6.00, json-1M +1.8%; text/skewed ratios unchanged
  or better; all outputs cross-checked against the reference zstd decoder.

* The slice path (std + hash, input >= 256 KiB, >= 2 usable CPUs) offloads
  the frame checksum to a sidecar thread: a per-thread single-producer/
  single-consumer ring of (pointer, len, state) tasks feeds one spinning
  XXH64 worker with per-frame states, so the four serial accumulator chains
  leave the compression core's critical path entirely; `finish` posts one
  reply task and waits on it. Every post claims its ring slot by reading
  the shared head, which keeps nested frames on the same thread in distinct
  slots (a producer-local sequence counter lets two live frames overwrite
  each other's tasks and kills the worker). Single-core pinning and small
  inputs hash inline as before; output bytes are identical. 32 MiB wall
  (4-core taskset, hash on): random +13%, text +26%, skewed +13%, zeros
  +2.5%, json +4%; random-1M +12%.

* The sequence bitstream encoder appends each sequence's add-bit payload
  with its state-transition bits in one accumulator push when the combined
  width fits (the common case), halving the per-sequence flush checks.
  json drops 0.4% instructions; wider pairs fall back to the two-push path
  and the emitted bits are identical everywhere.

* The matcher now emits sequences straight into the packed streams the
  sequence-section encoder consumes (`Matcher::start_matching_codes`, a new
  default trait method): each match computes its literal-length/match-length/
  offset codes and merged add-bits payload once, at the emit where the raw
  values are hot, instead of pushing a 12-byte (ll, ml, of) triple that the
  block encoder later re-read and re-encoded in a separate pass. The raw
  triple only survives in the public callback API, reconstructed from the
  packed form. json drops 3.5% instructions at 32 MiB (5.85G -> 5.65G, ~+2%
  throughput), text/random ~0.7%, output bytes identical.

* The frame checksum moved in-tree (spec-exact XXH64 with two 32-byte
  chunks per iteration, keeping eight accumulator chains in flight) and is
  now fused into passes that read the block anyway: the RLE uniform scan
  absorbs while it compares (uniform blocks hash in their only pass; any
  mismatch returns a resume offset), the raw block copy absorbs the
  remaining range while it copies, and a zero-sequence block whose
  literals fail the strided entropy sample skips the encode-and-discard
  round trip entirely (nothing written, raw emitted straight away - the
  outcome the size fallback would have chosen). With the hash feature on,
  zeros gains 22% at 32 MiB (14.8 -> 18.1 GiB/s) and random ~10% at small
  payloads while staying byte-identical everywhere; without it, random
  gains 4% and text 1%. Two inlining pitfalls fixed along the way: the
  uniform scan outlined (its data pointer used to reload from the stack
  every 32 bytes inside the enlarged caller) and the literals gate now
  runs before any literal is written for zero-sequence blocks.

* Zero-sequence blocks no longer stage their literals in the block scratch:
  the matcher skips the whole-block copy and the block encoder reads the
  bytes straight from the window (the callback path hands the window slice
  over as the trailing `Literals`). Blocks that compress to nothing - every
  random or skewed block - drop one full write+read pass; output bytes are
  unchanged. random gains ~13% at 64 KiB-1 MiB payloads in a clean process
  (7061 -> 8013, 8210 -> 9332 MiB/s), skewed ~4% at 32 MiB (2473 -> 2560
  MiB/s).

* `compress_slice_to_vec` pools its encoder state in a thread-local (hash
  table, default FSE tables, block scratch): per-call rebuilds of those
  dominated small inputs. 1 KiB payloads compress 3-7x faster (json 152 ->
  444, random 268 -> 1938 MiB/s), 4 KiB gains 30-80%; large inputs are
  unchanged. A new `bench_small` example tracks 1 KiB-1 MiB payloads
  against the zstd crate's bulk path.

* Literal blocks whose alphabet stays within sixteen symbols histogram
  through an AVX-512 kernel: sixteen per-slot byte compares with popcount
  accumulation over four 64-byte chunks at a time, entered once the slot
  set is established and left for the four-lane scalar pass whenever a
  seventeenth symbol appears (the popcount sum doubles as the coverage
  test). skewed executes 18% fewer instructions and gains ~28% throughput
  (1917 -> 2455 MiB/s); wider alphabets bail on the first offending chunk,
  costing one chunk per block. Counts are exact, so block bytes are
  unchanged.

* Near-incompressible literal blocks reject through a strided entropy
  sample (1024 draws, Miller-Madow corrected, distinct-symbol prescreen)
  before the exact four-lane histogram runs, with a sticky per-stream hint
  that skips the sample once a block clears the exact bound. random
  executes 43% fewer instructions and gains ~43% throughput (1450 ->
  2060 MiB/s); other shapes are within measurement noise. The reject floor
  sits ~0.2 bits/byte left of the exact bound's, so only literals within
  ~2% of raw size can encode slightly larger; the benchmark corpora are
  byte-identical.

* The `--no-default-features` build compiles again: the literals entropy
  precheck used `f64::log2`, which is std-only; no_std builds now use a
  linear-mantissa approximation (error < 0.086 against the 8% reject
  margin). std builds are unchanged.

* Flat four-bit huffman streams (uniform alphabets of 9..16 symbols, e.g.
  low-cardinality columns) pack through an AVX-512VBMI kernel: one 64-symbol
  chunk resolves its 256-entry code LUT with two byte permutes, reverses
  and pair-packs nibbles with two more, replacing sixteen scalar LUT loads
  per sixteen symbols. skewed executes 48% fewer instructions and gains
  ~43% throughput; the scalar loop remains for sub-64-symbol tails and
  non-x86/no-std builds, and the output is bit-identical.

* New `compress_slice_to_vec` entry point compresses an in-memory buffer
  with no intermediate copies: the matcher window borrows the input
  directly (eliminating the read pass, window compaction and per-call
  window allocation of the streaming path) and blocks append straight into
  the output vector, which is sized up front. Output is byte-identical to
  the streaming path, including its exact-block-multiple trailing empty
  block. text gains ~40% and zeros ~60% throughput, random ~12%; the
  matcher-bound shapes are unchanged.

* Long matches index only two anchors (start+2, end-2) in the hash table
  instead of every fourth position plus the final byte, mirroring zstd's
  fast-strategy fill policy; short matches (<= 16 bytes) keep their dense
  indexing. The 4-byte grid across long matches dominated encoder time on
  highly repetitive data: text executes 31% fewer instructions and gains
  32% throughput while its ratio improves (295.0 -> 298.6), json gains 5%
  with its ratio nearly unchanged (6.02 -> 5.99).

* The scan loop's hash-table probes read their slots unchecked as well: the
  hash masks to the table's power-of-two size, so the per-probe bounds
  checks were provably dead (the insertion side already dropped its check).
  json executes another 2% fewer instructions and text gains 4% throughput
  with no corpus regressing in either A/B order; output stays bit-identical.

* The matcher's index insertion stores its hash-table slot unchecked: the
  hash already masks to the table's power-of-two size, so the bounds check
  on every inserted position was provably dead. json executes 2.3% and text
  5.4% fewer instructions (text's long matches pay the most insertions);
  output stays bit-identical.

* Compressed blocks encode straight into the frame output: the block writer
  reserves the three-byte header, encodes the content in place and patches
  the header once the compressed size is known, removing the per-block
  staging vector and its full-content copy on adoption. The per-block
  literals, sequence and precomputed-code buffers moved into pooled
  compressor state (`BlockScratch`), so steady-state blocks run without the
  allocate-and-double chain. random executes 6.8% fewer instructions (raw
  fallback blocks used to copy their whole content), json and skewed gain
  2-3% each; output stays bit-identical.

* The sequence bitstream encoder keeps its bit accumulator in locals behind
  a small hot-push helper (one unaligned u64 store per flush instead of two
  writer-method round-trips per sequence) and reads the FSE transition rows
  through a flat unchecked `code << log | state` index — the row stride is a
  power-of-two shift, not a runtime multiply, and codes/states cannot leave
  the table by construction. json executes 1.4% fewer instructions and gains
  ~3% throughput; output stays bit-identical.

* The literals histogram fills four sub-histograms keyed by position mod 4
  and merges them once per block, so concurrent increments land in
  different cache lines instead of serializing on same-counter store
  forwarding (small alphabets hit the same counters constantly).
  Instruction count is unchanged; skewed drops 7% of its cycles and gains
  ~10% throughput. Output stays bit-identical.

* Flat huffman tables (every symbol sharing one code length, e.g. the
  9..16-symbol alphabets of uniform data) take a dedicated bulk encoder
  path: after byte-aligning the pending bits it packs two four-bit codes
  per output byte straight into the destination, replacing the
  variable-length accumulation chain. skewed gains 53% throughput
  (47% fewer instructions); other corpora are untouched and output stays
  bit-identical.

* The three sequence-code histograms (literal length, match length, offset)
  fill in a single pass over the packed codes instead of one pass per
  table; the per-table mode decision moved into a shared helper. json
  executes 0.7% fewer instructions; output is bit-identical.

* Huffman stream encoding batches four symbols between bit-container
  flushes (one unaligned u64 store per four symbols instead of a
  container-overflow branch per symbol), driven by a packed
  `(code << 4) | num_bits` u16 code table that keeps the whole table in
  one cache line pair. skewed gains 71% and json 4% throughput; output is
  bit-identical.

* The literals histogram is computed once and shared between the entropy
  precheck and the huffman table build (both used to scan the full
  literals buffer separately). skewed gains 17% throughput; output is
  bit-identical.

* The scan loop interleaves two adjacent positions (libzstd's ip0/ip1
  pipeline): hashes and table entries for both are prepared before either
  is probed, overlapping the hash multiply and table load latencies, and a
  fully-missed pair advances by twice the miss step so probe density on
  incompressible data is unchanged. json gains 2% throughput and 6%
  ratio (5.67 -> 6.02, near zstd -1's 6.11) because the pair-step skips
  over short-match starts the way the ml>=6 gate does; text gains 2%
  throughput at -1.3% ratio; other corpora are unchanged.

* The bit writer's 64-bit flush stores one unaligned u64 into the reserved
  output vector instead of calling memcpy for eight bytes, removing a call
  per flushed container from every entropy-coded block. Output is
  bit-identical; json/text/skewed gain 1-2% each.

* The uniform-block detector compares four u64 words per branch instead of
  one, so fully-uniform blocks (zero-filled inputs, padded corpus tails)
  stop paying a branchy scan of the whole block while non-uniform blocks
  still exit after the first batch. zeros +7%, text +1%, other corpora
  neutral.

* Encoder round eight: sequence codes and their add-bit payloads are
  precomputed in a single pass over the sequences (codes packed into one
  u32 stream, add bits pre-merged into one u64 per sequence), replacing the
  three separate code arrays, the per-sequence out-of-line encoder-helper
  calls, and the metadata re-lookups in the bitstream encoder. json +3%
  throughput, all other corpora neutral, output bit-identical.

* Encoder round seven, data-path focused: the match window holds two windows
  plus one block of capacity so compaction copies ~1x data volume instead of
  once per block; block input is read directly into the window tail through
  the new `Matcher::block_tail`/`commit_block` API (replacing the
  `get_next_space`/`commit_space` buffer ping-pong and its pooled-buffer
  zero-fill); sequence FSE tables build from flat arrays (probability list,
  start-state list, packed transition table) instead of 256 per-symbol
  vectors with two sorts each; the scan loop keeps its cursors in locals
  across emissions; and tiny literal runs copy with one unaligned u64 while
  the position hash loads the full u64 masked to five bytes (mathematically
  identical hash values, fewer instructions). text 2.5 -> 3.8 GiB/s, zeros
  4.5 -> 8.2 GiB/s, json 287 -> 334 MB/s, ratios bit-identical.
* Sequence codes are computed once per block and shared between the FSE
  table selection and the bitstream encoder, uniform literal sections
  encode with the one-byte RLE literals mode, and a cheap entropy-bound
  check skips Huffman attempts on near-incompressible literals instead of
  encoding them and discarding the result (random-bytes encode 2.4x
  faster, ratios unchanged).
* The match window widens from 448 KiB to 768 KiB so repository-tile-sized
  repetition periods stay matchable; hash matches now require 6 bytes (a
  5-byte match's sequence overhead roughly equals the literals it covers,
  and rejecting it lets the scan find the longer match that starts next);
  the hash table shrinks to 2^15 heavily-contended slots with half-rate
  miss-path inserts (newest-wins then prefers close, cheap-to-encode
  offsets); and sequence FSE tables gain libzstd's fast-strategy mode
  selection (RLE for single-code blocks, predefined below the dynamic-table
  break-even, otherwise a normalized table built with the ported
  FSE_normalizeCount + optimalTableLog and the last-sequence count
  discount). json ratio 5.22 -> 5.70 at +27% throughput, skewed ratio
  2.00 (= zstd -1) at +81%, text ratio 6.56 -> 239 where the wider window
  unlocks cross-tile matches.
* RLE block detection compares 8 bytes at a time (libzstd `ZSTD_isRLE`
  style) instead of a per-byte closure over an indexed first element, and
  skipped (RLE) blocks index only their first position instead of every
  byte — a uniform run hashes to one table slot, so per-byte indexing just
  rewrote it. zeros encode 7.3x faster.
* The encoder's block emit path is reworked to keep the hot scan loop free
  of allocator traffic and register spills: entropy tables are taken out of
  the compressor state by value for the duration of a block (a raw-block
  fallback puts them back) instead of deep-cloning three FSE tables plus
  the Huffman table per block; the built-in matcher gained a
  `start_matching_into` buffer sink (default-implemented on the `Matcher`
  trait, so custom matchers are unaffected) that appends literals and
  sequences directly instead of routing every emission through a closure
  capture; and the matcher's window reads (hash input, 4-byte probes,
  u64 match extension) are unchecked with caller-guaranteed bounds.
  json +24%, text +39%, skewed +6% end-to-end encode throughput.
* The encoder's match emitter indexes covered matches sparsely instead of
  hashing every byte: matches up to 16 bytes keep every position (they carry
  most of the alignment coverage on structured data), longer matches fall
  back to a 4-byte grid anchored at the match start plus the final byte.
  json +6% / text +5% encode throughput at -0.2% / -0.6% ratio.
* Sequence decoding extracts each group of bitstream reads (the three
  add-bit fields and the three FSE state transitions) with a single window
  read whose bits are then split in parallel, instead of six serial
  load-shift-store chains per sequence. The mid-sequence reload guard only
  applies to the rare wide-field path (all three add-bit widths sum above
  31). skewed +5-8% (zstd-3/zstd-9), json.zst3 +3-5%, text.zst1 +2-3%,
  rest neutral.
* Huffman literals decoding gains a double-symbol (X2) table for the
  interleaved 4-stream fast path, ported from libzstd: each lookup emits one
  or two literals (a single unaligned u16 store) and consumes the summed bit
  count, halving the serial load-shift chain on skewed distributions. The
  table is chosen per literals section with libzstd's size-based cost model
  and only kept when at least ~80% of the code space can pair two codes into
  the 11-bit lookup window (pairing pays off on skewed tables; on
  long-code tables the wider entries and variable advance lose to the plain
  single-symbol loop). skewed ~+15%/+43% (zstd-3/zstd-1), json +1-4%,
  text +0-2%.
* FSE decoding-table construction precomputes per-symbol spread constants
  (slice baselines/strides/bit counts) so the per-entry fill is a counter,
  compare, and multiply-add; the per-entry `highest_bit_set` scans and the
  division move to a once-per-symbol pass.
* The decoder only computes the xxhash checksum when the frame actually carries
  one (`Content_Checksum` flag set), instead of always hashing every drained byte.
* Sequence decoding reworked into a libzstd-style 64-bit backwards bit reader
  with packed single-load FSE tables; sequences decode ~15% faster.
* Sequence execution reserves the whole block's output up front and appends
  without per-sequence capacity checks; ring buffer wraps with a conditional
  subtract instead of a modulo.
* Interleaved Huffman decoding accesses its tables and output through
  unchecked reads/writes; the loop bounds already guarantee they are in range.
* Fix encoder panics on degenerate single-symbol FSE distributions: the table
  keeps the full weight for the lone symbol instead of redistributing to a
  nonexistent second maximum, and trailing zero probabilities no longer read
  past the end of the symbol array when writing the table description.
* FSE sequence encoding switches states through a flat per-symbol transition
  table instead of a linear scan, and literal/match length codes come from
  constant lookup tables for the dense low ranges.
* The matcher is rewritten as a zstd-fast style single-probe hash matcher over
  one contiguous window: newest-wins hash insertion, u64 chunked forward and
  backward match extension, and escalating probe steps on literal runs.
  Roughly 3x faster matching with better ratios on structured data.
* The matcher now emits repcode sequences: matches at the current repeated
  offset are encoded as offset code 1 instead of a full offset, and a raw-block
  fallback rolls the repeated-offset history back to match the decoder.
* A raw-block fallback now also rolls back the reusable Huffman and FSE tables:
  the decoder never sees the discarded block, so a later block must not reference
  entropy tables only introduced by it.
* Matcher parameters retuned: 448 KiB window and a steeper probe-step ramp on
  literal runs. Incompressible and skewed data compress up to twice as fast with
  slightly better ratios.
* Sequence encoding concatenates the three state-transition bit groups and the
  three extra-bit groups into one bit write each, halving the writer calls in
  the per-sequence loop.
* `StreamingDecoder`'s `read` decodes until the caller's buffer can be filled
  instead of stopping at the first collectible byte, batching block decodes
  under large reads.
* The sequence decode loop carries its FSE tables as raw pointers, caches each
  table entry between the symbol read and the state transition, and writes
  sequences through a raw pointer into pre-reserved capacity, cutting spills
  and redundant loads per sequence.
* Sequence decoding dispatches to a BMI2-compiled copy of its loop at runtime
  when the CPU supports it (x86-64 + std), turning the variable bit shifts
  into single-uop shlx/shrx.
* `decode_all` executes blocks straight into the caller's buffer when no
  dictionary is attached, bypassing the ring buffer and its drain copies
  entirely (flat output path). Slice decoding speeds up 13-35% depending on
  shape; the ring-buffer path remains for dictionaries and streaming.
* The corruption smoke example also fuzzes the flat `decode_all` path and no
  longer panics itself when corruption hits the frame magic (a legitimate
  header error).
* The matcher probes the second repeated offset immediately after every
  emitted match (zstd fast's rep_offset2 loop); alternating-period data now
  chains repcode matches with zero literals (json ratio +6%).
* Dictionary-free streaming decode executes blocks into a flat windowed
  buffer (libzstd's outBuff model) instead of the ring buffer: blocks decode
  straight into the buffer, flushes hand out bytes without retaining a
  window, and a full buffer wraps to its start with the previous segment's
  tail serving as the match window. Streaming speeds up 3% (small windows)
  to 125% (8MB windows); on large-window data ruzstd now matches or beats
  the zstd crate's streaming decoder.
* The sequence decode loop reads its bitstream through a pre-shifted window
  kept in a register (two dependent shifts per read instead of a
  consumed-counter shift chain), and the three packed FSE tables live in one
  fixed-slot array addressed through a single base pointer with constant
  offsets, removing the per-iteration table-pointer reloads and bit-container
  memory operands from the loop.
* Sequence decoding and flat-path sequence execution are fused into one loop
  (the libzstd model): each sequence is executed the moment it is decoded
  instead of round-tripping it through the sequence vector. RLE streams now
  decode through a one-state fake table packed like any FSE table, so the
  loop carries no RLE branches at all and the separate RLE/non-RLE decode
  loops collapse into one `SeqDecoder`. Slice decoding of sequence-heavy
  data speeds up 5-10% (json +10% at level 1, where ruzstd now decodes
  faster than the zstd crate's slice decoder).

# After 0.8.3

* Avoid emitting compressed blocks when the compressed payload is not smaller
  than the raw block.
* Fix Dictionary decoding. It should not panic on invalid inputs.
* Make the decode window size limit configurable via `FrameDecoder::set_max_window_size`/`max_window_size`, `StreamingDecoder::new_with_max_window_size`, and the `DEFAULT_MAX_WINDOW_SIZE` constant. The default stays 100mb.
* Apply the window size limit to the first frame of a stream, not just later frames.
* **Breaking** `FrameDecoderError::WindowSizeTooBig` gained a `max` field and now reports the effective limit.

# After 0.8.2
* Introduce the `rust-version` field
* Fix checksum generation when repeatedly using the encoder
* Expose decoding::Dictionary as public
* Add Debug derive to CompressionLevel enum
* Make RLE and Raw block decoding more efficient and not use intermediary buffer on the stack

# After 0.8.1
* The CLI has been refactored to use `clap`
* The MatchDriverGenerator has been made public so users can name it as `M` in `FrameCompressor<R,W,M>`

# After 0.8.0
* The compressor now includes a `content_checksum` when the `hash` feature is enabled
* Dictionary generation has been added

# After 0.7.3
* Add initial compression support
* **Breaking** Refactor modules to reflect that this is now also a compression library

# After 0.7.2
* Soundness fix in decoding::RingBuffer. The lengths of the diferent regions where sometimes calculated wrongly, resulting in reads of heap memory not belonging to that ringbuffer
    * Fixed by https://github.com/paolobarbolini
    * Affected versions: 0.7.0 up to and including 0.7.2

* Added convenience functions to FrameDecoder to decode multiple frames from a buffer (https://github.com/philipc)

# After 0.7.1

* Remove byteorder dependency (https://github.com/workingjubilee)
* Preparations to become a std dependency (https://github.com/workingjubilee)

# After 0.7.0
* Fix for drain_to functions into limited targets (https://github.com/michaelkirk)

# After 0.6.0
* Small fix in the zstd binary, progress tracking was slighty off for skippable frames resulting in an error only when the last frame in a file was skippable
* Small performance improvement by reorganizing code with `#[cold]` annotations
* Documentation for `StreamDecoder` mentioning the limitations around multiple frames (https://github.com/Sorseg)
* Documentation around skippable frames (https://github.com/Sorseg)
* **Breaking** `StreamDecoder` API changes to get access to the inner parts (https://github.com/ifd3f)
* Big internal documentation contribution (https://github.com/zleyyij)
* Dropped derive_more as a dependency (https://github.com/xd009642)
* Small improvement by removing the error cases from the reverse bitreader (and making sure invalid requests can't even happen)

# After 0.5.0
* Make the hashing checksum optional (thanks to [@tamird](https://github.com/tamird))
    * breaking change as the public API changes based on features
* The FrameDecoder is now Send + Sync (RingBuffer impls these traits now)
