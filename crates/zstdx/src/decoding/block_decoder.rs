use alloc::vec::Vec;

use super::{
    super::blocks::{
        block::{BlockHeader, BlockType},
        literals_section::{LiteralsSection, LiteralsSectionType},
        sequence_section::SequencesHeader,
    },
    literals_section_decoder::decode_literals,
    sequence_section_decoder::decode_sequences,
};
use crate::{
    common::MAX_BLOCK_SIZE,
    decoding::{
        errors::{
            BlockHeaderReadError, BlockSizeError, BlockTypeError, DecodeBlockContentError,
            DecodeSequenceError, DecompressBlockError, ExecuteSequencesError,
        },
        scratch::DecoderScratch,
        sequence_execution::{execute_decoded_flat, execute_sequences},
        sequence_section_decoder::SeqDecoder,
    },
    io::Read,
};

pub struct BlockDecoder {
    header_buffer: [u8; 3],
    internal_state: DecoderState,
}

enum DecoderState {
    ReadyToDecodeNextHeader,
    ReadyToDecodeNextBody,
    #[allow(dead_code)]
    Failed, /* TODO put "self.internal_state = DecoderState::Failed;" everywhere an
             * unresolvable error occurs */
}

/// Create a new [BlockDecoder].
pub fn new() -> BlockDecoder {
    BlockDecoder {
        internal_state: DecoderState::ReadyToDecodeNextHeader,
        header_buffer: [0u8; 3],
    }
}

impl BlockDecoder {
    pub fn decode_block_content(
        &mut self,
        header: &BlockHeader,
        workspace: &mut DecoderScratch, /* reuse this as often as possible. Not only if the
                                         * trees are reused but also reuse the allocations when
                                         * building new trees */
        mut source: impl Read,
    ) -> Result<u64, DecodeBlockContentError> {
        match self.internal_state {
            DecoderState::ReadyToDecodeNextBody => { /* Happy :) */ },
            DecoderState::Failed => return Err(DecodeBlockContentError::DecoderStateIsFailed),
            DecoderState::ReadyToDecodeNextHeader => {
                return Err(DecodeBlockContentError::ExpectedHeaderOfPreviousBlock)
            },
        }

        let block_type = header.block_type;
        // Raw/RLE blocks declare their output in the header: bounded to
        // MAX_BLOCK_SIZE there, but a frame with a small window must keep
        // every block under min(window, MAX_BLOCK_SIZE) too (libzstd's
        // blockSizeMax). Checked before the content is read or written.
        let block_out_max = crate::common::max_block_output(workspace.buffer.window_size);
        if matches!(block_type, BlockType::Raw | BlockType::RLE)
            && header.decompressed_size as usize > block_out_max
        {
            return Err(DecodeBlockContentError::BlockOutputTooLarge { max: block_out_max });
        }
        match block_type {
            BlockType::RLE => {
                let mut buf = [0u8; 1];
                source.read_exact(&mut buf[..]).map_err(|err| {
                    DecodeBlockContentError::ReadError {
                        step: block_type,
                        source: err,
                    }
                })?;
                workspace
                    .buffer
                    .extend_and_fill(buf[0], header.decompressed_size as usize);

                self.internal_state = DecoderState::ReadyToDecodeNextHeader;

                Ok(1)
            },
            BlockType::Raw => {
                workspace
                    .buffer
                    .extend_from_reader(&mut source, header.decompressed_size as usize)
                    .map_err(|err| DecodeBlockContentError::ReadError {
                        step: block_type,
                        source: err,
                    })?;

                self.internal_state = DecoderState::ReadyToDecodeNextHeader;
                Ok(u64::from(header.decompressed_size))
            },

            BlockType::Reserved => {
                panic!(
                    "How did you even get this. The decoder should error out if it detects a \
                     reserved-type block"
                );
            },

            BlockType::Compressed => {
                Self::decompress_block(header, workspace, source)?;

                self.internal_state = DecoderState::ReadyToDecodeNextHeader;
                Ok(u64::from(header.content_size))
            },
        }
    }

