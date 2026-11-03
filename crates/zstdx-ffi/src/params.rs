//! The pure-arithmetic corner of the libzstd surface: the level table
//! (`clevels.h`), parameter bounds, the cParams adjust chain, and the
//! `estimate*` memory budgets. Everything here is a deterministic function
//! of its inputs, ported formula-exact from the reference tree so advisory
//! numbers stay byte-identical to `libzstd.a` builds of the same ABI
//! (x86-64, no sanitizer, no `ZSTD_MULTITHREAD`).
//!
//! The estimate constants embed four reference object sizes (`CCtx`,
//! `DCtx`, `CDict`, `DDict`) probed from the reference build; the Rust
//! contexts are different objects, so the estimates are advisory budgets in
//! the reference's units rather than reports of this implementation's
//! allocations (the `initStatic*` family that would consume them is not
//! exported).

use core::{cmp::Ordering, ffi::c_int};

use crate::{
    ZSTD_CCtx, ZSTD_DCtx,
    error::{ErrorCode, code},
};

// Reference object sizes (see the module docs for their provenance).
/// `sizeof(ZSTD_CCtx)` in the reference build.
const SIZEOF_CCTX: usize = 5280;
/// `sizeof(ZSTD_DCtx)` in the reference build.
const SIZEOF_DCTX: usize = 95_976;
/// `sizeof(ZSTD_CDict)` in the reference build.
const SIZEOF_CDICT: usize = 6080;
/// `sizeof(ZSTD_DDict)` in the reference build.
const SIZEOF_DDICT: usize = 27_352;
/// `sizeof(ZSTD_compressedBlockState_t)`: the two entropy-table states a
/// frame keeps (previous block + current block).
const SIZEOF_BLOCK_STATE: usize = 5632;
/// `TMP_WORKSPACE_SIZE`: entropy scratch, the larger of HUF (8704) and the
/// sequence compression table set.
const TMP_WORKSPACE_SIZE: usize = 8920;
/// `HUF_WORKSPACE_SIZE`: the dictionary-side Huffman scratch.
const HUF_WORKSPACE_SIZE: usize = 8704;
/// `ZSTD_OPT_SIZE = ZSTD_OPT_NUM + 3`: optimizer window entries.
const OPT_SIZE: usize = 4096 + 3;
/// The opt strategy price tables (`litLengthFreq`..`ZSTD_optimal_t` slots,
/// every count rounded up to the 64-byte workspace alignment).
const OPT_POTENTIAL_SPACE: usize = align_up(53 * 4, 64)
    + align_up(36 * 4, 64)
    + align_up(32 * 4, 64)
    + align_up(256 * 4, 64)
    + align_up(OPT_SIZE * 8, 64)
    + align_up(OPT_SIZE * 28, 64);
/// `WILDCOPY_OVERLENGTH`: the block-token buffer's overread margin.
const WILDCOPY_OVERLENGTH: usize = 32;
/// `ZSTD_cwksp_slack_space_required`: workspace alignment reserve.
const CWKSP_SLACK: usize = 64 * 2;
/// `ZSTD_BLOCKSIZE_MAX` and the experimental `c_maxBlockSize` floor.
pub const BLOCKSIZE_MAX: usize = 128 * 1024;
const BLOCKSIZE_MAX_MIN: usize = 1024;
/// `ZSTD_WINDOWLOG_ABSOLUTEMIN`: the smallest window a frame header can
/// declare.
pub const WINDOWLOG_ABSOLUTEMIN: u32 = 10;
/// 64-bit `ZSTD_WINDOWLOG_MAX` / `ZSTD_CHAINLOG_MAX` / `ZSTD_HASHLOG_MAX`.
const WINDOWLOG_MAX: u32 = 31;
const HASHLOG_MAX: u32 = 30;
const CHAINLOG_MAX: u32 = 30;
const HASHLOG_MIN: u32 = 6;
/// `ZSTD_WINDOWLOG_LIMIT_DEFAULT`, also the LDM default window.
const WINDOWLOG_LIMIT_DEFAULT: u32 = 27;
/// `ZSTD_HASHLOG3_MAX`: the minMatch-3 hash ceiling.
const HASHLOG3_MAX: u32 = 17;
/// `ZSTD_SHORT_CACHE_TAG_BITS` / `ZSTD_ROW_HASH_TAG_BITS`.
const SHORT_CACHE_TAG_BITS: u32 = 8;
const ROW_HASH_TAG_BITS: u32 = 8;
/// `ZSTD_MINMATCH_MIN/MAX` (the cParam bounds; the strategy-specific
/// tighter checks are the engine's business).
const MINMATCH_MIN: u32 = 3;
const MINMATCH_MAX: u32 = 7;
const SEARCHLOG_MAX: u32 = WINDOWLOG_MAX - 1;
const SEARCHLOG_MIN: u32 = 1;
const TARGETLENGTH_MAX: usize = BLOCKSIZE_MAX;
/// LDM defaults: `LDM_MIN_MATCH_LENGTH` and `LDM_BUCKET_SIZE_LOG`.
const LDM_MIN_MATCH_LENGTH: u32 = 64;
const LDM_BUCKET_SIZE_LOG: u32 = 4;

/// `ZSTD_MAGIC_DICTIONARY` (little-endian `0xEC30A437`).
pub(crate) const DICT_MAGIC: [u8; 4] = [0x37, 0xa4, 0x30, 0xec];

/// The running frame's block size, as `ZSTD_getBlockSize` reports it: the
/// level row's window, overridden by an explicit windowLog, downsized to
/// the pledged size and dictionary, capped by the format block maximum.
pub(crate) fn applied_block_size(
    settings: &crate::cctx::Settings,
    pledged: Option<u64>,
    dict_len: usize,
) -> usize {
    let hint = pledged.unwrap_or(u64::MAX);
    let mut cp = get_cparams_internal(settings.level, hint, dict_len, CParamMode::NoAttachDict);
    if let Some(window_log) = settings.window_log {
        cp.windowLog = window_log;
    }
    let cp = adjust_cparams_internal(
        cp,
        hint,
        dict_len,
        CParamMode::NoAttachDict,
        ParamSwitch::Auto,
    );
    BLOCKSIZE_MAX.min(1_usize << cp.windowLog)
}

/// Round up to the 64-byte workspace section alignment.
const fn align_up(size: usize, align: usize) -> usize {
    (size + align - 1) & !(align - 1)
}

/// `ZSTD_highbit32`: index of the highest set bit (0 for 1, 63 for 2^31).
const fn highbit32(value: u32) -> u32 {
    value.ilog2()
}

/// `BOUNDED(min, val, max)`.
const fn bounded(min: u64, val: u64, max: u64) -> u64 {
    if val < min {
        min
    } else if val > max {
        max
    } else {
        val
    }
}

/// `ZSTD_strategy`, as the ABI integer it is: `ZSTD_adjustCParams` must
/// accept and clamp values outside the enum, so the type is a transparent
/// wrapper rather than a Rust enum.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
#[allow(non_camel_case_types, non_upper_case_globals)]
#[repr(transparent)]
pub struct ZSTD_strategy(pub c_int);

#[allow(non_upper_case_globals)]
impl ZSTD_strategy {
    pub const ZSTD_btlazy2: Self = Self(6);
    pub const ZSTD_btopt: Self = Self(7);
    pub const ZSTD_btultra: Self = Self(8);
    pub const ZSTD_btultra2: Self = Self(9);
    pub const ZSTD_dfast: Self = Self(2);
    pub const ZSTD_fast: Self = Self(1);
    pub const ZSTD_greedy: Self = Self(3);
    pub const ZSTD_lazy: Self = Self(4);
    pub const ZSTD_lazy2: Self = Self(5);

    /// The raw ABI integer.
    const fn raw(self) -> i32 {
        self.0
    }
}

