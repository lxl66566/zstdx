use alloc::vec::Vec;
use core::convert::TryInto;

use super::{
    super::blocks::sequence_section::{ModeType, Sequence, SequencesHeader},
    scratch::FSEScratch,
};
use crate::{
    blocks::sequence_section::{
        LL_DEFAULT_ACC_LOG, LL_DEFAULT_DISTRIBUTION, MAX_LITERAL_LENGTH_CODE,
        MAX_MATCH_LENGTH_CODE, MAX_OFFSET_CODE, ML_DEFAULT_ACC_LOG, ML_DEFAULT_DISTRIBUTION,
        OF_DEFAULT_ACC_LOG, OF_DEFAULT_DISTRIBUTION,
    },
    decoding::errors::DecodeSequenceError,
    fse::{FSETable, next_position, occurrence_from_state},
};

/// Decode the provided source as a series of sequences into the supplied `target`.
pub fn decode_sequences(
    section: &SequencesHeader,
    source: &[u8],
    scratch: &mut FSEScratch,
    target: &mut Vec<Sequence>,
) -> Result<(), DecodeSequenceError> {
    // BMI2 compiles the loop's variable shifts to single-uop shlx/shrx; the
    // detection cache makes this dispatch cheap relative to a whole block.
    #[cfg(all(target_arch = "x86_64", feature = "std"))]
    {
        if super::simd_tier::bmi2() {
            // SAFETY: simd_tier::bmi2() ran the runtime detection (or a forced
            // tier asserted it), so the BMI2 instantiation is executable
            return unsafe { decode_sequences_impl_bmi2(section, source, scratch, target) };
        }
    }
    decode_sequences_impl(section, source, scratch, target)
}

#[cfg(all(target_arch = "x86_64", feature = "std"))]
#[target_feature(enable = "bmi2")]
unsafe fn decode_sequences_impl_bmi2(
    section: &SequencesHeader,
    source: &[u8],
    scratch: &mut FSEScratch,
    target: &mut Vec<Sequence>,
) -> Result<(), DecodeSequenceError> {
    decode_sequences_impl(section, source, scratch, target)
}

/// Appending variant used by the segment decoder: the block's sequences land
/// behind whatever the caller already collected for earlier blocks of the
/// segment (see decoding::mt).
#[cfg(feature = "std")]
pub(crate) fn decode_sequences_into(
    section: &SequencesHeader,
    source: &[u8],
    scratch: &mut FSEScratch,
    target: &mut Vec<Sequence>,
) -> Result<(), DecodeSequenceError> {
    // BMI2: as above.
    #[cfg(all(target_arch = "x86_64", feature = "std"))]
    {
        if super::simd_tier::bmi2() {
            // SAFETY: simd_tier::bmi2() ran the runtime detection (or a forced
            // tier asserted it), so the BMI2 instantiation is executable
            return unsafe { decode_sequences_into_impl_bmi2(section, source, scratch, target) };
        }
    }
    decode_sequences_into_impl(section, source, scratch, target)
}

#[cfg(all(target_arch = "x86_64", feature = "std"))]
#[target_feature(enable = "bmi2")]
unsafe fn decode_sequences_into_impl_bmi2(
    section: &SequencesHeader,
    source: &[u8],
    scratch: &mut FSEScratch,
    target: &mut Vec<Sequence>,
) -> Result<(), DecodeSequenceError> {
    decode_sequences_into_impl(section, source, scratch, target)
}

/// Two-pass decode into the caller's vector (the ring-buffer execution path).
/// The flat output path instead fuses decoding with execution sequence by
/// sequence (see `sequence_execution::execute_decoded_flat`).
#[inline(always)]
fn decode_sequences_impl(
    section: &SequencesHeader,
    source: &[u8],
    scratch: &mut FSEScratch,
    target: &mut Vec<Sequence>,
) -> Result<(), DecodeSequenceError> {
    target.clear();
    decode_sequences_into_impl(section, source, scratch, target)
}

/// The appending core shared by both entry points: appends exactly
/// `num_sequences` sequences behind the target's current length.
#[inline(always)]
fn decode_sequences_into_impl(
    section: &SequencesHeader,
    source: &[u8],
    scratch: &mut FSEScratch,
    target: &mut Vec<Sequence>,
) -> Result<(), DecodeSequenceError> {
    let mut dec = SeqDecoder::new(section, source, scratch)?;
    let nseq = section.num_sequences as usize;
    let start = target.len();
    target.reserve(nseq);
    let out = target.as_mut_ptr();
    let mut idx = 0;
    while dec.rem > 0 {
        let seq = dec.next()?;
        // SAFETY: start + nseq slots were just reserved; `rem > 0` keeps
        // `idx` below it and each iteration writes exactly one Sequence.
        unsafe { *out.add(start + idx) = seq };
        idx += 1;
    }
    // SAFETY: idx sequences were written (idx == nseq; next() returns None
    // only at the end). Length is exposed before finish() so an error there
    // sees the same target state the historical loop left behind.
    unsafe { target.set_len(start + idx) };
    dec.finish()
}

/// Fixed slots inside `FSEScratch::seq_packed` (sizes 1 << LL_MAX_LOG,
/// 1 << ML_MAX_LOG, 1 << OF_MAX_LOG) so the decode loop addresses all three
/// packed tables through one base pointer with constant offsets.
pub(crate) const LL_SLOT: usize = 0;
pub(crate) const ML_SLOT: usize = 1 << LL_MAX_LOG;
pub(crate) const OF_SLOT: usize = ML_SLOT + (1 << ML_MAX_LOG);
pub(crate) const SEQ_TABLE_SLOTS: usize = OF_SLOT + (1 << OF_MAX_LOG);

/// Pack one sequence-table entry: everything the decode loops need for a
/// symbol in one u64, in libzstd's `ZSTD_seqSymbol` field order
/// `[base: u32][nb_bits: u8][add_bits: u8][next_state: u16]` (high to low).
/// The staging decoder extracts fields with shifts; the flat executor loads
/// them individually through [`EntryRef`].
const fn pack_entry(base: u32, add_bits: u8, next_state: u16, nb_bits: u8) -> u64 {
    ((base as u64) << 32) | ((nb_bits as u64) << 24) | ((add_bits as u64) << 16) | next_state as u64
}

const fn entry_base(e: u64) -> u32 {
    (e >> 32) as u32
}

const fn entry_add_bits(e: u64) -> u32 {
    ((e >> 16) & 0xff) as u32
}

