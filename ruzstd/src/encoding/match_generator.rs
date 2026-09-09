//! Matching algorithm used to find repeated parts of the original data
//!
//! The Zstd format relies on finding repeated sequences of data and compressing these
//! sequences as instructions for the decoder. A sequence basically tells the decoder
//! "Go back X bytes and copy Y bytes to the end of your decode buffer".
//!
//! This is a port of the official zstd matcher family: one contiguous
//! window buffer, single-probe u32 hash tables (newest-wins overwrite
//! semantics, dfast's second short-hash table for the Fast level), hash
//! chains for the levels above, forward match extension in u64 chunks plus
//! backward extension into pending literals. Positions are tracked as
//! absolute u64 offsets; table slots carry `(epoch << 48) | position` so a
//! reset just bumps the epoch instead of clearing the table.

use alloc::vec::Vec;

use super::seq_codes::{decode_packed, pack_seq};
use super::Matcher;
use super::SeqWord;
use super::Sequence;
use crate::Level;
// Shared with the decoder so both sides agree on offset-history semantics.
use crate::decoding::sequence_execution::do_offset_history;

/// Shortest match worth encoding; matches the format's MINMATCH range.
const MIN_MATCH: usize = 4;
/// The hash reads a full u64, so insertable/scannable positions need this
/// many window bytes ahead to stay in bounds.
const HASH_READ: usize = 8;
/// Hash table size as a power of two.
const HASH_LOG: u32 = 15;
/// History kept for matching; also the window size declared in the frame header.
const MAX_WINDOW: usize = 0xC0000;

const EMPTY: u64 = 0;
/// Positions in table/chain entries live in the low 48 bits; the high 16
/// carry the epoch (see [`MatchGeneratorDriver::reset`]).
const POS_MASK: u64 = (1u64 << 48) - 1;

/// Which search loop the matcher runs. The `chain` buffer is the second
/// table for the two-table strategies and empty for [`Strategy::Fast`].
#[derive(Clone, Copy, PartialEq)]
enum Strategy {
    /// One single-probe table keyed by a 5-byte hash (level Fastest,
    /// libzstd's `fast`).
    Fast,
    /// Two single-probe tables: an 8-byte long hash in `table` and a
    /// 5-byte short hash in `chain` (level Fast, libzstd's `dfast`).
    /// The payload is the short-table log.
    Dfast(u32),
    /// Head table plus chain links walked up to the level's search depth
    /// with lazy deferral (levels above Fast, libzstd's `lazy` family).
    /// The payload is the chain-table log.
    Chain(u32),
}

/// Per-level search parameters, following libzstd's `clevels.h` ladder.
#[derive(Clone, Copy, PartialEq)]
struct LevelParams {
    hash_log: u32,
    /// Match window; also the window declared in the frame header.
    window: usize,
    strategy: Strategy,
    search_depth: u32,
    lazy_depth: u32,
}

const FASTEST_PARAMS: LevelParams = LevelParams {
    hash_log: HASH_LOG,
    window: MAX_WINDOW,
    strategy: Strategy::Fast,
    search_depth: 1,
    lazy_depth: 0,
};

/// libzstd level 3: H17 (long) C16 (short) with a 1 MiB window (the level's
/// W21 is A/B-pending; the smaller window also keeps the frame header as-is).
const FAST_PARAMS: LevelParams = LevelParams {
    hash_log: 17,
    window: 1 << 20,
    strategy: Strategy::Dfast(16),
    search_depth: 0,
    lazy_depth: 0,
};

/// The chain table is position-indexed, so its log doubles as the match
/// reach; pinning it to the window makes every in-window position linked
/// (no reach truncation). A/B on the 32 MiB corpus: matching zstd-6's C18
/// truncates chains at 256 KiB and collapses skewed (0.97→2.2xslow), while
/// the full-coverage 1 MiB window beats the old W21+C20 on both speed
/// (json 1.14→1.07) and ratio (far-offset codes disappear).
const BALANCED_PARAMS: LevelParams = LevelParams {
    hash_log: 17,
    window: 1 << 20,
    strategy: Strategy::Chain(20),
    search_depth: 16,
    lazy_depth: 2,
};

const BEST_PARAMS: LevelParams = LevelParams {
    hash_log: 18,
    window: 1 << 22,
    strategy: Strategy::Chain(21),
    search_depth: 64,
    lazy_depth: 4,
};

fn params_for_level(level: Level) -> LevelParams {
    match level {
        Level::Uncompressed | Level::Fastest => FASTEST_PARAMS,
        Level::Fast => FAST_PARAMS,
        Level::Balanced => BALANCED_PARAMS,
        Level::Best => BEST_PARAMS,
    }
}

/// Hash the 5 bytes at `idx`. Caller guarantees `idx + 5 <= win.len()` (the
/// scanning and emit loops bound-check once per loop, not per position).
///
/// Five bytes skip the frequent 4-byte boilerplate fragments so probes land
/// on structural repeats instead of recent junk. The full u64 load feeds the
/// multiplier directly: bits above the fifth byte only add input entropy.
#[inline(always)]
fn hash_at(win: &[u8], idx: usize) -> usize {
    // SAFETY: callers only hash positions with HASH_READ bytes of window
    // ahead (the scan tail guard and the emit insert bound); unaligned
    // because positions are byte-granular.
    unsafe {
        let v = win.as_ptr().add(idx).cast::<u64>().read_unaligned() & 0xFFFF_FFFF_FF;
        (v.wrapping_mul(0xC2B2_AE3D_27D4_EB4F) as usize >> (64 - HASH_LOG)) & ((1 << HASH_LOG) - 1)
    }
}

/// [`hash_at`] for a runtime hash-log (the chain strategies size their
/// tables per level).
#[inline(always)]
fn hash_at_log(win: &[u8], idx: usize, log: u32) -> usize {
    // SAFETY: same contract as hash_at.
    unsafe {
        let v = win.as_ptr().add(idx).cast::<u64>().read_unaligned() & 0xFFFF_FFFF_FF;
        (v.wrapping_mul(0xC2B2_AE3D_27D4_EB4F) as usize >> (64 - log)) & ((1usize << log) - 1)
    }
}

/// Hash the 8 bytes at `idx` into the dfast long table (libzstd's
/// `prime8bytes` multiply). Same read contract as [`hash_at_log`].
#[inline(always)]
fn hash8_at_log(win: &[u8], idx: usize, log: u32) -> usize {
    // SAFETY: same contract as hash_at_log.
    unsafe {
        let v = win.as_ptr().add(idx).cast::<u64>().read_unaligned();
        (v.wrapping_mul(0xCF1B_BCDC_B7A5_6463) as usize >> (64 - log)) & ((1usize << log) - 1)
    }
}

/// Read 4 window bytes at `idx`. Caller guarantees `idx + 4 <= win.len()`.
#[inline(always)]
fn read4(win: &[u8], idx: usize) -> u32 {
    // SAFETY: see contract above; unaligned because byte-granular.
    unsafe { win.as_ptr().add(idx).cast::<u32>().read_unaligned() }
}

/// Read 8 window bytes at `idx`. Caller guarantees `idx + 8 <= win.len()`.
#[inline(always)]
fn read8(win: &[u8], idx: usize) -> u64 {
    // SAFETY: see contract above; unaligned because byte-granular.
    unsafe { win.as_ptr().add(idx).cast::<u64>().read_unaligned() }
}

/// Longest common prefix of `win[i..]` and `win[j..]` in u64 chunks. `i` is
/// the current scan position and `j` a candidate strictly before it, so
/// bounding by `i` also bounds `j`.
#[inline(always)]
fn extend_match(win: &[u8], i: usize, j: usize) -> usize {
    let limit = win.len() - i;
    let base = win.as_ptr();
    let mut len = 0;
    // SAFETY: i and j are valid indices and i + len + 8 <= win.len() bounds
    // the reads on both sides (j <= i).
    unsafe {
        while len + 8 <= limit {
            let a = base.add(i + len).cast::<u64>().read_unaligned();
            let b = base.add(j + len).cast::<u64>().read_unaligned();
            if a == b {
                len += 8;
            } else {
                return len + ((a ^ b).trailing_zeros() >> 3) as usize;
            }
        }
    }
    while len < limit && win[i + len] == win[j + len] {
        len += 1;
    }
    len
}

