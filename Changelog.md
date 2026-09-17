# Changelog

This document records the changes made between versions, starting with version 0.5.0

# After 0.9.0 (Current)


- Encoder: const-generic hash-log instantiation for the fastest tier's
  dense scan body (`start_matching_fast` takes `const HASH_LOG`; the
  dispatch instantiates the row's H15 for dense blocks of full-row inputs,
  everything else keeps the runtime-log body). The dense body's
  register-allocation re-roll lands well: json4 fastest scan Ir
  64.23M→62.04M (−3.4%, whole call −1.8%) at wall parity; the plain
  body's re-roll measured dll4 +1.4% Ir at wall parity, so the plain
  body keeps the runtime-log codegen and dll4's scan Ir stays
  bit-identical. Output byte-identical everywhere (full-ladder dump,
  dll100 l1/l3 emitframes); 199+7+16 tests pass.
- Encoder: text.best's last ratio residue (todo 9b) closed — the btlazy2
  lazy loop parses a frame's cold head at libzstd's exact probe step
  (`LazyStep` in `encoding/btlazy.rs`: `1 + run>>8`, libzstd's
  `kSearchStrength`-8 lazy-family step) instead of the fast/chain miss
  ramp (`1 + miss>>2`), which skipped matchable positions in runs of
  4..255 bytes that a cold head has no indexed history to recover —
  the whole −0.15..−0.23% cell was +1,262 raw literals in the first
  448 KiB. Scoped to the first 4 blocks (512 KiB) of genuine frame
  starts (reset or mt job zero's empty strip), gated by the row-9
  head's ≥48-symbol alphabet bar (json at 39 and skewed at 16 keep the
  ramp — the dense step measured +590/+2,011 B on them); row 9's own
  DUBT-head bridge keeps the ramp. text.best 87,145→86,838 B bulk-st
  (−0.35%, ahead of libzstd-13); the four sweep cells move
  −0.15..−0.23% → +0.12..+0.20%; dll100/dll32/dll16 l13 emitframes
  −4,108 B each; every other cell byte-identical (full-ladder dump +
  120-cell sweep diffed against a baseline build). text.best wall
  x0.94→x0.81-0.83 vs libzstd-13 (bounded head cost, json/skewed in
  band).
- Encoder: pledged stream-mt frames in the mid-size LDM class (chain row,
  Keep verdict, clamped window in [32, 64) MiB, wide-alphabet head) now
  re-grid at the reach verdict onto the bulk capture's schedule:
  reach-based job size (the whole-window stream strip had floored such
  frames at a single job), whole-prefix strips and JobPrefix arming.
  The clamped window sits below the Job bar, so the far class previously
  disarmed and the pledged stream paid a chain-only parse where bulk-mt
  captures it - dll32 pledged stream-mt 5,460,884 -> 4,390,255,
  byte-identical to bulk-mt (mt4 == mt8), restoring the pledged-equals-
  bulk contract on the class. The unpledged path keeps its unclamped
  full-window semantics, which dominate bulk-mt at every dll scale
  (dll16 2,929,187 vs 2,937,032; dll100 20,869,254 vs 23,459,589) -
  the todo-11 "engage the mid-size clamp there" question is closed as
  no. Gates: 120-cell ratio sweep and full-ladder dump byte-identical
  to the prior build; new regression test
  `pledged_midsize_capture_matches_bulk`.



- Encoder: dictionary content now prefill-indexes under its own grid
  instead of the MT strip's geometry (`FillGrid` in
  `match_generator`): fast/dfast take the stride grid plus
  libzstd's `dtlm_full` empty-slot backfill over the whole content,
  chain inserts every position with links (libzstd's
  `ZSTD_insertAndFindFirstIndex` dict load), the tree rows keep the
  lazy whole-content fill, and dict frames walk the chain at
  libzstd's full `1 << searchLog` attempt count (the base table
  halves it as a no-dict speed tuning). Small payloads parse mostly
  against dictionary history, where candidate coverage dominates:
  on the 54-file systemd holdout (0.5-4 KiB, per-file frames,
  summed bytes vs the zstd CLI), -9 with our formatted dict is now
  -0.0% (was +3.7%), with libzstd's formatted dict -2.0% (was
  +1.2%), raw-content controls +1.9-2.1% (was +5.5-5.9%), -1 raw
  +0.3-0.5% (was +3.2-4.1%), -3 unchanged at parity. No-dict output
  untouched: full-ladder dump and 120-cell ratio sweep
  byte-identical; dict cells roundtrip through both decoders.
- Encoder: the package-merge huffman build runs on fixed-capacity array
  stacks pooled in HuffScratch (structural bounds, unchecked hot-loop
  indexing, register-carried lengths) instead of seven working Vecs - the
  Vec push/bookkeeping machinery was ~24K Ir of a json-4K fastest call.
  Output byte-identical (full-ladder dump); small-payload encode a further
  -4.7%/-7.0% Ir on json/text-4K (cumulative -9.4%/-10.2% with the lane
  cut; wall json-4K x1.639->1.553, text-4K x1.837->1.674 vs libzstd).
- Encoder: the sequence-code lane histograms in `choose_tables_fast` are
  pooled with a prefix-only clear — only the 64-entry prefix of each
  256-wide lane is zeroed per block (wire codes never reach 64; `pack_seq`
  debug-asserts the invariant), cutting the 12 KB per-block memset to 3 KB.
  Output byte-identical (full-ladder dump); small-payload encode
  json-4K -5.0% Ir / text-4K -3.5% (wall json-4K x1.671->1.647 vs libzstd).

- Encoder: fixed a P0 where unpledged row-9 (balanced) stream-mt frames past
  the 32 MiB corpus scale were deterministically corrupt, and the same
  trigger deadlocked bulk-mt. The job-start seed and LDM probes compared
  candidates against a stale scan index after the repcode probe advanced the
  position — an offset of exactly one probed the position against itself and
  the store gate priced the zero offset with ilog2(0), panicking the worker.
  The stream encoder's drain assembled the aborted jobs empty and shipped the
  frame (decoded short by whole jobs); the bulk pool's workers all exited on
  poison and the ordered assembly waited forever on unclaimed slots. The
  probes now take strictly-older candidates only (byte-neutral: full-ladder
  dump and 5 shapes x 6 tiers stream-mt8 emitframe plus mt2/mt4/bulk-mt8
  byte-identical against the prior build), the drain surfaces the poison, and
  the bulk assembly resumes the panic instead of hanging. Regression test
  `strip_tail_period_run_roundtrips`.
- Encoder: the unpledged row-9 stream's finish tail now shares one prefix
  fill — output byte-identical everywhere (full-ladder dump, 120-cell sweep,
  emitframe stream gates incl. a 96 MiB probe), text.balanced stream-mt8
  +5.6% (537->568 MiB/s, interleaved medians). The tail jobs' whole-prefix
  strip prefills (nested prefixes, ~84% of the tail's summed job time)
  collapse into one pump-thread build to the median tail boundary,
  segmented at LDM batch-freeze points (the only exactly-resumable cut
  points; the asm gear4 c4 divergence that forces them is recorded in
  docs/dev/negative), with jobs above the median adopting the snapshot
  (`StripSnapshot`) and filling only their remainder. Chain rows only, and
  adopters bounded to prefix strips — the ungated form segfaulted the
  opt/btlazy rows in release and byte-diverged past-overlap tails. Also
  restores the no-default-features build behind the new items' std gates.
- Encoder: the stream-mt worker threads pool across encoders. A worker
  whose encoder drops parks back into a process-global slot pool instead
  of exiting, and the next encoder's first post re-leases it — a slot
  handoff measures ~10 us against the ~130 us the eight-thread
  spawn+join paid per stream (clone plus stack guard-page setup
  dominates), and that cost sat serially on the pump thread. Lease
  semantics: drop still waits every lease out of its queue (the join
  equivalent — the buffer may not move while a lease can touch it), a
  worker whose own job panicked retires exactly as before (never reuse a
  thread that has seen an unwind), a panicked lease machine retires its
  slot so no dropper can wait on a dead thread, and a parked thread
  retires after 5 s idle (nothing pinned in long-running processes); the
  pool and slot mutexes are never held together. The worker state now
  rides the thread across leases (provenance cannot reach the bytes —
  every job clears what it reads). Output byte-identical (emitframe
  stream: 5 shapes x 6 tiers at workers 8 plus json/text
  fastest/fast/balanced at workers 4; full-ladder dump; debug+release
  test suite). Interleaved solo A/B, paired-ratio medians, read path
  mt8: 4 MiB json.fastest +9.8% / text.fastest +13% / json.fast +6.3%,
  8 MiB json.fastest +17% / text.fastest +7.6%, 32 MiB text.fastest
  +3.1% / json.fast +4.3% / json.fastest +1%; slow tiers flat (json.opt
  +0.0%); 1 MiB cells flat by construction (single inline tail job, the
  pool never engages). Matrix enc-stream mt8 text.fastest
  7084-7318 -> 8088 MiB/s, the other fast-tier cells unchanged within
  noise. See docs/src/dev/perf/mt-stream.md.
- Build: the no-default-features (no_std) configuration compiles again
  — it had drifted broken through the r6/r7 encoder rounds without any
  gate noticing (`mt_job_size_for` re-exported without its std gate;
  `f64::log2`/`f64::round` are std-only, now behind core-only shims whose
  std forms delegate unchanged, so verdicts are bit-identical; the
  mt-only items `LdmArming::Job`, the strip helpers and
  `compress_job_blocks_inner` are std-gated like their callers). Also
  drops three probe helpers left dead by the r7 donation iteration.
  Std builds are unchanged (warning count back to the pre-r6 two).
- Encoder: the reach probe's donation now reaches the multithreaded stream
  core (the last stock-probe holdout), output byte-identical (emitframe
  stream gates at workers 8, 60-cell stream sweep, full test suite). At the
  staging gate (pledged: span; open-ended: 8 MiB) the pump posts the
  probe's keep side to a pool worker as job zero's own first span blocks
  and the shrink side to a second worker concurrently — the verdict lands
  on whichever finishes first plus the other, then is consumed lazily
  where the schedule first needs it (a post coming due, or flush/finish),
  so the donation overlaps the pump instead of stalling it. A Keep hands
  job zero the donated state and prefix (the continuation skips the strip
  prefill); a Shrink re-grids undonated as before. The donation's state
  and probe driver round-trip a process-global kit pool: the stream core's
  worker threads are ephemeral per encoder, and cold tables measured 4-5x
  the warm parse cost. text.balanced stream-mt8 366->445 MiB/s
  (x3.28->x2.16 vs zstd-9); json.balanced at ratio parity (x0.33) with the
  discarded keep parse hidden under the pump; the cell's remaining distance
  to the bulk-mt ceiling is the finish-tail strip prefills (the whole-window
  stream strips), not the probe. See docs/src/dev/perf/mt-stream.md.
- Encoder: the stream-mt read path's pump is single-copy and the buffer
  recycling it feeds is wait-free in the steady state. Output
  byte-identical (full-ladder dump, 120-cell ratio sweep, matrix
  enc-stream sizes all equal); interleaved solo A/B vs the previous
  build, 32 MiB mt8 medians: text.fastest 6736->9580, text.fast
  5515->7618, json.fastest 2742->3551, json.fast 2026->2263 MiB/s.
  Matrix enc-stream mt8: json.fastest 2667->3405 (91% of its own bulk
  ceiling), json.fast 1968->2260, text.fastest 6217->7084-7318,
  text.fast 4910->6464. Four scheduling/data-path changes:
  - `read::Encoder` under std reads straight into the mt accumulate
    buffer's spare capacity (`pump_direct`): the region handed to
    `Read::read` is always previously-initialized bytes (an `init_len`
    extent tracks it; the buffer pool carries it across encoders as the
    returned Vec's length, growth clamps it - a realloc copies only the
    live bytes). This halves the pump's serial memcpy; the old
    source->16 KiB chunk->buffer double copy stays as the ST core /
    no-std path (the ST pump hides behind its encode span). Each direct
    read is capped at 1 MiB so job posting stays close behind the
    buffered bytes.
  - The accumulate buffer now doubles at every recycle point up to the
    256 MiB cap: the old grow-only-when-full policy kept the buffer at
    epoch scale, paying a dead-prefix wrap (a live-tail move plus a
    quiesce wait on the in-flight epoch) once or twice per epoch
    forever; a grown buffer fits a whole stream without recycling, so
    the steady state wraps, waits and zero-fills zero times (the pool
    carries the initialized extent, so the fill is a first-use cost).
  - Worker states pool globally: the pool threads are fresh per encoder
    (spawned at its first post, joined at drop), so the per-encoder
    state pool always missed and every stream rebuilt eight matcher
    table sets; workers now loan their state from a depth-capped global
    pool and return it at the shutdown exit.
  - `finish` posts the tail re-slice before draining: the pre-drain
    idled workers behind the last straggler of the previous epoch while
    the tail jobs were already postable (a stream ending exactly on an
    epoch boundary still drains its pending jobs first - regression
    test).
- Encoder: the Balanced row's per-frame fixed costs (reach probe + DUBT
  head) no longer parse anything twice. Three changes, output
  byte-identical across the full ladder dump and the whole 120-cell
  ratio sweep (mt/stream included; every emitframe cross-checked at
  workers 4/8):
  - The probe's shrink side never runs the DUBT head: a shrunk frame is
    chain-selection class by definition, so the measurement and the
    executed parse are the same object on both reach sides (the
    head-in-shrink-parse was the single most expensive item on text:
    18.6 of the 26.7 ms shrink parse).
  - The probe's block loop calls the incompressibility gate the executed
    path uses; it was missing, so the "measurement" re-parsed blocks the
    frame emits verbatim — random paid two full ungated parses per frame
    (the 09-16 random.balanced 2317->1220 MiB/s regression).
  - The probe's keep side is donated: bulk-ST parses the first span
    blocks through the real pipeline while the matcher accumulates the
    cost, bulk-MT hands the same to job zero (a donated continuation
    skips the strip prefill - it would clear the tables the donation
    built), and only a Shrink verdict re-parses. The verdict compares
    the true (feedback-carrying) keep cost against the feedback-free
    shrink cost, calibrated by the feedback gain measured on a cheap
    twice-parsed 512 KiB prefix: json's keep side gains 4.75% from the
    entropy feedback, more than double the shrink margin, and the flat
    comparison the margin is calibrated on must be reconstructed
    (degenerate ties - skewed/random/zeros measure bit-identical costs -
    keep outright). Reach changes also stop reallocating the parse
    tables (sizes are reach-independent; the probe flips the reach three
    times a frame): the flip sites clear their residue explicitly.
  32 MiB matrix: text.balanced bulk-st 499->867 MiB/s (x3.47->x2.0),
  mt8 444->701 (x2.81->x1.81); skewed solo ~1390->1850, random
  951->2076 (regression repaid with interest), zeros ~9.3k->17k,
  dll100 +3% (probe amortizes over 100 MB), json -5% (a shrink-class
  frame pays the donated keep parse it then discards - it is ahead at
  x0.62 mt / x0.9 st either way).
