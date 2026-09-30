//! libzstd-compatible C ABI over the [zstdx] codec.
//!
//! This crate builds `libzstd.so` / `libzstd.a` artifacts (see the `[lib]`
//! section of `Cargo.toml`) exporting the stable libzstd API subset zstdx
//! implements: the simple one-shot entries, contexts with parameters and
//! dictionaries, and the `ZSTD_compressStream2` / `ZSTD_decompressStream`
//! streaming pair. Symbol coverage and semantic deviations from `zstd.h`
//! are tracked in `docs/src/dev/ffi.md`.
//!
//! The layer adapts at the boundary: contexts own zstdx-side state objects
//! (an encoder over a staging sink, a `FrameDecoder` with staged input) and
//! never change codec semantics.
#![allow(clippy::similar_names)]

mod cctx;
mod dctx;
mod error;
mod frame;
mod simple;

use core::ffi::c_void;

pub use error::{ErrorCode, MAX_CODE};
// The C symbols live at the crate root, as on the C side.
pub use simple::*;

/// Opaque context handles: zero-sized Rust stand-ins for the C structs; the
/// real state hangs behind the pointer (see `cctx::CCtx` / `dctx::DCtx`).
#[repr(C)]
pub struct ZSTD_CCtx {
    _opaque: [u8; 0],
}
#[repr(C)]
pub struct ZSTD_DCtx {
    _opaque: [u8; 0],
}

// libzstd naming is the ABI contract; the crate keeps it verbatim.
#[allow(non_camel_case_types)]
pub type ZSTD_CStream = ZSTD_CCtx;
#[allow(non_camel_case_types)]
pub type ZSTD_DStream = ZSTD_DCtx;

/// `ZSTD_inBuffer`: input buffer with the read cursor.
#[repr(C)]
pub struct ZSTD_inBuffer {
    pub src: *const c_void,
    pub size: usize,
    pub pos: usize,
}

impl ZSTD_inBuffer {
    /// The slice `src[pos..pos+len]`.
    ///
    /// # Safety
    /// `src[pos..pos+len]` must be valid for reads for the returned
    /// lifetime.
    pub unsafe fn slice_at(&self, pos: usize, len: usize) -> &[u8] {
        if len == 0 {
            return &[];
        }
        unsafe { core::slice::from_raw_parts((self.src as *const u8).add(pos), len) }
    }
}

/// `ZSTD_outBuffer`: output buffer with the write cursor.
#[repr(C)]
pub struct ZSTD_outBuffer {
    pub dst: *mut c_void,
    pub size: usize,
    pub pos: usize,
}

/// The subset of `ZSTD_cParameter` this layer implements; every other value
/// fails with `ZSTD_error_parameter_unsupported`, as libzstd does for
/// parameters it does not know.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[allow(non_camel_case_types)]
#[repr(i32)]
pub enum ZSTD_cParameter {
    CompressionLevel = 100,
    WindowLog = 101,
    ContentSizeFlag = 200,
    ChecksumFlag = 201,
    NbWorkers = 400,
}

impl ZSTD_cParameter {
    fn from_raw(raw: i32) -> Option<Self> {
        match raw {
            100 => Some(Self::CompressionLevel),
            101 => Some(Self::WindowLog),
            200 => Some(Self::ContentSizeFlag),
            201 => Some(Self::ChecksumFlag),
            400 => Some(Self::NbWorkers),
            _ => None,
        }
    }
}

impl TryFrom<i32> for ZSTD_cParameter {
    type Error = ErrorCode;

    fn try_from(value: i32) -> Result<Self, Self::Error> {
        Self::from_raw(value).ok_or(ErrorCode::ParameterUnsupported)
    }
}

/// The subset of `ZSTD_dParameter` this layer implements.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[allow(non_camel_case_types)]
#[repr(i32)]
pub enum ZSTD_dParameter {
    WindowLogMax = 100,
}

impl TryFrom<i32> for ZSTD_dParameter {
    type Error = ErrorCode;

    fn try_from(value: i32) -> Result<Self, Self::Error> {
        match value {
            100 => Ok(Self::WindowLogMax),
            _ => Err(ErrorCode::ParameterUnsupported),
        }
    }
}

/// `ZSTD_EndDirective`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[allow(non_camel_case_types)]
#[repr(i32)]
pub enum ZSTD_EndDirective {
    Continue = 0,
    Flush = 1,
    End = 2,
}

impl TryFrom<i32> for ZSTD_EndDirective {
    type Error = ErrorCode;

