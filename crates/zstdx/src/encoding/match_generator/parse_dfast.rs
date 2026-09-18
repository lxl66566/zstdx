//! The two-table dfast strategy loop (level Fast) — a method of [`super::MatchGeneratorDriver`],
//! split out of the driver file for size. Field and helper access resolves
//! through the parent module.

use super::*;

impl MatchGeneratorDriver {
    /// The double-hash strategy loop (level [`Level::Fast`], libzstd's
    /// dfast): one 8-byte long-hash probe and one 5-byte short-hash probe
    /// per position — no chain walk, no lazy deferral. A two-position
    /// pipeline overlaps the hash multiplies and table loads, a short hit
    /// is upgraded by the long probe prepared for the next position, and
    /// matched ranges re-seed both tables through a few anchors (see
    /// [`DfastEmit::emit`]). The miss step grows with the literal run
    /// (one per 256 B, libzstd's `kSearchStrength` grid).
    #[allow(clippy::too_many_lines)]
    pub(super) fn start_matching_dfast<
        const RAMPED: bool,
        const MM4: bool,
        const LONG_LOG: u32,
        const SMALL_LOG: u32,
    >(
        &mut self,
        literals: &mut Vec<u8>,
        seqs: &mut Vec<SeqWord>,
    ) {
        let win = window_slice(&self.win, self.ext.as_ref());
        // Known rows instantiate with their logs as constants (the hash
        // shifts fold to immediates and the two shift registers free up);
        // clamped-window shapes pass [`RUNTIME_LOG`] for both.
        let long_log = if LONG_LOG == RUNTIME_LOG {
            self.second.trailing_zeros()
        } else {
            LONG_LOG
        };
        let small_log = if SMALL_LOG == RUNTIME_LOG {
            (self.tables.len() - self.second).trailing_zeros()
        } else {
            SMALL_LOG
        };
        let win_base = self.win_base;
        let ramp = self.ramp;
        let block_len = (self.block_end - win_base) as usize;
        // Probing hashes 8 bytes, so the last probeable window index; the
        // insert bound coincides with it (the window ends at the block), so
        // one shared limit serves both.
        let limit_idx = block_len
            .saturating_sub(HASH_READ)
            .min(win.len().saturating_sub(HASH_READ));
        let insert_max_idx = limit_idx;
        let max_window = self.params.window as u64;
        let mut rep = self.rep;
        let mut rep_pending = self.rep_pending;
        let mut seed_offset = self.seed_offset;
        let mut seed_hits = self.seed_hits;
        let mut seed_budget = self.seed_budget;
        let mut anchor_idx = (self.anchor - win_base) as usize;
        let mut ip_idx = (self.pos - win_base) as usize;
        // The scan's own table writes go through raw pointers: routing them
        // through the emit context's slice fields kept the pointers,
        // log shifts and tag stack-resident (a re-load per access, the
        // classic SROA failure at this loop's live-value count).
        // The two tables live in ONE allocation: the small table is a
        // fixed displacement off the long table's base (the row's const
        // log), so the loop keeps a single table base live instead of
        // two pointers — one register and its spill round-trips off the
        // hottest loop's live set.
        // SAFETY: derived here, before the context below takes its
        // borrows; both address the same memory, and the loop and the
        // emit helpers never access a slot concurrently.
        let split = if LONG_LOG == RUNTIME_LOG {
            self.second
        } else {
            1usize << LONG_LOG
        };
        let (long, small) = self.tables.split_at_mut(split);
        let long_ptr: *mut u32 = long.as_mut_ptr();
        let small_ptr: *mut u32 = small.as_mut_ptr();
        // Dictionary frames hash the short table over 4 bytes (libzstd's
        // small-table dfast rows, minMatch 4 — see `dict_row`); every other
        // instantiation keeps the 5-byte width, so its body compiles
        // byte-identically to the pre-`MM4` form.
        let short_width = if MM4 {
            ChainHashWidth::Four
        } else {
            ChainHashWidth::Five
        };
        let mut emit = DfastEmit {
            long,
            small,
            literals,
            seqs,
            win_base,
            insert_max_idx,
            long_log,
            small_log,
            width: short_width,
        };

        // Resolve a table entry to a window index. Same distance trick as
        // the fast loop's `resolve`: `dist = pos - (entry - 1)` wraps huge
        // for the empty sentinel and previous-4-GiB-cycle entries, so
        // `dist <= reach` (reach = min(pos - win_base, max_window)) rejects
        // every failure mode in one compare, and invalid entries alias the
        // scanning position itself, whose bytes always compare equal — the
        // byte compare plus `cand != ip_idx` then rejects them with
        // predictable branches (libzstd's selectAddr trick).
        //
        // SAFETY: none needed — the select keeps the index inside the
        // window either way.
        let resolve = |entry: u32, pos_abs: u64, reach: u64, ip: usize| -> usize {
            // Same double-wrapping form as the fast loop's `resolve`.
            let dist = pos_abs.wrapping_sub(entry as u64).wrapping_add(1);
            if dist <= reach {
                (pos_abs - win_base) as usize - dist as usize
            } else {
                ip
            }
        };

        // Outer loop: one pass per emitted match; re-entering resets the
        // miss step. The inner loop walks single positions until a match
        // or the block tail. Like the fast loop, the body runs in two
        // phases (see `scan_fast`): the seeded phase carries the job-start
        // seed/gate state, the steady phase folds it out — this loop's live
        // set is even larger (two tables, two hash logs), so the dead
        // weight cost more here.
        macro_rules! scan_dfast {
            ($outer:lifetime, $gated:literal) => {
                // The short-match upgrade probes at ip + 1, so a pass needs a
                // full position pair inside the block.
                if ip_idx + 1 > limit_idx {
                    break $outer;
                }
                let mut ip1_idx = ip_idx + 1;
                let mut hl0 = hash8_at_log(win, ip_idx, long_log);
                // SAFETY: hl0 is masked to the long table size.
                let mut entry_l0 = unsafe { *long_ptr.add(hl0) };

                loop {
                let hs0 = hash_at_width(win, ip_idx, small_log, short_width);
                // SAFETY: hs0 is masked to the small table size.
                let entry_s0 = unsafe { *small_ptr.add(hs0) };
                let pos_abs = win_base + ip_idx as u64;
                // Oldest usable candidate age: within the level window and
                // inside the live window buffer.
                let reach = (pos_abs - win_base).min(max_window);
                // Insert after both lookups, before probing (newest-wins).
                // SAFETY: both hashes masked to their tables' sizes.
                unsafe {
                    let entry = pack_pos(pos_abs);
                    *long_ptr.add(hl0) = entry;
                    *small_ptr.add(hs0) = entry;
                }

                // Repcode pre-probe one byte ahead: the probed byte stays a
                // pending literal, so of_value 1 encodes rep0 instead of a
                // swap, and no backward extension may consume it. Gated job
                // starts skip the probe (unknown decoder history). The bound
                // is window-relative: `probe >= rep[0]` is
                // `win_base + probe >= win_base + rep[0]` with win_base
                // folded out.
                if !($gated && rep_pending != 0) {
                    let probe = ip_idx + 1;
                    if probe >= rep[0] as usize {
                        let cand = probe - rep[0] as usize;
                        if read4(win, cand) == read4(win, probe)
                            && !ramp_blocks::<RAMPED>(ramp,
                                win_base + probe as u64,
                                win_base + cand as u64,
                            )
                        {
                            let ml = extend_match(win, probe, cand);
                            debug_assert!(ml >= MIN_MATCH);
                            anchor_idx =
                                emit.emit(win, anchor_idx, ip_idx, probe, ml, 1, &mut rep);
                            ip_idx = emit.rep_chain::<RAMPED>(win, anchor_idx, limit_idx, &mut rep, ramp);
                            // The chain's matches advance the anchor too.
                            anchor_idx = ip_idx;
                            continue $outer;
                        }
                    }
                }

                // Seed-offset probe (job starts only): direct byte compare
                // at the prefill-detected long-repeat offset — neither
                // single-candidate table can serve it (see `seed_offset`).
                // Mirrors the fast loop's twin block.
                if $gated && seed_offset != 0 {
                    let ci = (pos_abs - seed_offset as u64 - win_base) as usize;
                    // Strictly older than the probed position: the rep
                    // probe's pos-advance can otherwise cancel a seed
                    // offset of one into a zero-offset self-compare (the
                    // pays gate's ilog2(0)).
                    if ci < ip_idx && read4(win, ci) == read4(win, ip_idx) {
                        let mut ml = extend_match(win, ip_idx, ci);
                        if ml >= 6
                            && pays_for_offset(ml, ip_idx, ci, false)
                            && !ramp_blocks::<RAMPED>(ramp, pos_abs, win_base + ci as u64)
                        {
                            let mut start = ip_idx;
                            let mut c = ci;
                            let cfl = ramp_ext_floor::<RAMPED>(ramp, ci, win_base);
                            while start > anchor_idx && c > cfl && win[c - 1] == win[start - 1] {
                                c -= 1;
                                start -= 1;
                                ml += 1;
                            }
                            let of_value = (start - c + 3) as u32;
                            anchor_idx =
                                emit.emit(win, anchor_idx, ip_idx, start, ml, of_value, &mut rep);
                            rep_pending = rep_pending.saturating_sub(1);
                            seed_hits += 1;
                            if seed_hits >= SEED_MATCHES {
                                seed_offset = 0;
                            }
                            ip_idx = if rep_pending == 0 {
                                emit.rep_chain::<RAMPED>(win, anchor_idx, limit_idx, &mut rep, ramp)
                            } else {
                                anchor_idx
                            };
                            anchor_idx = ip_idx;
                            continue $outer;
                        }
                    }
                    seed_budget -= 1;
                    if seed_budget == 0 {
                        seed_offset = 0;
                    }
                }

                let hl1 = hash8_at_log(win, ip1_idx, long_log);

                // Long probe: a full 8-byte match at the long-hash candidate.
                {
                    let cand = resolve(entry_l0, pos_abs, reach, ip_idx);
                    if cand != ip_idx
                        && read8(win, cand) == read8(win, ip_idx)
                        && !ramp_blocks::<RAMPED>(ramp, pos_abs, win_base + cand as u64)
                    {
                        let mut start = ip_idx;
                        let mut c = cand;
                        let mut ml = extend_match(win, ip_idx, cand);
                        let cfl = ramp_ext_floor::<RAMPED>(ramp, cand, win_base);
                        // Backward catch-up into the pending literals; the
                        // offset (start - c) stays constant.
                        while start > anchor_idx && c > cfl && win[c - 1] == win[start - 1] {
                            c -= 1;
                            start -= 1;
                            ml += 1;
                        }
                        let of_value = (start - c + 3) as u32;
                        anchor_idx =
                            emit.emit(win, anchor_idx, ip_idx, start, ml, of_value, &mut rep);
                        if ip1_idx < anchor_idx {
                            // The emit moved the anchor past ip1, so the
                            // match covers it: indexing it cannot pollute
                            // later probes.
                            // SAFETY: hl1 is masked to the long table size.
                            unsafe {
                                *long_ptr.add(hl1) = pack_pos(win_base + ip1_idx as u64);
                            }
                        }
                        // A literal offset shifts the decoder's history one
                        // slot down; after the third one a job-start gate
                        // has fully converged and repcode use is safe
                        // again.
                        if $gated {
                            rep_pending = rep_pending.saturating_sub(1);
                            ip_idx = if rep_pending == 0 {
                                emit.rep_chain::<RAMPED>(win, anchor_idx, limit_idx, &mut rep, ramp)
                            } else {
                                anchor_idx
                            };
                        } else {
                            ip_idx = emit.rep_chain::<RAMPED>(win, anchor_idx, limit_idx, &mut rep, ramp);
                        }
                        // The chain's matches advance the anchor too.
                        anchor_idx = ip_idx;
                        continue $outer;
                    }
                }

                // SAFETY: hl1 is masked to the long table size.
                let entry_l1 = unsafe { *long_ptr.add(hl1) };

                // Short probe: 4 bytes at the short-hash candidate, upgraded
                // by the long probe prepared for the next position.
                {
                    let cand = resolve(entry_s0, pos_abs, reach, ip_idx);
                    if cand != ip_idx && read4(win, cand) == read4(win, ip_idx) {
                        let mut start = ip_idx;
                        let mut c = cand;
                        let mut ml = extend_match(win, ip_idx, cand);
                        let pos1_abs = win_base + ip1_idx as u64;
                        let reach1 = (pos1_abs - win_base).min(max_window);
                        let c1 = resolve(entry_l1, pos1_abs, reach1, ip1_idx);
                        if c1 != ip1_idx && read8(win, c1) == read8(win, ip1_idx) {
                            let l1len = extend_match(win, ip1_idx, c1);
                            if l1len > ml {
                                start = ip1_idx;
                                c = c1;
                                ml = l1len;
                            }
                        }
                        // A ramp-blocked pair falls through to the miss
                        // advance below (never `break`: the inner loop's
                        // advance is what moves past this position).
                        if !ramp_blocks::<RAMPED>(ramp, win_base + start as u64, win_base + c as u64) {
                            let cfl = ramp_ext_floor::<RAMPED>(ramp, c, win_base);
                            while start > anchor_idx && c > cfl && win[c - 1] == win[start - 1] {
                                c -= 1;
                                start -= 1;
                                ml += 1;
                            }
                            let of_value = (start - c + 3) as u32;
                            anchor_idx =
                                emit.emit(win, anchor_idx, ip_idx, start, ml, of_value, &mut rep);
                            if ip1_idx < anchor_idx {
                                // The emit moved the anchor past ip1, so the
                                // match covers it: indexing it cannot pollute
                                // later probes.
                                // SAFETY: hl1 is masked to the long table
                                // size; see the long probe above.
                                unsafe {
                                    *long_ptr.add(hl1) = pack_pos(win_base + ip1_idx as u64);
                                }
                            }
                            if $gated {
                                rep_pending = rep_pending.saturating_sub(1);
                                ip_idx = if rep_pending == 0 {
                                    emit.rep_chain::<RAMPED>(win, anchor_idx, limit_idx, &mut rep, ramp)
                                } else {
                                    anchor_idx
                                };
                            } else {
                                ip_idx = emit.rep_chain::<RAMPED>(win, anchor_idx, limit_idx, &mut rep, ramp);
                            }
                            // The chain's matches advance the anchor too.
                            anchor_idx = ip_idx;
                            continue $outer;
                        }
                    }
                }

                // Miss: advance the pair; the step grows with the literal
                // run (one per 256 B), so compressible data keeps probing
                // every byte while incompressible runs accelerate.
                ip_idx = ip1_idx;
                ip1_idx += 1 + ((ip1_idx - anchor_idx) >> 8);
                hl0 = hl1;
                entry_l0 = entry_l1;
                if ip1_idx > limit_idx {
                    break;
                }
            }
            // The pair left the block: nothing left to probe.
            break $outer;
            };
        }
        // Seeded phase (job starts only; bulk-ST skips it entirely), then
        // the steady phase — same convergence contract as `scan_fast`.
        'seeded: while (rep_pending != 0 || seed_offset != 0) && ip_idx < limit_idx {
            scan_dfast!('seeded, true);
        }
        'outer: loop {
            scan_dfast!('outer, false);
        }
        if !emit.seqs.is_empty() && anchor_idx < block_len {
            emit.literals.extend_from_slice(&win[anchor_idx..block_len]);
        }
        self.pos = self.block_end;
        self.anchor = self.block_end;
        self.rep = rep;
        self.rep_pending = rep_pending;
        self.seed_offset = seed_offset;
        self.seed_hits = seed_hits;
        self.seed_budget = seed_budget;
    }
}