/// `ZSTD_compressionParameters`: the seven-match-finder knobs.
/// Field names mirror `zstd.h` verbatim (the ABI contract).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[allow(non_snake_case)]
#[repr(C)]
pub struct ZSTD_compressionParameters {
    pub windowLog: u32,
    pub chainLog: u32,
    pub hashLog: u32,
    pub searchLog: u32,
    pub minMatch: u32,
    pub targetLength: u32,
    pub strategy: ZSTD_strategy,
}

/// `ZSTD_frameParameters`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[allow(non_snake_case)]
#[repr(C)]
pub struct ZSTD_frameParameters {
    pub contentSizeFlag: c_int,
    pub checksumFlag: c_int,
    pub noDictIDFlag: c_int,
}

/// `ZSTD_parameters`: cParams plus the frame flags.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[allow(non_snake_case)]
#[repr(C)]
pub struct ZSTD_parameters {
    pub cParams: ZSTD_compressionParameters,
    pub fParams: ZSTD_frameParameters,
}

/// `ZSTD_bounds`: a value range or an error status.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[allow(non_snake_case)]
#[repr(C)]
pub struct ZSTD_bounds {
    /// An error code (test with `ZSTD_isError`), 0 on success.
    pub error: usize,
    pub lowerBound: c_int,
    pub upperBound: c_int,
}

impl ZSTD_bounds {
    fn ok(lower: c_int, upper: c_int) -> Self {
        Self {
            error: 0,
            lowerBound: lower,
            upperBound: upper,
        }
    }

    fn unsupported() -> Self {
        Self {
            error: code(ErrorCode::ParameterUnsupported),
            lowerBound: 0,
            upperBound: 0,
        }
    }
}

/// `ZSTD_ParamSwitch_e`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[allow(non_camel_case_types)]
#[repr(i32)]
pub enum ParamSwitch {
    Auto = 0,
    Enable = 1,
    Disable = 2,
}

impl ParamSwitch {
    /// The enum value as the raw ABI integer.
    const fn raw(self) -> i32 {
        self as i32
    }
}

/// `ZSTD_CParamMode_e`: how the adjust chain treats the dictionary size.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CParamMode {
    Unknown,
    NoAttachDict,
    CreateCDict,
    AttachDict,
}

/// `ldmParams_t` (the fields the estimate chain reads).
#[derive(Debug, Clone, Copy)]
struct LdmParams {
    enable: ParamSwitch,
    hash_log: u32,
    bucket_size_log: u32,
    min_match_length: u32,
    hash_rate_log: u32,
    window_log: u32,
}

impl LdmParams {
    const fn default_disabled() -> Self {
        Self {
            enable: ParamSwitch::Auto,
            hash_log: 0,
            bucket_size_log: 0,
            min_match_length: 0,
            hash_rate_log: 0,
            window_log: 0,
        }
    }
}

const fn cp(
    window_log: u32,
    chain_log: u32,
    hash_log: u32,
    search_log: u32,
    min_match: u32,
    target_length: u32,
    strategy: i32,
) -> ZSTD_compressionParameters {
    ZSTD_compressionParameters {
        windowLog: window_log,
        chainLog: chain_log,
        hashLog: hash_log,
        searchLog: search_log,
        minMatch: min_match,
        targetLength: target_length,
        strategy: ZSTD_strategy(strategy),
    }
}

/// `ZSTD_defaultCParameters[4][23]` from the reference `clevels.h`:
/// table 0 for srcSize > 256 KB, then <= 256 KB, <= 128 KB, <= 16 KB.
#[rustfmt::skip]
const CLEVELS: [[ZSTD_compressionParameters; 23]; 4] = [
    [
        cp(19,12,13, 1, 6,  1, 1), cp(19,13,14, 1, 7,  0, 1), cp(20,15,16, 1, 6,  0, 1),
        cp(21,16,17, 1, 5,  0, 2), cp(21,18,18, 1, 5,  0, 2), cp(21,18,19, 3, 5,  2, 3),
        cp(21,18,19, 3, 5,  4, 4), cp(21,19,20, 4, 5,  8, 4), cp(21,19,20, 4, 5, 16, 5),
        cp(22,20,21, 4, 5, 16, 5), cp(22,21,22, 5, 5, 16, 5), cp(22,21,22, 6, 5, 16, 5),
        cp(22,22,23, 6, 5, 32, 5), cp(22,22,22, 4, 5, 32, 6), cp(22,22,23, 5, 5, 32, 6),
        cp(22,23,23, 6, 5, 32, 6), cp(22,22,22, 5, 5, 48, 7), cp(23,23,22, 5, 4, 64, 7),
        cp(23,23,22, 6, 3, 64, 8), cp(23,24,22, 7, 3,256, 9), cp(25,25,23, 7, 3,256, 9),
        cp(26,26,24, 7, 3,512, 9), cp(27,27,25, 9, 3,999, 9),
    ],
    [
        cp(18,12,13, 1, 5,  1, 1), cp(18,13,14, 1, 6,  0, 1), cp(18,14,14, 1, 5,  0, 2),
        cp(18,16,16, 1, 4,  0, 2), cp(18,16,17, 3, 5,  2, 3), cp(18,17,18, 5, 5,  2, 3),
        cp(18,18,19, 3, 5,  4, 4), cp(18,18,19, 4, 4,  4, 4), cp(18,18,19, 4, 4,  8, 5),
        cp(18,18,19, 5, 4,  8, 5), cp(18,18,19, 6, 4,  8, 5), cp(18,18,19, 5, 4, 12, 6),
        cp(18,19,19, 7, 4, 12, 6), cp(18,18,19, 4, 4, 16, 7), cp(18,18,19, 4, 3, 32, 7),
        cp(18,18,19, 6, 3,128, 7), cp(18,19,19, 6, 3,128, 8), cp(18,19,19, 8, 3,256, 8),
        cp(18,19,19, 6, 3,128, 9), cp(18,19,19, 8, 3,256, 9), cp(18,19,19,10, 3,512, 9),
        cp(18,19,19,12, 3,512, 9), cp(18,19,19,13, 3,999, 9),
    ],
    [
        cp(17,12,12, 1, 5,  1, 1), cp(17,12,13, 1, 6,  0, 1), cp(17,13,15, 1, 5,  0, 1),
        cp(17,15,16, 2, 5,  0, 2), cp(17,17,17, 2, 4,  0, 2), cp(17,16,17, 3, 4,  2, 3),
        cp(17,16,17, 3, 4,  4, 4), cp(17,16,17, 3, 4,  8, 5), cp(17,16,17, 4, 4,  8, 5),
        cp(17,16,17, 5, 4,  8, 5), cp(17,16,17, 6, 4,  8, 5), cp(17,17,17, 5, 4,  8, 6),
        cp(17,18,17, 7, 4, 12, 6), cp(17,18,17, 3, 4, 12, 7), cp(17,18,17, 4, 3, 32, 7),
        cp(17,18,17, 6, 3,256, 7), cp(17,18,17, 6, 3,128, 8), cp(17,18,17, 8, 3,256, 8),
        cp(17,18,17,10, 3,512, 8), cp(17,18,17, 5, 3,256, 9), cp(17,18,17, 7, 3,512, 9),
        cp(17,18,17, 9, 3,512, 9), cp(17,18,17,11, 3,999, 9),
    ],
    [
        cp(14,12,13, 1, 5,  1, 1), cp(14,14,15, 1, 5,  0, 1), cp(14,14,15, 1, 4,  0, 1),
        cp(14,14,15, 2, 4,  0, 2), cp(14,14,14, 4, 4,  2, 3), cp(14,14,14, 3, 4,  4, 4),
        cp(14,14,14, 4, 4,  8, 5), cp(14,14,14, 6, 4,  8, 5), cp(14,14,14, 8, 4,  8, 5),
        cp(14,15,14, 5, 4,  8, 6), cp(14,15,14, 9, 4,  8, 6), cp(14,15,14, 3, 4, 12, 7),
        cp(14,15,14, 4, 3, 24, 7), cp(14,15,14, 5, 3, 32, 8), cp(14,15,15, 6, 3, 64, 8),
        cp(14,15,15, 7, 3,256, 8), cp(14,15,15, 5, 3, 48, 9), cp(14,15,15, 6, 3,128, 9),
        cp(14,15,15, 7, 3,256, 9), cp(14,15,15, 8, 3,256, 9), cp(14,15,15, 8, 3,512, 9),
        cp(14,15,15, 9, 3,512, 9), cp(14,15,15,10, 3,999, 9),
    ],
];

