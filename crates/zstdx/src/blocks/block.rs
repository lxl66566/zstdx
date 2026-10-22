//! Block header definitions.

/// There are 4 different kinds of blocks, and the type of block influences the meaning of
/// `Block_Size`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BlockType {
    /// An uncompressed block.
    Raw,
    /// A single byte, repeated `Block_Size` times (Run Length Encoding).
    #[allow(clippy::upper_case_acronyms)]
    RLE,
    /// A Zstandard compressed block. `Block_Size` is the length of the compressed data.
    Compressed,
    /// This is not a valid block, and this value should not be used.
    /// If this value is present, it should be considered corrupted data.
    Reserved,
}

impl core::fmt::Display for BlockType {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> Result<(), core::fmt::Error> {
        match self {
            BlockType::Compressed => write!(f, "Compressed"),
            BlockType::Raw => write!(f, "Raw"),
            BlockType::RLE => write!(f, "RLE"),
            BlockType::Reserved => write!(f, "Reserved"),
        }
    }
}

/// A representation of a single block header. Each Zstandard frame contains
/// one or more blocks, each introduced by such a header.
pub struct BlockHeader {
    /// Whether this block is the last block in the frame.
    /// It may be followed by an optional `Content_Checksum` if it is.
    pub last_block: bool,
    pub block_type: BlockType,
    /// The number of decompressed bytes the block produces. For [BlockType::Raw]
    /// this equals `content_size`, for [BlockType::RLE] it is the repetition
    /// count. For [BlockType::Reserved] and [BlockType::Compressed] it is set to
    /// zero and should not be referenced (a compressed block's output size is
    /// only known after decoding).
    pub decompressed_size: u32,
    /// The size of the block. If the block is [BlockType::RLE],
    /// this value will be 1.
    pub content_size: u32,
}
