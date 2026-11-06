//! The digested-dictionary objects: `ZSTD_CDict`/`ZSTD_DDict` hold the
//! parsed dictionary state once so repeated jobs skip re-prep, plus the
//! `getDictID` accessors. Compression through the handle rides the
//! pooled single-threaded dict state, whose seed-image cache restores the
//! filled match state across calls with the same seeding key (dictionary,
//! level, LDM mode and the payload size's seeding projection) — the
//! CDict attach economics; a key change re-seeds fully.

use std::sync::Arc;

use crate::{ZSTD_CDict, ZSTD_DDict, cctx, dctx, error::ErrorCode, simple::ret};

/// The `ZSTD_CDict` object: the dictionary parsed at creation plus the
/// compression level decided at the same time (`usingCDict` takes both
/// from the handle, per zstd.h). An empty dictionary creates a level-only
/// handle, as in libzstd.
pub struct CDict {
    pub dict: Option<zstdx::EncoderDictionary>,
    pub level: i32,
    /// The LDM mode decided at creation (the level-based constructors
    /// leave it `Auto`); `refCDict` and `usingCDict` take it with the
    /// level.
    pub ldm: zstdx::LdmMode,
    /// The raw byte length, for the frame window sizing consults.
    pub raw_len: usize,
}

/// The `ZSTD_DDict` object: the dictionary parsed once, shared with every
/// decoder built from the handle.
pub struct DDict {
    pub dict: dctx::SharedDict,
}

unsafe fn cdict_ref<'a>(ptr: *const ZSTD_CDict) -> Option<&'a CDict> {
    // SAFETY: the handle came from the create's box round-trip; null excluded
    // by the guard.
    (!ptr.is_null()).then(|| unsafe { &*ptr.cast::<CDict>() })
}

unsafe fn ddict_ref<'a>(ptr: *const ZSTD_DDict) -> Option<&'a DDict> {
    // SAFETY: the handle came from the create's box round-trip; null excluded
    // by the guard.
    (!ptr.is_null()).then(|| unsafe { &*ptr.cast::<DDict>() })
}

/// libzstd treats a null or sub-8-byte dictionary as "no dictionary".
pub(crate) unsafe fn dict_bytes(
    dict: *const core::ffi::c_void,
    size: usize,
) -> Option<&'static [u8]> {
    if dict.is_null() || size < 8 {
        return None;
    }
    // SAFETY: the caller-validated dictionary read pair, per as_slice's
    // contract.
    unsafe { crate::as_slice(dict, size).ok() }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn ZSTD_createCDict(
    dict_buffer: *const core::ffi::c_void,
    dict_size: usize,
    compression_level: core::ffi::c_int,
) -> *mut ZSTD_CDict {
    // SAFETY: the caller-validated dictionary read pair, per dict_bytes's
    // contract.
    let bytes = unsafe { dict_bytes(dict_buffer, dict_size) }.unwrap_or(&[]);
    // libzstd digests at creation and answers NULL for an unloadable
    // dictionary (an empty buffer stays a valid level-only handle).
    match zstdx::EncoderDictionary::parse(bytes) {
        Ok(dict) => Box::into_raw(Box::new(CDict {
            dict: (!bytes.is_empty()).then_some(dict),
            level: compression_level,
            ldm: zstdx::LdmMode::Auto,
            raw_len: bytes.len(),
        }))
        .cast(),
        Err(_) => core::ptr::null_mut(),
    }
}

/// `ZSTD_createCDict_byReference`: the handle digests into owned state
/// either way, so the reference load method only strengthens the caller's
/// lifetime contract.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ZSTD_createCDict_byReference(
    dict_buffer: *const core::ffi::c_void,
    dict_size: usize,
    compression_level: core::ffi::c_int,
) -> *mut ZSTD_CDict {
    // SAFETY: forwarding to the same contract with the caller's arguments.
    unsafe { ZSTD_createCDict(dict_buffer, dict_size, compression_level) }
}