// ---------------------------------------------------------------------------
// cParams machinery
// ---------------------------------------------------------------------------

/// `ZSTD_cycleLog`: the chain index width after the btlazy2 scale-down.
const fn cycle_log(chain_log: u32, strategy: ZSTD_strategy) -> u32 {
    if strategy.raw() >= ZSTD_strategy::ZSTD_btlazy2.raw() {
        chain_log - 1
    } else {
        chain_log
    }
}

/// `ZSTD_dictAndWindowLog`: grow the window to cover source plus
/// dictionary when both are known (the zstd format keeps the whole
/// dictionary valid while any byte of it is in the window).
fn dict_and_window_log(window_log: u32, src_size: u64, dict_size: u64) -> u32 {
    if dict_size == 0 {
        return window_log;
    }
    let window_size = 1_u64 << window_log;
    if window_size >= dict_size + src_size {
        return window_log;
    }
    let dict_and_window = dict_size + window_size;
    if dict_and_window >= 1_u64 << WINDOWLOG_MAX {
        return WINDOWLOG_MAX;
    }
    highbit32((dict_and_window - 1) as u32) + 1
}

/// `ZSTD_clampCParams`: pull every knob into its cParam bounds.
fn clamp_cparams(mut cp: ZSTD_compressionParameters) -> ZSTD_compressionParameters {
    let clamp_u32 = |value: &mut u32, lower: i32, upper: i32| {
        if i64::from(*value) < i64::from(lower) {
            *value = lower as u32;
        } else if i64::from(*value) > i64::from(upper) {
            *value = upper as u32;
        }
    };
    clamp_u32(
        &mut cp.windowLog,
        WINDOWLOG_ABSOLUTEMIN as i32,
        WINDOWLOG_MAX as i32,
    );
    clamp_u32(&mut cp.chainLog, HASHLOG_MIN as i32, CHAINLOG_MAX as i32);
    clamp_u32(&mut cp.hashLog, HASHLOG_MIN as i32, HASHLOG_MAX as i32);
    clamp_u32(
        &mut cp.searchLog,
        SEARCHLOG_MIN as i32,
        SEARCHLOG_MAX as i32,
    );
    clamp_u32(&mut cp.minMatch, MINMATCH_MIN as i32, MINMATCH_MAX as i32);
    clamp_u32(&mut cp.targetLength, 0, TARGETLENGTH_MAX as i32);
    cp.strategy = ZSTD_strategy(cp.strategy.raw().clamp(
        ZSTD_strategy::ZSTD_fast.raw(),
        ZSTD_strategy::ZSTD_btultra2.raw(),
    ));
    cp
}

/// The short-cache tag clamp for CDict loads: fast and dfast indices carry
/// the tag bits, so their hash/chain logs cap at 32 minus the tag width.
fn cdict_indices_tagged(cp: &ZSTD_compressionParameters) -> bool {
    cp.strategy == ZSTD_strategy::ZSTD_fast || cp.strategy == ZSTD_strategy::ZSTD_dfast
}

/// `ZSTD_adjustCParams_internal`: downsize the knobs to the input. A
/// validated `cp` is presumed; `src_size` may be `u64::MAX` for unknown.
fn adjust_cparams_internal(
    mut cpar: ZSTD_compressionParameters,
    mut src_size: u64,
    mut dict_size: usize,
    mode: CParamMode,
    mut use_row_match_finder: ParamSwitch,
) -> ZSTD_compressionParameters {
    const MIN_SRC_SIZE: u64 = 513;
    const MAX_WINDOW_RESIZE: u64 = 1 << (WINDOWLOG_MAX - 1);

    match mode {
        CParamMode::Unknown | CParamMode::NoAttachDict => {},
        // A CDict built for an unknown source assumes a small one.
        CParamMode::CreateCDict => {
            if dict_size != 0 && src_size == u64::MAX {
                src_size = MIN_SRC_SIZE;
            }
        },
        // The dictionary's own knobs are chosen; these are the source's.
        CParamMode::AttachDict => dict_size = 0,
    }

    if src_size <= MAX_WINDOW_RESIZE && dict_size as u64 <= MAX_WINDOW_RESIZE {
        let t_size = (src_size + dict_size as u64) as u32;
        let src_log = if t_size < 1 << HASHLOG_MIN {
            HASHLOG_MIN
        } else {
            highbit32(t_size - 1) + 1
        };
        if cpar.windowLog > src_log {
            cpar.windowLog = src_log;
        }
    }
    if src_size != u64::MAX {
        let dict_and_window_log = dict_and_window_log(cpar.windowLog, src_size, dict_size as u64);
        let cycle_log = cycle_log(cpar.chainLog, cpar.strategy);
        if cpar.hashLog > dict_and_window_log + 1 {
            cpar.hashLog = dict_and_window_log + 1;
        }
        if cycle_log > dict_and_window_log {
            cpar.chainLog -= cycle_log - dict_and_window_log;
        }
    }

    if cpar.windowLog < WINDOWLOG_ABSOLUTEMIN {
        cpar.windowLog = WINDOWLOG_ABSOLUTEMIN;
    }

    if mode == CParamMode::CreateCDict && cdict_indices_tagged(&cpar) {
        let max_short_cache_hash_log = 32 - SHORT_CACHE_TAG_BITS;
        if cpar.hashLog > max_short_cache_hash_log {
            cpar.hashLog = max_short_cache_hash_log;
        }
        if cpar.chainLog > max_short_cache_hash_log {
            cpar.chainLog = max_short_cache_hash_log;
        }
    }

    if use_row_match_finder == ParamSwitch::Auto {
        use_row_match_finder = ParamSwitch::Enable;
    }
    if row_match_finder_used(cpar.strategy, use_row_match_finder) {
        let row_log = cpar.searchLog.clamp(4, 6);
        let max_row_hash_log = 32 - ROW_HASH_TAG_BITS;
        let max_hash_log = max_row_hash_log + row_log;
        if cpar.hashLog > max_hash_log {
            cpar.hashLog = max_hash_log;
        }
    }
    cpar
}

/// `ZSTD_overrideCParams`: non-zero fields of `overrides` replace the base.
fn override_cparams(base: &mut ZSTD_compressionParameters, overrides: &ZSTD_compressionParameters) {
    if overrides.windowLog != 0 {
        base.windowLog = overrides.windowLog;
    }
    if overrides.chainLog != 0 {
        base.chainLog = overrides.chainLog;
    }
    if overrides.hashLog != 0 {
        base.hashLog = overrides.hashLog;
    }
    if overrides.searchLog != 0 {
        base.searchLog = overrides.searchLog;
    }
    if overrides.minMatch != 0 {
        base.minMatch = overrides.minMatch;
    }
    if overrides.targetLength != 0 {
        base.targetLength = overrides.targetLength;
    }
    if overrides.strategy.raw() != 0 {
        base.strategy = overrides.strategy;
    }
}

