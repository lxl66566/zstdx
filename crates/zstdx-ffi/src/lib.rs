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
mod dict;
mod error;
mod frame;
mod params;
mod simple;

use core::ffi::c_void;

// The C symbols live at the crate root, as on the C side.
pub use dict::*;
pub use error::{ErrorCode, MAX_CODE};
pub use frame::*;
pub use params::*;
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

/// Opaque dictionary handles: `dict::CDict` (a parsed dictionary plus the
/// level decided at creation) and `dict::DDict` (a parsed decoding
/// dictionary shared with every decoder built from it).
#[repr(C)]
pub struct ZSTD_CDict {
    _opaque: [u8; 0],
}
#[repr(C)]
pub struct ZSTD_DDict {
    _opaque: [u8; 0],
}

/// The create/free pair for one opaque handle type: `create` boxes a fresh
/// state behind the opaque pointer, `free` takes it back (a null free is a
/// no-op, the libzstd convention). The state type is spelled per pair, so
/// a wrong-type cast cannot creep in.
macro_rules! handle_pair {
    ($create:ident, $free:ident, $opaque:ty, $state:ty) => {
        #[unsafe(no_mangle)]
        pub extern "C" fn $create() -> *mut $opaque {
            Box::into_raw(Box::new(<$state>::new())).cast()
        }
        crate::handle_free!($free, $opaque, $state);
    };
}
pub(crate) use handle_pair;

/// The free half alone, for handles whose create builds a custom payload.
macro_rules! handle_free {
    ($free:ident, $opaque:ty, $state:ty) => {
        #[unsafe(no_mangle)]
        pub unsafe extern "C" fn $free(handle: *mut $opaque) -> usize {
            if handle.is_null() {
                return 0;
            }
            // SAFETY: the handle round-trips the create's `Box::into_raw` of
            // a `$state`.
            drop(unsafe { Box::from_raw(handle.cast::<$state>()) });
            0
        }
    };
}
pub(crate) use handle_free;

/// `ZSTD_ResetDirective`. Values outside the enum reach libzstd's reset
/// entries as no-ops (its two `if`s fall through and return 0); this layer
/// mirrors that instead of erroring.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[allow(non_camel_case_types)]
#[repr(i32)]
pub enum ZSTD_ResetDirective {
    SessionOnly = 1,
    Parameters = 2,
    SessionAndParameters = 3,
}

impl ZSTD_ResetDirective {
    /// The directive for a raw C value, `None` for anything else.
    #[must_use]
    pub fn from_raw(raw: core::ffi::c_int) -> Option<Self> {
        match raw {
            1 => Some(Self::SessionOnly),
            2 => Some(Self::Parameters),
            3 => Some(Self::SessionAndParameters),
            _ => None,
        }
    }
}

/// `ZSTD_FrameHeader` (the `ZSTD_STATIC_LINKING_ONLY` surface).
#[repr(C)]
pub struct ZSTD_FrameHeader {
    /// `ZSTD_CONTENTSIZE_UNKNOWN` when the frame declares none.
    pub frame_content_size: u64,
    pub window_size: u64,
    pub block_size_max: core::ffi::c_uint,
    /// `ZSTD_frame` (0) or `ZSTD_skippableFrame` (1).
    pub frame_type: core::ffi::c_int,
    pub header_size: core::ffi::c_uint,
    /// For skippable frames the magic variant 0..=15.
    pub dict_id: core::ffi::c_uint,
    pub checksum_flag: core::ffi::c_uint,
    pub reserved1: core::ffi::c_uint,
    pub reserved2: core::ffi::c_uint,
}

/// `ZSTD_FrameType_e` values (the C names are the ABI).
#[allow(non_upper_case_globals)]
pub const ZSTD_frame: core::ffi::c_int = 0;
#[allow(non_upper_case_globals)]
pub const ZSTD_skippableFrame: core::ffi::c_int = 1;

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
        // SAFETY: the caller contract: `src[pos..pos+len]` is valid for reads.
        unsafe { core::slice::from_raw_parts(self.src.cast::<u8>().add(pos), len) }
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
    EnableLongDistanceMatching = 160,
    ContentSizeFlag = 200,
    ChecksumFlag = 201,
    DictIDFlag = 202,
    NbWorkers = 400,
}