const fn entry_nb_bits(e: u64) -> u32 {
    ((e >> 24) & 0xff) as u32
}

const fn entry_next_state(e: u64) -> u32 {
    (e & 0xffff) as u32
}

/// One packed entry viewed as separate fields: the flat executor's
/// libzstd-shaped decode loads each field on its own instead of unpacking
/// the u64 with shifts. Only [`EntryRef::at`] mints one, under the
/// reachable-state contract, so the field reads stay in bounds.
#[derive(Clone, Copy)]
pub(crate) struct EntryRef(*const u64);

// Field positions inside the native u64 of `pack_entry`, each in units of
// its own width.
#[cfg(target_endian = "little")]
const NEXT_U16: usize = 0;
#[cfg(target_endian = "little")]
const ADD_U8: usize = 2;
#[cfg(target_endian = "little")]
const NB_U8: usize = 3;
#[cfg(target_endian = "little")]
const BASE_U32: usize = 1;
#[cfg(target_endian = "big")]
const NEXT_U16: usize = 3;
#[cfg(target_endian = "big")]
const ADD_U8: usize = 5;
#[cfg(target_endian = "big")]
const NB_U8: usize = 4;
#[cfg(target_endian = "big")]
const BASE_U32: usize = 0;

impl EntryRef {
    /// # Safety
    ///
    /// `tbl` is a [`SeqDecoder::tbl`] base, `slot` one of the stream slots
    /// and `state` a state the slot's table can reach (every FSE transition
    /// stays below the table size, see `decode_step`).
    #[inline(always)]
    pub(crate) unsafe fn at(tbl: *const u64, slot: usize, state: u32) -> Self {
        // SAFETY: the caller's contract keeps the entry inside the table
        Self(unsafe { tbl.add(slot + state as usize) })
    }

    #[inline(always)]
    pub(crate) fn base(self) -> u32 {
        // SAFETY: `at` minted an in-bounds entry, and every field sits
        // inside its 8 bytes at its own alignment
        unsafe { self.0.cast::<u32>().add(BASE_U32).read() }
    }

    #[inline(always)]
    pub(crate) fn add_bits(self) -> u32 {
        // SAFETY: as in `base`
        u32::from(unsafe { self.0.cast::<u8>().add(ADD_U8).read() })
    }

    #[inline(always)]
    pub(crate) fn nb_bits(self) -> u32 {
        // SAFETY: as in `base`
        u32::from(unsafe { self.0.cast::<u8>().add(NB_U8).read() })
    }

    #[inline(always)]
    pub(crate) fn next_state(self) -> u32 {
        // SAFETY: as in `base`
        u32::from(unsafe { self.0.cast::<u16>().add(NEXT_U16).read() })
    }
}

/// Stage one FSE state's symbol for the build's second pass: the symbol sits
/// in the entry's `base` field, which pass two overwrites. libzstd's
/// `ZSTD_buildFSETable` stages it in `baseValue` the same way.
const fn staged_symbol(symbol: u8) -> u64 {
    (symbol as u64) << 32
}

/// Build one packed sequence decoding table straight into its slot in the
/// reference's two passes (`ZSTD_buildFSETable`): the first spreads each
/// state's symbol into the slot's staging field along the table's own position
/// walk, the second walks the slots, reads the symbol back out of the field it
/// just wrote, and completes the entry from the distribution itself plus the
/// symbol's `(base, add-bits)` pair. Folding the two passes costs the fill
/// pass the code lookup, the spread multiply-adds and a 64-bit store per entry,
/// 29 percent more Ir per entry than the spread alone, and the deleted pack pass
/// was exactly what that work belongs to (docs/src/dev/negative/decoding.md).
///
/// Slot entries beyond the table's real size stay stale — states provably never
/// reach them (every transition stays below 1 << accuracy_log).
fn build_packed_table(table: &FSETable, codes: &[(u32, u8)], out: &mut [u64]) {
    let table_size = 1usize << table.accuracy_log;
    let slots = &mut out[..table_size];

    // Pass one: lay down each state's symbol. Low-probability (`-1`) symbols
    // own one state each and take the top of the table in descending symbol
    // order; the rest spread from the bottom, skipping that tail. `state`
    // starts each live symbol's running FSE state at its probability (one
    // for `-1`), the value pass two increments per occurrence.
    let mut state = [0u16; 256];
    let mut negative_idx = table_size;
    for (symbol, &prob) in table.symbol_probabilities.iter().enumerate() {
        if prob == -1 {
            negative_idx -= 1;
            slots[negative_idx] = staged_symbol(symbol as u8);
            state[symbol] = 1;
        }
    }
    let mut position = 0;
    for (symbol, &prob) in table.symbol_probabilities.iter().enumerate() {
        if prob <= 0 {
            continue;
        }
        state[symbol] = prob as u16;
        // for each probability point the symbol gets on slot
        for _ in 0..prob {
            slots[position] = staged_symbol(symbol as u8);

            position = next_position(position, table_size);
            while position >= negative_idx {
                position = next_position(position, table_size);
                // everything above negative_idx is already taken
            }
        }
    }

    // Pass two: the symbol comes back out of its staging field, and the code
    // pair and the baseline complete the entry with a single store. The
    // running state the counter carries fixes both baseline fields through
    // `occurrence_from_state` — the reference's per-slot form — so no hoisted
    // per-symbol constants sit between the two passes. The counter is 256
    // wide so the byte-wide symbol indexes it unchecked; only live symbols
    // are read, which pass one initialized.
    for slot in slots.iter_mut() {
        let symbol = (entry_base(*slot) & 0xff) as usize;
        let running = state[symbol];
        state[symbol] = running + 1;
        let (base, add) = codes[symbol];
        let (base_line, nb_bits) = occurrence_from_state(running as u32, table.accuracy_log);
        *slot = pack_entry(base, add, base_line as u16, nb_bits);
    }

    // Factory proof over the packed entries: the decode loop computes its
    // next state as `next_state_base + add_bits` and indexes the slot with
    // it, so the raw table's transition theorem must survive the bit
    // packing (the u16 field keeps the base only while the theorem holds,
    // since it bounds the base below 1 << accuracy_log). The stale slot
    // tail beyond the real table length is unreachable exactly while every
    // packed entry keeps its transition below that length.
    #[cfg(debug_assertions)]
    for (i, &p) in slots.iter().enumerate() {
        let (base_t, nb_t) = (entry_next_state(p) as usize, entry_nb_bits(p));
        debug_assert!(
            base_t + (1usize << nb_t) <= table_size,
            "packed seq entry {i} transitions past the table: {base_t} + 2^{nb_t} > {table_size}",
        );
    }
}

