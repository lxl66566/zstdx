//! Minimal frame walker for the metadata entry points and the streaming
//! decoder's input staging: frame-header parsing (magic, descriptor, FCS,
//! dictID, checksum flag, window size), skippable-frame detection, and the
//! block walk behind `ZSTD_findFrameCompressedSize`. Read-only,
//! bounds-checked, and independent of the codec so it can answer without
//! touching decoder state.

/// Serialized zstd frame magic number.
pub const MAGIC: u32 = 0xfd2f_b528;
/// Skippable-frame magic range (`0x184D2A50..=0x184D2A5F`).
const SKIPPABLE_MIN: u32 = 0x184d_2a50;
const SKIPPABLE_MAX: u32 = 0x184d_2a5f;
/// Format block-content maximum; the block header's 21-bit size field may
/// declare more, which is malformed.
const MAX_BLOCK_CONTENT: usize = 128 * 1024;
/// `ZSTD_FRAMEHEADERSIZE_PREFIX` (zstd1): the least input the header-size
/// arithmetic and the early magic check need.
pub const HEADER_PREFIX: usize = 5;
/// `ZSTD_SKIPPABLEHEADERSIZE`.
const SKIPPABLE_HEADER: usize = 8;
/// `ZSTD_WINDOWLOG_MAX` on 64-bit targets — the getFrameHeader bound (the
/// decoder's default window limit is tighter).
const WINDOWLOG_MAX: u32 = 31;
/// The window-descriptor exponent bias: `windowLog = 10 + (byte >> 3)`.
const WINDOWLOG_BASE: u32 = 10;

/// The parsed head of a frame.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HeaderInfo {
    /// Serialized length of the frame header, magic included.
    pub header_len: usize,
    /// `Frame_Content_Size` when the header declares one.
    pub content_size: Option<u64>,
    /// Content-checksum flag.
    pub checksum: bool,
}

/// What sits at the start of the input.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Header {
    Zstd(HeaderInfo),
    /// A skippable frame; `total` is its whole serialized length
    /// (magic + size field + content).
    Skippable {
        total: usize,
    },
}

/// Why parsing stopped.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ParseError {
    /// The input ended inside the structure being parsed.
    NeedMore,
    /// Neither a zstd nor a skippable-frame magic number.
    BadMagic,
    /// The bytes are a frame head but violate the format.
    Malformed,
    /// The reserved descriptor bit is set (`frameParameter_unsupported`).
    ReservedBit,
    /// The window descriptor exceeds the format's window ceiling
    /// (`frameParameter_windowTooLarge`).
    WindowTooLarge,
}

/// The zstd frame header at the start of `src`: descriptor, optional window
/// byte, Dictionary_ID and FCS fields. Validation order matches libzstd —
/// the full serialized header must be present before the descriptor
/// constraints are judged.
fn parse_zstd_header(src: &[u8]) -> Result<HeaderInfo, ParseError> {
    if src.len() < HEADER_PREFIX {
        return Err(ParseError::NeedMore);
    }
    let descriptor = src[4];
    let fcs_flag = descriptor >> 6;
    let single_segment = descriptor & 0x20 != 0;
    let dict_len = match descriptor & 3 {
        0 => 0,
        1 | 2 => usize::from(descriptor & 3),
        _ => 4,
    };
    // FCS field size: flag 0 means 0 bytes, except single-segment frames
    // (no window descriptor) where it carries the size in one byte.
    let fcs_len = match fcs_flag {
        0 if !single_segment => 0,
        0 => 1,
        1 => 2,
        2 => 4,
        _ => 8,
    };
    let header_len = HEADER_PREFIX + usize::from(!single_segment) + dict_len + fcs_len;
    if src.len() < header_len {
        return Err(ParseError::NeedMore);
    }
    if descriptor & 0x08 != 0 {
        // Reserved bit: libzstd's header parse rejects it outright.
        return Err(ParseError::ReservedBit);
    }
    if !single_segment {
        let exponent = u32::from(src[5] >> 3);
        if WINDOWLOG_BASE + exponent > WINDOWLOG_MAX {
            return Err(ParseError::WindowTooLarge);
        }
    }
    let mut pos = HEADER_PREFIX;
    if !single_segment {
        pos += 1; // window descriptor
    }
    let mut dict_bytes = [0u8; 4];
    dict_bytes[..dict_len].copy_from_slice(&src[pos..pos + dict_len]);
    pos += dict_len;
    let content_size = if fcs_len > 0 {
        let mut bytes = [0u8; 8];
        bytes[..fcs_len].copy_from_slice(&src[pos..pos + fcs_len]);
        let value = u64::from_le_bytes(bytes);
        // Flag 1 stores the value minus 256.
        Some(if fcs_len == 2 {
            value + 256
        } else {
            value
        })
    } else {
        None
    };
    Ok(HeaderInfo {
        header_len,
        content_size,
        // Content_Checksum_flag is descriptor bit 2 (RFC 8878 3.1.1.1).
        checksum: descriptor & 0x04 != 0,
    })
}

