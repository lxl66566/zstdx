use alloc::vec::Vec;

use super::{
    super::blocks::{
        block::{BlockHeader, BlockType},
        literals_section::{LiteralsSection, LiteralsSectionType},
        sequence_section::SequencesHeader,
    },
    literals_section_decoder::{decode_literals, decode_literals_direct},
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
}

/// Create a new [BlockDecoder].
pub fn new() -> BlockDecoder {
    BlockDecoder {
        header_buffer: [0u8; 3],
    }
}

/// Give the literals staging buffer its 16-byte zeroed tail behind the
/// length: the flat executor's `wildcopy_literals` reads up to 15 bytes
/// past the final literal (see `sequence_execution`), `reserve` keeps the
/// overshoot inside the allocation, and zeroing the overshoot window keeps
/// the read initialized — under the strict Rust model, scanning the
/// uninitialized capacity tail is UB even when the values are never used
/// (libzstd's ZSTD_wildcopy over-reads the same shape in C and gets away
/// with it there). The cost is a 16-byte memset per block; the buffer is
/// pooled and the reserve is a no-op after the first block, so this never
/// reallocates. The staging buffer's every producer must route through
/// here before the flat executor reads it.
fn zero_wildcopy_tail(literals_buffer: &mut Vec<u8>) {
    literals_buffer.reserve(16);
    let len = literals_buffer.len();
    literals_buffer.resize(len + 16, 0);
    literals_buffer.truncate(len);
}

/// Where a compressed block's literals landed.
#[derive(Clone, Copy, PartialEq, Eq)]
enum LiteralsPlacement {
    /// The scratch staging buffer (`literals_buffer`).
    Staged,
    /// Decoded straight to the output cursor (zero-sequence blocks).
    DirectAtOp,
    /// Staged inside the output buffer past the block's write limit, so
    /// the executor's literal copies write lines the literals decode just
    /// made cache-present instead of paying a write-allocate fill per line.
    InDst,
}

/// Out-of-line execution twin for in-dst staged blocks: the staging gate
/// implies the unclamped write limit, so the split hands the executor the
/// same bounded target the scratch path truncates to, with the literals
/// carved out of the output buffer above that limit. The `TargetTooSmall`
/// reclassification mirrors the staged path.
#[allow(clippy::too_many_arguments)]
#[inline(never)]
fn decompress_block_flat_indst(
    seq_section: &SequencesHeader,
    raw: &[u8],
    literals_len: u32,
    fse: &mut super::scratch::FSEScratch,
    offset_hist: &mut [u32; 3],
    out: &mut [u8],
    written: &mut usize,
    virt_base: usize,
    view: crate::decoding::flat_buffer::FlatView,
    headroom: bool,
    block_out_max: usize,
    block_limit: usize,
    real_out_len: usize,
) -> Result<usize, DecompressBlockError> {
    debug_assert_eq!(block_limit, *written + block_out_max);
    let (target, staging) = out.split_at_mut(block_limit);
    let literals = &staging[IN_DST_PAD..IN_DST_PAD + literals_len as usize];
    let mut dec = SeqDecoder::new(seq_section, raw, fse)?;
    execute_decoded_flat(
        &mut dec,
        literals,
        target,
        written,
        virt_base,
        &view,
        offset_hist,
        headroom,
    )
    .map_err(|e| match e {
        DecompressBlockError::ExecuteSequencesError(ExecuteSequencesError::TargetTooSmall)
            if block_limit < real_out_len =>
        {
            DecompressBlockError::BlockOutputTooLarge { max: block_out_max }
        },
        other => other,
    })?;
    Ok(*written)
}

/// Gap between the flat executor's write limit and the in-dst staging
/// region: wildcopy stores stop at most one byte short of `block_limit`,
/// so 16 bytes above it are safe by construction.
const IN_DST_PAD: usize = 16;
/// Readable bytes the in-dst staging region must keep above the literals
/// end: the executor over-reads at most 15 bytes past it.
const IN_DST_TAIL: usize = 16;
/// Literals a block must carry before in-dst staging engages: the
/// fill-avoidance targets frames whose runs average hundreds of bytes, so
/// at 128 KiB blocks only literal-dominated ones engage and short-run
/// frames stay on the staged executor.
const IN_DST_MIN_LITERALS: usize = 64 * 1024;