/// (base value, additional bits) per literal length code.
const LL_CODES: [(u32, u8); 36] = [
    (0, 0),
    (1, 0),
    (2, 0),
    (3, 0),
    (4, 0),
    (5, 0),
    (6, 0),
    (7, 0),
    (8, 0),
    (9, 0),
    (10, 0),
    (11, 0),
    (12, 0),
    (13, 0),
    (14, 0),
    (15, 0),
    (16, 1),
    (18, 1),
    (20, 1),
    (22, 1),
    (24, 2),
    (28, 2),
    (32, 3),
    (40, 3),
    (48, 4),
    (64, 6),
    (128, 7),
    (256, 8),
    (512, 9),
    (1024, 10),
    (2048, 11),
    (4096, 12),
    (8192, 13),
    (16384, 14),
    (32768, 15),
    (65536, 16),
];

/// (base value, additional bits) per match length code.
const ML_CODES: [(u32, u8); 53] = [
    (3, 0),
    (4, 0),
    (5, 0),
    (6, 0),
    (7, 0),
    (8, 0),
    (9, 0),
    (10, 0),
    (11, 0),
    (12, 0),
    (13, 0),
    (14, 0),
    (15, 0),
    (16, 0),
    (17, 0),
    (18, 0),
    (19, 0),
    (20, 0),
    (21, 0),
    (22, 0),
    (23, 0),
    (24, 0),
    (25, 0),
    (26, 0),
    (27, 0),
    (28, 0),
    (29, 0),
    (30, 0),
    (31, 0),
    (32, 0),
    (33, 0),
    (34, 0),
    (35, 1),
    (37, 1),
    (39, 1),
    (41, 1),
    (43, 2),
    (47, 2),
    (51, 3),
    (59, 3),
    (67, 4),
    (83, 4),
    (99, 5),
    (131, 7),
    (259, 8),
    (515, 9),
    (1027, 10),
    (2051, 11),
    (4099, 12),
    (8195, 13),
    (16387, 14),
    (32771, 15),
    (65539, 16),
];

/// (base value, additional bits) per offset code; the base is 1 << code.
const fn of_codes() -> [(u32, u8); 32] {
    let mut t = [(0u32, 0u8); 32];
    let mut i = 0;
    while i < 32 {
        t[i] = (1u32 << i, i as u8);
        i += 1;
    }
    t
}
const OF_CODES: [(u32, u8); 32] = of_codes();

/// Whether a stream's table was ever established: an RLE code, or a read
/// distribution (every read path sets accuracy log >= 5; a virgin table has
/// accuracy log 0 with empty probabilities).
fn table_established(table: &FSETable, rle: Option<u8>) -> bool {
    rle.is_some() || table.accuracy_log != 0 || !table.symbol_probabilities.is_empty()
}

/// Corruption-class rejection for a stream whose table was never
/// established (Repeat mode with nothing to repeat).
fn unestablished_table() -> DecodeSequenceError {
    DecodeSequenceError::FSEDecoderError(
        crate::decoding::errors::FSEDecoderError::TableIsUninitialized,
    )
}

/// Rebuild the packed table slots whose source tables changed since the last
/// block, straight from each table's distribution and spread constants. RLE
/// streams get a one-state fake table in slot 0: the entry carries the code's
/// (base, add-bits) with a zero transition, so the decode loop stays in state 0
/// forever and needs no RLE special cases at all (zero accuracy-log bits at the
/// initial state read).
fn pack_tables(scratch: &mut FSEScratch) -> Result<(), DecodeSequenceError> {
    if !scratch.ll_seq_valid {
        if !table_established(&scratch.literal_lengths, scratch.ll_rle) {
            // A 1-slot pseudo table from an empty distribution would decode
            // pooled residue as table content; reject instead.
            return Err(unestablished_table());
        }
        match scratch.ll_rle {
            Some(c) => pack_rle_entry(LL_CODES[c as usize], &mut scratch.seq_packed[LL_SLOT]),
            None => build_packed_table(
                &scratch.literal_lengths,
                &LL_CODES,
                &mut scratch.seq_packed[LL_SLOT..ML_SLOT],
            ),
        }
        scratch.ll_seq_valid = true;
    }
    if !scratch.ml_seq_valid {
        if !table_established(&scratch.match_lengths, scratch.ml_rle) {
            return Err(unestablished_table());
        }
        match scratch.ml_rle {
            Some(c) => pack_rle_entry(ML_CODES[c as usize], &mut scratch.seq_packed[ML_SLOT]),
            None => build_packed_table(
                &scratch.match_lengths,
                &ML_CODES,
                &mut scratch.seq_packed[ML_SLOT..OF_SLOT],
            ),
        }
        scratch.ml_seq_valid = true;
    }
    if !scratch.of_seq_valid {
        if !table_established(&scratch.offsets, scratch.of_rle) {
            return Err(unestablished_table());
        }
        match scratch.of_rle {
            Some(c) => pack_rle_entry(OF_CODES[c as usize], &mut scratch.seq_packed[OF_SLOT]),
            None => build_packed_table(
                &scratch.offsets,
                &OF_CODES,
                &mut scratch.seq_packed[OF_SLOT..],
            ),
        }
        scratch.of_seq_valid = true;
    }
    Ok(())
}

/// Write the self-transitioning entry an RLE stream decodes through.
fn pack_rle_entry((base, add): (u32, u8), slot: &mut u64) {
    *slot = pack_entry(base, add, 0, 0);
}

/// One-sequence-at-a-time FSE sequence decoder. `new` consumes the table
/// descriptions and reads the initial stream states; `next` decodes one
/// sequence and advances the three FSE streams (RLE streams stay at their
/// fixed values); `finish` validates the stream padding. Keeping the decode
/// state in this struct lets callers execute each sequence the moment it is
/// decoded (the flat output path) or collect them into a vector (the
/// ring-buffer path) from the same code.
///
/// The carried stream state is the three FSE states plus the shifted bit
/// window — deliberately NOT the packed table entries or the raw `bits`
/// container: the entries reload from the tables each step (same three
/// loads, issued where the transitions produce them), and `bits` is never
/// read across a sequence boundary (reload always rebuilds the window from
/// memory; see `reload`). Both removals exist to keep the fused decode and
/// execution loop register-resident: carrying entries alongside the
/// executor's cursor set spilled every sequence.
pub(crate) struct SeqDecoder {
    /// Base of the packed tables in their fixed slots (see `pack_tables`).
    pub(crate) tbl: *const u64,
    pub(crate) src_ptr: *const u8,
    pub(crate) ip: usize,
    /// Bit window, pre-shifted by the bits consumed so far (`win` always
    /// equals `bits_at_last_reload << consumed`, see `read`).
    pub(crate) win: u64,
    pub(crate) consumed: u32,
    pub(crate) ll_state: u32,
    pub(crate) ml_state: u32,
    pub(crate) of_state: u32,
    /// Sequences left to decode; `next` refuses when it reaches zero.
    pub(crate) rem: usize,
}