/// `ZSTD_getCParamRowSize`: the row-selection size, with the unknown
/// dictionary-less shape answered as unknown.
fn cparam_row_size(src_size_hint: u64, mut dict_size: usize, mode: CParamMode) -> u64 {
    if mode == CParamMode::AttachDict {
        dict_size = 0;
    }
    let unknown = src_size_hint == u64::MAX;
    let added_size = u64::from(unknown && dict_size > 0) * 500;
    if unknown && dict_size == 0 {
        u64::MAX
    } else {
        src_size_hint
            .wrapping_add(dict_size as u64)
            .wrapping_add(added_size)
    }
}

/// `ZSTD_getCParams_internal`: the level row for the input shape, then the
/// adjust chain. `src_size_hint` 0 means zero, `u64::MAX` unknown.
fn get_cparams_internal(
    compression_level: i32,
    src_size_hint: u64,
    dict_size: usize,
    mode: CParamMode,
) -> ZSTD_compressionParameters {
    let r_size = cparam_row_size(src_size_hint, dict_size, mode);
    let table_id = usize::from(r_size <= 256 * 1024)
        + usize::from(r_size <= 128 * 1024)
        + usize::from(r_size <= 16 * 1024);
    let row = match compression_level.cmp(&0) {
        Ordering::Equal => 3, // ZSTD_CLEVEL_DEFAULT
        Ordering::Less => 0,  // the negative-level baseline row
        Ordering::Greater => compression_level.min(crate::MAX_CLEVEL) as usize,
    };
    let mut cp = CLEVELS[table_id][row];
    if compression_level < 0 {
        // The acceleration factor: -level as the target length.
        let clamped = compression_level.max(crate::MIN_CLEVEL);
        cp.targetLength = (-clamped) as u32;
    }
    adjust_cparams_internal(cp, src_size_hint, dict_size, mode, ParamSwitch::Auto)
}

// ---------------------------------------------------------------------------
// Row-match-finder and LDM resolution
// ---------------------------------------------------------------------------

/// `ZSTD_rowMatchFinderSupported`: greedy..lazy2 can use the row finder.
const fn row_match_finder_supported(strategy: ZSTD_strategy) -> bool {
    strategy.raw() >= ZSTD_strategy::ZSTD_greedy.raw()
        && strategy.raw() <= ZSTD_strategy::ZSTD_lazy2.raw()
}

/// `ZSTD_rowMatchFinderUsed`: the resolved mode says the row finder runs.
const fn row_match_finder_used(strategy: ZSTD_strategy, mode: ParamSwitch) -> bool {
    row_match_finder_supported(strategy) && mode.raw() == ParamSwitch::Enable.raw()
}

/// `ZSTD_resolveRowMatchFinderMode`: auto enables above the window-log
/// lower bound (14 outside the Linux kernel).
fn resolve_row_match_finder_mode(
    mode: ParamSwitch,
    cp: &ZSTD_compressionParameters,
) -> ParamSwitch {
    const WINDOW_LOG_LOWER_BOUND: u32 = 14;
    if mode != ParamSwitch::Auto {
        return mode;
    }
    if !row_match_finder_supported(cp.strategy) {
        return ParamSwitch::Disable;
    }
    if cp.windowLog > WINDOW_LOG_LOWER_BOUND {
        ParamSwitch::Enable
    } else {
        ParamSwitch::Disable
    }
}

/// `ZSTD_resolveEnableLdm`: auto enables for btopt+ at a 128 MiB window.
fn resolve_enable_ldm(mode: ParamSwitch, cp: &ZSTD_compressionParameters) -> ParamSwitch {
    if mode != ParamSwitch::Auto {
        return mode;
    }
    if cp.strategy.raw() >= ZSTD_strategy::ZSTD_btopt.raw()
        && cp.windowLog >= WINDOWLOG_LIMIT_DEFAULT
    {
        ParamSwitch::Enable
    } else {
        ParamSwitch::Disable
    }
}

/// `ZSTD_ldm_adjustParameters`: fill in the zero (auto) fields from the
/// strategy and window.
fn ldm_adjust_parameters(params: &mut LdmParams, cp: &ZSTD_compressionParameters) {
    params.window_log = cp.windowLog;
    if params.hash_rate_log == 0 {
        if params.hash_log > 0 {
            if params.window_log > params.hash_log {
                params.hash_rate_log = params.window_log - params.hash_log;
            }
        } else {
            // fast..btultra2 maps to rates 7..4.
            params.hash_rate_log = 7 - (cp.strategy.raw() as u32) / 3;
        }
    }
    if params.hash_log == 0 {
        if params.window_log <= params.hash_rate_log {
            params.hash_log = HASHLOG_MIN;
        } else {
            params.hash_log =
                (params.window_log - params.hash_rate_log).clamp(HASHLOG_MIN, HASHLOG_MAX);
        }
    }
    if params.min_match_length == 0 {
        params.min_match_length = LDM_MIN_MATCH_LENGTH;
        if cp.strategy.raw() >= ZSTD_strategy::ZSTD_btultra.raw() {
            params.min_match_length /= 2;
        }
    }
    if params.bucket_size_log == 0 {
        params.bucket_size_log = (cp.strategy.raw() as u32).clamp(LDM_BUCKET_SIZE_LOG, 8);
    }
    params.bucket_size_log = params.bucket_size_log.min(params.hash_log);
}

/// `ZSTD_ldm_getTableSize`: the bucket heads plus the entry table.
fn ldm_get_table_size(params: &LdmParams) -> usize {
    if params.enable != ParamSwitch::Enable {
        return 0;
    }
    let ldm_h_size = 1_usize << params.hash_log;
    let bucket_size_log = params.bucket_size_log.min(params.hash_log);
    let ldm_bucket_size = 1_usize << (params.hash_log - bucket_size_log);
    ldm_bucket_size + ldm_h_size * 8
}

/// `ZSTD_ldm_getMaxNbSeq`: one long match per minimum match length.
fn ldm_get_max_nb_seq(params: &LdmParams, max_chunk_size: usize) -> usize {
    if params.enable == ParamSwitch::Enable {
        max_chunk_size / params.min_match_length as usize
    } else {
        0
    }
}

// ---------------------------------------------------------------------------
// Estimate core
// ---------------------------------------------------------------------------

/// `ZSTD_allocateChainTable`: fast and row-mode skip the chain table;
/// dedicated-dict-search states always carry one.
const fn allocate_chain_table(
    strategy: ZSTD_strategy,
    use_row_match_finder: ParamSwitch,
    for_dds_dict: bool,
) -> bool {
    for_dds_dict
        || (strategy.raw() != ZSTD_strategy::ZSTD_fast.raw()
            && !row_match_finder_used(strategy, use_row_match_finder))
}

/// `ZSTD_resolveMaxBlockSize`: 0 selects the format maximum.
const fn resolve_max_block_size(max_block_size: usize) -> usize {
    if max_block_size == 0 {
        BLOCKSIZE_MAX
    } else {
        max_block_size
    }
}

/// `ZSTD_sizeof_matchState`: the hash/chain/hash3 tables, the optimizer
/// state, the row-heads copy and the workspace slack.
fn sizeof_match_state(
    cp: &ZSTD_compressionParameters,
    use_row_match_finder: ParamSwitch,
    enable_dedicated_dict_search: bool,
    for_cctx: bool,
) -> usize {
    let chain_size = if allocate_chain_table(
        cp.strategy,
        use_row_match_finder,
        enable_dedicated_dict_search && !for_cctx,
    ) {
        1_usize << cp.chainLog
    } else {
        0
    };
    let h_size = 1_usize << cp.hashLog;
    let hash_log3 = if for_cctx && cp.minMatch == 3 {
        HASHLOG3_MAX.min(cp.windowLog)
    } else {
        0
    };
    // The hash3 table exists only when the row asks for it (hashLog3 0
    // is no table, not a one-entry one).
    let h3_size = if hash_log3 == 0 {
        0
    } else {
        (1_usize << hash_log3) * 4
    };
    let table_space = chain_size * 4 + h_size * 4 + h3_size;
    let opt_space = if for_cctx && cp.strategy.raw() >= ZSTD_strategy::ZSTD_btopt.raw() {
        OPT_POTENTIAL_SPACE
    } else {
        0
    };
    let lazy_additional = if row_match_finder_used(cp.strategy, use_row_match_finder) {
        align_up(h_size, 64)
    } else {
        0
    };
    table_space + opt_space + CWKSP_SLACK + lazy_additional
}

