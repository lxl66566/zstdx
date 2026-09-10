//! Structures that wrap around various decoders to make decoding easier.

use super::super::blocks::sequence_section::Sequence;
use super::decode_buffer::DecodeBuffer;
use crate::decoding::dictionary::Dictionary;
use crate::fse::FSETable;
use crate::huff0::HuffmanTable;
use alloc::vec::Vec;

use crate::blocks::sequence_section::{
    MAX_LITERAL_LENGTH_CODE, MAX_MATCH_LENGTH_CODE, MAX_OFFSET_CODE,
};

/// A block level decoding buffer.
pub struct DecoderScratch {
    /// The decoder used for Huffman blocks.
    pub huf: HuffmanScratch,
    /// The decoder used for FSE blocks.
    pub fse: FSEScratch,

    pub buffer: DecodeBuffer,
    pub offset_hist: [u32; 3],

    pub literals_buffer: Vec<u8>,
    pub sequences: Vec<Sequence>,
    pub block_content_buffer: Vec<u8>,
}

impl DecoderScratch {
    pub fn new(window_size: usize) -> DecoderScratch {
        DecoderScratch {
            huf: HuffmanScratch {
                table: HuffmanTable::new(),
            },
            fse: FSEScratch::new(),
            buffer: DecodeBuffer::new(window_size),
            offset_hist: [1, 4, 8],

            block_content_buffer: Vec::new(),
            literals_buffer: Vec::new(),
            sequences: Vec::new(),
        }
    }

    pub fn reset(&mut self, window_size: usize) {
        self.offset_hist = [1, 4, 8];
        self.literals_buffer.clear();
        self.sequences.clear();
        self.block_content_buffer.clear();

        self.buffer.reset(window_size);

        self.fse.literal_lengths.reset();
        self.fse.match_lengths.reset();
        self.fse.offsets.reset();
        self.fse.ll_rle = None;
        self.fse.ml_rle = None;
        self.fse.of_rle = None;
        self.fse.ll_predefined = false;
        self.fse.ml_predefined = false;
        self.fse.of_predefined = false;
        self.fse.ll_seq_valid = false;
        self.fse.ml_seq_valid = false;
        self.fse.of_seq_valid = false;
        self.fse.ll_ready = false;
        self.fse.of_ready = false;
        self.fse.ml_ready = false;

        self.huf.table.reset();
    }

    pub fn init_from_dict(&mut self, dict: &Dictionary) {
        self.fse.reinit_from(&dict.fse);
        self.huf.table.reinit_from(&dict.huf.table);
        self.offset_hist = dict.offset_hist;
        self.buffer.dict_content.clear();
        self.buffer
            .dict_content
            .extend_from_slice(&dict.dict_content);
    }

    #[cfg(feature = "hash")]
    pub fn set_checksum_enabled(&mut self, on: bool) {
        self.buffer.set_checksum_enabled(on);
    }
}

pub struct HuffmanScratch {
    pub table: HuffmanTable,
}

impl HuffmanScratch {
    pub fn new() -> HuffmanScratch {
        HuffmanScratch {
            table: HuffmanTable::new(),
        }
    }
}

impl Default for HuffmanScratch {
    fn default() -> Self {
        Self::new()
    }
}

pub struct FSEScratch {
    pub offsets: FSETable,
    pub of_rle: Option<u8>,
    /// True while `offsets` holds the predefined distribution, so consecutive
    /// Predefined-mode blocks can skip rebuilding it.
    pub of_predefined: bool,
    pub literal_lengths: FSETable,
    pub ll_rle: Option<u8>,
    pub ll_predefined: bool,
    pub match_lengths: FSETable,
    pub ml_rle: Option<u8>,
    pub ml_predefined: bool,
    /// Whether each stream's decoding table is established (FSE table built,
    /// RLE fake entry written, or predefined installed) — Repeat mode with
    /// none of these ever happening is rejected as uninitialized.
    pub ll_ready: bool,
    pub of_ready: bool,
    pub ml_ready: bool,
    /// Packed sequence-decoding tables for the three streams in fixed slots
    /// (LL, then ML, then OF; see `sequence_section_decoder`), so the decode
    /// loop addresses all three through one base pointer with constant
    /// offsets. The per-slot valid flags track which slots need repacking
    /// after the underlying table changed.
    pub seq_packed: Vec<u64>,
    pub ll_seq_valid: bool,
    pub ml_seq_valid: bool,
    pub of_seq_valid: bool,
}

impl FSEScratch {
    pub fn new() -> FSEScratch {
        FSEScratch {
            offsets: FSETable::new(MAX_OFFSET_CODE),
            of_rle: None,
            of_predefined: false,
            literal_lengths: FSETable::new(MAX_LITERAL_LENGTH_CODE),
            ll_rle: None,
            ll_predefined: false,
            match_lengths: FSETable::new(MAX_MATCH_LENGTH_CODE),
            ml_rle: None,
            ml_predefined: false,
            seq_packed: alloc::vec![
                0;
                crate::decoding::sequence_section_decoder::SEQ_TABLE_SLOTS
            ],
            ll_seq_valid: false,
            ml_seq_valid: false,
            of_seq_valid: false,
            ll_ready: false,
            of_ready: false,
            ml_ready: false,
        }
    }

    pub fn reinit_from(&mut self, other: &Self) {
        self.offsets.reinit_from(&other.offsets);
        self.literal_lengths.reinit_from(&other.literal_lengths);
        self.match_lengths.reinit_from(&other.match_lengths);
        self.of_rle = other.of_rle;
        self.ll_rle = other.ll_rle;
        self.ml_rle = other.ml_rle;
        self.of_predefined = false;
        self.ll_predefined = false;
        self.ml_predefined = false;
        self.ll_seq_valid = false;
        self.ml_seq_valid = false;
        self.of_seq_valid = false;
        // The dictionary's tables are fully established; a first block may
        // legally reference them through Repeat mode.
        self.ll_ready = true;
        self.of_ready = true;
        self.ml_ready = true;
    }
}

impl Default for FSEScratch {
    fn default() -> Self {
        Self::new()
    }
}
