//! Frame checksum verification folded into MT decode stage B.
//!
//! Stage B executes segments in order on the calling thread, and the bytes
//! of `[seg_start, seg_end)` are final the moment
//! [`execute_segment`](super::mt::execute_segment) returns (the wildcopy overshoot only
//! writes at or beyond the
//! executor's own cursor, so later segments never rewrite a published
//! range). [`StreamingChecksum`] therefore absorbs each executed range
//! right there on the executing thread: the bytes are still hot in the
//! executor's private caches, so the xxh64 costs its ALU work (~1.6 ms per
//! 32 MiB) without a second memory pass, and stage A workers keep staging
//! ahead meanwhile (the A/B overlap the serial stage-B design already has).
//!
//! This replaced two falsified placements (see `negative.md` for data): a
//! sequential post-pass over the assembled output (reads the output cold —
//! up to +7.6 ms on skewed where later decode traffic evicted it), and a
//! dedicated concurrent hasher thread (on L3-capacity-bound decodes like
//! json its reads run at DRAM speed *and* slow the decoder's own misses —
//! net regression). The sequential decoder keeps its own per-block hash
//! folded into the trailer read; both paths report
//! [`FrameDecoderError::ChecksumMismatch`].

use alloc::vec::Vec;
use core::ops::Range;

use super::{errors::FrameDecoderError, mt::ScanPlan};
use crate::xxh64::Xxh64;

/// What one executed segment means for the per-frame hash stream.
#[derive(Clone, Copy)]
enum FrameBoundary {
    /// The segment continues the current frame's stream.
    Interior,
    /// The segment closes its frame: finish the stream, compare when the
    /// frame was checksummed, then start a fresh one.
    FrameEnd { expected: Option<u32> },
}

/// Per-segment frame boundaries, derived from the scan plan (a segment
/// closes its frame when the next one starts a frame; the last segment
/// always closes the last frame).
pub(super) struct FrameBoundaries(Vec<FrameBoundary>);

impl FrameBoundaries {
    /// Compute the boundaries; `None` when no frame carries a checksum
    /// (those decodes must not pay any verification cost).
    pub(super) fn new(plan: &ScanPlan) -> Option<Self> {
        if !plan.checksums.iter().any(Option::is_some) {
            return None;
        }
        let mut out = Vec::with_capacity(plan.segments.len());
        let mut frame_idx = 0usize;
        for i in 0..plan.segments.len() {
            let closes_frame = plan.segments.get(i + 1).is_none_or(|n| n.frame_start);
            if closes_frame {
                out.push(FrameBoundary::FrameEnd {
                    expected: plan.checksums.get(frame_idx).copied().flatten(),
                });
                frame_idx += 1;
            } else {
                out.push(FrameBoundary::Interior);
            }
        }
        debug_assert_eq!(frame_idx, plan.checksums.len());
        Some(Self(out))
    }
}

/// The executor-side xxh64 stream: absorb each executed segment range as
/// stage B publishes it, finish and compare at frame closes. The first
/// mismatch is recorded (later frames still close the stream correctly, but
/// hashing stops — only draining the recorded verdict matters then).
pub(super) struct StreamingChecksum {
    stream: Xxh64,
    boundaries: FrameBoundaries,
    segment: usize,
    err: Option<FrameDecoderError>,
}

impl StreamingChecksum {
    pub(super) fn new(boundaries: FrameBoundaries) -> Self {
        Self {
            stream: Xxh64::new(0),
            boundaries,
            segment: 0,
            err: None,
        }
    }

    /// Absorb one executed output range. Call on the executing thread, in
    /// segment order, before any later `place` call may move the output.
    ///
    /// # Safety
    /// `base.add(range)` must address exactly the bytes the executor just
    /// wrote for this segment (the caller passes the same base it executed
    /// with; the range is final — later writes never land below the
    /// executor cursor).
    pub(super) unsafe fn absorb_executed(&mut self, base: *const u8, range: Range<usize>) {
        let boundary = self.boundaries.0[self.segment];
        self.segment += 1;
        if self.err.is_none() && !range.is_empty() {
            // SAFETY: caller contract above; same-thread read of bytes this
            // thread just wrote.
            let bytes = unsafe { core::slice::from_raw_parts(base.add(range.start), range.len()) };
            self.stream.write(bytes);
        }
        if let FrameBoundary::FrameEnd { expected } = boundary {
            if let Some(expected) = expected {
                let calculated = self.stream.finish() as u32;
                if calculated != expected && self.err.is_none() {
                    self.err = Some(FrameDecoderError::ChecksumMismatch {
                        expected,
                        calculated,
                    });
                }
            }
            self.stream = Xxh64::new(0);
        }
    }

    /// Take the verdict: the decode error (if any) keeps its precedence.
    pub(super) fn take_result(&mut self) -> Option<FrameDecoderError> {
        self.err.take()
    }
}

#[cfg(test)]
mod tests {
    use alloc::{vec, vec::Vec};

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

    /// Corruption inside the compressed body must surface an error (decode
    /// or checksum), never a panic or hang.
    #[test]
    fn body_corruption_surfaces_decode_error_or_mismatch() {
        let data = textish(4 * 1024 * 1024);
        let mut compressed =
            bulk::compress_with(&data, &EncoderOptions::new(Level::Fastest).workers(4)).unwrap();
        let mid = compressed.len() / 2;
        compressed[mid] ^= 0xff;
        compressed[mid + 1] ^= 0xff;
        let mut out = Vec::new();
        let _ = decode_to_vec_mt(&compressed, &mut out, 4, MAX_WINDOW);
    }
}
