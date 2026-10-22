//! Handcrafted zstd frames for decode-parity differential replay (the
//! R64/R65 mutation classes; docs/src/dev/review/parity.md). Shared by the
//! in-crate parity tests (`tests/parity.rs`) and the `zstdx-bench parity`
//! differential so the pinned shapes and the replayed corpus cannot drift
//! apart. Not part of the public API: compiled for tests and dev tooling
//! (`fuzz_exports`) only.

use alloc::{vec, vec::Vec};

use crate::common::MAGIC_NUM;

/// Wire block types (RFC 8878 3.1.1.3).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BlockWireType {
    Raw,
    Rle,
    Compressed,
    Reserved,
}

impl BlockWireType {
    const fn code(self) -> u32 {
        match self {
            BlockWireType::Raw => 0,
            BlockWireType::Rle => 1,
            BlockWireType::Compressed => 2,
            BlockWireType::Reserved => 3,
        }
    }
}

/// Append a block header plus body to `out`.
pub fn push_block(out: &mut Vec<u8>, last: bool, btype: BlockWireType, content: &[u8]) {
    let hdr = ((content.len() as u32) << 3) | (btype.code() << 1) | u32::from(last);
    out.extend_from_slice(&hdr.to_le_bytes()[..3]);
    out.extend_from_slice(content);
}

/// Frame_Content_Size field forms (FHD bits 7-6; the 2-byte form carries
/// value + 256 on the wire).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FcsForm {
    /// 1 byte; only legal with the single-segment flag.
    S1,
    /// 2 bytes, +256 offset.
    S2,
    S4,
    S8,
}

/// The header pieces the crafted shapes vary.
#[derive(Debug, Clone, Default)]
#[allow(clippy::struct_excessive_bools)] // the flag set IS the vocabulary
pub struct HeaderSpec {
    /// Declared Frame_Content_Size and its field form.
    pub fcs: Option<(FcsForm, u64)>,
    /// SingleSegment_flag: no window descriptor byte, window = FCS.
    pub single_segment: bool,
    /// Window_Descriptor byte (ignored when `single_segment`).
    pub window_descriptor: u8,
    /// Dictionary_ID field bytes (1, 2 or 4 long; the low FHD bits follow
    /// the length).
    pub dict_id: Option<Vec<u8>>,
    /// FHD bit 4: unused, "shall not be interpreted".
    pub unused_bit: bool,
    /// FHD bit 3: reserved, must be zero.
    pub reserved_bit: bool,
    /// Append a Frame_Content_Checksum field of zeros after the last
    /// block (always wrong for nonempty content: an instant reject class).
    pub zero_checksum: bool,
}

impl HeaderSpec {
    /// No FCS, no dict id, 1 KiB window, no extra bits.
    pub const fn plain() -> Self {
        HeaderSpec {
            fcs: None,
            single_segment: false,
            window_descriptor: 0,
            dict_id: None,
            unused_bit: false,
            reserved_bit: false,
            zero_checksum: false,
        }
    }

    fn dict_id_code(&self) -> u8 {
        match self.dict_id.as_deref().map(<[u8]>::len) {
            None => 0,
            Some(1) => 1,
            Some(2) => 2,
            Some(4) => 3,
            // Any other length is a construction bug: fail loudly at the
            // caller instead of silently writing a wrong field.
            Some(_) => panic!("dict id field must be 1, 2 or 4 bytes"),
        }
    }

