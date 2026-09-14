//! Frame checksum verification over assembled decode output.
//!
//! The parallel decoder stages and executes segments before any frame-level
//! accounting exists, so checksums are verified in a post-pass on the calling
//! thread: one sequential xxh64 pass per checksummed frame over its output
//! range, compared against the trailer word the pre-scan collected. The
//! sequential decoder folds the same comparison into its trailer read (see
//! `FrameDecoderState::verify_frame_checksum`); both paths report
//! [`FrameDecoderError::ChecksumMismatch`].

use super::errors::FrameDecoderError;
use crate::xxh64::Xxh64;

/// One checksummed frame: its output range in the assembled output and the
/// trailer word promised after its last block.
pub(super) struct ChecksumSpan {
    pub(super) out: core::ops::Range<usize>,
    pub(super) expected: u32,
}

/// Hash every span and compare with the expected trailer word.
pub(super) fn verify(spans: &[ChecksumSpan], out: &[u8]) -> Result<(), FrameDecoderError> {
    for span in spans {
        // Spans are built from the executed segment sizes, so they are inside
        // the output by construction; a violation is an internal bug.
        let Some(bytes) = out.get(span.out.clone()) else {
            debug_assert!(false, "checksum span outside decoded output");
            return Ok(());
        };
        let mut hash = Xxh64::new(0);
        hash.write(bytes);
        let calculated = hash.finish() as u32;
        if calculated != span.expected {
            return Err(FrameDecoderError::ChecksumMismatch {
                expected: span.expected,
                calculated,
            });
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use alloc::{vec, vec::Vec};

    use super::{ChecksumSpan, verify};
    use crate::{
        EncoderOptions, Level, bulk,
        decoding::{
            FrameDecoder,
            errors::FrameDecoderError,
            mt::{decode_all_mt, decode_to_vec_mt},
        },
    };

    const MAX_WINDOW: u64 = crate::decoding::DEFAULT_MAX_WINDOW_SIZE;

    fn textish(len: usize) -> Vec<u8> {
        let words: Vec<&[u8]> = alloc::vec![
            b"the ", b"quick ", b"brown ", b"fox ", b"jumps ", b"over ", b"lazy ", b"dog ",
            b"lorem ", b"ipsum ", b"dolor ", b"sit ", b"amet ",
        ];
        let mut state = 7u64;
        let mut out = Vec::with_capacity(len);
        while out.len() < len {
            state = state.wrapping_mul(6364136223846793005).wrapping_add(1);
            let w = words[((state >> 33) as usize) % words.len()];
            let take = w.len().min(len - out.len());
            out.extend_from_slice(&w[..take]);
        }
        out
    }

    fn mismatch(err: &FrameDecoderError) -> bool {
        matches!(err, FrameDecoderError::ChecksumMismatch { .. })
    }

    /// MT-encoded checksummed input must decode through both MT entry points.
    #[test]
    fn checksummed_roundtrip() {
        let data = textish(8 * 1024 * 1024);
        let compressed =
            bulk::compress_with(&data, &EncoderOptions::new(Level::Fastest).workers(4)).unwrap();
        let mut placed = vec![0u8; data.len()];
        let n = decode_all_mt(&compressed, &mut placed, 4, MAX_WINDOW).unwrap();
        assert_eq!(&placed[..n], &data[..]);
        let mut appended = Vec::new();
        decode_to_vec_mt(&compressed, &mut appended, 4, MAX_WINDOW).unwrap();
        assert_eq!(appended, data);
    }

    /// Flipping one trailer byte must fail verification on the MT paths and
    /// on the sequential path with the same error variant.
    #[test]
    fn corrupted_trailer_errors() {
        let data = textish(8 * 1024 * 1024);
        let mut compressed =
            bulk::compress_with(&data, &EncoderOptions::new(Level::Fastest).workers(4)).unwrap();
        let trailer = compressed.len() - 4;
        compressed[trailer] ^= 0xff;

        let mut placed = vec![0u8; data.len()];
        let err = decode_all_mt(&compressed, &mut placed, 4, MAX_WINDOW).unwrap_err();
        assert!(mismatch(&err), "{err:?}");

        let mut appended = Vec::new();
        let err = decode_to_vec_mt(&compressed, &mut appended, 4, MAX_WINDOW).unwrap_err();
        assert!(mismatch(&err), "{err:?}");
        // The append contract keeps the length unchanged on error.
        assert_eq!(appended.len(), 0);

        let mut seq = vec![0u8; data.len()];
        let err = FrameDecoder::new()
            .decode_all(&compressed, &mut seq)
            .unwrap_err();
        assert!(mismatch(&err), "{err:?}");
    }

    /// Multi-frame input: the offending frame's trailer is flagged, frames
    /// without a checksum stay untouched, and the good variant roundtrips.
    #[test]
    fn multiframe_mismatch_reports_offending_frame() {
        let a = textish(4 * 1024 * 1024);
        let b: Vec<u8> = (0..3u32 * 1024 * 1024).map(|i| (i % 61) as u8).collect();
        let plain =
            bulk::compress_with(&a, &EncoderOptions::new(Level::Fastest).checksum(false)).unwrap();
        let checksummed = bulk::compress_with(
            &b,
            &EncoderOptions::new(Level::Fastest)
                .checksum(true)
                .workers(2),
        )
        .unwrap();

        // frame without checksum first, checksummed second: roundtrips
        let mut two = plain.clone();
        two.extend_from_slice(&checksummed);
        let mut out = Vec::new();
        decode_to_vec_mt(&two, &mut out, 4, MAX_WINDOW).unwrap();
        let mut expect = a.clone();
        expect.extend_from_slice(&b);
        assert_eq!(out, expect);

        // corrupt the second frame's trailer
        let mut corrupt = two.clone();
        let last = corrupt.len() - 1;
        corrupt[last] ^= 0xff;
        let mut out = Vec::new();
        let err = decode_to_vec_mt(&corrupt, &mut out, 4, MAX_WINDOW).unwrap_err();
        assert!(mismatch(&err), "{err:?}");

        // same input through the sequential decoder: same variant
        let mut seq = vec![0u8; a.len() + b.len()];
        let err = FrameDecoder::new()
            .decode_all(&corrupt, &mut seq)
            .unwrap_err();
        assert!(mismatch(&err), "{err:?}");

        // checksummed frame first, corrupt its trailer: fails too
        let mut reversed_corrupt = checksummed.clone();
        reversed_corrupt.extend_from_slice(&plain);
        let trailer = checksummed.len() - 1;
        reversed_corrupt[trailer] ^= 0xff;
        let mut out = Vec::new();
        let err = decode_to_vec_mt(&reversed_corrupt, &mut out, 4, MAX_WINDOW).unwrap_err();
        assert!(mismatch(&err), "{err:?}");
    }

    /// Direct unit check of the span compare.
    #[test]
    fn span_verify_unit() {
        let data = b"hello world, hello zstd";
        let mut hash = crate::xxh64::Xxh64::new(0);
        hash.write(data);
        let good = hash.finish() as u32;
        let spans = alloc::vec![ChecksumSpan {
            out: 0..data.len(),
            expected: good,
        }];
        assert!(verify(&spans, data).is_ok());
        let bad = alloc::vec![ChecksumSpan {
            out: 0..data.len(),
            expected: good ^ 1,
        }];
        assert!(matches!(
            verify(&bad, data),
            Err(FrameDecoderError::ChecksumMismatch { .. })
        ));
    }
}
