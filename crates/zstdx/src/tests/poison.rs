//! Poison-wall sweep over both encoder cores: the debug poison margins
//! (see `crate::encoding::poison`) armed over the cores' owned slack fire
//! inside the encoder on any touch past a buffer's logical length, so a
//! green roundtrip is itself the walls' proof — no explicit assertions
//! here. The sweep only has to drive the matrix hard enough to cross
//! every arming site: the full numeric level ladder (every strategy
//! family) x size-boundary payloads x the option bits (checksum, pledged
//! size, forced window) on the bulk one-shot and the streaming cores
//! (single-threaded and multithreaded), each cell decoding back through
//! this crate's own decoder.

use alloc::{format, vec::Vec};

/// Payload sizes crossing the wire boundaries: the size-format steps
/// (0/1/3/7/8/9/15), the literals-section format steps (63/64/511), mid
/// blocks (750/1500), the multi-block straddle (4095) and the 64 KiB step
/// (65537).
const SIZES: [usize; 14] = [0, 1, 3, 7, 8, 9, 15, 63, 64, 511, 750, 1500, 4095, 65537];

/// One option-bit combination: (checksum, pledged size, forced window
/// log). The full cross product with the ladder would be 16 cells per
/// (level, size); the sweep rotates one combination per cell instead —
/// every combination runs on every level across the size ladder — keeping
/// the debug matrix inside its wall budget.
type Bits = (bool, bool, Option<u32>);

const BITS: [Bits; 16] = [
    (false, false, None),
    (true, false, None),
    (false, true, None),
    (true, true, None),
    (false, false, Some(9)), // clamps up to 10: the forced-window clamp path
    (true, false, Some(9)),
    (false, true, Some(9)),
    (true, true, Some(9)),
    (false, false, Some(10)),
    (true, false, Some(10)),
    (false, true, Some(10)),
    (true, true, Some(10)),
    (false, false, Some(17)),
    (true, false, Some(17)),
    (false, true, Some(17)),
    (true, true, Some(17)),
];

/// Deterministic xorshift64 payload source.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }

    fn byte(&mut self) -> u8 {
        (self.next() >> 33) as u8
    }
}

/// A text template with sprinkled fresh noise: matches, RLE-ish runs and
/// entropy tails in every block, deterministic per size (the seed mixes
/// the length).
fn payload(len: usize) -> Vec<u8> {
    const TEMPLATE: &[u8] = b"{\"id\":123,\"name\":\"user\",\"tags\":[\"a\",\"b\"],\"score\":42}\n";
    let mut rng = Rng(0x9e37_79b9_7f4a_7c15 ^ (len as u64).wrapping_mul(0x2545_f491_4f6c_dd1d));
    let mut data = Vec::with_capacity(len);
    while data.len() < len {
        let take = TEMPLATE.len().min(len - data.len());
        data.extend_from_slice(&TEMPLATE[..take]);
        if rng.next() & 7 == 0 {
            let noise = 1 + rng.next() as usize % 64;
            for _ in 0..noise.min(len - data.len()) {
                data.push(rng.byte());
            }
        }
    }
    data
}

/// The payloads for [`SIZES`], built once per test.
fn payloads() -> Vec<Vec<u8>> {
    SIZES.iter().map(|&len| payload(len)).collect()
}

fn cell_options(
    (checksum, pledged, window_log): Bits,
    level: crate::Level,
    len: usize,
    workers: u32,
) -> crate::EncoderOptions {
    let mut opts = crate::EncoderOptions::new(level)
        .checksum(checksum)
        .workers(workers);
    if pledged {
        opts = opts.pledged_size(Some(len as u64));
    }
    if let Some(log) = window_log {
        opts = opts.with_input_shape(crate::InputShape::default().with_window_log(log));
    }
    opts
}

/// How a stream cell feeds its payload: one write (the common case), a
/// few odd-sized chunks (block straddles through the staging window), or
/// many small writes with a mid-stream flush (partial-block emission on
/// the cores' flush walls).
#[derive(Clone, Copy)]
enum Feed {
    Single,
    Wide,
    TinyFlush,
}

impl Feed {
    fn from_index(i: usize) -> Self {
        match i % 3 {
            0 => Self::Single,
            1 => Self::Wide,
            _ => Self::TinyFlush,
        }
    }
}