/// `ZSTD_maxNbSeq`: the sequence-store capacity for one block, from the
/// minimum match (a sequence costs at least minMatch, or 3 when minMatch
/// itself is 3).
fn max_nb_seq(block_size: usize, min_match: u32) -> usize {
    let divider = if min_match == 3 {
        3
    } else {
        4
    };
    block_size / divider
}

/// `ZSTD_estimateCCtxSize_usingCCtxParams_internal` (the shared core of
/// every compress-side estimate).
#[allow(clippy::too_many_arguments)]
fn estimate_using_params_internal(
    cp: &ZSTD_compressionParameters,
    ldm_params: &LdmParams,
    is_static: bool,
    use_row_match_finder: ParamSwitch,
    buff_in_size: usize,
    buff_out_size: usize,
    pledged_src_size: u64,
    max_block_size: usize,
) -> usize {
    let window_size = bounded(1, 1 << cp.windowLog, pledged_src_size) as usize;
    let block_size = resolve_max_block_size(max_block_size).min(window_size);
    let nb_seq = max_nb_seq(block_size, cp.minMatch);
    // cwksp_alloc_size is identity without the sanitizer.
    let token_space = WILDCOPY_OVERLENGTH + block_size + align_up(nb_seq * 8, 64) + 3 * nb_seq;
    let tmp_workspace = TMP_WORKSPACE_SIZE;
    let block_state_space = 2 * SIZEOF_BLOCK_STATE;
    let match_state_size = sizeof_match_state(cp, use_row_match_finder, false, true);
    let ldm_space = ldm_get_table_size(ldm_params);
    let max_nb_ldm_seq = ldm_get_max_nb_seq(ldm_params, block_size);
    let ldm_seq_space = if ldm_params.enable == ParamSwitch::Enable {
        align_up(max_nb_ldm_seq * 12, 64)
    } else {
        0
    };
    let buffer_space = buff_in_size + buff_out_size;
    let cctx_space = if is_static {
        SIZEOF_CCTX
    } else {
        0
    };
    cctx_space
        + tmp_workspace
        + block_state_space
        + ldm_space
        + ldm_seq_space
        + match_state_size
        + token_space
        + buffer_space
}

/// The parameter bundle `ZSTD_makeCCtxParamsFromCParams` builds from raw
/// cParams: the resolved row-finder and LDM modes feeding the estimates.
struct CParamsParams {
    row_match_finder: ParamSwitch,
    ldm: LdmParams,
}

fn make_params_from_cparams(cp: &ZSTD_compressionParameters) -> CParamsParams {
    let mut ldm = LdmParams::default_disabled();
    ldm.enable = resolve_enable_ldm(ParamSwitch::Auto, cp);
    if ldm.enable == ParamSwitch::Enable {
        ldm_adjust_parameters(&mut ldm, cp);
    }
    CParamsParams {
        row_match_finder: resolve_row_match_finder_mode(ParamSwitch::Auto, cp),
        ldm,
    }
}

/// `ZSTD_getCParamsFromCCtxParams` with the raw-cParams bundle: derive the
/// level-3 row, apply the non-zero overrides, then adjust. The row mode
/// passed through is the pre-resolved one from the bundle.
fn get_cparams_from_cparams_bundle(
    overrides: &ZSTD_compressionParameters,
    row_match_finder: ParamSwitch,
    ldm: &LdmParams,
) -> ZSTD_compressionParameters {
    let mut cp = get_cparams_internal(3, u64::MAX, 0, CParamMode::NoAttachDict);
    if ldm.enable == ParamSwitch::Enable {
        cp.windowLog = WINDOWLOG_LIMIT_DEFAULT;
    }
    override_cparams(&mut cp, overrides);
    adjust_cparams_internal(cp, u64::MAX, 0, CParamMode::NoAttachDict, row_match_finder)
}

/// `ZSTD_estimateCCtxSize_usingCParams` / `_usingCCtxParams` chain: pick
/// the larger of the row and chain finder layouts for row-capable
/// strategies.
fn estimate_cctx_size_using_cparams(cp: ZSTD_compressionParameters) -> usize {
    let bundle = make_params_from_cparams(&cp);
    let estimate = |row_mode: ParamSwitch, mut ldm: LdmParams| {
        let resolved = get_cparams_from_cparams_bundle(&cp, row_mode, &bundle.ldm);
        if ldm.enable == ParamSwitch::Enable {
            ldm_adjust_parameters(&mut ldm, &resolved);
        }
        estimate_using_params_internal(
            &resolved,
            &ldm,
            true,
            resolve_row_match_finder_mode(row_mode, &resolved),
            0,
            0,
            u64::MAX,
            0,
        )
    };
    if row_match_finder_supported(cp.strategy) {
        estimate(ParamSwitch::Disable, bundle.ldm).max(estimate(ParamSwitch::Enable, bundle.ldm))
    } else {
        estimate(bundle.row_match_finder, bundle.ldm)
    }
}

/// `ZSTD_estimateCStreamSize_usingCParams`: the one-shot estimate plus the
/// streaming in/out buffers for the resolved window.
fn estimate_cstream_size_using_cparams(cp: ZSTD_compressionParameters) -> usize {
    let bundle = make_params_from_cparams(&cp);
    let estimate = |row_mode: ParamSwitch, mut ldm: LdmParams| {
        let resolved = get_cparams_from_cparams_bundle(&cp, row_mode, &bundle.ldm);
        let block_size = resolve_max_block_size(0).min(1_usize << resolved.windowLog);
        let in_buff_size = (1_usize << resolved.windowLog) + block_size;
        let out_buff_size = crate::compress_bound(block_size) + 1;
        if ldm.enable == ParamSwitch::Enable {
            ldm_adjust_parameters(&mut ldm, &resolved);
        }
        estimate_using_params_internal(
            &resolved,
            &ldm,
            true,
            resolve_row_match_finder_mode(row_mode, &resolved),
            in_buff_size,
            out_buff_size,
            u64::MAX,
            0,
        )
    };
    if row_match_finder_supported(cp.strategy) {
        estimate(ParamSwitch::Disable, bundle.ldm).max(estimate(ParamSwitch::Enable, bundle.ldm))
    } else {
        estimate(bundle.row_match_finder, bundle.ldm)
    }
}

/// `ZSTD_estimateCCtxSize_internal`: the largest tier budget for a level.
fn estimate_cctx_size_internal(level: i32) -> usize {
    const SRC_SIZE_TIERS: [u64; 4] = [16 * 1024, 128 * 1024, 256 * 1024, u64::MAX];
    SRC_SIZE_TIERS
        .iter()
        .map(|&tier| {
            estimate_cctx_size_using_cparams(get_cparams_internal(
                level,
                tier,
                0,
                CParamMode::NoAttachDict,
            ))
        })
        .max()
        .unwrap_or(0)
}

/// `ZSTD_estimateCStreamSize_internal`: the unknown-size tier budget.
fn estimate_cstream_size_internal(level: i32) -> usize {
    estimate_cstream_size_using_cparams(get_cparams_internal(
        level,
        u64::MAX,
        0,
        CParamMode::NoAttachDict,
    ))
}