- Encoder: RLE-class frames (uniform/low-alphabet inputs whose parse is
  one uniform scan per block) no longer pay per-frame table fixed costs
  that real shapes amortize. The btlazy2 rows' DUBT tables clear at the
  frame's first searching block instead of at reset (all-RLE/all-raw
  frames never touch them, 48 MiB/frame at level 13);
  `apply_level` re-keys its table reallocation on table lengths rather
  than the whole params struct (the reach probe's Keep/Shrink
  alternation re-zeroed same-sized chain tables on every head parse);
  and the LDM alphabet gate parks the pooled `LdmState` instead of
  dropping it (low-alphabet shapes re-allocated and re-cleared its
  table every frame), with the state's frame-boundary table clear
  likewise deferred to its first fill/generate. Output is
  byte-identical everywhere (full-ladder dump and the 120-cell ratio
  sweep, zeros included). 32 MiB matrix, MiB/s of raw: zeros.balanced
  16,080 -> 41,768, zeros.best 8,515 -> 42,918, zeros.opt 30,874 ->
  41,208, zeros.ultra 29,958 -> 39,633.
- Encoder: dictionary-seeded entropy tables now compete for the block on
  measured bit costs (libzstd's dict paths) instead of losing to the
  between-block reuse heuristics. A per-stream `DictEntropy` flag tracks
  which reusable tables still carry dictionary statistics (cleared as the
  frame installs its own): the huffman side reuses the dict table when
  its stream cost beats a fresh table plus its exact serialized
  description and takes the single-stream literals form below 1 KiB
  (repeat tables cost no jump table); the FSE side runs the lazy+
  three-way cost comparison (predefined vs repeat vs fresh) instead of
  letting the small-block predefined thresholds preempt the repeat mode.
  Every no-dict path keeps the stock selection: full-ladder dump
  byte-identical. End-to-end on the 54-file systemd holdout at -9 with
  our own formatted dict: 5,917 -> 5,413 B vs libzstd's 5,256 (+3.0%,
  was +11.6%; formatted now -5.7% vs our raw-content dict, mirroring
  libzstd's -1.2%).
- Dict trainer: formatted-dictionary emission, the `ZDICT_finalizeDictionary`
  port (`dict/finalize.rs`, bench `train --formatted`): every training
  sample's first block is parsed against the trained content as raw match
  history, the literals and ll/ml/of code streams feed four all-ones-seeded
  histograms, and huffman description + three FSE NCcounts + repcodes
  {1,4,8} serialize in front of the content in libzstd's exact layout
  (content-hash dictID, tail-clipped content budget, flat-lit rescue).
  C's dead code is not ported (the most-common-first-offsets analysis
  never reaches the wire). Interop gated both directions through the
  zstd CLI (54/54 holdout files compress/decode cross); libzstd using our
  finalized dictionary matches its own trained one (5,256 vs 5,258 B at
  -9 on the systemd holdout).
- Encoder: raw literals sections use libzstd's size-format ladder
  (`flSize = 1 + (size>31) + (size>4095)`, mirroring `rle_literals`)
  instead of always writing the 20-bit form — every raw-literals block
  with <=4095 literal bytes shrinks by 1-2 B. This was the whole
  text.Opt/Ultra ratio residue (todo 9): the parse was at parity with
  libzstd (12 differing sequences of 27k, entropy bound 12 B better)
  while the tiled/zero tail's ~130 one-match blocks each carried a +2 B
  header. text opt/ultra bulk-st 82,092->81,832 / 81,333->81,073
  (zstd -17/-19: 81,844/81,079; tier now -0.011/-0.019% vs the crate
  ref), text.best -178 B, dll100 opt/ultra -327/-368 B; 120-cell ratio
  sweep: no cell worse, geo-mean +8.48%; speeds in band (text.opt
  x0.632, text.ultra x0.716); full-ladder dump roundtrip-gated.
- Encoder: the opt tier's MT jobs fill only the tail half of their
  history strip through the tree (libzstd zstdmt's btopt overlap parity,
  `ZSTDMT_overlapLog_default` 8; ultra keeps the full strip like
  btultra2), while the LDM table still ingests the whole strip — measured
  by the new per-job decomposition (`jobdecomp` bench, `job_trace` dev
  feature), the strip tree-fill was 40-73% of summed job time on every
  shape. Ultra's job-boundary seed parse span halved to one block (the
  frame-start depth). Ratio gates: corpus worst json.opt +0.011%,
  text.opt -0.017% (denser), dll100 -0.002%, dll32 -0.04%, dll16 -0.11%,
  st paths byte-identical; the whole-strip-halved form (LDM included) is
  falsified (dll100 +4.13%). Interleaved mt8: json.opt stream 9->13
  MiB/s (+44%), json.opt bulk +36%, dll100.opt bulk +20%, text.ultra
  stream +10%, text.opt stream +3%, json.ultra flat.
- Encoder: the fastest tier's fast scan instantiates a dense-mode body per
  block (`const DENSE` pair alongside the ramp instantiation), selected
  from the previous block's parse (>=512 sequences, <4 literal B/seq, and
  the fed-back literal table covering <32 symbols — the alphabet arm is
  what keeps text/skewed/dll byte-identical: their match-dense blocks
  overlap json's parse-statistic bands). The dense body hosts the
  fed-back-literal-price acceptance bar (dist >= 1024, priced at the
  previous block's Huffman code lengths, declines fall through to the
  next probe) and miss-run stepping (pair advance doubles after one full
  missed pair). json.fastest -3.3..-3.9% size in all four modes at
  559->590 MiB/s interleaved (x1.53->x1.44 vs libzstd-1); json-1M -2.72%
  at 608->630 MiB/s; every other output byte-identical (120-cell sweep,
  full-ladder dump, dll100 emitframes); text.fastest -1.8% wall at
  identical output (plain-body placement in the doubled-code binary).
  Falsified variants in docs/src/dev/negative/matchers.md (stats-only
  gates, step thresholds, the 48-symbol bar).
- Encoder: the dfast scan instantiates const-generic table logs — the
  dispatch keys on the tables' actual lengths and the two full rows
  (17/16, 18/18) compile their hash shifts to immediates, freeing the two
  shift registers; clamped-window inputs keep a runtime-log instantiation.
  Byte-identical output (full-ladder dump); dfast scan instructions -4.9%
  (dll 4MiB slice callgrind, 1.42x -> 1.36x libzstd's
  ZSTD_compressBlock_doubleFast), -4.4% on json; wall interleaved dll100
  fast +3.4%, json fast +3.6%. Two C-parity restructures of the same loop
  (nextStep counter advance, rep-probe delta hoisting) are falsified in
  docs/src/dev/negative/matchers.md: the loop's saturated register budget
  re-rolls allocation on every added live value.
- Streaming MT encode: the burst-barrier model is now a posted-job queue
  (encoder_mt.rs). Jobs post to the persistent pool the moment their epoch
  is fully buffered (per-job re-checked gate — posting an incomplete epoch
  changes the stream-end tail re-slice), workers keep a per-thread
  CompressState across jobs, assembly drains incrementally at post
  cadence, and pledged streams post each bulk-grid job at its own
  completion. Buffer recycling is quiesce-gated wrap+grow (the WIP's
  spare/regen ping-pong carried unmeasured benefit and three lifecycle
  bugs; see negative/mt-stream.md). The read-side pump keeps a persistent
  16 KiB staging chunk. The historical sporadic hang is root-caused (a
  completion-burst race against the drain wait's snapshot — notify fires
  before the waiter parks, the snapshot-delta exit never fires; idle
  machines reproduce it, loaded ones dodge it) and fixed by re-checking
  the quiesce predicate on every wait wake. Interleaved A/B, stream-mt8
  32 MiB: text.fastest +11.5%, text.fast +11%, json.fast +4.7%,
  json.fastest flat (encode-span-bound). Output byte-identical (30/30
  stream-mt ratio cells, pledged ≡ bulk-mt). zstdx-bench gains
  `prof enc-stream-read` (the matrix enc-stream cell shape).
- Encoder: the fastest tier's short-match interior fill is density-gated per
  block (previous block parsed >=64 sequences at >=4 literal bytes per
  sequence -> stride-2 fill instead of dense; the branch lives inside the
  outlined insert_covered). dll100 fastest -0.043% size at +1.1-1.7% wall;
  corpus output byte-identical (full-ladder dump); small-band instructions
  +0.2-0.5% (dormant gate cost).
- Encoder: per-block entropy-table builds recycle their buffers through a
  pool in the block scratch (FSE transition tables, huffman codes,
  package-merge lists, wire weights); a buffer zeroes only its growth tail,
  once per size. Byte-identical output (25-cell emit diff + pooled-call
  regression test); small-payload band: json-4K -13.6% instructions
  (gungraun), wall x1.96->1.83 vs libzstd; text-4K -11.1% / x1.91->1.87;
  8K/16K -4..6% wall; skewed-4K x0.90->0.80; 64K-1M and the 32MiB matrix
  flat to ahead. zstdx-gungraun gains a deterministic small-payload bench
  (4K-64K cells, both implementations).
