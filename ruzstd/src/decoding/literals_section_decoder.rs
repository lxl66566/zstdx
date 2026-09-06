//! This module contains the decompress_literals function, used to take a
//! parsed literals header and a source and decompress it.

use super::super::blocks::literals_section::{LiteralsSection, LiteralsSectionType};
use super::scratch::HuffmanScratch;
use crate::bit_io::BitReaderReversed;
use crate::decoding::errors::DecompressLiteralsError;
use crate::huff0::{HuffmanDecoder, HuffmanTable};
use alloc::vec::Vec;
use core::convert::TryInto;

/// Decode and decompress the provided literals section into `target`, returning the number of bytes read.
pub fn decode_literals(
    section: &LiteralsSection,
    scratch: &mut HuffmanScratch,
    source: &[u8],
    target: &mut Vec<u8>,
) -> Result<u32, DecompressLiteralsError> {
    match section.ls_type {
        LiteralsSectionType::Raw => {
            target.extend(&source[0..section.regenerated_size as usize]);
            Ok(section.regenerated_size)
        }
        LiteralsSectionType::RLE => {
            target.resize(target.len() + section.regenerated_size as usize, source[0]);
            Ok(1)
        }
        LiteralsSectionType::Compressed | LiteralsSectionType::Treeless => {
            let bytes_read = decompress_literals(section, scratch, source, target)?;

            //return sum of used bytes
            Ok(bytes_read)
        }
    }
}

/// Decompress the provided literals section and source into the provided `target`.
/// This function is used when the literals section is `Compressed` or `Treeless`
///
/// Returns the number of bytes read.
fn decompress_literals(
    section: &LiteralsSection,
    scratch: &mut HuffmanScratch,
    source: &[u8],
    target: &mut Vec<u8>,
) -> Result<u32, DecompressLiteralsError> {
    use DecompressLiteralsError as err;

    let compressed_size = section.compressed_size.ok_or(err::MissingCompressedSize)? as usize;
    let num_streams = section.num_streams.ok_or(err::MissingNumStreams)?;

    target.reserve(section.regenerated_size as usize);
    let source = &source[0..compressed_size];
    let mut bytes_read = 0;

    match section.ls_type {
        LiteralsSectionType::Compressed => {
            //read Huffman tree description
            bytes_read += scratch.table.build_decoder(source)?;
            vprintln!("Built huffman table using {} bytes", bytes_read);
        }
        LiteralsSectionType::Treeless if scratch.table.max_num_bits == 0 => {
            return Err(err::UninitializedHuffmanTable);
        }

        _ => { /* nothing to do, huffman tree has been provided by previous block */ }
    }

    let source = &source[bytes_read as usize..];

    if num_streams == 4 {
        //build jumptable
        if source.len() < 6 {
            return Err(err::MissingBytesForJumpHeader { got: source.len() });
        }
        let jump1 = source[0] as usize + ((source[1] as usize) << 8);
        let jump2 = jump1 + source[2] as usize + ((source[3] as usize) << 8);
        let jump3 = jump2 + source[4] as usize + ((source[5] as usize) << 8);
        bytes_read += 6;
        let source = &source[6..];

        if source.len() < jump3 {
            return Err(err::MissingBytesForLiterals {
                got: source.len(),
                needed: jump3,
            });
        }

        //decode 4 streams. The format splits the literals into four equal parts
        //(the fourth may be smaller), so stream k decodes exactly segment * (k+1)
        //bytes (the last stream the remainder).
        let total_out = section.regenerated_size as usize;
        let segment = total_out.div_ceil(4);
        let stream_bounds = [
            (0, jump1),
            (jump1, jump2),
            (jump2, jump3),
            (jump3, source.len()),
        ];

        // Fast interleaved path: needs one 8 byte window per stream and enough output
        // for the 4-way interleave to make progress (stream 4 must start before the end)
        if total_out > 3 * segment
            && stream_bounds.iter().all(|&(s, e)| e - s >= 8)
            && scratch.table.max_num_bits >= 1
        {
            decompress_4streams_interleaved(
                &scratch.table,
                source,
                &stream_bounds,
                total_out,
                target,
            )?;
        } else {
            for &(start, end) in &stream_bounds {
                let stream = &source[start..end];
                let mut decoder = HuffmanDecoder::new(&scratch.table);
                let mut br = BitReaderReversed::new(stream);
                //skip the 0 padding at the end of the last byte of the bit stream and throw away the first 1 found
                let mut skipped_bits = 0;
                loop {
                    let val = br.get_bits(1);
                    skipped_bits += 1;
                    if val == 1 || skipped_bits > 8 {
                        break;
                    }
                }
                if skipped_bits > 8 {
                    //if more than 7 bits are 0, this is not the correct end of the bitstream. Either a bug or corrupted data
                    return Err(DecompressLiteralsError::ExtraPadding { skipped_bits });
                }
                decoder.init_state(&mut br);

                while br.bits_remaining() > -(scratch.table.max_num_bits as isize) {
                    target.push(decoder.decode_symbol());
                    decoder.next_state(&mut br);
                }
                if br.bits_remaining() != -(scratch.table.max_num_bits as isize) {
                    return Err(DecompressLiteralsError::BitstreamReadMismatch {
                        read_til: br.bits_remaining(),
                        expected: -(scratch.table.max_num_bits as isize),
                    });
                }
            }
        }

        bytes_read += source.len() as u32;
    } else {
        //just decode the one stream
        assert!(num_streams == 1);
        let mut decoder = HuffmanDecoder::new(&scratch.table);
        let mut br = BitReaderReversed::new(source);
        let mut skipped_bits = 0;
        loop {
            let val = br.get_bits(1);
            skipped_bits += 1;
            if val == 1 || skipped_bits > 8 {
                break;
            }
        }
        if skipped_bits > 8 {
            //if more than 7 bits are 0, this is not the correct end of the bitstream. Either a bug or corrupted data
            return Err(DecompressLiteralsError::ExtraPadding { skipped_bits });
        }
        decoder.init_state(&mut br);
        while br.bits_remaining() > -(scratch.table.max_num_bits as isize) {
            target.push(decoder.decode_and_advance(&mut br));
        }
        bytes_read += source.len() as u32;
    }

    if target.len() != section.regenerated_size as usize {
        return Err(DecompressLiteralsError::DecodedLiteralCountMismatch {
            decoded: target.len(),
            expected: section.regenerated_size as usize,
        });
    }

    Ok(bytes_read)
}