/// The monotone envelope over levels up to `level` (the reference keeps
/// memory usage non-decreasing with the level, from `min(level, 1)` up).
fn monotone_envelope(level: i32, per_level: impl Fn(i32) -> usize) -> usize {
    let mut budget = 0;
    let mut current = level.min(1);
    while current <= level {
        budget = budget.max(per_level(current));
        current += 1;
    }
    budget
}

// ---------------------------------------------------------------------------
// Public entries: bounds, cParams helpers, estimates
// ---------------------------------------------------------------------------

/// The full `ZSTD_cParameter` id space (the ABI values; the subset this
/// layer *sets* lives in [`crate::ZSTD_cParameter`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[allow(non_camel_case_types)]
#[repr(i32)]
pub enum CParamId {
    CompressionLevel = 100,
    WindowLog = 101,
    HashLog = 102,
    ChainLog = 103,
    SearchLog = 104,
    MinMatch = 105,
    TargetLength = 106,
    Strategy = 107,
    EnableLongDistanceMatching = 160,
    LdmHashLog = 161,
    LdmMinMatch = 162,
    LdmBucketSizeLog = 163,
    LdmHashRateLog = 164,
    ContentSizeFlag = 200,
    ChecksumFlag = 201,
    DictIDFlag = 202,
    NbWorkers = 400,
    JobSize = 401,
    OverlapLog = 402,
    Rsyncable = 500,
    Format = 10,
    ForceMaxWindow = 1000,
    ForceAttachDict = 1001,
    LiteralCompressionMode = 1002,
    SrcSizeHint = 1004,
    EnableDedicatedDictSearch = 1005,
    StableInBuffer = 1006,
    StableOutBuffer = 1007,
    BlockDelimiters = 1008,
    ValidateSequences = 1009,
    SplitAfterSequences = 1010,
    UseRowMatchFinder = 1011,
    DeterministicRefPrefix = 1012,
    PrefetchCDictTables = 1013,
    EnableSeqProducerFallback = 1014,
    MaxBlockSize = 1015,
    RepcodeResolution = 1016,
    BlockSplitterLevel = 1017,
}

impl CParamId {
    /// The id for a raw value, `None` outside the enum.
    const fn from_raw(raw: i32) -> Option<Self> {
        match raw {
            100 => Some(Self::CompressionLevel),
            101 => Some(Self::WindowLog),
            102 => Some(Self::HashLog),
            103 => Some(Self::ChainLog),
            104 => Some(Self::SearchLog),
            105 => Some(Self::MinMatch),
            106 => Some(Self::TargetLength),
            107 => Some(Self::Strategy),
            160 => Some(Self::EnableLongDistanceMatching),
            161 => Some(Self::LdmHashLog),
            162 => Some(Self::LdmMinMatch),
            163 => Some(Self::LdmBucketSizeLog),
            164 => Some(Self::LdmHashRateLog),
            200 => Some(Self::ContentSizeFlag),
            201 => Some(Self::ChecksumFlag),
            202 => Some(Self::DictIDFlag),
            400 => Some(Self::NbWorkers),
            401 => Some(Self::JobSize),
            402 => Some(Self::OverlapLog),
            500 => Some(Self::Rsyncable),
            10 => Some(Self::Format),
            1000 => Some(Self::ForceMaxWindow),
            1001 => Some(Self::ForceAttachDict),
            1002 => Some(Self::LiteralCompressionMode),
            1004 => Some(Self::SrcSizeHint),
            1005 => Some(Self::EnableDedicatedDictSearch),
            1006 => Some(Self::StableInBuffer),
            1007 => Some(Self::StableOutBuffer),
            1008 => Some(Self::BlockDelimiters),
            1009 => Some(Self::ValidateSequences),
            1010 => Some(Self::SplitAfterSequences),
            1011 => Some(Self::UseRowMatchFinder),
            1012 => Some(Self::DeterministicRefPrefix),
            1013 => Some(Self::PrefetchCDictTables),
            1014 => Some(Self::EnableSeqProducerFallback),
            1015 => Some(Self::MaxBlockSize),
            1016 => Some(Self::RepcodeResolution),
            1017 => Some(Self::BlockSplitterLevel),
            _ => None,
        }
    }

    /// `ZSTD_cParam_getBounds` for this id (the non-multithread build).
    // The arms stay grouped by the reference's parameter families, so the
    // equal-body groups stay separate on purpose.
    #[allow(clippy::match_same_arms)]
    fn bounds(self) -> ZSTD_bounds {
        use ZSTD_bounds as b;
        match self {
            Self::CompressionLevel => b::ok(crate::MIN_CLEVEL, crate::MAX_CLEVEL),
            Self::WindowLog => b::ok(WINDOWLOG_ABSOLUTEMIN as i32, WINDOWLOG_MAX as i32),
            Self::HashLog => b::ok(HASHLOG_MIN as i32, HASHLOG_MAX as i32),
            Self::ChainLog => b::ok(HASHLOG_MIN as i32, CHAINLOG_MAX as i32),
            Self::SearchLog => b::ok(SEARCHLOG_MIN as i32, SEARCHLOG_MAX as i32),
            Self::MinMatch => b::ok(MINMATCH_MIN as i32, MINMATCH_MAX as i32),
            Self::TargetLength => b::ok(0, TARGETLENGTH_MAX as i32),
            Self::Strategy => b::ok(
                ZSTD_strategy::ZSTD_fast.raw(),
                ZSTD_strategy::ZSTD_btultra2.raw(),
            ),
            Self::LdmHashLog => b::ok(HASHLOG_MIN as i32, HASHLOG_MAX as i32),
            Self::LdmMinMatch => b::ok(4, 4096),
            Self::LdmBucketSizeLog => b::ok(1, 8),
            Self::LdmHashRateLog => b::ok(0, (WINDOWLOG_MAX - HASHLOG_MIN) as i32),
            Self::EnableLongDistanceMatching
            | Self::LiteralCompressionMode
            | Self::SplitAfterSequences
            | Self::UseRowMatchFinder
            | Self::PrefetchCDictTables
            | Self::RepcodeResolution => {
                b::ok(ParamSwitch::Auto as i32, ParamSwitch::Disable as i32)
            },
            Self::ContentSizeFlag | Self::ChecksumFlag | Self::DictIDFlag | Self::Rsyncable => {
                b::ok(0, 1)
            },
            // The reference build carries no ZSTD_MULTITHREAD: the worker
            // knobs collapse to their default-only range. This layer's
            // nbWorkers setter accepts more (a recorded deviation); the
            // bounds answer mirrors the reference ABI.
            Self::NbWorkers | Self::JobSize | Self::OverlapLog => b::ok(0, 0),
            Self::Format | Self::StableInBuffer | Self::StableOutBuffer => b::ok(0, 1),
            Self::ForceMaxWindow
            | Self::EnableDedicatedDictSearch
            | Self::ValidateSequences
            | Self::DeterministicRefPrefix
            | Self::EnableSeqProducerFallback => b::ok(0, 1),
            Self::ForceAttachDict => b::ok(0, 3),
            Self::SrcSizeHint => b::ok(0, i32::MAX),
            Self::BlockDelimiters => b::ok(0, 1),
            Self::MaxBlockSize => b::ok(BLOCKSIZE_MAX_MIN as i32, BLOCKSIZE_MAX as i32),
            Self::BlockSplitterLevel => b::ok(0, 6),
        }
    }

    /// `ZSTD_cParam_withinBounds`.
    fn within_bounds(self, value: i32) -> bool {
        let bounds = self.bounds();
        bounds.error == 0 && value >= bounds.lowerBound && value <= bounds.upperBound
    }
}