/// Parse the frame header at the start of `src` (skippable frames included).
pub fn parse_header(src: &[u8]) -> Result<Header, ParseError> {
    if src.len() < 4 {
        return Err(ParseError::NeedMore);
    }
    let magic = u32::from_le_bytes(src[..4].try_into().expect("4 bytes checked"));
    if (SKIPPABLE_MIN..=SKIPPABLE_MAX).contains(&magic) {
        if src.len() < 8 {
            return Err(ParseError::NeedMore);
        }
        let content = u32::from_le_bytes(src[4..8].try_into().expect("4 bytes checked"));
        return Ok(Header::Skippable {
            total: 8 + content as usize,
        });
    }
    if magic != MAGIC {
        return Err(ParseError::BadMagic);
    }
    Ok(Header::Zstd(parse_zstd_header(src)?))
}

/// The complete-header answer for `ZSTD_getFrameHeader`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FullHeader {
    pub frame_type: FrameType,
    /// The header-declared content size; `None` when absent (the FFI maps
    /// it to `ZSTD_CONTENTSIZE_UNKNOWN`).
    pub content_size: Option<u64>,
    /// The window the frame requires (the FCS on single-segment frames).
    pub window_size: u64,
    /// `min(window_size, 128 KiB)`, the largest block content allowed.
    pub block_size_max: u64,
    pub header_size: usize,
    /// The dictID field; for skippable frames the magic variant 0..=15.
    pub dict_id: u32,
    pub checksum: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FrameType {
    Zstd,
    Skippable,
}

/// The `ZSTD_getFrameHeader` contract: the filled header, or the total
/// input size the parser still wants (any valid prefix).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HeaderParse {
    Complete(FullHeader),
    /// The input so far is a valid frame prefix; this many bytes are wanted
    /// in total before the header can be answered.
    Wanted(usize),
}

/// `ZSTD_getFrameHeader` parsing, mirroring libzstd's
/// `ZSTD_getFrameHeader_advanced`: below `ZSTD_FRAMEHEADERSIZE_PREFIX` the
/// magic prefix is validated and 5 is wanted; skippable frames want 8 and
/// then complete; zstd frames want their full serialized header.
pub fn parse_full_header(src: &[u8]) -> Result<HeaderParse, ParseError> {
    if src.len() < 4 {
        // libzstd validates whatever magic bytes it can see before asking
        // for more (a mismatching prefix is an error, not a size request).
        if !src.is_empty() && !magic_prefix_ok(src) {
            return Err(ParseError::BadMagic);
        }
        return Ok(HeaderParse::Wanted(HEADER_PREFIX));
    }
    let magic = u32::from_le_bytes(src[..4].try_into().expect("4 bytes checked"));
    if (SKIPPABLE_MIN..=SKIPPABLE_MAX).contains(&magic) {
        if src.len() < SKIPPABLE_HEADER {
            return Ok(HeaderParse::Wanted(SKIPPABLE_HEADER));
        }
        let content = u32::from_le_bytes(src[4..8].try_into().expect("4 bytes checked"));
        return Ok(HeaderParse::Complete(FullHeader {
            frame_type: FrameType::Skippable,
            content_size: Some(u64::from(content)),
            window_size: 0,
            block_size_max: 0,
            header_size: SKIPPABLE_HEADER,
            dict_id: magic - SKIPPABLE_MIN,
            checksum: false,
        }));
    }
    if magic != MAGIC {
        return Err(ParseError::BadMagic);
    }
    let info = match parse_zstd_header(src) {
        Ok(info) => info,
        // A valid prefix of a longer header: report the formula's total,
        // exactly as libzstd's arithmetic-before-validation order does.
        Err(ParseError::NeedMore) => return Ok(HeaderParse::Wanted(header_size_formula(src[4]))),
        Err(e) => return Err(e),
    };
    let descriptor = src[4];
    let single_segment = descriptor & 0x20 != 0;
    let window_size = if single_segment {
        info.content_size.expect("single segment declares FCS")
    } else {
        window_from_descriptor(src[5])
    };
    let dict_len = match descriptor & 3 {
        0 => 0,
        1 | 2 => usize::from(descriptor & 3),
        _ => 4,
    };
    let dict_pos = HEADER_PREFIX + usize::from(!single_segment);
    let mut dict_bytes = [0u8; 4];
    dict_bytes[..dict_len].copy_from_slice(&src[dict_pos..dict_pos + dict_len]);
    Ok(HeaderParse::Complete(FullHeader {
        frame_type: FrameType::Zstd,
        content_size: info.content_size,
        window_size,
        block_size_max: window_size.min(MAX_BLOCK_CONTENT as u64),
        header_size: info.header_len,
        dict_id: u32::from_le_bytes(dict_bytes),
        checksum: info.checksum,
    }))
}

