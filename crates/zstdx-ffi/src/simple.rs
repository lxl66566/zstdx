//! The simple (context-free) entry points: version constants, one-shot
//! compression and decompression, error accessors, and frame metadata.

use core::ffi::c_char;

use crate::{
    cctx::{CCtx, Settings as CSettings},
    dctx::{DCtx, Settings as DSettings},
    error::{self, ErrorCode},
};

/// Mirrors `ZSTD_VERSION_NUMBER` of the libzstd API this layer implements.
/// Dictionary magic (little-endian `0xEC30A437`), the formatting
/// `zstd --train` writes and `ZSTD_getDictID_fromDict` keys on.
const DICT_MAGIC: [u8; 4] = [0x37, 0xa4, 0x30, 0xec];

pub const VERSION_NUMBER: u32 = 1 * 100 * 100 + 6 * 100;
pub const VERSION_MAJOR: u32 = 1;
pub const VERSION_MINOR: u32 = 6;
pub const VERSION_RELEASE: u32 = 0;
const VERSION_STRING: &[u8] = b"1.6.0\0";

/// `ZSTD_MAX_INPUT_SIZE`: inputs at or above this size have no compress
/// bound (their `size_t` would overflow the formula's margin).
const MAX_INPUT_SIZE: usize = if cfg!(target_pointer_width = "64") {
    0xff00_ff00_ff00_ff00
} else {
    0xff00_ff00
};

/// `ZSTD_maxCLevel` / `ZSTD_minCLevel` / `ZSTD_defaultCLevel`.
pub const MAX_CLEVEL: i32 = 22;
/// -(ZSTD_TARGETLENGTH_MAX) == -ZSTD_BLOCKSIZE_MAX, as in libzstd. Values
/// below -1 clamp to level 1 here (no accelerated levels).
pub const MIN_CLEVEL: i32 = -(128 * 1024);
pub const DEFAULT_CLEVEL: i32 = 3;

/// `ZSTD_COMPRESSBOUND(srcSize)`, verbatim: the margin formula keeps the
/// bound subadditive above 128 KiB and padded below.
#[must_use]
pub fn compress_bound(src_size: usize) -> usize {
    if src_size >= MAX_INPUT_SIZE {
        return 0;
    }
    let margin = if src_size < 128 * 1024 {
        (128 * 1024 - src_size) >> 11
    } else {
        0
    };
    src_size + (src_size >> 8) + margin
}

/// Build the `size_t` return for an `ErrorCode`.
pub(crate) fn ret(code: ErrorCode) -> usize {
    error::code(code)
}

// ---------------------------------------------------------------------------
// Version
// ---------------------------------------------------------------------------

#[unsafe(no_mangle)]
pub extern "C" fn ZSTD_versionNumber() -> core::ffi::c_uint {
    VERSION_NUMBER
}

#[unsafe(no_mangle)]
pub extern "C" fn ZSTD_versionString() -> *const c_char {
    VERSION_STRING.as_ptr().cast()
}

#[unsafe(no_mangle)]
pub extern "C" fn ZSTD_versionMajor() -> core::ffi::c_uint {
    VERSION_MAJOR
}

#[unsafe(no_mangle)]
pub extern "C" fn ZSTD_versionMinor() -> core::ffi::c_uint {
    VERSION_MINOR
}

#[unsafe(no_mangle)]
pub extern "C" fn ZSTD_versionRelease() -> core::ffi::c_uint {
    VERSION_RELEASE
}

#[unsafe(no_mangle)]
pub extern "C" fn ZSTD_maxCLevel() -> core::ffi::c_int {
    MAX_CLEVEL
}

#[unsafe(no_mangle)]
pub extern "C" fn ZSTD_minCLevel() -> core::ffi::c_int {
    MIN_CLEVEL
}

#[unsafe(no_mangle)]
pub extern "C" fn ZSTD_defaultCLevel() -> core::ffi::c_int {
    DEFAULT_CLEVEL
}

