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
        // Small-frame tree hash width (libzstd's small-input cParams
        // rows run searchLength 4 at every level): a declared <= 128 KiB
        // wide-alphabet frame — text class — hashes the tree on 4 bytes
        // like the rows it mirrors, while structured frames keep the
        // knob's 5 (their 4-byte candidates are net-negative: json 4 KiB
        // +8 B, skewed 16 KiB +255 B under 4). The DECLARED shape gates
        // it — the live window slice's length is capped near one block
        // and fired on every block of large text frames when used here
        // (32 MiB text +30% at the Best tier before the gate moved).
        let tree_mls = if self.params.small_src && self.win_small_wide() {
            4
        } else {
            knobs.mls as usize
        };
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
            tree_mls,
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
