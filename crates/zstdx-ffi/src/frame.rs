//! Minimal frame walker for the metadata entry points and the streaming
//! decoder's input staging: frame-header parsing (magic, descriptor, FCS,
//! dictID, checksum flag), skippable-frame detection, and the block walk
//! behind `ZSTD_findFrameCompressedSize`. Read-only, bounds-checked, and
//! independent of the codec so it can answer without touching decoder state.

/// Serialized zstd frame magic number.
pub const MAGIC: u32 = 0xfd2f_b528;
/// Skippable-frame magic range (`0x184D2A50..=0x184D2A5F`).
const SKIPPABLE_MIN: u32 = 0x184d_2a50;
const SKIPPABLE_MAX: u32 = 0x184d_2a5f;
/// Format block-content maximum; the block header's 21-bit size field may
/// declare more, which is malformed.
const MAX_BLOCK_CONTENT: usize = 128 * 1024;

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
    if src.len() < 5 {
        return Err(ParseError::NeedMore);
    }
    let descriptor = src[4];
    let fcs_flag = descriptor >> 6;
    let single_segment = descriptor & 0x20 != 0;
    let mut len = 5usize;
    if !single_segment {
        if src.len() < len {
            return Err(ParseError::NeedMore);
        }
        // Window descriptor: irrelevant to the metadata answers.
        len += 1;
    }
    let dict_len = match descriptor & 3 {
        0 => 0,
        1 | 2 => usize::from(descriptor & 3),
        _ => 4,
    };
    if src.len() < len + dict_len {
        return Err(ParseError::NeedMore);
    }
    // The Dictionary_ID field is parsed only to advance; the decoder
    // resolves dictionary requirements itself.
    len += dict_len;

    // FCS field size: flag 0 means 0 bytes, except single-segment frames
    // (no window descriptor) where it carries the size in one byte.
    let fcs_len = match fcs_flag {
        0 if !single_segment => 0,
        0 => 1,
        1 => 2,
        2 => 4,
        _ => 8,
    };
    if src.len() < len + fcs_len {
        return Err(ParseError::NeedMore);
    }
    let content_size = if fcs_len > 0 {
        let mut bytes = [0u8; 8];
        bytes[..fcs_len].copy_from_slice(&src[len..len + fcs_len]);
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

    Ok(Header::Zstd(HeaderInfo {
        header_len: len + fcs_len,
        content_size,
        // Content_Checksum_flag is descriptor bit 2 (RFC 8878 3.1.1.1).
        checksum: descriptor & 0x04 != 0,
    }))
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
    }

    #[test]
    fn skippable_and_bad_magic() {
        let skip = [0x50, 0x2a, 0x4d, 0x18, 4, 0, 0, 0, 1, 2, 3, 4];
        assert_eq!(parse_header(&skip).unwrap(), Header::Skippable {
            total: 12
        });
        assert_eq!(frame_compressed_size(&skip).unwrap(), 12);
        assert_eq!(
            parse_header(&[0u8, 1, 2, 3]).unwrap_err(),
            ParseError::BadMagic
        );
        assert_eq!(parse_header(&[0u8, 1]).unwrap_err(), ParseError::NeedMore);
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
}
