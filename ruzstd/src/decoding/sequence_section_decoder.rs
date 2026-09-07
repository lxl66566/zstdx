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

    if scratch.ll_rle.is_some() || scratch.ml_rle.is_some() || scratch.of_rle.is_some() {
        decode_sequences_with_rle(section, &mut br, scratch, target)
    } else {
        decode_sequences_without_rle(section, &mut br, scratch, target)
    }
}

/// Backwards bitstream reader over a 64-bit container, ported from libzstd's
/// BIT_DStream. The window position is tracked by a byte pointer instead of
/// recounting consumed bits, and reads are served by two shifts on the
/// container. Reads must respect the reload discipline: after a reload at most
/// 57 bits are available, which bounds how far reads may run between reloads.
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

    /// Next `n` bits without consuming (n <= 31). The `& 63` mirrors
    /// libzstd's masking: defined (garbage) values for overconsumed streams.
    #[inline(always)]
    fn peek(&self, n: u32) -> u64 {
        if n == 0 {
            return 0;
        }
        (self.bits << (self.consumed & 63)) >> (64 - n)
    }

    #[inline(always)]
    fn read(&mut self, n: u32) -> u64 {
        let v = self.peek(n);
        self.consumed += n;
        v
    }

    /// Exact number of unread bits; negative when overconsumed.
    fn remaining(&self) -> isize {
        self.ip as isize * 8 + 64 - self.consumed as isize
    }

    /// Move the window backwards by the consumed whole bytes. Mirrors
    /// BIT_reloadDStream: fast path when at least 8 bytes remain below the
    /// window, cautious clamp near the stream start (kept out of line so the
    /// fast path can inline and the reader fields stay in registers).
    #[inline(always)]
    fn reload(&mut self) -> Reload {
        if self.consumed > 64 {
            // Stream overconsumed; feeding zeroes would move ip below the
            // stream start, so freeze the window instead
            return Reload::Overflow;
        }
        if self.ip >= 8 {
            // SAFETY: ip only ever decreases from its initial value len-8, so
            // ip+8 <= source.len() holds on this path
            let p = self.source.as_ptr();
            self.ip -= (self.consumed >> 3) as usize;
            self.consumed &= 7;
            self.bits = unsafe { p.add(self.ip).cast::<u64>().read_unaligned() };
            return Reload::Unfinished;
        }
        self.reload_slow()
    }

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

