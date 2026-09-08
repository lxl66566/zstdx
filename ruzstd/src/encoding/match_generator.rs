//! Matching algorithm used to find repeated parts of the original data
//!
//! The Zstd format relies on finding repeated sequences of data and compressing these
//! sequences as instructions for the decoder. A sequence basically tells the decoder
//! "Go back X bytes and copy Y bytes to the end of your decode buffer".
//!
//! This is a port of the official zstd `fast` strategy: one contiguous window buffer,
//! a single-probe u32 hash table with newest-wins overwrite semantics, forward match
//! extension in u64 chunks plus backward extension into pending literals. Positions are
//! tracked as absolute u64 offsets; hash slots carry `(epoch << 48) | position` so a
//! reset just bumps the epoch instead of clearing the table.

use alloc::vec::Vec;

use super::CompressionLevel;
use super::Matcher;
use super::Sequence;
// Shared with the decoder so both sides agree on offset-history semantics.
use crate::decoding::sequence_execution::do_offset_history;

/// Shortest match worth encoding; matches the format's MINMATCH range.
const MIN_MATCH: usize = 4;
/// Bytes fed into the position hash.
const MIN_HASH: usize = 5;
/// Hash table size as a power of two.
const HASH_LOG: u32 = 15;
/// History kept for matching; also the window size declared in the frame header.
const MAX_WINDOW: usize = 0xC0000;

const EMPTY: u64 = 0;

/// Hash the 5 bytes at `idx`. Caller guarantees `idx + 5 <= win.len()` (the
/// scanning and emit loops bound-check once per loop, not per position).
///
/// Five bytes skip the frequent 4-byte boilerplate fragments so probes land
/// on structural repeats instead of recent junk.
#[inline(always)]
fn hash_at(win: &[u8], idx: usize) -> usize {
    // SAFETY: caller guarantees idx + 5 <= win.len(); unaligned reads
    // because positions are byte-granular.
    unsafe {
        let p = win.as_ptr().add(idx);
        let v = (p.cast::<u32>().read_unaligned() as u64) | ((p.add(4).read() as u64) << 32);
        (v.wrapping_mul(0xC2B2_AE3D_27D4_EB4F) as usize >> (64 - HASH_LOG))
            & ((1 << HASH_LOG) - 1)
    }
}