    fn try_from(value: i32) -> Result<Self, Self::Error> {
        match value {
            0 => Ok(Self::Continue),
            1 => Ok(Self::Flush),
            2 => Ok(Self::End),
            _ => Err(ErrorCode::ParameterOutOfBound),
        }
    }
}

/// Read `len` bytes from `ptr`; a null pointer with a positive length is a
/// buffer error rather than a crash (libzstd would read it, but a defensive
/// error beats UB for the same callers).
///
/// # Safety
/// `ptr[..len]` must be valid for reads when non-null.
pub unsafe fn as_slice(ptr: *const c_void, len: usize) -> Result<&'static [u8], ErrorCode> {
    if len == 0 {
        return Ok(&[]);
    }
    if ptr.is_null() {
        return Err(ErrorCode::SrcBufferWrong);
    }
    Ok(unsafe { core::slice::from_raw_parts(ptr as *const u8, len) })
}

/// Write side of [`as_slice`]: null plus positive capacity is
/// `dstBuffer_null`; zero capacity yields an empty (unwritable) slice so the
/// codec reports `dstSize_tooSmall` itself.
///
/// # Safety
/// `ptr[..len]` must be valid for writes when non-null.
pub unsafe fn as_dst(ptr: *mut c_void, len: usize) -> Option<&'static mut [u8]> {
    if len == 0 {
        return Some(&mut []);
    }
    if ptr.is_null() {
        return None;
    }
    Some(unsafe { core::slice::from_raw_parts_mut(ptr as *mut u8, len) })
}

/// Output slice of an out-buffer.
///
/// # Safety
/// `dst[..size]` must be valid for writes when non-null.
pub unsafe fn as_mut_slice(ptr: *mut c_void, len: usize) -> &'static mut [u8] {
    if len == 0 || ptr.is_null() {
        return &mut [];
    }
    unsafe { core::slice::from_raw_parts_mut(ptr as *mut u8, len) }
}

// ---------------------------------------------------------------------------
// Context entries (parameters, one-shot with context, dictionaries)
// ---------------------------------------------------------------------------

unsafe fn cctx_mut<'a>(ptr: *mut ZSTD_CCtx) -> &'a mut cctx::CCtx {
    unsafe { &mut *ptr.cast::<cctx::CCtx>() }
}

