//! Errors that might occur while decoding zstd formatted data
//!
//! All enums derive their `Display`/`From`/`std::error::Error` impls with
//! thiserror; without the `std` feature the `std::error::Error` impls are
//! omitted (the derive's no_std mode) exactly like the hand-written cfg gates
//! they replaced.

use alloc::vec::Vec;

use crate::{
    bit_io::GetBitsError,
    blocks::{block::BlockType, literals_section::LiteralsSectionType},
    io::Error,
};

#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum FrameDescriptorError {
    #[error("Invalid Frame_Content_Size_Flag; Is: {got}, Should be one of: 0, 1, 2, 3")]
    InvalidFrameContentSizeFlag { got: u8 },
}

#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum FrameHeaderError {
    #[error(
        "window_size bigger than allowed maximum. Is: {got}, Should be lower than: {}",
        crate::common::MAX_WINDOW_SIZE
    )]
    WindowTooBig { got: u64 },
    #[error(
        "window_size smaller than allowed minimum. Is: {got}, Should be greater than: {}",
        crate::common::MIN_WINDOW_SIZE
    )]
    WindowTooSmall { got: u64 },
    #[error("{0:?}")]
    FrameDescriptorError(#[from] FrameDescriptorError),
    #[error("Not enough bytes in dict_id. Is: {got}, Should be: {expected}")]
    DictIdTooSmall { got: usize, expected: usize },
    #[error("frame_content_size does not have the right length. Is: {got}, Should be: {expected}")]
    MismatchedFrameSize { got: usize, expected: u8 },
    #[error("frame_content_size was zero")]
    FrameSizeIsZero,
    #[error("Invalid frame_content_size. Is: {got}, Should be one of 1, 2, 4, 8 bytes")]
    InvalidFrameSize { got: u8 },
}

#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum ReadFrameHeaderError {
    #[error("Error while reading magic number: {0}")]
    MagicNumberReadError(Error),
    #[error("Read wrong magic number: 0x{0:X}")]
    BadMagicNumber(u32),
    #[error("Error while reading frame descriptor: {0}")]
    FrameDescriptorReadError(Error),
    #[error("{0:?}")]
    InvalidFrameDescriptor(#[from] FrameDescriptorError),
    #[error("Error while reading window descriptor: {0}")]
    WindowDescriptorReadError(Error),
    #[error("Error while reading dictionary id: {0}")]
    DictionaryIdReadError(Error),
    #[error("Error while reading frame content size: {0}")]
    FrameContentSizeReadError(Error),
    #[error(
        "SkippableFrame encountered with MagicNumber 0x{magic_number:X} and length {length} bytes"
    )]
    SkipFrame { magic_number: u32, length: u32 },
}

#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum BlockTypeError {
    #[error(
        "Invalid Blocktype number. Is: {num} Should be one of: 0, 1, 2, 3 (3 is reserved though"
    )]
    InvalidBlocktypeNumber { num: u8 },
}

#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum BlockSizeError {
    #[error(
        "Blocksize was bigger than the absolute maximum {} (128kb). Is: {}",
        crate::common::MAX_BLOCK_SIZE,
        size
    )]
    BlockSizeTooLarge { size: u32 },
}