/// Pack a sequence decoding table: one u64 per FSE state carrying everything
/// the decode loop needs for a symbol, so a single load serves a stream.
/// Layout: `[base_value: u32][add_bits: u8][next_state_base: u16][nb_bits: u8]`.
fn pack_seq_table(table: &FSETable, codes: &[(u32, u8)], out: &mut Vec<u64>) {
    out.clear();
    out.reserve(table.decode.len());
    for e in &table.decode {
        let sym = e.symbol as usize;
        let (base, add) = codes[sym];
        out.push(
            ((base as u64) << 32)
                | ((add as u64) << 24)
                | ((e.base_line as u64) << 8)
                | e.num_bits as u64,
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

/// A single FSE stream over the shared bit reader: the packed table plus the
/// current state index into it. The table is a raw pointer instead of a slice
/// so the decode loop carries one register per stream instead of two.
struct FseStream {
    table: *const u64,
    state: u32,
    /// Entry for `state`, cached so the loop loads each table slot exactly
    /// once per sequence (the transition needs the same fields as the output).
    entry: u64,
}

impl FseStream {
    /// Load the entry for the current state. RLE streams leave `table`
    /// dangling and must not call this.
    #[inline(always)]
    unsafe fn load_entry(&mut self) {
        debug_assert!((self.state as usize) < 1 << 9);
        // SAFETY: FSE table construction guarantees every reachable state
        // (next_state_base + the value read with nb_bits) stays below the
        // table size, independent of the bitstream content
        self.entry = unsafe { *self.table.add(self.state as usize) };
    }

    /// Advance the state with the entry's transition bits and cache the next
    /// entry. `e` must be the cached entry of the current state.
    #[inline(always)]
    fn transition(&mut self, e: u64, br: &mut SeqBitReader<'_>) {
        self.state = (((e >> 8) & 0xFFFF) as u32) + br.read((e & 0xFF) as u32) as u32;
        // SAFETY: same invariant as load_entry
        unsafe { self.load_entry() };
    }
}

/// Rebuild the packed tables whose source tables changed since the last block.
fn pack_tables(scratch: &mut FSEScratch) {
    if !scratch.ll_seq_valid {
        pack_seq_table(&scratch.literal_lengths, &LL_CODES, &mut scratch.ll_seq);
        scratch.ll_seq_valid = true;
    }
    if !scratch.ml_seq_valid {
        pack_seq_table(&scratch.match_lengths, &ML_CODES, &mut scratch.ml_seq);
        scratch.ml_seq_valid = true;
    }
    if !scratch.of_seq_valid {
        pack_seq_table(&scratch.offsets, &OF_CODES, &mut scratch.of_seq);
        scratch.of_seq_valid = true;
    }
}

fn decode_sequences_without_rle(
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

    // Initial states are read in the order ll, of, ml
    let mut ll = FseStream {
        table: scratch.ll_seq.as_ptr(),
        state: br.read(ll_acc as u32) as u32,
        entry: 0,
    };
    let mut of = FseStream {
        table: scratch.of_seq.as_ptr(),
        state: br.read(of_acc as u32) as u32,
        entry: 0,
    };
    let mut ml = FseStream {
        table: scratch.ml_seq.as_ptr(),
        state: br.read(ml_acc as u32) as u32,
        entry: 0,
    };
    // Realign the window before the first sequence; the loop only reloads at
    // sequence ends (the state reads above already consumed up to 26 bits)
    br.reload();
    // SAFETY: states come straight from accuracy-log reads, so they are
    // below the table sizes
    unsafe {
        ll.load_entry();
        of.load_entry();
        ml.load_entry();
    }

    target.clear();
    target.reserve(section.num_sequences as usize);
    let nseq = section.num_sequences as usize;
    // SAFETY: nseq slots were just reserved; each iteration writes exactly
    // one Sequence and the length is only exposed on success
    let out = target.as_mut_ptr();

    for idx in 0..nseq {
        let e_ll = ll.entry;
        let e_ml = ml.entry;
        let e_of = of.entry;
        let ll_add_bits = ((e_ll >> 24) & 0xFF) as u32;
        let ml_add_bits = ((e_ml >> 24) & 0xFF) as u32;
        let of_add_bits = ((e_of >> 24) & 0xFF) as u32;

        let obits = br.read(of_add_bits);
        let ml_add = br.read(ml_add_bits);
        // of+ml+ll add bits can sum above the 57 bits guaranteed after a
        // reload; reload mid-sequence then (libzstd's totalBits guard)
        if ll_add_bits + ml_add_bits + of_add_bits >= 31 {
            br.reload();
        }
        let ll_add = br.read(ll_add_bits);

        unsafe {
            out.add(idx).write(Sequence {
                ll: ((e_ll >> 32) as u32) + ll_add as u32,
                ml: ((e_ml >> 32) as u32) + ml_add as u32,
                of: ((e_of >> 32) as u32) + obits as u32,
            });
        }

        if idx + 1 < nseq {
            ll.transition(e_ll, br);
            ml.transition(e_ml, br);
            of.transition(e_of, br);
            if matches!(br.reload(), Reload::Overflow) {
                return Err(DecodeSequenceError::NotEnoughBytesForNumSequences);
            }
        }
    }
    // SAFETY: nseq sequences were written above
    unsafe { target.set_len(nseq) };

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
    pack_tables(scratch);

    let ll_rle = scratch.ll_rle;
    let ml_rle = scratch.ml_rle;
    let of_rle = scratch.of_rle;

    // Initial states are read in the order ll, of, ml (only for FSE streams)
    let uninit = || {
        Err(DecodeSequenceError::FSEDecoderError(
            crate::decoding::errors::FSEDecoderError::TableIsUninitialized,
        ))
    };
    let ll_state = match ll_rle {
        None => br.read(scratch.literal_lengths.accuracy_log as u32) as u32,
        Some(_) => 0,
    };
    let of_state = match of_rle {
        None => {
            if scratch.offsets.accuracy_log == 0 {
                return uninit();
            }
            br.read(scratch.offsets.accuracy_log as u32) as u32
        }
        Some(_) => 0,
    };
    let ml_state = match ml_rle {
        None => {
            if scratch.match_lengths.accuracy_log == 0 {
                return uninit();
            }
            br.read(scratch.match_lengths.accuracy_log as u32) as u32
        }
        Some(_) => 0,
    };
    if ll_rle.is_none() && scratch.literal_lengths.accuracy_log == 0 {
        return uninit();
    }
    // Realign the window before the first sequence; the loop only reloads at
    // sequence ends (the state reads above may have consumed up to 26 bits)
    br.reload();

    let mut ll = FseStream {
        table: scratch.ll_seq.as_ptr(),
        state: ll_state,
        entry: 0,
    };
    let mut ml = FseStream {
        table: scratch.ml_seq.as_ptr(),
        state: ml_state,
        entry: 0,
    };
    let mut of = FseStream {
        table: scratch.of_seq.as_ptr(),
        state: of_state,
        entry: 0,
    };
    // SAFETY: states are either 0 (RLE, table untouched) or read straight
    // from accuracy logs, so they are below the table sizes
    unsafe {
        if ll_rle.is_none() {
            ll.load_entry();
        }
        if ml_rle.is_none() {
            ml.load_entry();
        }
        if of_rle.is_none() {
            of.load_entry();
        }
    }

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
            None => ((ll.entry >> 32) as u32, ((ll.entry >> 24) & 0xFF) as u32),
        };
        let (ml_base, ml_add_bits) = match ml_rle {
            Some(_) => (ml_base_rle, ml_nb_rle as u32),
            None => ((ml.entry >> 32) as u32, ((ml.entry >> 24) & 0xFF) as u32),
        };
        let (of_base, of_add_bits) = match of_rle {
            Some(_) => (of_base_rle, of_nb_rle as u32),
            None => ((of.entry >> 32) as u32, ((of.entry >> 24) & 0xFF) as u32),
        };

        let obits = br.read(of_add_bits);
        let ml_add = br.read(ml_add_bits);
        if ll_add_bits + ml_add_bits + of_add_bits >= 31 {
            br.reload();
        }
        let ll_add = br.read(ll_add_bits);

        unsafe {
            out.add(idx).write(Sequence {
                ll: ll_base + ll_add as u32,
                ml: ml_base + ml_add as u32,
                of: of_base + obits as u32,
            });
        }

        if idx + 1 < nseq {
            if ll_rle.is_none() {
                ll.transition(ll.entry, br);
            }
            if ml_rle.is_none() {
                ml.transition(ml.entry, br);
            }
            if of_rle.is_none() {
                of.transition(of.entry, br);
            }
            if matches!(br.reload(), Reload::Overflow) {
                return Err(DecodeSequenceError::NotEnoughBytesForNumSequences);
            }
        }
    }
    // SAFETY: nseq sequences were written above
    unsafe { target.set_len(nseq) };

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
