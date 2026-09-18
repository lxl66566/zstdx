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
/// The maximum window size allowed by the spec is 3.75TB
pub const MAX_WINDOW_SIZE: u64 = (1 << 41) + 7 * (1 << 38);

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
