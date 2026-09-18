//! The bridge into the btlazy2 parser (rows 13-15) — a method of [`super::MatchGeneratorDriver`],
//! split out of the driver file for size. Field and helper access resolves
//! through the parent module.

use super::*;

impl MatchGeneratorDriver {
    /// Bridge into the btlazy2 parser (rows 13-15): the same tables and
    /// cursors as the opt bridge, no price state. The search domain is the
    /// `chain_reach` override (the stock row window; the frame window may
    /// sit wider for LDM's far classes).
    pub(super) fn start_matching_btlazy(
        &mut self,
        knobs: OptKnobs,
        literals: &mut Vec<u8>,
        seqs: &mut Vec<SeqWord>,
        step: LazyStep,
    ) -> bool {
        let win = window_slice(&self.win, self.ext.as_ref());
        let mut rep = self.rep;
        let mut rep_pending = self.rep_pending;
        let max_window = self.params.chain_reach.unwrap_or(self.params.window) as u64;
        let ldm_seqs: &[LdmSeq] = &self.ldm_seqs[..];
        let Some(scratch) = self.lazy_scratch.as_mut() else {
            unreachable!("btlazy scratch allocated by apply_level")
        };
        let lit_lens = self.lit_lens;
        // Disjoint field borrows: the window (win/ext) against the tables.
        let ldm_won = crate::encoding::btlazy::run_block_lazy(
            &knobs,
            step,
            win,
            self.win_base,
            self.block_start,
            self.block_end,
            max_window,
            ldm_seqs,
            &mut self.dubt_table,
            &mut self.dubt_bt,
            &mut self.next_update,
            scratch,
            &lit_lens,
            &mut rep,
            &mut rep_pending,
            literals,
            seqs,
        );
        self.pos = self.block_end;
        self.anchor = self.block_end;
        self.rep = rep;
        self.rep_pending = rep_pending;
        ldm_won
    }
}