impl BlockDecoder {
    pub fn decode_block_content(
        header: &BlockHeader,
        workspace: &mut DecoderScratch, /* reuse this as often as possible. Not only if the
                                         * trees are reused but also reuse the allocations when
                                         * building new trees */
        mut source: impl Read,
    ) -> Result<u64, DecodeBlockContentError> {
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
        let (seq_section, raw, ..) = Self::parse_sections(
            header,
            block_content_buffer,
            huf,
            literals_buffer,
            block_out_max,
            &mut source,
            None,
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
        block_content_buffer.resize(header.content_size as usize, 0);
        source.read_exact(block_content_buffer.as_mut_slice())?;
        Self::decompress_block_flat_body(
            header,
            block_out_max,
            block_content_buffer,
            huf,
            fse,
            offset_hist,
            literals_buffer,
            sequences,
            out,
            written,
            virt_base,
            view,
            headroom,
        )
    }

    /// Slice-source twin of [`Self::decompress_block_flat`]: the block body
    /// is split off the input slice and parsed in place, so the per-block
    /// copy into the staging buffer disappears. A body shorter than the
    /// header's content size reports the same error the `Read` path's
    /// `read_exact` produces.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn decompress_block_flat_slice(
        header: &BlockHeader,
        workspace: &mut DecoderScratch,
        source: &mut &[u8],
        out: &mut [u8],
        written: &mut usize,
        virt_base: usize,
        view: crate::decoding::flat_buffer::FlatView,
        headroom: bool,
    ) -> Result<usize, DecompressBlockError> {
        let Some(body) = source.get(..header.content_size as usize) else {
            return Err(DecompressBlockError::BlockContentReadError(
                crate::io::Error::from(crate::io::ErrorKind::UnexpectedEof),
            ));
        };
        let rest = &source[header.content_size as usize..];
        *source = rest;
        let block_out_max = crate::common::max_block_output(workspace.buffer.window_size);
        let DecoderScratch {
            huf,
            fse,
            offset_hist,
            literals_buffer,
            sequences,
            ..
        } = workspace;
        Self::decompress_block_flat_body(
            header,
            block_out_max,
            body,
            huf,
            fse,
            offset_hist,
            literals_buffer,
            sequences,
            out,
            written,
            virt_base,
            view,
            headroom,
        )
    }

