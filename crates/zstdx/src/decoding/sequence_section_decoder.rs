use super::super::blocks::sequence_section::ModeType;
use super::super::blocks::sequence_section::Sequence;
use super::super::blocks::sequence_section::SequencesHeader;
use super::scratch::FSEScratch;
use crate::blocks::sequence_section::{
    MAX_LITERAL_LENGTH_CODE, MAX_MATCH_LENGTH_CODE, MAX_OFFSET_CODE,
};
use crate::decoding::errors::DecodeSequenceError;
use crate::fse::FSETable;
use alloc::vec::Vec;
use core::convert::TryInto;

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
        if std::is_x86_feature_detected!("bmi2") {
            // SAFETY: bmi2 was just detected at runtime
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
        if std::is_x86_feature_detected!("bmi2") {
            // SAFETY: bmi2 was just detected at runtime
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
    // SAFETY: start + nseq slots were just reserved; each iteration writes
    // exactly one Sequence
    let out = target.as_mut_ptr();
    let mut idx = 0;
    while dec.rem > 0 {
        let seq = dec.next()?;
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
const LL_SLOT: usize = 0;
const ML_SLOT: usize = 1 << LL_MAX_LOG;
const OF_SLOT: usize = ML_SLOT + (1 << ML_MAX_LOG);
pub(crate) const SEQ_TABLE_SLOTS: usize = OF_SLOT + (1 << OF_MAX_LOG);

/// Pack a sequence decoding table into its fixed slot: one u64 per FSE state
/// carrying everything the decode loop needs for a symbol, so a single load
/// serves a stream. Layout: `[base_value: u32][add_bits: u8][next_state_base: u16][nb_bits: u8]`.
/// Slot entries beyond the table's real size stay stale — states provably
/// never reach them (every transition stays below 1 << accuracy_log).
fn pack_seq_table(table: &FSETable, codes: &[(u32, u8)], out: &mut [u64]) {
    for (dst, e) in out.iter_mut().zip(&table.decode) {
        let sym = e.symbol as usize;
        let (base, add) = codes[sym];
        *dst = ((base as u64) << 32)
            | ((add as u64) << 24)
            | ((e.base_line as u64) << 8)
            | e.num_bits as u64;
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

/// Rebuild the packed table slots whose source tables changed since the last
/// block. RLE streams get a one-state fake table in slot 0: the entry carries
/// the code's (base, add-bits) with a zero transition, so the decode loop
/// stays in state 0 forever and needs no RLE special cases at all (zero
/// accuracy-log bits at the initial state read).
fn pack_tables(scratch: &mut FSEScratch) {
    if !scratch.ll_seq_valid {
        match scratch.ll_rle {
            Some(c) => pack_rle_entry(LL_CODES[c as usize], &mut scratch.seq_packed[LL_SLOT]),
            None => pack_seq_table(
                &scratch.literal_lengths,
                &LL_CODES,
                &mut scratch.seq_packed[LL_SLOT..ML_SLOT],
            ),
        }
        scratch.ll_seq_valid = true;
    }
    if !scratch.ml_seq_valid {
        match scratch.ml_rle {
            Some(c) => pack_rle_entry(ML_CODES[c as usize], &mut scratch.seq_packed[ML_SLOT]),
            None => pack_seq_table(
                &scratch.match_lengths,
                &ML_CODES,
                &mut scratch.seq_packed[ML_SLOT..OF_SLOT],
            ),
        }
        scratch.ml_seq_valid = true;
    }
    if !scratch.of_seq_valid {
        match scratch.of_rle {
            Some(c) => pack_rle_entry(OF_CODES[c as usize], &mut scratch.seq_packed[OF_SLOT]),
            None => pack_seq_table(
                &scratch.offsets,
                &OF_CODES,
                &mut scratch.seq_packed[OF_SLOT..],
            ),
        }
        scratch.of_seq_valid = true;
    }
}

/// Write the self-transitioning entry an RLE stream decodes through.
fn pack_rle_entry((base, add): (u32, u8), slot: &mut u64) {
    *slot = ((base as u64) << 32) | ((add as u64) << 24);
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
        pack_tables(scratch);

        let uninit = || {
            DecodeSequenceError::FSEDecoderError(
                crate::decoding::errors::FSEDecoderError::TableIsUninitialized,
            )
        };
        if !scratch.ll_ready || !scratch.of_ready || !scratch.ml_ready {
            // Repeat mode with no table ever established (RLE streams count:
            // their one-state fake table lives in the packed slot)
            return Err(uninit());
        }

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
        if rem > 0 {
            Err(DecodeSequenceError::ExtraBits {
                bits_remaining: rem,
            })
        } else if rem < 0 {
            Err(DecodeSequenceError::NotEnoughBytesForNumSequences)
        } else {
            Ok(())
        }
    }
}

/// Decode one sequence and advance the streams for the one after it (the
/// body of `next`). The caller guarantees `*rem > 0`; `rem` counts down and
/// the last sequence skips the state transitions and the reload (they would
/// read past the stream's end). State passes as scalars so the caller's SROA
/// sees plain local variables; the flat executor calls this directly with
/// loop-local state to keep it register-resident (see `sequence_execution`).
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
    // independent of the bitstream content
    let ll_entry = unsafe { *tbl.add(LL_SLOT + *ll_state as usize) };
    // SAFETY: same invariant as the ll stream
    let ml_entry = unsafe { *tbl.add(ML_SLOT + *ml_state as usize) };
    // SAFETY: same invariant as the ll stream
    let of_entry = unsafe { *tbl.add(OF_SLOT + *of_state as usize) };
    let (ll_base, ll_nb) = ((ll_entry >> 32) as u32, ((ll_entry >> 24) & 0xFF) as u32);
    let (ml_base, ml_nb) = ((ml_entry >> 32) as u32, ((ml_entry >> 24) & 0xFF) as u32);
    let (of_base, of_nb) = ((of_entry >> 32) as u32, ((of_entry >> 24) & 0xFF) as u32);

    // All three add-bit widths are known before reading, so the fields (of,
    // then ml, then ll in read order, from the top) can be extracted with a
    // single serial window read and split in parallel off the extracted
    // value — one dependent chain instead of three. sum <= 31 keeps the
    // extraction shift in range, and 7 + sum + 26 transition bits <= 64
    // means the post-reload window always covers it: the mid-sequence reload
    // guard (libzstd's totalBits guard) only applies to the unbatched path.
    // All fields are masked, so a zero-width total (rep0-heavy sequences)
    // needs no branch: every mask is zero exactly then.
    let sum = ll_nb + ml_nb + of_nb;
    let (obits, ml_add, ll_add) = if sum <= 31 {
        let v = read(win, consumed, sum);
        (
            (v >> (ml_nb + ll_nb)) & ((1u64 << of_nb) - 1),
            (v >> ll_nb) & ((1u64 << ml_nb) - 1),
            v & ((1u64 << ll_nb) - 1),
        )
    } else {
        // Cold: fields too wide to batch. of+ml+ll add bits sum above the 57
        // bits guaranteed after a reload; reload mid-sequence then
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
        let ll_nb_t = (ll_entry & 0xFF) as u32;
        let ml_nb_t = (ml_entry & 0xFF) as u32;
        let of_nb_t = (of_entry & 0xFF) as u32;
        let ll_base_t = ((ll_entry >> 8) & 0xFFFF) as u32;
        let ml_base_t = ((ml_entry >> 8) & 0xFFFF) as u32;
        let of_base_t = ((of_entry >> 8) & 0xFFFF) as u32;
        // Same batching for the three state transitions (ll, then ml, then
        // of): at most 9+9+8 = 26 bits, always extractable in one read, with
        // every field masked so a zero-width total needs no branch
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

enum Reload {
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
fn reload(src_ptr: *const u8, ip: &mut usize, win: &mut u64, consumed: &mut u32) -> Reload {
    if *consumed > 64 {
        // Stream overconsumed; feeding zeroes would move ip below the
        // stream start, so freeze the window instead
        return Reload::Overflow;
    }
    if *ip >= 8 {
        // SAFETY: ip only ever decreases from its initial value len-8, so
        // ip+8 <= src_len holds on this path
        *ip -= (*consumed >> 3) as usize;
        *consumed &= 7;
        let bits = unsafe { src_ptr.add(*ip).cast::<u64>().read_unaligned() };
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
        let bits = unsafe { src_ptr.add(*ip).cast::<u64>().read_unaligned() };
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

fn maybe_update_fse_tables(
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
            let bytes = scratch.literal_lengths.build_decoder(source, LL_MAX_LOG)?;
            bytes_read += bytes;

            vprintln!("Updating ll table");
            vprintln!("Used bytes: {}", bytes);
            scratch.ll_rle = None;
            scratch.ll_predefined = false;
            scratch.ll_seq_valid = false;
            scratch.ll_ready = true;
        }
        ModeType::RLE => {
            vprintln!("Use RLE ll table");
            if source.is_empty() {
                return Err(DecodeSequenceError::MissingByteForRleLlTable);
            }
            bytes_read += 1;
            if source[0] > MAX_LITERAL_LENGTH_CODE {
                return Err(DecodeSequenceError::MissingByteForRleMlTable);
            }
            scratch.ll_rle = Some(source[0]);
            scratch.ll_predefined = false;
            // RLE decodes through a one-state fake table (see pack_tables);
            // zero accuracy bits at the initial state read.
            scratch.literal_lengths.accuracy_log = 0;
            scratch.ll_seq_valid = false;
            scratch.ll_ready = true;
        }
        ModeType::Predefined => {
            vprintln!("Use predefined ll table");
            // The predefined table is immutable; only (re)build it when the
            // scratch table currently holds something else.
            if !scratch.ll_predefined {
                scratch.literal_lengths.build_from_probabilities(
                    LL_DEFAULT_ACC_LOG,
                    &LITERALS_LENGTH_DEFAULT_DISTRIBUTION,
                )?;
                scratch.ll_predefined = true;
                scratch.ll_seq_valid = false;
            }
            scratch.ll_rle = None;
            scratch.ll_ready = true;
        }
        ModeType::Repeat => {
            vprintln!("Repeat ll table");
            /* Nothing to do */
        }
    };

    let of_source = &source[bytes_read..];

    match modes.of_mode() {
        ModeType::FSECompressed => {
            let bytes = scratch.offsets.build_decoder(of_source, OF_MAX_LOG)?;
            vprintln!("Updating of table");
            vprintln!("Used bytes: {}", bytes);
            bytes_read += bytes;
            scratch.of_rle = None;
            scratch.of_predefined = false;
            scratch.of_seq_valid = false;
            scratch.of_ready = true;
        }
        ModeType::RLE => {
            vprintln!("Use RLE of table");
            if of_source.is_empty() {
                return Err(DecodeSequenceError::MissingByteForRleOfTable);
            }
            bytes_read += 1;
            if of_source[0] > MAX_OFFSET_CODE {
                return Err(DecodeSequenceError::MissingByteForRleMlTable);
            }
            scratch.of_rle = Some(of_source[0]);
            scratch.of_predefined = false;
            scratch.offsets.accuracy_log = 0;
            scratch.of_seq_valid = false;
            scratch.of_ready = true;
        }
        ModeType::Predefined => {
            vprintln!("Use predefined of table");
            if !scratch.of_predefined {
                scratch
                    .offsets
                    .build_from_probabilities(OF_DEFAULT_ACC_LOG, &OFFSET_DEFAULT_DISTRIBUTION)?;
                scratch.of_predefined = true;
                scratch.of_seq_valid = false;
            }
            scratch.of_rle = None;
            scratch.of_ready = true;
        }
        ModeType::Repeat => {
            vprintln!("Repeat of table");
            /* Nothing to do */
        }
    };

    let ml_source = &source[bytes_read..];

    match modes.ml_mode() {
        ModeType::FSECompressed => {
            let bytes = scratch.match_lengths.build_decoder(ml_source, ML_MAX_LOG)?;
            bytes_read += bytes;
            vprintln!("Updating ml table");
            vprintln!("Used bytes: {}", bytes);
            scratch.ml_rle = None;
            scratch.ml_predefined = false;
            scratch.ml_seq_valid = false;
            scratch.ml_ready = true;
        }
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
        }
        ModeType::Predefined => {
            vprintln!("Use predefined ml table");
            if !scratch.ml_predefined {
                scratch.match_lengths.build_from_probabilities(
                    ML_DEFAULT_ACC_LOG,
                    &MATCH_LENGTH_DEFAULT_DISTRIBUTION,
                )?;
                scratch.ml_predefined = true;
                scratch.ml_seq_valid = false;
            }
            scratch.ml_rle = None;
            scratch.ml_ready = true;
        }
        ModeType::Repeat => {
            vprintln!("Repeat ml table");
            /* Nothing to do */
        }
    };

    Ok(bytes_read)
}

// The default Literal Length decoding table uses an accuracy logarithm of 6 bits.
const LL_DEFAULT_ACC_LOG: u8 = 6;
/// If [ModeType::Predefined] is selected for a symbol type, its FSE decoding
/// table is generated using a predefined distribution table.
///
/// https://github.com/facebook/zstd/blob/dev/doc/zstd_compression_format.md#literals-length
const LITERALS_LENGTH_DEFAULT_DISTRIBUTION: [i32; 36] = [
    4, 3, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 1, 1, 1, 2, 2, 2, 2, 2, 2, 2, 2, 2, 3, 2, 1, 1, 1, 1, 1,
    -1, -1, -1, -1,
];

// The default Match Length decoding table uses an accuracy logarithm of 6 bits.
const ML_DEFAULT_ACC_LOG: u8 = 6;
/// If [ModeType::Predefined] is selected for a symbol type, its FSE decoding
/// table is generated using a predefined distribution table.
///
/// https://github.com/facebook/zstd/blob/dev/doc/zstd_compression_format.md#match-length
const MATCH_LENGTH_DEFAULT_DISTRIBUTION: [i32; 53] = [
    1, 4, 3, 2, 2, 2, 2, 2, 2, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1,
    1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, -1, -1, -1, -1, -1, -1, -1,
];

// The default Match Length decoding table uses an accuracy logarithm of 5 bits.
const OF_DEFAULT_ACC_LOG: u8 = 5;
/// If [ModeType::Predefined] is selected for a symbol type, its FSE decoding
/// table is generated using a predefined distribution table.
///
/// https://github.com/facebook/zstd/blob/dev/doc/zstd_compression_format.md#match-length
const OFFSET_DEFAULT_DISTRIBUTION: [i32; 29] = [
    1, 1, 1, 1, 1, 1, 2, 2, 2, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, -1, -1, -1, -1, -1,
];

#[test]
fn test_ll_default() {
    let mut table = crate::fse::FSETable::new(MAX_LITERAL_LENGTH_CODE);
    table
        .build_from_probabilities(
            LL_DEFAULT_ACC_LOG,
            &LITERALS_LENGTH_DEFAULT_DISTRIBUTION.to_vec(),
        )
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

    assert!(table.decode.len() == 64);

    //just test a few values. TODO test all values
    assert!(table.decode[0].symbol == 0);
    assert!(table.decode[0].num_bits == 4);
    assert!(table.decode[0].base_line == 0);

    assert!(table.decode[19].symbol == 27);
    assert!(table.decode[19].num_bits == 6);
    assert!(table.decode[19].base_line == 0);

    assert!(table.decode[39].symbol == 25);
    assert!(table.decode[39].num_bits == 4);
    assert!(table.decode[39].base_line == 16);

    assert!(table.decode[60].symbol == 35);
    assert!(table.decode[60].num_bits == 6);
    assert!(table.decode[60].base_line == 0);

    assert!(table.decode[59].symbol == 24);
    assert!(table.decode[59].num_bits == 5);
    assert!(table.decode[59].base_line == 32);
}