/// Whether `src` is a prefix of either magic family (libzstd's early check
/// on short inputs).
fn magic_prefix_ok(src: &[u8]) -> bool {
    let magic = MAGIC.to_le_bytes();
    let skip = SKIPPABLE_MIN.to_le_bytes();
    let head = src.len().min(4);
    src[..head] == magic[..head] || src[..head] == skip[..head]
}

/// RFC 8878 window descriptor: `10 + exponent` plus a 3-bit mantissa
/// fraction of the base.
fn window_from_descriptor(byte: u8) -> u64 {
    let exponent = u32::from(byte >> 3);
    let base = 1_u64 << (WINDOWLOG_BASE + exponent);
    base + (base >> 3) * u64::from(byte & 7)
}

/// `ZSTD_frameHeaderSize`'s pure arithmetic: the serialized header length
/// implied by the frame descriptor byte, no magic validation (libzstd's
/// entry reads `src[4]` whatever the magic is).
pub fn header_size_formula(descriptor: u8) -> usize {
    let dict_id = descriptor & 3;
    let single_segment = descriptor >> 5 & 1 != 0;
    let fcs_id = descriptor >> 6;
    let did_len = [0, 1, 2, 4][usize::from(dict_id)];
    let fcs_len = [0, 2, 4, 8][usize::from(fcs_id)] + usize::from(single_segment && fcs_id == 0);
    HEADER_PREFIX + usize::from(!single_segment) + did_len + fcs_len
}

/// Walk the blocks of the first frame in `src` and return its whole
/// serialized size (header, blocks, checksum). Skippable frames return
/// their total size.
pub fn frame_compressed_size(src: &[u8]) -> Result<usize, ParseError> {
    let header = match parse_header(src)? {
        Header::Skippable { total } => {
            return if src.len() >= total {
                Ok(total)
            } else {
                Err(ParseError::NeedMore)
            };
        },
        Header::Zstd(header) => header,
    };
    let mut pos = header.header_len;
    loop {
        if src.len() < pos + 3 {
            return Err(ParseError::NeedMore);
        }
        let [b0, b1, b2, ..] = src[pos..] else {
            return Err(ParseError::NeedMore);
        };
        // last_block: bit 0; block_type: bits 1-2; block_size: bits 3-23.
        let last = b0 & 1 != 0;
        let block_type = (b0 >> 1) & 3;
        let size = (usize::from(b0) >> 3) | (usize::from(b1) << 5) | (usize::from(b2) << 13);
        let body = match block_type {
            0 | 2 => {
                if size > MAX_BLOCK_CONTENT {
                    return Err(ParseError::Malformed);
                }
                size
            },
            // RLE blocks carry one content byte; `size` is the regenerated
            // length and is bounded at decode time by the window.
            1 => 1,
            _ => return Err(ParseError::Malformed),
        };
        pos += 3 + body;
        if src.len() < pos {
            return Err(ParseError::NeedMore);
        }
        if last {
            if header.checksum {
                if src.len() < pos + 4 {
                    return Err(ParseError::NeedMore);
                }
                pos += 4;
            }
            return Ok(pos);
        }
    }
}