    fn header(&self) -> Vec<u8> {
        let fcs_flag = match &self.fcs {
            None | Some((FcsForm::S1, _)) => 0,
            Some((FcsForm::S2, _)) => 1,
            Some((FcsForm::S4, _)) => 2,
            Some((FcsForm::S8, _)) => 3,
        };
        let mut fhd = (fcs_flag << 6)
            | (u8::from(self.single_segment) << 5)
            | (u8::from(self.unused_bit) << 4)
            | (u8::from(self.reserved_bit) << 3)
            | self.dict_id_code();
        if self.zero_checksum {
            fhd |= 1 << 2;
        }
        let mut out = Vec::new();
        out.extend_from_slice(&MAGIC_NUM.to_le_bytes());
        out.push(fhd);
        if let Some(dict_id) = &self.dict_id {
            out.extend_from_slice(dict_id);
        }
        if !self.single_segment {
            out.push(self.window_descriptor);
        }
        if let Some((form, size)) = &self.fcs {
            // The 2-byte form stores size - 256.
            let wire = match form {
                FcsForm::S2 => size - 256,
                FcsForm::S1 | FcsForm::S4 | FcsForm::S8 => *size,
            };
            let len = match form {
                FcsForm::S1 => 1,
                FcsForm::S2 => 2,
                FcsForm::S4 => 4,
                FcsForm::S8 => 8,
            };
            out.extend_from_slice(&wire.to_le_bytes()[..len]);
        }
        out
    }
}

/// A complete frame: header per `spec`, one last Raw block carrying
/// `payload` (empty payload = empty last block).
pub fn raw_frame(spec: &HeaderSpec, payload: &[u8]) -> Vec<u8> {
    let mut out = spec.header();
    push_block(&mut out, true, BlockWireType::Raw, payload);
    if spec.zero_checksum {
        out.extend_from_slice(&[0; 4]);
    }
    out
}

/// Header only: no blocks (a mid-frame truncation shape).
pub fn header_only(spec: &HeaderSpec) -> Vec<u8> {
    spec.header()
}

/// A minimal valid frame with one Compressed block: Raw literals "ababababab"
/// (header 0x50 = 10 << 3), one sequence with all three FSE streams in RLE
/// mode (LL code 9 → ll 9, OF code 0 → rep0 = offset 1, ML code 2 → ml 5)
/// and a bare end-marker bitstream (every read is zero-width). Output:
/// "ababababa" + "aaaaa" (offset-1 match off the just-written 'a') + the
/// trailing literal 'b' = 15 bytes.
pub fn one_sequence_frame(modes_byte: u8) -> Vec<u8> {
    seq_frame(&SeqSpec::one_sequence(modes_byte))
}

/// The pieces of a one-block sequence section.
#[derive(Debug, Clone)]
pub struct SeqSpec {
    /// Raw literals carried by the block (kept < 31 so the 1-byte
    /// literals header suffices).
    pub literals: Vec<u8>,
    /// nbSeq varint bytes (1/2/4-byte forms).
    pub nb_seq: Vec<u8>,
    /// Symbol_Compression_Modes byte.
    pub modes: u8,
    /// RLE table bytes for each mode set to RLE, in LL/OF/ML order.
    pub rle_tables: Vec<u8>,
    /// Sequence bitstream (backward-read; the last byte carries the end
    /// marker).
    pub bitstream: Vec<u8>,
}

impl SeqSpec {
    /// The valid RLE-only one-sequence shape `one_sequence_frame` builds.
    pub fn one_sequence(modes_byte: u8) -> Self {
        SeqSpec {
            literals: b"ababababab".to_vec(),
            nb_seq: vec![0x01],
            modes: modes_byte,
            rle_tables: vec![9, 0, 2],
            bitstream: vec![0x01],
        }
    }

    fn block(&self) -> Vec<u8> {
        let mut block = Vec::new();
        debug_assert!(self.literals.len() < 31, "1-byte raw literals header only");
        block.push((self.literals.len() as u8) << 3);
        block.extend_from_slice(&self.literals);
        block.extend_from_slice(&self.nb_seq);
        block.push(self.modes);
        block.extend_from_slice(&self.rle_tables);
        block.extend_from_slice(&self.bitstream);
        block
    }
}

/// A complete frame carrying one last Compressed block per `spec`, header
/// flags `plain` (1 KiB window, nothing declared).
pub fn seq_frame(spec: &SeqSpec) -> Vec<u8> {
    let mut out = HeaderSpec::plain().header();
    push_block(&mut out, true, BlockWireType::Compressed, &spec.block());
    out
}