/// Store `abs` as the newest position for its hash; `tag` is the caller's
/// `epoch << 48`. Caller guarantees `idx` has at least `MIN_HASH` bytes of
/// window behind it.
#[inline(always)]
fn insert_at(win: &[u8], table: &mut [u64], tag: u64, idx: usize, abs: u64) {
    let h = hash_at(win, idx);
    // SAFETY: the hash masks down to HASH_LOG bits and the table always
    // holds 1 << HASH_LOG slots, so the index cannot leave it.
    unsafe {
        *table.get_unchecked_mut(h) = tag | abs;
    }
}

/// Push one sequence's literals and packed code/add-bits streams, and
/// update the repeated-offset history. Shared by the fast and chain emit
/// paths; returns the literal length and the window-relative match end.
#[inline(always)]
#[allow(clippy::too_many_arguments)]
fn push_seq_packed(
    win: &[u8],
    win_base: u64,
    anchor: u64,
    start: usize,
    match_len: usize,
    of_value: u32,
    rep: &mut [u32; 3],
    literals: &mut Vec<u8>,
    seqs: &mut Vec<SeqWord>,
) -> (u32, usize) {
    let anchor_idx = (anchor - win_base) as usize;
    let ll = (start - anchor_idx) as u32;
    // Most sequences carry only a handful of literals; an out-of-line memcpy
    // per match costs more than the copy itself. Half the sequences on
    // structured data are repcode chains with no literals at all — their
    // copy is skipped outright.
    let lits = &win[anchor_idx..start];
    if ll == 0 {
        do_offset_history(of_value, 0, rep);
        seqs.push(pack_seq(0, match_len as u32, of_value));
        return (0, start + match_len);
    }
    // SAFETY: both sides are within the window / buffer; unaligned because
    // byte-granular. The 8-byte read may overlap the match that follows the
    // literals, so it stays in bounds whenever 8 bytes from the anchor fit
    // the window; the write may spill up to 7 bytes past the new length, but
    // reserve(8) covers them and later pushes overwrite them.
    unsafe {
        if lits.len() <= 8 && anchor_idx + 8 <= win.len() {
            literals.reserve(8);
            let dst = literals.as_mut_ptr().add(literals.len());
            let v = lits.as_ptr().cast::<u64>().read_unaligned();
            dst.cast::<u64>().write_unaligned(v);
            literals.set_len(literals.len() + lits.len());
        } else {
            literals.extend_from_slice(lits);
        }
    }
    do_offset_history(of_value, ll, rep);
    seqs.push(pack_seq(ll, match_len as u32, of_value));
    (ll, start + match_len)
}

/// Shared mutable state of the single-table strategies (fast and chain):
/// the head hash table, the output streams and the per-block constants,
/// bundled so the emit helpers stay inside the register argument budget
/// (their former twelve-to-fourteen-argument free signatures moved several
/// arguments through the stack on every call).
struct TableEmit<'a> {
    table: &'a mut [u64],
    literals: &'a mut Vec<u8>,
    seqs: &'a mut Vec<SeqWord>,
    /// `epoch << 48`, the constant half of every table entry this frame
    /// writes.
    tag: u64,
    win_base: u64,
    /// Last absolute position whose hash reads stay inside the window.
    insert_max: u64,
}

impl TableEmit<'_> {
    /// Emit the sequence for a match covering `match_len` bytes at window
    /// index `start`, update the repeated-offset history, index the covered
    /// range and return the new cursor (which is also the new anchor). The
    /// sequence is pushed straight into the packed code/add-bits streams the
    /// block encoder consumes — the raw (ll, ml, of) triple never gets its
    /// own buffer.
    #[inline]
    #[allow(clippy::too_many_arguments)]
    fn emit(
        &mut self,
        win: &[u8],
        anchor: u64,
        start: usize,
        match_len: usize,
        of_value: u32,
        rep: &mut [u32; 3],
    ) -> u64 {
        let (_ll, match_end) = push_seq_packed(
            win,
            self.win_base,
            anchor,
            start,
            match_len,
            of_value,
            rep,
            self.literals,
            self.seqs,
        );
        // Short matches keep every position (they carry the alignment coverage on
        // structured data). Long matches only index two anchors (zstd fast's fill
        // policy): one just inside the start, one just before the end — the scan
        // loop already indexes the positions it probes, so interior coverage only
        // needs seed points for the phases the scan skips over, and hashing a
        // 4-byte grid across long matches dominated encoder time. Both anchors
        // need HASH_READ bytes of window ahead; a match reaching the insert bound
        // simply leaves them out.
        if match_len <= 16 {
            let end = (self.win_base + match_end as u64).min(self.insert_max);
            let mut p = self.win_base + start as u64;
            while p < end {
                insert_at(win, self.table, self.tag, (p - self.win_base) as usize, p);
                p += 1;
            }
        } else {
            let base = self.win_base + start as u64;
            let hi = base + match_len as u64 - 2;
            if hi <= self.insert_max {
                let lo = base + 2;
                insert_at(win, self.table, self.tag, (lo - self.win_base) as usize, lo);
                if hi > lo {
                    insert_at(win, self.table, self.tag, (hi - self.win_base) as usize, hi);
                }
            }
        }
        self.win_base + match_end as u64
    }

    /// [`TableEmit::emit`] for the chain strategies: the covered range is
    /// indexed with complete hash head plus chain links (a coarse grid for
    /// long matches, so huge runs cannot dominate the hash work), keeping
    /// later chain walks connected to same-hash predecessors.
    #[inline]
    #[allow(clippy::too_many_arguments)]
    fn emit_chain(
        &mut self,
        win: &[u8],
        chain: &mut [u64],
        hash_log: u32,
        anchor: u64,
        start: usize,
        match_len: usize,
        of_value: u32,
        rep: &mut [u32; 3],
    ) -> u64 {
        let (_ll, match_end) = push_seq_packed(
            win,
            self.win_base,
            anchor,
            start,
            match_len,
            of_value,
            rep,
            self.literals,
            self.seqs,
        );
        let chain_mask = chain.len() - 1;
        let step = (if match_len <= 64 { 1 } else { 4 }) as u64;
        let end_abs = (self.win_base + match_end as u64).min(self.insert_max);
        let mut p = self.win_base + start as u64;
        while p < end_abs {
            let i = (p - self.win_base) as usize;
            let h = hash_at_log(win, i, hash_log);
            // SAFETY: h is masked to hash_log bits, p to the chain table
            // size. The chain slot key is the ABSOLUTE position — the walk
            // side resolves candidates absolutely: a window-relative index
            // diverges from it once win_base stops being a multiple of the
            // chain size (window compaction in streaming, per-block adopted
            // windows in bulk, job-relative windows in MT), scrambling every
            // walk past its first hop.
            unsafe {
                let head = *self.table.get_unchecked(h);
                *chain.get_unchecked_mut(p as usize & chain_mask) = head;
                *self.table.get_unchecked_mut(h) = self.tag | p;
            }
            p += step;
        }
        self.win_base + match_end as u64
    }

    /// Probe continuations at the second repeated offset immediately after a
    /// match (zstd fast's rep_offset2 loop). Alternating-period data chains
    /// rep0/rep1 matches back to back with zero literals; emitting with
    /// of_value 1 at ll == 0 swaps rep0/rep1, so the loop alternates distances
    /// on its own. Returns the cursor after the last chained match.
    #[inline]
    fn rep1_chain(&mut self, win: &[u8], pos: u64, block_end: u64, rep: &mut [u32; 3]) -> u64 {
        let mut pos = pos;
        while block_end - pos >= MIN_MATCH as u64 {
            let Some(cand_abs) = pos.checked_sub(rep[1] as u64) else {
                break;
            };
            if cand_abs < self.win_base {
                break;
            }
            let pidx = (pos - self.win_base) as usize;
            let cand = (cand_abs - self.win_base) as usize;
            if read4(win, cand) != read4(win, pidx) {
                break;
            }
            let ml = extend_match(win, pidx, cand);
            debug_assert!(ml >= MIN_MATCH);
            pos = self.emit(win, pos, pidx, ml, 1, rep);
        }
        pos
    }
}