/// `ZSTD_findDecompressedSize` walk: the summed FCS of every frame in the
/// series, `None` when any frame's size is unknown, `Err` when the input is
/// not an exact series of complete frames.
pub fn find_decompressed_size(src: &[u8]) -> Result<Option<u64>, ()> {
    let mut total = 0_u64;
    let mut rest = src;
    loop {
        if rest.len() < HEADER_PREFIX {
            return if rest.is_empty() {
                Ok(Some(total))
            } else {
                Err(())
            };
        }
        let Header::Zstd(info) = parse_header(rest).map_err(|_| ())? else {
            // Skippable frames contribute nothing and are skipped whole.
            let Header::Skippable { total: whole } = parse_header(rest).map_err(|_| ())? else {
                unreachable!("narrowed above");
            };
            if rest.len() < whole {
                return Err(());
            }
            rest = &rest[whole..];
            continue;
        };
        if rest.len() < info.header_len {
            return Err(());
        }
        let Some(fcs) = info.content_size else {
            return Ok(None); // an unknown-sized frame makes the series unknown
        };
        total = total.checked_add(fcs).ok_or(())?;
        let whole = frame_compressed_size(rest).map_err(|_| ())?;
        rest = &rest[whole..];
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn compress_default() -> Vec<u8> {
        // A real frame with checksum and pledged size (the FFI one-shot
        // shape).
        let opts = zstdx::EncoderOptions::new(zstdx::Level::Fastest)
            .checksum(true)
            .pledged_size(Some(7));
        let mut sink = Vec::new();
        {
            use std::io::Write as _;
            let mut enc = zstdx::stream::write::Encoder::with_options(&mut sink, opts).unwrap();
            enc.write_all(b"payload").unwrap();
            enc.do_finish().unwrap();
        }
        sink
    }

    #[test]
    fn walks_real_frame() {
        let frame = compress_default();
        let header = match parse_header(&frame).unwrap() {
            Header::Zstd(h) => h,
            other => panic!("unexpected {other:?}"),
        };
        assert_eq!(header.content_size, Some(7));
        assert_eq!(header.checksum, true);
        assert_eq!(frame_compressed_size(&frame).unwrap(), frame.len());
        let full = match parse_full_header(&frame).unwrap() {
            HeaderParse::Complete(h) => h,
            other => panic!("unexpected {other:?}"),
        };
        assert_eq!(full.frame_type, FrameType::Zstd);
        // 7-byte single-segment frame: window == FCS == block max.
        assert_eq!(full.window_size, 7);
        assert_eq!(full.block_size_max, 7);
        assert_eq!(full.header_size, header.header_len);
        assert_eq!(full.content_size, Some(7));
        assert_eq!(full.dict_id, 0);
    }

    #[test]
    fn skippable_and_bad_magic() {
        let skip = [0x50, 0x2a, 0x4d, 0x18, 4, 0, 0, 0, 1, 2, 3, 4];
        assert_eq!(parse_header(&skip).unwrap(), Header::Skippable {
            total: 12
        });
        assert_eq!(frame_compressed_size(&skip).unwrap(), 12);
        let full = match parse_full_header(&skip).unwrap() {
            HeaderParse::Complete(h) => h,
            other => panic!("unexpected {other:?}"),
        };
        assert_eq!(full.frame_type, FrameType::Skippable);
        assert_eq!(full.content_size, Some(4));
        assert_eq!(full.header_size, 8);
        assert_eq!(full.dict_id, 0);
        assert_eq!(
            parse_header(&[0u8, 1, 2, 3]).unwrap_err(),
            ParseError::BadMagic
        );
        assert_eq!(parse_header(&[0u8, 1]).unwrap_err(), ParseError::NeedMore);
        // libzstd's getFrameHeader: short input with a mismatching prefix
        // is an error, with a matching prefix it wants 5.
        assert_eq!(
            parse_full_header(&[1u8, 2, 3]).unwrap_err(),
            ParseError::BadMagic
        );
        assert_eq!(
            parse_full_header(&[0x28u8, 0xb5]).unwrap(),
            HeaderParse::Wanted(5)
        );
        assert_eq!(parse_full_header(&[]).unwrap(), HeaderParse::Wanted(5));
    }

    #[test]
    fn truncated_walk_needs_more() {
        let frame = compress_default();
        for cut in 1..frame.len() {
            match frame_compressed_size(&frame[..cut]) {
                Ok(n) => assert_eq!(n, cut, "only the exact prefix can satisfy the walk"),
                Err(ParseError::NeedMore) | Err(ParseError::Malformed) => {},
                Err(e) => panic!("unexpected {e:?}"),
            }
        }
    }

    #[test]
    fn header_size_formula_matches_descriptor_parse() {
        // For zstd frames the formula must agree with the parsed length.
        let frame = compress_default();
        let header = match parse_header(&frame).unwrap() {
            Header::Zstd(h) => h,
            other => panic!("unexpected {other:?}"),
        };
        assert_eq!(header_size_formula(frame[4]), header.header_len);
        // A windowed frame (pledge off, streaming shape): 4+1+1.
        assert_eq!(header_size_formula(0x00), 6);
        // Single segment, no FCS flag: the 1-byte FCS form.
        assert_eq!(header_size_formula(0x20), 6);
        // Single segment, 8-byte FCS, 4-byte dictID: 5+0+4+8.
        assert_eq!(header_size_formula(0xe3), 17);
        // Skippable parity with libzstd: the formula applies to src[4]
        // unverified (probe: magic 0x184D2A50 + size 4 -> 6).
        assert_eq!(header_size_formula(4), 6);
    }

    #[test]
    fn find_size_sums_and_propagates_unknown() {
        let frame = compress_default();
        let skip = [0x50, 0x2a, 0x4d, 0x18, 4, 0, 0, 0, 1, 2, 3, 4];
        let mut series = frame.clone();
        series.extend_from_slice(&skip);
        assert_eq!(find_decompressed_size(&series), Ok(Some(7)));
        assert_eq!(find_decompressed_size(&[]), Ok(Some(0)));
        // Trailing bytes below the prefix length are an error.
        let mut trailing = frame.clone();
        trailing.extend_from_slice(&[1, 2]);
        assert_eq!(find_decompressed_size(&trailing), Err(()));
        // Truncated frame is an error.
        assert_eq!(find_decompressed_size(&frame[..frame.len() - 1]), Err(()));
    }

    #[test]
    fn reserved_bit_and_window_ceiling() {
        // Reserved descriptor bit 0x08 (header long enough to be judged).
        let frame = [0x28, 0xb5, 0x2f, 0xfd, 0x08, 0x00];
        assert_eq!(parse_header(&frame).unwrap_err(), ParseError::ReservedBit);
        // Window exponent beyond the format ceiling (log 10+31 > 31).
        let frame = [0x28, 0xb5, 0x2f, 0xfd, 0x00, 0xff];
        assert_eq!(
            parse_header(&frame).unwrap_err(),
            ParseError::WindowTooLarge
        );
        // log 10+20 = 30 is inside.
        let frame = [0x28, 0xb5, 0x2f, 0xfd, 0x00, 0xa0];
        assert!(parse_header(&frame).is_ok());
        // Truncated reserved-bit input still wants more, as in libzstd.
        assert_eq!(
            parse_full_header(&[0x28, 0xb5, 0x2f, 0xfd, 0x08]).unwrap(),
            HeaderParse::Wanted(6)
        );
    }

    #[test]
    fn window_descriptor_values() {
        // RFC 8878: exponent 0 mantissa 0 -> 1<<10.
        assert_eq!(window_from_descriptor(0x00), 1024);
        // exponent 1, mantissa 4 (half the base fraction): 2048 + 1024.
        assert_eq!(window_from_descriptor(0x0c), 3072);
        assert_eq!(window_from_descriptor(0xa0), 1 << 30);
    }
}