/// The full `ZSTD_dParameter` id space.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[allow(non_camel_case_types)]
#[repr(i32)]
pub enum DParamId {
    WindowLogMax = 100,
    Format = 1000,
    StableOutBuffer = 1001,
    ForceIgnoreChecksum = 1002,
    RefMultipleDDicts = 1003,
    DisableHuffmanAssembly = 1004,
    MaxBlockSize = 1005,
}

impl DParamId {
    /// The id for a raw value, `None` outside the enum.
    const fn from_raw(raw: i32) -> Option<Self> {
        match raw {
            100 => Some(Self::WindowLogMax),
            1000 => Some(Self::Format),
            1001 => Some(Self::StableOutBuffer),
            1002 => Some(Self::ForceIgnoreChecksum),
            1003 => Some(Self::RefMultipleDDicts),
            1004 => Some(Self::DisableHuffmanAssembly),
            1005 => Some(Self::MaxBlockSize),
            _ => None,
        }
    }

    /// `ZSTD_dParam_getBounds` for this id.
    fn bounds(self) -> ZSTD_bounds {
        use ZSTD_bounds as b;
        match self {
            Self::WindowLogMax => b::ok(WINDOWLOG_ABSOLUTEMIN as i32, WINDOWLOG_MAX as i32),
            Self::Format
            | Self::StableOutBuffer
            | Self::ForceIgnoreChecksum
            | Self::RefMultipleDDicts
            | Self::DisableHuffmanAssembly => b::ok(0, 1),
            Self::MaxBlockSize => b::ok(BLOCKSIZE_MAX_MIN as i32, BLOCKSIZE_MAX as i32),
        }
    }
}

/// `ZSTD_cParam_getBounds`.
#[unsafe(no_mangle)]
pub extern "C" fn ZSTD_cParam_getBounds(c_param: c_int) -> ZSTD_bounds {
    match CParamId::from_raw(c_param) {
        Some(id) => id.bounds(),
        None => ZSTD_bounds::unsupported(),
    }
}

/// `ZSTD_dParam_getBounds`.
#[unsafe(no_mangle)]
pub extern "C" fn ZSTD_dParam_getBounds(d_param: c_int) -> ZSTD_bounds {
    match DParamId::from_raw(d_param) {
        Some(id) => id.bounds(),
        None => ZSTD_bounds::unsupported(),
    }
}

/// `ZSTD_getCParams`: the level row for the input shape (a 0 size hint
/// means unknown).
#[unsafe(no_mangle)]
pub extern "C" fn ZSTD_getCParams(
    compression_level: c_int,
    estimated_src_size: u64,
    dict_size: usize,
) -> ZSTD_compressionParameters {
    let hint = if estimated_src_size == 0 {
        u64::MAX
    } else {
        estimated_src_size
    };
    get_cparams_internal(compression_level, hint, dict_size, CParamMode::Unknown)
}

/// `ZSTD_getParams`: `ZSTD_getCParams` plus the default frame flags.
#[unsafe(no_mangle)]
pub extern "C" fn ZSTD_getParams(
    compression_level: c_int,
    estimated_src_size: u64,
    dict_size: usize,
) -> ZSTD_parameters {
    let hint = if estimated_src_size == 0 {
        u64::MAX
    } else {
        estimated_src_size
    };
    ZSTD_parameters {
        cParams: get_cparams_internal(compression_level, hint, dict_size, CParamMode::Unknown),
        fParams: ZSTD_frameParameters {
            contentSizeFlag: 1,
            checksumFlag: 0,
            noDictIDFlag: 0,
        },
    }
}

/// `ZSTD_adjustCParams`: clamp into range, then downsize to the input.
#[unsafe(no_mangle)]
pub extern "C" fn ZSTD_adjustCParams(
    c_par: ZSTD_compressionParameters,
    src_size: u64,
    dict_size: usize,
) -> ZSTD_compressionParameters {
    let c_par = clamp_cparams(c_par);
    let src_size = if src_size == 0 {
        u64::MAX
    } else {
        src_size
    };
    adjust_cparams_internal(
        c_par,
        src_size,
        dict_size,
        CParamMode::Unknown,
        ParamSwitch::Auto,
    )
}

/// `ZSTD_checkCParams`: every knob inside its cParam bounds.
#[unsafe(no_mangle)]
pub extern "C" fn ZSTD_checkCParams(c_params: ZSTD_compressionParameters) -> usize {
    use CParamId as id;
    let fields = [
        (id::WindowLog, c_params.windowLog as i64),
        (id::ChainLog, c_params.chainLog as i64),
        (id::HashLog, c_params.hashLog as i64),
        (id::SearchLog, c_params.searchLog as i64),
        (id::MinMatch, c_params.minMatch as i64),
        (id::TargetLength, c_params.targetLength as i64),
        (id::Strategy, i64::from(c_params.strategy.raw())),
    ];
    for &(param, value) in &fields {
        if !param.within_bounds(value as i32) {
            return code(ErrorCode::ParameterOutOfBound);
        }
    }
    0
}

/// `ZSTD_estimateCCtxSize`: the monotone level envelope over the four
/// source-size tiers.
#[unsafe(no_mangle)]
pub extern "C" fn ZSTD_estimateCCtxSize(compression_level: c_int) -> usize {
    monotone_envelope(compression_level, estimate_cctx_size_internal)
}

/// `ZSTD_estimateCCtxSize_usingCParams`.
#[unsafe(no_mangle)]
pub extern "C" fn ZSTD_estimateCCtxSize_usingCParams(
    c_params: ZSTD_compressionParameters,
) -> usize {
    estimate_cctx_size_using_cparams(c_params)
}

/// `ZSTD_estimateCStreamSize`: the monotone level envelope for the
/// unknown-size streaming shape.
#[unsafe(no_mangle)]
pub extern "C" fn ZSTD_estimateCStreamSize(compression_level: c_int) -> usize {
    monotone_envelope(compression_level, estimate_cstream_size_internal)
}

/// `ZSTD_estimateCStreamSize_usingCParams`.
#[unsafe(no_mangle)]
pub extern "C" fn ZSTD_estimateCStreamSize_usingCParams(
    c_params: ZSTD_compressionParameters,
) -> usize {
    estimate_cstream_size_using_cparams(c_params)
}

/// `ZSTD_decodingBufferSize_min`: the streaming output buffer a window
/// needs (a full window plus two blocks plus the wildcopy margins, capped
/// by the declared content size).
#[unsafe(no_mangle)]
pub extern "C" fn ZSTD_decodingBufferSize_min(window_size: u64, frame_content_size: u64) -> usize {
    decoding_buffer_size_min(window_size, frame_content_size, BLOCKSIZE_MAX)
}

fn decoding_buffer_size_min(
    window_size: u64,
    frame_content_size: u64,
    block_size_max: usize,
) -> usize {
    let block_size = window_size
        .min(BLOCKSIZE_MAX as u64)
        .min(block_size_max as u64);
    let needed_rb_size = window_size + block_size * 2 + WILDCOPY_OVERLENGTH as u64 * 2;
    needed_rb_size.min(frame_content_size) as usize
}

/// `ZSTD_estimateDCtxSize`: the reference decoder object.
#[unsafe(no_mangle)]
pub extern "C" fn ZSTD_estimateDCtxSize() -> usize {
    SIZEOF_DCTX
}

/// `ZSTD_estimateDStreamSize`: the decoder object plus one block of input
/// staging and the window-sized output buffer.
#[unsafe(no_mangle)]
pub extern "C" fn ZSTD_estimateDStreamSize(window_size: usize) -> usize {
    let block_size = window_size.min(BLOCKSIZE_MAX);
    let in_buff_size = block_size;
    let out_buff_size = decoding_buffer_size_min(window_size as u64, u64::MAX, BLOCKSIZE_MAX);
    SIZEOF_DCTX + in_buff_size + out_buff_size
}