#[unsafe(no_mangle)]
pub extern "C" fn ZSTD_compressBound(src_size: usize) -> usize {
    compress_bound(src_size)
}

// ---------------------------------------------------------------------------
// Errors
// ---------------------------------------------------------------------------

#[unsafe(no_mangle)]
pub extern "C" fn ZSTD_isError(code: usize) -> core::ffi::c_uint {
    error::is_error(code).into()
}

#[unsafe(no_mangle)]
pub extern "C" fn ZSTD_getErrorName(code: usize) -> *const c_char {
    error::name_raw(code).as_ptr()
}

#[unsafe(no_mangle)]
pub extern "C" fn ZSTD_getErrorCode(code: usize) -> core::ffi::c_uint {
    ErrorCode::from_raw(code) as u32
}

// ---------------------------------------------------------------------------
// One-shot codec
// ---------------------------------------------------------------------------

/// `ZSTD_compress` with default context settings and an explicit level.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ZSTD_compress(
    dst: *mut core::ffi::c_void,
    dst_capacity: usize,
    src: *const core::ffi::c_void,
    src_size: usize,
    compression_level: core::ffi::c_int,
) -> usize {
    let src = match unsafe { crate::as_slice(src, src_size) } {
        Ok(src) => src,
        Err(_) => return ret(ErrorCode::SrcBufferWrong),
    };
    let dst = match unsafe { crate::as_dst(dst, dst_capacity) } {
        Some(dst) => dst,
        None => return ret(ErrorCode::DstBufferNull),
    };
    let mut settings = CSettings::default();
    settings.level = compression_level;
    match crate::cctx::compress_oneshot(&settings, None, src, dst) {
        Ok(written) => written,
        Err(e) => ret(e),
    }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn ZSTD_decompress(
    dst: *mut core::ffi::c_void,
    dst_capacity: usize,
    src: *const core::ffi::c_void,
    src_size: usize,
) -> usize {
    let src = match unsafe { crate::as_slice(src, src_size) } {
        Ok(src) => src,
        Err(_) => return ret(ErrorCode::SrcBufferWrong),
    };
    let dst = match unsafe { crate::as_dst(dst, dst_capacity) } {
        Some(dst) => dst,
        None => return ret(ErrorCode::DstBufferNull),
    };
    match crate::dctx::decompress_oneshot(&DSettings::default(), None, src, dst) {
        Ok(written) => written,
        Err(e) => ret(e),
    }
}

// ---------------------------------------------------------------------------
// Frame metadata
// ---------------------------------------------------------------------------

/// `ZSTD_CONTENTSIZE_UNKNOWN` / `ZSTD_CONTENTSIZE_ERROR`.
pub const CONTENTSIZE_UNKNOWN: u64 = u64::MAX;
pub const CONTENTSIZE_ERROR: u64 = u64::MAX - 1;

#[unsafe(no_mangle)]
pub unsafe extern "C" fn ZSTD_getFrameContentSize(
    src: *const core::ffi::c_void,
    src_size: usize,
) -> u64 {
    let src = match unsafe { crate::as_slice(src, src_size) } {
        Ok(src) => src,
        Err(_) => return CONTENTSIZE_ERROR,
    };
    match crate::frame::parse_header(src) {
        Ok(crate::frame::Header::Zstd(header)) => {
            header.content_size.unwrap_or(CONTENTSIZE_UNKNOWN)
        },
        // A complete skippable frame reports 0 (verified against the
        // reference library); a truncated one is an error, like any other
        // unreadable head.
        Ok(crate::frame::Header::Skippable { .. }) => 0,
        _ => CONTENTSIZE_ERROR,
    }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn ZSTD_getDecompressedSize(
    src: *const core::ffi::c_void,
    src_size: usize,
) -> u64 {
    // The obsolete blend: known-and-nonempty keeps its value, everything
    // else (unknown, error, empty, skippable) collapses to 0.
    let fcs = unsafe { ZSTD_getFrameContentSize(src, src_size) };
    if fcs >= CONTENTSIZE_ERROR {
        0
    } else {
        fcs
    }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn ZSTD_findDecompressedSize(
    src: *const core::ffi::c_void,
    src_size: usize,
) -> u64 {
    let src = match unsafe { crate::as_slice(src, src_size) } {
        Ok(src) => src,
        Err(_) => return CONTENTSIZE_ERROR,
    };
    match crate::frame::find_decompressed_size(src) {
        Ok(Some(total)) => total,
        // A frame without a declared size makes the whole series unknown.
        Ok(None) => CONTENTSIZE_UNKNOWN,
        Err(()) => CONTENTSIZE_ERROR,
    }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn ZSTD_frameHeaderSize(
    src: *const core::ffi::c_void,
    src_size: usize,
) -> usize {
    let src = match unsafe { crate::as_slice(src, src_size) } {
        Ok(src) => src,
        Err(_) => return ret(ErrorCode::SrcBufferWrong),
    };
    if src.len() < crate::frame::HEADER_PREFIX {
        return ret(ErrorCode::SrcSizeWrong);
    }
    // libzstd's entry is pure arithmetic on the descriptor byte — no magic
    // validation (verified against the reference library: a skippable
    // frame is measured through its own size field's first byte).
    crate::frame::header_size_formula(src[4])
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn ZSTD_getFrameHeader(
    zfh_ptr: *mut crate::ZSTD_FrameHeader,
    src: *const core::ffi::c_void,
    src_size: usize,
) -> usize {
    if zfh_ptr.is_null() {
        return ret(ErrorCode::Generic);
    }
    let src = match unsafe { crate::as_slice(src, src_size) } {
        Ok(src) => src,
        Err(_) => return ret(ErrorCode::SrcBufferWrong),
    };
    let zfh = unsafe { &mut *zfh_ptr };
    let unknown = CONTENTSIZE_UNKNOWN;
    *zfh = crate::ZSTD_FrameHeader {
        frame_content_size: unknown,
        window_size: 0,
        block_size_max: 0,
        frame_type: crate::ZSTD_frame,
        header_size: 0,
        dict_id: 0,
        checksum_flag: 0,
        _reserved1: 0,
        _reserved2: 0,
    };
    match crate::frame::parse_full_header(src) {
        Ok(crate::frame::HeaderParse::Complete(header)) => {
            zfh.frame_content_size = header.content_size.unwrap_or(unknown);
            zfh.window_size = header.window_size;
            zfh.block_size_max = header.block_size_max as core::ffi::c_uint;
            zfh.frame_type = match header.frame_type {
                crate::frame::FrameType::Zstd => crate::ZSTD_frame,
                crate::frame::FrameType::Skippable => crate::ZSTD_skippableFrame,
            };
            zfh.header_size = header.header_size as core::ffi::c_uint;
            zfh.dict_id = header.dict_id;
            zfh.checksum_flag = u32::from(header.checksum);
            0
        },
        // A valid prefix: the total size the parser wants.
        Ok(crate::frame::HeaderParse::Wanted(wanted)) => wanted,
        Err(crate::frame::ParseError::NeedMore) => crate::frame::HEADER_PREFIX,
        Err(crate::frame::ParseError::BadMagic) => ret(ErrorCode::PrefixUnknown),
        Err(crate::frame::ParseError::ReservedBit) => ret(ErrorCode::FrameParameterUnsupported),
        Err(crate::frame::ParseError::WindowTooLarge) => {
            ret(ErrorCode::FrameParameterWindowTooLarge)
        },
        Err(crate::frame::ParseError::Malformed) => ret(ErrorCode::CorruptionDetected),
    }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn ZSTD_getDictID_fromDict(
    dict: *const core::ffi::c_void,
    dict_size: usize,
) -> core::ffi::c_uint {
    // The reference reads the four bytes behind the dictionary magic with
    // no validity judgment of the tables that follow.
    let Some(bytes) = (unsafe { crate::dict::dict_bytes(dict, dict_size) }) else {
        return 0;
    };
    if bytes[..4] != DICT_MAGIC {
        return 0;
    }
    u32::from_le_bytes(bytes[4..8].try_into().expect("8 bytes checked"))
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn ZSTD_getDictID_fromFrame(
    src: *const core::ffi::c_void,
    src_size: usize,
) -> core::ffi::c_uint {
    let src = match unsafe { crate::as_slice(src, src_size) } {
        Ok(src) => src,
        Err(_) => return 0,
    };
    match crate::frame::parse_full_header(src) {
        // The reference returns the skippable magic variant for skippable
        // frames (it reads the same struct field); incomplete or invalid
        // heads report 0.
        Ok(crate::frame::HeaderParse::Complete(header)) => header.dict_id,
        _ => 0,
    }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn ZSTD_findFrameCompressedSize(
    src: *const core::ffi::c_void,
    src_size: usize,
) -> usize {
    let src = match unsafe { crate::as_slice(src, src_size) } {
        Ok(src) => src,
        Err(_) => return ret(ErrorCode::SrcBufferWrong),
    };
    match crate::frame::frame_compressed_size(src) {
        Ok(size) => size,
        Err(crate::frame::ParseError::BadMagic) => ret(ErrorCode::PrefixUnknown),
        Err(crate::frame::ParseError::NeedMore) => ret(ErrorCode::SrcSizeWrong),
        Err(crate::frame::ParseError::Malformed) => ret(ErrorCode::CorruptionDetected),
        Err(crate::frame::ParseError::ReservedBit) => ret(ErrorCode::FrameParameterUnsupported),
        Err(crate::frame::ParseError::WindowTooLarge) => {
            ret(ErrorCode::FrameParameterWindowTooLarge)
        },
    }
}

/// libzstd's check is purely lexical: four bytes that spell either frame
/// magic family, no descriptor validation.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ZSTD_isFrame(
    src: *const core::ffi::c_void,
    src_size: usize,
) -> core::ffi::c_uint {
    let src = match unsafe { crate::as_slice(src, src_size) } {
        Ok(src) => src,
        Err(_) => return 0,
    };
    if src.len() < 4 {
        return 0;
    }
    let magic = u32::from_le_bytes(src[..4].try_into().expect("4 bytes checked"));
    (magic == crate::frame::MAGIC || (0x184d_2a50..=0x184d_2a5f).contains(&magic)).into()
}

// ---------------------------------------------------------------------------
// Context management (both stream types share the objects)
// ---------------------------------------------------------------------------

#[unsafe(no_mangle)]
pub extern "C" fn ZSTD_createCCtx() -> *mut crate::ZSTD_CCtx {
    Box::into_raw(Box::new(CCtx::new())).cast()
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn ZSTD_freeCCtx(cctx: *mut crate::ZSTD_CCtx) -> usize {
    if cctx.is_null() {
        return 0;
    }
    drop(unsafe { Box::from_raw(cctx.cast::<CCtx>()) });
    0
}

#[unsafe(no_mangle)]
pub extern "C" fn ZSTD_createDCtx() -> *mut crate::ZSTD_DCtx {
    Box::into_raw(Box::new(DCtx::new())).cast()
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn ZSTD_freeDCtx(dctx: *mut crate::ZSTD_DCtx) -> usize {
    if dctx.is_null() {
        return 0;
    }
    drop(unsafe { Box::from_raw(dctx.cast::<DCtx>()) });
    0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bound_matches_libzstd_formula() {
        // Checked against the reference library on this host.
        assert_eq!(compress_bound(0), 64);
        assert_eq!(compress_bound(1), 64);
        assert_eq!(compress_bound(131_072), 131_584);
        assert_eq!(compress_bound(usize::MAX), 0);
    }

    #[test]
    fn version_surface() {
        assert_eq!(VERSION_NUMBER, 10_600);
        assert_eq!(MAX_CLEVEL, 22);
        assert_eq!(MIN_CLEVEL, -131_072);
        assert_eq!(DEFAULT_CLEVEL, 3);
    }
}