/// Shared mutable state of the dfast strategy: the two tables, the output
/// streams and the per-block constants, bundled so the emit helpers stay
/// inside the register argument budget (their former fourteen-argument
/// signatures moved several arguments through the stack on every call).
struct DfastEmit<'a> {
    long: &'a mut [u64],
    small: &'a mut [u64],
    literals: &'a mut Vec<u8>,
    seqs: &'a mut Vec<SeqWord>,
    /// `epoch << 48`, the constant half of every table entry this frame
    /// writes.
    tag: u64,
    win_base: u64,
    /// Last window index whose hash reads stay inside the window.
    insert_max_idx: usize,
    long_log: u32,
    small_log: u32,
}

impl DfastEmit<'_> {
    /// Insert `idx` into both tables. Caller guarantees `idx + HASH_READ`
    /// bytes of window.
    #[inline(always)]
    fn insert_both(&mut self, win: &[u8], idx: usize) {
        // SAFETY: both hashes are masked to their tables' sizes.
        unsafe {
            let tag = self.tag | (self.win_base + idx as u64);
            *self
                .long
                .get_unchecked_mut(hash8_at_log(win, idx, self.long_log)) = tag;
            *self
                .small
                .get_unchecked_mut(hash_at_log(win, idx, self.small_log)) = tag;
        }
    }

    /// Emit one sequence (literals from `anchor_idx`, match at `start`) and
    /// seed libzstd's complementary anchors instead of indexing every
    /// covered position: both tables take the position two past the scan
    /// position the match was found at (`curr_idx + 2`), the long table the
    /// match end minus two, the small table the match end minus one.
    /// Anchors past `insert_max_idx` are dropped; the scan tail never
    /// probes them. Returns the new anchor as a window index.
    #[inline]
    #[allow(clippy::too_many_arguments)]
    fn emit(
        &mut self,
        win: &[u8],
        anchor_idx: usize,
        curr_idx: usize,
        start: usize,
        match_len: usize,
        of_value: u32,
        rep: &mut [u32; 3],
    ) -> usize {
        let (_ll, match_end) = push_seq_packed(
            win,
            self.win_base,
            self.win_base + anchor_idx as u64,
            start,
            match_len,
            of_value,
            rep,
            self.literals,
            self.seqs,
        );
        if curr_idx + 2 <= self.insert_max_idx {
            self.insert_both(win, curr_idx + 2);
        }
        if match_end >= 2 && match_end - 2 <= self.insert_max_idx {
            // SAFETY: masked to the long table size.
            unsafe {
                *self
                    .long
                    .get_unchecked_mut(hash8_at_log(win, match_end - 2, self.long_log)) =
                    self.tag | (self.win_base + (match_end - 2) as u64);
            }
        }
        if match_end >= 1 && match_end - 1 <= self.insert_max_idx {
            // SAFETY: masked to the small table size.
            unsafe {
                *self
                    .small
                    .get_unchecked_mut(hash_at_log(win, match_end - 1, self.small_log)) =
                    self.tag | (self.win_base + (match_end - 1) as u64);
            }
        }
        match_end
    }

    /// [`rep1_chain`] for the dfast strategy: each repcode position goes
    /// into both tables (no complementary anchors — libzstd's offset_2 loop
    /// only re-seeds the position it consumes). Returns the cursor as a
    /// window index, which is also the new anchor.
    fn rep_chain(
        &mut self,
        win: &[u8],
        pos_idx: usize,
        ilimit_idx: usize,
        rep: &mut [u32; 3],
    ) -> usize {
        let mut pos = pos_idx;
        while pos <= ilimit_idx {
            let Some(cand_abs) = (self.win_base + pos as u64).checked_sub(rep[1] as u64) else {
                break;
            };
            if cand_abs < self.win_base {
                break;
            }
            let cand = (cand_abs - self.win_base) as usize;
            if read4(win, cand) != read4(win, pos) {
                break;
            }
            let ml = extend_match(win, pos, cand);
            debug_assert!(ml >= MIN_MATCH);
            self.insert_both(win, pos);
            let (_ll, match_end) = push_seq_packed(
                win,
                self.win_base,
                self.win_base + pos as u64,
                pos,
                ml,
                1,
                rep,
                self.literals,
                self.seqs,
            );
            pos = match_end;
        }
        pos
    }
}

pub struct MatchGeneratorDriver {
    /// Contiguous history: `win[0]` is absolute position `win_base`.
    /// Capacity holds two windows plus one block, so compaction (which keeps
    /// MAX_WINDOW) runs at ~1x data volume amortized; blocks are read
    /// directly into the spare tail.
    win: Vec<u8>,
    /// Direct-window mode: the window points into caller-owned memory
    /// instead of `win` (see [`MatchGeneratorDriver::adopt_window`]).
    ext: Option<ExtWindow>,
    win_base: u64,
    /// Absolute end of committed data; matching runs up to `block_end`.
    pos: u64,
    block_end: u64,
    /// Start of the literals not yet covered by a sequence.
    anchor: u64,
    /// Absolute start of the last committed block (for `get_last_space`).
    block_start: u64,
    table: Vec<u64>,
    /// Second search table for the two-table strategies (the dfast short
    /// hash or the chain links; see [`Strategy`]), epoch-tagged like
    /// `table`; empty for the fast strategy.
    chain: Vec<u64>,
    epoch: u64,
    miss_count: usize,
    params: LevelParams,
    /// Repeated-offset history, kept in lockstep with the decoder's
    /// `offset_hist` so repcode probes see the same candidates it will.
    rep: [u32; 3],
    /// Literal-offset sequences still required before repcode references
    /// are decodable: 0 means the history above is known to match the
    /// decoder's. A job that starts mid-frame (see the mt module) cannot
    /// know the decoder's history; three literal offsets shift it fully
    /// into known territory because the update is a plain 3-slot shift.
    rep_pending: u8,
    slice_size: usize,
}

/// Borrowed window for the slice-compression path. The caller guarantees the
/// buffer stays alive and unmodified until the matcher is reset or dropped;
/// only the single thread that called `adopt_window` touches it.
struct ExtWindow {
    data: *const u8,
    len: usize,
}

// The pointer never escapes the matcher, and the compress_slice entry points
// keep the borrowed buffer on the same thread's stack for the whole call.
unsafe impl Send for MatchGeneratorDriver {}

/// Resolve the active window (owned or borrowed). A free function so callers
/// can split-borrow `win`/`ext` against `&mut table`.
fn window_slice<'a>(win: &'a Vec<u8>, ext: &'a Option<ExtWindow>) -> &'a [u8] {
    match ext {
        None => win,
        // SAFETY: the ExtWindow invariant (caller buffer alive and unchanged
        // until reset) makes this slice valid for as long as the matcher is.
        Some(e) => unsafe { core::slice::from_raw_parts(e.data, e.len) },
    }
}

impl MatchGeneratorDriver {
    /// The window size frames compressed at `level` declare in their
    /// header; usable without constructing an instance.
    pub fn window_for_level(level: Level) -> u64 {
        params_for_level(level).window as u64
    }

    /// Create a matcher whose blocks hold `slice_size` bytes of input (the
    /// zstd block maximum is 128 KiB).
    pub fn new(slice_size: usize) -> Self {
        // Two windows of capacity: compacting down to MAX_WINDOW then only
        // after another MAX_WINDOW of input means the copy_within runs at
        // ~1x data volume instead of once per block.
        Self {
            win: Vec::with_capacity(2 * MAX_WINDOW + slice_size),
            ext: None,
            win_base: 0,
            pos: 0,
            block_end: 0,
            anchor: 0,
            block_start: 0,
            table: alloc::vec![EMPTY; 1usize << HASH_LOG],
            chain: Vec::new(),
            // Epoch 0 is the never-valid state of a zeroed table.
            epoch: 1,
            miss_count: 0,
            params: FASTEST_PARAMS,
            rep: [1, 4, 8],
            rep_pending: 0,
            slice_size,
        }
    }

