//! The libzstd error convention over zstdx's error types.
//!
//! libzstd reports failures as `size_t` return values equal to
//! `-ZSTD_ErrorCode` (wrapped to `usize`); `ZSTD_isError` recognizes them by
//! being larger than `-(ZSTD_error_maxCode)`. [`code`] builds such values,
//! [`ErrorCode::from_raw`] inverts them, and [`decode`]/[`encode`] map the
//! zstdx error enums onto the closest libzstd code.

/// Mirrors `ZSTD_ErrorCode` from `zstd_errors.h` (numeric values included).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u32)]
pub enum ErrorCode {
    NoError = 0,
    Generic = 1,
    PrefixUnknown = 10,
    VersionUnsupported = 12,
    FrameParameterUnsupported = 14,
    FrameParameterWindowTooLarge = 16,
    CorruptionDetected = 20,
    ChecksumWrong = 22,
    LiteralsHeaderWrong = 24,
    DictionaryCorrupted = 30,
    DictionaryWrong = 32,
    DictionaryCreationFailed = 34,
    ParameterUnsupported = 40,
    ParameterCombinationUnsupported = 41,
    ParameterOutOfBound = 42,
    TableLogTooLarge = 44,
    MaxSymbolValueTooLarge = 46,
    MaxSymbolValueTooSmall = 48,
    CannotProduceUncompressedBlock = 49,
    StabilityConditionNotRespected = 50,
    StageWrong = 60,
    InitMissing = 62,
    MemoryAllocation = 64,
    WorkSpaceTooSmall = 66,
    DstSizeTooSmall = 70,
    SrcSizeWrong = 72,
    DstBufferNull = 74,
    NoForwardProgressDestFull = 80,
    NoForwardProgressInputEmpty = 82,
    FrameIndexTooLarge = 100,
    SeekableIo = 102,
    DstBufferWrong = 104,
    SrcBufferWrong = 105,
    SequenceProducerFailed = 106,
    ExternalSequencesInvalid = 107,
}

/// `ZSTD_error_maxCode` — never a valid return, only the recognizer bound.
pub const MAX_CODE: u32 = 120;

impl ErrorCode {
    /// The libzstd error-string table (`ERR_getErrorString`), as
    /// NUL-terminated C strings for `ZSTD_getErrorName`.
    #[must_use]
    pub fn name(self) -> &'static core::ffi::CStr {
        match self {
            Self::NoError => c"No error detected",
            Self::Generic => c"Error (generic)",
            Self::PrefixUnknown => c"Unknown frame descriptor",
            Self::VersionUnsupported => c"Version not supported",
            Self::FrameParameterUnsupported => c"Unsupported frame parameter",
            Self::FrameParameterWindowTooLarge => c"Frame requires too much memory for decoding",
            Self::CorruptionDetected => c"Data corruption detected",
            Self::ChecksumWrong => c"Restored data doesn't match checksum",
            Self::LiteralsHeaderWrong => {
                c"Header of Literals' block doesn't respect format specification"
            },
            Self::ParameterUnsupported => c"Unsupported parameter",
            Self::ParameterCombinationUnsupported => c"Unsupported combination of parameters",
            Self::ParameterOutOfBound => c"Parameter is out of bound",
            Self::TableLogTooLarge => c"tableLog requires too much memory : unsupported",
            Self::MaxSymbolValueTooLarge => c"Unsupported max Symbol Value : too large",
            Self::MaxSymbolValueTooSmall => c"Specified maxSymbolValue is too small",
            Self::CannotProduceUncompressedBlock => {
                c"This mode cannot generate an uncompressed block"
            },
            Self::StabilityConditionNotRespected => {
                c"pledged buffer stability condition is not respected"
            },
            Self::DictionaryCorrupted => c"Dictionary is corrupted",
            Self::DictionaryWrong => c"Dictionary mismatch",
            Self::DictionaryCreationFailed => c"Cannot create Dictionary from provided samples",
            Self::DstSizeTooSmall => c"Destination buffer is too small",
            Self::SrcSizeWrong => c"Src size is incorrect",
            Self::DstBufferNull => c"Operation on NULL destination buffer",
            Self::NoForwardProgressDestFull => {
                c"Operation made no progress over multiple calls, due to output buffer being full"
            },
            Self::NoForwardProgressInputEmpty => {
                c"Operation made no progress over multiple calls, due to input being empty"
            },
            Self::FrameIndexTooLarge => c"Frame index is too large",
            Self::SeekableIo => c"An I/O error occurred when reading/seeking",
            Self::DstBufferWrong => c"Destination buffer is wrong",
            Self::SrcBufferWrong => c"Source buffer is wrong",
            Self::SequenceProducerFailed => {
                c"Block-level external sequence producer returned an error code"
            },
            Self::ExternalSequencesInvalid => c"External sequences are not valid",
            Self::StageWrong => c"Operation not authorized at current processing stage",
            Self::MemoryAllocation => c"Allocation error : not enough memory",
            Self::WorkSpaceTooSmall | Self::InitMissing => c"Unspecified error code",
        }
    }

    /// Invert a libzstd `size_t` return into its code: `no_error` when the
    /// value is not in the error range, and gap values (the enum has none)
    /// collapse to `no_error` as well.: `no_error` when the
    /// value is not in the error range, and gap values (the enum has none)
    /// collapse to `no_error` as well.
    #[must_use]
    pub fn from_raw(raw: usize) -> Self {
        if !is_error(raw) {
            return Self::NoError;
        }
        let value = (raw as isize).wrapping_neg() as u32;
        match value {
            0 => Self::NoError,
            1 => Self::Generic,
            10 => Self::PrefixUnknown,
            12 => Self::VersionUnsupported,
            14 => Self::FrameParameterUnsupported,
            16 => Self::FrameParameterWindowTooLarge,
            20 => Self::CorruptionDetected,
            22 => Self::ChecksumWrong,
            24 => Self::LiteralsHeaderWrong,
            30 => Self::DictionaryCorrupted,
            32 => Self::DictionaryWrong,
            34 => Self::DictionaryCreationFailed,
            40 => Self::ParameterUnsupported,
            41 => Self::ParameterCombinationUnsupported,
            42 => Self::ParameterOutOfBound,
            44 => Self::TableLogTooLarge,
            46 => Self::MaxSymbolValueTooLarge,
            48 => Self::MaxSymbolValueTooSmall,
            49 => Self::CannotProduceUncompressedBlock,
            50 => Self::StabilityConditionNotRespected,
            60 => Self::StageWrong,
            62 => Self::InitMissing,
            64 => Self::MemoryAllocation,
            66 => Self::WorkSpaceTooSmall,
            70 => Self::DstSizeTooSmall,
            72 => Self::SrcSizeWrong,
            74 => Self::DstBufferNull,
            80 => Self::NoForwardProgressDestFull,
            82 => Self::NoForwardProgressInputEmpty,
            100 => Self::FrameIndexTooLarge,
            102 => Self::SeekableIo,
            104 => Self::DstBufferWrong,
            105 => Self::SrcBufferWrong,
            106 => Self::SequenceProducerFailed,
            107 => Self::ExternalSequencesInvalid,
            _ => Self::NoError, // maxCode and any future code have no enum slot
        }
    }
}

