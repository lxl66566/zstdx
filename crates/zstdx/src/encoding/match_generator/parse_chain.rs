//! The hash-chain strategy loop (levels above Fast) — a method of [`super::MatchGeneratorDriver`],
//! split out of the driver file for size. Field and helper access resolves
//! through the parent module.

use super::*;

impl MatchGeneratorDriver {
    /// The hash-chain strategy loop (levels above [`Level::Fastest`]):
    /// walk same-hash candidates through the chain table up to the level's
    /// search depth, prefer repcode candidates (they encode nearly free),
    /// and defer emission across up to `lazy_depth` further positions when
    /// a longer match may start there — libzstd's lazy family.
    #[allow(clippy::too_many_lines)]
    pub(super) fn start_matching_chain<const LDM: bool>(
        &mut self,
        literals: &mut Vec<u8>,
        seqs: &mut Vec<SeqWord>,
    ) {
        let win = window_slice(&self.win, self.ext.as_ref());
        let (table_view, chain) = self.tables.split_at_mut(self.second);
        let chain_mask = chain.len() - 1;
        let win_base = self.win_base;
        let ramp = self.ramp;
        let block_end = self.block_end;
        let hash_log = self.params.hash_log;
        let search_depth = self.params.search_depth as usize;
        let lazy_depth = self.params.lazy_depth;
        let min_match = self.params.min_match as usize;
        let dict_row = self.dict_row;
        let width = ChainHashWidth::of(dict_row);
        let max_window = self.params.chain_reach.unwrap_or(self.params.window) as u64;
        let insert_max = win_base + win.len().saturating_sub(HASH_READ) as u64;
        // The scan's own table accesses go through a raw pointer (see the
        // fast loop's note on the same SROA failure); emits go through the
        // context. The chain-link reads share the pointer for the same
        // reason (the walk's slot reads must not re-derive the slice).
        // SAFETY: derived here, before the context below takes its borrow;
        // both address the same memory, and the loop and the emit helpers
        // never access a slot concurrently. The table is never resized.
        let table_ptr: *mut u32 = table_view.as_mut_ptr();
        let chain_ptr: *const u32 = chain.as_ptr();
        let mut emit = TableEmit {
            table: table_view,
            literals,
            seqs,
            win_base,
            insert_max,
            hash_log,
            // The chain's emits go through `emit_chain`; the fast-only
            // covered-fill policy is dead state here.
            covered_fill: CoveredFill::Dense,
            width,
        };
        let hash_read = HASH_READ as u64;
        let mut pos = self.pos;
        let mut anchor = self.anchor;
        let mut rep = self.rep;
        let mut rep_pending = self.rep_pending;
        let mut miss_count = self.miss_count;
        let mut seed_offset = self.seed_offset;
        let mut seed_hits = self.seed_hits;
        let mut seed_budget = self.seed_budget;
        let lit_lens = &self.lit_lens;
        // Long-distance candidates of this block ([`Self::ldm_generate`]):
        // `ldm_i` is the first unconsumed one; covered candidates (their
        // split lies behind the anchor) are dropped as emissions advance.
        let ldm_seqs = &self.ldm_seqs[..];
        let mut ldm_i = 0usize;
        let mut ldm_won = false;

        // Chain-walk search from the hash head `entry` at window index `idx`,
        // returning the longest match's (length, candidate window index):
        // the module-level [`chain_search`], inlined here and in the lazy
        // walk below. The head entry is computed once by the caller (the
        // insert block links the same value — see there).
        let search = |win: &[u8], chain: *const u32, idx: usize, entry: u32| -> (usize, usize) {
            chain_search(
                win,
                chain,
                idx,
                entry,
                win_base,
                block_end,
                search_depth,
                chain_mask,
                max_window,
                ramp,
            )
        };

        // The body exists in two phases (the fast loop's pattern): the
        // seeded phase carries the job-start state (`seed_offset`/
        // `seed_hits`/`seed_budget`, `rep_pending`), which only ever winds
        // down; once it converges the steady phase runs a body with all of
        // it folded out. One loop kept those four values live across every
        // iteration — pure dead weight over bulk-ST and post-convergence
        // scanning — and the register pressure they added spilled the chain
        // walk's own loop values (win_base/pos_abs/reach/depth/mask
        // reloaded from the stack on every walk step). `$gated` is a
        // literal, so the phase-only paths constant-fold away in the steady
        // instantiation.
        macro_rules! scan_chain {
            ($restart:lifetime, $gated:literal, $ldm:expr) => {
            let idx = (pos - win_base) as usize;
            // Hash and head read once per position: the search, and the
            // insert below (whose chain link wants the same previous head),
            // share them — nothing writes the table in between.
            // SAFETY: hash_at_log masks to hash_log bits and the table holds
            // 1 << hash_log slots.
            let h = hash_at_width(win, idx, hash_log, width);
            let entry = unsafe { *table_ptr.add(h) };
            // Cross-position pipelining of the hash+head read (the dfast
            // ip0/ip1 pattern): the lazy walk below usually searches pos+1
            // first, and that head read is an L3-class random load that
            // would otherwise serialize in front of the walk. Issued here
            // it completes under the incumbent walk's shadow — issue
            // placement is load-bearing: after the walk's loop the loads
            // only decode behind its poorly-predicted exit branch and the
            // shadow evaporates (measured: dll +4.5% pre-walk vs +0.5%
            // post-walk). When the depth-0 rep probe advances pos the walk
            // starts at pos+2 instead and computes fresh (the None arm).
            // Skipped on the block tail (hashing idx+1 needs HASH_READ+1
            // bytes ahead).
            let mut pre1 = (0usize, 0u32);
            let piped = block_end - pos > hash_read;
            if piped {
                pre1.0 = hash_at_width(win, idx + 1, hash_log, width);
                // SAFETY: the hash masks to hash_log bits and the table
                // holds 1 << hash_log slots.
                pre1.1 = unsafe { *table_ptr.add(pre1.0) };
            }
            let (mut best_len, mut best_cand) = search(win, chain_ptr, idx, entry);

            // Long-distance candidate ([`super::ldm`]): a split whose far
            // 64-byte-window twin the sparse table retained. Probed before
            // the rep probe (which may advance pos past the split) and
            // after the chain search — a plain length competition; the
            // store gate and the lazy walk price the far offset. The live
            // length re-derivation cannot fall below MIN_MATCH_LENGTH
            // against the same bytes generation verified.
            if $ldm && ldm_i < ldm_seqs.len() {
                while ldm_i < ldm_seqs.len() && ldm_seqs[ldm_i].split < anchor {
                    ldm_i += 1;
                }
                if ldm_i < ldm_seqs.len() && ldm_seqs[ldm_i].split == pos {
                    let seq = ldm_seqs[ldm_i];
                    ldm_i += 1;
                    let cand_abs = pos - seq.offset as u64;
                    // The rep probe above may have advanced `pos` past this
                    // iteration's `idx`; an offset equal to that advance
                    // (offset 1) then resolves the candidate to `idx`
                    // itself — a self-compare whose zero offset the store
                    // gate below prices (ilog2(0)). Candidates must be
                    // strictly older than the probed position.
                    if cand_abs >= win_base {
                        let ci = (cand_abs - win_base) as usize;
                        if ci < idx && read4(win, ci) == read4(win, idx) {
                            let ml = extend_match(win, idx, ci);
                            if ml > best_len {
                                best_len = ml;
                                best_cand = ci;
                                ldm_won = true;
                            }
                        }
                    }
                }
            }

            // Repcode probe first when armed: with literals pending it runs
            // at the current position, otherwise one byte ahead so that
            // byte becomes the pending literal and of_value 1 stays
            // encodable (mirrors the fast loop). The offset encodes nearly
            // free, so bias it past the chain match.
            let mut rep_hit = false;
            if !$gated || rep_pending == 0 {
                let probe = if pos == anchor {
                    pos + 1
                } else {
                    pos
                };
                if let Some(cand_abs) = probe.checked_sub(rep[0] as u64)
                    && cand_abs >= win_base
                {
                    let pidx = (probe - win_base) as usize;
                    let cand = (cand_abs - win_base) as usize;
                    if read4(win, cand) == read4(win, pidx) {
                        let ml = extend_match(win, pidx, cand);
                        if ml >= MIN_MATCH
                            && ml + 3 > best_len
                            && !ramp.blocks(probe, cand_abs)
                        {
                            best_len = ml;
                            best_cand = cand;
                            rep_hit = true;
                            pos = probe;
                        }
                    }
                }
            }

            // Seed-offset probe (job starts only): the prefill-detected
            // long-repeat offset as a direct candidate, before the chain's
            // candidates — clumped data buries the period twin too deep in
            // the chain for the depth-limited walk to reach (see
            // `seed_offset`). Mirrors the fast loop's twin block; the pays
            // gate below keeps a far seed honest.
            let mut seed_hit = false;
            if $gated && seed_offset != 0 {
                let ci = (pos - seed_offset as u64 - win_base) as usize;
                // The rep probe may have advanced `pos` one byte past this
                // iteration's `idx`; a seed offset of exactly one then
                // resolves the candidate to `idx` itself — a self-compare
                // whose zero offset the store gate below prices
                // (ilog2(0)). The seed's twin must be strictly older than
                // the probed position.
                if ci < idx && read4(win, ci) == read4(win, idx) {
                    let ml = extend_match(win, idx, ci);
                    if ml >= 6 && ml + 3 > best_len && !ramp.blocks(pos, win_base + ci as u64) {
                        best_len = ml;
                        best_cand = ci;
                        rep_hit = false;
                        seed_hit = true;
                    }
                }
                if !seed_hit {
                    seed_budget -= 1;
                    if seed_budget == 0 {
                        seed_offset = 0;
                    }
                }
            }

            // Insert this position behind the probe (newest-wins), linking
            // the chain to the previous head. The chain slot key is the
            // absolute position — what the walk resolves candidates with.
            // The head value is the one read before the search: nothing has
            // written the table since.
            // SAFETY: both indices are masked to their tables' sizes.
            unsafe {
                *chain.get_unchecked_mut(pos as usize & chain_mask) = entry;
                *table_ptr.add(h) = pack_pos(pos);
            }

            // Resolve the pipelined head for the lazy walk below: the
            // insert above wrote table[h], so a pre-read of that very slot
            // must observe the new head (a fresh read at search time
            // would). The rep probe advancing pos leaves `pipe` empty —
            // its first search sits at pos+2 and computes fresh.
            let mut pipe: Option<u32> = None;
            if piped && (pos - win_base) as usize == idx {
                let (ph, mut pe) = pre1;
                if ph == h {
                    pe = pack_pos(pos);
                }
                pipe = Some(pe);
            }

            // Repcode matches stay legal from MIN_MATCH up regardless of the
            // row's min_match (libzstd's rep probe also checks 4 bytes).
            // The offset-pays gate is a no-dict speed calibration; a
            // dictionary frame bypasses it (libzstd has no such gate, and
            // the far-dict matches it rejects are the dict parse's
            // payload).
            if (best_len < min_match && !(rep_hit && best_len >= MIN_MATCH))
                || (!dict_row
                    && !pays_for_offset_lit(win, idx, best_len, best_cand, rep_hit, lit_lens))
            {
                // Grow the probe step on long literal runs (same policy as
                // the fast loop) so incompressible data does not pay a full
                // chain walk per byte. Faster-growing than libzstd's
                // anchor-distance grid: our per-probe chain walk is dearer,
                // and skipping over sparse-match gaps is what keeps the
                // Balanced levels fast on them.
                miss_count += 1;
                // Dictionary frames take libzstd's anchor-distance grid
                // exactly (step 1 for the first 256 literal bytes): the
                // ramp's skipped positions are the small-dict parse's
                // missed ml-4 twins (the literal-volume residue in the -5
                // seqstats decomposition), and dict frames are not a hot
                // path. `dict_row` is constant per frame, so the no-dict
                // bodies keep the ramp alone.
                let step = if dict_row {
                    1 + ((pos - anchor) >> 8)
                } else {
                    1 + (miss_count >> 2).min(255) as u64
                };
                if $ldm && ldm_i < ldm_seqs.len() {
                    // Never step over an unconsumed split: its candidate
                    // dies with the position (the scan is its only
                    // consumer).
                    pos = (pos + step).min(ldm_seqs[ldm_i].split);
                } else {
                    pos += step;
                }
                continue $restart;
            }
            miss_count = 0;

            // Lazy evaluation, ported to libzstd lazy's alternating gain
            // walk: each next position competes on offset-aware gain — a
            // match's distance is priced at its highbit (~4 bits per length
            // byte), so a much nearer match of near-equal length displaces a
            // long-range one (the selection error that cost text ~2% ratio:
            // pure-length comparison kept far chain candidates). A repcode
            // probe rides along (rep offsets price ~free). The walk advances
            // while some probe improves; a position that fails the depth-1
            // margins gets one second-chance probe at depth-2 margins before
            // giving up — exactly libzstd's ZSTD_lazy/ZSTD_lazy2 split.
            // Long-enough matches skip straight to emission.
            let mut best_pos = pos;
            let mut best_price = if rep_hit {
                0
            } else {
                price_of((best_pos - win_base) as usize, best_cand)
            };
            // Greedy rows (lazy_depth 0) emit the first acceptable match
            // without probing further positions (libzstd's ZSTD_greedy).
            if lazy_depth > 0 && best_len < 64 {
                // The incumbent's displaced-literal value (see lit_value);
                // recomputed whenever the incumbent changes so both sides
                // of the walk's gain comparisons price in fed-back literal
                // code lengths instead of the flat 4 bits/byte.
                let mut best_v = lit_value(win, (best_pos - win_base) as usize, best_len, lit_lens);
                'lazy: loop {
                    for (attempt, &(rep_mul, rep_m, search_m)) in
                        [(3i32, 1i32, 4i32), (4, 1, 7)].iter().enumerate()
                    {
                        if attempt > 0 && lazy_depth < 2 {
                            // Second-chance margins are the depth-2 strategy.
                            break;
                        }
                        let p2 = pos + 1;
                        if block_end.saturating_sub(p2) < hash_read {
                            break 'lazy;
                        }
                        pos = p2;
                        let idx2 = (p2 - win_base) as usize;
                        // Pipelined hash+head for this position, issued one
                        // search back under that walk's shadow (fresh for
                        // the first step, the refresh below for the rest):
                        // no table writes happen inside the lazy walk, and
                        // every path to the next search steps exactly one
                        // position. None only before the first search and
                        // at the block tail.
                        let entry2 = match pipe.take() {
                            Some(pre) => pre,
                            None => {
                                let hf = hash_at_width(win, idx2, hash_log, width);
                                // SAFETY: hash_at_log masks to hash_log
                                // bits and the table holds 1 << hash_log
                                // slots.
                                unsafe { *table_ptr.add(hf) }
                            }
                        };
                        // Refresh the pipeline for the next search (this
                        // position +1); the guard equals the next attempt's
                        // own break condition, so a skipped refresh is
                        // never consumed.
                        if block_end.saturating_sub(p2 + 1) >= hash_read {
                            let hn = hash_at_width(win, idx2 + 1, hash_log, width);
                            // SAFETY: as above.
                            pipe = Some(unsafe { *table_ptr.add(hn) });
                        }
                        // Repcode probe at the stepped position; literals are
                        // pending by construction (the walk advanced past the
                        // anchor), so of_value 1 stays encodable. Skipped when
                        // the incumbent is itself a rep: rep-vs-rep only
                        // accepts a strictly longer ride, which the depth-0
                        // probe and rep1_chain already cover, and on
                        // rep-dense shapes the extra read4+extend per step
                        // visibly taxed scan speed.
                        if (!$gated || rep_pending == 0)
                            && !rep_hit
                            && let Some(cand_abs) = p2.checked_sub(rep[0] as u64)
                            && cand_abs >= win_base
                            && !ramp.blocks(p2, cand_abs)
                        {
                            let ci = (cand_abs - win_base) as usize;
                            if read4(win, ci) == read4(win, idx2) {
                                let ml = extend_match(win, idx2, ci);
                                if ml >= MIN_MATCH {
                                    // Same gain-cap cheap reject as the
                                    // chain probe: lit_value tops out at 6
                                    // bits per byte (the clamp).
                                    if 6 * ml as i32 * rep_mul / 4
                                        > best_v * rep_mul / 4 - best_price + rep_m
                                    {
                                        let v = lit_value(win, idx2, ml, lit_lens);
                                        // rep_mul keeps libzstd's rep discount
                                        // (3/4 of the byte value at depth 1);
                                        // at the default lens v = ml*4 and this
                                        // is the flat ml*rep_mul comparison.
                                        if v * rep_mul / 4
                                            > best_v * rep_mul / 4 - best_price + rep_m
                                        {
                                            best_len = ml;
                                            best_cand = ci;
                                            best_pos = p2;
                                            best_price = 0;
                                            best_v = v;
                                            rep_hit = true;
                                            seed_hit = false;
                                            continue 'lazy;
                                        }
                                    }
                                }
                            }
                        }
                        // Chain search at the stepped position, on the
                        // pipelined hash+head read above.
                        let (len2, cand2) = search(win, chain_ptr, idx2, entry2);
                        let price2 = if len2 >= min_match {
                            price_of((p2 - win_base) as usize, cand2)
                        } else {
                            0
                        };
                        if len2 >= min_match {
                            // Cheap reject: lit_value tops out at 6 bits per
                            // byte (the clamp), so a candidate whose gain
                            // cap cannot beat the incumbent skips the four
                            // gathers entirely.
                            if 6 * len2 as i32 - price2 > best_v - best_price + search_m {
                                let v2 = lit_value(win, idx2, len2, lit_lens);
                                if v2 - price2 > best_v - best_price + search_m {
                                    best_len = len2;
                                    best_cand = cand2;
                                    best_pos = p2;
                                    best_price = price2;
                                    best_v = v2;
                                    rep_hit = false;
                                    seed_hit = false;
                                    continue 'lazy;
                                }
                            }
                        }
                    }
                    break;
                }
            }
            let mut start = (best_pos - win_base) as usize;

            // Backward extension into the pending literals; the offset
            // (start - cand) stays constant. A repcode emission must keep
            // one literal pending: of_value 1 with a zero literal length
            // resolves to a repcode *swap* on the decoder side, not rep0.
            let anchor_idx = (anchor - win_base) as usize;
            let floor = if rep_hit {
                anchor_idx + 1
            } else {
                anchor_idx
            };
            let cfl = ramp.ext_floor(best_cand, win_base);
            let mut cand = best_cand;
            let mut ml = best_len;
            while start > floor && cand > cfl && win[cand - 1] == win[start - 1] {
                cand -= 1;
                start -= 1;
                ml += 1;
            }
            let of_value = if rep_hit {
                1
            } else {
                (start - cand + 3) as u32
            };
            anchor = emit.emit_chain(win, chain, hash_log, anchor, start, ml, of_value, &mut rep);
            if $gated && rep_pending != 0 && of_value > 3 {
                rep_pending -= 1;
            }
            if seed_hit {
                seed_hits += 1;
                if seed_hits >= SEED_MATCHES {
                    seed_offset = 0;
                }
            }
            if $gated && rep_pending != 0 {
                pos = anchor;
            } else {
                pos = emit.rep1_chain::<true>(
                    win,
                    Some(&mut *chain),
                    anchor,
                    block_end,
                    &mut rep,
                    ramp,
                );
            }
            anchor = pos;
            };
        }
        // Seeded phase: job starts only (bulk-ST skips it entirely); each
        // emit re-checks convergence at the head, and the values only ever
        // wind down.
        'seeded: while (rep_pending != 0 || seed_offset != 0)
            && block_end.saturating_sub(pos) >= hash_read
        {
            scan_chain!('seeded, true, LDM);
        }
        'restart: while block_end.saturating_sub(pos) >= hash_read {
            scan_chain!('restart, false, LDM);
        }
        if !emit.seqs.is_empty() && anchor < block_end {
            let tail = (anchor - win_base) as usize..(block_end - win_base) as usize;
            emit.literals.extend_from_slice(&win[tail]);
        }
        if LDM {
            self.ldm_note_block(ldm_won);
        }
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