impl SeqDecoder {
    pub(crate) fn new(
        section: &SequencesHeader,
        source: &[u8],
        scratch: &mut FSEScratch,
    ) -> Result<Self, DecodeSequenceError> {
        let bytes_read = maybe_update_fse_tables(section, source, scratch)?;

        vprintln!("Updating tables used {} bytes", bytes_read);

        let bit_stream = &source[bytes_read..];
        let br = SeqBitReader::new(bit_stream)?;

        if !scratch.ll_ready || !scratch.of_ready || !scratch.ml_ready {
            // Repeat mode with no table ever established (RLE streams count:
            // their one-state fake table lives in the packed slot). Checked
            // before packing so the mode-level cause wins over the build's
            // own guard below.
            return Err(unestablished_table());
        }
        pack_tables(scratch)?;

        // SAFETY: seq_packed is not reallocated between here and the last
        // next() call (the tables only change through maybe_update_fse_tables)
        let tbl = scratch.seq_packed.as_ptr();
        let src_ptr = br.source.as_ptr();
        let mut ip = br.ip;
        let mut consumed = br.consumed;
        // wrapping: degenerate short streams clamp with consumed == 64 (error path)
        let mut win = br.bits.wrapping_shl(consumed);

        // Initial states are read in the order ll, of, ml (RLE streams have
        // accuracy_log 0, so their read yields a masked zero state)
        let ll_state = read_state(
            &mut win,
            &mut consumed,
            scratch.literal_lengths.accuracy_log as u32,
        );
        let of_state = read_state(&mut win, &mut consumed, scratch.offsets.accuracy_log as u32);
        let ml_state = read_state(
            &mut win,
            &mut consumed,
            scratch.match_lengths.accuracy_log as u32,
        );
        // Realign the window before the first sequence; the loop only reloads
        // at sequence ends (the state reads above already consumed up to 26
        // bits)
        let _ = reload(src_ptr, &mut ip, &mut win, &mut consumed);

        Ok(SeqDecoder {
            tbl,
            src_ptr,
            ip,
            win,
            consumed,
            ll_state,
            ml_state,
            of_state,
            rem: section.num_sequences as usize,
        })
    }

    /// Decode the next sequence and advance the streams for the one after
    /// it; the caller checks `rem` first (it counts down to zero).
    #[inline(always)]
    pub(crate) fn next(&mut self) -> Result<Sequence, DecodeSequenceError> {
        let SeqDecoder {
            tbl,
            src_ptr,
            ip,
            win,
            consumed,
            ll_state,
            ml_state,
            of_state,
            rem,
        } = self;
        decode_step(
            *tbl, *src_ptr, ip, win, consumed, ll_state, ml_state, of_state, rem,
        )
    }

    /// Final padding check: exactly as many bits consumed as the stream had.
    pub(crate) fn finish(&self) -> Result<(), DecodeSequenceError> {
        let rem = self.ip as isize * 8 + 64 - self.consumed as isize;
        match rem.cmp(&0) {
            core::cmp::Ordering::Greater => Err(DecodeSequenceError::ExtraBits {
                bits_remaining: rem,
            }),
            core::cmp::Ordering::Less => Err(DecodeSequenceError::NotEnoughBytesForNumSequences),
            core::cmp::Ordering::Equal => Ok(()),
        }
    }
}