/// The libzstd error string for a raw `size_t` return (gap values in the
/// error range report libzstd's unspecified-code name).
#[must_use]
pub fn name_raw(raw: usize) -> &'static core::ffi::CStr {
    if !is_error(raw) {
        return ErrorCode::NoError.name();
    }
    match ErrorCode::from_raw(raw) {
        ErrorCode::NoError => c"Unspecified error code",
        code => code.name(),
    }
}

/// `ZSTD_isError`: every value above `-(maxCode)` is an error return.
#[must_use]
pub fn is_error(code: usize) -> bool {
    code >= usize::wrapping_neg(MAX_CODE as usize)
}

/// Build the libzstd `size_t` return value for `code`.
#[must_use]
pub fn code(code: ErrorCode) -> usize {
    usize::wrapping_neg(code as u64 as usize)
}

/// Which entry point is mapping a decode failure (kept for the entries
/// whose codes still differ between the paths).
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum DecodeSite {
    OneShot,
    Stream,
}

/// Map a high-level zstdx decode error to the libzstd code.
#[must_use]
pub fn decode(error: &zstdx::Error, site: DecodeSite) -> ErrorCode {
    match error {
        zstdx::Error::Frame(e) => frame(e, site),
        zstdx::Error::Dictionary(_) => ErrorCode::DictionaryCorrupted,
        other => common(other),
    }
}