    /// Create a matcher for direct-window mode: no owned window is allocated
    /// and blocks are handed over through [`Self::adopt_window`] instead of
    /// [`Matcher::block_tail`].
    pub fn new_direct() -> Self {
        Self {
            win: Vec::new(),
            ext: None,
            win_base: 0,
            pos: 0,
            block_end: 0,
            anchor: 0,
            block_start: 0,
            table: alloc::vec![EMPTY; 1usize << HASH_LOG],
            chain: Vec::new(),
            epoch: 1,
            miss_count: 0,
            params: FASTEST_PARAMS,
            rep: [1, 4, 8],
            rep_pending: 0,
            slice_size: 0,
        }
    }

    /// Size the search tables for `level` (no-op when unchanged), so pooled
    /// states re-size at most once per level change.
    fn apply_level(&mut self, level: Level) {
        let params = params_for_level(level);
        if params != self.params {
            if params.hash_log != self.params.hash_log {
                self.table = alloc::vec![EMPTY; 1usize << params.hash_log];
            }
            self.chain = match params.strategy {
                Strategy::Fast => Vec::new(),
                Strategy::Dfast(small_log) => alloc::vec![EMPTY; 1usize << small_log],
                Strategy::Chain(chain_log) => alloc::vec![EMPTY; 1usize << chain_log],
            };
            // The owned window (streaming path) compacts down to the level's
            // window; grow the buffer so block_tail's set_len stays inside
            // the capacity. Direct-window matchers (slice size zero) never
            // use block_tail.
            if self.slice_size > 0 {
                let need = 2 * params.window + self.slice_size;
                if self.win.capacity() < need {
                    self.win.reserve(need - self.win.len());
                }
            }
            self.params = params;
        }
    }

    /// Point the window at caller-owned memory: `data` holds the bytes at
    /// absolute offset `base`. The caller must then declare the block to
    /// match with [`Self::set_block`]. `data` must stay alive and unmodified
    /// until the next `adopt_window` or `reset`.
    pub fn adopt_window(&mut self, data: &[u8], base: u64) {
        debug_assert!(self.ext.is_some() || self.win.is_empty());
        debug_assert!(!data.is_empty());
        self.ext = Some(ExtWindow {
            data: data.as_ptr(),
            len: data.len(),
        });
        self.win_base = base;
    }

    /// Declare `[start, end)` (absolute offsets inside the adopted window)
    /// as the block to match next. Matching restarts at `start`; the history
    /// below it is only a match source.
    pub fn set_block(&mut self, start: u64, end: u64) {
        debug_assert!(start >= self.win_base);
        debug_assert!(end <= self.win_base + self.ext.as_ref().map_or(0, |e| e.len) as u64);
        self.pos = start;
        self.block_start = start;
        self.block_end = end;
        self.anchor = start;
    }

    /// Forbid repcode references until three literal-offset sequences have
    /// rewritten the repeated-offset history. Used by multithreaded jobs
    /// that start mid-frame: the decoder's history there is unknown, and a
    /// repcode reference to a stale value would copy from the wrong place.
    /// Mirrors libzstd's `ZSTD_invalidateRepCodes`.
    pub fn gate_repcodes(&mut self) {
        self.rep_pending = 3;
    }

    #[inline(always)]
    fn idx_of(&self, abs: u64) -> usize {
        (abs - self.win_base) as usize
    }
}

impl Matcher for MatchGeneratorDriver {
    fn reset(&mut self, level: Level) {
        self.apply_level(level);
        self.ext = None;
        self.win.clear();
        self.win_base = 0;
        self.pos = 0;
        self.block_end = 0;
        self.anchor = 0;
        self.block_start = 0;
        // Stale entries from previous frames fail the epoch check.
        self.epoch += 1;
        if self.epoch > 0xFFFF {
            self.table.fill(EMPTY);
            if !self.chain.is_empty() {
                self.chain.fill(EMPTY);
            }
            self.epoch = 1;
        }
        self.miss_count = 0;
        // Matches the decoder's per-frame offset_hist reset.
        self.rep = [1, 4, 8];
        self.rep_pending = 0;
    }

    fn window_size(&self) -> u64 {
        self.params.window as u64
    }

    fn repcode_snapshot(&self) -> [u32; 3] {
        self.rep
    }

    fn restore_repcode(&mut self, rep: [u32; 3]) {
        self.rep = rep;
    }

    fn block_tail(&mut self) -> &mut [u8] {
        debug_assert!(self.ext.is_none(), "block_tail is owned-window only");
        if self.win.len() + self.slice_size > self.win.capacity() {
            let keep = self.win.len().min(self.params.window);
            let drop = self.win.len() - keep;
            self.win.copy_within(drop.., 0);
            self.win.truncate(keep);
            self.win_base += drop as u64;
        }
        let old = self.win.len();
        // SAFETY: the capacity check above guarantees old + slice_size fits.
        // The caller writes the block into the returned slice and passes the
        // written count to commit_block, which shrinks the length back to
        // exactly that, so the never-written tail bytes are never observed.
        unsafe { self.win.set_len(old + self.slice_size) };
        &mut self.win[old..]
    }

    fn get_last_space(&mut self) -> &[u8] {
        &window_slice(&self.win, &self.ext)[self.idx_of(self.block_start)..]
    }

    fn commit_block(&mut self, read_bytes: usize) {
        debug_assert!(self.ext.is_none(), "commit_block is owned-window only");
        let old_len = self.win.len() - self.slice_size;
        // SAFETY: the caller wrote read_bytes bytes into the tail slice
        // handed out by block_tail.
        unsafe { self.win.set_len(old_len + read_bytes) };
        // Matching restarts at the head of the freshly committed block; the
        // window behind it is only a match source.
        self.pos = self.win_base + old_len as u64;
        self.block_start = self.pos;
        self.block_end = self.win_base + self.win.len() as u64;
        self.anchor = self.pos;
    }

    fn start_matching(&mut self, mut handle_sequence: impl for<'a> FnMut(Sequence<'a>)) {
        let mut literals = Vec::new();
        let mut seqs: Vec<SeqWord> = Vec::new();
        self.start_matching_codes(&mut literals, &mut seqs);
        // Rebuild the interleaved callback order from the collected
        // buffers: each sequence's ll literals came right before it.
        // Zero-sequence blocks staged nothing; their literals are the block
        // itself, handed over straight from the window.
        let zero_seq = seqs.is_empty();
        let mut offset = 0usize;
        for &word in seqs.iter() {
            let (ll, ml, of) = decode_packed(word.codes, word.add);
            let lits = &literals[offset..offset + ll as usize];
            offset += ll as usize;
            handle_sequence(Sequence::Triple {
                literals: lits,
                offset: of as usize,
                match_len: ml as usize,
            });
        }
        if zero_seq {
            handle_sequence(Sequence::Literals {
                literals: self.get_last_space(),
            });
        } else {
            handle_sequence(Sequence::Literals {
                literals: &literals[offset..],
            });
        }
    }

    fn start_matching_codes(&mut self, literals: &mut Vec<u8>, seqs: &mut Vec<SeqWord>) {
        match self.params.strategy {
            Strategy::Fast => self.start_matching_fast(literals, seqs),
            Strategy::Dfast(_) => self.start_matching_dfast(literals, seqs),
            Strategy::Chain(_) => self.start_matching_chain(literals, seqs),
        }
    }

    fn skip_matching(&mut self) {
        // Only called for RLE blocks so far: every 5-byte window in a
        // uniform run hashes to the same slot, so indexing each byte just
        // rewrites one table entry. The first position covers that slot;
        // future probes into the run resolve through it or the repcode
        // chain.
        let idx = (self.block_start - self.win_base) as usize;
        let win = window_slice(&self.win, &self.ext);
        if idx + HASH_READ <= win.len() {
            match self.params.strategy {
                Strategy::Chain(log) => {
                    let h = hash_at_log(win, idx, log);
                    // SAFETY: h is masked to log bits, the absolute block
                    // start to the chain size (absolute key; see
                    // emit_chain's note on the walk side's indexing).
                    unsafe {
                        let head = *self.table.get_unchecked(h);
                        let chain_mask = self.chain.len() - 1;
                        *self
                            .chain
                            .get_unchecked_mut(self.block_start as usize & chain_mask) = head;
                        *self.table.get_unchecked_mut(h) = (self.epoch << 48) | self.block_start;
                    }
                }
                Strategy::Dfast(small_log) => {
                    let hl = hash8_at_log(win, idx, self.params.hash_log);
                    let hs = hash_at_log(win, idx, small_log);
                    // SAFETY: both hashes masked to their tables' sizes.
                    unsafe {
                        let tag = (self.epoch << 48) | self.block_start;
                        *self.table.get_unchecked_mut(hl) = tag;
                        *self.chain.get_unchecked_mut(hs) = tag;
                    }
                }
                Strategy::Fast => {
                    insert_at(
                        win,
                        &mut self.table,
                        self.epoch << 48,
                        idx,
                        self.block_start,
                    );
                }
            }
        }
        self.pos = self.block_end;
        self.anchor = self.block_end;
    }
}