impl ZSTD_cParameter {
    fn from_raw(raw: i32) -> Option<Self> {
        match raw {
            100 => Some(Self::CompressionLevel),
            101 => Some(Self::WindowLog),
            160 => Some(Self::EnableLongDistanceMatching),
            200 => Some(Self::ContentSizeFlag),
            201 => Some(Self::ChecksumFlag),
            202 => Some(Self::DictIDFlag),
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
/// The returned borrow is unbounded: every caller consumes the slice within
/// the call and must never retain it past the raw pointers' validity.
///
/// # Safety
/// `ptr[..len]` must be valid for reads when non-null.
pub unsafe fn as_slice<'a>(ptr: *const c_void, len: usize) -> Result<&'a [u8], ErrorCode> {
    if len == 0 {
        return Ok(&[]);
    }
    if ptr.is_null() {
        return Err(ErrorCode::SrcBufferWrong);
    }
    // SAFETY: the caller contract: `ptr[..len]` is valid for reads (null and
    // empty handled above).
    Ok(unsafe { core::slice::from_raw_parts(ptr.cast::<u8>(), len) })
}

/// Write side of [`as_slice`]: null plus positive capacity is
/// `dstBuffer_null`; zero capacity yields an empty (unwritable) slice so the
/// codec reports `dstSize_tooSmall` itself.
///
/// The returned borrow is unbounded, with the same consume-within-call rule.
///
/// # Safety
/// `ptr[..len]` must be valid for writes when non-null.
pub unsafe fn as_dst<'a>(ptr: *mut c_void, len: usize) -> Option<&'a mut [u8]> {
    if len == 0 {
        return Some(&mut []);
    }
    if ptr.is_null() {
        return None;
    }
    // SAFETY: the caller contract: `ptr[..len]` is valid for writes (null and
    // empty handled above).
    Some(unsafe { core::slice::from_raw_parts_mut(ptr.cast::<u8>(), len) })
}

/// Output slice of an out-buffer, with the same unbounded-borrow rule as
/// [`as_slice`].
///
/// # Safety
/// `dst[..size]` must be valid for writes when non-null.
pub unsafe fn as_mut_slice<'a>(ptr: *mut c_void, len: usize) -> &'a mut [u8] {
    if len == 0 || ptr.is_null() {
        return &mut [];
    }
    // SAFETY: the caller contract: `ptr[..len]` is valid for writes (null and
    // empty handled above).
    unsafe { core::slice::from_raw_parts_mut(ptr.cast::<u8>(), len) }
}

// ---------------------------------------------------------------------------
// Context entries (parameters, one-shot with context, dictionaries)
// ---------------------------------------------------------------------------

unsafe fn cctx_mut<'a>(ptr: *mut ZSTD_CCtx) -> &'a mut cctx::CCtx {
    // SAFETY: the handle came from handle_pair's box round-trip and is still
    // live.
    unsafe { &mut *ptr.cast::<cctx::CCtx>() }
}

unsafe fn dctx_mut<'a>(ptr: *mut ZSTD_DCtx) -> &'a mut dctx::DCtx {
    // SAFETY: the handle came from handle_pair's box round-trip and is still
    // live.
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
    // SAFETY: the handle came from handle_pair's box round-trip and is still
    // live.
    let ctx = unsafe { cctx_mut(cctx) };
    match ctx.set_parameter(param, value) {
        // The reference setters return the applied value on success.
        Ok(applied) => applied,
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
    // SAFETY: the handle came from handle_pair's box round-trip and is still
    // live.
    let ctx = unsafe { dctx_mut(dctx) };
    match ctx.set_parameter(param, value) {
        Ok(()) => 0,
        Err(e) => ret(e),
    }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn ZSTD_CCtx_setPledgedSrcSize(
    cctx: *mut ZSTD_CCtx,
    pledged_src_size: u64,
) -> usize {
    if cctx.is_null() {
        return ret(ErrorCode::Generic);
    }
    // SAFETY: the handle came from handle_pair's box round-trip and is still
    // live.
    match unsafe { cctx_mut(cctx) }.set_pledged(pledged_src_size) {
        Ok(()) => 0,
        Err(e) => ret(e),
    }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn ZSTD_CCtx_reset(cctx: *mut ZSTD_CCtx, reset: core::ffi::c_int) -> usize {
    let Some(directive) = ZSTD_ResetDirective::from_raw(reset) else {
        // libzstd's fall-through: an unknown directive resets nothing and
        // still reports success.
        return 0;
    };
    if cctx.is_null() {
        return ret(ErrorCode::Generic);
    }
    // SAFETY: the handle came from handle_pair's box round-trip and is still
    // live.
    match unsafe { cctx_mut(cctx) }.reset(directive) {
        Ok(()) => 0,
        Err(e) => ret(e),
    }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn ZSTD_DCtx_reset(dctx: *mut ZSTD_DCtx, reset: core::ffi::c_int) -> usize {
    let Some(directive) = ZSTD_ResetDirective::from_raw(reset) else {
        return 0;
    };
    if dctx.is_null() {
        return ret(ErrorCode::Generic);
    }
    // SAFETY: the handle came from handle_pair's box round-trip and is still
    // live.
    match unsafe { dctx_mut(dctx) }.reset(directive) {
        Ok(()) => 0,
        Err(e) => ret(e),
    }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn ZSTD_compress2(
    cctx: *mut ZSTD_CCtx,
    dst: *mut c_void,
    dst_capacity: usize,
    src: *const c_void,
    src_size: usize,
) -> usize {
    // SAFETY: the caller-validated (src, src_size) read pair, per as_slice's
    // contract.
    let Ok(src) = (unsafe { as_slice(src, src_size) }) else {
        return ret(ErrorCode::SrcBufferWrong);
    };
    // SAFETY: the caller-validated (dst, dst_capacity) write pair, per
    // as_dst's contract.
    let Some(dst) = (unsafe { as_dst(dst, dst_capacity) }) else {
        return ret(ErrorCode::DstBufferNull);
    };
    if cctx.is_null() {
        return ret(ErrorCode::Generic);
    }
    // Sticky settings and the loaded dictionary; always a fresh frame with
    // the source size pledged (libzstd's single-round override).
    // SAFETY: the handle came from handle_pair's box round-trip and is still
    // live.
    match unsafe { cctx_mut(cctx) }.compress2(src, dst) {
        Ok(written) => written,
        Err(e) => ret(e),
    }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn ZSTD_CCtx_loadDictionary(
    cctx: *mut ZSTD_CCtx,
    dict: *const c_void,
    dict_size: usize,
) -> usize {
    if cctx.is_null() {
        return ret(ErrorCode::Generic);
    }
    // SAFETY: the caller-validated dictionary read pair, per dict_bytes's
    // contract.
    let bytes = unsafe { dict_bytes(dict, dict_size) };
    // SAFETY: the handle came from handle_pair's box round-trip and is still
    // live.
    match unsafe { cctx_mut(cctx) }.load_dictionary(bytes) {
        Ok(()) => 0,
        Err(e) => ret(e),
    }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn ZSTD_DCtx_loadDictionary(
    dctx: *mut ZSTD_DCtx,
    dict: *const c_void,
    dict_size: usize,
) -> usize {
    if dctx.is_null() {
        return ret(ErrorCode::Generic);
    }
    // SAFETY: the caller-validated dictionary read pair, per dict_bytes's
    // contract.
    let bytes = unsafe { dict_bytes(dict, dict_size) };
    // SAFETY: the handle came from handle_pair's box round-trip and is still
    // live.
    match unsafe { dctx_mut(dctx) }.load_dictionary(bytes) {
        Ok(()) => 0,
        Err(e) => ret(e),
    }
}

/// `ZSTD_customMem` (only the null-default shape is honored; custom
/// allocators are a static-workspace concern this layer does not carry).
#[allow(non_snake_case)]
#[repr(C)]
pub struct ZSTD_customMem {
    pub customAlloc: Option<unsafe extern "C" fn(core::ffi::c_uint) -> *mut c_void>,
    pub customFree: Option<unsafe extern "C" fn(*mut c_void)>,
    pub opaque: *mut c_void,
}

impl ZSTD_customMem {
    /// The reference's validity rule: alloc and free must come as a pair.
    #[must_use]
    pub fn is_default(&self) -> bool {
        self.customAlloc.is_none() && self.customFree.is_none() && self.opaque.is_null()
    }
}

/// `ZSTD_CCtx_loadDictionary_byReference`: the parse copies into owned
/// state either way, so the reference load method only strengthens the
/// caller's lifetime contract.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ZSTD_CCtx_loadDictionary_byReference(
    cctx: *mut ZSTD_CCtx,
    dict: *const c_void,
    dict_size: usize,
) -> usize {
    // SAFETY: forwarding to the same contract with the caller's arguments.
    unsafe { ZSTD_CCtx_loadDictionary(cctx, dict, dict_size) }
}

/// `ZSTD_CCtx_loadDictionary_advanced`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ZSTD_CCtx_loadDictionary_advanced(
    cctx: *mut ZSTD_CCtx,
    dict: *const c_void,
    dict_size: usize,
    dict_load_method: DictLoadMethod,
    dict_content_type: DictContentType,
) -> usize {
    let _ = dict_load_method; // byRef/byCopy parse identically here
    if cctx.is_null() {
        return ret(ErrorCode::Generic);
    }
    // SAFETY: the caller-validated dictionary read pair, per dict_bytes's
    // contract.
    let bytes = unsafe { dict_bytes(dict, dict_size) };
    // SAFETY: the handle came from handle_pair's box round-trip and is still
    // live.
    match unsafe { cctx_mut(cctx) }.load_dictionary_typed(bytes, dict_content_type) {
        Ok(()) => 0,
        Err(e) => ret(e),
    }
}

/// `ZSTD_DCtx_loadDictionary_byReference`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ZSTD_DCtx_loadDictionary_byReference(
    dctx: *mut ZSTD_DCtx,
    dict: *const c_void,
    dict_size: usize,
) -> usize {
    // SAFETY: forwarding to the same contract with the caller's arguments.
    unsafe { ZSTD_DCtx_loadDictionary(dctx, dict, dict_size) }
}

/// `ZSTD_DCtx_loadDictionary_advanced`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ZSTD_DCtx_loadDictionary_advanced(
    dctx: *mut ZSTD_DCtx,
    dict: *const c_void,
    dict_size: usize,
    dict_load_method: DictLoadMethod,
    dict_content_type: DictContentType,
) -> usize {
    let _ = dict_load_method;
    if dctx.is_null() {
        return ret(ErrorCode::Generic);
    }
    // SAFETY: the caller-validated dictionary read pair, per dict_bytes's
    // contract.
    let bytes = unsafe { dict_bytes(dict, dict_size) };
    // SAFETY: the handle came from handle_pair's box round-trip and is still
    // live.
    match unsafe { dctx_mut(dctx) }.load_dictionary_typed(bytes, dict_content_type) {
        Ok(()) => 0,
        Err(e) => ret(e),
    }
}

/// `ZSTD_CCtx_refPrefix`: the raw-content prefix for the next frame only.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ZSTD_CCtx_refPrefix(
    cctx: *mut ZSTD_CCtx,
    prefix: *const c_void,
    prefix_size: usize,
) -> usize {
    // SAFETY: forwarding to the same contract with the caller's arguments.
    unsafe { ZSTD_CCtx_refPrefix_advanced(cctx, prefix, prefix_size, DictContentType::RawContent) }
}

/// `ZSTD_CCtx_refPrefix_advanced`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ZSTD_CCtx_refPrefix_advanced(
    cctx: *mut ZSTD_CCtx,
    prefix: *const c_void,
    prefix_size: usize,
    dict_content_type: DictContentType,
) -> usize {
    if cctx.is_null() {
        return ret(ErrorCode::Generic);
    }
    // SAFETY: the caller-validated (prefix, prefix_size) read pair, per
    // dict_bytes's contract.
    let bytes = unsafe { dict_bytes(prefix, prefix_size) };
    // SAFETY: the handle came from handle_pair's box round-trip and is still
    // live.
    match unsafe { cctx_mut(cctx) }.ref_prefix(bytes, dict_content_type) {
        Ok(()) => 0,
        Err(e) => ret(e),
    }
}

/// `ZSTD_DCtx_refPrefix`: the raw-content prefix for the next frame only
/// (`use_once`).
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ZSTD_DCtx_refPrefix(
    dctx: *mut ZSTD_DCtx,
    prefix: *const c_void,
    prefix_size: usize,
) -> usize {
    // SAFETY: forwarding to the same contract with the caller's arguments.
    unsafe { ZSTD_DCtx_refPrefix_advanced(dctx, prefix, prefix_size, DictContentType::RawContent) }
}

/// `ZSTD_DCtx_refPrefix_advanced`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ZSTD_DCtx_refPrefix_advanced(
    dctx: *mut ZSTD_DCtx,
    prefix: *const c_void,
    prefix_size: usize,
    dict_content_type: DictContentType,
) -> usize {
    if dctx.is_null() {
        return ret(ErrorCode::Generic);
    }
    // SAFETY: the caller-validated (prefix, prefix_size) read pair, per
    // dict_bytes's contract.
    let bytes = unsafe { dict_bytes(prefix, prefix_size) };
    // SAFETY: the handle came from handle_pair's box round-trip and is still
    // live.
    match unsafe { dctx_mut(dctx) }.ref_prefix_typed(bytes, dict_content_type) {
        Ok(()) => 0,
        Err(e) => ret(e),
    }
}

/// `ZSTD_DCtx_setMaxWindowSize`: the byte form of `ZSTD_d_windowLogMax`
/// (no 0-means-default special case here; out-of-range bytes error).
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ZSTD_DCtx_setMaxWindowSize(
    dctx: *mut ZSTD_DCtx,
    max_window_size: usize,
) -> usize {
    if dctx.is_null() {
        return ret(ErrorCode::Generic);
    }
    // SAFETY: the handle came from handle_pair's box round-trip and is still
    // live.
    match unsafe { dctx_mut(dctx) }.set_max_window_size(max_window_size) {
        Ok(()) => 0,
        Err(e) => ret(e),
    }
}

/// `ZSTD_DCtx_refDDict`: attach a parsed handle dictionary to every
/// following frame; a null handle clears the slot.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ZSTD_DCtx_refDDict(
    dctx: *mut ZSTD_DCtx,
    ddict: *const ZSTD_DDict,
) -> usize {
    if dctx.is_null() {
        return ret(ErrorCode::Generic);
    }
    // SAFETY: the handle came from the create's box round-trip; null excluded
    // by the guard.
    let dict = (!ddict.is_null()).then(|| unsafe { &*ddict.cast::<DDict>() });
    // SAFETY: the handle came from handle_pair's box round-trip and is still
    // live.
    match unsafe { dctx_mut(dctx) }.ref_ddict(dict.map(|d| &d.dict)) {
        Ok(()) => 0,
        Err(e) => ret(e),
    }
}

/// `ZSTD_getBlockSize`: the running (or last) frame's block size; 0
/// before any frame opened, as the reference's zeroed `appliedParams`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ZSTD_getBlockSize(cctx: *const ZSTD_CCtx) -> usize {
    if cctx.is_null() {
        return 0;
    }
    // SAFETY: the handle came from the create's box round-trip and is still
    // live.
    unsafe { &*cctx.cast::<cctx::CCtx>() }.applied_block_size
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
    // SAFETY: the caller-validated (src, src_size) read pair, per as_slice's
    // contract.
    let Ok(src) = (unsafe { as_slice(src, src_size) }) else {
        return ret(ErrorCode::SrcBufferWrong);
    };
    // SAFETY: the caller-validated (dst, dst_capacity) write pair, per
    // as_dst's contract.
    let Some(dst) = (unsafe { as_dst(dst, dst_capacity) }) else {
        return ret(ErrorCode::DstBufferNull);
    };
    if cctx.is_null() {
        return ret(ErrorCode::Generic);
    }
    // One-shot variants run on fresh level-only settings: zstd.h reserves
    // the sticky parameters for compress2 and the streaming entries.
    let settings = cctx::Settings::one_shot(compression_level);
    match cctx::compress_oneshot(&settings, None, src, dst) {
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
    // SAFETY: the caller-validated (src, src_size) read pair, per as_slice's
    // contract.
    let Ok(src) = (unsafe { as_slice(src, src_size) }) else {
        return ret(ErrorCode::SrcSizeWrong);
    };
    // Null dst reads as zero capacity, like ZSTD_decompress.
    // SAFETY: the caller-validated (dst, dst_capacity) write pair, per
    // as_mut_slice's contract.
    let dst = unsafe { as_mut_slice(dst, dst_capacity) };
    if dctx.is_null() {
        return ret(ErrorCode::Generic);
    }
    // SAFETY: the handle came from handle_pair's box round-trip and is still
    // live.
    let ctx = unsafe { dctx_mut(dctx) };
    let result = dctx::decompress_oneshot(&ctx.settings, ctx.dict.as_ref(), src, dst);
    // A refPrefix dictionary covers this one call (the reference's
    // getDDict consumes use_once at entry).
    ctx.consume_use_once();
    match result {
        Ok(written) => written,
        Err(e) => ret(e),
    }
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
    // SAFETY: the caller-validated (src, src_size) read pair, per as_slice's
    // contract.
    let Ok(src) = (unsafe { as_slice(src, src_size) }) else {
        return ret(ErrorCode::SrcBufferWrong);
    };
    // SAFETY: the caller-validated (dst, dst_capacity) write pair, per
    // as_dst's contract.
    let Some(dst) = (unsafe { as_dst(dst, dst_capacity) }) else {
        return ret(ErrorCode::DstBufferNull);
    };
    if cctx.is_null() {
        return ret(ErrorCode::Generic);
    }
    // Fresh level-only settings, like compressCCtx; the dictionary is this
    // call's alone and does not linger on the context.
    let settings = cctx::Settings::one_shot(compression_level);
    // SAFETY: the caller-validated dictionary read pair, per dict_bytes's
    // contract.
    let parsed = parse_cdict_bytes(unsafe { dict_bytes(dict, dict_size) });
    let parsed = match parsed {
        Ok(parsed) => parsed,
        Err(e) => return ret(e),
    };
    match cctx::compress_oneshot(&settings, parsed.as_ref(), src, dst) {
        Ok(written) => written,
        Err(e) => ret(e),
    }
}

/// Parse dictionary bytes for a one-shot call; null or sub-8-byte
/// dictionaries mean "no dictionary" (libzstd's rule).
fn parse_cdict_bytes(bytes: Option<&[u8]>) -> Result<Option<zstdx::EncoderDictionary>, ErrorCode> {
    match bytes {
        Some(bytes) => zstdx::EncoderDictionary::parse(bytes)
            .map(Some)
            .map_err(|_| ErrorCode::DictionaryCorrupted),
        None => Ok(None),
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
    // SAFETY: the caller-validated (src, src_size) read pair, per as_slice's
    // contract.
    let Ok(src) = (unsafe { as_slice(src, src_size) }) else {
        return ret(ErrorCode::SrcSizeWrong);
    };
    // Null dst reads as zero capacity, like ZSTD_decompress.
    // SAFETY: the caller-validated (dst, dst_capacity) write pair, per
    // as_mut_slice's contract.
    let dst = unsafe { as_mut_slice(dst, dst_capacity) };
    if dctx.is_null() {
        return ret(ErrorCode::Generic);
    }
    // SAFETY: the handle came from handle_pair's box round-trip and is still
    // live.
    let ctx = unsafe { dctx_mut(dctx) };
    // The dictionary is loaded into the context (libzstd's usingDict leaves
    // it loaded for the following calls) and survives until initDStream.
    // Parsed once: the slot is shared with the streams that follow.
    // SAFETY: the caller-validated dictionary read pair, per dict_bytes's
    // contract.
    let loaded = match unsafe { dict_bytes(dict, dict_size) } {
        Some(bytes) => match zstdx::decoding::Dictionary::load(bytes) {
            Ok(parsed) => Some(std::sync::Arc::new(parsed)),
            Err(_) => return ret(ErrorCode::DictionaryCorrupted),
        },
        None => None,
    };
    ctx.set_dict(loaded);
    match dctx::decompress_oneshot(&ctx.settings, ctx.dict.as_ref(), src, dst) {
        Ok(written) => written,
        Err(e) => ret(e),
    }
}

// ---------------------------------------------------------------------------
// Streaming compression
// ---------------------------------------------------------------------------

crate::handle_pair!(
    ZSTD_createCStream,
    ZSTD_freeCStream,
    ZSTD_CStream,
    cctx::CCtx
);

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
    // SAFETY: the handle came from handle_pair's box round-trip and is still
    // live.
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
    // SAFETY: non-null above; caller-provided buffer structs valid for reads
    // and writes.
    let (output, input) = unsafe { (&mut *output, &mut *input) };
    // Wild cursors are rejected exactly as the reference does, output first.
    if output.pos > output.size {
        return ret(ErrorCode::DstSizeTooSmall);
    }
    if input.pos > input.size {
        return ret(ErrorCode::SrcSizeWrong);
    }
    // SAFETY: the handle came from handle_pair's box round-trip and is still
    // live.
    let ctx = unsafe { cctx_mut(cctx) };
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
    // SAFETY: forwarding to the same contract with the caller's arguments.
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
    // SAFETY: output is non-null above; empty is a valid local buffer struct.
    unsafe { ZSTD_compressStream2(zcs, output, &raw mut empty, ZSTD_EndDirective::Flush as i32) }
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
    // SAFETY: output is non-null above; empty is a valid local buffer struct.
    unsafe { ZSTD_compressStream2(zcs, output, &raw mut empty, ZSTD_EndDirective::End as i32) }
}

// ---------------------------------------------------------------------------
// Streaming decompression
// ---------------------------------------------------------------------------

crate::handle_pair!(
    ZSTD_createDStream,
    ZSTD_freeDStream,
    ZSTD_DStream,
    dctx::DCtx
);

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
    // SAFETY: the handle came from handle_pair's box round-trip and is still
    // live.
    unsafe { dctx_mut(zds) }.init_stream();
    // The reference returns ZSTD_startingInputLength: the prefix size a
    // first read wants, not the general DStreamInSize.
    HEADER_PREFIX
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn ZSTD_initDStream_usingDict(
    zds: *mut ZSTD_DStream,
    dict: *const c_void,
    dict_size: usize,
) -> usize {
    if zds.is_null() {
        return ret(ErrorCode::Generic);
    }
    // SAFETY: the handle came from handle_pair's box round-trip and is still
    // live.
    let dctx = unsafe { dctx_mut(zds) };
    // SAFETY: the caller-validated dictionary read pair, per dict_bytes's
    // contract.
    match dctx.init_stream_with_dict(unsafe { dict_bytes(dict, dict_size) }) {
        Ok(()) => HEADER_PREFIX,
        Err(e) => ret(e),
    }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn ZSTD_initDStream_usingDDict(
    zds: *mut ZSTD_DStream,
    ddict: *const ZSTD_DDict,
) -> usize {
    if zds.is_null() {
        return ret(ErrorCode::Generic);
    }
    // A null handle clears the slot, as ZSTD_DCtx_refDDict(zds, NULL).
    // SAFETY: the handle came from the create's box round-trip; null excluded
    // by the guard.
    let dict = (!ddict.is_null()).then(|| unsafe { &*ddict.cast::<DDict>() });
    // SAFETY: the handle came from handle_pair's box round-trip and is still
    // live.
    unsafe { dctx_mut(zds) }.init_stream_with_ddict(dict.map(|d| &d.dict));
    HEADER_PREFIX
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn ZSTD_resetDStream(zds: *mut ZSTD_DStream) -> usize {
    if zds.is_null() {
        return ret(ErrorCode::Generic);
    }
    // Session-only: the dictionary reference survives, unlike
    // ZSTD_initDStream.
    // SAFETY: the handle came from handle_pair's box round-trip and is still
    // live.
    unsafe { dctx_mut(zds) }.init_stream_keep_dict();
    HEADER_PREFIX
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
    // SAFETY: non-null above; caller-provided buffer structs valid for reads
    // and writes.
    let (output, input) = unsafe { (&mut *output, &mut *input) };
    // Wild cursors rejected in the reference order: input first, then output.
    if input.pos > input.size {
        return ret(ErrorCode::SrcSizeWrong);
    }
    if output.pos > output.size {
        return ret(ErrorCode::DstSizeTooSmall);
    }
    // SAFETY: the handle came from handle_pair's box round-trip and is still
    // live.
    let ctx = unsafe { dctx_mut(zds) };
    match ctx.decompress_stream(output, input) {
        Ok(hint) => hint,
        Err(e) => {
            ctx.record_error(e);
            ret(e)
        },
    }
}