- Docs/falsification: the fast-tier acceptance bar (todo item 3's last
  open lever). Fed-back literal pricing (the chain store gate's mechanism)
  is shape-safe and wins json.fastest -1.25% size in all four modes with
  text/skewed unharmed; static flat pricing is not shape-safe (text +1.5%
  at 3 bits/B). Hosting the bar in the scan loop is falsified: a dormant
  bar (code present, never taken, byte-identical output) costs text.fastest
  -12% and json.fastest -4% wall; the outlined-cold form does not recover.
  Bar reverted; negative/matchers.md carries the data and the reopening
  condition (a hosting that leaves the loop body byte-for-byte). seqstats
  gains a joint ml x offset-log weak-match anatomy table (the bar's design
  data: json's pollution is ml=6 at 4 KiB-768 KiB distances, 2.5% of
  sequences).
- Encoder: `encode_sequences` carries one output cursor pointer instead of a
  stack-reloaded base plus byte offset (and terminates on the SeqWord
  pointer, dropping a spilled counter). Byte-identical output; fastest/fast
  json -0.35%/-0.42% instructions (gungraun), wall neutral.
- Fast scan: the pair's two hash-match compares share one branch (the rep
  OR-fold's class). Both positions' candidates are resolved and
  pre-compared up front — `m = (cand == ip) | (read4(cand) ^ cur)`, fold
  on `(m0 == 0) | (m1 == 0)` — with the armed-rep entry kept off the fold
  condition (a folded spilled bool put its reload on the hot input path).
  Byte-identical output (full-ladder dump ×30, L1/L2 × ST/MT emitframe);
  mid-size fastest A/Bs: json-256K +9% (x1.53→1.40 vs libzstd-1),
  json-1M +7%, text-256K +4.5%, text-1M +3.3%, 32MiB json.fastest
  x1.502 (was 1.52-1.57), 4K flat; json-64K −5% (the merged branch
  mispredicts in the L1-resident single-block regime — reproduced, the
  one cell traded). Post-fold the scan's branch inventory is flat.
- Bench: `small` gains a `--level` axis (tier names or numeric levels,
  libzstd-paired) and checksum parity with `zstd::bulk::compress` — the
  old timed path paid our checksum + ≥256 KiB sidecar thread against a
  reference that never did (~8-12% of the recorded mid-size "2×").
- Docs: SIMD copy scheduling of the fused sequence loop (todo 2, the last
  unassessed structural idea) is falsified — porting libzstd's wildcopy
  issue structure (head-first copy16 + unrolled landing tail + dead-branch
  removal; three builds) shrinks the exec half by the predicted 0.5-0.6M
  Ir (json) but regresses net Ir and wall on every shape: the loop is one
  register-allocation domain and the saturated budget returns the saving
  as decode-half spills. No code change; the loop stands.
- Encoder-side deep-offset ramp for multithreaded jobs (env-gated
  `ZSTDX_MT_RAMP_BYTES`, off by default): a gated job rejects every match
  whose source lies within D bytes below the job start (DEPTH semantics —
  a piece-parallel decoder's binding constraint is how far below the cut
  a crossing read reaches, not its offset), across all 9 probe/acceptance
  paths plus a piecewise backward-extension floor (in-job sources stop at
  the boundary; already-deep sources extend freely). Suppressed crossing
  matches are weak, so the ratio cost is ~zero where D clears the shape's
  match period: json D=2 MiB -0.72% size, skewed -0.02%, text D=512 KiB
  +1.27% (text at D=2 MiB is +403% — its ~800 KiB period falls inside the
  band). Executor-truth simulation shows zero guarantee violations and
  piece-parallel ceilings of 8.0x (json, piece-count bound), 1.99x
  (skewed), 1.94x (text); ramp frames decode byte-identically at speed
  parity through the current serial-B decoder. Full ratio sweep with the
  gate compiled in (env unset): no regression (+8.43% geo-mean vs
  libzstd, worst cell text.best -0.45% as before).
- Bench tooling for the decode-parallelism work: `emitframe` (one corpus
  file to a zstdx frame, bulk/stream, st/mt, roundtrip-gated, prints the
  exact MT job size), `piecepipe` (piece-pipeline critical-path
  simulation on one frame through a new `seq_dump` executor-truth hook
  recording every match's absolute position and resolved offset; uniform
  or bounded-lane schedules at rate 1, depth-guarantee audit), and
  `files --threads` (MT decode timing on explicit files). The piece
  cutting uses the encoder's real job grid via the hidden
  `zstdx::encoding::mt_job_size_for`.
- Docs: the two open fastest-tier angles (todo 3) are falsified and
  withdrawn — miss-run stepping (and its dense-insert / parse-statistic
  gate variants) wins json on both axes (−3.2% size, +8.8% speed) but
  regresses text's short-match coverage at every setting, and search-side
  batched probing is output-neutral yet +3.8% instructions and −10..14%
  wall (json's miss runs are 1-2 pairs; batching never amortizes). No
  code change; the scan loop stands.
- Best tier (btlazy2) DUBT finder converted to u32 entries like libzstd
  (truncated positions rebuilt against the scanning position), halving
  the strategy's random-access working set from 96 to 48 MiB: the
  skewed.best cell was memory-latency-bound on the doubled footprint
  (~460 cycles per tree visit, 200M L1 + 100M dTLB misses per 32 MiB
  pass). Interleaved matrix: skewed.best x2.865 -> x2.16-2.22, json.best
  x1.627 -> x1.38-1.50, text.best x1.184 -> x0.82-0.94 (ahead of
  libzstd); compressed output byte-identical (full-ladder dump gate).
- btlazy2 selection hoists the per-position literal-cost scale out of the
  candidate loops (all candidates of a search share it; LLVM did not
  hoist the four gathers behind the matches-array loop): driver
  instructions -23% on skewed, output byte-identical.
- Level 9 (Balanced) frames now parse their cold-start head (first
  1.125 MiB) through the DUBT/btlazy2 machinery before the chain scan
  takes over. The chain's newest-first candidate order accepts
  nearer-shorter matches while its tables are still filling, which was
  the entire text.Balanced deficit (localized to the first cold-start
  tile); the tree's oldest-first order closes exactly that span. The
  handoff dense-indexes the head region into the chain tables via the
  existing catch-up fill, so later blocks match into it at full
  resolution. Three gates keep the head where it wins: genuine frame
  starts only (mt job zero's empty strip; dictionary/strip-warm starts
  disarm), declared-or-unknown lengths >= 4 MiB, and >= 48 distinct
  bytes in the first parsed block (small-symbol alphabets collapse the
  tree's batch sort, and the corpus json at 39 distinct bytes keeps the
  chain selection that carries its +19.5% density — both stay
  byte-identical). Corpus A/B: text.balanced -2.49 -> +2.24% vs zstd-9
  (stream-st +2.26; mt cells -2.73 -> +8.8), json/skewed byte-identical
  in all modes; text solo speed 1305 -> ~505 MiB/s (bounded per-frame
  head cost), json/skewed/dll100 speeds unchanged (dll100 -0.07% size).
  Whole-tier DUBT rows were measured and rejected first: the json
  density is the chain selection's, not the strategy class (see
  docs/src/dev/negative.md).
- MT decode checksum verification moved off the serial post-pass into
  stage B: the executor absorbs each executed segment's output range into
  the frame's xxh64 stream the moment it is final (bytes hot in its
  caches; stage A keeps staging ahead during the absorb), finishing and
  comparing at frame closes. Locked 32 MiB matrix dec-mt: json mt4
  1871->2029 and mt16 1996->2200 MiB/s (1.33x/1.41x -> 1.44x/1.56x ours
  ST), skewed mt4 695->756 (1.07x -> 1.16x), text unchanged; frames
  without the checksum flag skip verification entirely, error contract
  unchanged (decode errors win, `decode_to_vec_mt` keeps its length on
  checksum error). Two alternative placements measured and rejected: a
  dedicated concurrent hasher thread (on L3-capacity-bound json the
  concurrent output read stream runs at DRAM speed and slows the decoder:
  wall +33% for identical instructions) and the previous sequential
  post-pass (reads the assembled output cold, up to +7.6 ms on skewed).
- `decode_to_vec_mt` corrupted memory when appending to a non-empty
  vector: the place callback handed out the vector's base (execution
  coordinates are append-relative), reserved only `end - start_len` extra
  bytes (short by `start_len`), and reported the write limit as
  `capacity + start_len`. All callers passed fresh vectors, so the path was
  only reachable through the public API. Now the base is offset past the
  existing contents, the reserve covers the executor-relative `end`, and the
  limit is the remaining capacity; regression test appends behind a 1 MiB
  prefix (caught as heap corruption under MALLOC_CHECK_/valgrind).
- Docs: the streaming-MT "checksum absorb off the calling thread" plan
  (todo 12) is falsified and withdrawn — XXH64 admits no chunk-level
  merge, and pool-pickup / sidecar-thread / fused-pump absorbs all
  measure neutral-to-worse vs the existing fire-time calling-thread
  absorb (which already overlaps the burst span); the fastest-tier
  checksum residue is a resource tax, not a placement tax. No code
  change; the placement stays.
- MT decode staging buffers are pooled globally (256 MiB cap): every
  decode call used to map fresh multi-megabyte staging vectors per
  segment and pay their whole first-touch fault cost again (the encoder
  accumulate-buffer lesson, same machine). Repeat decodes now reuse the
  faulted pages; contents are never observable across uses. 64 MiB solo:
  json.zst3 mt4/mt8 1.47x/1.68x -> 1.5-1.7x/1.74x ST, skewed.zst9
  1.14x -> 1.57x, our-encoder skewed mt4 1.54x -> 2.10x.
- MT decode stage B (staged sequence execution) now uses the flat
  executor's inline wildcopy strategy (16/8-byte chunks, doubling fallback
  near the buffer end) instead of libc calls per doubling chunk: json64
  mt4/mt8 1.10x/1.14x -> 1.65x/1.80x ST, skewed.zst9 0.79x -> 1.13x.
- MT decode: a zero-sequence compressed block mid-segment (our own MT
  encoder emits them on small-alphabet data at balanced levels) aborted
  stage A with `MissingCompressionMode`. Stage A now skips sequence
  decoding for them like the sequential path, keeping the scratch tables
  and rejecting trailing bytes after the header with the same error.
- Frame checksums are now verified on every decode path (libzstd parity;
  previously *no* path compared — the ST decoder computed the hash and
  stored the trailer word but left the comparison to the caller via
  `get_checksum_from_data`/`get_calculated_checksum`, and the MT path did
  not hash at all). Mismatch errors with the new
  `FrameDecoderError::ChecksumMismatch { expected, calculated }`: the ST
  paths fold the compare into the trailer read (folding any not-yet-
  drained ring-buffer bytes first, then disabling further hashing so
  later drains don't double-count), the MT paths post-pass one
  sequential xxh64 per checksummed frame over the assembled output on
  the calling thread (`decoding/frame_checksum.rs`; frame ranges derive
  from the `place` callback's per-segment ends — `decode_parallel`
  itself is untouched). `decode_to_vec_mt` verifies before publishing so
  its length stays unchanged on error, matching `decode_all_to_vec`.
  Multi-frame inputs report the offending frame. Cost on checksummed
  frames (4T, 32MiB): ~1.7 ms — text 25.2->11.5, skewed 4.1->3.5, json
  1.88->1.63 GiB/s; the ST paths already paid the hash per block, so
  their cost is just the compare; frames without the checksum flag are
  untouched.
- MT decode fixed on zero-sequence blocks: `decode_segment` called the
  sequence decoder unconditionally, so a compressed block with nbSeq=0
  (all-literals — 16-symbol skewed data at Fastest emits them as full
  128KiB literal blocks) failed with `MissingCompressionMode` where the
  sequential decoder skipped the sequence section. The guard now mirrors
  the ST path, including its `ExtraBits` report for leftover bytes after
  the bare nbSeq=0 byte. Found while benching checksum overhead on the
  skewed corpus (libzstd and our ST decoder both decoded the same frame).
- btlazy: the depth-0 rep probe priced the empty incumbent through
  `lazy_value`'s MIN_MATCH contract (`{off: 0, len: 0}` sentinel from a
  candidate-less tree search) — `full_ladder_roundtrip` failed its debug
  assert since the DUBT landing while release output was correct (the
  len-0 arithmetic degenerates to exactly the no-candidate baseline 0).
  The sentinel is now priced explicitly; release bytes unchanged.
- Fast-tier heap overread fixed (fuzz-found, 8-byte reproducer): the
  matcher's insert bound `insert_max` is inclusive, but the short-match
  fill in `insert_covered` used it as an exclusive loop end — a match
  starting past it (first blocks < 9 B where the bound saturates to the
  window base, or `rep1_chain` tails within MIN_MATCH of the block end,
  which also reach ordinary inputs) left an "empty" range whose wrapped
  `(end - p)` parity peel hashed 8 bytes past the window. Streaming inputs
  overread silently inside the ring buffer; bulk inputs whose window ends
  at the allocation end are the ASAN-visible case. Output byte-identical
  on every corpus cell (30-cell dump gate), fastest-tier Ir +0.23% for
  the empty-range guard (wall flat), 4-min ASAN fuzz clean.
- Best tier (levels 13-15) re-strategied from a low-config optimal parser
  to a btlazy2 port (`encoding/btlazy.rs`): lazy2 selection (two-deep lazy
  walk with libzstd's alternating margins, literal-aware displaced-literal
  pricing, rep0 probes, backward extension, offset-2 chains) over the
  optimal parser's binary tree, with the fill's compare budget split from
  the search's (`OptKnobs::insert_log`; opt rows keep insert == search and
  are byte-identical). json.best 9 -> 17 MiB/s (x4.22 -> x2.21 vs zstd-13)
  at ratio 6.27 (was 6.75; zstd-13: 6.10), text 416 -> 500 (x1.77 ->
  x1.50) at 378 vs 386, skewed 2 -> 4 (x6.22 -> x3.76) at parity;
  zeros/random unchanged. Levels outside 13-15 verified byte-identical
  (full-ladder dump gate); 120-cell ratio sweep roundtrip-gated through
  both decoders.