crate::handle_free!(ZSTD_freeCDict, ZSTD_CDict, CDict);

/// The shared CDict build: the content-type load rules, plus the level
/// and LDM mode the handle carries.
///
/// SAFETY-helper note: `bytes` is the caller-validated dictionary read
/// pair per `dict_bytes`'s contract.
unsafe fn build_cdict(
    bytes: Option<&[u8]>,
    dict_content_type: crate::params::DictContentType,
    level: core::ffi::c_int,
    ldm: zstdx::LdmMode,
) -> Option<CDict> {
    let bytes = bytes.unwrap_or(&[]);
    let parsed = match dict_content_type {
        crate::params::DictContentType::Auto => zstdx::EncoderDictionary::parse(bytes),
        crate::params::DictContentType::RawContent => {
            Ok(zstdx::EncoderDictionary::raw_content(bytes))
        },
        crate::params::DictContentType::FullDict => {
            if bytes.len() < 8 || bytes.first_chunk::<4>() != Some(&crate::params::DICT_MAGIC) {
                return None;
            }
            zstdx::EncoderDictionary::parse(bytes)
        },
    };
    let dict = match parsed {
        Ok(dict) => (!bytes.is_empty()).then_some(dict),
        Err(_) => return None,
    };
    Some(CDict {
        dict,
        level,
        ldm,
        raw_len: bytes.len(),
    })
}

/// `ZSTD_createCDict_advanced`: custom allocators are refused (the
/// null-default `customMem` only, as `createDDict_advanced` records),
/// and explicit cParams are refused with NULL when any knob is
/// non-zero: this codec exposes no per-knob tuning, so adopting one
/// could only silently ignore it. Zeroed knobs take the level-default
/// row (the reference resolves `ZSTD_CLEVEL_DEFAULT`), so an all-zero
/// cParams CDict equals `createCDict` at level 3.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ZSTD_createCDict_advanced(
    dict_buffer: *const core::ffi::c_void,
    dict_size: usize,
    dict_load_method: crate::params::DictLoadMethod,
    dict_content_type: crate::params::DictContentType,
    c_params: crate::params::ZSTD_compressionParameters,
    custom_mem: crate::ZSTD_customMem,
) -> *mut ZSTD_CDict {
    let _ = dict_load_method;
    if !custom_mem.is_default() {
        return core::ptr::null_mut();
    }
    if c_params.hashLog != 0
        || c_params.chainLog != 0
        || c_params.searchLog != 0
        || c_params.minMatch != 0
        || c_params.targetLength != 0
        || c_params.strategy.0 != 0
        || c_params.windowLog != 0
    {
        return core::ptr::null_mut();
    }
    // SAFETY: the caller-validated dictionary read pair, per dict_bytes's
    // contract.
    let bytes = unsafe { dict_bytes(dict_buffer, dict_size) };
    unsafe {
        build_cdict(
            bytes,
            dict_content_type,
            crate::DEFAULT_CLEVEL,
            zstdx::LdmMode::Auto,
        )
    }
    .map(|c| Box::into_raw(Box::new(c)).cast())
    .unwrap_or_else(core::ptr::null_mut)
}

