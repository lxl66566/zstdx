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
    let bytes_read = maybe_update_fse_tables(section, source, scratch)?;

    vprintln!("Updating tables used {} bytes", bytes_read);

    let bit_stream = &source[bytes_read..];

    let mut br = SeqBitReader::new(bit_stream)?;

    // BMI2 compiles the loop's variable shifts to single-uop shlx/shrx; the
    // detection cache makes this dispatch cheap relative to a whole block.
    #[cfg(all(target_arch = "x86_64", feature = "std"))]
    {
        if std::is_x86_feature_detected!("bmi2") {
            // SAFETY: bmi2 was just detected at runtime
            return unsafe {
                if scratch.ll_rle.is_some()
                    || scratch.ml_rle.is_some()
                    || scratch.of_rle.is_some()
                {
                    decode_sequences_with_rle_bmi2(section, &mut br, scratch, target)
                } else {
                    decode_sequences_without_rle_bmi2(section, &mut br, scratch, target)
                }
            };
        }
    }

    if scratch.ll_rle.is_some() || scratch.ml_rle.is_some() || scratch.of_rle.is_some() {
        decode_sequences_with_rle(section, &mut br, scratch, target)
    } else {
        decode_sequences_without_rle(section, &mut br, scratch, target)
    }
}

/// Backwards bitstream reader over a 64-bit container, ported from libzstd's
/// BIT_DStream. The window position is tracked by a byte index instead of
/// recounting consumed bits, and reads are served by two shifts on the
/// container. Reads must respect the reload discipline: after a reload at most
/// 57 bits are available, which bounds how far reads may run between reloads.
///
/// The decode loops copy `ip`/`bits`/`consumed` into locals so the container
/// stays in a register; the struct itself only serves the initial state and
/// the cold clamped-reload path (`reload_slow`).
struct SeqBitReader<'s> {
    source: &'s [u8],
    /// The container holds `source[ip..ip+8]`.
    ip: usize,
    bits: u64,
    /// Bits consumed from the top of the container. May exceed 64 once the
    /// stream is overconsumed; `remaining` stays exact either way.
    consumed: u32,
}

enum Reload {
    Unfinished,
    /// Window is clamped at the stream start; reads past the end yield zeroes.
    EndOfBuffer,
    /// Stream overconsumed (more bits read than it contains).
    Overflow,
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

    /// Exact number of unread bits; negative when overconsumed.
    fn remaining(&self) -> isize {
        self.ip as isize * 8 + 64 - self.consumed as isize
    }

    /// Clamped reload for windows near the stream start, kept out of line so
    /// the hot path can inline on the loop-local reader state.
    #[cold]
    #[inline(never)]
    fn reload_slow(&mut self) -> Reload {
        let nb = ((self.consumed >> 3) as usize).min(self.ip);
        self.ip -= nb;
        self.consumed -= (nb * 8) as u32;
        if nb > 0 && self.source.len() >= 8 {
            self.bits = u64::from_le_bytes(self.source[self.ip..][..8].try_into().unwrap());
        }
        if self.ip == 0 {
            Reload::EndOfBuffer
        } else {
            Reload::Unfinished
        }
    }
}

/// Take the next `n` bits (n <= 31) from the pre-shifted window `win`
/// (container `<<` bits-consumed-so-far) and advance the window. Keeping the
/// window pre-shifted instead of shifting by a consumed counter on every read
/// shortens the per-read dependency chain to two shifts — the serial
/// bottleneck of the decode loop — and reads past the container naturally
/// yield zeroes (the surrounding checks turn that into an error).
#[inline(always)]
fn read(win: &mut u64, consumed: &mut u32, n: u32) -> u64 {
    if n == 0 {
        return 0;
    }
    let v = *win >> (64 - n);
    *win <<= n;
    *consumed += n;
    v
}