/// Decode four interleaved Huffman streams in parallel (port of the official zstd
/// `HUF_decompress4X1_usingDTable_internal_fast_c_loop`).
///
/// Each stream keeps its bitstream MSB-aligned in a u64 with a 1 marker below the
/// unconsumed bits (`trailing_zeros` then yields the bits consumed since the last
/// reload - no separate counter needed), all four streams advance together with
/// five decoded symbols per iteration, and bounds checks are amortized: the loop
/// bound is precomputed from the minimum of output/5 and input/7 iterations.
///
/// `region` holds the four streams back to back; `stream_bounds` are the (start, end)
/// offsets into `region`. The output is written into `target`, which is resized to
/// hold exactly `total_out` bytes. Every stream must decode exactly its quarter of
/// the output, as guaranteed by the format.
#[allow(clippy::too_many_lines)]
fn decompress_4streams_interleaved(
    table: &HuffmanTable,
    region: &[u8],
    stream_bounds: &[(usize, usize); 4],
    total_out: usize,
    target: &mut Vec<u8>,
) -> Result<(), DecompressLiteralsError> {
    use DecompressLiteralsError as err;

    let tl = table.max_num_bits; // 1..=11
    debug_assert!((1..=11).contains(&tl));
    let shift = 64 - tl as u32;
    let packed = table.packed_table();
    debug_assert_eq!(packed.len(), 1 << tl);

    // per-stream output segments: stream k ends at (k+1) * segment, stream 4 at total_out
    let segment = total_out.div_ceil(4);
    let seg_end = [segment, 2 * segment, 3 * segment, total_out];

    // initialize the four bit windows. Reading is backwards from the stream end;
    // the padding and final 1 marker of the last byte are shifted out.
    let mut ip = [0usize; 4];
    let mut bits = [0u64; 4];
    for s in 0..4 {
        let end = stream_bounds[s].1;
        let p = end - 8;
        let last_byte = region[p + 7];
        let skip = if last_byte != 0 {
            1 + last_byte.leading_zeros()
        } else {
            0
        };
        ip[s] = p;
        bits[s] = (u64::from_le_bytes(region[p..][..8].try_into().unwrap()) | 1) << skip;
    }
    // stream k starts writing at k * segment (stream 4 takes the remainder)
    let mut op = [0, segment, 2 * segment, 3 * segment];

    let out_start = target.len();
    target.resize(out_start + total_out, 0);
    let out = &mut target[out_start..];

    macro_rules! decode_sym {
        ($s:literal, $k:literal) => {{
            let entry = packed[(bits[$s] >> shift) as usize];
            out[op[$s] + $k] = (entry >> 8) as u8;
            bits[$s] <<= entry & 0x3F;
        }};
    }
    macro_rules! reload {
        ($s:literal) => {{
            let ctz = bits[$s].trailing_zeros() as usize;
            ip[$s] -= ctz >> 3;
            bits[$s] =
                (u64::from_le_bytes(region[ip[$s]..][..8].try_into().unwrap()) | 1) << (ctz & 7);
            op[$s] += 5;
        }};
    }

    loop {
        // Each iteration produces 5 output symbols per stream and consumes at most
        // 7 bytes (11 bits * 5 = 55 bits) per stream. Run only as many iterations as
        // both bounds safely allow, then re-check.
        let iters = (ip[0] / 7).min((total_out - op[3]) / 5);
        if iters == 0 {
            break;
        }
        // A stream crossing below its predecessor indicates corruption
        if ip[1] < ip[0] || ip[2] < ip[1] || ip[3] < ip[2] {
            return Err(err::BitstreamReadMismatch {
                read_til: 0,
                expected: -(tl as isize),
            });
        }
        let olimit = op[3] + iters * 5;

        loop {
            // Decode 5 symbols in each of the 4 streams, fully unrolled so the
            // four streams stay in registers and their independent loads overlap.
            decode_sym!(0, 0);
            decode_sym!(1, 0);
            decode_sym!(2, 0);
            decode_sym!(3, 0);
            decode_sym!(0, 1);
            decode_sym!(1, 1);
            decode_sym!(2, 1);
            decode_sym!(3, 1);
            decode_sym!(0, 2);
            decode_sym!(1, 2);
            decode_sym!(2, 2);
            decode_sym!(3, 2);
            decode_sym!(0, 3);
            decode_sym!(1, 3);
            decode_sym!(2, 3);
            decode_sym!(3, 3);
            decode_sym!(0, 4);
            decode_sym!(1, 4);
            decode_sym!(2, 4);
            decode_sym!(3, 4);

            reload!(0);
            reload!(1);
            reload!(2);
            reload!(3);

            if op[3] >= olimit {
                break;
            }
        }
    }

    // Finish each stream with the scalar decoder, bounded by its segment end.
    for s in 0..4 {
        let (start, end) = stream_bounds[s];
        // The window may reach at most 8 bytes below the stream start (bytes already
        // consumed); anything lower means the stream over-consumed => corruption
        if ip[s] + 8 < start {
            return Err(err::BitstreamReadMismatch {
                read_til: 0,
                expected: -(tl as isize),
            });
        }
        if op[s] > seg_end[s] {
            return Err(err::DecodedLiteralCountMismatch {
                decoded: op[s],
                expected: seg_end[s],
            });
        }
        // Hand the window state back to a regular bit reader. The slice may start up
        // to 8 bytes below the stream (those foreign bytes are already consumed);
        // account for them when checking the remaining bits.
        let ext = start.saturating_sub(8);
        let foreign = (8 * (start - ext)) as isize;
        let slice = &region[ext..end];
        let ctz = bits[s].trailing_zeros() as u8;
        let mut br = BitReaderReversed::from_parts(slice, ip[s] - ext, ctz, 0);

        let mut decoder = HuffmanDecoder::new(table);
        decoder.init_state(&mut br);
        let mut o = op[s];
        while o < seg_end[s] && br.bits_remaining() - foreign > -(tl as isize) {
            out[o] = decoder.decode_and_advance(&mut br);
            o += 1;
        }
        if o != seg_end[s] || br.bits_remaining() - foreign != -(tl as isize) {
            return Err(err::BitstreamReadMismatch {
                read_til: br.bits_remaining() - foreign,
                expected: -(tl as isize),
            });
        }
    }

    Ok(())
}