/// Map the streaming decoder's own error type (same table as [`decode`]).
#[must_use]
pub fn frame(error: &zstdx::decoding::errors::FrameDecoderError, site: DecodeSite) -> ErrorCode {
    use zstdx::decoding::errors::{FrameDecoderError as Fde, ReadFrameHeaderError};
    match error {
        Fde::ReadFrameHeaderError(ReadFrameHeaderError::BadMagicNumber(_)) => {
            // The reference reports prefix_unknown on both paths (its
            // srcSize_wrong remap needs a completed frame in front).
            let _ = site;
            ErrorCode::PrefixUnknown
        },
        Fde::ReadFrameHeaderError(_) if is_truncated(error) => ErrorCode::SrcSizeWrong,
        Fde::ReadFrameHeaderError(_) => ErrorCode::CorruptionDetected,
        Fde::FrameHeaderError(_) if is_truncated(error) => ErrorCode::SrcSizeWrong,
        Fde::FrameHeaderError(_) => ErrorCode::CorruptionDetected,
        Fde::WindowSizeTooBig { .. } => ErrorCode::FrameParameterWindowTooLarge,
        Fde::DictionaryDecodeError(_) => ErrorCode::DictionaryCorrupted,
        Fde::FailedToReadBlockHeader(_) if is_truncated(error) => ErrorCode::SrcSizeWrong,
        Fde::FailedToReadBlockHeader(_) => ErrorCode::CorruptionDetected,
        Fde::FailedToReadBlockBody(_) if is_truncated(error) => ErrorCode::SrcSizeWrong,
        Fde::FailedToReadBlockBody(_) => ErrorCode::CorruptionDetected,
        Fde::FailedToReadChecksum(_) if is_truncated(error) => ErrorCode::SrcSizeWrong,
        Fde::FailedToReadChecksum(_) => ErrorCode::ChecksumWrong,
        Fde::ChecksumMismatch { .. } => ErrorCode::ChecksumWrong,
        Fde::ContentSizeMismatch { .. } => ErrorCode::SrcSizeWrong,
        Fde::NotYetInitialized | Fde::FailedToInitialize(_) => ErrorCode::StageWrong,
        Fde::FailedToDrainDecodebuffer(_) | Fde::FailedToSkipFrame => ErrorCode::Generic,
        Fde::TargetTooSmall => ErrorCode::DstSizeTooSmall,
        Fde::DictNotProvided { .. } => ErrorCode::DictionaryWrong,
        _ => ErrorCode::CorruptionDetected,
    }
}

/// Map a high-level zstdx encode error to the libzstd code.
#[must_use]
pub fn encode(error: &zstdx::Error) -> ErrorCode {
    match error {
        zstdx::Error::Dictionary(_) => ErrorCode::DictionaryCorrupted,
        zstdx::Error::Unsupported { .. } => ErrorCode::ParameterUnsupported,
        zstdx::Error::PledgedSizeMismatch { .. } => ErrorCode::SrcSizeWrong,
        other => common(other),
    }
}

fn common(error: &zstdx::Error) -> ErrorCode {
    match error {
        zstdx::Error::Io(e) if e.kind() == std::io::ErrorKind::UnexpectedEof => {
            ErrorCode::SrcSizeWrong
        },
        zstdx::Error::Io(_) => ErrorCode::Generic,
        zstdx::Error::Parameter(_) => ErrorCode::StageWrong,
        zstdx::Error::UnreadOutput { .. } => ErrorCode::StageWrong,
        // Non-exhaustive upstream enum; the decode/encode entry points have
        // already claimed their own variants.
        _ => ErrorCode::Generic,
    }
}

/// Walk the error chain looking for an `UnexpectedEof` — the shape every
/// "input ended early" path funnels into, whatever the nesting (libzstd
/// reports those as `srcSize_wrong`).
fn is_truncated(error: &(dyn std::error::Error + 'static)) -> bool {
    let mut source = Some(error);
    while let Some(e) = source {
        if let Some(io) = e.downcast_ref::<std::io::Error>() {
            if io.kind() == std::io::ErrorKind::UnexpectedEof {
                return true;
            }
        }
        source = e.source();
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn error_values_match_libzstd_layout() {
        assert_eq!(ErrorCode::Generic as u32, 1);
        assert_eq!(ErrorCode::SrcSizeWrong as u32, 72);
        assert_eq!(code(ErrorCode::SrcSizeWrong), usize::MAX - 71);
        assert!(is_error(code(ErrorCode::Generic)));
        assert!(is_error(code(ErrorCode::ExternalSequencesInvalid)));
        assert!(!is_error(0));
        assert!(!is_error(119));
        assert_eq!(
            ErrorCode::from_raw(code(ErrorCode::ChecksumWrong)),
            ErrorCode::ChecksumWrong
        );
        assert_eq!(ErrorCode::from_raw(0), ErrorCode::NoError);
        assert_eq!(ErrorCode::from_raw(17), ErrorCode::NoError);
    }

    #[test]
    fn names_match_reference_strings() {
        assert_eq!(
            ErrorCode::PrefixUnknown.name().to_bytes(),
            b"Unknown frame descriptor"
        );
        assert_eq!(
            ErrorCode::SrcSizeWrong.name().to_bytes(),
            b"Src size is incorrect"
        );
        assert_eq!(
            ErrorCode::DstSizeTooSmall.name().to_bytes(),
            b"Destination buffer is too small"
        );
        assert_eq!(ErrorCode::NoError.name().to_bytes(), b"No error detected");
    }
}