unsafe fn dctx_mut<'a>(ptr: *mut ZSTD_DCtx) -> &'a mut dctx::DCtx {
    unsafe { &mut *ptr.cast::<dctx::DCtx>() }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn ZSTD_CCtx_setParameter(
    cctx: *mut ZSTD_CCtx,
    param: core::ffi::c_int,
    value: core::ffi::c_int,
) -> usize {
    if cctx.is_null() {
        return ret(ErrorCode::Generic);
    }
    let ctx = unsafe { cctx_mut(cctx) };
    match ctx.set_parameter(param, value) {
        Ok(()) => 0,
        Err(e) => ret(e),
    }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn ZSTD_DCtx_setParameter(
    dctx: *mut ZSTD_DCtx,
    param: core::ffi::c_int,
    value: core::ffi::c_int,
) -> usize {
    if dctx.is_null() {
        return ret(ErrorCode::Generic);
    }
    let ctx = unsafe { dctx_mut(dctx) };
    match ctx.settings.set_parameter(param, value) {
        Ok(()) => 0,
        Err(e) => ret(e),
    }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn ZSTD_compressCCtx(
    cctx: *mut ZSTD_CCtx,
    dst: *mut c_void,
    dst_capacity: usize,
    src: *const c_void,
    src_size: usize,
    compression_level: core::ffi::c_int,
) -> usize {
    let src = match unsafe { as_slice(src, src_size) } {
        Ok(src) => src,
        Err(_) => return ret(ErrorCode::SrcBufferWrong),
    };
    let dst = match unsafe { as_dst(dst, dst_capacity) } {
        Some(dst) => dst,
        None => return ret(ErrorCode::DstBufferNull),
    };
    if cctx.is_null() {
        return ret(ErrorCode::Generic);
    }
    let ctx = unsafe { cctx_mut(cctx) };
    let mut settings = ctx.settings.clone();
    settings.level = compression_level;
    match cctx::compress_oneshot(&settings, src, dst) {
        Ok(written) => written,
        Err(e) => ret(e),
    }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn ZSTD_decompressDCtx(
    dctx: *mut ZSTD_DCtx,
    dst: *mut c_void,
    dst_capacity: usize,
    src: *const c_void,
    src_size: usize,
) -> usize {
    let src = match unsafe { as_slice(src, src_size) } {
        Ok(src) => src,
        Err(_) => return ret(ErrorCode::SrcBufferWrong),
    };
    let dst = match unsafe { as_dst(dst, dst_capacity) } {
        Some(dst) => dst,
        None => return ret(ErrorCode::DstBufferNull),
    };
    if dctx.is_null() {
        return ret(ErrorCode::Generic);
    }
    let ctx = unsafe { dctx_mut(dctx) };
    match dctx::decompress_oneshot(&ctx.settings, src, dst) {
        Ok(written) if !src.is_empty() => written,
        Ok(_) => 0,
        Err(e) => ret(e),
    }
}

/// libzstd treats a null or sub-8-byte dictionary as "no dictionary".
unsafe fn dict_bytes(dict: *const c_void, size: usize) -> Option<&'static [u8]> {
    if dict.is_null() || size < 8 {
        return None;
    }
    unsafe { as_slice(dict, size).ok() }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn ZSTD_compress_usingDict(
    cctx: *mut ZSTD_CCtx,
    dst: *mut c_void,
    dst_capacity: usize,
    src: *const c_void,
    src_size: usize,
    dict: *const c_void,
    dict_size: usize,
    compression_level: core::ffi::c_int,
) -> usize {
    let src = match unsafe { as_slice(src, src_size) } {
        Ok(src) => src,
        Err(_) => return ret(ErrorCode::SrcBufferWrong),
    };
    let dst = match unsafe { as_dst(dst, dst_capacity) } {
        Some(dst) => dst,
        None => return ret(ErrorCode::DstBufferNull),
    };
    if cctx.is_null() {
        return ret(ErrorCode::Generic);
    }
    let ctx = unsafe { cctx_mut(cctx) };
    let mut settings = ctx.settings.clone();
    settings.level = compression_level;
    settings.dict = unsafe { dict_bytes(dict, dict_size) }.map(<[u8]>::to_vec);
    match cctx::compress_oneshot(&settings, src, dst) {
        Ok(written) => written,
        Err(e) => ret(e),
    }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn ZSTD_decompress_usingDict(
    dctx: *mut ZSTD_DCtx,
    dst: *mut c_void,
    dst_capacity: usize,
    src: *const c_void,
    src_size: usize,
    dict: *const c_void,
    dict_size: usize,
) -> usize {
    let src = match unsafe { as_slice(src, src_size) } {
        Ok(src) => src,
        Err(_) => return ret(ErrorCode::SrcBufferWrong),
    };
    let dst = match unsafe { as_dst(dst, dst_capacity) } {
        Some(dst) => dst,
        None => return ret(ErrorCode::DstBufferNull),
    };
    if dctx.is_null() {
        return ret(ErrorCode::Generic);
    }
    let ctx = unsafe { dctx_mut(dctx) };
    // The dictionary is loaded into the context (libzstd's usingDict leaves
    // it loaded for the following calls) and survives until initDStream.
    ctx.settings.dict = unsafe { dict_bytes(dict, dict_size) }.map(<[u8]>::to_vec);
    match dctx::decompress_oneshot(&ctx.settings, src, dst) {
        Ok(written) => written,
        Err(e) => ret(e),
    }
}

// ---------------------------------------------------------------------------
// Streaming compression
// ---------------------------------------------------------------------------

#[unsafe(no_mangle)]
pub extern "C" fn ZSTD_createCStream() -> *mut ZSTD_CStream {
    Box::into_raw(Box::new(cctx::CCtx::new())).cast()
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn ZSTD_freeCStream(zcs: *mut ZSTD_CStream) -> usize {
    if zcs.is_null() {
        return 0;
    }
    drop(unsafe { Box::from_raw(zcs.cast::<cctx::CCtx>()) });
    0
}

#[unsafe(no_mangle)]
pub extern "C" fn ZSTD_CStreamInSize() -> usize {
    128 * 1024
}

#[unsafe(no_mangle)]
pub extern "C" fn ZSTD_CStreamOutSize() -> usize {
    // ZSTD_compressBound(ZSTD_CStreamInSize()) + block header + checksum.
    compress_bound(128 * 1024) + 3 + 4
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn ZSTD_initCStream(
    zcs: *mut ZSTD_CStream,
    compression_level: core::ffi::c_int,
) -> usize {
    if zcs.is_null() {
        return ret(ErrorCode::Generic);
    }
    unsafe { cctx_mut(zcs) }.init_stream(compression_level);
    0
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn ZSTD_compressStream2(
    cctx: *mut ZSTD_CCtx,
    output: *mut ZSTD_outBuffer,
    input: *mut ZSTD_inBuffer,
    end_op: core::ffi::c_int,
) -> usize {
    if cctx.is_null() || output.is_null() || input.is_null() {
        return ret(ErrorCode::Generic);
    }
    let Ok(mode) = ZSTD_EndDirective::try_from(end_op) else {
        return ret(ErrorCode::ParameterOutOfBound);
    };
    let ctx = unsafe { cctx_mut(cctx) };
    let (output, input) = unsafe { (&mut *output, &mut *input) };
    // Keep the contract sane on wild cursors (libzstd asserts pos <= size).
    output.pos = output.pos.min(output.size);
    input.pos = input.pos.min(input.size);
    match ctx.compress_stream2(output, input, mode) {
        Ok(hint) => hint,
        Err(e) => {
            ctx.record_error(e);
            ret(e)
        },
    }
}

/// Deprecated alias: `ZSTD_compressStream2(,,ZSTD_e_continue)`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ZSTD_compressStream(
    zcs: *mut ZSTD_CStream,
    output: *mut ZSTD_outBuffer,
    input: *mut ZSTD_inBuffer,
) -> usize {
    unsafe { ZSTD_compressStream2(zcs, output, input, ZSTD_EndDirective::Continue as i32) }
}

/// `ZSTD_compressStream2(, emptyInput, ZSTD_e_flush)`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ZSTD_flushStream(
    zcs: *mut ZSTD_CStream,
    output: *mut ZSTD_outBuffer,
) -> usize {
    if output.is_null() {
        return ret(ErrorCode::Generic);
    }
    let mut empty = ZSTD_inBuffer {
        src: core::ptr::null(),
        size: 0,
        pos: 0,
    };
    unsafe { ZSTD_compressStream2(zcs, output, &mut empty, ZSTD_EndDirective::Flush as i32) }
}

/// `ZSTD_compressStream2(, emptyInput, ZSTD_e_end)`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ZSTD_endStream(
    zcs: *mut ZSTD_CStream,
    output: *mut ZSTD_outBuffer,
) -> usize {
    if output.is_null() {
        return ret(ErrorCode::Generic);
    }
    let mut empty = ZSTD_inBuffer {
        src: core::ptr::null(),
        size: 0,
        pos: 0,
    };
    unsafe { ZSTD_compressStream2(zcs, output, &mut empty, ZSTD_EndDirective::End as i32) }
}

// ---------------------------------------------------------------------------
// Streaming decompression
// ---------------------------------------------------------------------------

#[unsafe(no_mangle)]
pub extern "C" fn ZSTD_createDStream() -> *mut ZSTD_DStream {
    Box::into_raw(Box::new(dctx::DCtx::new())).cast()
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn ZSTD_freeDStream(zds: *mut ZSTD_DStream) -> usize {
    if zds.is_null() {
        return 0;
    }
    drop(unsafe { Box::from_raw(zds.cast::<dctx::DCtx>()) });
    0
}

#[unsafe(no_mangle)]
pub extern "C" fn ZSTD_DStreamInSize() -> usize {
    dctx::DSTREAM_IN_SIZE
}

#[unsafe(no_mangle)]
pub extern "C" fn ZSTD_DStreamOutSize() -> usize {
    dctx::DSTREAM_OUT_SIZE
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn ZSTD_initDStream(zds: *mut ZSTD_DStream) -> usize {
    if zds.is_null() {
        return ret(ErrorCode::Generic);
    }
    unsafe { dctx_mut(zds) }.init_stream();
    dctx::DSTREAM_IN_SIZE
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn ZSTD_decompressStream(
    zds: *mut ZSTD_DStream,
    output: *mut ZSTD_outBuffer,
    input: *mut ZSTD_inBuffer,
) -> usize {
    if zds.is_null() || output.is_null() || input.is_null() {
        return ret(ErrorCode::Generic);
    }
    let ctx = unsafe { dctx_mut(zds) };
    let (output, input) = unsafe { (&mut *output, &mut *input) };
    output.pos = output.pos.min(output.size);
    input.pos = input.pos.min(input.size);
    match ctx.decompress_stream(output, input) {
        Ok(hint) => hint,
        Err(e) => {
            ctx.record_error(e);
            ret(e)
        },
    }
}
