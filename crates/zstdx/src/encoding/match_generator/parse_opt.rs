//! The bridge into the optimal parser (levels Opt/Ultra) — a method of
//! [`super::MatchGeneratorDriver`], split out of the driver file for size. Field and helper access
//! resolves through the parent module.

use super::*;

impl MatchGeneratorDriver {
    /// Bridge into the optimal parser (levels Opt/Ultra): hands over
    /// the window, the origin-biased tables and the persistent price state,
    /// then stores back the cursors the parser advanced. The tree's search
    /// domain is the `chain_reach` override (the stock row window; the
    /// frame window may sit wider for LDM's far classes).
    pub(super) fn start_matching_opt(
        &mut self,
        knobs: OptKnobs,
        literals: &mut Vec<u8>,
        seqs: &mut Vec<SeqWord>,
    ) -> bool {
        let win = window_slice(&self.win, self.ext.as_ref());
        let win_base = self.win_base;
        let block_start = self.block_start;
        let block_end = self.block_end;
        let max_window = self.params.chain_reach.unwrap_or(self.params.window) as u64;
        let ldm_seqs: &[LdmSeq] = &self.ldm_seqs[..];
        let mut origin = self.opt_origin;
        let mut next_update = self.next_update;
        let mut rep = self.rep;
        let mut rep_pending = self.rep_pending;
        // The fill-lag clamp in `opt::run_block` runs only on the
        // alphabet-gated population: `ldm_checked` set with `ldm` disabled
        // means LDM was armed and the first block's alphabet check turned
        // it off (a size-gated frame never reaches the check, and an
        // armed frame keeps its far candidates). Job/dictionary strips
        // (`prefill_window`) stay unclamped: their restart parses measurably
        // used the lagged region's candidates.
        let clamp_lag = self.ldm_checked && self.ldm.is_none() && !self.strip_parse;
        let Some(scratch) = self.opt_scratch.as_mut() else {
            unreachable!("opt scratch allocated by apply_level")
        };
        // Disjoint field borrows: the window (win/ext) against the tables.
        let ldm_won = if knobs.ultra {
            crate::encoding::opt::run_block::<true>(
                &knobs,
                win,
                win_base,
                block_start,
                block_end,
                max_window,
                ldm_seqs,
                &mut origin,
                &mut self.opt_table,
                &mut self.bt,
                &mut self.hash3,
                &mut next_update,
                &mut self.opt_state,
                scratch,
                &mut rep,
                &mut rep_pending,
                literals,
                seqs,
                clamp_lag,
            )
        } else {
            crate::encoding::opt::run_block::<false>(
                &knobs,
                win,
                win_base,
                block_start,
                block_end,
                max_window,
                ldm_seqs,
                &mut origin,
                &mut self.opt_table,
                &mut self.bt,
                &mut self.hash3,
                &mut next_update,
                &mut self.opt_state,
                scratch,
                &mut rep,
                &mut rep_pending,
                literals,
                seqs,
                clamp_lag,
            )
        };
        self.opt_origin = origin;
        self.next_update = next_update;
        self.rep = rep;
        self.rep_pending = rep_pending;
        self.pos = block_end;
        self.anchor = block_end;
        ldm_won
    }

    /// Whether the cold-start DUBT head applies to this frame: the row
    /// opts in and the input is large enough that the head stays a
    /// minority share of the parse (a declared length below
    /// [`HEAD_MIN_TOTAL`] would be parsed entirely by the head).
    pub(super) fn head_eligible(&self) -> bool {
        self.params.dubt_head
            && matches!(self.params.strategy, Strategy::Chain(_))
            // A shrunk frame never runs the head: the shrink side is the
            // chain-selection class, so the probe's measurement and the
            // executed parse stay the same object on both reach sides.
            && self.reach_choice != ReachChoice::Shrink
            && self.shape.len.is_none_or(|l| l >= HEAD_MIN_TOTAL)
    }

    /// Dispatch guard for the cold-start DUBT head ([`HEAD_LIMIT`]): on
    /// the first head block, evaluate the alphabet gate and arm the head
    /// tables; true while this block parses through the btlazy2 driver.
    pub(super) fn head_block(&mut self) -> bool {
        if self.dubt_head != HeadPhase::Armed || self.block_start >= HEAD_LIMIT {
            if self.dubt_head == HeadPhase::Running && self.block_start >= HEAD_LIMIT {
                // First dispatch past the span: a gated or RLE-skipped final
                // head block never reaches the parser, so the handoff rides
                // the first block that does (before its chain scan probes).
                self.finish_head();
            }
            return self.dubt_head == HeadPhase::Running;
        }
        let win = window_slice(&self.win, self.ext.as_ref());
        let idx = self.idx_of(self.block_start);
        let mut seen = [0u64; 4];
        for &b in &win[idx..(idx + 8192).min(win.len())] {
            seen[(b >> 6) as usize] |= 1 << (b & 63);
        }
        let syms: u32 = seen.iter().map(|w| w.count_ones()).sum();
        if syms < HEAD_SYMS_MIN {
            self.dubt_head = HeadPhase::Off;
            return false;
        }
        if self.dubt_table.len() == 1 << HEAD_HASH_LOG
            && self.dubt_bt.len() == 2 << HEAD_KNOBS.bt_log
        {
            self.dubt_table.fill(0);
            self.dubt_bt.fill(0);
        } else {
            self.dubt_table = alloc::vec![0u32; 1 << HEAD_HASH_LOG];
            self.dubt_bt = alloc::vec![0u32; 2 << HEAD_KNOBS.bt_log];
        }
        if self.lazy_scratch.is_none() {
            self.lazy_scratch = Some(LazyScratch::new());
        }
        self.dubt_head = HeadPhase::Running;
        true
    }

    /// Head handoff after the block that crosses [`HEAD_LIMIT`]: the chain
    /// takes over from the next block with the whole head region
    /// dense-indexed into its tables (`gap_start` at the frame start
    /// drives [`Self::catch_up_insertions`] before the next scan probes).
    /// The head tables stay allocated for the pooled state's next frame.
    pub(super) fn finish_head(&mut self) {
        if self.block_end >= HEAD_LIMIT {
            self.dubt_head = HeadPhase::Done;
            self.gap_start = 0;
            self.miss_count = 0;
            self.rep_pending = 0;
        }
    }

    /// Cold-head probe-step dispatch for the btlazy2 rows ([`BtStepPhase`]):
    /// evaluate the alphabet gate at the frame's first searching block
    /// inside the dense span, keep head blocks dense, resume the steady
    /// ramp past it. Gated and RLE-skipped blocks never reach this, so
    /// `Armed` survives to the first block that actually parses.
    pub(super) fn bt_lazy_step(&mut self) -> LazyStep {
        if self.block_start >= BT_DENSE_LIMIT {
            self.bt_step = BtStepPhase::Off;
            return LazyStep::Ramp;
        }
        if self.bt_step == BtStepPhase::Armed {
            let win = window_slice(&self.win, self.ext.as_ref());
            let idx = self.idx_of(self.block_start);
            let mut seen = [0u64; 4];
            for &b in &win[idx..(idx + 8192).min(win.len())] {
                seen[(b >> 6) as usize] |= 1 << (b & 63);
            }
            let syms: u32 = seen.iter().map(|w| w.count_ones()).sum();
            self.bt_step = if syms >= HEAD_SYMS_MIN {
                BtStepPhase::Dense
            } else {
                BtStepPhase::Off
            };
        }
        if self.bt_step == BtStepPhase::Dense {
            LazyStep::Dense
        } else {
            LazyStep::Ramp
        }
    }
}
