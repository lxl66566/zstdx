//! Allocation shape of the frame decoder's output buffers.
//!
//! A counting global allocator measures this binary's Rust allocations:
//! every test must run inside `ALLOC_LOCK`, because the tracker is
//! process-global. Inputs are built before tracking starts, so the peak
//! covers only the measured decode.

use std::{
    alloc::{GlobalAlloc, Layout, System},
    sync::{
        Mutex, OnceLock,
        atomic::{AtomicIsize, AtomicUsize, Ordering},
    },
};

use zstdx::decoding::{BlockDecodingStrategy, Dictionary, FrameDecoder};

struct Counting;

// isize so a dealloc of a pre-tracking allocation underflows to negative
// instead of wrapping the counter and poisoning the peak.
static LIVE: AtomicIsize = AtomicIsize::new(0);
static PEAK: AtomicUsize = AtomicUsize::new(0);
static TRACK: AtomicUsize = AtomicUsize::new(0);

unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, l: Layout) -> *mut u8 {
        let p = unsafe { System.alloc(l) };
        if !p.is_null() && TRACK.load(Ordering::Relaxed) != 0 {
            let size = l.size() as isize;
            let live = LIVE.fetch_add(size, Ordering::Relaxed) + size;
            if live > 0 {
                PEAK.fetch_max(live as usize, Ordering::Relaxed);
            }
        }
        p
    }

    unsafe fn dealloc(&self, p: *mut u8, l: Layout) {
        if TRACK.load(Ordering::Relaxed) != 0 {
            LIVE.fetch_sub(l.size() as isize, Ordering::Relaxed);
        }
        unsafe { System.dealloc(p, l) }
    }
}

#[global_allocator]
static A: Counting = Counting;

fn alloc_lock() -> &'static Mutex<()> {
    static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| Mutex::new(()))
}

/// Peak live allocation bytes while `f` runs. Everything `f` allocates must
/// also be freed inside it, or the peak drifts up by the remainder.
fn peak<T>(f: impl FnOnce() -> T) -> (T, usize) {
    LIVE.store(0, Ordering::SeqCst);
    PEAK.store(0, Ordering::SeqCst);
    TRACK.store(1, Ordering::SeqCst);
    let out = f();
    TRACK.store(0, Ordering::SeqCst);
    (out, PEAK.load(Ordering::SeqCst))
}

/// A 9-byte frame: no content size, window descriptor for a `wlog`
/// window, one empty last Raw block. Decodes to nothing.
fn empty_frame_with_window(wlog: u32) -> Vec<u8> {
    let mut out = Vec::new();
    out.extend_from_slice(&0xfd2f_b528u32.to_le_bytes());
    out.push(0x00);
    out.push(((wlog - 10) as u8) << 3);
    out.extend_from_slice(&[0x01, 0x00, 0x00]);
    out
}

fn decode_oneshot(input: &[u8]) -> Result<usize, String> {
    let mut out = Vec::new();
    out.reserve(1 << 16);
    FrameDecoder::new()
        .decode_all_to_vec(input, &mut out)
        .map(|()| out.len())
        .map_err(|e| format!("{e}"))
}

/// A reused decoder must not reserve the next frame's declared window at
/// reset: the flat output path owns the decode and the ring only activates
/// on a force_dict takeover, so a second 9-byte frame with a 2^27 window
/// descriptor stays in the first frame's allocation class.
#[test]
fn reused_decoder_does_not_reserve_the_declared_window() {
    let _guard = alloc_lock().lock().unwrap();
    let frame = empty_frame_with_window(27); // 2^27 = 128 MiB window
    let single = [frame.clone()].concat();
    let pair = [frame.clone(), frame].concat();

    let (single_res, single_peak) = peak(|| decode_oneshot(&single));
    let (pair_res, pair_peak) = peak(|| decode_oneshot(&pair));

    assert_eq!(single_res, Ok(0));
    assert_eq!(pair_res, Ok(0));
    assert!(
        single_peak < 1 << 20,
        "single frame peak {single_peak} B should stay tiny"
    );
    assert!(
        pair_peak < 1 << 20,
        "two frames peak {pair_peak} B must stay in the single frame's class"
    );
}

/// The ring's window-sized reserve moved with its activation point: a
/// force_dict takeover reserves the declared window on demand, so the ring
/// path still gets its one up-front allocation.
#[test]
fn force_dict_takeover_reserves_the_ring() {
    let _guard = alloc_lock().lock().unwrap();
    let frame = empty_frame_with_window(27);
    let dict = Dictionary::load(&[0xab; 64]).unwrap();

    let (finished, takeover_peak) = peak(|| {
        let mut dec = FrameDecoder::new();
        dec.add_dict(dict).unwrap();
        let mut src: &[u8] = &frame;
        dec.reset(&mut src).unwrap();
        dec.force_dict(0).unwrap();
        dec.decode_blocks(&mut src, BlockDecodingStrategy::All)
            .unwrap()
    });
    assert!(finished, "the frame must finish on the ring path");
    assert!(
        takeover_peak > 64 << 20,
        "takeover peak {takeover_peak} B should show the window reserve"
    );
}