    fn decompress_block(
        header: &BlockHeader,
        workspace: &mut DecoderScratch, /* reuse this as often as possible. Not only if the
                                         * trees are reused but also reuse the allocations when
                                         * building new trees */
        mut source: impl Read,
    ) -> Result<(), DecompressBlockError> {
        let block_out_max = crate::common::max_block_output(workspace.buffer.window_size);
        let DecoderScratch {
            huf,
            fse,
            buffer,
            offset_hist: _,
            literals_buffer,
            sequences,
            block_content_buffer,
        } = workspace;
        let (seq_section, raw) = Self::parse_sections(
            header,
            block_content_buffer,
            huf,
            literals_buffer,
            block_out_max,
            &mut source,
        )?;

        if seq_section.num_sequences != 0 {
            decode_sequences(&seq_section, raw, fse, sequences)?;
            vprintln!("Executing sequences");
            execute_sequences(workspace)?;
        } else {
            if !raw.is_empty() {
                return Err(DecompressBlockError::DecodeSequenceError(
                    DecodeSequenceError::ExtraBits {
                        bits_remaining: raw.len() as isize * 8,
                    },
                ));
            }
            buffer.push(literals_buffer);
            sequences.clear();
        }

        Ok(())
    }

    /// Compressed-block fast path for flat decoding: decodes the sections as
    /// usual but executes each sequence the moment it is decoded, straight
    /// into `out[*written..]`, bypassing the ring buffer. Returns the number
    /// of bytes produced so far. `virt_base`/`view` describe the virtual
    /// window mapping of the backing flat buffer (see
    /// `execute_decoded_flat`).
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn decompress_block_flat(
        header: &BlockHeader,
        workspace: &mut DecoderScratch,
        source: &mut impl Read,
        out: &mut [u8],
        written: &mut usize,
        virt_base: usize,
        view: crate::decoding::flat_buffer::FlatView,
        headroom: bool,
    ) -> Result<usize, DecompressBlockError> {
        // Enforce the per-block output cap by bounding the executor's
        // target: a block may never produce more than
        // min(window, MAX_BLOCK_SIZE) bytes, so a sequence section claiming
        // beyond that is stopped by the existing budget checks against the
        // bounded slice. When the bounded slice was the binding bound (the
        // real target had more room), a TargetTooSmall from below is the
        // block cap, not a small caller buffer, and is reclassified below.
        let block_out_max = crate::common::max_block_output(workspace.buffer.window_size);
        let DecoderScratch {
            huf,
            fse,
            offset_hist,
            literals_buffer,
            sequences,
            block_content_buffer,
            ..
        } = workspace;
        let (seq_section, raw) = Self::parse_sections(
            header,
            block_content_buffer,
            huf,
            literals_buffer,
            block_out_max,
            source,
        )?;

        let real_out_len = out.len();
        let block_limit = (*written).saturating_add(block_out_max).min(real_out_len);
        let out = &mut out[..block_limit];

        if seq_section.num_sequences != 0 {
            // The flat executor's inline 16-byte literal chunks read up to
            // 15 bytes past the final literal (see `wildcopy_literals`).
            // `reserve` keeps the overshoot inside the allocation; zeroing
            // the overshoot window keeps the read initialized — under the
            // strict Rust model, scanning the uninitialized capacity tail
            // is UB even when the values are never used (libzstd's
            // ZSTD_wildcopy over-reads the same shape in C and gets away
            // with it there). The cost is a 16-byte memset per block; the
            // buffer is pooled and `reserve` ran above, so this never
            // reallocates.
            literals_buffer.reserve(16);
            let len = literals_buffer.len();
            literals_buffer.resize(len + 16, 0);
            literals_buffer.truncate(len);
            let mut dec = SeqDecoder::new(&seq_section, raw, fse)?;
            execute_decoded_flat(
                &mut dec,
                literals_buffer,
                out,
                written,
                virt_base,
                view,
                offset_hist,
                headroom,
            )
            .map_err(|e| match e {
                DecompressBlockError::ExecuteSequencesError(
                    ExecuteSequencesError::TargetTooSmall,
                ) if block_limit < real_out_len => {
                    DecompressBlockError::BlockOutputTooLarge { max: block_out_max }
                },
                other => other,
            })?;
        } else {
            if !raw.is_empty() {
                return Err(DecompressBlockError::DecodeSequenceError(
                    DecodeSequenceError::ExtraBits {
                        bits_remaining: raw.len() as isize * 8,
                    },
                ));
            }
            let literals_len = literals_buffer.len();
            if *written + literals_len > out.len() {
                return Err(DecompressBlockError::ExecuteSequencesError(
                    ExecuteSequencesError::TargetTooSmall,
                ));
            }
            out[*written..*written + literals_len].copy_from_slice(literals_buffer);
            *written += literals_len;
            sequences.clear();
        }
        Ok(*written)
    }