#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum BlockHeaderReadError {
    #[error("Error while reading the block header")]
    ReadError(#[from] Error),
    #[error("Reserved block occured. This is considered corruption by the documentation")]
    FoundReservedBlock,
    #[error("Error getting block type: {0}")]
    BlockTypeError(#[from] BlockTypeError),
    #[error("Error getting block content size: {0}")]
    BlockSizeError(#[from] BlockSizeError),
}

#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum DecompressBlockError {
    #[error("Error while reading the block content: {0}")]
    BlockContentReadError(#[from] Error),
    #[error(
        "Malformed section header. Says literals would be this long: {expected_len} but there are \
         only {remaining_bytes} bytes left"
    )]
    MalformedSectionHeader {
        expected_len: usize,
        remaining_bytes: usize,
    },
    #[error("{0:?}")]
    DecompressLiteralsError(#[from] DecompressLiteralsError),
    #[error("{0:?}")]
    LiteralsSectionParseError(#[from] LiteralsSectionParseError),
    #[error("{0:?}")]
    SequencesHeaderParseError(#[from] SequencesHeaderParseError),
    #[error("{0:?}")]
    DecodeSequenceError(#[from] DecodeSequenceError),
    #[error("{0:?}")]
    ExecuteSequencesError(#[from] ExecuteSequencesError),
}

#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum DecodeBlockContentError {
    #[error("Can't decode next block if failed along the way. Results will be nonsense")]
    DecoderStateIsFailed,
    #[error(
        "Can't decode next block body, while expecting to decode the header of the previous \
         block. Results will be nonsense"
    )]
    ExpectedHeaderOfPreviousBlock,
    #[error("Error while reading bytes for {step}: {source}")]
    ReadError { step: BlockType, source: Error },
    #[error("{0:?}")]
    DecompressBlockError(#[from] DecompressBlockError),
}

#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum DecodeBufferError {
    #[error("Need {need} bytes from the dictionary but it is only {got} bytes long")]
    NotEnoughBytesInDictionary { got: usize, need: usize },
    #[error("offset: {offset} bigger than buffer: {buf_len}")]
    OffsetTooBig { offset: usize, buf_len: usize },
}

#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum DictionaryDecodeError {
    #[error("The raw bytes did not contain a full valid zstd dictionary")]
    NotEnoughBytes,
    #[error(
        "Bad magic_num at start of the dictionary; Got: {got:#04X?}, Expected: {:#04x?}",
        crate::decoding::dictionary::MAGIC_NUM
    )]
    BadMagicNum { got: [u8; 4] },
    #[error("{0:?}")]
    FSETableError(#[from] FSETableError),
    #[error("{0:?}")]
    HuffmanTableError(#[from] HuffmanTableError),
}

#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum FrameDecoderError {
    #[error("{0:?}")]
    ReadFrameHeaderError(#[from] ReadFrameHeaderError),
    #[error("{0:?}")]
    FrameHeaderError(#[from] FrameHeaderError),
    #[error("Specified window_size is too big; Requested: {requested}, Max: {max}")]
    WindowSizeTooBig { requested: u64, max: u64 },
    #[error("{0:?}")]
    DictionaryDecodeError(#[from] DictionaryDecodeError),
    #[error("Failed to parse/decode block body: {0}")]
    FailedToReadBlockHeader(#[from] BlockHeaderReadError),
    #[error("Failed to parse block header: {0}")]
    FailedToReadBlockBody(#[from] DecodeBlockContentError),
    #[error("Failed to read checksum: {0}")]
    FailedToReadChecksum(Error),
    #[error("Decoder must initialized or reset before using it")]
    NotYetInitialized,
    #[error("Decoder encountered error while initializing: {0}")]
    FailedToInitialize(#[source] FrameHeaderError),
    #[error("Decoder encountered error while draining the decodebuffer: {0}")]
    FailedToDrainDecodebuffer(Error),
    #[error("Failed to skip bytes for the length given in the frame header")]
    FailedToSkipFrame,
    #[error("Target must have at least as many bytes as the contentsize of the frame reports")]
    TargetTooSmall,
    #[error(
        "Frame header specified dictionary id 0x{dict_id:X} that wasnt provided by add_dict() or \
         reset_with_dict()"
    )]
    DictNotProvided { dict_id: u32 },
}

#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum DecompressLiteralsError {
    #[error(
        "compressed size was none even though it must be set to something for compressed literals"
    )]
    MissingCompressedSize,
    #[error(
        "num_streams was none even though it must be set to something (1 or 4) for compressed \
         literals"
    )]
    MissingNumStreams,
    #[error("{0:?}")]
    GetBitsError(#[from] GetBitsError),
    #[error("{0:?}")]
    HuffmanTableError(#[from] HuffmanTableError),
    #[error("{0:?}")]
    HuffmanDecoderError(#[from] HuffmanDecoderError),
    #[error("Tried to reuse huffman table but it was never initialized")]
    UninitializedHuffmanTable,
    #[error("Need 6 bytes to decode jump header, got {got} bytes")]
    MissingBytesForJumpHeader { got: usize },
    #[error("Need at least {needed} bytes to decode literals. Have: {got} bytes")]
    MissingBytesForLiterals { got: usize, needed: usize },
    #[error(
        "Padding at the end of the sequence_section was more than a byte long: {skipped_bits} \
         bits. Probably caused by data corruption"
    )]
    ExtraPadding { skipped_bits: i32 },
    #[error("Bitstream was read till: {read_til}, should have been: {expected}")]
    BitstreamReadMismatch { read_til: isize, expected: isize },
    #[error("Did not decode enough literals: {decoded}, Should have been: {expected}")]
    DecodedLiteralCountMismatch { decoded: usize, expected: usize },
}

#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum ExecuteSequencesError {
    #[error("{0:?}")]
    DecodebufferError(#[from] DecodeBufferError),
    #[error("Sequence wants to copy up to byte {wanted}. Bytes in literalsbuffer: {have}")]
    NotEnoughBytesForSequence { wanted: usize, have: usize },
    #[error("Illegal offset: 0 found")]
    ZeroOffset,
    /// Flat-output execution ran out of space in the caller's target buffer.
    #[error("Not enough space in the target buffer for this block")]
    TargetTooSmall,
}