fn feed<W: crate::io::Write>(
    enc: &mut crate::stream::write::Encoder<W>,
    data: &[u8],
    policy: Feed,
    label: &str,
) {
    use crate::io::Write as _;
    let fail = |what: &str| -> ! { panic!("{label}: stream {what} failed") };
    match policy {
        Feed::Single => enc.write_all(data).unwrap_or_else(|_| fail("write")),
        Feed::Wide => {
            // Three-ish odd chunks straddle block and staging boundaries.
            let chunk = ((data.len() / 3).max(1)) | 1;
            for start in (0..data.len()).step_by(chunk) {
                let end = (start + chunk).min(data.len());
                enc.write_all(&data[start..end])
                    .unwrap_or_else(|_| fail("write"));
            }
        },
        Feed::TinyFlush => {
            let chunk = if data.len() > 4096 {
                1024
            } else {
                7
            };
            let mut flushed = false;
            let mut start = 0;
            while start < data.len() {
                let end = (start + chunk).min(data.len());
                enc.write_all(&data[start..end])
                    .unwrap_or_else(|_| fail("write"));
                start = end;
                if !flushed && start >= data.len() / 2 {
                    enc.flush().unwrap_or_else(|_| fail("flush"));
                    flushed = true;
                }
            }
            if !flushed && !data.is_empty() {
                enc.flush().unwrap_or_else(|_| fail("flush"));
            }
        },
    }
}

/// Compress through the streaming core with `workers` workers and decode
/// the frame back through this crate's own decoder.
fn stream_cell(data: &[u8], opts: crate::EncoderOptions, policy: Feed, label: &str) {
    let mut enc = crate::stream::write::Encoder::with_options(Vec::new(), opts)
        .unwrap_or_else(|e| panic!("{label}: encoder init failed: {e:?}"));
    feed(&mut enc, data, policy, label);
    let frame = enc
        .finish()
        .unwrap_or_else(|e| panic!("{label}: finish failed: {e:?}"));
    let back = crate::bulk::decompress(&frame, data.len())
        .unwrap_or_else(|e| panic!("{label}: own decode failed: {e:?}"));
    assert_eq!(back, data, "{label}: roundtrip bytes differ");
}

/// The bulk one-shot: `bulk::compress` on the all-default cells (it is
/// the fixed-options entry), `bulk::compress_with` everywhere else.
#[test]
fn poison_bulk_ladder() {
    let payloads = payloads();
    for li in 0..=22usize {
        let level = crate::Level::from_zstd(li as i32);
        for (si, data) in payloads.iter().enumerate() {
            let bits = BITS[(li * SIZES.len() + si) % BITS.len()];
            let label = format!("bulk l{li} len {}", data.len());
            let frame = if (li * SIZES.len() + si) % BITS.len() == 0 {
                crate::bulk::compress(data, level)
            } else {
                crate::bulk::compress_with(data, &cell_options(bits, level, data.len(), 0))
                    .unwrap_or_else(|e| panic!("{label}: compress failed: {e:?}"))
            };
            let back = crate::bulk::decompress(&frame, data.len())
                .unwrap_or_else(|e| panic!("{label}: own decode failed: {e:?}"));
            assert_eq!(
                back.as_slice(),
                data.as_slice(),
                "{label}: roundtrip bytes differ"
            );
        }
    }
}

/// The single-threaded streaming core: the write/finish/flush/drain walls
/// arm at every public method exit.
#[test]
fn poison_stream_st_ladder() {
    let payloads = payloads();
    for li in 0..=22usize {
        let level = crate::Level::from_zstd(li as i32);
        for (si, data) in payloads.iter().enumerate() {
            let idx = li * SIZES.len() + si;
            let bits = BITS[idx % BITS.len()];
            let policy = Feed::from_index(idx);
            let label = format!("st l{li} len {}", data.len());
            stream_cell(
                data,
                cell_options(bits, level, data.len(), 0),
                policy,
                &label,
            );
        }
    }
}

/// The multithreaded streaming core (workers > 1): worker-side arming and
/// the consumer's assemble/donation assertions ride the sweep the same
/// way. On single-core machines the ST core serves the cells instead and
/// stays green.
#[test]
#[cfg(feature = "std")]
fn poison_stream_mt_ladder() {
    let payloads = payloads();
    for li in 0..=22usize {
        let level = crate::Level::from_zstd(li as i32);
        for (si, data) in payloads.iter().enumerate() {
            let idx = li * SIZES.len() + si;
            let bits = BITS[(idx + 5) % BITS.len()];
            let policy = Feed::from_index(idx + 1);
            let label = format!("mt l{li} len {}", data.len());
            stream_cell(
                data,
                cell_options(bits, level, data.len(), 2),
                policy,
                &label,
            );
        }
    }
}

/// Multi-job MT leg: a payload past the job grid (`MIN_JOB_SIZE` = 1 MiB)
/// engages the worker pool — the worker-side arming and the consumer's
/// assemble/donation assertions — which the size ladder's sub-job cells
/// never reach. A compact level set keeps the debug cost bounded; the
/// feeds and bits rotate the same way the ladder's cells do.
#[test]
#[cfg(feature = "std")]
fn poison_stream_mt_multijob() {
    let data = payload(3 * 1024 * 1024 + 123);
    for li in [1usize, 5, 9] {
        let label = format!("mt-multi l{li}");
        stream_cell(
            &data,
            cell_options(BITS[li], crate::Level::from_zstd(li as i32), data.len(), 3),
            Feed::from_index(li + 1),
            &label,
        );
    }
}
