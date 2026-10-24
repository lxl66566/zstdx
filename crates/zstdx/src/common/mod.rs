//! Values and interfaces shared between the encoding side
//! and the decoding side.

// --- FRAMES ---
/// This magic number is included at the start of a single Zstandard frame
pub const MAGIC_NUM: u32 = 0xfd2f_b528;
/// Window size refers to the minimum amount of memory needed to decode any given frame.
///
/// The minimum window size is defined as 1 KB
pub const MIN_WINDOW_SIZE: u64 = 1024;
/// Window size refers to the minimum amount of memory needed to decode any given frame.
///
/// The wire format can express windows up to 3.75 TB (windowLog 41), but no
/// libzstd build decodes beyond `ZSTD_WINDOWLOG_MAX` (31 on 64-bit): the
/// header parse rejects a larger log outright and `ZSTD_d_windowLogMax`
/// cannot be raised above it, so no reference tool ever accepts a window
/// over 2^31. Align with that absolute ceiling.
pub const MAX_WINDOW_SIZE: u64 = 1 << 31;

// --- BLOCKS ---
/// While the spec limits block size to 128KB, the implementation uses
/// 128kibibytes
///
/// <https://github.com/facebook/zstd/blob/eca205fc7849a61ab287492931a04960ac58e031/doc/educational_decoder/zstd_decompress.c#L28-L29>
pub const MAX_BLOCK_SIZE: u32 = 128 * 1024;

/// Decompressed-output maximum for a single block (RFC 8878
/// `Block_Maximum_Size`): a block may never produce more than the frame's
/// declared window or [`MAX_BLOCK_SIZE`], whichever is smaller. libzstd
/// enforces the same bound as `fParams.blockSizeMax`. The decode side checks
/// this before sizing any per-block allocation so a hostile frame claiming
/// gigabytes of output is rejected instead of being reserved for.
pub const fn max_block_output(window_size: usize) -> usize {
    if window_size < MAX_BLOCK_SIZE as usize {
        window_size
    } else {
        MAX_BLOCK_SIZE as usize
    }
}

/// libzstd's buffered-streaming output buffer size, which doubles as the
/// streaming decode acceptance envelope (`ZSTD_decodingBufferSize_internal`:
/// `window + 2 * blockSize + 2 * WILDCOPY_OVERLENGTH`, with the window
/// floored at 1 KiB by `ZSTD_decompressStream` before sizing and the whole
/// size capped by a declared Frame_Content_Size). The streaming decoder
/// wraps this buffer when a fully flushed generation cannot fit one more
/// block (`outStart + blockSizeMax > outBuffSize`), so every generation is
/// at least `window + blockSize + 64` long — which is what makes the
/// per-sequence check `offset <= bytes decoded since the last wrap` a
/// window check: in-window offsets never reach below the generation start.
pub const fn streaming_buffer_cap(window_size: usize, fcs: Option<u64>) -> usize {
    let window = if window_size < MIN_WINDOW_SIZE as usize {
        MIN_WINDOW_SIZE as usize
    } else {
        window_size
    };
    let needed = window + 2 * max_block_output(window_size) + 64;
    match fcs {
        Some(fcs) if fcs < needed as u64 => fcs as usize,
        _ => needed,
    }
}

/// Runtime SIMD tier clamp (`ZSTDX_SIMD_FORCE`): the dev override that
/// lowers runtime kernel dispatch to a narrower tier for honest
/// measurement on wider hardware.
#[cfg(feature = "std")]
pub(crate) mod simd;