/// A one-block frame whose literals section is RLE: `byte` repeated
/// `size` times (header type 1), followed by `rest` (nbSeq + modes +
/// tables + bitstream).
pub fn rle_literals_frame(byte: u8, size: u8, rest: &[u8]) -> Vec<u8> {
    debug_assert!(size < 31, "1-byte literals header only");
    let mut block = Vec::new();
    block.push(1 | (size << 3));
    block.push(byte);
    block.extend_from_slice(rest);
    let mut out = HeaderSpec::plain().header();
    push_block(&mut out, true, BlockWireType::Compressed, &block);
    out
}

/// The sequence-section tail of [`SeqSpec::one_sequence`] (nbSeq through
/// bitstream), for frames that swap the literals section.
fn one_sequence_tail(modes_byte: u8) -> Vec<u8> {
    let spec = SeqSpec::one_sequence(modes_byte);
    let mut tail = spec.nb_seq;
    tail.push(spec.modes);
    tail.extend_from_slice(&spec.rle_tables);
    tail.extend_from_slice(&spec.bitstream);
    tail
}

/// A raw (nibble-packed) Huffman weight description for a uniform
/// tableLog-2 table over symbols 0..=3 (explicit weights [1, 1, 1], the
/// last implied): every symbol has a 2-bit code equal to its value.
pub fn uniform2_raw_weights() -> Vec<u8> {
    vec![130, 0x11, 0x10]
}

/// A backward Huffman bitstream for `codes` (2 bits each), zero-padded to
/// `size` bytes: the end marker sits at the top of the last byte and the
/// code bits follow it MSB-first. The zero bytes below the last code are
/// never read by an exact decoder but ARE by the reference's fast tail.
pub fn huff2_stream(codes: &[u8], size: usize) -> Vec<u8> {
    let mut out = vec![0u8; size];
    let mut bit = size * 8 - 1;
    out[bit / 8] |= 1 << (bit % 8);
    for code in codes {
        for k in (0..2).rev() {
            bit -= 1;
            if code >> k & 1 == 1 {
                out[bit / 8] |= 1 << (bit % 8);
            }
        }
    }
    out
}

/// A tight backward stream for `codes`: marker + payload consume every
/// bit of the 3-byte buffer (the accept twin of
/// [`huff2_leftover_stream`]).
pub fn huff2_tight_stream(codes: &[u8]) -> Vec<u8> {
    let mut out = vec![0u8; 3];
    let mut bit = 20; // 1 marker bit + 10 two-bit codes
    out[bit / 8] |= 1 << (bit % 8);
    for code in codes {
        for k in (0..2).rev() {
            bit -= 1;
            if code >> k & 1 == 1 {
                out[bit / 8] |= 1 << (bit % 8);
            }
        }
    }
    debug_assert_eq!(bit, 0);
    out
}

/// `codes` with the marker at the top of a 4-byte buffer: 11 zero bits
/// remain below the payload (a leftover-bits single stream: rejected by
/// the exact `BIT_endOfDStream` check both decoders apply there).
pub fn huff2_leftover_stream(codes: &[u8]) -> Vec<u8> {
    let mut out = vec![0u8; 4];
    let mut bit = 31;
    out[bit / 8] |= 1 << (bit % 8);
    for code in codes {
        for k in (0..2).rev() {
            bit -= 1;
            if code >> k & 1 == 1 {
                out[bit / 8] |= 1 << (bit % 8);
            }
        }
    }
    out
}

/// A literals section header (type Compressed, 10-bit sizes) + payload +
/// nbSeq = 0. `four_streams` selects the size_format bit (1: four streams
/// with a jump table, 0: one stream).
fn literals_block(four_streams: bool, regenerated: u16, payload: &[u8]) -> Vec<u8> {
    let compressed = payload.len() as u16;
    let b0 = 0b10 | (u8::from(four_streams) << 2) | ((regenerated & 0xf) as u8) << 4;
    let b1 = ((regenerated >> 4) as u8 & 0x3f) | ((compressed & 3) as u8) << 6;
    let b2 = (compressed >> 2) as u8;
    let mut out = vec![b0, b1, b2];
    out.extend_from_slice(payload);
    out.push(0x00); // nbSeq = 0
    out
}