/// Decode one sequence and advance the streams for the one after it (the
/// body of `next`). The caller guarantees `*rem > 0`; `rem` counts down and
/// the last sequence skips the state transitions and the reload (they would
/// read past the stream's end). State passes as scalars so the caller's SROA
/// sees plain local variables.
///
/// The fields come out of batched window reads: the three add-bit widths
/// are known before reading, so one read serves all three fields, and one
/// more serves the three transitions. The offset stays a wire value, because
/// the staging callers resolve repcodes later (MT stage A cannot know the
/// history at a segment start).
#[allow(clippy::too_many_arguments)]
#[inline(always)]
pub(crate) fn decode_step(
    tbl: *const u64,
    src_ptr: *const u8,
    ip: &mut usize,
    win: &mut u64,
    consumed: &mut u32,
    ll_state: &mut u32,
    ml_state: &mut u32,
    of_state: &mut u32,
    rem: &mut usize,
) -> Result<Sequence, DecodeSequenceError> {
    // SAFETY: FSE table construction keeps every reachable state below the
    // table size (RLE streams sit at state 0, their own fake slot),
    // independent of the bitstream content. The debug factory-proof scans
    // in `build_decoding_table` and `pack_seq_table` pin this theorem per
    // built table, so a builder regression panics here in debug builds
    // instead of turning this load into an overread.
    let ll_entry = unsafe { *tbl.add(LL_SLOT + *ll_state as usize) };
    // SAFETY: same invariant as the ll stream
    let ml_entry = unsafe { *tbl.add(ML_SLOT + *ml_state as usize) };
    // SAFETY: same invariant as the ll stream
    let of_entry = unsafe { *tbl.add(OF_SLOT + *of_state as usize) };
    let (ll_base, ll_nb) = (entry_base(ll_entry), entry_add_bits(ll_entry));
    let (ml_base, ml_nb) = (entry_base(ml_entry), entry_add_bits(ml_entry));
    let (of_base, of_nb) = (entry_base(of_entry), entry_add_bits(of_entry));

    // The fields (of, then ml, then ll in read order, from the top) come
    // out of a single serial window read split in parallel off the
    // extracted value — one dependent chain instead of three. sum <= 31
    // keeps the extraction shift in range, and 7 + sum + 26 transition bits
    // <= 64 means the post-reload window always covers it: the mid-sequence
    // reload guard (libzstd's totalBits guard) only applies to the
    // unbatched path. All fields are masked, so a zero-width total
    // (rep0-heavy sequences) needs no branch: every mask is zero exactly
    // then.
    let sum = ll_nb + ml_nb + of_nb;
    let (obits, ml_add, ll_add) = if sum <= 31 {
        let v = read(win, consumed, sum);
        (
            (v >> (ml_nb + ll_nb)) & ((1u64 << of_nb) - 1),
            (v >> ll_nb) & ((1u64 << ml_nb) - 1),
            v & ((1u64 << ll_nb) - 1),
        )
    } else {
        // Cold: fields too wide to batch. of+ml+ll add bits sum above
        // the 57 bits guaranteed after a reload; reload mid-sequence
        // then
        let obits = read(win, consumed, of_nb) & ((1u64 << of_nb) - 1);
        let ml_add = read(win, consumed, ml_nb) & ((1u64 << ml_nb) - 1);
        if matches!(reload(src_ptr, ip, win, consumed), Reload::Overflow) {
            return Err(DecodeSequenceError::NotEnoughBytesForNumSequences);
        }
        let ll_add = read(win, consumed, ll_nb) & ((1u64 << ll_nb) - 1);
        (obits, ml_add, ll_add)
    };
    let seq = Sequence {
        ll: ll_base + ll_add as u32,
        ml: ml_base + ml_add as u32,
        of: of_base + obits as u32,
    };

    *rem -= 1;
    if *rem > 0 {
        let ll_nb_t = entry_nb_bits(ll_entry);
        let ml_nb_t = entry_nb_bits(ml_entry);
        let of_nb_t = entry_nb_bits(of_entry);
        let ll_base_t = entry_next_state(ll_entry);
        let ml_base_t = entry_next_state(ml_entry);
        let of_base_t = entry_next_state(of_entry);
        // Same batching for the three state transitions (ll, then ml,
        // then of): at most 9+9+8 = 26 bits, always extractable in one
        // read, with every field masked so a zero-width total needs no
        // branch
        let sum_t = ll_nb_t + ml_nb_t + of_nb_t;
        let v = read(win, consumed, sum_t);
        let (s_ll, s_ml, s_of) = (
            ll_base_t + (((v >> (ml_nb_t + of_nb_t)) & ((1u64 << ll_nb_t) - 1)) as u32),
            ml_base_t + (((v >> of_nb_t) & ((1u64 << ml_nb_t) - 1)) as u32),
            of_base_t + ((v & ((1u64 << of_nb_t) - 1)) as u32),
        );
        *ll_state = s_ll;
        *ml_state = s_ml;
        *of_state = s_of;
        if matches!(reload(src_ptr, ip, win, consumed), Reload::Overflow) {
            return Err(DecodeSequenceError::NotEnoughBytesForNumSequences);
        }
    }
    // Dev-feature differential dump: records every decoded sequence for
    // parse comparison against a reference frame (see decoding::seq_dump).
    // Compiled out unless `seq_dump` is enabled, so release builds are
    // untouched.
    #[cfg(feature = "seq_dump")]
    super::seq_dump::record(seq.ll, seq.ml, seq.of);
    Ok(seq)
}

/// Backwards bitstream reader over a 64-bit container, ported from libzstd's
/// BIT_DStream. Only serves the initial (validated) state for `SeqDecoder`;
/// the per-sequence reads run on the decoder's loop-local fields.
struct SeqBitReader<'s> {
    source: &'s [u8],
    /// The container holds `source[ip..ip+8]`.
    ip: usize,
    bits: u64,
    /// Bits consumed from the top of the container. May exceed 64 once the
    /// stream is overconsumed; the accounting stays exact either way.
    consumed: u32,
}

impl<'s> SeqBitReader<'s> {
    /// Start reading from the end of the stream, skipping the zero padding and
    /// the final 1 marker (which must sit within the last byte).
    fn new(source: &'s [u8]) -> Result<Self, DecodeSequenceError> {
        let Some(&last) = source.last() else {
            return Err(DecodeSequenceError::ExtraPadding { skipped_bits: 9 });
        };
        if last == 0 {
            // Marker not within the last byte: invalid stream end
            return Err(DecodeSequenceError::ExtraPadding { skipped_bits: 9 });
        }
        let pad = 1 + last.leading_zeros();
        if source.len() >= 8 {
            let ip = source.len() - 8;
            let bits = u64::from_le_bytes(source[ip..].try_into().unwrap());
            Ok(SeqBitReader {
                source,
                ip,
                bits,
                consumed: pad,
            })
        } else {
            // Short stream: zero-extend above the real bytes so bit positions
            // line up with the regular case; the window never moves.
            let mut buf = [0u8; 8];
            buf[..source.len()].copy_from_slice(source);
            Ok(SeqBitReader {
                source,
                ip: 0,
                bits: u64::from_le_bytes(buf),
                consumed: pad + (8 - source.len() as u32) * 8,
            })
        }
    }
}

pub(crate) enum Reload {
    Unfinished,
    /// Window is clamped at the stream start; reads past the end yield zeroes.
    EndOfBuffer,
    /// Stream overconsumed (more bits read than it contains).
    Overflow,
}

/// Take the next `n` bits (n <= 31) from the pre-shifted window `win`
/// (container `<<` bits-consumed-so-far) and advance the window. Keeping the
/// window pre-shifted instead of shifting by a consumed counter on every read
/// shortens the per-read dependency chain to two shifts — the serial
/// bottleneck of the decode loop — and reads past the container naturally
/// yield zeroes (the surrounding checks turn that into an error).
///
/// Zero-width reads are branchless: `wrapping_shr` lets the hardware's free
/// count masking turn `n == 0` into a shift by zero, returning the unmasked
/// window while advancing nothing. Callers extract their fields under masks
/// (bzhi), and every such mask is zero exactly when the field's width is
/// zero, so the garbage never escapes — the `sum == 0` special cases the
/// decode loop used to carry were a branch on data (all-zero-width reads are
/// the rep0-heavy norm on structured input) and mispredicted constantly.
#[inline(always)]
fn read(win: &mut u64, consumed: &mut u32, n: u32) -> u64 {
    let v = (*win).wrapping_shr(64 - n);
    *win <<= n;
    *consumed += n;
    v
}

/// Read an initial FSE state: the accuracy log doubles as the mask width,
/// which zeroes the result for RLE streams (accuracy log 0).
#[inline(always)]
fn read_state(win: &mut u64, consumed: &mut u32, log: u32) -> u32 {
    (read(win, consumed, log) & ((1u64 << log) - 1)) as u32
}