impl MatchGeneratorDriver {
    /// The single-probe `fast` strategy loop (level [`Level::Fastest`]).
    fn start_matching_fast(&mut self, literals: &mut Vec<u8>, seqs: &mut Vec<SeqWord>) {
        // Hot state lives in locals for the whole loop: the emit helpers
        // used to take `&mut self`, which forced a reload of every cursor
        // from memory after each match.
        let win = window_slice(&self.win, &self.ext);
        let epoch = self.epoch;
        let tag = epoch << 48;
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
        let table_ptr: *mut u64 = self.table.as_mut_ptr();
        let mut emit = TableEmit {
            table: &mut self.table[..],
            literals,
            seqs,
            tag,
            win_base,
            insert_max,
        };
        let max_window = MAX_WINDOW as u64;
        let mut pos = self.pos;
        let mut anchor = self.anchor;
        let mut miss_count = self.miss_count;
        let mut rep = self.rep;
        let mut rep_pending = self.rep_pending;
        let hash_read = HASH_READ as u64;

        // Resolve a table entry to a window index: invalid entries (wrong
        // epoch, or older than the level window / the live window buffer)
        // alias the scanning position itself, whose bytes always compare
        // equal — the byte compare plus `cand != ip` then rejects them with
        // one predictable branch instead of a three-way check per probe
        // (libzstd's selectAddr trick). Entries are always strictly older
        // than the scanning position: the pair reads both entries before
        // its stores, and every insert sits at or behind the cursor when it
        // happens.
        //
        // SAFETY: none needed — the select keeps the index inside the
        // window either way.
        let resolve = |entry: u64, lo: u64, ip: usize| -> usize {
            let cand_abs = entry & POS_MASK;
            if (entry >> 48 == epoch) & (cand_abs >= lo) {
                (cand_abs - win_base) as usize
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
        'restart: while block_end.saturating_sub(pos) >= hash_read {
            let idx0 = (pos - win_base) as usize;
            let h0 = hash_at(win, idx0);
            // SAFETY: the hash masks to HASH_LOG bits and the table always
            // holds 1 << HASH_LOG slots (see insert_at).
            let prev0 = unsafe { *table_ptr.add(h0) };
            let cur0 = read4(win, idx0);
            let mut pair_len = 1u64;
            let mut idx1 = idx0;
            let mut h1 = h0;
            let mut prev1 = prev0;
            let mut cur1 = cur0;
            if block_end - pos - 1 >= hash_read {
                pair_len = 2;
                idx1 = idx0 + 1;
                h1 = hash_at(win, idx1);
                // SAFETY: as above.
                prev1 = unsafe { *table_ptr.add(h1) };
                cur1 = read4(win, idx1);
            }
            // Store after both lookups so each probe sees the pre-store
            // entry (newest-wins).
            // SAFETY: as above.
            unsafe {
                *table_ptr.add(h0) = tag | pos;
            }
            if pair_len == 2 {
                // SAFETY: as above.
                unsafe {
                    *table_ptr.add(h1) = tag | (pos + 1);
                }
            }

            // Probe the first position. Repcode candidate first (mirrors
            // zstd's fast strategy: rep[0] only); a repcode match needs at
            // least one pending literal so of_value 1 stays encodable: with
            // literals pending the probe runs at the current position,
            // otherwise one byte ahead so that byte becomes the literal.
            {
                // Absolute lower bound of rep[0] candidates; rep[0] only
                // changes inside emits, which re-enter the loop.
                let rep0_lim = win_base + rep[0] as u64;
                // A gated job start must not probe repcodes (unknown decoder
                // history); probe 0 sits below the bound, which folds the
                // underflow guard into the same single comparison.
                let probe = if rep_pending != 0 {
                    0
                } else if pos == anchor {
                    pos + 1
                } else {
                    pos
                };
                if probe >= rep0_lim {
                    let mut cand = (probe - rep0_lim) as usize;
                    let pidx = (probe - win_base) as usize;
                    if read4(win, cand) == read4(win, pidx) {
                        let mut ml = extend_match(win, pidx, cand);
                        if ml >= MIN_MATCH {
                            let anchor_idx = (anchor - win_base) as usize;
                            let mut start = pidx;
                            // Extend backwards into the pending literals;
                            // the offset (pidx - cand) stays constant.
                            while start > anchor_idx + 1
                                && cand > 0
                                && win[cand - 1] == win[start - 1]
                            {
                                cand -= 1;
                                start -= 1;
                                ml += 1;
                            }
                            anchor = emit.emit(win, anchor, start, ml, 1, &mut rep);
                            pos = emit.rep1_chain(win, anchor, block_end, &mut rep);
                            // The chain's matches advance the cursor too.
                            anchor = pos;
                            miss_count = 0;
                            continue 'restart;
                        }
                    }
                }

                let lo = pos.saturating_sub(max_window).max(win_base);
                let mut cand0 = resolve(prev0, lo, idx0);
                if cand0 != idx0 && read4(win, cand0) == cur0 {
                    let mut ml = extend_match(win, idx0, cand0);
                    // A hash match already spans 5 bytes; below 6 the
                    // sequence overhead roughly equals the literals
                    // it covers, and rejecting it lets the scan try
                    // the next position where a longer match may
                    // start.
                    if ml >= 6 {
                        let anchor_idx = (anchor - win_base) as usize;
                        let mut start = idx0;
                        // Extend backwards into the pending literals;
                        // the offset (idx0 - cand) stays constant.
                        while start > anchor_idx && cand0 > 0 && win[cand0 - 1] == win[start - 1] {
                            cand0 -= 1;
                            start -= 1;
                            ml += 1;
                        }
                        let of_value = (start - cand0 + 3) as u32;
                        anchor = emit.emit(win, anchor, start, ml, of_value, &mut rep);
                        // A literal offset shifts the decoder's history
                        // one slot down; after the third one a job-start
                        // gate has fully converged and repcode use is
                        // safe again.
                        if rep_pending != 0 {
                            rep_pending -= 1;
                        }
                        pos = if rep_pending == 0 {
                            emit.rep1_chain(win, anchor, block_end, &mut rep)
                        } else {
                            anchor
                        };
                        anchor = pos;
                        miss_count = 0;
                        continue 'restart;
                    }
                }
            }

            // Probe the second position through the entry prepared above.
            if pair_len == 2 {
                let pos1 = pos + 1;
                // Absolute lower bound of rep[0] candidates (see above).
                let rep0_lim = win_base + rep[0] as u64;
                // Gated job starts skip the repcode probe (see above).
                let probe = if rep_pending != 0 {
                    0
                } else if pos1 == anchor {
                    pos1 + 1
                } else {
                    pos1
                };
                if probe >= rep0_lim {
                    let mut cand = (probe - rep0_lim) as usize;
                    let pidx = (probe - win_base) as usize;
                    if read4(win, cand) == read4(win, pidx) {
                        let mut ml = extend_match(win, pidx, cand);
                        if ml >= MIN_MATCH {
                            let anchor_idx = (anchor - win_base) as usize;
                            let mut start = pidx;
                            while start > anchor_idx + 1
                                && cand > 0
                                && win[cand - 1] == win[start - 1]
                            {
                                cand -= 1;
                                start -= 1;
                                ml += 1;
                            }
                            anchor = emit.emit(win, anchor, start, ml, 1, &mut rep);
                            pos = emit.rep1_chain(win, anchor, block_end, &mut rep);
                            anchor = pos;
                            miss_count = 0;
                            continue 'restart;
                        }
                    }
                }

                let lo1 = pos1.saturating_sub(max_window).max(win_base);
                let mut cand1 = resolve(prev1, lo1, idx1);
                if cand1 != idx1 && read4(win, cand1) == cur1 {
                    let mut ml = extend_match(win, idx1, cand1);
                    if ml >= 6 {
                        let anchor_idx = (anchor - win_base) as usize;
                        let mut start = idx1;
                        while start > anchor_idx && cand1 > 0 && win[cand1 - 1] == win[start - 1] {
                            cand1 -= 1;
                            start -= 1;
                            ml += 1;
                        }
                        let of_value = (start - cand1 + 3) as u32;
                        anchor = emit.emit(win, anchor, start, ml, of_value, &mut rep);
                        // A literal offset shifts the decoder's history
                        // one slot down; after the third one a job-start
                        // gate has fully converged and repcode use is
                        // safe again.
                        if rep_pending != 0 {
                            rep_pending -= 1;
                        }
                        pos = if rep_pending == 0 {
                            emit.rep1_chain(win, anchor, block_end, &mut rep)
                        } else {
                            anchor
                        };
                        anchor = pos;
                        miss_count = 0;
                        continue 'restart;
                    }
                }
            }

            // Both positions missed: grow the probe step on long literal
            // runs so incompressible data does not pay a full hash per byte.
            // The step scales the whole pair so the probes-per-byte density
            // matches the single-position loop at every step size.
            miss_count += pair_len as usize;
            let step = 1 + (miss_count >> 2).min(255) as u64;
            pos += pair_len * step;
        }
        if !emit.seqs.is_empty() && anchor < block_end {
            let tail = (anchor - win_base) as usize..(block_end - win_base) as usize;
            emit.literals.extend_from_slice(&win[tail]);
        }
        // A zero-sequence block stages nothing: its literals are exactly the
        // block, which the caller reads through get_last_space() instead of
        // paying a whole-block copy into and out of the scratch buffer.
        self.pos = block_end;
        self.anchor = block_end;
        self.miss_count = miss_count;
        self.rep = rep;
        self.rep_pending = rep_pending;
    }

    /// The double-hash strategy loop (level [`Level::Fast`], libzstd's
    /// dfast): one 8-byte long-hash probe and one 5-byte short-hash probe
    /// per position — no chain walk, no lazy deferral. A two-position
    /// pipeline overlaps the hash multiplies and table loads, a short hit
    /// is upgraded by the long probe prepared for the next position, and
    /// matched ranges re-seed both tables through a few anchors (see
    /// [`DfastEmit::emit`]). The miss step only grows every 256 skipped
    /// positions (libzstd's `kSearchStrength`).
    #[allow(clippy::too_many_lines)]
    fn start_matching_dfast(&mut self, literals: &mut Vec<u8>, seqs: &mut Vec<SeqWord>) {
        let win = window_slice(&self.win, &self.ext);
        let long_log = self.table.len().trailing_zeros();
        let small_log = self.chain.len().trailing_zeros();
        let epoch = self.epoch;
        let win_base = self.win_base;
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
        let mut anchor_idx = (self.anchor - win_base) as usize;
        let mut ip_idx = (self.pos - win_base) as usize;
        // The scan's own table writes go through raw pointers: routing them
        // through the emit context's slice fields kept the pointers,
        // log shifts and tag stack-resident (a re-load per access, the
        // classic SROA failure at this loop's live-value count).
        // SAFETY: derived here, before the context below takes its
        // borrows; both address the same memory, and the loop and the
        // emit helpers never access a slot concurrently.
        let long_ptr: *mut u64 = self.table.as_mut_ptr();
        let small_ptr: *mut u64 = self.chain.as_mut_ptr();
        let tag = epoch << 48;
        let mut emit = DfastEmit {
            long: &mut self.table[..],
            small: &mut self.chain[..],
            literals,
            seqs,
            tag,
            win_base,
            insert_max_idx,
            long_log,
            small_log,
        };

        // Resolve a table entry to a window index: invalid entries (wrong
        // epoch, or older than the level window / the live window buffer)
        // alias the scanning position itself, whose bytes always compare
        // equal — the byte compare plus `cand != ip_idx` then rejects them
        // with predictable branches instead of a three-way check per probe
        // (libzstd's selectAddr trick). Entries are always strictly older
        // than the scanning position: every insert happens at or ahead of
        // the cursor, and reads see the pre-insert value.
        //
        // SAFETY: none needed — the select keeps the index inside the
        // window either way.
        let resolve = |entry: u64, lo: u64, ip: usize| -> usize {
            let cand_abs = entry & POS_MASK;
            if (entry >> 48 == epoch) & (cand_abs >= lo) {
                (cand_abs - win_base) as usize
            } else {
                ip
            }
        };

        // Outer loop: one pass per emitted match; re-entering resets the
        // miss step. The inner loop walks single positions until a match
        // or the block tail.
        'outer: loop {
            // The short-match upgrade probes at ip + 1, so a pass needs a
            // full position pair inside the block.
            if ip_idx + 1 > limit_idx {
                break;
            }
            let mut step = 1usize;
            let mut next_step = ip_idx + 256;
            let mut ip1_idx = ip_idx + 1;
            let mut hl0 = hash8_at_log(win, ip_idx, long_log);
            // SAFETY: hl0 is masked to the long table size.
            let mut entry_l0 = unsafe { *long_ptr.add(hl0) };

            loop {
                let hs0 = hash_at_log(win, ip_idx, small_log);
                // SAFETY: hs0 is masked to the small table size.
                let entry_s0 = unsafe { *small_ptr.add(hs0) };
                let pos_abs = win_base + ip_idx as u64;
                // Oldest usable candidate age: within the level window and
                // inside the live window buffer.
                let lo = pos_abs.saturating_sub(max_window).max(win_base);
                // Insert after both lookups, before probing (newest-wins).
                // SAFETY: both hashes masked to their tables' sizes.
                unsafe {
                    let tagged = tag | pos_abs;
                    *long_ptr.add(hl0) = tagged;
                    *small_ptr.add(hs0) = tagged;
                }

                // Repcode pre-probe one byte ahead: the probed byte stays a
                // pending literal, so of_value 1 encodes rep0 instead of a
                // swap, and no backward extension may consume it. Gated job
                // starts skip the probe (unknown decoder history).
                if rep_pending == 0 {
                    let probe = ip_idx + 1;
                    if let Some(cand_abs) = (win_base + probe as u64).checked_sub(rep[0] as u64) {
                        if cand_abs >= win_base {
                            let cand = (cand_abs - win_base) as usize;
                            if read4(win, cand) == read4(win, probe) {
                                let ml = extend_match(win, probe, cand);
                                debug_assert!(ml >= MIN_MATCH);
                                anchor_idx =
                                    emit.emit(win, anchor_idx, ip_idx, probe, ml, 1, &mut rep);
                                ip_idx = emit.rep_chain(win, anchor_idx, limit_idx, &mut rep);
                                // The chain's matches advance the anchor too.
                                anchor_idx = ip_idx;
                                continue 'outer;
                            }
                        }
                    }
                }

                let hl1 = hash8_at_log(win, ip1_idx, long_log);

                // Long probe: a full 8-byte match at the long-hash candidate.
                {
                    let cand = resolve(entry_l0, lo, ip_idx);
                    if cand != ip_idx && read8(win, cand) == read8(win, ip_idx) {
                        let mut start = ip_idx;
                        let mut c = cand;
                        let mut ml = extend_match(win, ip_idx, cand);
                        // Backward catch-up into the pending literals; the
                        // offset (start - c) stays constant.
                        while start > anchor_idx && c > 0 && win[c - 1] == win[start - 1] {
                            c -= 1;
                            start -= 1;
                            ml += 1;
                        }
                        let of_value = (start - c + 3) as u32;
                        anchor_idx =
                            emit.emit(win, anchor_idx, ip_idx, start, ml, of_value, &mut rep);
                        if step < 4 {
                            // ip1 lies inside the covered range whenever
                            // step < 4 (matches are at least 4 long), so
                            // indexing it cannot pollute later probes.
                            // SAFETY: hl1 is masked to the long table size.
                            unsafe {
                                *long_ptr.add(hl1) = tag | (win_base + ip1_idx as u64);
                            }
                        }
                        // A literal offset shifts the decoder's history one
                        // slot down; after the third one a job-start gate
                        // has fully converged and repcode use is safe
                        // again.
                        if rep_pending != 0 {
                            rep_pending -= 1;
                        }
                        ip_idx = if rep_pending == 0 {
                            emit.rep_chain(win, anchor_idx, limit_idx, &mut rep)
                        } else {
                            anchor_idx
                        };
                        // The chain's matches advance the anchor too.
                        anchor_idx = ip_idx;
                        continue 'outer;
                    }
                }

                // SAFETY: hl1 is masked to the long table size.
                let entry_l1 = unsafe { *long_ptr.add(hl1) };

                // Short probe: 4 bytes at the short-hash candidate, upgraded
                // by the long probe prepared for the next position.
                {
                    let cand = resolve(entry_s0, lo, ip_idx);
                    if cand != ip_idx && read4(win, cand) == read4(win, ip_idx) {
                        let mut start = ip_idx;
                        let mut c = cand;
                        let mut ml = extend_match(win, ip_idx, cand);
                        let lo1 = (win_base + ip1_idx as u64)
                            .saturating_sub(max_window)
                            .max(win_base);
                        let c1 = resolve(entry_l1, lo1, ip1_idx);
                        if c1 != ip1_idx && read8(win, c1) == read8(win, ip1_idx) {
                            let l1len = extend_match(win, ip1_idx, c1);
                            if l1len > ml {
                                start = ip1_idx;
                                c = c1;
                                ml = l1len;
                            }
                        }
                        while start > anchor_idx && c > 0 && win[c - 1] == win[start - 1] {
                            c -= 1;
                            start -= 1;
                            ml += 1;
                        }
                        let of_value = (start - c + 3) as u32;
                        anchor_idx =
                            emit.emit(win, anchor_idx, ip_idx, start, ml, of_value, &mut rep);
                        if step < 4 {
                            // SAFETY: hl1 is masked to the long table size;
                            // see the long probe above.
                            unsafe {
                                *long_ptr.add(hl1) = tag | (win_base + ip1_idx as u64);
                            }
                        }
                        if rep_pending != 0 {
                            rep_pending -= 1;
                        }
                        ip_idx = if rep_pending == 0 {
                            emit.rep_chain(win, anchor_idx, limit_idx, &mut rep)
                        } else {
                            anchor_idx
                        };
                        // The chain's matches advance the anchor too.
                        anchor_idx = ip_idx;
                        continue 'outer;
                    }
                }

                // Miss: advance the pair; the step only grows every 256
                // skipped positions, so compressible data keeps probing
                // every byte while incompressible runs accelerate.
                if ip1_idx >= next_step {
                    step += 1;
                    next_step += 256;
                }
                ip_idx = ip1_idx;
                ip1_idx += step;
                hl0 = hl1;
                entry_l0 = entry_l1;
                if ip1_idx > limit_idx {
                    break;
                }
            }
            // The pair left the block: nothing left to probe.
            break;
        }
        if !emit.seqs.is_empty() && anchor_idx < block_len {
            emit.literals.extend_from_slice(&win[anchor_idx..block_len]);
        }
        self.pos = self.block_end;
        self.anchor = self.block_end;
        self.rep = rep;
        self.rep_pending = rep_pending;
        // dfast tracks misses through the per-match step counter, not the
        // cross-block miss count.
        self.miss_count = 0;
    }