- Dictionary trainer reworked as a deterministic fastCover port
  (`dict_builder` no longer pulls fastrand): fixed-seed sample shuffle
  before concatenation (libzstd's `DiB_shuffle` — a sorted body makes
  every epoch a cluster of similar files and the positional 75/25 split a
  distribution shift; worth ~20% dict quality on its own), per-epoch
  sliding-window selection scoring distinct 8-byte dmer frequencies with
  zero-out on selection, epoch wraparound until the size-capped budget is
  spent, and a 9-point segment-size sweep scored by compressing the
  held-out 25% (level 3, libzstd's optimizer protocol). The reservoir
  sampler, per-kmer Karp-Rabin rescans and the epoch-buffer/scoring bugs
  are deleted. systemd fixture (207 sub-2KB samples, 16 KiB dict, referee
  libzstd -9): holdout 6,030 B vs libzstd's trained content 6,085 (−0.9%),
  full-set 23,282 vs 23,833 — the old trainer emitted 44× the requested
  size of a repeated segment (72,437 B full-set) and was nondeterministic.
  With our own encoder our trained dict also beats libzstd's dict by 3.9%
  (26,532 vs 27,603 B); remaining gap is entropy-table emission, not
  content selection.
- Raw content dictionaries on every codec path (libzstd parity): a
  headerless dictionary loads as pure match history with the
  format-default repcodes — on encode `EncDictionary::parse` grows a raw
  branch (entropy-table fields become `Option`, seeding unchanged for
  formatted dicts), on decode `Dictionary::load` replaces the
  `decode_dict` validation call sites and `FrameDecoder::reset` applies a
  registered id-0 dictionary to frames that carry no dictID (raw content,
  or formatted dicts trained with `--dictID=0`). CLI `-D` accepts them
  both directions; libzstd decodes the resulting frames (roundtrip
  verified against `zstd::bulk::Decompressor`).
- zstdx-bench: `train` subcommand — trains a raw-content dictionary from
  files/directories at a given size (enables the zstdx `dict_builder`
  feature for the bench crate), or extracts a formatted dict's content
  section (`--content-of`) so content selection and entropy-table seeding
  can be A/B'd in isolation.
- Balanced matcher: cross-position pipelining of the lazy walk's hash+head
  read (the dfast ip0/ip1 pattern). Every chain search paid one L3-class
  random head load serialized in front of its walk; the scan body now
  issues the pos+1 hash+head *before* the incumbent walk (issue placement
  is load-bearing: after the walk's loop the loads decode behind its
  poorly-predicted exit branch and the shadow evaporates), the insert's
  newest-wins write is fixed up on a slot collision, and inside the walk
  each search consumes the head issued one search back and refreshes for
  the next — no table writes happen inside the walk and every path to the
  next search steps exactly one position, so the refresh (guarded exactly
  by the next attempt's break condition) is never stale and never
  unconsumed. The depth-0 rep probe advancing pos (first search at pos+2)
  and the block tail fall back to fresh compute. Byte-identical output
  (full-ladder dump gate); wall solo: dll100 105-107 to 110-113 MiB/s
  (+4.5%), json 89 to 90, text 3251 to 3295 (6-pair run, +1.4%; interleaved
  matrix text +2.5%), skewed -2% (wasted pre-reads on long-match
  iterations: +18% RAM hits at +0.5% Ir); gungraun balanced Ir json/text
  +2.6%/+2.6% (the issued-early loads are the cost, the hidden latency the
  pay), random/zeros bit-identical.

- Sequence packing: `pack_seq` gains a zero-add-bit fast path. LL codes 0-15
  are ll itself and ML codes 0-31 are ml-3, both carrying zero add bits, so
  `ll < 16 && ml < 35` degenerates the packed word to `bsr` + ors with no
  LUT/META loads and no variable-shift add merge (the `(1<<log)-1` mask is
  built as `0x80000000 >> log; dec`). The band covers 99.997% of ll / 96.5%
  of ml on json.fast and dll-shaped corpora sit deeper still; skewed's
  63.5% ll share costs nothing measurable — branch-misses are exactly
  unchanged. Byte-identical output (full-ladder dump gate); gungraun encode:
  fast json/text −4.5%/−4.2% Ir, fastest −1.7%/−2.1%, balanced
  −0.3%/−0.6%, skewed/random/zeros flat; wall within noise on every
  re-measured cell (the fast tiers on the 32MiB shapes are branch/latency-
  bound, not instruction-bound).

- Dfast emit path: `DfastEmit::emit` moves from `#[inline]` to
  `#[inline(always)]` (the treatment `rep_chain` already documents). LLVM's
  own judgment left the steady-phase scan-loop sites outlined, and the
  eight-argument call — three stack-passed args, rep round-tripping `&mut`
  memory, Vec ptr/len/cap loads through `self`, prologue/epilogue — taxed
  every sequence with ~30% of the emit body: callgrind dll32 put the
  outlined helper at 170 Ir/seq over 1.21M calls, 26% of the whole fast
  encode. Inlined, the seq push and the anchor table writes fold into the
  scan loop like libzstd's inlined storeSeq. Byte-identical output
  (full-ladder dump gate); gungraun encode fast tier: json −8.2%, text
  −7.5%, skewed −1.3%, random/zeros unchanged, and the fastest tier
  bit-for-bit unchanged (`TableEmit::emit` was already `inline(always)`);
  interleaved wall: dll100 335→359 MiB/s (fast x1.47→x1.35 vs libzstd-3),
  json 386→421, text 11055→11616, skewed 184→199; balanced/best/opt within
  noise (their `emit_chain` was already inlined).

- Huffman literal-stream encoder: the scalar 4-symbol batch loop is replaced
  by libzstd `HUF_CStream`'s left-aligned dual-accumulator design — each
  table entry carries the code in the top `nb` bits of a u64 plus `nb` in
  the low nibble, so one load feeds the container shift, the OR and the bit
  counter; two containers alternate (two independent shift/OR chains) and
  flush whole pending windows through one unaligned store each. A BMI2
  runtime dispatch (house pattern from the decode side) compiles the
  variable shifts to shrx, freeing the count from `cl` (the non-BMI2 build
  pays one `mov`+`and` per symbol extra and regresses short-code corpora
  ~5% Ir). Byte-identical output (full-ladder dump gate); callgrind on the
  100MB binary corpus: huff0 stream 88.8M → 50M Ir (libzstd's own HUF path
  is 45.7M — the 2x gap closed), dll fast total −3.7%; gungraun encode
  json −1.1..−3.9%, text −0.4..−2.6% (fast tiers), zeros/random +0.1-0.4%
  (the table's extra 2KB aligned-array init); wall within noise on the
  32MiB shapes (the loop is a ~5%-of-cycles share there).

- Small-literal huffman: the literals encoder's hard >1024-byte cutoff
  (everything below went out raw) is gone — any non-empty literal run now
  attempts huffman with the entropy gate and the encoded-vs-raw comparison
  bounding the cost — and the gate margins model the real fixed cost
  (~160-byte weight description + ~2% stream overhead, was +256B/+8%).
  The cutoff showed as a flat ~35% block tax on small payloads: json-4KiB
  went from +46%/+43%/+45% vs libzstd at -1/-9/-19 to +5.3%/−0.1%/+0.9%;
  dictionary-encoding gap on the sub-2KiB systemd fixture +13.3% →
  +10.3%. Large-corpus outputs move ≤0.14% (the huffman feedback into the
  chain store gate shifts a few match decisions); ladder speeds unchanged
  within drift.

- Dictionary encoding: `EncoderOptions::dictionary` (bulk, streaming,
  compat's `Compressor::with_dictionary`/`set_dictionary` and
  `write::Encoder::with_dictionary`), `FrameCompressor::set_dictionary`,
  and CLI `-D` (both directions). The dictionary's content loads as match
  history through the owned-window matcher (grid prefill + seed detection
  reuse the MT strip machinery; repcodes start from the dictionary's
  offset history), its Huffman/FSE tables seed the first blocks as the
  reusable previous tables (treeless/repeat legal via the declared
  dictID), and the window clamps by src+dict like libzstd. `bulk::
  compress_with` is now fallible (`Result`; an invalid dictionary is the
  error case), and the bulk decode paths no longer silently drop an
  attached dictionary. Decoder-side Huffman tables expose their full
  `code_lengths` (the wire omits the last symbol's weight — building
  encoder tables from raw stored weights produced incomplete sets).
  MT-with-dictionary falls back to single-threaded. Sizes on the real
  `zstd --train` systemd fixture: +13% vs libzstd at -9 (sub-2KB
  fixed-overhead band); all frames roundtrip through libzstd and our
  decoder.

- Forced window log (`EncoderOptions::with_input_shape`,
  `InputShape::with_window_log`, clamped 10..=27): overrides the level's
  row before the length adjustment, as in libzstd's param-override order
  (a known smaller length still clamps it). Plumbed through bulk, MT and
  streaming paths plus the CLI-sized `FrameCompressor`. Landing it exposed
  a spec rule the encoder had been violating on sub-128KiB windows: the
  format caps blocks at the declared window (RFC 8878
  Block_Maximum_Size = min(window, 128K)) — every path now sizes blocks
  through `Matcher::block_size` (default keeps 128K); before the fix
  libzstd rejected our small-window frames ("Data corruption detected"),
  including all-raw ones. Verified: 7 levels x W14-20 matrices plus raw
  and periodic inputs roundtrip through libzstd.

- Source-length parameter adjustment (port of libzstd's
  `ZSTD_adjustCParams`): when the input length is known the level's row
  downsizes — window to the source's log, hash tables to windowLog+1,
  chain/ring cycle to windowLog. The bulk paths pass the exact size, MT
  jobs the whole-frame length, streaming the pledge
  (`Matcher::set_source_hint`, `FrameCompressor::set_size_hint`,
  `compress_to_vec_sized`), the CLI file metadata; unknown sizes keep the
  row. Raw-block frames are exempt (outputs stay hint-independent).
  Adjusted-band A/B vs libzstd (64KiB-4MiB json/text, levels 1/3/9/13/19):
  ratio parity throughout (chain rows -5..-15% denser); 4KiB small-call
  speed 5.96 vs 16.20 ms (L19, incl. process spawn).

- Full 1-22 level ladder: every numeric level now selects its own parameter
  row (`LEVEL_PARAMS`), modeled on libzstd's `clevels.h` large-source table
  and adapted to the implemented strategies — fast rows 1-2, dfast 3-4,
  chain rows 5-12 (greedy/lazy/lazy2 via lazy_depth 0/1/2, min_match 5,
  search depths half libzstd's `1 << searchLog` since our per-probe chain
  walk is dearer), opt rows 13-22 (btopt/btultra/btultra2 knobs per row;
  rows 20-22 widen windows to W24-26 with the ring capped at bt24 and hash
  at H22 as a u64-slot memory guard). Chain-family support added: greedy
  (lazy walk skipped), per-row min_match (repcodes stay legal at 4). Fixed
  a latent bug the ladder exposed: skip_matching/catch_up_insertions used
  the chain-table log as the head-table hash log (invisible while H==C).
  Ladder A/B vs libzstd (32MiB): json chain rows -13..-17% size, rows
  13-15 -9..-10%; text rows 1-8 far denser (window), rows 9-12 +2.6%;
  opt rows ±0.4%. Known inversions mirror libzstd's own (json -1 vs -3).
  Bench A/B pairings moved to same-level comparisons (Balanced↔9,
  Best↔13, Opt↔17). All 22 levels × 5 corpora roundtrip via libzstd CLI.

- `Level` is now a newtype over the numeric libzstd level (0-22) instead of
  a strategy enum: `Level::from_zstd(n)` maps exactly, negative levels clamp
  to 1, and the named tier constants alias representative levels
  (Fastest=1, Fast=3, Balanced=9, Best=13, Opt=17, Ultra=19). Parameter
  selection still maps through the seven tiers in this change (outputs
  byte-identical); the full 1-22 parameter ladder lands separately. The
  compat layer maps level 0 to libzstd's default (3), matching the zstd
  crate instead of the crate-native raw-block level 0.

- Ultra multithreaded job-boundary statistics seeding: each job's first
  parsed block now runs a throwaway parse of the strip tail (two blocks) with
  the window re-based to the span — candidates and fills clamp inside it,
  reproducing the empty-window frame start — purely to seed the opt parser's
  price statistics; the real parse then re-fills the strip and runs. A
  sequential in-order-carry decomposition (json.ultra, 4x8MiB jobs, every
  variant roundtrip-gated) had put the entire boundary loss (~85KiB of
  mt-vs-st) on the per-job `opt_state` reset: BASE-priced parses lock into a
  near-offset/ll0 basin that updateStats reinforces for the whole job
  (stats reset 104KiB vs entropy restart 1.3KiB, rep gate 2.6KiB, tree
  refill 3.1KiB). json.ultra bulk-mt/stream-mt -1.85% -> -0.13% vs
  libzstd-19 mt; full-ladder ST dump byte-identical (frame-start seeding
  path unchanged); mt wall unchanged (json 4.1 MiB/s, best-of-5). Seeding
  the same span with the strip reachable recovers nothing at any span size
  (negative.md).

- Incompressibility gate extended to the table strategies (Fastest/Fast/
  Balanced): the same probe + entropy-bar gate now skips the match search
  for near-random blocks at every level. A gated block opens a `gap_start`
  watermark instead of being indexed; the next scanning block dense-fills
  the gap before its first probe (`catch_up_insertions`, window-clamped),
  so skipped blocks stay match history — a later duplicate still matches
  (unit-tested at all four strategy families). Two sampled-distinct
  micro-screens (reject at <32 distinct after 64 samples, <96 after 512)
  keep never-gating corpora at ~0.1-0.5% instruction overhead. random.fast
  1469→1678 MiB/s (x1.39→1.25), random.balanced 1405→1688 (x1.29→1.09),
  random.fastest 1652→1681 (x1.33→1.32);
  full-ladder dump byte-identical, 120-cell ratio sweep flat. Residual:
  each rebuild re-rolls the fast-tier layout lottery and this build parks
  a ~7% wall loss on text.fastest at +0.1% instructions (see
  docs/src/dev/negative.md).

- Pre-match incompressibility gate for the opt strategies (Best/Opt/Ultra):
  a strided content-tag probe (47-bit content hashes, ~2000 samples per
  block, table shared across the frame) plus an exact-histogram entropy bar
  (≥7.97 bits/byte, Miller-Madow corrected) skip the match search entirely
  and emit the block raw. The opt tree's lazy fill keeps skipped blocks
  available as later match history, and `update_tree` now clamps its
  catch-up fill to the live window (dead positions could neither resolve
  nor thread). random.best 12→1574 MiB/s (libzstd-12 615), random.opt 1573
  vs 14 (112×), random.ultra 1542 vs 6 (257×); full-corpus dump
  byte-identical, 120-cell ratio sweep flat.

- Sequence-section per-sequence cost cuts in `compressed.rs` (json.Fast
  residue, todo 5): the three code histograms of `choose_tables_fast` are
  lane-split (4 sub-histograms per channel, gated at nb_seq ≥ 128), and
  `encode_sequences` precomputes the loop-invariant `code << table_log`
  row-index halves into per-block stack tables, dropping the per-sequence
  variable shifts and shift-register stack reloads. Output byte-identical
  (full-ladder dump gate). Gungraun: json.fast −0.5%, text.fast −0.5%
  instructions; choose_tables_fast cycles 2.83%→2.00% on json.fast.

- Chain-strategy lazy walk gain comparisons are now literal-cost-aware: the
  rep probe and the chain probe price a candidate at its displaced
  literals' fed-back code lengths (first four bytes, clamped to 6 bits —
  uncapped 11-bit prices let cap-priced bytes dominate a whole walk's local
  decisions; clamp sweep on balanced bulk-st: json 4726875/4701990/4697420/
  4725425, text 91243/91232/91200/91140 at none/8/6/5) minus the offset
  highbit, replacing the flat `ml*4` scale; at the default lengths the
  formulas reduce bit-for-bit, and cheap-reject gain caps skip the code
  -length gathers when even the best case cannot beat the incumbent.
  json.balanced 6.66→7.14 (+15.7%→+24.1% ahead of zstd-6), text.balanced
  flat (368.07→367.92, −0.57%→−0.61% vs zstd-6), all other 104 ratio cells
  bit-identical. Cost: the better pricing accepts more walk steps on json —
  +13% matcher instructions, json wall 119→~106 MiB/s (still ×1.72 ahead of
  zstd-6); the same exchange pattern as the lazy walk rework, at a better
  rate.

- Chain-strategy store gate is now literal-cost-aware: the block encoder
  feeds the Huffman code lengths of each block's literal table back to the
  matcher (`Matcher::note_literal_costs`), and the gate stores a match only
  if the literals it displaces price (at those lengths) above the offset's
  `highbit + 7`. The flat 4-bits-per-literal constant traded json against
  text monotonically (json 6.21/6.55, text 368.1/366.3 at margins +4/+7);
  the swing lives in 5-byte matches at 8-32 KiB offsets, whose displaced
  bytes are nearly absent from text's hyper-skewed residual literal stream
  (they price at the 11-bit cap) but common in json's (they price cheap) —
  marginal code length separates what no flat margin could. text.balanced
  366.3→368.1 (deficit vs zstd-6 −1.05%→−0.57%) while json.balanced
  improves further 6.55→6.66; skewed/random/zeros unchanged, speed neutral
  (json 116→120, text 2607→2602 MiB/s). Only the chain levels consume the
  feedback; fast/dfast seed gates keep the static margin.

- Unpledged streaming-MT jobs now grow along the stream (`JobGrid::Growing`):
  the job starting at absolute offset `o` is sized by the shared bulk
  formula against a 4×`o` estimate of the final size, replacing the fixed
  1 MiB floor grid. Boundaries stay a pure function of the absolute offset,
  so the no-flush write-chunking independence contract is preserved (a
  burst-time growth rule was ruled out for breaking it). At 32 MiB/4
  workers an unpledged stream cuts 9 jobs instead of 32 (bulk cuts 8),
  shrinking the per-boundary entropy-restart cost: json.opt stream-mt
  −0.30%, json.ultra −0.28%, text.balanced −0.086% vs the old grid; fewer
  jobs also amortize the per-job prefill/table-clear fixed costs. Pledged
  streams keep the fixed bulk grid and stay byte-identical to bulk MT.

- Chain-strategy lazy walk reworked to libzstd lazy's offset-aware gain
  comparison: lazy positions compete on `ml*4 - highbit(offset)` (rep
  incumbents price 0) instead of raw length, a rep0 probe rides each lazy
  step, and the walk advances while improving with a second-chance probe at
  depth-2 margins (the `ZSTD_lazy`/`ZSTD_lazy2` alternating structure) —
  before, pure-length comparison kept far chain candidates wherever the
  offset exponent paid more than the extra length saved. Balanced json
  ratio 6.08→6.55 (+7.7%, now +13.8% ahead of zstd-6), text 361.4→366.3
  (deficit −2.35%→−1.05%), all bulk/stream/st/mt cells move together;
  sequence count drops (json −11%, longer matches). Speed: json 149→120
  MiB/s (the second-chance search per walk costs ~30% matcher instructions
  on rep-dense shapes), text 3168→3009, skewed/random/zeros faster.

- `package_merge_lengths` per-level package sort removed: packages are
  disjoint adjacent-pair sums of the (weight, node)-sorted previous level,
  so pkg[k+1] >= pkg[k] (w[2k+2] >= w[2k] and w[2k+3] >= w[2k+1]) and ties
  break by strictly increasing arena id — the list is provably already
  sorted, the sort was a no-op. Verified by a 2M-random-vector brute force
  (sortedness debug-assert + identical lengths) and the full-ladder dump
  byte-identical gate. Each of the 10 merge levels per table drops its
  O(n log n) compare sort; gungraun text fastest/fast -0.34% instructions,
  skewed fastest -0.19%, text-4K small-payload +3% wall-clock.

- `package_merge_lengths` scratch rework: level lists collapse from a
  `Vec<Vec<Ent>>` (two allocations per level, 11 levels per table) to two
  swapped reusable buffers - only the previous level is ever read; `Ent`
  shrinks from 12 to 8 bytes by holding the weight as `u32` (block literals
  are capped far below 2^32, so package weights cannot overflow). Identical
  output and tie-breaking; gungraun instruction counts drop on every
  literals-heavy cell (text fastest -0.7%, text fast -1.2%, skewed fastest
  -0.5%), text-4K small-payload encode reaches +8% cumulative with the
  counting sort.

- Huffman literals table build (`build_from_weights`): the weight-ordered
  symbol sort is now a 12-bucket counting sort over stack arrays instead of a
  heap-allocated `Vec` + comparison sort. Package-merge caps weights at 11, so
  bucket-major iteration with symbols scattered in ascending order reproduces
  the previous weight-ascending/symbol-ascending order exactly (encoder output
  stays byte-identical). Removes the per-block allocation and the O(n log n)
  sort from every literals table build; text-4K small-payload encode gains
  ~5% wall-clock and the deterministic instruction counts drop on every
  text-shaped cell (no regressions elsewhere).

- New `zstdx-bench ratio` subcommand: a compression-ratio sweep over every
  ladder level x bulk/streaming x single-/multi-thread (120 cells on the full
  corpus), one deterministic pass per cell with the libzstd reference side,
  running cells concurrently on a rayon pool so the full sweep finishes in
  about a minute. Sizes diff cleanly across runs and builds, every zstdx
  cell is roundtrip-gated through both decoders, and a scale-free geo-mean
  Δ% summary (per level x mode, per mode, per shape, worst/best cells) is
  printed last so `tail` sees the verdict. The crate also gained a Readme
  documenting the usage of every subcommand.

- Fix a decode panic on malformed frames found by fuzzing: the flat
  sequence executor folded the output's virtual base into `vbase_op` with a
  wrapping subtraction, but the cold wrapped-match path re-materialized
  `virt_base` with a checked addition (`attempt to add with overflow` at
  `sequence_execution.rs`), which panics whenever the output pointer
  numerically exceeds the virtual base - the common case on the flat decode
  path. The re-addition now wraps symmetrically; the corrupt offset is then
  rejected by the existing out-of-window checks as an error. The crashing
  input is archived under `zstdx-fuzz/artifacts/decode/` for replay by the
  in-tree regression test.

* `--no-default-features` testing fixes: four stream doc examples imported
  `std::io::{Read, Write}` directly and failed to compile in the no_std io
  configuration (`read_to_end`/`write_all`/`flush` not found); they now import
  the re-exported `zstdx::io` traits, which resolve to `std::io` under the
  `std` feature and to the crate's own trait definitions otherwise.
  `FlatOut::abs_slice` (used only by the checksum `hash` feature) and
  `NT_CLEAR_MIN` (used only by the x86_64+std AVX-512 table clear) gained the
  matching `cfg` gates, removing the dead-code warnings those builds emitted.

- New `zstdx-gungraun` workspace member: deterministic valgrind instruction-count
  benchmarks comparing zstdx against the `zstd` crate on identical 1 MiB corpus
  slices (see the crate's Readme). Three bench binaries - `decode` (libzstd- and
  zstdx-produced frames, bulk path), `encode` (all six ladder levels vs the
  matching libzstd levels) and `stream` (streaming decode and encode) - pair the
  implementations per corpus shape with `compare_by_id`, so every `zc*` benchmark
  prints a `zstdx | libzstd` instruction delta. Setup expressions produce the
  frames and run cross-implementation roundtrip gates outside the measured
  region; counts are machine-independent and immune to CPU drift, complementing
  the wall-clock A/B harness in `zstdx-bench`. Benches only run under valgrind
  (`test = false`, lib `bench = false`), so Windows `cargo test`/`clippy` are
  unaffected; the workspace gains a `[profile.bench]` with `debug = true`,
  `strip = false` for callgrind symbol attribution.

- Docs and CI pass: new `.github/workflows/docs.yml` builds the mdbook (`mdbook build
docs`) and deploys `docs/book` to GitHub Pages on pushes to `master` (+ `workflow_dispatch`),
  mirroring youpipe's docs workflow (OIDC permissions, `pages` concurrency group,
  upload/deploy-pages split). The handbook under `docs/src` is rewritten in accurate
  English - every contiguous paragraph is a single physical line, CJK removed, all
  figures and commit hashes preserved, `book.toml` retitled ("zstdx Development Handbook",
  `language = "en"`). New `dev/comparison.md` contrasts zstdx with official zstd v1.6.0
  across encoder front-end, entropy coding, decoder and systems architecture, splitting
  differences into own-approaches / zstdx-only / gaps with code-verified citations on
  both sides: the optimal parser (`opt.rs` vs `zstd_opt.c`) and the Huffman X1/X2 decode
  fast loops are faithful ports; MT seed offsets, packed `SeqWord` sequence collection,
  AVX-512 encode kernels, package-merge Huffman construction and the restart-point
  parallel decoder are zstdx-only; LDM, block splitting, superblocks and encode-side
  dictionaries remain absent. Benchmark pages (`dev/bench/snapshot.md`, `matrix.md`)
  refreshed from a fresh interleaved-A/B matrix run at this commit (5 shapes, decode
  zst1/3/9, all ST encode tiers, mt8/16, streaming, roundtrip gates on): decode bulk
  wins 10/11 cells, streaming decode still trails on compressible shapes (x1.04-1.36),
  MT encode leads 2-8x with ratio within 0.5% of ST everywhere, and the Best tier is
  now the largest encoder deficit (x2.4-29 slower than zstd while leading ratio).

- Toolchain hygiene pass: root-level `clippy.toml` (msrv 1.89, complexity and
  line-width thresholds), nightly `rustfmt.toml` (crate-granularity import
  merging, `StdExternalCrate` grouping, comment wrapping) and `.tombi.toml`
  (TOML alignment). Workspace-level `[workspace.lints]` turn on clippy `all` +
  `pedantic` for every member crate via `[lints] workspace = true`
  (`zstdx-fuzz` duplicates them inline - separate workspace); the cast family,
  `inline_always`, `unreadable_literal`, `must_use_candidate` /
  `return_self_not_must_use`, `missing_safety_doc` and `similar_names` are
  allowed explicitly as codec-inherent noise. The whole tree is now clippy-
  and rustfmt-clean (~2400 findings fixed or allowed), and the stricter lints
  surfaced real bugs: edition-2018 `assert!`/`panic!` messages whose `{var}`
  placeholders printed literally (7 sites - fixed with explicit args and
  rendered moot by the edition bump), a dead `State::last_index` field, a
  duplicated `#[test]`, and a broken unused-format-string panic. All crates
  move to edition 2024; rust-version goes 1.87 -> 1.89 for the stabilized
  AVX-512 intrinsics, `unsafe fn` bodies now carry explicit `unsafe` blocks,
  and the bench harness's `env::set_var` call is annotated.

- `decoding::StreamingDecoder` decodes concatenated frames and skips
  skippable frames transparently, matching `FrameDecoder::decode_all` and the
  reference decoders (libzstd and the zstd crate were probed as the oracle on
  every stream shape). Trailing bytes that do not start a frame surface as an
  error instead of a silent EOF after the first frame; a zero-content frame no
  longer reads as end of stream. The frame-boundary logic now lives in the
  internal `decoding::frame_source` module, shared with `stream::read::Decoder`.
  Found by the new consistency assertion in the `decode` fuzz target
  (crash-b5593b58, regression test in `src/tests/multi_frame.rs`).

- The FSE encoder's packed transition entries widen their baseline and
  target-index fields from 9 to 12 bits each. Tables with accuracy logs above
  9 - only reachable through the fuzz exports' `round_trip`, which passes
  max_log 22 - silently truncated baselines, so the encoder emitted state
  deltas that did not fit the declared bit width (a debug assertion under
  fuzzing, silent bitstream corruption in release). `build_table_from_counts`
  now clamps accuracy to 12 and `build_table_from_probabilities` asserts the
  packing limit. Found by the `fse` fuzz target (crash-61378f); the
  `roundtrip` unit test now replays the fuzz artifacts again, following their
  move to `crates/zstdx-fuzz`.

- The flat sequence executor's `headroom` instantiation skipped the
  per-sequence budget check `op + ll + ml <= out_end`, trusting the streaming
  buffer's block-maximum reservation. A corrupt sequence section can claim
  more output than the block maximum and march the cursor out of the
  allocation: nightly's `copy_nonoverlapping` precondition check surfaced it
  as an overlapping copy under fuzzing, replays segfaulted. The budget check
  now runs in both instantiations, matching libzstd's per-sequence `oend`
  rejection; `headroom` only selects the unconditional wildcopy strategy. The
  dec-st bench matrix shows no regression from the restored check. Found by
  the `decode` fuzz target (crash-01de01).

- The fuzz targets move from `crates/zstdx/fuzz` to their own
  `crates/zstdx-fuzz` crate (excluded from the workspace, as cargo-fuzz
  requires). `libfuzzer-sys` now comes from crates.io and the reference
  side is the same zstd 0.13 binding the bench crate uses. Encoder coverage
  spans the whole ladder and the streaming path: `encode` picks the level
  from the input and checks both decoders, the new `encode_stream` drives
  the write encoder with input-derived chunk sizes and checks the output
  through both our decoder and the reference (the outputs are only
  byte-identical in a fresh process, see the encoder pitfalls), and `interop` sweeps all
  libzstd levels into both of our decode paths. This replaces a stale
  `interop` target that still used the zstd 0.5 API (and duplicated the
  uncompressed path into its "compressed" helper).

- The benchmark and dev-tool examples leave the library for a new
  `zstdx-bench` workspace crate with subcommands: `matrix` (the full
  cross-matrix, now filterable by `--mode/--shape/--level/--workers/
--mt-workers`), `small`, `files` (budget-based decode of explicit files,
  replacing `bench_corpus` and the fixed-iteration `bench_files`), `prof`
  (merged `dec_prof`/`enc_prof`/`enc_stream_prof`), `dump` (merged
  `dump_comp`/`dump_all_levels`), `corrupt`, `mtcheck`, `seqstats` and
  `prefill`. Timing subcommands take `--budget-ms` (the `BENCH_BUDGET_MS`
  env var still works). The superseded one-off examples (`bench_compare`,
  `bench_encode`, `ab_fast`, `ab_mt`, `compression_ratio`, `stream_cmp`)
  and the criterion bench are dropped, along with the library's
  criterion/rand and the CLI's unused dev-dependencies; a
  `--no-default-features` check no longer recompiles example code over the
  release artifacts.

- The repository is reshaped into a cargo workspace with every crate under
  `crates/`: the library now lives at `crates/zstdx`, the CLI at
  `crates/cli`. The root member list uses the `crates/*` glob so future
  crates need no root-manifest edit.

- The crate is renamed: `ruzstd` is now `zstdx` (CLI: `zstdx-cli`, fuzz:
  `zstdx-fuzz`). All code, manifests, docs and CI copy use the new name;
  historical entries below keep the name they were written under.

- New `docs/` mdBook consolidating the branch's untracked working notes into a
  themed handbook: a status overview, the head-to-head benchmark archive with
  measurement methodology, per-area optimization records (decoding, encoding,
  matchers/levels, mt/streaming), a todo list cross-checked against this
  changelog, the falsified-directions list and per-area pitfall notes. The
  root-level working documents stay in place as archives.

- The streaming encoders accept `workers > 1`: input accumulates in one
  contiguous buffer (the previous burst's window strip followed by the
  unencoded bytes) and once at least one full round of workers' worth of
  job-sized slices is pending, a burst encodes them in parallel through
  the bulk mt path's job machinery, assembling the blocks in order while
  the calling thread absorbs the frame checksum. Jobs are cut on absolute
  job-size boundaries, so without flushes the frame bytes do not depend
  on how the input was written; a flush is the documented exception (it
  re-grids early so the pending bytes become visible). A stream written
  exactly to its pledged size shares the bulk job grid and flags, so its
  output is byte-identical to `bulk::compress_with` at the same worker
  count — the pledged final job is held back for `finish` to emit with
  the last-block flag, which costs that one job's inline encode at the
  end. The job-size formula shared by both mt paths gains a 1 GiB
  ceiling (libzstd's zstdmt caps its job size the same way) so a huge
  pledge cannot scale the streaming burst buffer into memory failure.
  Worker states are pooled on the encoder across bursts (burst threads
  are fresh every time, so the thread-local slice pool never carries
  anything between them); a state whose job panicked is dropped instead
  of pooled. Raw-block levels and single-core processes fall back to the
  single-threaded core, and no_std builds keep rejecting workers > 1
  with `Error::Unsupported`. `FrameEncoderCore` is now an enum
  (`Single`/`Mt`) behind the same interface, so `stream::read` and
  `stream::write` are unchanged. On the 32 MiB corpus the mt8 streaming
  encoder reaches 0.5x-3.5x zstd's multithreaded streaming speed
  (json/text at the fast and best levels, parity at balanced) and
  3-4x our own single-threaded streaming on the json shapes.

- The job-start seed scan is vectorized with AVX-512: 64-candidate blocks
  scanned from the anchor down, eight overlapping 64-byte loads per block
  assembling one occupancy bit per position, taken highest-first — the
  byte-wise walk's exact nearest-first order, so the chosen seed (and the
  output) is unchanged. On strips with no repeat (random-like shapes) the
  scan walks the whole window and drops from ~0.45 ms to ~0.06-0.13 ms
  per job (4-7x); repeating shapes still stop at the first qualifying
  block. An equivalence unit test sweeps planted hits across residue
  classes mod 8, block-grid boundaries, the scalar tail and the agree
  bound against a naive walk. Non-x86-64 and pre-AVX-512 builds keep the
  scalar path, which also lost its redundant 4-byte prefilter (a matching
  u64's low half is that u32).

- Every multithreaded job now starts from cleared head tables: the pooled
  matcher state carries whatever earlier jobs — on this frame or a
  previous one — left, and a leftover entry that decodes into the window
  with matching bytes acts as a legal candidate whose presence depends on
  which worker ran which job, so the frame bytes were not reproducible
  (observed flipping ±0.06% on json.Balanced after the stride-3 grid
  fill; the dense fill it replaced had been masking the same latent
  hazard since the u32 tables dropped the per-frame reset). The clear
  makes a job's candidates a function of its strip and scan alone, as
  libzstd's job path does. The chain link table is left uncleared: a
  chain slot is only read at a candidate position, and candidates arise
  only from the cleared head table or from link values written this job,
  so stale link slots are unreachable. Clears of 4 MiB and above use
  non-temporal stores to skip the ownership read and spare the cache.

- The dfast and chain multithreaded-job strip prefills now fill their
  tables on a stride-3 grid (like the fast strategy and libzstd's
  dictionary-content load for fast/dfast) instead of every position. The
  chain strategy deviates from libzstd's dense dictionary fill on
  purpose: our job strip is the full window (libzstd's job prefix is
  window>>3), where a dense fill costs about half the job's scan time.
  The grid keeps the head table's first hop and links grid positions
  oldest-to-newest; unwritten chain slots read as dead or stale entries
  that the walk's position-domain check already discards. Periodic
  locking never depended on the fill (it rides the job-start seed).
  Interleaved old/new mt8 medians on the 32 MiB corpus: text.fast
  +68%, zeros.fast +110%, json/random/skewed.balanced +5-28%; output
  drift within ±0.13% (mostly improvements).

- The fast, dfast and chain match tables now hold u32 entries: each slot
  stores its absolute position biased by one and truncated to 32 bits
  (zero remains the never-written sentinel), and readers rebuild the
  high bits from the scanning position, unwrapping one 4 GiB cycle when
  the value lands above it. Stale entries from an earlier frame that
  cannot unwrap die on the spot; the rest is disposed of by the
  window-range check and byte compare already behind every probe. This
  halves the tables' working set (16 MiB to 8 MiB at Balanced, matching
  libzstd) and the bytes every multithreaded job prefill writes, and
  retires the per-frame epoch machinery for these strategies (the opt
  parser keeps its epoch-tagged u64 tables in dedicated fields). Output
  sizes are byte-identical across the whole corpus matrix; medians of
  interleaved old/new solo runs on the 32 MiB corpus: json.balanced
  +18%, text.balanced +20%, skewed.balanced +9%, random.balanced +4%,
  skewed.fast +5%; Fastest levels, zeros and the opt strategies
  unchanged.

- The dfast miss step is now stateless: instead of a per-match
  step/next-step counter pair bumped every 256 skipped positions, the
  probe pair advances by `1 + (distance since the last match) >> 8` —
  the same one-step-per-256-bytes growth grid with no branch and two
  fewer live values, and the cross-block `miss_count` reset the loop
  never read is gone. The fast and chain loops keep their
  miss-count-driven step on purpose: their per-probe cost and table
  density differ from libzstd's anchor-distance grid, where porting
  the formula was measured as a size and speed regression. Fast level
  on the 32 MiB corpus, medians of three interleaved old/new runs:
  json +1.7%, skewed +0.8%, text +2.6%, random and zeros unchanged;
  output sizes byte-identical everywhere.

- The dfast backfill insert — indexing the prepared second probe
  position after a match was emitted — was gated on `step < 4`, a
  proxy for "the match covered the probe position" that misses every
  long match found once the miss step has grown past 3: those matches
  cover the probe position but skipped the insert, leaving covered
  positions unindexed. The gate is now the exact predicate (the emit
  moved the anchor past the probe position), which no longer leans on
  the assumption that match length bounds the step.

- Multithreaded compression now holds the single-threaded ratio. The
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

- New levels `Level::Opt` (≈zstd 16-17, btopt) and `Level::Ultra`
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

- The literals Huffman table now gets optimal length-limited code
  lengths (boundary package-merge, Larmore-Hirschberg) instead of the
  rank-only weight ladder: the old scheme depended on count order
  but not magnitude, losing heavily on skewed literal histograms.
  Every level benefits; 32 MiB corpus ratios before -> after:
  json fastest 6.05 -> 6.20, json ultra 7.33 -> 7.52, text fastest
  196.6 -> 206.0, text best 242.0 -> 245.5.

- `Level::Best` (≈zstd 10-15) now runs the optimal parser in its
  cheapest setting (16 tree compares, targetLength 32) instead of a
  depth-24 hash-chain search: the chain matcher could not reach the
  tier's ratio at any depth. json Best 5.77 -> 6.88 (zstd-12: 6.14),
  text Best 245.5 -> 270.8 (zstd-12: 256.1), skewed Best 1.87 ->
  1.996, at 12-19 MiB/s depending on shape.

- The chain search now beat-checks candidates (libzstd's "potentially
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

- The chain strategies' table links diverged from their walks: inserts
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

- A match source below the active segment (a wrapped-away previous-segment
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

- The fused sequence decoder carries the three FSE states instead of the
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

- `overlap_copy8` computed its post-spread source as `s2 + (8 - dec64)`
  in usize; for spread offsets 5-7 the adjustment is negative and the
  subtraction underflowed — a debug-build panic (12 corpus tests fail)
  and a wrapped-but-accidentally-correct pointer in release. The table
  is now the signed `8 - dec64` applied via `offset`, matching libzstd's
  `*ip -= dec64table[offset]` directly.
- The fused sequence loop's bitstream reads and repcode resolution lost
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
- New `bench_matrix` example: the head-to-head comparison widened to the
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

- The sequential fallback of `decode_to_vec_mt` retries with doubling
  capacity like `bulk::decompress`, honoring the same append contract as
  the parallel path for a fresh output vec (a plain
  `FrameDecoder::decode_all_to_vec` errors with `TargetTooSmall` on a
  zero-capacity vec because the frame size cannot fit). The doubling is
  tracked explicitly: re-reserving the current capacity is a no-op once
  spare capacity covers it, which would spin. Adds a regression test that
  multithreaded-decodes libzstd single-thread output of semi-structured
  records (huffman literals in back-to-back blocks, the shape whose
  accumulated-buffer staging the old check rejected) at levels 1/3/9.

- `decompress_literals` validates the number of literals it appended
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

- The interleaved 4-stream huffman fast loops (X1 and X2) keep their per
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

- The async checksum worker spins under a bounded budget (~300 µs) and then
  parks on a condvar instead of burning a core for the whole process
  lifetime. The head flip is published under the wake mutex so a parked
  worker cannot miss a post; the budget sits above the cadence of a
  worker-saturated pipeline (a 128 KiB raw block hashes in 50-100 µs), so
  the engaging payloads never pay a wake — verified by the worker's
  voluntary context switches staying at ~1 per frame during a saturated
  run and zero CPU during idle stretches. Throughput on the corpus is
  unchanged within noise (json.Fast 367 MiB/s, random.Fast interleaved A/B
  inside machine-drift variance).

- The fused flat decode loop addresses active-segment match sources as a
  plain `dst - offset` pointer (virtual distances are physical distances
  inside the linear active segment — the buffer-base indirection cancels),
  and moves the wrapped-history copy — sources at or below the segment
  boundary — into a cold outlined function, dropping the four-segment
  mapping state from the hot loop's live set. The per-sequence bounds
  checks merge into one budget: `w + ll + ml` against the target plus one
  `end + 16 <= out_len` gate shared by both wildcopy overshoots. Streaming
  decode on the 32 MiB corpus: json.zst1 1656 → ~1690 MiB/s (+2%),
  skewed.zst3 1183 → ~1220 (+3%), other shapes unchanged within noise.

- `do_offset_history` resolves and updates the repcode history from a
  single slot index (`code - 1 + ll0`, where slot 3 is the `rep0 - 1`
  pseudo-slot folded onto `scratch[0]`), replacing the two chained match
  trees with one branch; behavior is byte-for-byte identical (exhaustively
  tested against the old implementation). Throughput is neutral within
  noise on the corpus; the win is fewer instructions per sequence on
  repcode-dense streams.

- The single-table emit helpers (`emit_seq`, `emit_seq_chain`,
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

- The Fastest level's scan loop resolves probe validity through a select
  (libzstd's selectAddr trick) instead of a three-comparison chain: stale
  hash-table entries alias the scanning position itself, and the byte
  compare plus one `cand != ip` branch rejects them. The repcode
  pre-probe folds its checked subtraction and window-base comparison into
  a single `probe >= win_base + rep[0]` bound computed once per scan
  iteration. Output is byte-identical at every level; interleaved A/B
  against zstd -1 on the 32 MiB corpus: json 455 → 466 MiB/s
  (1.90× → 1.85×), skewed 2724 → 2915 MiB/s, text ~1% faster.

- The Balanced level pins its chain-table log to its window (W20 + C20 +
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

- The matcher's per-sequence emit path pays one buffer push instead of
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

- The Fast level now uses a port of libzstd's double-fast (dfast) matcher:
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

- The flat decoder's sequence executor copies literals and matches with
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

- The chain levels grow their probe step on long literal runs (the fast
  strategy's miss-acceleration policy), so incompressible data no longer
  pays a full chain walk per byte: random 32 MiB compresses at ~2.1 GiB/s
  on every level, with `Best` faster than libzstd's level 12 (2118 vs
  918 MiB/s) at identical (stored) ratios.

- Compression levels beyond `Fastest`: `Level::Fast` (≈ zstd 3-5), `Level::Balanced`
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

- Parallel decoding of complete in-memory inputs: `bulk::decompress_with`
  and `bulk::decompress_to_buffer_with` with `DecoderOptions::threads(n)`
  engage a segment-parallel decoder on std builds. A pre-scan walks the
  block headers and splits the input at _restart points_ — blocks whose
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

- Multithreaded one-shot compression: `bulk::compress_with(source,
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

- The `dict_builder` feature's raw-dictionary builder module moved from
  `ruzstd::dictionary` to `ruzstd::dict`, matching the zstd crate's naming
  and the new top-level module layout (`decoding::Dictionary` stays where
  it is; it parses dictionaries rather than building them).

- The benchmark examples now measure through a shared interleaved A/B
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

- A zstd-crate compatibility layer at `ruzstd::compat` (std builds): the
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

- Streaming decoders: `ruzstd::stream::read::Decoder` decompresses while
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

- Streaming encoders: `ruzstd::stream::write::Encoder` (io::Write in,
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

- New high-level one-shot API: `ruzstd::{compress, decompress}` and
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

- All hand-written `Display`/`From`/`std::error::Error` impls in
  `decoding::errors` are now derived with thiserror (the crate's first
  external dependency; compile-time only, and `default-features = false`
  keeps the no_std and `rustc-dep-of-std` builds working — without `std` the
  derive emits `core::error::Error` impls instead). Variant names, payloads
  and message texts are byte-identical to the previous impls; the file drops
  from 1163 to ~330 lines. `io_nostd::Error` and `GetBitsError` now implement
  `core::error::Error` unconditionally (std re-exports the same trait) so they
  can stay error-chain sources under no_std.

- `CompressionLevel` is replaced by a root `Level` enum
  (`ruzstd::Level::{Uncompressed, Fastest}`, `#[non_exhaustive]`,
  `Level::DEFAULT = Fastest`). The `Default`/`Better`/`Best` variants never had
  implementations and panicked at runtime when reached, so they are gone;
  further variants arrive as their strategies land. The CLI previously
  defaulted to the panicking `Default` variant (level 2) and now maps 1..=4 to
  `Fastest` with `Fastest` as the default. Codegen is unchanged except that
  the per-block level dispatch loses its dead `unimplemented!()` arm (verified
  by instruction-stream diff of the release assembly: 249 global symbols
  compared, only `compress_slice_to_vec` differs, at exactly that dispatch).

- The decoder now verifies frame checksums with the same in-tree XXH64 as
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

- Sequence FSE tables can now be repeated across blocks (mode 3): when the
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

- The slice path (std + hash, input >= 256 KiB, >= 2 usable CPUs) offloads
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

- The sequence bitstream encoder appends each sequence's add-bit payload
  with its state-transition bits in one accumulator push when the combined
  width fits (the common case), halving the per-sequence flush checks.
  json drops 0.4% instructions; wider pairs fall back to the two-push path
  and the emitted bits are identical everywhere.

- The matcher now emits sequences straight into the packed streams the
  sequence-section encoder consumes (`Matcher::start_matching_codes`, a new
  default trait method): each match computes its literal-length/match-length/
  offset codes and merged add-bits payload once, at the emit where the raw
  values are hot, instead of pushing a 12-byte (ll, ml, of) triple that the
  block encoder later re-read and re-encoded in a separate pass. The raw
  triple only survives in the public callback API, reconstructed from the
  packed form. json drops 3.5% instructions at 32 MiB (5.85G -> 5.65G, ~+2%
  throughput), text/random ~0.7%, output bytes identical.

- The frame checksum moved in-tree (spec-exact XXH64 with two 32-byte
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

- Zero-sequence blocks no longer stage their literals in the block scratch:
  the matcher skips the whole-block copy and the block encoder reads the
  bytes straight from the window (the callback path hands the window slice
  over as the trailing `Literals`). Blocks that compress to nothing - every
  random or skewed block - drop one full write+read pass; output bytes are
  unchanged. random gains ~13% at 64 KiB-1 MiB payloads in a clean process
  (7061 -> 8013, 8210 -> 9332 MiB/s), skewed ~4% at 32 MiB (2473 -> 2560
  MiB/s).

- `compress_slice_to_vec` pools its encoder state in a thread-local (hash
  table, default FSE tables, block scratch): per-call rebuilds of those
  dominated small inputs. 1 KiB payloads compress 3-7x faster (json 152 ->
  444, random 268 -> 1938 MiB/s), 4 KiB gains 30-80%; large inputs are
  unchanged. A new `bench_small` example tracks 1 KiB-1 MiB payloads
  against the zstd crate's bulk path.

- Literal blocks whose alphabet stays within sixteen symbols histogram
  through an AVX-512 kernel: sixteen per-slot byte compares with popcount
  accumulation over four 64-byte chunks at a time, entered once the slot
  set is established and left for the four-lane scalar pass whenever a
  seventeenth symbol appears (the popcount sum doubles as the coverage
  test). skewed executes 18% fewer instructions and gains ~28% throughput
  (1917 -> 2455 MiB/s); wider alphabets bail on the first offending chunk,
  costing one chunk per block. Counts are exact, so block bytes are
  unchanged.

- Near-incompressible literal blocks reject through a strided entropy
  sample (1024 draws, Miller-Madow corrected, distinct-symbol prescreen)
  before the exact four-lane histogram runs, with a sticky per-stream hint
  that skips the sample once a block clears the exact bound. random
  executes 43% fewer instructions and gains ~43% throughput (1450 ->
  2060 MiB/s); other shapes are within measurement noise. The reject floor
  sits ~0.2 bits/byte left of the exact bound's, so only literals within
  ~2% of raw size can encode slightly larger; the benchmark corpora are
  byte-identical.

- The `--no-default-features` build compiles again: the literals entropy
  precheck used `f64::log2`, which is std-only; no_std builds now use a
  linear-mantissa approximation (error < 0.086 against the 8% reject
  margin). std builds are unchanged.

- Flat four-bit huffman streams (uniform alphabets of 9..16 symbols, e.g.
  low-cardinality columns) pack through an AVX-512VBMI kernel: one 64-symbol
  chunk resolves its 256-entry code LUT with two byte permutes, reverses
  and pair-packs nibbles with two more, replacing sixteen scalar LUT loads
  per sixteen symbols. skewed executes 48% fewer instructions and gains
  ~43% throughput; the scalar loop remains for sub-64-symbol tails and
  non-x86/no-std builds, and the output is bit-identical.

- New `compress_slice_to_vec` entry point compresses an in-memory buffer
  with no intermediate copies: the matcher window borrows the input
  directly (eliminating the read pass, window compaction and per-call
  window allocation of the streaming path) and blocks append straight into
  the output vector, which is sized up front. Output is byte-identical to
  the streaming path, including its exact-block-multiple trailing empty
  block. text gains ~40% and zeros ~60% throughput, random ~12%; the
  matcher-bound shapes are unchanged.

- Long matches index only two anchors (start+2, end-2) in the hash table
  instead of every fourth position plus the final byte, mirroring zstd's
  fast-strategy fill policy; short matches (<= 16 bytes) keep their dense
  indexing. The 4-byte grid across long matches dominated encoder time on
  highly repetitive data: text executes 31% fewer instructions and gains
  32% throughput while its ratio improves (295.0 -> 298.6), json gains 5%
  with its ratio nearly unchanged (6.02 -> 5.99).

- The scan loop's hash-table probes read their slots unchecked as well: the
  hash masks to the table's power-of-two size, so the per-probe bounds
  checks were provably dead (the insertion side already dropped its check).
  json executes another 2% fewer instructions and text gains 4% throughput
  with no corpus regressing in either A/B order; output stays bit-identical.

- The matcher's index insertion stores its hash-table slot unchecked: the
  hash already masks to the table's power-of-two size, so the bounds check
  on every inserted position was provably dead. json executes 2.3% and text
  5.4% fewer instructions (text's long matches pay the most insertions);
  output stays bit-identical.

- Compressed blocks encode straight into the frame output: the block writer
  reserves the three-byte header, encodes the content in place and patches
  the header once the compressed size is known, removing the per-block
  staging vector and its full-content copy on adoption. The per-block
  literals, sequence and precomputed-code buffers moved into pooled
  compressor state (`BlockScratch`), so steady-state blocks run without the
  allocate-and-double chain. random executes 6.8% fewer instructions (raw
  fallback blocks used to copy their whole content), json and skewed gain
  2-3% each; output stays bit-identical.

- The sequence bitstream encoder keeps its bit accumulator in locals behind
  a small hot-push helper (one unaligned u64 store per flush instead of two
  writer-method round-trips per sequence) and reads the FSE transition rows
  through a flat unchecked `code << log | state` index — the row stride is a
  power-of-two shift, not a runtime multiply, and codes/states cannot leave
  the table by construction. json executes 1.4% fewer instructions and gains
  ~3% throughput; output stays bit-identical.

- The literals histogram fills four sub-histograms keyed by position mod 4
  and merges them once per block, so concurrent increments land in
  different cache lines instead of serializing on same-counter store
  forwarding (small alphabets hit the same counters constantly).
  Instruction count is unchanged; skewed drops 7% of its cycles and gains
  ~10% throughput. Output stays bit-identical.

- Flat huffman tables (every symbol sharing one code length, e.g. the
  9..16-symbol alphabets of uniform data) take a dedicated bulk encoder
  path: after byte-aligning the pending bits it packs two four-bit codes
  per output byte straight into the destination, replacing the
  variable-length accumulation chain. skewed gains 53% throughput
  (47% fewer instructions); other corpora are untouched and output stays
  bit-identical.

- The three sequence-code histograms (literal length, match length, offset)
  fill in a single pass over the packed codes instead of one pass per
  table; the per-table mode decision moved into a shared helper. json
  executes 0.7% fewer instructions; output is bit-identical.

- Huffman stream encoding batches four symbols between bit-container
  flushes (one unaligned u64 store per four symbols instead of a
  container-overflow branch per symbol), driven by a packed
  `(code << 4) | num_bits` u16 code table that keeps the whole table in
  one cache line pair. skewed gains 71% and json 4% throughput; output is
  bit-identical.

- The literals histogram is computed once and shared between the entropy
  precheck and the huffman table build (both used to scan the full
  literals buffer separately). skewed gains 17% throughput; output is
  bit-identical.

- The scan loop interleaves two adjacent positions (libzstd's ip0/ip1
  pipeline): hashes and table entries for both are prepared before either
  is probed, overlapping the hash multiply and table load latencies, and a
  fully-missed pair advances by twice the miss step so probe density on
  incompressible data is unchanged. json gains 2% throughput and 6%
  ratio (5.67 -> 6.02, near zstd -1's 6.11) because the pair-step skips
  over short-match starts the way the ml>=6 gate does; text gains 2%
  throughput at -1.3% ratio; other corpora are unchanged.

- The bit writer's 64-bit flush stores one unaligned u64 into the reserved
  output vector instead of calling memcpy for eight bytes, removing a call
  per flushed container from every entropy-coded block. Output is
  bit-identical; json/text/skewed gain 1-2% each.

- The uniform-block detector compares four u64 words per branch instead of
  one, so fully-uniform blocks (zero-filled inputs, padded corpus tails)
  stop paying a branchy scan of the whole block while non-uniform blocks
  still exit after the first batch. zeros +7%, text +1%, other corpora
  neutral.

- Encoder round eight: sequence codes and their add-bit payloads are
  precomputed in a single pass over the sequences (codes packed into one
  u32 stream, add bits pre-merged into one u64 per sequence), replacing the
  three separate code arrays, the per-sequence out-of-line encoder-helper
  calls, and the metadata re-lookups in the bitstream encoder. json +3%
  throughput, all other corpora neutral, output bit-identical.

- Encoder round seven, data-path focused: the match window holds two windows
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
- Sequence codes are computed once per block and shared between the FSE
  table selection and the bitstream encoder, uniform literal sections
  encode with the one-byte RLE literals mode, and a cheap entropy-bound
  check skips Huffman attempts on near-incompressible literals instead of
  encoding them and discarding the result (random-bytes encode 2.4x
  faster, ratios unchanged).
- The match window widens from 448 KiB to 768 KiB so repository-tile-sized
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
- RLE block detection compares 8 bytes at a time (libzstd `ZSTD_isRLE`
  style) instead of a per-byte closure over an indexed first element, and
  skipped (RLE) blocks index only their first position instead of every
  byte — a uniform run hashes to one table slot, so per-byte indexing just
  rewrote it. zeros encode 7.3x faster.
- The encoder's block emit path is reworked to keep the hot scan loop free
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
- The encoder's match emitter indexes covered matches sparsely instead of
  hashing every byte: matches up to 16 bytes keep every position (they carry
  most of the alignment coverage on structured data), longer matches fall
  back to a 4-byte grid anchored at the match start plus the final byte.
  json +6% / text +5% encode throughput at -0.2% / -0.6% ratio.
- Sequence decoding extracts each group of bitstream reads (the three
  add-bit fields and the three FSE state transitions) with a single window
  read whose bits are then split in parallel, instead of six serial
  load-shift-store chains per sequence. The mid-sequence reload guard only
  applies to the rare wide-field path (all three add-bit widths sum above
  31). skewed +5-8% (zstd-3/zstd-9), json.zst3 +3-5%, text.zst1 +2-3%,
  rest neutral.
- Huffman literals decoding gains a double-symbol (X2) table for the
  interleaved 4-stream fast path, ported from libzstd: each lookup emits one
  or two literals (a single unaligned u16 store) and consumes the summed bit
  count, halving the serial load-shift chain on skewed distributions. The
  table is chosen per literals section with libzstd's size-based cost model
  and only kept when at least ~80% of the code space can pair two codes into
  the 11-bit lookup window (pairing pays off on skewed tables; on
  long-code tables the wider entries and variable advance lose to the plain
  single-symbol loop). skewed ~+15%/+43% (zstd-3/zstd-1), json +1-4%,
  text +0-2%.
- FSE decoding-table construction precomputes per-symbol spread constants
  (slice baselines/strides/bit counts) so the per-entry fill is a counter,
  compare, and multiply-add; the per-entry `highest_bit_set` scans and the
  division move to a once-per-symbol pass.
- The decoder only computes the xxhash checksum when the frame actually carries
  one (`Content_Checksum` flag set), instead of always hashing every drained byte.
- Sequence decoding reworked into a libzstd-style 64-bit backwards bit reader
  with packed single-load FSE tables; sequences decode ~15% faster.
- Sequence execution reserves the whole block's output up front and appends
  without per-sequence capacity checks; ring buffer wraps with a conditional
  subtract instead of a modulo.
- Interleaved Huffman decoding accesses its tables and output through
  unchecked reads/writes; the loop bounds already guarantee they are in range.
- Fix encoder panics on degenerate single-symbol FSE distributions: the table
  keeps the full weight for the lone symbol instead of redistributing to a
  nonexistent second maximum, and trailing zero probabilities no longer read
  past the end of the symbol array when writing the table description.
- FSE sequence encoding switches states through a flat per-symbol transition
  table instead of a linear scan, and literal/match length codes come from
  constant lookup tables for the dense low ranges.
- The matcher is rewritten as a zstd-fast style single-probe hash matcher over
  one contiguous window: newest-wins hash insertion, u64 chunked forward and
  backward match extension, and escalating probe steps on literal runs.
  Roughly 3x faster matching with better ratios on structured data.
- The matcher now emits repcode sequences: matches at the current repeated
  offset are encoded as offset code 1 instead of a full offset, and a raw-block
  fallback rolls the repeated-offset history back to match the decoder.
- A raw-block fallback now also rolls back the reusable Huffman and FSE tables:
  the decoder never sees the discarded block, so a later block must not reference
  entropy tables only introduced by it.
- Matcher parameters retuned: 448 KiB window and a steeper probe-step ramp on
  literal runs. Incompressible and skewed data compress up to twice as fast with
  slightly better ratios.
- Sequence encoding concatenates the three state-transition bit groups and the
  three extra-bit groups into one bit write each, halving the writer calls in
  the per-sequence loop.
- `StreamingDecoder`'s `read` decodes until the caller's buffer can be filled
  instead of stopping at the first collectible byte, batching block decodes
  under large reads.
- The sequence decode loop carries its FSE tables as raw pointers, caches each
  table entry between the symbol read and the state transition, and writes
  sequences through a raw pointer into pre-reserved capacity, cutting spills
  and redundant loads per sequence.
- Sequence decoding dispatches to a BMI2-compiled copy of its loop at runtime
  when the CPU supports it (x86-64 + std), turning the variable bit shifts
  into single-uop shlx/shrx.
- `decode_all` executes blocks straight into the caller's buffer when no
  dictionary is attached, bypassing the ring buffer and its drain copies
  entirely (flat output path). Slice decoding speeds up 13-35% depending on
  shape; the ring-buffer path remains for dictionaries and streaming.
- The corruption smoke example also fuzzes the flat `decode_all` path and no
  longer panics itself when corruption hits the frame magic (a legitimate
  header error).
- The matcher probes the second repeated offset immediately after every
  emitted match (zstd fast's rep_offset2 loop); alternating-period data now
  chains repcode matches with zero literals (json ratio +6%).
- Dictionary-free streaming decode executes blocks into a flat windowed
  buffer (libzstd's outBuff model) instead of the ring buffer: blocks decode
  straight into the buffer, flushes hand out bytes without retaining a
  window, and a full buffer wraps to its start with the previous segment's
  tail serving as the match window. Streaming speeds up 3% (small windows)
  to 125% (8MB windows); on large-window data ruzstd now matches or beats
  the zstd crate's streaming decoder.
- The sequence decode loop reads its bitstream through a pre-shifted window
  kept in a register (two dependent shifts per read instead of a
  consumed-counter shift chain), and the three packed FSE tables live in one
  fixed-slot array addressed through a single base pointer with constant
  offsets, removing the per-iteration table-pointer reloads and bit-container
  memory operands from the loop.
- Sequence decoding and flat-path sequence execution are fused into one loop
  (the libzstd model): each sequence is executed the moment it is decoded
  instead of round-tripping it through the sequence vector. RLE streams now
  decode through a one-state fake table packed like any FSE table, so the
  loop carries no RLE branches at all and the separate RLE/non-RLE decode
  loops collapse into one `SeqDecoder`. Slice decoding of sequence-heavy
  data speeds up 5-10% (json +10% at level 1, where ruzstd now decodes
  faster than the zstd crate's slice decoder).

# After 0.8.3

- Avoid emitting compressed blocks when the compressed payload is not smaller
  than the raw block.
- Fix Dictionary decoding. It should not panic on invalid inputs.
- Make the decode window size limit configurable via `FrameDecoder::set_max_window_size`/`max_window_size`, `StreamingDecoder::new_with_max_window_size`, and the `DEFAULT_MAX_WINDOW_SIZE` constant. The default stays 100mb.
- Apply the window size limit to the first frame of a stream, not just later frames.
- **Breaking** `FrameDecoderError::WindowSizeTooBig` gained a `max` field and now reports the effective limit.

# After 0.8.2

- Introduce the `rust-version` field
- Fix checksum generation when repeatedly using the encoder
- Expose decoding::Dictionary as public
- Add Debug derive to CompressionLevel enum
- Make RLE and Raw block decoding more efficient and not use intermediary buffer on the stack

# After 0.8.1

- The CLI has been refactored to use `clap`
- The MatchDriverGenerator has been made public so users can name it as `M` in `FrameCompressor<R,W,M>`

# After 0.8.0

- The compressor now includes a `content_checksum` when the `hash` feature is enabled
- Dictionary generation has been added

# After 0.7.3

- Add initial compression support
- **Breaking** Refactor modules to reflect that this is now also a compression library

# After 0.7.2

- Soundness fix in decoding::RingBuffer. The lengths of the diferent regions where sometimes calculated wrongly, resulting in reads of heap memory not belonging to that ringbuffer
  - Fixed by https://github.com/paolobarbolini
  - Affected versions: 0.7.0 up to and including 0.7.2

- Added convenience functions to FrameDecoder to decode multiple frames from a buffer (https://github.com/philipc)

# After 0.7.1

- Remove byteorder dependency (https://github.com/workingjubilee)
- Preparations to become a std dependency (https://github.com/workingjubilee)

# After 0.7.0

- Fix for drain_to functions into limited targets (https://github.com/michaelkirk)

# After 0.6.0

- Small fix in the zstd binary, progress tracking was slighty off for skippable frames resulting in an error only when the last frame in a file was skippable
- Small performance improvement by reorganizing code with `#[cold]` annotations
- Documentation for `StreamDecoder` mentioning the limitations around multiple frames (https://github.com/Sorseg)
- Documentation around skippable frames (https://github.com/Sorseg)
- **Breaking** `StreamDecoder` API changes to get access to the inner parts (https://github.com/ifd3f)
- Big internal documentation contribution (https://github.com/zleyyij)
- Dropped derive_more as a dependency (https://github.com/xd009642)
- Small improvement by removing the error cases from the reverse bitreader (and making sure invalid requests can't even happen)

# After 0.5.0

- Make the hashing checksum optional (thanks to [@tamird](https://github.com/tamird))
  - breaking change as the public API changes based on features
- The FrameDecoder is now Send + Sync (RingBuffer impls these traits now)