/// Move the window backwards by the consumed whole bytes. Mirrors
/// BIT_reloadDStream, except the raw container never leaves this function:
/// both paths rebuild the window straight from memory, and when `nb == 0`
/// clamps the retreat the existing window is already correct (`win` always
/// equals `bits_at_last_reload << consumed`; `read` keeps the two in sync),
/// so nothing needs preserving — which is what lets the decode loop carry
/// only `win` and drop `bits` from its state entirely.
#[inline(always)]
pub(crate) fn reload(
    src_ptr: *const u8,
    ip: &mut usize,
    win: &mut u64,
    consumed: &mut u32,
) -> Reload {
    if *consumed > 64 {
        // Stream overconsumed; feeding zeroes would move ip below the
        // stream start, so freeze the window instead
        return Reload::Overflow;
    }
    if *ip >= 8 {
        *ip -= (*consumed >> 3) as usize;
        *consumed &= 7;
        // SAFETY: ip only ever decreases from its initial value len-8, so
        // ip+8 <= src_len holds on this path
        let bits = unsafe { crate::bit_io::load_le64(src_ptr.add(*ip)) };
        *win = bits << *consumed;
        return Reload::Unfinished;
    }
    reload_slow(src_ptr, ip, win, consumed)
}

/// Clamped reload for windows near the stream start. Inlined but cold-placed:
/// it runs once per block at most (the final bytes of a sequence section), so
/// a real call here would force every loop-carried value of the fused decode
/// loop into a stack home — measured as several stores per sequence.
#[cold]
#[inline(always)]
fn reload_slow(src_ptr: *const u8, ip: &mut usize, win: &mut u64, consumed: &mut u32) -> Reload {
    let nb = ((*consumed >> 3) as usize).min(*ip);
    if nb > 0 {
        // ip >= 1 here, so the stream holds at least 9 bytes and the 8-byte
        // read below stays inside it (ip started at len-8 and only decreased)
        *ip -= nb;
        *consumed -= (nb * 8) as u32;
        // SAFETY: ip + 8 <= src_len as proven above
        let bits = unsafe { crate::bit_io::load_le64(src_ptr.add(*ip)) };
        // wrapping: consumed can reach 64 here (error path detects it later)
        *win = bits.wrapping_shl(*consumed);
    }
    // nb == 0: the retreat clamps at the stream start; `win` already equals
    // bits << consumed, so the window drains in place (reads past its end
    // yield zeroes) and the accounting in `finish` flags the mismatch.
    if *ip == 0 {
        Reload::EndOfBuffer
    } else {
        Reload::Unfinished
    }
}

// This info is buried in the symbol compression mode table
/// "The maximum allowed accuracy log for literals length and match length tables is 9"
pub const LL_MAX_LOG: u8 = 9;
/// "The maximum allowed accuracy log for literals length and match length tables is 9"
pub const ML_MAX_LOG: u8 = 9;
/// "The maximum accuracy log for the offset table is 8."
pub const OF_MAX_LOG: u8 = 8;

/// Apply one block's sequence-section modes to the scratch tables without
/// touching its bitstream. Besides the decode entry points this serves the
/// MT pre-scan, which replays the last explicit description of each FSE
/// stream to prime a mid-epoch segment start.
pub(crate) fn maybe_update_fse_tables(
    section: &SequencesHeader,
    source: &[u8],
    scratch: &mut FSEScratch,
) -> Result<usize, DecodeSequenceError> {
    let modes = section
        .modes
        .ok_or(DecodeSequenceError::MissingCompressionMode)?;

    let mut bytes_read = 0;

    match modes.ll_mode() {
        ModeType::FSECompressed => {
            let bytes = scratch
                .literal_lengths
                .read_description(source, LL_MAX_LOG)?;
            bytes_read += bytes;

            vprintln!("Updating ll table");
            vprintln!("Used bytes: {}", bytes);
            scratch.ll_rle = None;
            scratch.ll_predefined = false;
            scratch.ll_seq_valid = false;
            scratch.ll_ready = true;
        },
        ModeType::RLE => {
            vprintln!("Use RLE ll table");
            if source.is_empty() {
                return Err(DecodeSequenceError::MissingByteForRleLlTable);
            }
            bytes_read += 1;
            if source[0] > MAX_LITERAL_LENGTH_CODE {
                return Err(DecodeSequenceError::MissingByteForRleLlTable);
            }
            scratch.ll_rle = Some(source[0]);
            scratch.ll_predefined = false;
            // RLE decodes through a one-state fake table (see pack_tables);
            // zero accuracy bits at the initial state read.
            scratch.literal_lengths.accuracy_log = 0;
            scratch.ll_seq_valid = false;
            scratch.ll_ready = true;
        },
        ModeType::Predefined => {
            vprintln!("Use predefined ll table");
            // The predefined table is immutable; only (re)build it when the
            // scratch table currently holds something else.
            if !scratch.ll_predefined {
                scratch
                    .literal_lengths
                    .set_distribution(LL_DEFAULT_ACC_LOG, &LL_DEFAULT_DISTRIBUTION)?;
                scratch.ll_predefined = true;
                scratch.ll_seq_valid = false;
            }
            scratch.ll_rle = None;
            scratch.ll_ready = true;
        },
        ModeType::Repeat => {
            vprintln!("Repeat ll table");
            // Nothing to do
        },
    }

    let of_source = &source[bytes_read..];

    match modes.of_mode() {
        ModeType::FSECompressed => {
            let bytes = scratch.offsets.read_description(of_source, OF_MAX_LOG)?;
            vprintln!("Updating of table");
            vprintln!("Used bytes: {}", bytes);
            bytes_read += bytes;
            scratch.of_rle = None;
            scratch.of_predefined = false;
            scratch.of_seq_valid = false;
            scratch.of_ready = true;
        },
        ModeType::RLE => {
            vprintln!("Use RLE of table");
            if of_source.is_empty() {
                return Err(DecodeSequenceError::MissingByteForRleOfTable);
            }
            bytes_read += 1;
            if of_source[0] > MAX_OFFSET_CODE {
                return Err(DecodeSequenceError::MissingByteForRleOfTable);
            }
            scratch.of_rle = Some(of_source[0]);
            scratch.of_predefined = false;
            scratch.offsets.accuracy_log = 0;
            scratch.of_seq_valid = false;
            scratch.of_ready = true;
        },
        ModeType::Predefined => {
            vprintln!("Use predefined of table");
            if !scratch.of_predefined {
                scratch
                    .offsets
                    .set_distribution(OF_DEFAULT_ACC_LOG, &OF_DEFAULT_DISTRIBUTION)?;
                scratch.of_predefined = true;
                scratch.of_seq_valid = false;
            }
            scratch.of_rle = None;
            scratch.of_ready = true;
        },
        ModeType::Repeat => {
            vprintln!("Repeat of table");
            // Nothing to do
        },
    }

    let ml_source = &source[bytes_read..];

    match modes.ml_mode() {
        ModeType::FSECompressed => {
            let bytes = scratch
                .match_lengths
                .read_description(ml_source, ML_MAX_LOG)?;
            bytes_read += bytes;
            vprintln!("Updating ml table");
            vprintln!("Used bytes: {}", bytes);
            scratch.ml_rle = None;
            scratch.ml_predefined = false;
            scratch.ml_seq_valid = false;
            scratch.ml_ready = true;
        },
        ModeType::RLE => {
            vprintln!("Use RLE ml table");
            if ml_source.is_empty() {
                return Err(DecodeSequenceError::MissingByteForRleMlTable);
            }
            bytes_read += 1;
            if ml_source[0] > MAX_MATCH_LENGTH_CODE {
                return Err(DecodeSequenceError::MissingByteForRleMlTable);
            }
            scratch.ml_rle = Some(ml_source[0]);
            scratch.ml_predefined = false;
            scratch.match_lengths.accuracy_log = 0;
            scratch.ml_seq_valid = false;
            scratch.ml_ready = true;
        },
        ModeType::Predefined => {
            vprintln!("Use predefined ml table");
            if !scratch.ml_predefined {
                scratch
                    .match_lengths
                    .set_distribution(ML_DEFAULT_ACC_LOG, &ML_DEFAULT_DISTRIBUTION)?;
                scratch.ml_predefined = true;
                scratch.ml_seq_valid = false;
            }
            scratch.ml_rle = None;
            scratch.ml_ready = true;
        },
        ModeType::Repeat => {
            vprintln!("Repeat ml table");
            // Nothing to do
        },
    }

    Ok(bytes_read)
}

