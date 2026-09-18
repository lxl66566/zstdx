//! This module contains the decompress_literals function, used to take a
//! parsed literals header and a source and decompress it.

use alloc::vec::Vec;
use core::convert::TryInto;

use super::{
    super::blocks::literals_section::{LiteralsSection, LiteralsSectionType},
    scratch::HuffmanScratch,
};
use crate::{
    bit_io::BitReaderReversed,
    decoding::errors::DecompressLiteralsError,
    huff0::{HuffmanDecoder, HuffmanTable},
};

/// Decode and decompress the provided literals section into `target`, returning the number of bytes
/// read.
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
        },
        LiteralsSectionType::RLE => {
            target.resize(target.len() + section.regenerated_size as usize, source[0]);
            Ok(1)
        },
        LiteralsSectionType::Compressed | LiteralsSectionType::Treeless => {
            let bytes_read = decompress_literals(section, scratch, source, target)?;

            // return sum of used bytes
            Ok(bytes_read)
        },
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

    let start_len = target.len();
    let compressed_size = section.compressed_size.ok_or(err::MissingCompressedSize)? as usize;
    let num_streams = section.num_streams.ok_or(err::MissingNumStreams)?;

    target.reserve(section.regenerated_size as usize);
    let source = &source[0..compressed_size];
    let mut bytes_read = 0;

    match section.ls_type {
        LiteralsSectionType::Compressed => {
            // read Huffman tree description
            bytes_read += scratch.table.build_decoder(source)?;
            // pick the decoding table shape for this block's literals,
            // mirroring libzstd's HUF_selectDecoder cost model
            if select_x2_table(section.regenerated_size as usize, compressed_size) {
                scratch.table.build_x2_table();
            }
            vprintln!("Built huffman table using {} bytes", bytes_read);
        },
        LiteralsSectionType::Treeless if scratch.table.max_num_bits == 0 => {
            return Err(err::UninitializedHuffmanTable);
        },

        _ => { /* nothing to do, huffman tree has been provided by previous block */ },
    }

    let source = &source[bytes_read as usize..];

    if num_streams == 4 {
        // The quarter split needs a non-degenerate fourth segment
        // (libzstd's MIN_LITERALS_FOR_4_STREAMS).
        if section.regenerated_size < 6 {
            return Err(err::LiteralsTooSmallFor4Streams {
                got: section.regenerated_size as usize,
            });
        }
        // build jumptable
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

        // decode 4 streams. The format splits the literals into four equal
        // parts (the fourth may be smaller), so stream k decodes exactly
        // `segment` bytes (the last stream the remainder).
        let total_out = section.regenerated_size as usize;
        let segment = total_out.div_ceil(4);
        let stream_bounds = [
            (0, jump1),
            (jump1, jump2),
            (jump2, jump3),
            (jump3, source.len()),
        ];
        let seg_end = [segment, 2 * segment, 3 * segment, total_out];

        // Fast interleaved path: needs one 8 byte window per stream and enough output
        // for the 4-way interleave to make progress (stream 4 must start before the end)
        if total_out > 3 * segment
            && stream_bounds.iter().all(|&(s, e)| e - s >= 8)
            && scratch.table.max_num_bits >= 1
        {
            if scratch.table.x2_table().is_empty() {
                decompress_4streams_interleaved(
                    &scratch.table,
                    source,
                    &stream_bounds,
                    total_out,
                    target,
                )?;
            } else {
                decompress_4streams_interleaved_x2(
                    &scratch.table,
                    source,
                    &stream_bounds,
                    total_out,
                    target,
                )?;
            }
        } else {
            // Slow path: same per-stream segment contract as the fast path
            // (and libzstd's op/olimit + endOfDStream checks). A stream that
            // encodes past its quarter would shift every later segment's
            // output position, so it is corruption even when the totals
            // still add up.
            let tl = scratch.table.max_num_bits as isize;
            let mut seg_start = 0;
            for (&(start, end), &seg_end) in stream_bounds.iter().zip(seg_end.iter()) {
                let seg_len = seg_end - seg_start;
                let stream = &source[start..end];
                let mut decoder = HuffmanDecoder::new(&scratch.table);
                let mut br = BitReaderReversed::new(stream);
                // skip the 0 padding at the end of the last byte of the bit stream and throw away
                // the first 1 found
                let mut skipped_bits = 0;
                loop {
                    let val = br.get_bits(1);
                    skipped_bits += 1;
                    if val == 1 || skipped_bits > 8 {
                        break;
                    }
                }
                if skipped_bits > 8 {
                    // if more than 7 bits are 0, this is not the correct end of the bitstream.
                    // Either a bug or corrupted data
                    return Err(DecompressLiteralsError::ExtraPadding { skipped_bits });
                }
                decoder.init_state(&mut br);

                let mut produced = 0;
                while produced < seg_len && br.bits_remaining() > -tl {
                    target.push(decoder.decode_symbol());
                    decoder.next_state(&mut br);
                    produced += 1;
                }
                if produced != seg_len {
                    return Err(DecompressLiteralsError::DecodedLiteralCountMismatch {
                        decoded: produced,
                        expected: seg_len,
                    });
                }
                if br.bits_remaining() != -tl {
                    return Err(DecompressLiteralsError::BitstreamReadMismatch {
                        read_til: br.bits_remaining(),
                        expected: -tl,
                    });
                }
                seg_start = seg_end;
            }
        }

        bytes_read += source.len() as u32;
    } else {
        // just decode the one stream
        assert_eq!(num_streams, 1);
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
            // if more than 7 bits are 0, this is not the correct end of the bitstream. Either a bug
            // or corrupted data
            return Err(DecompressLiteralsError::ExtraPadding { skipped_bits });
        }
        decoder.init_state(&mut br);
        while br.bits_remaining() > -(scratch.table.max_num_bits as isize) {
            target.push(decoder.decode_and_advance(&mut br));
        }
        bytes_read += source.len() as u32;
    }

    if target.len() - start_len != section.regenerated_size as usize {
        return Err(DecompressLiteralsError::DecodedLiteralCountMismatch {
            decoded: target.len() - start_len,
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

    // Stream state runs on raw pointers (see the X2 loop): folding the
    // region/out bases into ip[]/op[] keeps the fast loop's live values down
    // at 14 so the reload section stays spill-free.

    // initialize the four bit windows. Reading is backwards from the stream end;
    // the padding and final 1 marker of the last byte are shifted out.
    let base = region.as_ptr();
    let mut ip = [base; 4];
    let mut bits = [0u64; 4];
    for s in 0..4 {
        let p = stream_bounds[s].1 - 8;
        let last_byte = region[p + 7];
        let skip = if last_byte != 0 {
            1 + last_byte.leading_zeros()
        } else {
            0
        };
        ip[s] = unsafe { base.add(p) };
        bits[s] = (u64::from_le_bytes(region[p..][..8].try_into().unwrap()) | 1) << skip;
    }

    let out_start = target.len();
    target.resize(out_start + total_out, 0);
    let out = &mut target[out_start..];
    // stream k starts writing at k * segment (stream 4 takes the remainder)
    let out_base = out.as_mut_ptr();
    let mut op = [
        out_base,
        unsafe { out_base.add(segment) },
        unsafe { out_base.add(2 * segment) },
        unsafe { out_base.add(3 * segment) },
    ];
    let out_end = unsafe { out_base.add(total_out) };

    macro_rules! decode_sym {
        ($s:literal, $k:literal) => {{
            // SAFETY: `bits >> shift` is below 2^tl == packed.len() by
            // construction, and the write lands below the iteration bound
            // precomputed from the output size.
            let entry = unsafe { *packed.get_unchecked((bits[$s] >> shift) as usize) };
            unsafe { *op[$s].add($k) = (entry >> 8) as u8 };
            bits[$s] <<= entry & 0x3f;
        }};
    }
    macro_rules! reload {
        ($s:literal) => {{
            let ctz = bits[$s].trailing_zeros() as usize;
            // SAFETY: each reload consumes at most 7 bytes and the iteration
            // count is bounded by ip[0]'s distance to the region start, so
            // the 8 byte read never crosses below `region` (streams sit
            // back to back inside it).
            unsafe {
                ip[$s] = ip[$s].sub(ctz >> 3);
                bits[$s] = (ip[$s].cast::<u64>().read_unaligned() | 1) << (ctz & 7);
                op[$s] = op[$s].add(5);
            }
        }};
    }

    loop {
        // Each iteration produces 5 output symbols per stream and consumes at most
        // 7 bytes (11 bits * 5 = 55 bits) per stream. Run only as many iterations as
        // both bounds safely allow, then re-check.
        let iters =
            ((ip[0] as usize - base as usize) / 7).min((out_end as usize - op[3] as usize) / 5);
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
        let olimit = unsafe { op[3].add(iters * 5) };

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

    // hand the pointer state back to the scalar tail as region/out offsets
    let ip = ip.map(|p| p as usize - base as usize);
    let op = op.map(|p| p as usize - out_base as usize);

    // Finish each stream with the scalar decoder, bounded by its segment end.
    finish_streams(table, region, stream_bounds, &seg_end, &ip, &bits, &op, out)?;

    Ok(())
}

/// Per-stream tail of the interleaved fast loops: hands the u64 window state
/// (bit position `ip[s]`, marker at `bits[s].trailing_zeros()`) back to a
/// regular bit reader and decodes the remaining symbols of the stream's
/// segment one at a time through the single-symbol table.
#[allow(clippy::too_many_arguments)]
fn finish_streams(
    table: &HuffmanTable,
    region: &[u8],
    stream_bounds: &[(usize, usize); 4],
    seg_end: &[usize; 4],
    ip: &[usize; 4],
    bits: &[u64; 4],
    op: &[usize; 4],
    out: &mut [u8],
) -> Result<(), DecompressLiteralsError> {
    use DecompressLiteralsError as err;

    let tl = table.max_num_bits as isize;
    for s in 0..4 {
        let (start, end) = stream_bounds[s];
        // The window may reach at most 8 bytes below the stream start (bytes already
        // consumed); anything lower means the stream over-consumed => corruption
        if ip[s] + 8 < start {
            return Err(err::BitstreamReadMismatch {
                read_til: 0,
                expected: -tl,
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
        while o < seg_end[s] && br.bits_remaining() - foreign > -tl {
            out[o] = decoder.decode_and_advance(&mut br);
            o += 1;
        }
        if o != seg_end[s] || br.bits_remaining() - foreign != -tl {
            return Err(err::BitstreamReadMismatch {
                read_til: br.bits_remaining() - foreign,
                expected: -tl,
            });
        }
    }

    Ok(())
}

/// Decide between the single-symbol (X1) and double-symbol (X2) huffman
/// decoding table from the literals section sizes. Port of libzstd's
/// `HUF_selectDecoder` / `algoTime`: Q quantizes the compression ratio of the
/// literals, and the two cost models trade the (higher) X2 table build time
/// against its (lower) per-256-bytes decode time; X2 additionally gets a
/// slight penalty for its larger memory footprint.
fn select_x2_table(dst_size: usize, c_src_size: usize) -> bool {
    /// (table_time, decode_time_per_256_bytes) for [X1, X2] per ratio quant Q.
    #[rustfmt::skip]
    const ALGO_TIME: [[(u32, u32); 2]; 16] = [
        [(0, 0), (1, 1)], [(0, 0), (1, 1)],
        [(150, 216), (381, 119)], [(170, 205), (514, 112)],
        [(177, 199), (539, 110)], [(197, 194), (644, 107)],
        [(221, 192), (735, 107)], [(256, 189), (881, 106)],
        [(359, 188), (1167, 109)], [(582, 187), (1570, 114)],
        [(688, 187), (1712, 122)], [(825, 186), (1965, 136)],
        [(976, 185), (2131, 150)], [(1180, 186), (2070, 175)],
        [(1377, 185), (1731, 202)], [(1412, 185), (1695, 202)],
    ];

    let q = if c_src_size >= dst_size {
        15
    } else {
        (c_src_size * 16 / dst_size).min(15)
    };
    let d256 = (dst_size >> 8) as u32;
    let t0 = ALGO_TIME[q][0].0 + ALGO_TIME[q][0].1 * d256;
    let t1 = ALGO_TIME[q][1].0 + ALGO_TIME[q][1].1 * d256;
    let t1 = t1 + (t1 >> 5);
    t1 < t0
}

/// Double-symbol variant of the interleaved 4-stream decoder, port of libzstd's
/// `HUF_decompress4X2_usingDTable_internal_fast_c_loop`.
///
/// Each lookup produces one or two literal bytes (written as a single unaligned
/// u16) and consumes the summed bit count, roughly halving the
/// load→shift→load dependency chain per decoded byte on skewed distributions.
/// The iteration bound takes the minimum over all four output segments
/// (each iteration writes at most 10 bytes per stream), which also keeps every
/// u16 write at least one byte away from the segment end: the four writes
/// preceding a lookup advance by at most 8, so a lookup starting within an
/// iteration never starts at `seg_end - 1` and cannot spill into the next
/// stream's territory. The final byte of each stream is written by the scalar
/// finish loop.
#[allow(clippy::too_many_lines)]
fn decompress_4streams_interleaved_x2(
    table: &HuffmanTable,
    region: &[u8],
    stream_bounds: &[(usize, usize); 4],
    total_out: usize,
    target: &mut Vec<u8>,
) -> Result<(), DecompressLiteralsError> {
    use DecompressLiteralsError as err;

    let dt = table.x2_table();
    debug_assert_eq!(dt.len(), 1 << 11);

    // per-stream output segments: stream k ends at (k+1) * segment, stream 4 at total_out
    let segment = total_out.div_ceil(4);
    let seg_end = [segment, 2 * segment, 3 * segment, total_out];

    // Stream state is kept as raw pointers instead of offsets: folding the
    // region/out bases into ip[]/op[] leaves 14 live values in the fast loop
    // (bits x4, ip x4, op x4, dt), which fits the GPR file and keeps the
    // reload section spill-free.

    // initialize the four bit windows (same layout as the X1 loop)
    let base = region.as_ptr();
    let mut ip = [base; 4];
    let mut bits = [0u64; 4];
    for s in 0..4 {
        let p = stream_bounds[s].1 - 8;
        let last_byte = region[p + 7];
        let skip = if last_byte != 0 {
            1 + last_byte.leading_zeros()
        } else {
            0
        };
        ip[s] = unsafe { base.add(p) };
        bits[s] = (u64::from_le_bytes(region[p..][..8].try_into().unwrap()) | 1) << skip;
    }

    let out_start = target.len();
    target.resize(out_start + total_out, 0);
    let out = &mut target[out_start..];
    let out_base = out.as_mut_ptr();
    let mut op = [
        out_base,
        unsafe { out_base.add(segment) },
        unsafe { out_base.add(2 * segment) },
        unsafe { out_base.add(3 * segment) },
    ];
    let oend = [
        unsafe { out_base.add(seg_end[0]) },
        unsafe { out_base.add(seg_end[1]) },
        unsafe { out_base.add(seg_end[2]) },
        unsafe { out_base.add(seg_end[3]) },
    ];

    macro_rules! decode_sym_x2 {
        ($s:literal) => {{
            // SAFETY: `bits >> 53` is below 2^11 == dt.len() by construction.
            // The write stays inside the stream's segment: see the function
            // level comment, and `op[s]` is advanced by the entry length so it
            // never passes `seg_end[s]`.
            let entry = unsafe { *dt.get_unchecked((bits[$s] >> 53) as usize) };
            unsafe {
                (op[$s].cast::<u16>()).write_unaligned(entry as u16);
                op[$s] = op[$s].add((entry >> 24) as usize);
            }
            bits[$s] <<= (entry >> 16) & 0x3f;
        }};
    }
    macro_rules! reload_x2 {
        ($s:literal) => {{
            let ctz = bits[$s].trailing_zeros() as usize;
            // SAFETY: same reload discipline as the X1 loop - each reload
            // consumes at most 7 bytes and the iteration count is bounded by
            // ip[0]'s distance to the region start, so the 8 byte read stays
            // inside `region` (streams sit back to back).
            unsafe {
                ip[$s] = ip[$s].sub(ctz >> 3);
                bits[$s] = (ip[$s].cast::<u64>().read_unaligned() | 1) << (ctz & 7);
            }
        }};
    }

    loop {
        // Each iteration performs 5 lookups per stream: at most 55 input bits
        // (7 bytes) and at most 10 output bytes per stream. Bound iterations by
        // the input left in the first stream and by EVERY stream's remaining
        // output segment.
        let iters = ((ip[0] as usize - base as usize) / 7)
            .min((oend[0] as usize - op[0] as usize) / 10)
            .min((oend[1] as usize - op[1] as usize) / 10)
            .min((oend[2] as usize - op[2] as usize) / 10)
            .min((oend[3] as usize - op[3] as usize) / 10);
        if iters == 0 {
            break;
        }
        // A stream crossing below its predecessor indicates corruption
        if ip[1] < ip[0] || ip[2] < ip[1] || ip[3] < ip[2] {
            return Err(err::BitstreamReadMismatch {
                read_til: 0,
                expected: -(table.max_num_bits as isize),
            });
        }
        // each iteration advances op[3] by at least 5 bytes
        let olimit = unsafe { op[3].add(iters * 5) };

        loop {
            // Decode 5 lookups in each of the 4 streams, column-major so the
            // four dependency chains interleave.
            decode_sym_x2!(0);
            decode_sym_x2!(1);
            decode_sym_x2!(2);
            decode_sym_x2!(3);
            decode_sym_x2!(0);
            decode_sym_x2!(1);
            decode_sym_x2!(2);
            decode_sym_x2!(3);
            decode_sym_x2!(0);
            decode_sym_x2!(1);
            decode_sym_x2!(2);
            decode_sym_x2!(3);
            decode_sym_x2!(0);
            decode_sym_x2!(1);
            decode_sym_x2!(2);
            decode_sym_x2!(3);
            decode_sym_x2!(0);
            decode_sym_x2!(1);
            decode_sym_x2!(2);
            decode_sym_x2!(3);

            reload_x2!(0);
            reload_x2!(1);
            reload_x2!(2);
            reload_x2!(3);

            if op[3] >= olimit {
                break;
            }
        }
    }

    // hand the pointer state back to the scalar tail as region/out offsets
    let ip = ip.map(|p| p as usize - base as usize);
    let op = op.map(|p| p as usize - out_base as usize);
    finish_streams(table, region, stream_bounds, &seg_end, &ip, &bits, &op, out)?;

    Ok(())
}

#[cfg(test)]
mod tests {
    use alloc::vec::Vec;

    use super::decode_literals;
    use crate::{
        blocks::literals_section::{LiteralsSection, LiteralsSectionType},
        decoding::{errors::DecompressLiteralsError, scratch::HuffmanScratch},
    };

    // Minimal huffman table description: direct weights [2, 1]. Sum 3 ->
    // max_num_bits 2, the implicit last weight closes the table. Symbol 0
    // owns a 1-bit code, so a stream of 1-bits decodes to symbol 0 at one
    // bit per symbol.
    const TABLE: [u8; 2] = [0x81, 0x21];

    /// A 1-byte stream that decodes `symbols` copies of symbol 0 and then
    /// exhausts exactly: `7 - symbols` padding zeros, the end marker, then
    /// `symbols` one-bits, read MSB-first.
    fn stream_byte(symbols: usize) -> u8 {
        assert!((1..=6).contains(&symbols));
        ((1usize << (symbols + 1)) - 1) as u8
    }

    /// A 4-stream literals section whose streams are 1 byte each (below the
    /// interleaved fast path's 8-byte minimum, so the slow path runs).
    fn four_stream_section(regenerated: u32, streams: [u8; 4]) -> (LiteralsSection, Vec<u8>) {
        let mut source = Vec::new();
        source.extend_from_slice(&TABLE);
        source.extend_from_slice(&[1, 0, 1, 0, 1, 0]); // jumptable: 3 streams of 1 byte
        source.extend_from_slice(&streams);
        let section = LiteralsSection {
            regenerated_size: regenerated,
            compressed_size: Some(source.len() as u32),
            num_streams: Some(4),
            ls_type: LiteralsSectionType::Compressed,
        };
        (section, source)
    }

    fn decode(
        scratch: &mut HuffmanScratch,
        section: &LiteralsSection,
        source: &[u8],
    ) -> Result<Vec<u8>, DecompressLiteralsError> {
        let mut target = Vec::new();
        decode_literals(section, scratch, source, &mut target)?;
        Ok(target)
    }

    #[test]
    fn slow_path_accepts_well_formed_streams() {
        let (section, source) = four_stream_section(8, [stream_byte(2); 4]);
        let mut scratch = HuffmanScratch::new();
        let out = decode(&mut scratch, &section, &source).unwrap();
        assert_eq!(out.len(), 8);
        assert!(out.iter().all(|&b| b == 0), "every symbol is 0");
    }

    #[test]
    fn slow_path_rejects_stream_decoding_past_its_quarter() {
        // Stream 1 encodes 3 symbols, stream 3 only 1: totals still add up to
        // the regenerated size, but the per-segment layout is broken (the old
        // total-only check accepted this).
        let (section, source) = four_stream_section(8, [
            stream_byte(3),
            stream_byte(2),
            stream_byte(1),
            stream_byte(2),
        ]);
        let mut scratch = HuffmanScratch::new();
        match decode(&mut scratch, &section, &source) {
            Err(DecompressLiteralsError::BitstreamReadMismatch { .. }) => {},
            other => panic!("expected BitstreamReadMismatch, got {other:?}"),
        }
    }

    #[test]
    fn slow_path_rejects_stream_decoding_short_of_its_quarter() {
        let (section, source) = four_stream_section(8, [
            stream_byte(1),
            stream_byte(2),
            stream_byte(3),
            stream_byte(2),
        ]);
        let mut scratch = HuffmanScratch::new();
        match decode(&mut scratch, &section, &source) {
            Err(DecompressLiteralsError::DecodedLiteralCountMismatch {
                decoded: 1,
                expected: 2,
            }) => {},
            other => panic!("expected DecodedLiteralCountMismatch(1, 2), got {other:?}"),
        }
    }

    #[test]
    fn four_streams_reject_regenerated_size_below_six() {
        // The quarter split is degenerate below 6 bytes (libzstd's
        // MIN_LITERALS_FOR_4_STREAMS); reject before touching the streams.
        let (section, source) = four_stream_section(4, [stream_byte(1); 4]);
        let mut scratch = HuffmanScratch::new();
        match decode(&mut scratch, &section, &source) {
            Err(DecompressLiteralsError::LiteralsTooSmallFor4Streams { got: 4 }) => {},
            other => panic!("expected LiteralsTooSmallFor4Streams, got {other:?}"),
        }
    }
}