    /// The hash-chain strategy loop (levels above [`Level::Fastest`]):
    /// walk same-hash candidates through the chain table up to the level's
    /// search depth, prefer repcode candidates (they encode nearly free),
    /// and defer emission across up to `lazy_depth` further positions when
    /// a longer match may start there — libzstd's lazy family.
    fn start_matching_chain(&mut self, literals: &mut Vec<u8>, seqs: &mut Vec<SeqWord>) {
        let win = window_slice(&self.win, &self.ext);
        let chain = &mut self.chain[..];
        let chain_mask = chain.len() - 1;
        let epoch = self.epoch;
        let tag = epoch << 48;
        let win_base = self.win_base;
        let block_end = self.block_end;
        let hash_log = self.params.hash_log;
        let search_depth = self.params.search_depth as usize;
        let lazy_depth = self.params.lazy_depth;
        let max_window = self.params.window as u64;
        let insert_max = win_base + win.len().saturating_sub(HASH_READ) as u64;
        // The scan's own table accesses go through a raw pointer (see the
        // fast loop's note on the same SROA failure); emits go through the
        // context.
        // SAFETY: derived here, before the context below takes its borrow;
        // both address the same memory, and the loop and the emit helpers
        // never access a slot concurrently. The table is never resized.
        let table_ptr: *mut u64 = self.table.as_mut_ptr();
        let mut emit = TableEmit {
            table: &mut self.table[..],
            literals,
            seqs,
            tag,
            win_base,
            insert_max,
        };
        let hash_read = HASH_READ as u64;
        let mut pos = self.pos;
        let mut anchor = self.anchor;
        let mut rep = self.rep;
        let mut rep_pending = self.rep_pending;
        let mut miss_count = self.miss_count;

        // Chain-walk search from the hash head at window index `idx`,
        // returning the longest match's (length, candidate window index).
        // Every candidate is read4-verified, so stale chain links (see the
        // sparse fill in rep1_chain) only cost probes, never correctness.
        let search = |win: &[u8], chain: &[u64], idx: usize| -> (usize, usize) {
            // SAFETY: hash_at_log masks to hash_log bits and the table holds
            // 1 << hash_log slots.
            let h = hash_at_log(win, idx, hash_log);
            let mut entry = unsafe { *table_ptr.add(h) };
            let cur4 = read4(win, idx);
            let pos_abs = win_base + idx as u64;
            let mut best_len = 0usize;
            let mut best_cand = usize::MAX;
            let mut tried = 0usize;
            while tried < search_depth {
                if entry >> 48 != epoch {
                    break;
                }
                let cand_abs = entry & POS_MASK;
                if cand_abs < win_base || pos_abs - cand_abs > max_window {
                    break;
                }
                let cand = (cand_abs - win_base) as usize;
                if read4(win, cand) == cur4 {
                    let ml = extend_match(win, idx, cand);
                    if ml > best_len {
                        best_len = ml;
                        best_cand = cand;
                        // Cannot be improved on within this block.
                        if pos_abs + ml as u64 >= block_end {
                            break;
                        }
                    }
                }
                tried += 1;
                // SAFETY: masked to the chain table size.
                entry = unsafe { *chain.get_unchecked(cand_abs as usize & chain_mask) };
            }
            (best_len, best_cand)
        };

        while block_end.saturating_sub(pos) >= hash_read {
            let idx = (pos - win_base) as usize;
            let (mut best_len, mut best_cand) = search(win, chain, idx);

            // Repcode probe first when armed: with literals pending it runs
            // at the current position, otherwise one byte ahead so that
            // byte becomes the pending literal and of_value 1 stays
            // encodable (mirrors the fast loop). The offset encodes nearly
            // free, so bias it past the chain match.
            let mut rep_hit = false;
            if rep_pending == 0 {
                let probe = if pos == anchor { pos + 1 } else { pos };
                if let Some(cand_abs) = probe.checked_sub(rep[0] as u64) {
                    if cand_abs >= win_base {
                        let pidx = (probe - win_base) as usize;
                        let cand = (cand_abs - win_base) as usize;
                        if read4(win, cand) == read4(win, pidx) {
                            let ml = extend_match(win, pidx, cand);
                            if ml >= MIN_MATCH && ml + 3 > best_len {
                                best_len = ml;
                                best_cand = cand;
                                rep_hit = true;
                                pos = probe;
                            }
                        }
                    }
                }
            }

            // Insert this position behind the probe (newest-wins), linking
            // the chain to the previous head. The chain slot key is the
            // absolute position — what the walk resolves candidates with.
            // SAFETY: both indices are masked to their tables' sizes.
            unsafe {
                let h = hash_at_log(win, idx, hash_log);
                let head = *table_ptr.add(h);
                *chain.get_unchecked_mut(pos as usize & chain_mask) = head;
                *table_ptr.add(h) = tag | pos;
            }

            if best_len < MIN_MATCH {
                // Grow the probe step on long literal runs (same policy as
                // the fast loop) so incompressible data does not pay a full
                // chain walk per byte.
                miss_count += 1;
                pos += 1 + (miss_count >> 2).min(255) as u64;
                continue;
            }
            miss_count = 0;

            // Lazy evaluation: a longer match starting a few positions later
            // is worth the literals skipped on the way. Long-enough matches
            // skip straight to emission.
            let mut lazy_shift = 0u32;
            if best_len < 64 {
                for step in 1..=lazy_depth {
                    let p2 = pos + step as u64;
                    if block_end.saturating_sub(p2) < hash_read {
                        break;
                    }
                    let idx2 = (p2 - win_base) as usize;
                    let (len2, cand2) = search(win, chain, idx2);
                    if len2 > best_len {
                        best_len = len2;
                        best_cand = cand2;
                        rep_hit = false;
                        lazy_shift = step;
                    } else {
                        break;
                    }
                }
            }
            pos += lazy_shift as u64;
            let mut start = (pos - win_base) as usize;

            // Backward extension into the pending literals; the offset
            // (start - cand) stays constant. A repcode emission must keep
            // one literal pending: of_value 1 with a zero literal length
            // resolves to a repcode *swap* on the decoder side, not rep0.
            let anchor_idx = (anchor - win_base) as usize;
            let floor = if rep_hit { anchor_idx + 1 } else { anchor_idx };
            let mut cand = best_cand;
            let mut ml = best_len;
            while start > floor && cand > 0 && win[cand - 1] == win[start - 1] {
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
            if rep_pending != 0 && of_value > 3 {
                rep_pending -= 1;
            }
            if rep_pending == 0 {
                pos = emit.rep1_chain(win, anchor, block_end, &mut rep);
            } else {
                pos = anchor;
            }
            anchor = pos;
        }
        if !emit.seqs.is_empty() && anchor < block_end {
            let tail = (anchor - win_base) as usize..(block_end - win_base) as usize;
            emit.literals.extend_from_slice(&win[tail]);
        }
        self.pos = block_end;
        self.anchor = block_end;
        self.miss_count = miss_count;
        self.rep = rep;
        self.rep_pending = rep_pending;
    }
}

#[cfg(test)]
mod tests {
    use super::MatchGeneratorDriver;
    use crate::encoding::{Matcher, Sequence};
    use alloc::vec::Vec;