#[test]
fn test_ll_default() {
    let mut table = FSETable::new(MAX_LITERAL_LENGTH_CODE);
    table
        .build_from_probabilities(LL_DEFAULT_ACC_LOG, &LL_DEFAULT_DISTRIBUTION)
        .unwrap();

    #[cfg(feature = "std")]
    for idx in 0..table.decode.len() {
        std::println!(
            "{:3}: {:3} {:3} {:3}",
            idx,
            table.decode[idx].symbol,
            table.decode[idx].num_bits,
            table.decode[idx].base_line
        );
    }

    assert_eq!(table.decode.len(), 64);

    // just test a few values. TODO test all values
    assert_eq!(table.decode[0].symbol, 0);
    assert_eq!(table.decode[0].num_bits, 4);
    assert_eq!(table.decode[0].base_line, 0);

    assert_eq!(table.decode[19].symbol, 27);
    assert_eq!(table.decode[19].num_bits, 6);
    assert_eq!(table.decode[19].base_line, 0);

    assert_eq!(table.decode[39].symbol, 25);
    assert_eq!(table.decode[39].num_bits, 4);
    assert_eq!(table.decode[39].base_line, 16);

    assert_eq!(table.decode[60].symbol, 35);
    assert_eq!(table.decode[60].num_bits, 6);
    assert_eq!(table.decode[60].base_line, 0);

    assert_eq!(table.decode[59].symbol, 24);
    assert_eq!(table.decode[59].num_bits, 5);
    assert_eq!(table.decode[59].base_line, 32);
}

/// The two-stage build the packed slots replaced, kept as the test reference:
/// the `Entry` walk followed by a per-entry pack pass.
#[cfg(test)]
fn entry_walk_packed(table: &FSETable, codes: &[(u32, u8)]) -> Vec<u64> {
    table
        .decode
        .iter()
        .map(|e| {
            let (base, add) = codes[e.symbol as usize];
            pack_entry(base, add, e.base_line as u16, e.num_bits)
        })
        .collect()
}

/// Synthetic `(base value, additional bits)` pairs wide enough for any symbol a
/// 255-symbol table holds; the pack fold is code-table independent.
#[cfg(test)]
fn synthetic_codes() -> [(u32, u8); 256] {
    let mut codes = [(0u32, 0u8); 256];
    for (symbol, code) in codes.iter_mut().enumerate() {
        *code = ((symbol * 7 + 1) as u32, (symbol % 17) as u8);
    }
    codes
}

/// Build one distribution every way the decoder does and require the packed
/// slots byte-identical to the two-stage form's: from the probabilities alone
/// with its `Entry` view (the test reference), from the probabilities alone
/// without one (the predefined path), and parsed back off its `write_ncount`
/// description (the FSE path). `codes` sizes the alphabet, so `probs` must not
/// outrun it.
#[cfg(test)]
fn check_packed_distribution(acc_log: u8, probs: &[i32], codes: &[(u32, u8)], what: &str) {
    use crate::{bit_io::BitWriter, fse::fse_encoder::write_ncount};

    let table_size = 1usize << acc_log;
    let states: i32 = probs
        .iter()
        .map(|&p| {
            if p == -1 {
                1
            } else {
                p
            }
        })
        .sum();
    assert_eq!(states, table_size as i32, "{what}: distribution sums");
    let max_symbol = (codes.len() - 1) as u8;

    let mut entries = FSETable::new(max_symbol);
    entries.build_from_probabilities(acc_log, probs).unwrap();
    let reference = entry_walk_packed(&entries, codes);

    let mut packed = alloc::vec![0u64; table_size];
    build_packed_table(&entries, codes, &mut packed);
    assert_eq!(packed, reference, "{what}: entry-view table");

    let mut probs_only = FSETable::new(max_symbol);
    probs_only.set_distribution(acc_log, probs).unwrap();
    assert!(probs_only.decode.is_empty(), "{what}: Entry view built");
    let mut packed = alloc::vec![0u64; table_size];
    build_packed_table(&probs_only, codes, &mut packed);
    assert_eq!(packed, reference, "{what}: probabilities");

    let mut writer = BitWriter::new();
    write_ncount(probs, acc_log, &mut writer);
    let bytes = writer.dump();
    let mut parsed = FSETable::new(max_symbol);
    let used = parsed.read_description(&bytes, 12).unwrap();
    assert_eq!(used, bytes.len(), "{what}: consumed bytes");
    assert_eq!(parsed.accuracy_log, acc_log, "{what}: accuracy log");
    assert!(parsed.decode.is_empty(), "{what}: Entry view built");
    let mut packed = alloc::vec![0u64; table_size];
    build_packed_table(&parsed, codes, &mut packed);
    assert_eq!(packed, reference, "{what}: description");
}