    /// The flat compressed-block decode over an already fetched body: the
    /// section parse (direct literals included) and the fused execution,
    /// shared by the `Read` and slice entries. Takes the scratch fields
    /// individually so the fetched body (from either buffer) borrows
    /// alongside them.
    #[allow(clippy::too_many_arguments)]
    fn decompress_block_flat_body(
        header: &BlockHeader,
        block_out_max: usize,
        body: &[u8],
        huf: &mut super::scratch::HuffmanScratch,
        fse: &mut super::scratch::FSEScratch,
        offset_hist: &mut [u32; 3],
        literals_buffer: &mut Vec<u8>,
        sequences: &mut Vec<crate::blocks::sequence_section::Sequence>,
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
        // HEADROOM chain, consumer side: `headroom` promises the backing
        // allocation holds the block maximum plus wildcopy slack beyond the
        // cursor (`FlatOut::ensure_block_space`'s true return, asserted at
        // its checked entry), because the executor's HEADROOM instantiation
        // wildcopies without a per-sequence slack check. Checked here, the
        // one place that knows the block maximum.
        debug_assert!(
            !headroom || out.len() >= block_out_max + crate::decoding::flat_buffer::WILDCOPY_SLACK
        );
        // In-dst staging needs the frame's whole history in the target with
        // no wrapped generations: the HEADROOM paths run the windowed
        // streaming buffer, whose previous generation physically sits above
        // the write cursor, where the staging region would, and wrapped
        // match reads must not see staging bytes. libzstd refuses the same
        // arrangement when streaming.
        let in_dst_staging = if headroom {
            None
        } else {
            Some((*written).saturating_add(block_out_max + IN_DST_PAD))
        };
        let (seq_section, raw, literals_len, placement) = Self::parse_sections_body(
            header,
            huf,
            literals_buffer,
            block_out_max,
            body,
            Some((&mut *out, *written)),
            in_dst_staging,
        )?;

        let real_out_len = out.len();
        let block_limit = (*written).saturating_add(block_out_max).min(real_out_len);

        // The in-dst twin holds its own executor body: keeping this
        // literal source out of the code below preserves the staged call
        // site's register allocation, which every short-run shape pays per
        // sequence.
        if placement == LiteralsPlacement::InDst && seq_section.num_sequences != 0 {
            return decompress_block_flat_indst(
                &seq_section,
                raw,
                literals_len,
                fse,
                offset_hist,
                out,
                written,
                virt_base,
                view,
                headroom,
                block_out_max,
                block_limit,
                real_out_len,
            );
        }

        let out = &mut out[..block_limit];

        if seq_section.num_sequences != 0 {
            // The flat executor's literal copy reads up to 15 bytes
            // past the final literal; give the staging buffer its 16-byte
            // zeroed tail so the overread stays initialized (see
            // `zero_wildcopy_tail`).
            zero_wildcopy_tail(literals_buffer);
            let mut dec = SeqDecoder::new(&seq_section, raw, fse)?;
            execute_decoded_flat(
                &mut dec,
                literals_buffer,
                out,
                written,
                virt_base,
                &view,
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
            let literals_len = literals_len as usize;
            if placement == LiteralsPlacement::DirectAtOp {
                // The literals were decoded straight into the target above;
                // the fit was checked before the decode ran.
                *written += literals_len;
            } else {
                if *written + literals_len > out.len() {
                    return Err(DecompressBlockError::ExecuteSequencesError(
                        ExecuteSequencesError::TargetTooSmall,
                    ));
                }
                out[*written..*written + literals_len].copy_from_slice(literals_buffer);
                *written += literals_len;
            }
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
    #[allow(clippy::too_many_arguments)]
    fn parse_sections<'a>(
        header: &BlockHeader,
        block_content_buffer: &'a mut Vec<u8>,
        huf: &mut super::scratch::HuffmanScratch,
        literals_buffer: &mut Vec<u8>,
        block_out_max: usize,
        source: &mut impl Read,
        direct_out: Option<(&mut [u8], usize)>,
    ) -> Result<(SequencesHeader, &'a [u8], u32, LiteralsPlacement), DecompressBlockError> {
        block_content_buffer.resize(header.content_size as usize, 0);
        source.read_exact(block_content_buffer.as_mut_slice())?;
        Self::parse_sections_body(
            header,
            huf,
            literals_buffer,
            block_out_max,
            block_content_buffer,
            direct_out,
            None,
        )
    }

    /// Parsing half of a compressed block over an already fetched body:
    /// the literals section (the pure-literals direct decode included) and
    /// the sequence section header. Returns the sequence section header,
    /// the raw sequence bytes (borrowed from `body`), the section's
    /// literals length and whether the literals were decoded directly into
    /// the caller's target.
    #[allow(clippy::too_many_arguments)]
    #[allow(clippy::type_complexity)]
    fn parse_sections_body<'a>(
        header: &BlockHeader,
        huf: &mut super::scratch::HuffmanScratch,
        literals_buffer: &mut Vec<u8>,
        block_out_max: usize,
        body: &'a [u8],
        direct_out: Option<(&mut [u8], usize)>,
        in_dst_staging: Option<usize>,
    ) -> Result<(SequencesHeader, &'a [u8], u32, LiteralsPlacement), DecompressBlockError> {
        // The stored (compressed) body is bounded by the same per-block cap
        // as the output (libzstd's header-stage `cBlockSize > blockSizeMax`
        // for every block type): a small-window frame cannot carry a block
        // body larger than its window.
        if header.content_size as usize > block_out_max {
            return Err(DecompressBlockError::BlockOutputTooLarge { max: block_out_max });
        }
        let raw: &'a [u8] = body;

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

        // Pure-literals block fast path: a sequence section declaring zero
        // sequences encodes its count as a single zero byte, so peeking
        // that byte lets a flat caller receive its literals decoded
        // straight into the output slice, and the staging buffer with its
        // copy disappear. The peek only reads: every real parse error keeps
        // its position after the literals decode, and a target too small
        // falls back to staging, which reports the error where it always
        // did.
        let raw = &raw[upper_limit_for_literals..];
        let mut placement = LiteralsPlacement::Staged;
        if let Some((out, written)) = direct_out {
            if Self::seq_count_is_zero(raw)
                && section.regenerated_size as usize <= out.len() - written
            {
                let target = &mut out[written..][..section.regenerated_size as usize];
                placement = if decode_literals_direct(&section, huf, raw_literals, target)? {
                    LiteralsPlacement::DirectAtOp
                } else {
                    LiteralsPlacement::Staged
                };
            } else if let Some(base) = in_dst_staging {
                // In-dst staging, the port of libzstd's ZSTD_in_dst
                // arrangement: the literals decode writes the block's
                // staging region inside the output buffer, past the write
                // limit the executor can reach. The gate keeps the pad and
                // the over-read tail inside the target; a declined gate or
                // a section the direct decoder does not serve falls back to
                // the scratch buffer with unchanged error behavior. The
                // literals floor keeps short-run shapes on the staged
                // inline executor: the in-dst twin is out of line, and its
                // register allocation costs those shapes per sequence,
                // while the fill-avoidance only pays when long literal
                // runs dominate the block.
                let lit = section.regenerated_size as usize;
                if lit >= IN_DST_MIN_LITERALS
                    && base
                        .checked_add(lit + IN_DST_TAIL)
                        .is_some_and(|end| end <= out.len())
                {
                    let target = &mut out[base..][..lit];
                    if decode_literals_direct(&section, huf, raw_literals, target)? {
                        placement = LiteralsPlacement::InDst;
                    }
                }
            }
        }

        // all literals of the previous block must have been used in the sequence execution anyways.
        // just be defensive here
        literals_buffer.clear();
        let bytes_used_in_literals_section = if placement == LiteralsPlacement::Staged {
            let bytes_used = decode_literals(&section, huf, raw_literals, literals_buffer)?;
            assert!(
                section.regenerated_size == literals_buffer.len() as u32,
                "Wrong number of literals: {}, Should have been: {}",
                literals_buffer.len(),
                section.regenerated_size
            );
            assert_eq!(bytes_used, upper_limit_for_literals as u32);
            bytes_used
        } else {
            upper_limit_for_literals as u32
        };
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

        Ok((seq_section, raw, section.regenerated_size, placement))
    }

    /// Whether a sequence section's first byte encodes a zero sequence
    /// count (the peek behind the pure-literals fast path in
    /// [`Self::parse_sections`]). An empty section is not one: its
    /// rejection lives in the sequence header parse, in its original
    /// position.
    fn seq_count_is_zero(seq_section: &[u8]) -> bool {
        seq_section.first() == Some(&0)
    }

    /// Reads 3 bytes from the provided reader and returns
    /// the deserialized header and the number of bytes read.
    pub fn read_block_header(
        &mut self,
        mut r: impl Read,
    ) -> Result<(BlockHeader, u8), BlockHeaderReadError> {
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

#[cfg(test)]
mod tests {
    use super::new;
    use crate::{
        blocks::block::BlockType,
        decoding::errors::{BlockHeaderReadError, BlockSizeError},
    };

    fn read(bytes: &[u8]) -> Result<(crate::blocks::block::BlockHeader, u8), BlockHeaderReadError> {
        new().read_block_header(bytes)
    }

    /// RFC 8878 3.1.1: 3-byte header packing the last-block bit, the 2-bit
    /// type and the 21-bit block size, little-endian, plus the per-type
    /// meaning of the size for Raw (stored and output size), RLE (output
    /// repetitions, one stored byte) and Compressed (stored size only).
    #[test]
    fn header_field_layout() {
        let (h, n) = read(&[0x01, 0x00, 0x00]).unwrap();
        assert!(h.last_block);
        assert_eq!(h.block_type, BlockType::Raw);
        assert_eq!((h.content_size, h.decompressed_size, n), (0, 0, 3));

        let (h, _) = read(&[0x28, 0x00, 0x00]).unwrap();
        assert_eq!(h.block_type, BlockType::Raw);
        assert_eq!((h.content_size, h.decompressed_size), (5, 5));

        let (h, _) = read(&[0x2a, 0x00, 0x00]).unwrap();
        assert_eq!(h.block_type, BlockType::RLE);
        assert_eq!((h.content_size, h.decompressed_size), (1, 5));

        // max block size must pass: 128 KiB, high bits in byte 2
        let (h, _) = read(&[0x04, 0x00, 0x10]).unwrap();
        assert_eq!(h.block_type, BlockType::Compressed);
        assert_eq!((h.content_size, h.decompressed_size), (128 * 1024, 0));
    }

    /// Reserved block types and sizes over the 128 KiB block maximum are
    /// rejected at header-parse time.
    #[test]
    fn reserved_and_oversized_are_rejected() {
        const OVER: u32 = 128 * 1024 + 1;
        assert!(matches!(
            read(&[0x06, 0x00, 0x00]),
            Err(BlockHeaderReadError::FoundReservedBlock)
        ));
        assert!(matches!(
            read(&[0x08, 0x00, 0x10]),
            Err(BlockHeaderReadError::BlockSizeError(
                BlockSizeError::BlockSizeTooLarge { size: OVER }
            ))
        ));
    }

    #[test]
    fn truncated_header_is_a_read_error() {
        assert!(read(&[0x01, 0x00]).is_err());
    }
}