/// Move the window backwards by the consumed whole bytes, operating on the
/// loop-local reader state. Mirrors BIT_reloadDStream: fast path when at
/// least 8 bytes remain below the window, clamped out-of-line path near the
/// stream start (which round-trips the state through the reader struct).
#[inline(always)]
fn reload(
    br: &mut SeqBitReader<'_>,
    src_ptr: *const u8,
    ip: &mut usize,
    bits: &mut u64,
    win: &mut u64,
    consumed: &mut u32,
) -> Reload {
    if *consumed > 64 {
        // Stream overconsumed; feeding zeroes would move ip below the
        // stream start, so freeze the window instead
        return Reload::Overflow;
    }
    if *ip >= 8 {
        // SAFETY: ip only ever decreases from its initial value len-8, so
        // ip+8 <= source.len() holds on this path
        *ip -= (*consumed >> 3) as usize;
        *consumed &= 7;
        *bits = unsafe { src_ptr.add(*ip).cast::<u64>().read_unaligned() };
        *win = *bits << *consumed;
        return Reload::Unfinished;
    }
    br.ip = *ip;
    br.bits = *bits;
    br.consumed = *consumed;
    let r = br.reload_slow();
    *ip = br.ip;
    *bits = br.bits;
    *consumed = br.consumed;
    // wrapping: short streams can clamp with consumed == 64 (error path)
    *win = br.bits.wrapping_shl(br.consumed);
    r
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
/// block.
fn pack_tables(scratch: &mut FSEScratch) {
    if !scratch.ll_seq_valid {
        pack_seq_table(
            &scratch.literal_lengths,
            &LL_CODES,
            &mut scratch.seq_packed[LL_SLOT..ML_SLOT],
        );
        scratch.ll_seq_valid = true;
    }
    if !scratch.ml_seq_valid {
        pack_seq_table(
            &scratch.match_lengths,
            &ML_CODES,
            &mut scratch.seq_packed[ML_SLOT..OF_SLOT],
        );
        scratch.ml_seq_valid = true;
    }
    if !scratch.of_seq_valid {
        pack_seq_table(&scratch.offsets, &OF_CODES, &mut scratch.seq_packed[OF_SLOT..]);
        scratch.of_seq_valid = true;
    }
}

/// Portable entry; the body lives in `#[inline(always)]` impls so the BMI2
/// wrappers below compile a specialized copy of the same loop.
fn decode_sequences_without_rle(
    section: &SequencesHeader,
    br: &mut SeqBitReader<'_>,
    scratch: &mut FSEScratch,
    target: &mut Vec<Sequence>,
) -> Result<(), DecodeSequenceError> {
    decode_sequences_without_rle_impl(section, br, scratch, target)
}

#[cfg(all(target_arch = "x86_64", feature = "std"))]
#[target_feature(enable = "bmi2")]
unsafe fn decode_sequences_without_rle_bmi2(
    section: &SequencesHeader,
    br: &mut SeqBitReader<'_>,
    scratch: &mut FSEScratch,
    target: &mut Vec<Sequence>,
) -> Result<(), DecodeSequenceError> {
    decode_sequences_without_rle_impl(section, br, scratch, target)
}

#[inline(always)]
fn decode_sequences_without_rle_impl(
    section: &SequencesHeader,
    br: &mut SeqBitReader<'_>,
    scratch: &mut FSEScratch,
    target: &mut Vec<Sequence>,
) -> Result<(), DecodeSequenceError> {
    pack_tables(scratch);

    let ll_acc = scratch.literal_lengths.accuracy_log;
    let ml_acc = scratch.match_lengths.accuracy_log;
    let of_acc = scratch.offsets.accuracy_log;
    if ll_acc == 0 || ml_acc == 0 || of_acc == 0 {
        return Err(DecodeSequenceError::FSEDecoderError(
            crate::decoding::errors::FSEDecoderError::TableIsUninitialized,
        ));
    }

    // SAFETY (all table loads below): FSE table construction guarantees every
    // reachable state (initial accuracy-log reads and next_state_base plus the
    // value read with nb_bits) stays below the table size, independent of the
    // bitstream content
    let tbl = scratch.seq_packed.as_ptr();
    let src_ptr = br.source.as_ptr();
    // Reader state as loop locals; the struct is only touched on the cold
    // reload path and at the ends, so the bit container stays in a register.
    let mut ip = br.ip;
    let mut bits = br.bits;
    let mut consumed = br.consumed;
    // wrapping: degenerate short streams clamp with consumed == 64 (error path)
    let mut win = bits.wrapping_shl(consumed);

    // Initial states are read in the order ll, of, ml
    let ll_state = read(&mut win, &mut consumed, ll_acc as u32) as u32;
    let of_state = read(&mut win, &mut consumed, of_acc as u32) as u32;
    let ml_state = read(&mut win, &mut consumed, ml_acc as u32) as u32;
    // Realign the window before the first sequence; the loop only reloads at
    // sequence ends (the state reads above already consumed up to 26 bits)
    reload(br, src_ptr, &mut ip, &mut bits, &mut win, &mut consumed);
    let mut ll_entry = unsafe { *tbl.add(LL_SLOT + ll_state as usize) };
    let mut of_entry = unsafe { *tbl.add(OF_SLOT + of_state as usize) };
    let mut ml_entry = unsafe { *tbl.add(ML_SLOT + ml_state as usize) };

    target.clear();
    target.reserve(section.num_sequences as usize);
    let nseq = section.num_sequences as usize;
    // SAFETY: nseq slots were just reserved; each iteration writes exactly
    // one Sequence and the length is only exposed on success
    let out = target.as_mut_ptr();

    for idx in 0..nseq {
        let e_ll = ll_entry;
        let e_ml = ml_entry;
        let e_of = of_entry;
        let ll_add_bits = ((e_ll >> 24) & 0xFF) as u32;
        let ml_add_bits = ((e_ml >> 24) & 0xFF) as u32;
        let of_add_bits = ((e_of >> 24) & 0xFF) as u32;

        let obits = read(&mut win, &mut consumed, of_add_bits);
        let ml_add = read(&mut win, &mut consumed, ml_add_bits);
        // of+ml+ll add bits can sum above the 57 bits guaranteed after a
        // reload; reload mid-sequence then (libzstd's totalBits guard)
        if ll_add_bits + ml_add_bits + of_add_bits >= 31 {
            if matches!(
                reload(br, src_ptr, &mut ip, &mut bits, &mut win, &mut consumed),
                Reload::Overflow
            ) {
                return Err(DecodeSequenceError::NotEnoughBytesForNumSequences);
            }
        }
        let ll_add = read(&mut win, &mut consumed, ll_add_bits);

        unsafe {
            out.add(idx).write(Sequence {
                ll: ((e_ll >> 32) as u32) + ll_add as u32,
                ml: ((e_ml >> 32) as u32) + ml_add as u32,
                of: ((e_of >> 32) as u32) + obits as u32,
            });
        }

        if idx + 1 < nseq {
            let nb = (e_ll & 0xFF) as u32;
            let state = (((e_ll >> 8) & 0xFFFF) as u32) + read(&mut win, &mut consumed, nb) as u32;
            ll_entry = unsafe { *tbl.add(LL_SLOT + state as usize) };
            let nb = (e_ml & 0xFF) as u32;
            let state = (((e_ml >> 8) & 0xFFFF) as u32) + read(&mut win, &mut consumed, nb) as u32;
            ml_entry = unsafe { *tbl.add(ML_SLOT + state as usize) };
            let nb = (e_of & 0xFF) as u32;
            let state = (((e_of >> 8) & 0xFFFF) as u32) + read(&mut win, &mut consumed, nb) as u32;
            of_entry = unsafe { *tbl.add(OF_SLOT + state as usize) };
            if matches!(
                reload(br, src_ptr, &mut ip, &mut bits, &mut win, &mut consumed),
                Reload::Overflow
            ) {
                return Err(DecodeSequenceError::NotEnoughBytesForNumSequences);
            }
        }
    }
    // SAFETY: nseq sequences were written above
    unsafe { target.set_len(nseq) };

    br.ip = ip;
    br.bits = bits;
    br.consumed = consumed;
    let rem = br.remaining();
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

fn decode_sequences_with_rle(
    section: &SequencesHeader,
    br: &mut SeqBitReader<'_>,
    scratch: &mut FSEScratch,
    target: &mut Vec<Sequence>,
) -> Result<(), DecodeSequenceError> {
    decode_sequences_with_rle_impl(section, br, scratch, target)
}

#[cfg(all(target_arch = "x86_64", feature = "std"))]
#[target_feature(enable = "bmi2")]
unsafe fn decode_sequences_with_rle_bmi2(
    section: &SequencesHeader,
    br: &mut SeqBitReader<'_>,
    scratch: &mut FSEScratch,
    target: &mut Vec<Sequence>,
) -> Result<(), DecodeSequenceError> {
    decode_sequences_with_rle_impl(section, br, scratch, target)
}

#[inline(always)]
fn decode_sequences_with_rle_impl(
    section: &SequencesHeader,
    br: &mut SeqBitReader<'_>,
    scratch: &mut FSEScratch,
    target: &mut Vec<Sequence>,
) -> Result<(), DecodeSequenceError> {
    pack_tables(scratch);

    let ll_rle = scratch.ll_rle;
    let ml_rle = scratch.ml_rle;
    let of_rle = scratch.of_rle;

    // SAFETY (table loads below): same invariant as the non-RLE loop
    let tbl = scratch.seq_packed.as_ptr();
    let src_ptr = br.source.as_ptr();
    let mut ip = br.ip;
    let mut bits = br.bits;
    let mut consumed = br.consumed;
    // wrapping: degenerate short streams clamp with consumed == 64 (error path)
    let mut win = bits.wrapping_shl(consumed);

    // Initial states are read in the order ll, of, ml (only for FSE streams)
    let uninit = || {
        Err(DecodeSequenceError::FSEDecoderError(
            crate::decoding::errors::FSEDecoderError::TableIsUninitialized,
        ))
    };
    let ll_state = match ll_rle {
        None => read(&mut win, &mut consumed, scratch.literal_lengths.accuracy_log as u32) as u32,
        Some(_) => 0,
    };
    let of_state = match of_rle {
        None => {
            if scratch.offsets.accuracy_log == 0 {
                return uninit();
            }
            read(&mut win, &mut consumed, scratch.offsets.accuracy_log as u32) as u32
        }
        Some(_) => 0,
    };
    let ml_state = match ml_rle {
        None => {
            if scratch.match_lengths.accuracy_log == 0 {
                return uninit();
            }
            read(&mut win, &mut consumed, scratch.match_lengths.accuracy_log as u32) as u32
        }
        Some(_) => 0,
    };
    if ll_rle.is_none() && scratch.literal_lengths.accuracy_log == 0 {
        return uninit();
    }
    // Realign the window before the first sequence; the loop only reloads at
    // sequence ends (the state reads above may have consumed up to 26 bits)
    reload(br, src_ptr, &mut ip, &mut bits, &mut win, &mut consumed);

    let mut ll_entry = if ll_rle.is_none() {
        unsafe { *tbl.add(LL_SLOT + ll_state as usize) }
    } else {
        0
    };
    let mut ml_entry = if ml_rle.is_none() {
        unsafe { *tbl.add(ML_SLOT + ml_state as usize) }
    } else {
        0
    };
    let mut of_entry = if of_rle.is_none() {
        unsafe { *tbl.add(OF_SLOT + of_state as usize) }
    } else {
        0
    };

    // Fixed per-stream values for RLE streams, extracted once
    let (ll_base_rle, ll_nb_rle) = match ll_rle {
        Some(c) => LL_CODES[c as usize],
        None => (0, 0),
    };
    let (ml_base_rle, ml_nb_rle) = match ml_rle {
        Some(c) => ML_CODES[c as usize],
        None => (0, 0),
    };
    let (of_base_rle, of_nb_rle) = match of_rle {
        Some(c) => OF_CODES[c as usize],
        None => (0, 0),
    };

    target.clear();
    target.reserve(section.num_sequences as usize);
    let nseq = section.num_sequences as usize;
    // SAFETY: nseq slots were just reserved; the length is only exposed on
    // success
    let out = target.as_mut_ptr();

    for idx in 0..nseq {
        let (ll_base, ll_add_bits) = match ll_rle {
            Some(_) => (ll_base_rle, ll_nb_rle as u32),
            None => ((ll_entry >> 32) as u32, ((ll_entry >> 24) & 0xFF) as u32),
        };
        let (ml_base, ml_add_bits) = match ml_rle {
            Some(_) => (ml_base_rle, ml_nb_rle as u32),
            None => ((ml_entry >> 32) as u32, ((ml_entry >> 24) & 0xFF) as u32),
        };
        let (of_base, of_add_bits) = match of_rle {
            Some(_) => (of_base_rle, of_nb_rle as u32),
            None => ((of_entry >> 32) as u32, ((of_entry >> 24) & 0xFF) as u32),
        };

        let obits = read(&mut win, &mut consumed, of_add_bits);
        let ml_add = read(&mut win, &mut consumed, ml_add_bits);
        if ll_add_bits + ml_add_bits + of_add_bits >= 31 {
            if matches!(
                reload(br, src_ptr, &mut ip, &mut bits, &mut win, &mut consumed),
                Reload::Overflow
            ) {
                return Err(DecodeSequenceError::NotEnoughBytesForNumSequences);
            }
        }
        let ll_add = read(&mut win, &mut consumed, ll_add_bits);

        unsafe {
            out.add(idx).write(Sequence {
                ll: ll_base + ll_add as u32,
                ml: ml_base + ml_add as u32,
                of: of_base + obits as u32,
            });
        }

        if idx + 1 < nseq {
            if ll_rle.is_none() {
                let nb = (ll_entry & 0xFF) as u32;
                let state =
                    (((ll_entry >> 8) & 0xFFFF) as u32) + read(&mut win, &mut consumed, nb) as u32;
                ll_entry = unsafe { *tbl.add(LL_SLOT + state as usize) };
            }
            if ml_rle.is_none() {
                let nb = (ml_entry & 0xFF) as u32;
                let state =
                    (((ml_entry >> 8) & 0xFFFF) as u32) + read(&mut win, &mut consumed, nb) as u32;
                ml_entry = unsafe { *tbl.add(ML_SLOT + state as usize) };
            }
            if of_rle.is_none() {
                let nb = (of_entry & 0xFF) as u32;
                let state =
                    (((of_entry >> 8) & 0xFFFF) as u32) + read(&mut win, &mut consumed, nb) as u32;
                of_entry = unsafe { *tbl.add(OF_SLOT + state as usize) };
            }
            if matches!(
                reload(br, src_ptr, &mut ip, &mut bits, &mut win, &mut consumed),
                Reload::Overflow
            ) {
                return Err(DecodeSequenceError::NotEnoughBytesForNumSequences);
            }
        }
    }
    // SAFETY: nseq sequences were written above
    unsafe { target.set_len(nseq) };

    br.ip = ip;
    br.bits = bits;
    br.consumed = consumed;
    let rem = br.remaining();
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
        }
        ModeType::Predefined => {
            vprintln!("Use predefined of table");
            if !scratch.of_predefined {
                scratch.offsets.build_from_probabilities(
                    OF_DEFAULT_ACC_LOG,
                    &OFFSET_DEFAULT_DISTRIBUTION,
                )?;
                scratch.of_predefined = true;
                scratch.of_seq_valid = false;
            }
            scratch.of_rle = None;
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