/// The packed slots are what the decode loops read, so the two-pass build must
/// reproduce the `Entry` walk plus pack pass byte for byte.
#[test]
fn packed_table_matches_entry_walk() {
    use crate::fse::ACC_LOG_OFFSET;

    let codes = synthetic_codes();

    // Pseudo-random normalized distributions over the serializable accuracy
    // range, live-symbol counts and low-probability mixes.
    let mut seed = 0x243f_6a88_85a3_08d3u64;
    let mut rand = move || {
        seed ^= seed << 13;
        seed ^= seed >> 7;
        seed ^= seed << 17;
        seed
    };
    for case in 0..150u32 {
        let acc_log = ACC_LOG_OFFSET + (rand() % 8) as u8;
        let table_size = 1i32 << acc_log;
        let nsym = 2 + (rand() % 200) as usize;
        let mut probs = alloc::vec![0i32; nsym];
        let mut budget = table_size;
        let mut has_positive = false;
        for prob in &mut probs {
            let r = rand();
            if budget <= 1 || r % 3 == 0 {
                continue; // dead symbol
            }
            budget -= 1;
            if r % 7 == 0 {
                *prob = -1;
            } else {
                *prob = 1;
                has_positive = true;
            }
        }
        if !has_positive {
            // The pour below needs a positive sink. Promoting a dead slot costs
            // one of the states left over; if every slot is live, promoting one
            // of the single-state symbols costs none.
            if let Some(i) = probs.iter().position(|&p| p == 0) {
                probs[i] = 1;
                budget -= 1;
            } else {
                let i = probs.iter().position(|&p| p == -1).unwrap();
                probs[i] = 1;
            }
        }
        // Pour the remaining states onto the live positives in random chunks so
        // the sum lands exactly on the table size.
        while budget > 0 {
            let idx = (rand() as usize) % nsym;
            if probs[idx] > 0 {
                let add = (1 + rand() % budget as u64) as i32;
                probs[idx] += add;
                budget -= add;
            }
        }
        check_packed_distribution(
            acc_log,
            &probs,
            &codes,
            &alloc::format!("random case {case}"),
        );
    }

    // Hand-built spread shapes, the distribution extremes a random draw covers
    // thinly.
    let mut wide = alloc::vec![0i32; 200];
    for (i, prob) in wide.iter_mut().enumerate() {
        *prob = match i % 7 {
            0 => -1,
            1 | 4 => 0,
            2 | 5 => 2,
            _ => 4,
        };
    }
    let placed: i32 = wide
        .iter()
        .map(|&p| {
            if p == -1 {
                1
            } else {
                p
            }
        })
        .sum();
    wide[3] += (1 << 9) - placed;
    let classes: [(&str, u8, &[i32]); 6] = [
        // One symbol owns every state.
        ("single symbol", 6, &[64]),
        // A power-of-two split: every symbol a double-width slice.
        ("power-of-two split", 6, &[16, 16, 16, 16]),
        // Low-probability symbols at both ends of the alphabet.
        ("-1 symbols at both ends", 6, &[-1, -1, 20, 20, 20, -1, -1]),
        // Dead symbols between the live ones.
        ("interior dead symbols", 6, &[
            8, 0, 0, 0, 8, 8, 0, 0, 8, 8, 0, 24,
        ]),
        // Every state its own single-state symbol, no mass left to spread.
        ("full single-state alphabet", 5, &[1; 32]),
        ("wide alphabet", 9, &wide),
    ];
    for (what, acc_log, probs) in classes {
        check_packed_distribution(acc_log, probs, &codes, what);
    }

    // The three RFC default tables, from their distributions and off their wire
    // descriptions.
    check_packed_distribution(
        LL_DEFAULT_ACC_LOG,
        &LL_DEFAULT_DISTRIBUTION,
        &LL_CODES,
        "ll default",
    );
    check_packed_distribution(
        ML_DEFAULT_ACC_LOG,
        &ML_DEFAULT_DISTRIBUTION,
        &ML_CODES,
        "ml default",
    );
    check_packed_distribution(
        OF_DEFAULT_ACC_LOG,
        &OF_DEFAULT_DISTRIBUTION,
        &OF_CODES,
        "of default",
    );
}

/// The packed build keeps no per-table fill state on the description path: a
/// pooled `FSETable` reused across shrinking and growing wire descriptions
/// must produce exactly the slots of a fresh table.
#[test]
fn packed_table_survives_table_reuse() {
    use crate::{bit_io::BitWriter, fse::fse_encoder::write_ncount};

    let codes = synthetic_codes();
    // All sum to 64 (accuracy log 6); zero-probability gaps keep the live
    // range shorter than the previous table's.
    let long = [0i32, 0, -1, 3, 2, 2, 0, 0, 10, 15, 0, 31];
    let short = [0, 33, 0, 31];

    let mut reused = FSETable::new(255);
    for probs in [&long[..], &short[..], &long[..], &short[..]] {
        let mut writer = BitWriter::new();
        write_ncount(probs, 6, &mut writer);
        let bytes = writer.dump();

        let mut fresh = FSETable::new(255);
        fresh.read_description(&bytes, 9).unwrap();
        reused.reset();
        reused.read_description(&bytes, 9).unwrap();

        let mut reference = alloc::vec![0u64; 1 << 6];
        build_packed_table(&fresh, &codes, &mut reference);
        let mut packed = alloc::vec![0u64; 1 << 6];
        build_packed_table(&reused, &codes, &mut packed);
        assert_eq!(packed, reference);
    }
}

/// The flat executor's per-field loads and the staging decoder's shifts read
/// the same packed entry; the byte positions behind `EntryRef` are
/// endian-specific, so this pins them against the shift form.
#[test]
fn entry_field_loads_match_shift_extraction() {
    let entries = [
        pack_entry(0, 0, 0, 0),
        pack_entry(65539, 16, 511, 9),
        pack_entry(0x7fff_fffd, 31, 0x1234, 8),
        pack_entry(u32::MAX, 0xff, u16::MAX, 0xff),
    ];
    for (i, &e) in entries.iter().enumerate() {
        // SAFETY: state `i` indexes inside `entries`
        let r = unsafe { EntryRef::at(entries.as_ptr(), 0, i as u32) };
        assert_eq!(
            (r.base(), r.add_bits(), r.nb_bits(), r.next_state()),
            (
                entry_base(e),
                entry_add_bits(e),
                entry_nb_bits(e),
                entry_next_state(e)
            ),
            "entry {i}"
        );
    }
}