/// The R66 four-stream literals tail class: stream 1 carries only 4 of
/// its 10 codes, so the tail must keep decoding (zero-padded / foreign
/// bits) until its output segment is full. The reference's fast decode
/// (streams >= 8 bytes) validates by output length, NOT by exact bit
/// consumption — the frame is ACCEPTED with the zero-fill visible in the
/// output. A strict `BIT_endOfDStream` tail rejects it (the fallback-body
/// semantics; the class that found zstdx rejecting what libzstd accepts).
pub fn four_stream_underflow_frame() -> Vec<u8> {
    const CODES: [u8; 10] = [1, 2, 3, 1, 1, 2, 3, 1, 2, 2];
    let mut payload = uniform2_raw_weights();
    for len in [8u16, 8, 8] {
        payload.extend_from_slice(&len.to_le_bytes());
    }
    payload.extend_from_slice(&huff2_stream(&CODES, 8));
    payload.extend_from_slice(&huff2_stream(&CODES[..4], 8));
    payload.extend_from_slice(&huff2_stream(&CODES, 8));
    payload.extend_from_slice(&huff2_stream(&CODES, 8));
    let mut frame = HeaderSpec::plain().header();
    push_block(
        &mut frame,
        true,
        BlockWireType::Compressed,
        &literals_block(true, 40, &payload),
    );
    frame
}

/// The R66 single-stream leftovers class: a tight stream (every bit
/// consumed) is accepted; the same codes with 11 leftover zero bits are
/// rejected by the exact-consumption gate (`BIT_endOfDStream`) both
/// decoders apply on the single-stream path.
pub fn single_stream_frame(leftover: bool) -> Vec<u8> {
    const CODES: [u8; 10] = [1, 2, 3, 1, 1, 2, 3, 1, 2, 2];
    let stream = if leftover {
        huff2_leftover_stream(&CODES)
    } else {
        huff2_tight_stream(&CODES)
    };
    let mut payload = uniform2_raw_weights();
    payload.extend_from_slice(&stream);
    let mut frame = HeaderSpec::plain().header();
    push_block(
        &mut frame,
        true,
        BlockWireType::Compressed,
        &literals_block(false, 10, &payload),
    );
    frame
}

/// Skippable-frame shapes (magic 0x184D2A50..=5F + 4-byte length + payload).
#[derive(Debug, Clone, Copy)]
pub enum SkippableSpec {
    /// Complete frame with `length` zero bytes of payload.
    Complete { length: u32 },
    /// Zero-length payload: valid, contributes nothing.
    ZeroLength,
    /// Declares more payload than follows.
    Truncated { declared: u32, present: u32 },
}

pub fn skippable_frame(spec: SkippableSpec) -> Vec<u8> {
    let (declared, present) = match spec {
        SkippableSpec::Complete { length } => (length, length),
        SkippableSpec::ZeroLength => (0, 0),
        SkippableSpec::Truncated { declared, present } => (declared, present),
    };
    let mut out = Vec::new();
    out.extend_from_slice(&0x184d2a50u32.to_le_bytes());
    out.extend_from_slice(&declared.to_le_bytes());
    out.resize(out.len() + present as usize, 0);
    out
}

/// `frame` with trailing bytes that belong to no frame (a next-frame
/// header parse error for both decoders).
pub fn with_trailing(frame: &[u8], extra: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(frame.len() + extra.len());
    out.extend_from_slice(frame);
    out.extend_from_slice(extra);
    out
}