    /// Read a compressed block's body and decode its literals and sequence
    /// section header into the workspace; shared by the ring-buffer and flat
    /// execution paths. Returns the sequence section header and the raw
    /// sequence-section bytes (borrowed from the block content buffer) —
    /// decoding them into actual sequences is the caller's job, so the flat
    /// path can fuse it with execution. Takes the scratch fields individually
    /// so the returned borrow coexists with further field borrows.
    /// `block_out_max` is the per-block output cap the literals section is
    /// validated against before its size is used for allocation.
    fn parse_sections<'a>(
        header: &BlockHeader,
        block_content_buffer: &'a mut Vec<u8>,
        huf: &mut super::scratch::HuffmanScratch,
        literals_buffer: &mut Vec<u8>,
        block_out_max: usize,
        source: &mut impl Read,
    ) -> Result<(SequencesHeader, &'a [u8]), DecompressBlockError> {
        // The stored (compressed) body is bounded by the same per-block cap
        // as the output (libzstd's header-stage `cBlockSize > blockSizeMax`
        // for every block type): a small-window frame cannot carry a block
        // body larger than its window. Checked before the body is read.
        if header.content_size as usize > block_out_max {
            return Err(DecompressBlockError::BlockOutputTooLarge { max: block_out_max });
        }
        block_content_buffer.resize(header.content_size as usize, 0);

        source.read_exact(block_content_buffer.as_mut_slice())?;
        let raw: &'a [u8] = block_content_buffer.as_slice();

        let mut section = LiteralsSection::new();
        let bytes_in_literals_header = section.parse_from_header(raw)?;
        // The regenerated size is itself bounded by the block maximum
        // (libzstd rejects litSize > blockSizeMax); enforce before the
        // literals buffer is sized from it — an RLE section can claim up to
        // 1 MiB from a single payload byte.
        if section.regenerated_size as usize > block_out_max {
            return Err(DecompressBlockError::BlockOutputTooLarge { max: block_out_max });
        }
        let raw = &raw[bytes_in_literals_header as usize..];
        vprintln!(
            "Found {} literalssection with regenerated size: {}, and compressed size: {:?}",
            section.ls_type,
            section.regenerated_size,
            section.compressed_size
        );

        let upper_limit_for_literals = match section.compressed_size {
            Some(x) => x as usize,
            None => match section.ls_type {
                LiteralsSectionType::RLE => 1,
                LiteralsSectionType::Raw => section.regenerated_size as usize,
                _ => panic!("Bug in this library"),
            },
        };

        if raw.len() < upper_limit_for_literals {
            return Err(DecompressBlockError::MalformedSectionHeader {
                expected_len: upper_limit_for_literals,
                remaining_bytes: raw.len(),
            });
        }

        let raw_literals = &raw[..upper_limit_for_literals];
        vprintln!("Slice for literals: {}", raw_literals.len());

        literals_buffer.clear(); //all literals of the previous block must have been used in the sequence execution anyways. just be defensive here
        let bytes_used_in_literals_section =
            decode_literals(&section, huf, raw_literals, literals_buffer)?;
        assert!(
            section.regenerated_size == literals_buffer.len() as u32,
            "Wrong number of literals: {}, Should have been: {}",
            literals_buffer.len(),
            section.regenerated_size
        );
        assert_eq!(
            bytes_used_in_literals_section,
            upper_limit_for_literals as u32
        );

        let raw = &raw[upper_limit_for_literals..];
        vprintln!("Slice for sequences with headers: {}", raw.len());

        let mut seq_section = SequencesHeader::new();
        let bytes_in_sequence_header = seq_section.parse_from_header(raw)?;
        let raw = &raw[bytes_in_sequence_header as usize..];
        vprintln!(
            "Found sequencessection with sequences: {} and size: {}",
            seq_section.num_sequences,
            raw.len()
        );

        assert_eq!(
            u32::from(bytes_in_literals_header)
                + bytes_used_in_literals_section
                + u32::from(bytes_in_sequence_header)
                + raw.len() as u32,
            header.content_size
        );
        vprintln!("Slice for sequences: {}", raw.len());

        Ok((seq_section, raw))
    }

    /// Reads 3 bytes from the provided reader and returns
    /// the deserialized header and the number of bytes read.
    pub fn read_block_header(
        &mut self,
        mut r: impl Read,
    ) -> Result<(BlockHeader, u8), BlockHeaderReadError> {
        // match self.internal_state {
        //    DecoderState::ReadyToDecodeNextHeader => {/* Happy :) */},
        //    DecoderState::Failed => return Err(format!("Cant decode next block if failed along the
        // way. Results will be nonsense")),    DecoderState::ReadyToDecodeNextBody =>
        // return Err(format!("Cant decode next block header, while expecting to decode the body of
        // the previous block. Results will be nonsense")),
        //}

        r.read_exact(&mut self.header_buffer[0..3])?;

        let btype = self.block_type()?;
        if let BlockType::Reserved = btype {
            return Err(BlockHeaderReadError::FoundReservedBlock);
        }

        let block_size = self.block_content_size()?;
        let decompressed_size = match btype {
            BlockType::Raw | BlockType::RLE => block_size,
            // Reserved is rejected above; Compressed is only known after decoding
            BlockType::Reserved | BlockType::Compressed => 0,
        };
        let content_size = match btype {
            BlockType::Raw | BlockType::Compressed => block_size,
            BlockType::RLE => 1,
            // Reserved is rejected above, this is an error state
            BlockType::Reserved => 0,
        };

        let last_block = self.is_last();

        self.reset_buffer();
        self.internal_state = DecoderState::ReadyToDecodeNextBody;

        // just return 3. Blockheaders always take 3 bytes
        Ok((
            BlockHeader {
                last_block,
                block_type: btype,
                decompressed_size,
                content_size,
            },
            3,
        ))
    }

    fn reset_buffer(&mut self) {
        self.header_buffer[0] = 0;
        self.header_buffer[1] = 0;
        self.header_buffer[2] = 0;
    }

    fn is_last(&self) -> bool {
        self.header_buffer[0] & 0x1 == 1
    }

    fn block_type(&self) -> Result<BlockType, BlockTypeError> {
        let t = (self.header_buffer[0] >> 1) & 0x3;
        match t {
            0 => Ok(BlockType::Raw),
            1 => Ok(BlockType::RLE),
            2 => Ok(BlockType::Compressed),
            3 => Ok(BlockType::Reserved),
            other => Err(BlockTypeError::InvalidBlocktypeNumber { num: other }),
        }
    }

    fn block_content_size(&self) -> Result<u32, BlockSizeError> {
        let val = self.block_content_size_unchecked();
        if val > MAX_BLOCK_SIZE {
            Err(BlockSizeError::BlockSizeTooLarge { size: val })
        } else {
            Ok(val)
        }
    }

    fn block_content_size_unchecked(&self) -> u32 {
        u32::from(self.header_buffer[0] >> 3) //push out type and last_block flags. Retain 5 bit
            | (u32::from(self.header_buffer[1]) << 5)
            | (u32::from(self.header_buffer[2]) << 13)
    }
}
