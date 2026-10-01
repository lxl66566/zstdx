//! The digested-dictionary objects: `ZSTD_CDict`/`ZSTD_DDict` hold the
//! parsed dictionary state once so repeated jobs skip re-prep, plus the
//! `getDictID` accessors. The engine re-seeds a fresh matcher/decoder per
//! frame from the shared tables (intrinsic to its frame model — libzstd
//! additionally caches the filled match state inside its CDict); the
//! parse-and-digest half is what the handles amortize, honestly.

use std::sync::Arc;

use crate::{ZSTD_CDict, ZSTD_DDict, cctx, dctx, error::ErrorCode, simple::ret};

/// The `ZSTD_CDict` object: the dictionary parsed at creation plus the
/// compression level decided at the same time (`usingCDict` takes both
/// from the handle, per zstd.h). An empty dictionary creates a level-only
/// handle, as in libzstd.
pub struct CDict {
    pub dict: Option<zstdx::EncoderDictionary>,
    pub level: i32,
}

/// The `ZSTD_DDict` object: the dictionary parsed once, shared with every
/// decoder built from the handle.
pub struct DDict {
    pub dict: dctx::SharedDict,
}

unsafe fn cdict_ref<'a>(ptr: *const ZSTD_CDict) -> Option<&'a CDict> {
    (!ptr.is_null()).then(|| unsafe { &*ptr.cast::<CDict>() })
}

unsafe fn ddict_ref<'a>(ptr: *const ZSTD_DDict) -> Option<&'a DDict> {
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
    unsafe { crate::as_slice(dict, size).ok() }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn ZSTD_createCDict(
    dict_buffer: *const core::ffi::c_void,
    dict_size: usize,
    compression_level: core::ffi::c_int,
) -> *mut ZSTD_CDict {
    let bytes = unsafe { dict_bytes(dict_buffer, dict_size) }.unwrap_or(&[]);
    // libzstd digests at creation and answers NULL for an unloadable
    // dictionary (an empty buffer stays a valid level-only handle).
    match zstdx::EncoderDictionary::parse(bytes) {
        Ok(dict) => Box::into_raw(Box::new(CDict {
            dict: (!bytes.is_empty()).then_some(dict),
            level: compression_level,
        }))
        .cast(),
        Err(_) => core::ptr::null_mut(),
    }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn ZSTD_freeCDict(cdict: *mut ZSTD_CDict) -> usize {
    if cdict.is_null() {
        return 0;
    }
    drop(unsafe { Box::from_raw(cdict.cast::<CDict>()) });
    0
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn ZSTD_getDictID_fromCDict(cdict: *const ZSTD_CDict) -> core::ffi::c_uint {
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
    let Ok(src) = (unsafe { crate::as_slice(src, src_size) }) else {
        return ret(ErrorCode::SrcBufferWrong);
    };
    let Some(dst) = (unsafe { crate::as_dst(dst, dst_capacity) }) else {
        return ret(ErrorCode::DstBufferNull);
    };
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
    let settings = cctx::Settings::one_shot(cdict.level);
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
    let bytes = unsafe { dict_bytes(dict_buffer, dict_size) }.unwrap_or(&[]);
    match zstdx::decoding::Dictionary::load(bytes) {
        Ok(dict) => Box::into_raw(Box::new(DDict {
            dict: Arc::new(dict),
        }))
        .cast(),
        Err(_) => core::ptr::null_mut(),
    }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn ZSTD_freeDDict(ddict: *mut ZSTD_DDict) -> usize {
    if ddict.is_null() {
        return 0;
    }
    drop(unsafe { Box::from_raw(ddict.cast::<DDict>()) });
    0
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn ZSTD_getDictID_fromDDict(ddict: *const ZSTD_DDict) -> core::ffi::c_uint {
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
    let Ok(src) = (unsafe { crate::as_slice(src, src_size) }) else {
        return ret(ErrorCode::SrcBufferWrong);
    };
    let Some(dst) = (unsafe { crate::as_dst(dst, dst_capacity) }) else {
        return ret(ErrorCode::DstBufferNull);
    };
    if dctx.is_null() {
        return ret(ErrorCode::Generic);
    }
    // The handle's dictionary is exclusive for this call; a NULL handle
    // decodes plain, ignoring the context's dictionary (libzstd's
    // decompressBegin_usingDDict(NULL) clears).
    let dict = unsafe { ddict_ref(ddict) }.map(|ddict| &ddict.dict);
    let ctx = unsafe { &mut *dctx.cast::<dctx::DCtx>() };
    match dctx::decompress_oneshot(&ctx.settings, dict, src, dst) {
        Ok(written) => written,
        Err(e) => ret(e),
    }
}