/// `ZSTD_estimateDStreamSize_fromFrame`: the window a real frame needs.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ZSTD_estimateDStreamSize_fromFrame(
    src: *const core::ffi::c_void,
    src_size: usize,
) -> usize {
    // SAFETY: the caller-validated (src, src_size) read pair, per as_slice's
    // contract.
    let Ok(bytes) = (unsafe { crate::as_slice(src, src_size) }) else {
        // The reference's getFrameHeader deref is guarded the same way by
        // its own srcSize gate before any read.
        return code(ErrorCode::SrcSizeWrong);
    };
    match crate::frame::parse_full_header(bytes) {
        Ok(crate::frame::HeaderParse::Complete(header)) => {
            // The 32-bit windowSizeMax of the reference: 1 << WINDOWLOG_MAX.
            if header.window_size > 1_u64 << WINDOWLOG_MAX {
                return code(ErrorCode::FrameParameterWindowTooLarge);
            }
            ZSTD_estimateDStreamSize(header.window_size as usize)
        },
        // A valid prefix wants its header size, which fromFrame answers as
        // srcSize_wrong (its "> 0" branch).
        Ok(crate::frame::HeaderParse::Wanted(_)) => code(ErrorCode::SrcSizeWrong),
        Err(e) => code(crate::frame::parse_error_code(e)),
    }
}

/// `ZSTD_estimateCDictSize`.
#[unsafe(no_mangle)]
pub extern "C" fn ZSTD_estimateCDictSize(dict_size: usize, compression_level: c_int) -> usize {
    let c_params = get_cparams_internal(
        compression_level,
        u64::MAX,
        dict_size,
        CParamMode::CreateCDict,
    );
    ZSTD_estimateCDictSize_advanced(dict_size, c_params, DictLoadMethod::ByCopy)
}

/// `ZSTD_dictLoadMethod_e`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[allow(non_camel_case_types)]
#[repr(i32)]
pub enum DictLoadMethod {
    ByCopy = 0,
    ByReference = 1,
}

/// `ZSTD_dictContentType_e`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[allow(non_camel_case_types)]
#[repr(i32)]
pub enum DictContentType {
    Auto = 0,
    RawContent = 1,
    FullDict = 2,
}

/// `ZSTD_estimateCDictSize_advanced`: the dictionary object, its Huffman
/// scratch, the dedicated-dict-search match state, and the copied content.
#[unsafe(no_mangle)]
pub extern "C" fn ZSTD_estimateCDictSize_advanced(
    dict_size: usize,
    c_params: ZSTD_compressionParameters,
    dict_load_method: DictLoadMethod,
) -> usize {
    let row = resolve_row_match_finder_mode(ParamSwitch::Auto, &c_params);
    SIZEOF_CDICT
        + HUF_WORKSPACE_SIZE
        + sizeof_match_state(&c_params, row, true, false)
        + match dict_load_method {
            DictLoadMethod::ByReference => 0,
            // Content is stored pointer-aligned.
            DictLoadMethod::ByCopy => align_up(dict_size, 8),
        }
}

/// `ZSTD_estimateDDictSize`.
#[unsafe(no_mangle)]
pub extern "C" fn ZSTD_estimateDDictSize(
    dict_size: usize,
    dict_load_method: DictLoadMethod,
) -> usize {
    SIZEOF_DDICT
        + match dict_load_method {
            DictLoadMethod::ByReference => 0,
            DictLoadMethod::ByCopy => dict_size,
        }
}

/// `ZSTD_sequenceBound`: sequences plus explicit block delimiters.
#[unsafe(no_mangle)]
pub extern "C" fn ZSTD_sequenceBound(src_size: usize) -> usize {
    src_size / MINMATCH_MIN as usize + 1 + src_size / BLOCKSIZE_MAX_MIN + 1
}

// ---------------------------------------------------------------------------
// Context parameter readback
// ---------------------------------------------------------------------------

unsafe fn cctx_ref<'a>(ptr: *const ZSTD_CCtx) -> &'a crate::cctx::CCtx {
    // SAFETY: the handle came from the create's box round-trip and is still
    // live.
    unsafe { &*ptr.cast::<crate::cctx::CCtx>() }
}

/// `ZSTD_CCtx_getParameter`: read a stored parameter back. The knobs this
/// layer cannot set never leave their defaults, so their reference answers
/// (the zeroed cParams bag) hold for every reachable state.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ZSTD_CCtx_getParameter(
    cctx: *mut ZSTD_CCtx,
    param: c_int,
    value: *mut c_int,
) -> usize {
    use CParamId as id;
    if cctx.is_null() || value.is_null() {
        return code(ErrorCode::Generic);
    }
    // SAFETY: the caller-provided handle, live per the create's box
    // round-trip.
    let settings = &unsafe { cctx_ref(cctx) }.settings;
    let Some(id) = CParamId::from_raw(param) else {
        return code(ErrorCode::ParameterUnsupported);
    };
    let read = match id {
        id::CompressionLevel => {
            // The reference stores the default level resolved (3), and the
            // setter's clamp applied value; this layer's 0 marker maps to
            // the same answer.
            if settings.level == 0 {
                3
            } else {
                settings.level
            }
        },
        id::WindowLog => settings.window_log.unwrap_or(0) as i32,
        id::HashLog
        | id::ChainLog
        | id::SearchLog
        | id::MinMatch
        | id::TargetLength
        | id::Strategy => 0,
        id::EnableLongDistanceMatching => match settings.ldm {
            zstdx::LdmMode::Auto => 0,
            zstdx::LdmMode::Enable => 1,
            zstdx::LdmMode::Disable => 2,
        },
        id::ContentSizeFlag => i32::from(settings.content_size),
        id::ChecksumFlag => i32::from(settings.checksum),
        id::DictIDFlag => i32::from(settings.dict_id_flag),
        id::NbWorkers => settings.workers as i32,
        // The reference build without ZSTD_MULTITHREAD answers these.
        id::JobSize | id::OverlapLog | id::Rsyncable => {
            return code(ErrorCode::ParameterUnsupported)
        },
        _ => return code(ErrorCode::ParameterUnsupported),
    };
    // SAFETY: value is non-null above and a caller-provided valid c_int
    // target.
    unsafe { *value = read };
    0
}

/// `ZSTD_DCtx_getParameter`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ZSTD_DCtx_getParameter(
    dctx: *mut ZSTD_DCtx,
    param: c_int,
    value: *mut c_int,
) -> usize {
    use DParamId as id;
    if dctx.is_null() || value.is_null() {
        return code(ErrorCode::Generic);
    }
    // SAFETY: the handle came from the create's box round-trip and is still
    // live.
    let settings = &unsafe { &*dctx.cast::<crate::dctx::DCtx>() }.settings;
    let Some(id) = DParamId::from_raw(param) else {
        return code(ErrorCode::ParameterUnsupported);
    };
    // The knobs this layer cannot set never leave their defaults, so the
    // reference's stored-default answers hold for every reachable state.
    let read = match id {
        // 0 stores the default limit; the reference reports highbit32 of
        // the byte limit.
        id::WindowLogMax => settings.window_log_max.unwrap_or(WINDOWLOG_LIMIT_DEFAULT) as i32,
        // d_format (zstd1), d_stableOutBuffer (buffered),
        // d_forceIgnoreChecksum (validate), d_refMultipleDDicts (single),
        // d_disableHuffmanAssembly (off), d_maxBlockSize (format maximum).
        id::Format
        | id::StableOutBuffer
        | id::ForceIgnoreChecksum
        | id::RefMultipleDDicts
        | id::DisableHuffmanAssembly
        | id::MaxBlockSize => 0,
    };
    // SAFETY: value is non-null above and a caller-provided valid c_int
    // target.
    unsafe { *value = read };
    0
}