    fn block_label(i: usize) -> Vec<u8> {
        // "block N filler text; " without needing format! in no_std tests
        let mut v = Vec::new();
        v.extend_from_slice(b"block ");
        v.push(b'0' + i as u8);
        v.extend_from_slice(b" filler text; ");
        v
    }

    /// Feed `data` through the matcher one block at a time and reconstruct the
    /// original from the emitted sequences.
    fn match_and_reconstruct(data: &[u8], block_size: usize) -> Vec<u8> {
        let mut driver = MatchGeneratorDriver::new(block_size);
        driver.reset(crate::Level::Fastest);
        // Offset history mirrors the decoder's per-frame scratch.
        let mut rep = [1u32, 4, 8];
        let mut reconstructed = Vec::new();
        for block in data.chunks(block_size) {
            driver.block_tail()[..block.len()].copy_from_slice(block);
            driver.commit_block(block.len());
            driver.start_matching(|seq| match seq {
                Sequence::Literals { literals } => reconstructed.extend_from_slice(literals),
                Sequence::Triple {
                    literals,
                    offset,
                    match_len,
                } => {
                    reconstructed.extend_from_slice(literals);
                    let actual = crate::decoding::sequence_execution::do_offset_history(
                        offset as u32,
                        literals.len() as u32,
                        &mut rep,
                    );
                    // Matches may overlap their own output (offset < match_len).
                    let start = reconstructed.len() - actual as usize;
                    for i in 0..match_len {
                        let b = reconstructed[start + i];
                        reconstructed.push(b);
                    }
                }
            });
        }
        reconstructed
    }

