//! The single-probe fast strategy loop (level Fastest) — a method of
//! [`super::MatchGeneratorDriver`], split out of the driver file for size. Field and helper access
//! resolves through the parent module.

use super::*;

impl MatchGeneratorDriver {
    /// The single-probe `fast` strategy loop (level [`Level::Fastest`]).
    ///
    /// Armed LDM segments the block (libzstd's `ZSTD_ldm_blockCompress`
    /// for the fast family, the chain row's gap-parse model): each
    /// long-distance candidate ends the current segment at its split and
    /// is emitted wholesale there, and the scan parses only the literal
    /// gap ahead of it — every bound (the loop guards, the forward
    /// extensions, the repcode chains) retargets at the segment end like
    /// C's per-gap compressor call. The wholesale emission rides the row's
    /// own emit, so the covered fill keeps the stock anchor policy. An
    /// empty candidate set (LDM off, gated, latched, far-less shapes) is
    /// one segment — the stock parse, byte-identical.
    #[allow(clippy::too_many_lines)]
    pub(super) fn start_matching_fast<
        const RAMPED: bool,
        const DENSE: bool,
        const HASH_LOG: u32,
        const SMALL_WIDE: bool,
        const LDM: bool,
    >(
        &mut self,
        literals: &mut Vec<u8>,
        seqs: &mut Vec<SeqWord>,
    ) {
        // Hot state lives in locals for the whole loop: the emit helpers
        // used to take `&mut self`, which forced a reload of every cursor
        // from memory after each match.
        let win = window_slice(&self.win, self.ext.as_ref());
        let win_base = self.win_base;
        let block_end = self.block_end;
        // saturating: tiny first blocks never reach an emit, so the bound is
        // never consulted when win.len() < HASH_READ.
        let insert_max = win_base + win.len().saturating_sub(HASH_READ) as u64;
        // The scan's own table accesses go through a raw pointer: routing
        // them through the emit context's slice field kept the pointer
        // stack-resident (a re-load per access; see the dfast loop's note on
        // the same SROA failure).
        // SAFETY: derived here, before the context below takes its borrow;
        // both address the same memory, and the loop and the emit helpers
        // never access a slot concurrently. The table is never resized.
        let table_ptr: *mut u32 = self.tables[..self.second].as_mut_ptr();
        // Known rows instantiate with the fastest row's log as a constant
        // (the hash shift folds to an immediate and the shift-count register
        // frees — the same disease the dfast const-log landing treated);
        // clamped-window inputs (hash log below the row) take the
        // [`RUNTIME_LOG`] instantiation.
        let hash_log = if HASH_LOG == RUNTIME_LOG {
            self.params.hash_log
        } else {
            HASH_LOG
        };
        let mut emit = TableEmit {
            table: &mut self.tables[..self.second],
            literals,
            seqs,
            win_base,
            insert_max,
            hash_log,
            covered_fill: self.covered_fill,
            width: ChainHashWidth::Five,
            // The fast strategy's tables never advance the origin (see
            // `head_origin`); zero keeps its emit math on plain pack_pos.
            origin: 0,
        };
        // Armed frames keep the stock scan domain through `chain_reach`
        // (the widened window is the far domain); stock frames have none.
        let max_window = self.params.chain_reach.unwrap_or(self.params.window) as u64;
        let mut pos = self.pos;
        let mut anchor = self.anchor;
        let mut miss_count = self.miss_count;
        let mut rep = self.rep;
        let mut rep_pending = self.rep_pending;
        let mut seed_offset = self.seed_offset;
        let mut seed_hits = self.seed_hits;
        let mut seed_budget = self.seed_budget;
        let hash_read = HASH_READ as u64;
        // Fed-back literal prices for the dense body's acceptance bar. A
        // plain block's copy is dead code (DENSE folds to false), so the
        // plain instantiation stays byte-for-byte the stock body.
        let lit_lens = self.lit_lens;
        // Long-distance candidates of this block ([`Self::ldm_generate`]):
        // `ldm_i` is the first unconsumed one, each ending a segment at
        // its split. Generate's anchor skip keeps spans disjoint and
        // splits ascending. The `LDM = false` instantiation binds the
        // empty slice constant, so the segment loop folds to the stock
        // single-segment scan.
        let ldm_seqs: &[LdmSeq] = if LDM {
            &self.ldm_seqs[..]
        } else {
            &[]
        };
        let mut ldm_i = 0usize;
        let mut ldm_won = false;

        // Resolve a table entry to a window index. The truncated absolute
        // position is rebuilt as a *distance* from the scanning position:
        // `dist = pos - (entry - 1)` wraps to a huge u64 for both the empty
        // sentinel (dist = pos + 1 > reach) and candidates from the previous
        // 4 GiB cycle, so the window-range check `dist <= reach` (reach =
        // min(pos - win_base, max_window), i.e. `pos - lo`) rejects all
        // three failure modes in one compare. Invalid entries alias the
        // scanning position itself, whose bytes always compare equal — the
        // byte compare plus `cand != ip` then rejects them with one
        // predictable branch instead of a three-way check per probe
        // (libzstd's selectAddr trick).
        //
        // Known regime cost: the zero-extended entry subtraction keeps
        // dist >= pos + 1 - 2^32, which exceeds every window reach once pos
        // passes 2^32 + max_window — from there on all hash candidates
        // stay rejected for the rest of the frame (repcodes and the miss
        // ramp keep the output valid; ratio degrades toward rep-only).
        // The u32 wrap-domain resolve admits in-window candidates at any
        // stream size but costs hot-path instructions here — measured out
        // four ways, see the negative notes before retrying.
        //
        // SAFETY: none needed — the select keeps the index inside the
        // window either way.
        let resolve = |entry: u32, pos_abs: u64, reach: u64, ip: usize| -> usize {
            // dist = pos - entry + 1 without a debug panic on entry == 0
            // (the empty sentinel wraps to dist = pos + 1 > reach).
            let dist = pos_abs.wrapping_sub(entry as u64).wrapping_add(1);
            if dist <= reach {
                (pos_abs - win_base) as usize - dist as usize
            } else {
                ip
            }
        };

        // Two-position pipeline (libzstd's ip0/ip1 interleave): the hash and
        // table entry of the next position are prepared before the current
        // one is probed, so the hash multiply and table-load latencies of
        // both positions overlap instead of chaining behind every probe. A
        // pair advances by the miss step once both positions miss; the
        // second position is dropped at the block tail.
        // The miss step can jump past the block end, so the guard must
        // saturate instead of relying on pos < block_end.
        //
        // The body exists in two phases. The seeded phase carries the
        // job-start state (`seed_offset`/`seed_hits`/`seed_budget`,
        // `rep_pending`), which only ever winds down; once it converges the
        // steady phase runs a body with all of it folded out. In one loop
        // those four values stayed live across every iteration (bulk-ST and
        // post-convergence MT scanning carried pure dead weight), and the
        // scan paid for them in spill traffic — a quarter of its cycles sat
        // on stack reloads. `$gated` is a literal, so the phase-only code
        // paths are constant-folded away in the steady instantiation.
        //
        // A steady/tail loop split that folds `pair_len` to the constant 2
        // (full pairs while a pair fits, then a single-position epilogue
        // instantiation) costs 3% fewer instructions but regressed
        // text.fastest wall by 7% — the third macro instantiation displaces
        // the hot blocks; do not retry without a code-layout story.
        let ramp = self.ramp;
        // Small-input policy bars, compile-time per instantiation (a
        // runtime-variable bar in this loop cost text-16K fastest -22%
        // wall — the compare and the shift must fold): the stock bars
        // are large-input tunings — the sparse miss ramp exists to keep
        // incompressible spans cheap on big inputs, and the ml >= 6
        // hash bar prices literals against long-run entropy. A <= 128
        // KiB wide-alphabet block has neither (libzstd's small-src
        // tables switch their fast rows there: any 4-byte hash match
        // accepted, step grown one per 128 missed bytes). The alphabet
        // screen at the dispatch keeps the structured classes on the
        // stock scan — their short matches are net-negative (json 16
        // KiB: +171 B under the dense policy), while text-class short
        // matches are payload.
        #[allow(non_snake_case)]
        let MIN_ML: usize = if SMALL_WIDE {
            4
        } else {
            6
        };
        #[allow(non_snake_case)]
        let RAMP_SHIFT: u32 = if SMALL_WIDE {
            7
        } else {
            2
        };
        // The current segment's end: the block end, or the next
        // long-distance candidate's split (retargeted per segment below).
        // Declared before the scan macro: macro-internal identifiers
        // resolve against the definition site's locals. `$ldm` likewise:
        // the gap bounds only exist in the segmenting instantiation.
        let mut end;
        macro_rules! scan_fast {
            ($restart:lifetime, $gated:literal, $ldm:expr) => {
                let idx0 = (pos - win_base) as usize;
                // One u64 load per position feeds the hash, the 4-byte
                // probe prefilter (its low half) and the repcode compares.
                let v0 = read8(win, idx0);
                let h0 = hash5_log(v0, hash_log);
                // SAFETY: the hash masks to hash_log bits and the table
                // always holds 1 << hash_log slots (see insert_at).
                let prev0 = unsafe { *table_ptr.add(h0) };
                let cur0 = v0 as u32;
                let mut pair_len = 1u64;
                let mut idx1 = idx0;
                let mut h1 = h0;
                let mut prev1 = prev0;
                let mut cur1 = cur0;
                if end - pos > hash_read {
                    pair_len = 2;
                    idx1 = idx0 + 1;
                    let v1 = read8(win, idx1);
                    h1 = hash5_log(v1, hash_log);
                    // SAFETY: as above.
                    prev1 = unsafe { *table_ptr.add(h1) };
                    cur1 = v1 as u32;
                }
                // Store after both lookups so each probe sees the pre-store
                // entry (newest-wins).
                // SAFETY: as above.
                unsafe {
                    *table_ptr.add(h0) = pack_pos(pos);
                }
                if pair_len == 2 {
                    // SAFETY: as above.
                    unsafe {
                        *table_ptr.add(h1) = pack_pos(pos + 1);
                    }
                }

                // Probe the first position. Repcode candidate first (mirrors
                // zstd's fast strategy: rep[0] only); a repcode match needs
                // at least one pending literal so of_value 1 stays encodable: with
                // literals pending the probe runs at the current position,
                // otherwise one byte ahead so that byte becomes the literal.
                // The probe bound is window-relative (`pidx >= rep[0]` is
                // `probe >= win_base + rep[0]` with win_base folded out).
                let mut rep1_armed = false;
                {
                    let anchor_idx = (anchor - win_base) as usize;
                    // A gated job start must not probe repcodes (unknown
                    // decoder history); probe 0 sits below the bound, which
                    // folds the underflow guard into the same single
                    // comparison.
                    let pidx = if $gated && rep_pending != 0 {
                        0
                    } else if idx0 == anchor_idx {
                        idx0 + 1
                    } else {
                        idx0
                    };
                    // OR-folded repcode prefilter for the pair: both
                    // positions' compares collapse into one unpredictable
                    // branch (adjacent-position rep hits are correlated, so
                    // the fold predicts no worse than either compare alone).
                    // `(a == 0) | (b == 0)`, not `||`: the short-circuit
                    // form compiles to three unpredictable branches (the
                    // two compares plus the combine — ~19% of json.fastest's
                    // mispredicts); the bitwise OR keeps the one intended
                    // branch, with the `a`-disambiguation on the taken path.
                    // A taken fold whose first position misses arms the
                    // second position's full probe — its compare is then
                    // known good. Probe and emission order are unchanged,
                    // so the output is byte-identical to the two-compare
                    // form. `cur1` reuses the already-loaded second position
                    // (read4(idx1) is its low half); 1 = never hit, covering
                    // both the tail pair (idx1 falls back to idx0) and the
                    // second position sitting below the window bound.
                    if pidx >= rep[0] as usize {
                        let r = rep[0] as usize;
                        let a = read4(win, pidx - r) ^ read4(win, pidx);
                        let b = if pair_len == 2 && idx1 >= r {
                            read4(win, idx1 - r) ^ cur1
                        } else {
                            1
                        };
                        if (a == 0) | (b == 0) {
                            if a == 0 {
                                let mut cand = pidx - r;
                                let mut ml = extend_match(win, pidx, cand);
                                // Gap bound (C's per-gap compressor call
                                // counts to the gap end): no emission may
                                // cover bytes past the split — the wholesale
                                // emission there owns them.
                                if $ldm {
                                    ml = ml.min((end - win_base - pidx as u64) as usize);
                                }
                                if ml >= MIN_MATCH
                                    && !ramp_blocks::<RAMPED>(ramp, win_base + pidx as u64, win_base + cand as u64)
                                {
                                    let mut start = pidx;
                                    let cfl = ramp_ext_floor::<RAMPED>(ramp, cand, win_base);
                                    // Extend backwards into the pending literals;
                                    // the offset (pidx - cand) stays constant.
                                    while start > anchor_idx + 1
                                        && cand > cfl
                                        && win[cand - 1] == win[start - 1]
                                    {
                                        cand -= 1;
                                        start -= 1;
                                        ml += 1;
                                    }
                                    anchor = emit.emit(win, anchor, start, ml, 1, &mut rep);
                                    pos = emit.rep1_chain::<RAMPED>(win, None, anchor, end, &mut rep, ramp);
                                    // The chain's matches advance the cursor too.
                                    anchor = pos;
                                    miss_count = 0;
                                    continue $restart;
                                }
                            } else {
                                rep1_armed = true;
                            }
                        }
                    }

                    // Seed-offset probe (job starts only): direct byte compare
                    // at the prefill-detected long-repeat offset — the table
                    // cannot serve this candidate (see `seed_offset`). Emits as
                    // a plain literal-offset match, so it is legal under the
                    // repcode gate; each emit rotates `rep` toward holding the
                    // offset, and after SEED_MATCHES the repcode probes ride it
                    // without further seed help.
                    if $gated && seed_offset != 0 {
                    let ci = (pos - seed_offset as u64 - win_base) as usize;
                    // Strictly older than the probed position: the rep
                    // probe's pos-advance can otherwise cancel a seed
                    // offset of one into a zero-offset self-compare (the
                    // pays gate's ilog2(0)).
                    if ci < idx0 && read4(win, ci) == cur0 {
                        let mut ml = extend_match(win, idx0, ci);
                        // Gap bound, as above.
                        if $ldm {
                            ml = ml.min((end - pos) as usize);
                        }
                        // Same bar as the hash path: below 6 the sequence
                        // overhead eats the match, and the offset-gain gate
                        // keeps a far seed honest about its worth.
                        if ml >= 6
                            && pays_for_offset(ml, idx0, ci, false)
                            && !ramp_blocks::<RAMPED>(ramp, pos, win_base + ci as u64)
                        {
                            let anchor_idx = (anchor - win_base) as usize;
                            let mut start = idx0;
                            let mut ci = ci;
                            let cfl = ramp_ext_floor::<RAMPED>(ramp, ci, win_base);
                            // Extend backwards into the pending literals;
                            // the offset stays constant.
                            while start > anchor_idx && ci > cfl && win[ci - 1] == win[start - 1] {
                                ci -= 1;
                                start -= 1;
                                ml += 1;
                            }
                            let of_value = (start - ci + 3) as u32;
                            anchor = emit.emit(win, anchor, start, ml, of_value, &mut rep);
                            rep_pending = rep_pending.saturating_sub(1);
                            seed_hits += 1;
                            if seed_hits >= SEED_MATCHES {
                                seed_offset = 0;
                            }
                            pos = if rep_pending == 0 {
                                emit.rep1_chain::<RAMPED>(win, None, anchor, end, &mut rep, ramp)
                            } else {
                                anchor
                            };
                            anchor = pos;
                            miss_count = 0;
                            continue $restart;
                        }
                    }
                    // A seed that never pays retires on its budget instead
                    // of comparing dead bytes for the whole job.
                    seed_budget -= 1;
                    if seed_budget == 0 {
                        seed_offset = 0;
                    }
                }

                // Both positions' hash candidates are resolved and compared
                // up front so the two data-dependent compare outcomes share
                // ONE unpredictable branch (the rep OR-fold's rationale:
                // adjacent-position outcomes are correlated). The pre-reads
                // are loads only — the table writes happen inside emit, after
                // every probe here — and probe/emission order is untouched,
                // so the output is byte-identical to the two-compare form.
                // An `m` is 0 iff the candidate is not the scanning position
                // itself and its first 4 bytes agree; the tail pair
                // (pair_len 1) aliases the first position's values, so its
                // `m1` can only re-fire the already-tried `m0` probe. The
                // fold consumes only the two register-resident `m`s —
                // folding `rep1_armed` into the same condition kept its
                // spilled byte load on the hot branch input (a measured
                // json-64K regression), so the armed-but-no-hash-hit case
                // enters through its own rarely-taken branch instead.
                let reach = (pos - win_base).min(max_window);
                let mut cand0 = resolve(prev0, pos, reach, idx0);
                let m0 = (cand0 == idx0) as u32 | (read4(win, cand0) ^ cur0);
                let pos1 = win_base + idx1 as u64;
                let reach1 = (pos1 - win_base).min(max_window);
                let mut cand1 = resolve(prev1, pos1, reach1, idx1);
                let m1 = (cand1 == idx1) as u32 | (read4(win, cand1) ^ cur1);
                'probes: {
                if (m0 == 0) | (m1 == 0) {
                    if m0 == 0 {
                        let mut ml = extend_match(win, idx0, cand0);
                        // A hash match already spans 5 bytes; below 6 the
                        // sequence overhead roughly equals the literals
                        // it covers, and rejecting it lets the scan try
                        // the next position where a longer match may
                        // start.
                        // Gap bound, as above (the match starts at `pos`).
                        if $ldm {
                            ml = ml.min((end - pos) as usize);
                        }
                        if ml >= MIN_ML && !ramp_blocks::<RAMPED>(ramp, pos, win_base + cand0 as u64) {
                            let anchor_idx = (anchor - win_base) as usize;
                            let mut start = idx0;
                            let cfl = ramp_ext_floor::<RAMPED>(ramp, cand0, win_base);
                            // Extend backwards into the pending literals;
                            // the offset (idx0 - cand) stays constant.
                            while start > anchor_idx
                                && cand0 > cfl
                                && win[cand0 - 1] == win[start - 1]
                            {
                                cand0 -= 1;
                                start -= 1;
                                ml += 1;
                            }
                            // Dense-mode acceptance bar: decline a far
                            // match whose displaced literals, priced at the
                            // fed-back code lengths, do not cover its offset
                            // (the chain store gate's pricing). The decline
                            // falls through to the second-position probe
                            // and feeds the miss ramp below; with DENSE
                            // false the condition folds to the constant
                            // true, keeping this body stock.
                            let bar_ok = !DENSE
                                || start - cand0 < DENSE_BAR_MIN_DIST
                                || pays_for_offset_lit(
                                    win, start, ml, cand0, false, &lit_lens,
                                );
                            if bar_ok {
                                let of_value = (start - cand0 + 3) as u32;
                                anchor = emit.emit(win, anchor, start, ml, of_value, &mut rep);
                                // A literal offset shifts the decoder's history
                                // one slot down; after the third one a job-start
                                // gate has fully converged and repcode use is
                                // safe again.
                                if $gated {
                                    rep_pending = rep_pending.saturating_sub(1);
                                    pos = if rep_pending == 0 {
                                        emit.rep1_chain::<RAMPED>(win, None, anchor, end, &mut rep, ramp)
                                    } else {
                                        anchor
                                    };
                                } else {
                                    pos =
                                        emit.rep1_chain::<RAMPED>(win, None, anchor, end, &mut rep, ramp);
                                }
                                anchor = pos;
                                miss_count = 0;
                                continue $restart;
                            }
                        }
                    }
                } else if !rep1_armed {
                    break 'probes;
                }

                // Probe the second position through the entry prepared
                // above. `pos1 == anchor` is impossible (anchor <= pos <
                // pos + 1), so the pending-literal select folds away
                // here. The repcode probe is armed by the folded
                // prefilter above (which also applies the window bound
                // and the job-start gate), so it runs compare-free.
                if rep1_armed {
                    let mut cand = idx1 - rep[0] as usize;
                    let mut ml = extend_match(win, idx1, cand);
                    // Gap bound, as above (the match starts at `pos1`).
                    if $ldm {
                        ml = ml.min((end - pos1) as usize);
                    }
                    if ml >= MIN_MATCH
                        && !ramp_blocks::<RAMPED>(ramp, win_base + idx1 as u64, win_base + cand as u64)
                    {
                        let anchor_idx = (anchor - win_base) as usize;
                        let mut start = idx1;
                        let cfl = ramp_ext_floor::<RAMPED>(ramp, cand, win_base);
                        while start > anchor_idx + 1
                            && cand > cfl
                            && win[cand - 1] == win[start - 1]
                        {
                            cand -= 1;
                            start -= 1;
                            ml += 1;
                        }
                        anchor = emit.emit(win, anchor, start, ml, 1, &mut rep);
                        pos = emit.rep1_chain::<RAMPED>(win, None, anchor, end, &mut rep, ramp);
                        anchor = pos;
                        miss_count = 0;
                        continue $restart;
                    }
                }

                if m1 == 0 {
                    let mut ml = extend_match(win, idx1, cand1);
                    // Gap bound, as above (the match starts at `pos1`).
                    if $ldm {
                        ml = ml.min((end - pos1) as usize);
                    }
                    if ml >= MIN_ML && !ramp_blocks::<RAMPED>(ramp, pos1, win_base + cand1 as u64) {
                        let anchor_idx = (anchor - win_base) as usize;
                        let mut start = idx1;
                        let cfl = ramp_ext_floor::<RAMPED>(ramp, cand1, win_base);
                        while start > anchor_idx
                            && cand1 > cfl
                            && win[cand1 - 1] == win[start - 1]
                        {
                            cand1 -= 1;
                            start -= 1;
                            ml += 1;
                        }
                        // Dense-mode acceptance bar, second position;
                        // a decline falls to the miss ramp.
                        let bar_ok = !DENSE
                            || start - cand1 < DENSE_BAR_MIN_DIST
                            || pays_for_offset_lit(win, start, ml, cand1, false, &lit_lens);
                        if bar_ok {
                            let of_value = (start - cand1 + 3) as u32;
                            anchor = emit.emit(win, anchor, start, ml, of_value, &mut rep);
                            // A literal offset shifts the decoder's history
                            // one slot down; after the third one a job-start
                            // gate has fully converged and repcode use is
                            // safe again.
                            if $gated {
                                rep_pending = rep_pending.saturating_sub(1);
                                pos = if rep_pending == 0 {
                                    emit.rep1_chain::<RAMPED>(win, None, anchor, end, &mut rep, ramp)
                                } else {
                                    anchor
                                };
                            } else {
                                pos = emit.rep1_chain::<RAMPED>(win, None, anchor, end, &mut rep, ramp);
                            }
                            anchor = pos;
                            miss_count = 0;
                            continue $restart;
                        }
                    }
                }
                }
                }

            // Both positions missed: grow the probe step on long literal
            // runs so incompressible data does not pay a full hash per
            // byte. The step scales the whole pair so the probes-per-byte
            // density matches the single-position loop at every step size.
            // Faster-growing than libzstd's anchor-distance grid: our
            // single-probe table loses its far matches to overwrites when
            // incompressible gaps are probed densely (measured: json at
            // Fastest +9% size), and skipping is what keeps sparse-match
            // corpora fast.
            miss_count += pair_len as usize;
            let step = 1 + (miss_count >> RAMP_SHIFT).min(255) as u64;
            // Dense-mode miss-run stepping: once a miss run outgrows
            // DENSE_STEP_AFTER bytes, double the pair advance — weak
            // mid-run matches on match-dense parses are net-negative.
            // Folds to the stock step on plain blocks.
            let step = if DENSE && miss_count > DENSE_STEP_AFTER {
                step * 2
            } else {
                step
            };
            pos += pair_len * step;
            };
        }
        // One segment per long-distance candidate (plus the final one to
        // the block end): the gap scans run the stock two-phase loop with
        // every bound retargeted at the segment end (`end`), then the
        // candidate emits wholesale at its split. The phases span segments
        // like blocks — the seeded state is the scan's, not a segment's.
        'segments: loop {
            let seq = if let Some(s) = ldm_seqs.get(ldm_i) {
                end = s.split;
                Some(*s)
            } else {
                end = block_end;
                None
            };
            // Seeded phase: job starts only (bulk-ST skips it entirely); each
            // emit re-checks convergence at the head, and the values only
            // ever wind down.
            'seeded: while (rep_pending != 0 || seed_offset != 0)
                && end.saturating_sub(pos) >= hash_read
            {
                scan_fast!('seeded, true, LDM);
            }
            'restart: while end.saturating_sub(pos) >= hash_read {
                scan_fast!('restart, false, LDM);
            }
            let Some(seq) = seq else {
                // Last segment: the stock literals-only tail rule.
                if !emit.seqs.is_empty() && anchor < block_end {
                    let tail = (anchor - win_base) as usize..(block_end - win_base) as usize;
                    emit.literals.extend_from_slice(&win[tail]);
                }
                break 'segments;
            };
            ldm_i += 1;
            debug_assert_eq!(seq.split, end);
            // The 4-byte guard only defends a stale candidate (generate
            // verified the whole span at fill time); a failure drops it and
            // re-opens the split as gap bytes of the next segment, the
            // pending literals still attached.
            let cand_abs = end.checked_sub(seq.offset as u64);
            let valid = match cand_abs {
                Some(c) if c >= win_base && seq.len as usize >= MIN_MATCH => {
                    read4(win, (c - win_base) as usize) == read4(win, (end - win_base) as usize)
                },
                _ => false,
            };
            if !valid {
                pos = end;
                continue 'segments;
            }
            // The wholesale emission: the gap's pending literals become the
            // candidate's own literal run, the match carries generate's
            // measured span, and the offset enters the repcode history like
            // any literal-offset sequence. The split lands where the gear
            // hash triggered, mid-match for misaligned twins — the emission
            // backward-extends into the pending gap literals like every
            // other emission (the offset stays constant), and rides the
            // row's own emit so the covered fill keeps the stock anchor
            // policy (no bulk interior fill — the uncapped form stays
            // falsified on the chain row).
            let mut start = (end - win_base) as usize;
            let mut cand = (cand_abs.unwrap() - win_base) as usize;
            let mut ml = seq.len as usize;
            let floor = (anchor - win_base) as usize;
            let cfl = ramp_ext_floor::<RAMPED>(ramp, cand, win_base);
            while start > floor && cand > cfl && win[cand - 1] == win[start - 1] {
                cand -= 1;
                start -= 1;
                ml += 1;
            }
            anchor = emit.emit(win, anchor, start, ml, seq.offset + 3, &mut rep);
            ldm_won = true;
            // A wholesale emission is a literal-offset sequence: the
            // job-start rep gate counts it.
            rep_pending = rep_pending.saturating_sub(1);
            // Offset-2 chain after the emission (the row's convention: every
            // literal-offset emission rides rep1 while it pays), bounded at
            // the next candidate's split; a chain that covers a split drops
            // that candidate (the coverage rule — its span is emitted
            // content).
            let bound = ldm_seqs.get(ldm_i).map_or(block_end, |s| s.split);
            let mut cursor = anchor;
            if rep_pending == 0 {
                cursor = emit.rep1_chain::<RAMPED>(win, None, anchor, bound, &mut rep, ramp);
            }
            while ldm_i < ldm_seqs.len() && ldm_seqs[ldm_i].split < cursor {
                ldm_i += 1;
            }
            // Fresh segment, fresh literal-run ramp (the emission reset the
            // miss streak's meaning across the skipped interior).
            miss_count = 0;
            if cursor >= block_end {
                break 'segments;
            }
            pos = cursor;
            anchor = cursor;
        }
        if LDM {
            self.ldm_note_block(ldm_won);
        }
        // A zero-sequence block stages nothing: its literals are exactly the
        // block, which the caller reads through get_last_space() instead of
        // paying a whole-block copy into and out of the scratch buffer.
        self.pos = block_end;
        self.anchor = block_end;
        self.miss_count = miss_count;
        self.rep = rep;
        self.rep_pending = rep_pending;
        self.seed_offset = seed_offset;
        self.seed_hits = seed_hits;
        self.seed_budget = seed_budget;
    }
}