/// Read 4 window bytes at `idx`. Caller guarantees `idx + 4 <= win.len()`.
#[inline(always)]
fn read4(win: &[u8], idx: usize) -> u32 {
    // SAFETY: see contract above; unaligned because byte-granular.
    unsafe { win.as_ptr().add(idx).cast::<u32>().read_unaligned() }
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

/// Store `abs` as the newest position for its hash. Caller guarantees `idx`
/// has at least `MIN_HASH` bytes of window behind it.
#[inline(always)]
fn insert_at(win: &[u8], table: &mut [u64], epoch: u64, idx: usize, abs: u64) {
    let h = hash_at(win, idx);
    table[h] = (epoch << 48) | abs;
}

/// Emit the sequence for a match covering `match_len` bytes at window index
/// `start`, update the repeated-offset history, index the covered range and
/// return the new cursor (which is also the new anchor). Everything hot is
/// passed explicitly so the scan loop keeps its cursors in registers across
/// the call instead of reloading them from the matcher struct.
#[allow(clippy::too_many_arguments)]
fn emit_seq(
    win: &[u8],
    table: &mut [u64],
    epoch: u64,
    anchor: u64,
    win_base: u64,
    insert_max: u64,
    start: usize,
    match_len: usize,
    of_value: u32,
    rep: &mut [u32; 3],
    literals: &mut Vec<u8>,
    sequences: &mut Vec<super::EncodedSequence>,
) -> u64 {
    let anchor_idx = (anchor - win_base) as usize;
    let ll = (start - anchor_idx) as u32;
    literals.extend_from_slice(&win[anchor_idx..start]);
    do_offset_history(of_value, ll, rep);
    sequences.push(super::EncodedSequence {
        ll,
        ml: match_len as u32,
        of: of_value,
    });
    let match_end = start + match_len;
    // Index the covered range: short matches keep every position (they carry
    // most of the alignment coverage on structured data); long matches fall
    // back to a 4-byte grid anchored at the match start plus the final byte,
    // because hashing every byte of long matches was a large share of encoder
    // time. The insert bound is hoisted: a position is insertable while 5
    // window bytes start at it.
    if match_len <= 16 {
        let end = (win_base + match_end as u64).min(insert_max);
        let mut p = win_base + start as u64;
        while p < end {
            insert_at(win, table, epoch, (p - win_base) as usize, p);
            p += 1;
        }
    } else {
        let first = win_base + start as u64;
        let last = (win_base + (match_end - 1) as u64).min(insert_max);
        let mut p = first;
        while p < last {
            insert_at(win, table, epoch, (p - win_base) as usize, p);
            p += 4;
        }
        if last > first {
            insert_at(win, table, epoch, (last - win_base) as usize, last);
        }
    }
    win_base + match_end as u64
}

/// Probe continuations at the second repeated offset immediately after a
/// match (zstd fast's rep_offset2 loop). Alternating-period data chains
/// rep0/rep1 matches back to back with zero literals; emitting with
/// of_value 1 at ll == 0 swaps rep0/rep1, so the loop alternates distances
/// on its own. Returns the cursor after the last chained match.
#[allow(clippy::too_many_arguments)]
fn rep1_chain(
    win: &[u8],
    table: &mut [u64],
    epoch: u64,
    pos: u64,
    block_end: u64,
    win_base: u64,
    insert_max: u64,
    rep: &mut [u32; 3],
    literals: &mut Vec<u8>,
    sequences: &mut Vec<super::EncodedSequence>,
) -> u64 {
    let mut pos = pos;
    while block_end - pos >= MIN_MATCH as u64 {
        let Some(cand_abs) = pos.checked_sub(rep[1] as u64) else {
            break;
        };
        if cand_abs < win_base {
            break;
        }
        let pidx = (pos - win_base) as usize;
        let cand = (cand_abs - win_base) as usize;
        if read4(win, cand) != read4(win, pidx) {
            break;
        }
        let ml = extend_match(win, pidx, cand);
        debug_assert!(ml >= MIN_MATCH);
        pos = emit_seq(
            win, table, epoch, pos, win_base, insert_max, pidx, ml, 1, rep, literals, sequences,
        );
    }
    pos
}

pub struct MatchGeneratorDriver {
    /// Contiguous history: `win[0]` is absolute position `win_base`.
    /// Capacity holds MAX_WINDOW plus one block so a full block always fits
    /// after compaction.
    win: Vec<u8>,
    win_base: u64,
    /// Absolute end of committed data; matching runs up to `block_end`.
    pos: u64,
    block_end: u64,
    /// Start of the literals not yet covered by a sequence.
    anchor: u64,
    /// Absolute start of the last committed block (for `get_last_space`).
    block_start: u64,
    table: Vec<u64>,
    epoch: u64,
    miss_count: usize,
    /// Repeated-offset history, kept in lockstep with the decoder's
    /// `offset_hist` so repcode probes see the same candidates it will.
    rep: [u32; 3],
    space_pool: Vec<Vec<u8>>,
    slice_size: usize,
}

impl MatchGeneratorDriver {
    /// Create a matcher whose blocks hold `slice_size` bytes of input (the
    /// zstd block maximum is 128 KiB).
    pub fn new(slice_size: usize) -> Self {
        // Two windows of capacity: compacting down to MAX_WINDOW then only
        // after another MAX_WINDOW of input means the copy_within runs at
        // ~1x data volume instead of once per block.
        Self {
            win: Vec::with_capacity(2 * MAX_WINDOW + slice_size),
            win_base: 0,
            pos: 0,
            block_end: 0,
            anchor: 0,
            block_start: 0,
            table: alloc::vec![EMPTY; 1usize << HASH_LOG],
            // Epoch 0 is the never-valid state of a zeroed table.
            epoch: 1,
            miss_count: 0,
            rep: [1, 4, 8],
            space_pool: Vec::new(),
            slice_size,
        }
    }

    #[inline(always)]
    fn idx_of(&self, abs: u64) -> usize {
        (abs - self.win_base) as usize
    }
}

impl Matcher for MatchGeneratorDriver {
    fn reset(&mut self, _level: CompressionLevel) {
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
            self.epoch = 1;
        }
        self.miss_count = 0;
        // Matches the decoder's per-frame offset_hist reset.
        self.rep = [1, 4, 8];
    }

    fn window_size(&self) -> u64 {
        MAX_WINDOW as u64
    }

    fn repcode_snapshot(&self) -> [u32; 3] {
        self.rep
    }

    fn restore_repcode(&mut self, rep: [u32; 3]) {
        self.rep = rep;
    }

    fn get_next_space(&mut self) -> Vec<u8> {
        let size = self.slice_size;
        match self.space_pool.pop() {
            Some(mut v) => {
                // Pooled buffers come from `vec![0; size]` and were only
                // truncated since, so all bytes stay initialized; the caller
                // overwrites what it reads and truncates to that length.
                debug_assert!(v.capacity() >= size);
                // SAFETY: see invariant above.
                unsafe { v.set_len(size) };
                v
            }
            None => alloc::vec![0; size],
        }
    }

    fn get_last_space(&mut self) -> &[u8] {
        &self.win[self.idx_of(self.block_start)..]
    }

    fn commit_space(&mut self, mut space: Vec<u8>) {
        if self.win.len() + space.len() > self.win.capacity() {
            let keep = self.win.len().min(MAX_WINDOW);
            let drop = self.win.len() - keep;
            self.win.copy_within(drop.., 0);
            self.win.truncate(keep);
            self.win_base += drop as u64;
        }
        self.win.extend_from_slice(&space);
        // Matching restarts at the head of the freshly committed block; the
        // window behind it is only a match source.
        self.pos = self.win_base + (self.win.len() - space.len()) as u64;
        self.block_start = self.pos;
        self.block_end = self.win_base + self.win.len() as u64;
        self.anchor = self.pos;
        space.clear();
        self.space_pool.push(space);
    }

    fn start_matching(&mut self, mut handle_sequence: impl for<'a> FnMut(Sequence<'a>)) {
        let mut literals = Vec::new();
        let mut sequences = Vec::new();
        self.start_matching_into(&mut literals, &mut sequences);
        // Rebuild the interleaved callback order from the collected
        // buffers: each sequence's ll literals came right before it.
        let mut offset = 0usize;
        for seq in sequences {
            let lits = &literals[offset..offset + seq.ll as usize];
            offset += seq.ll as usize;
            handle_sequence(Sequence::Triple {
                literals: lits,
                offset: seq.of as usize,
                match_len: seq.ml as usize,
            });
        }
        handle_sequence(Sequence::Literals {
            literals: &literals[offset..],
        });
    }

    fn start_matching_into(
        &mut self,
        literals: &mut Vec<u8>,
        sequences: &mut Vec<super::EncodedSequence>,
    ) {
        // Hot state lives in locals for the whole loop: the emit helpers
        // used to take `&mut self`, which forced a reload of every cursor
        // from memory after each match.
        let win = &self.win[..];
        let table = &mut self.table[..];
        let epoch = self.epoch;
        let win_base = self.win_base;
        let block_end = self.block_end;
        // saturating: tiny first blocks never reach an emit, so the bound is
        // never consulted when win.len() < MIN_HASH.
        let insert_max = win_base + win.len().saturating_sub(MIN_HASH) as u64;
        let max_window = MAX_WINDOW as u64;
        let mut pos = self.pos;
        let mut anchor = self.anchor;
        let mut miss_count = self.miss_count;
        let mut rep = self.rep;

        while pos < block_end {
            if block_end - pos < MIN_HASH as u64 {
                break;
            }
            let idx = (pos - win_base) as usize;
            let cur = read4(win, idx);

            // Repcode probe (mirrors zstd's fast strategy: rep[0] only; the
            // weaker rep1/rep2 candidates steal positions from longer hash
            // matches). A repcode match needs at least one pending literal so
            // of_value 1 stays encodable: with literals pending the probe
            // runs at the current position, otherwise one byte ahead so that
            // byte becomes the literal. The backward extension guard below
            // never consumes that literal either way.
            let probe = if pos == anchor { pos + 1 } else { pos };
            if let Some(cand_abs) = probe.checked_sub(rep[0] as u64) {
                if cand_abs >= win_base {
                    let mut cand = (cand_abs - win_base) as usize;
                    let pidx = (probe - win_base) as usize;
                    let pcur = read4(win, pidx);
                    if read4(win, cand) == pcur {
                        let mut ml = extend_match(win, pidx, cand);
                        if ml >= MIN_MATCH {
                            let anchor_idx = (anchor - win_base) as usize;
                            let mut start = pidx;
                            // Extend backwards into the pending literals; the
                            // offset (pidx - cand) stays constant.
                            while start > anchor_idx + 1
                                && cand > 0
                                && win[cand - 1] == win[start - 1]
                            {
                                cand -= 1;
                                start -= 1;
                                ml += 1;
                            }
                            anchor = emit_seq(
                                win, table, epoch, anchor, win_base, insert_max, start, ml, 1,
                                &mut rep, literals, sequences,
                            );
                            pos = rep1_chain(
                                win, table, epoch, anchor, block_end, win_base, insert_max,
                                &mut rep, literals, sequences,
                            );
                            // The chain's matches advance the cursor too.
                            anchor = pos;
                            miss_count = 0;
                            continue;
                        }
                    }
                }
            }

            let h = hash_at(win, idx);
            let prev = table[h];
            // Index only every other position in the miss path: with a small
            // table each slot is heavily contended, and halving the insert
            // rate doubles how long a far repeat stays discoverable.
            if idx & 1 == 0 {
                table[h] = (epoch << 48) | pos;
            }

            let mut matched = false;
            if prev >> 48 == epoch {
                let cand_abs = prev & ((1u64 << 48) - 1);
                if cand_abs >= win_base && pos - cand_abs <= max_window {
                    let mut cand = (cand_abs - win_base) as usize;
                    if read4(win, cand) == cur {
                        let mut ml = extend_match(win, idx, cand);
                        // A hash match already spans 5 bytes; below 6 the
                        // sequence overhead roughly equals the literals it
                        // covers, and rejecting it lets the scan try the next
                        // position where a longer match may start.
                        if ml >= 6 {
                            let anchor_idx = (anchor - win_base) as usize;
                            let mut start = idx;
                            // Extend backwards into the pending literals; the
                            // offset (idx - cand) stays constant.
                            while start > anchor_idx
                                && cand > 0
                                && win[cand - 1] == win[start - 1]
                            {
                                cand -= 1;
                                start -= 1;
                                ml += 1;
                            }
                            let of_value = (start - cand + 3) as u32;
                            anchor = emit_seq(
                                win, table, epoch, anchor, win_base, insert_max, start, ml,
                                of_value, &mut rep, literals, sequences,
                            );
                            pos = rep1_chain(
                                win, table, epoch, anchor, block_end, win_base, insert_max,
                                &mut rep, literals, sequences,
                            );
                            // The chain's matches advance the cursor too.
                            anchor = pos;
                            miss_count = 0;
                            matched = true;
                        }
                    }
                }
            }
            if !matched {
                miss_count += 1;
                // Grow the probe step on long literal runs so incompressible
                // data does not pay a full hash per byte.
                pos += 1 + (miss_count >> 2).min(255) as u64;
            }
        }
        if anchor < block_end {
            let tail = (anchor - win_base) as usize..(block_end - win_base) as usize;
            literals.extend_from_slice(&win[tail]);
        }
        self.pos = block_end;
        self.anchor = block_end;
        self.miss_count = miss_count;
        self.rep = rep;
    }

    fn skip_matching(&mut self) {
        // Only called for RLE blocks so far: every 5-byte window in a
        // uniform run hashes to the same slot, so indexing each byte just
        // rewrites one table entry. The first position covers that slot;
        // future probes into the run resolve through it or the repcode
        // chain.
        let idx = (self.block_start - self.win_base) as usize;
        if idx + MIN_HASH <= self.win.len() {
            insert_at(&self.win, &mut self.table, self.epoch, idx, self.block_start);
        }
        self.pos = self.block_end;
        self.anchor = self.block_end;
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
        driver.reset(crate::encoding::CompressionLevel::Fastest);
        // Offset history mirrors the decoder's per-frame scratch.
        let mut rep = [1u32, 4, 8];
        let mut reconstructed = Vec::new();
        for block in data.chunks(block_size) {
            let mut space = driver.get_next_space();
            space.clear();
            space.extend_from_slice(block);
            driver.commit_space(space);
            driver.start_matching(|seq| match seq {
                Sequence::Literals { literals } => reconstructed.extend_from_slice(literals),
                Sequence::Triple {
                    literals,
                    offset,
                    match_len,
                } => {
                    reconstructed.extend_from_slice(literals);
                    let actual =
                        crate::decoding::sequence_execution::do_offset_history(
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
        driver.reset(crate::encoding::CompressionLevel::Fastest);
        let pattern = [3u8, 1, 4, 1, 5, 9, 2, 6];
        let mut space = driver.get_next_space();
        space.clear();
        space.extend_from_slice(&pattern);
        driver.commit_space(space);
        driver.skip_matching();
        let mut space = driver.get_next_space();
        space.clear();
        space.extend_from_slice(&pattern);
        driver.commit_space(space);
        let mut got_triple = false;
        driver.start_matching(|seq| {
            if let Sequence::Triple { offset, .. } = seq {
                // New-offset wire value: actual offset 8 encodes as 8 + 3.
                assert_eq!(offset, pattern.len() + 3);
                got_triple = true;
            }
        });
        assert!(got_triple, "second block must match the skipped first block");
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
        driver.reset(crate::encoding::CompressionLevel::Fastest);
        let mut space = driver.get_next_space();
        space.clear();
        space.extend_from_slice(&data);
        driver.commit_space(space);
        let mut repcodes = 0usize;
        driver.start_matching(|seq| {
            if let Sequence::Triple { offset, .. } = seq {
                if offset <= 3 {
                    repcodes += 1;
                }
            }
        });
        assert!(repcodes > 0, "repeated structure must produce repcode matches");
        assert_eq!(match_and_reconstruct(&data, 128 * 1024), data);
    }
}