    #[test]
    fn reconstructs_short_runs() {
        let mut data = Vec::new();
        data.extend([0u8; 16]);
        data.extend([1u8, 2, 3, 4, 5, 6]);
        data.extend([1u8, 2, 3, 4, 5, 6]);
        data.extend([0u8; 8]);
        assert_eq!(match_and_reconstruct(&data, 8), data);
        assert_eq!(match_and_reconstruct(&data, 4), data);
    }

    #[test]
    fn reconstructs_across_blocks() {
        // Matches must reach into previous blocks through the shared window.
        let mut data = Vec::new();
        for i in 0..10 {
            data.extend_from_slice(&[0xA5, 0x5A, 0xC3, 0x3C, 0x99, 0x66, 0xF0, 0x0F]);
            data.extend_from_slice(&block_label(i));
        }
        assert_eq!(match_and_reconstruct(&data, 32), data);
        assert_eq!(match_and_reconstruct(&data, 128 * 1024), data);
    }

    #[test]
    fn reconstructs_random() {
        let mut state = 0x1234_5678_9ABC_DEF0u64;
        let mut data = Vec::with_capacity(300 * 1024);
        while data.len() < 300 * 1024 {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            data.extend_from_slice(&state.to_le_bytes());
        }
        assert_eq!(match_and_reconstruct(&data, 64 * 1024), data);
    }

    #[test]
    fn reconstructs_after_compaction() {
        // 3 MiB of data with repeats forces window compaction to run.
        let mut data = Vec::with_capacity(3 << 20);
        for i in 0..(3 << 20) / 64 {
            data.push((i % 251) as u8);
            data.extend_from_slice(&[7u8; 63]);
        }
        assert_eq!(match_and_reconstruct(&data, 128 * 1024), data);
    }

    #[test]
    fn skip_matching_indexes_for_later_blocks() {
        let mut driver = MatchGeneratorDriver::new(16);
        driver.reset(crate::Level::Fastest);
        let pattern = [3u8, 1, 4, 1, 5, 9, 2, 6];
        driver.block_tail()[..pattern.len()].copy_from_slice(&pattern);
        driver.commit_block(pattern.len());
        driver.skip_matching();
        driver.block_tail()[..pattern.len()].copy_from_slice(&pattern);
        driver.commit_block(pattern.len());
        let mut got_triple = false;
        driver.start_matching(|seq| {
            if let Sequence::Triple { offset, .. } = seq {
                // New-offset wire value: actual offset 8 encodes as 8 + 3.
                assert_eq!(offset, pattern.len() + 3);
                got_triple = true;
            }
        });
        assert!(
            got_triple,
            "second block must match the skipped first block"
        );
    }

    #[test]
    fn emits_repcode_sequences() {
        // Structured repetition at a fixed distance: the first repeat is found
        // by the hash probe (offset becomes rep[0]), later repeats must be
        // emitted as repcode 1 (wire offset value 1).
        let pattern: &[u8] = &[
            0xA5, 0x5A, 0xC3, 0x3C, 0x99, 0x66, 0xF0, 0x0D, 0x11, 0x22, 0x33, 0x44, 0x55, 0x66,
            0x77, 0x88, 0x91, 0x82, 0x73, 0x64,
        ];
        let mut data = Vec::new();
        for i in 0..40 {
            data.extend_from_slice(pattern);
            // Constant-length, varying separators keep one pending literal in
            // front of each repeat and hold the period stable.
            data.push(0xF0 ^ i as u8);
        }
        let mut driver = MatchGeneratorDriver::new(128 * 1024);
        driver.reset(crate::Level::Fastest);
        driver.block_tail()[..data.len()].copy_from_slice(&data);
        driver.commit_block(data.len());
        let mut repcodes = 0usize;
        driver.start_matching(|seq| {
            if let Sequence::Triple { offset, .. } = seq {
                if offset <= 3 {
                    repcodes += 1;
                }
            }
        });
        assert!(
            repcodes > 0,
            "repeated structure must produce repcode matches"
        );
        assert_eq!(match_and_reconstruct(&data, 128 * 1024), data);
    }
}