/// Names of the crafted corpus shapes (reporting vocabulary of the
/// differential; the constructors below are the single source).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CraftedName {
    OneSequence,
    ModeReservedBits,
    ModeRleOverCapLl,
    ModeRleOverCapOf,
    ModeRleOverCapMl,
    RepeatModeNoTable,
    OffsetBeyondOutput,
    LlOverLiterals,
    NbSeqTwoByteZero,
    NbSeqTwoByteZeroTrailing,
    NbSeqZero,
    StreamUnderflow,
    LeftoverBits,
    RleLiterals,
    EmptySingleSegment,
    Fcs2Form,
    Fcs2FormMismatch,
    Fcs4Form,
    Fcs8Form,
    SingleSegmentFcs8,
    WindowAtDefaultLimit,
    WindowOverDefaultLimit,
    WindowCeilingProbe,
    DictIdMissing,
    ReservedDescriptorBit,
    UnusedDescriptorBit,
    ZeroChecksumField,
    TrailingGarbage,
    Skippable,
    SkippableZero,
    SkippableTruncated,
    ReservedBlockType,
    MultiFrame,
    FourStreamUnderflow,
    SingleStreamTight,
    SingleStreamLeftover,
}

impl CraftedName {
    pub const fn tag(self) -> &'static str {
        match self {
            CraftedName::OneSequence => "seq-one",
            CraftedName::ModeReservedBits => "modes-reserved-bits",
            CraftedName::ModeRleOverCapLl => "modes-rle-over-cap-ll",
            CraftedName::ModeRleOverCapOf => "modes-rle-over-cap-of",
            CraftedName::ModeRleOverCapMl => "modes-rle-over-cap-ml",
            CraftedName::RepeatModeNoTable => "modes-repeat-no-table",
            CraftedName::OffsetBeyondOutput => "seq-offset-beyond-output",
            CraftedName::LlOverLiterals => "seq-ll-over-literals",
            CraftedName::NbSeqTwoByteZero => "nbseq-2b-zero",
            CraftedName::NbSeqTwoByteZeroTrailing => "nbseq-2b-zero-trailing",
            CraftedName::NbSeqZero => "nbseq-zero",
            CraftedName::StreamUnderflow => "seq-stream-underflow",
            CraftedName::LeftoverBits => "seq-leftover-bytes",
            CraftedName::RleLiterals => "rle-literals",
            CraftedName::EmptySingleSegment => "empty-single-segment",
            CraftedName::Fcs2Form => "fcs2-form",
            CraftedName::Fcs2FormMismatch => "fcs2-form-mismatch",
            CraftedName::Fcs4Form => "fcs4-form",
            CraftedName::Fcs8Form => "fcs8-form",
            CraftedName::SingleSegmentFcs8 => "single-segment-fcs8",
            CraftedName::WindowAtDefaultLimit => "window-at-limit",
            CraftedName::WindowOverDefaultLimit => "window-over-limit",
            CraftedName::WindowCeilingProbe => "window-ceiling-probe",
            CraftedName::DictIdMissing => "dictid-missing",
            CraftedName::ReservedDescriptorBit => "fhd-reserved-bit",
            CraftedName::UnusedDescriptorBit => "fhd-unused-bit",
            CraftedName::ZeroChecksumField => "zero-checksum",
            CraftedName::TrailingGarbage => "trailing-garbage",
            CraftedName::Skippable => "skippable",
            CraftedName::SkippableZero => "skippable-zero",
            CraftedName::SkippableTruncated => "skippable-truncated",
            CraftedName::ReservedBlockType => "reserved-block-type",
            CraftedName::MultiFrame => "multi-frame",
            CraftedName::FourStreamUnderflow => "literals-4stream-underflow",
            CraftedName::SingleStreamTight => "literals-1stream-tight",
            CraftedName::SingleStreamLeftover => "literals-1stream-leftover",
        }
    }
}