/// `ZSTD_createCDict_advanced2`: the bag-based constructor. The null-
/// default `customMem` only, and the same knob refusal as the bag apply
/// path (`CCtxParams::unsupported_requests`): the level and the LDM
/// switch are honored, everything else must sit at its default.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ZSTD_createCDict_advanced2(
    dict_buffer: *const core::ffi::c_void,
    dict_size: usize,
    dict_load_method: crate::params::DictLoadMethod,
    dict_content_type: crate::params::DictContentType,
    cctx_params: *const crate::ZSTD_CCtx_params,
    custom_mem: crate::ZSTD_customMem,
) -> *mut ZSTD_CDict {
    let _ = dict_load_method;
    if !custom_mem.is_default() || cctx_params.is_null() {
        return core::ptr::null_mut();
    }
    // SAFETY: the caller-provided bag, live per its create's contract.
    let bag = unsafe { &*cctx_params.cast::<crate::params::CCtxParams>() };
    if bag.unsupported_requests() || bag.c_params_window_log() != 0 {
        return core::ptr::null_mut();
    }
    let ldm = match bag.ldm_switch() {
        1 => zstdx::LdmMode::Enable,
        2 => zstdx::LdmMode::Disable,
        _ => zstdx::LdmMode::Auto,
    };
    // SAFETY: the caller-validated dictionary read pair, per dict_bytes's
    // contract.
    let bytes = unsafe { dict_bytes(dict_buffer, dict_size) };
    let level = bag.compression_level_for_context();
    unsafe { build_cdict(bytes, dict_content_type, level, ldm) }
        .map(|c| Box::into_raw(Box::new(c)).cast())
        .unwrap_or_else(core::ptr::null_mut)
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn ZSTD_getDictID_fromCDict(cdict: *const ZSTD_CDict) -> core::ffi::c_uint {
    // SAFETY: the caller-provided handle, live per the create's box
    // round-trip.
    match unsafe { cdict_ref(cdict) } {
        Some(cdict) => cdict
            .dict
            .as_ref()
            .and_then(zstdx::EncoderDictionary::header_id)
            .unwrap_or(0),
        None => 0,
    }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn ZSTD_compress_usingCDict(
    cctx: *mut crate::ZSTD_CCtx,
    dst: *mut core::ffi::c_void,
    dst_capacity: usize,
    src: *const core::ffi::c_void,
    src_size: usize,
    cdict: *const ZSTD_CDict,
) -> usize {
    // SAFETY: the caller-validated (src, src_size) read pair, per as_slice's
    // contract.
    let Ok(src) = (unsafe { crate::as_slice(src, src_size) }) else {
        return ret(ErrorCode::SrcBufferWrong);
    };
    // SAFETY: the caller-validated (dst, dst_capacity) write pair, per
    // as_dst's contract.
    let Some(dst) = (unsafe { crate::as_dst(dst, dst_capacity) }) else {
        return ret(ErrorCode::DstBufferNull);
    };
    // SAFETY: the caller-provided handle, live per the create's box
    // round-trip.
    let Some(cdict) = (unsafe { cdict_ref(cdict) }) else {
        // libzstd's usingCDict_internal rejects the NULL handle outright.
        return ret(ErrorCode::DictionaryWrong);
    };
    if cctx.is_null() {
        return ret(ErrorCode::Generic);
    }
    // Frame parameters come from the handle: the level decided at creation
    // plus libzstd's hardcoded frame set (contentSize=yes, checksum=no,
    // dictID=yes); the context's sticky parameters do not apply.
    let mut settings = cctx::Settings::one_shot(cdict.level);
    settings.ldm = cdict.ldm;
    match cctx::compress_oneshot(&settings, cdict.dict.as_ref(), src, dst) {
        Ok(written) => written,
        Err(e) => ret(e),
    }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn ZSTD_createDDict(
    dict_buffer: *const core::ffi::c_void,
    dict_size: usize,
) -> *mut ZSTD_DDict {
    // SAFETY: the caller-validated dictionary read pair, per dict_bytes's
    // contract.
    let bytes = unsafe { dict_bytes(dict_buffer, dict_size) }.unwrap_or(&[]);
    match zstdx::decoding::Dictionary::load(bytes) {
        Ok(dict) => Box::into_raw(Box::new(DDict {
            dict: Arc::new(dict),
        }))
        .cast(),
        Err(_) => core::ptr::null_mut(),
    }
}

/// `ZSTD_createDDict_byReference`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ZSTD_createDDict_byReference(
    dict_buffer: *const core::ffi::c_void,
    dict_size: usize,
) -> *mut ZSTD_DDict {
    // SAFETY: forwarding to the same contract with the caller's arguments.
    unsafe { ZSTD_createDDict(dict_buffer, dict_size) }
}

/// `ZSTD_createDDict_advanced`: custom allocators are refused (the
/// null-default `customMem` only); the content types load as the DCtx
/// family does.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ZSTD_createDDict_advanced(
    dict_buffer: *const core::ffi::c_void,
    dict_size: usize,
    dict_load_method: crate::params::DictLoadMethod,
    dict_content_type: crate::params::DictContentType,
    custom_mem: crate::ZSTD_customMem,
) -> *mut ZSTD_DDict {
    let _ = dict_load_method;
    if !custom_mem.is_default() {
        // The reference accepts a matched allocator pair; this layer has
        // no hook for caller allocation, so the request is refused.
        return core::ptr::null_mut();
    }
    // SAFETY: the caller-validated dictionary read pair, per dict_bytes's
    // contract.
    let Some(bytes) = (unsafe { dict_bytes(dict_buffer, dict_size) }) else {
        return Box::into_raw(Box::new(DDict {
            dict: Arc::new(zstdx::decoding::Dictionary::raw_content(&[])),
        }))
        .cast();
    };
    let parsed = match dict_content_type {
        crate::params::DictContentType::Auto => zstdx::decoding::Dictionary::load(bytes),
        crate::params::DictContentType::RawContent => {
            Ok(zstdx::decoding::Dictionary::raw_content(bytes))
        },
        crate::params::DictContentType::FullDict => {
            if bytes.len() < 8 || bytes.first_chunk::<4>() != Some(&crate::params::DICT_MAGIC) {
                return core::ptr::null_mut();
            }
            zstdx::decoding::Dictionary::decode_dict(bytes)
        },
    };
    match parsed {
        Ok(dict) => Box::into_raw(Box::new(DDict {
            dict: Arc::new(dict),
        }))
        .cast(),
        Err(_) => core::ptr::null_mut(),
    }
}

crate::handle_free!(ZSTD_freeDDict, ZSTD_DDict, DDict);

#[unsafe(no_mangle)]
pub unsafe extern "C" fn ZSTD_getDictID_fromDDict(ddict: *const ZSTD_DDict) -> core::ffi::c_uint {
    // SAFETY: the caller-provided handle, live per the create's box
    // round-trip.
    match unsafe { ddict_ref(ddict) } {
        Some(ddict) if ddict.dict.id != 0 => ddict.dict.id,
        _ => 0,
    }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn ZSTD_decompress_usingDDict(
    dctx: *mut crate::ZSTD_DCtx,
    dst: *mut core::ffi::c_void,
    dst_capacity: usize,
    src: *const core::ffi::c_void,
    src_size: usize,
    ddict: *const ZSTD_DDict,
) -> usize {
    // SAFETY: the caller-validated (src, src_size) read pair, per as_slice's
    // contract.
    let Ok(src) = (unsafe { crate::as_slice(src, src_size) }) else {
        return ret(ErrorCode::SrcSizeWrong);
    };
    // Null dst reads as zero capacity, like ZSTD_decompress.
    // SAFETY: the caller-validated (dst, dst_capacity) write pair, per
    // as_mut_slice's contract.
    let dst = unsafe { crate::as_mut_slice(dst, dst_capacity) };
    if dctx.is_null() {
        return ret(ErrorCode::Generic);
    }
    // The handle's dictionary is exclusive for this call; a NULL handle
    // decodes plain, ignoring the context's dictionary (libzstd's
    // decompressBegin_usingDDict(NULL) clears).
    // SAFETY: the caller-provided handle, live per the create's box
    // round-trip.
    let dict = unsafe { ddict_ref(ddict) }.map(|ddict| &ddict.dict);
    // SAFETY: the handle came from the create's box round-trip and is still
    // live.
    let ctx = unsafe { &mut *dctx.cast::<dctx::DCtx>() };
    match dctx::decompress_oneshot(&ctx.settings, dict, src, dst) {
        Ok(written) => written,
        Err(e) => ret(e),
    }
}