#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum DecodeSequenceError {
    #[error("{0:?}")]
    GetBitsError(#[from] GetBitsError),
    #[error("{0:?}")]
    FSEDecoderError(#[from] FSEDecoderError),
    #[error("{0:?}")]
    FSETableError(#[from] FSETableError),
    #[error(
        "Padding at the end of the sequence_section was more than a byte long: {skipped_bits} \
         bits. Probably caused by data corruption"
    )]
    ExtraPadding { skipped_bits: i32 },
    #[error("Do not support offsets bigger than 1<<32; got: {offset_code}")]
    UnsupportedOffset { offset_code: u8 },
    #[error("Read an offset == 0. That is an illegal value for offsets")]
    ZeroOffset,
    #[error("Bytestream did not contain enough bytes to decode num_sequences")]
    NotEnoughBytesForNumSequences,
    #[error("{bits_remaining}")]
    ExtraBits { bits_remaining: isize },
    #[error("compression modes are none but they must be set to something")]
    MissingCompressionMode,
    #[error("Need a byte to read for RLE ll table")]
    MissingByteForRleLlTable,
    #[error("Need a byte to read for RLE of table")]
    MissingByteForRleOfTable,
    #[error("Need a byte to read for RLE ml table")]
    MissingByteForRleMlTable,
}

#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum LiteralsSectionParseError {
    #[error("Illegal literalssectiontype. Is: {got}, must be in: 0, 1, 2, 3")]
    IllegalLiteralSectionType { got: u8 },
    #[error("{0:?}")]
    GetBitsError(#[from] GetBitsError),
    #[error("Not enough byte to parse the literals section header. Have: {have}, Need: {need}")]
    NotEnoughBytes { have: usize, need: u8 },
}

impl core::fmt::Display for LiteralsSectionType {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> Result<(), core::fmt::Error> {
        match self {
            LiteralsSectionType::Compressed => write!(f, "Compressed"),
            LiteralsSectionType::Raw => write!(f, "Raw"),
            LiteralsSectionType::RLE => write!(f, "RLE"),
            LiteralsSectionType::Treeless => write!(f, "Treeless"),
        }
    }
}

#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum SequencesHeaderParseError {
    #[error("source must have at least {need_at_least} bytes to parse header; got {got} bytes")]
    NotEnoughBytes { need_at_least: u8, got: usize },
}

#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum FSETableError {
    #[error("Acclog must be at least 1")]
    AccLogIsZero,
    #[error("Found FSE acc_log: {got} bigger than allowed maximum in this case: {max}")]
    AccLogTooBig { got: u8, max: u8 },
    #[error("{0:?}")]
    GetBitsError(#[from] GetBitsError),
    #[error(
        "The counter ({got}) exceeded the expected sum: {expected_sum}. This means an error or \
         corrupted data \n {symbol_probabilities:?}"
    )]
    ProbabilityCounterMismatch {
        got: u32,
        expected_sum: u32,
        symbol_probabilities: Vec<i32>,
    },
    #[error("There are too many symbols in this distribution: {got}. Max: 256")]
    TooManySymbols { got: usize },
}

#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum FSEDecoderError {
    #[error("{0:?}")]
    GetBitsError(#[from] GetBitsError),
    #[error("Tried to use an uninitialized table!")]
    TableIsUninitialized,
}

#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum HuffmanTableError {
    #[error("{0:?}")]
    GetBitsError(#[from] GetBitsError),
    #[error("{0:?}")]
    FSEDecoderError(#[from] FSEDecoderError),
    #[error("{0:?}")]
    FSETableError(#[from] FSETableError),
    #[error("Source needs to have at least one byte")]
    SourceIsEmpty,
    #[error(
        "Header says there should be {expected_bytes} bytes for the weights but there are only \
         {got_bytes} bytes in the stream"
    )]
    NotEnoughBytesForWeights {
        got_bytes: usize,
        expected_bytes: u8,
    },
    #[error(
        "Padding at the end of the sequence_section was more than a byte long: {skipped_bits} \
         bits. Probably caused by data corruption"
    )]
    ExtraPadding { skipped_bits: i32 },
    #[error("More than 255 weights decoded (got {got} weights). Stream is probably corrupted")]
    TooManyWeights { got: usize },
    #[error("Can't build huffman table without any weights")]
    MissingWeights,
    #[error("Leftover must be power of two but is: {got}")]
    LeftoverIsNotAPowerOf2 { got: u32 },
    #[error("Not enough bytes in stream to decompress weights. Is: {have}, Should be: {need}")]
    NotEnoughBytesToDecompressWeights { have: usize, need: usize },
    #[error(
        "FSE table used more bytes: {used} than were meant to be used for the whole stream of \
         huffman weights ({available_bytes})"
    )]
    FSETableUsedTooManyBytes { used: usize, available_bytes: u8 },
    #[error("Source needs to have at least {need} bytes, got: {got}")]
    NotEnoughBytesInSource { got: usize, need: usize },
    #[error(
        "Cant have weight: {got} bigger than max_num_bits: {}",
        crate::huff0::MAX_MAX_NUM_BITS
    )]
    WeightBiggerThanMaxNumBits { got: u8 },
    #[error(
        "max_bits derived from weights is: {got} should be lower than: {}",
        crate::huff0::MAX_MAX_NUM_BITS
    )]
    MaxBitsTooHigh { got: u8 },
}

#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum HuffmanDecoderError {
    #[error("{0:?}")]
    GetBitsError(#[from] GetBitsError),
}