/// Every crafted shape, in a fixed order. Each entry is one differential
/// case on its own and mutation substrate for the sweep.
pub fn crafted_frames() -> Vec<(CraftedName, Vec<u8>)> {
    let mut out: Vec<(CraftedName, Vec<u8>)> = Vec::new();
    let mut push = |name, frame| out.push((name, frame));

    // --- sequence-section shapes (1 KiB window, one last Compressed block)
    push(CraftedName::OneSequence, one_sequence_frame(0b01_01_01_00));
    // Reserved bits set: libzstd rejects (R65 finding 1).
    push(
        CraftedName::ModeReservedBits,
        one_sequence_frame(0b01_01_01_11),
    );
    // RLE symbol past each alphabet cap (LL 35 / OF 31 / ML 52).
    for (name, table) in [
        (CraftedName::ModeRleOverCapLl, vec![36u8, 0, 2]),
        (CraftedName::ModeRleOverCapOf, vec![9u8, 32, 2]),
        (CraftedName::ModeRleOverCapMl, vec![9u8, 0, 53]),
    ] {
        let mut spec = SeqSpec::one_sequence(0b01_01_01_00);
        spec.rle_tables = table;
        push(name, seq_frame(&spec));
    }
    // Repeat mode with no prior table in the frame.
    push(
        CraftedName::RepeatModeNoTable,
        one_sequence_frame(0b11_01_01_00),
    );
    // Offset code 12: offset_value 4096 -> offset 4093, past the 1 KiB
    // window and the 15-byte output. The bitstream carries exactly the 12
    // extra bits the code demands (marker in the final byte's bit 4).
    {
        let mut spec = SeqSpec::one_sequence(0b01_01_01_00);
        spec.rle_tables = vec![9, 12, 2];
        spec.bitstream = vec![0x00, 0x10];
        push(CraftedName::OffsetBeyondOutput, seq_frame(&spec));
    }
    // ll 9 over 3 literals.
    {
        let mut spec = SeqSpec::one_sequence(0b01_01_01_00);
        spec.literals = b"abc".to_vec();
        push(CraftedName::LlOverLiterals, seq_frame(&spec));
    }
    // nbSeq = 0 in the two-byte form (0x80 0x00): valid alone, rejected
    // with a trailing byte (R65 class).
    {
        let mut spec = SeqSpec::one_sequence(0b01_01_01_00);
        spec.nb_seq = vec![0x80, 0x00];
        spec.rle_tables.clear();
        spec.bitstream.clear();
        push(CraftedName::NbSeqTwoByteZero, seq_frame(&spec));
        spec.nb_seq.push(0x00);
        push(CraftedName::NbSeqTwoByteZeroTrailing, seq_frame(&spec));
    }
    // nbSeq = 0 single-byte form: literals-only compressed block, valid.
    {
        let mut spec = SeqSpec::one_sequence(0b01_01_01_00);
        spec.nb_seq = vec![0x00];
        spec.rle_tables.clear();
        spec.bitstream.clear();
        push(CraftedName::NbSeqZero, seq_frame(&spec));
    }
    // 127 sequences against a bare end marker: stream underflow.
    {
        let mut spec = SeqSpec::one_sequence(0b01_01_01_00);
        spec.nb_seq = vec![127];
        push(CraftedName::StreamUnderflow, seq_frame(&spec));
    }
    // One extra byte after the bitstream inside the block.
    {
        let mut spec = SeqSpec::one_sequence(0b01_01_01_00);
        spec.bitstream.push(0xff);
        push(CraftedName::LeftoverBits, seq_frame(&spec));
    }
    // RLE literals (one 'a' standing for 10) feeding the same sequence.
    push(
        CraftedName::RleLiterals,
        rle_literals_frame(b'a', 10, &one_sequence_tail(0b01_01_01_00)),
    );

    // --- frame-header shapes
    push(
        CraftedName::EmptySingleSegment,
        raw_frame(
            &HeaderSpec {
                single_segment: true,
                fcs: Some((FcsForm::S1, 0)),
                ..HeaderSpec::plain()
            },
            b"",
        ),
    );
    // FCS-2 with the +256 offset: declared 256, payload 256 -> accept;
    // and the mismatch twin (payload 255 -> reject).
    push(
        CraftedName::Fcs2Form,
        raw_frame(
            &HeaderSpec {
                fcs: Some((FcsForm::S2, 256)),
                ..HeaderSpec::plain()
            },
            &[b'x'; 256],
        ),
    );
    push(
        CraftedName::Fcs2FormMismatch,
        raw_frame(
            &HeaderSpec {
                fcs: Some((FcsForm::S2, 256)),
                ..HeaderSpec::plain()
            },
            &[b'x'; 255],
        ),
    );
    push(
        CraftedName::Fcs4Form,
        raw_frame(
            &HeaderSpec {
                fcs: Some((FcsForm::S4, 12)),
                ..HeaderSpec::plain()
            },
            &[b'y'; 12],
        ),
    );
    push(
        CraftedName::Fcs8Form,
        raw_frame(
            &HeaderSpec {
                fcs: Some((FcsForm::S8, 300)),
                ..HeaderSpec::plain()
            },
            &[b'z'; 300],
        ),
    );
    push(
        CraftedName::SingleSegmentFcs8,
        raw_frame(
            &HeaderSpec {
                single_segment: true,
                fcs: Some((FcsForm::S8, 15)),
                ..HeaderSpec::plain()
            },
            b"single-segment!",
        ),
    );
    // Window descriptor exponent 17 = 128 MiB: exactly the default limit
    // (accepted); mantissa 1 = +16 MiB (rejected). Small payloads: the
    // verdict rides on the window check, not on truncation.
    push(
        CraftedName::WindowAtDefaultLimit,
        raw_frame(
            &HeaderSpec {
                window_descriptor: 17 << 3,
                ..HeaderSpec::plain()
            },
            b"w",
        ),
    );
    push(
        CraftedName::WindowOverDefaultLimit,
        raw_frame(
            &HeaderSpec {
                window_descriptor: (17 << 3) | 1,
                ..HeaderSpec::plain()
            },
            b"w",
        ),
    );
    // Header-only single-segment FCS = 2^27+1: the one window value at
    // the default limit (pinned test `default_window_limit_is_exactly_
    // libzstds`). Rejected for the missing blocks, never for the window.
    push(
        CraftedName::WindowCeilingProbe,
        header_only(&HeaderSpec {
            single_segment: true,
            fcs: Some((FcsForm::S8, (1 << 27) + 1)),
            ..HeaderSpec::plain()
        }),
    );
    // Dictionary_ID declared (4 bytes), no dictionary attached.
    push(
        CraftedName::DictIdMissing,
        raw_frame(
            &HeaderSpec {
                dict_id: Some(0x1234_5678u32.to_le_bytes().to_vec()),
                ..HeaderSpec::plain()
            },
            b"d",
        ),
    );
    push(
        CraftedName::ReservedDescriptorBit,
        raw_frame(
            &HeaderSpec {
                reserved_bit: true,
                ..HeaderSpec::plain()
            },
            b"r",
        ),
    );
    push(
        CraftedName::UnusedDescriptorBit,
        raw_frame(
            &HeaderSpec {
                unused_bit: true,
                ..HeaderSpec::plain()
            },
            b"u",
        ),
    );
    push(
        CraftedName::ZeroChecksumField,
        raw_frame(
            &HeaderSpec {
                zero_checksum: true,
                ..HeaderSpec::plain()
            },
            b"c",
        ),
    );
    push(
        CraftedName::TrailingGarbage,
        with_trailing(&one_sequence_frame(0b01_01_01_00), &[0x00, 0x01, 0x02]),
    );
    push(
        CraftedName::Skippable,
        skippable_frame(SkippableSpec::Complete { length: 16 }),
    );
    push(
        CraftedName::SkippableZero,
        skippable_frame(SkippableSpec::ZeroLength),
    );
    push(
        CraftedName::SkippableTruncated,
        skippable_frame(SkippableSpec::Truncated {
            declared: 32,
            present: 8,
        }),
    );
    // Reserved block type (3): rejected outright.
    {
        let mut frame = HeaderSpec::plain().header();
        push_block(&mut frame, true, BlockWireType::Reserved, b"");
        push(CraftedName::ReservedBlockType, frame);
    }
    // Two valid frames back to back.
    {
        let one = one_sequence_frame(0b01_01_01_00);
        push(CraftedName::MultiFrame, with_trailing(&one, &one));
    }

    // --- R66 huffman literals tail classes
    push(
        CraftedName::FourStreamUnderflow,
        four_stream_underflow_frame(),
    );
    push(CraftedName::SingleStreamTight, single_stream_frame(false));
    push(CraftedName::SingleStreamLeftover, single_stream_frame(true));

    out
}
