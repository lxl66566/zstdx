//! Bounds how many memory-heavy lib tests run at once.
//!
//! The paranoid gate runs the whole lib suite in one libtest process at
//! default parallelism. The heavy tail (cap-scale stream rows, raised-window
//! LDM grids, ladder sequences) holds hundreds of MiB live per test, and the
//! process-wide pools pin what a finished test touched for as long as the
//! process lives, so the unbounded peak sat near 5 GiB. On this 13.7 GiB
//! host with zram swap and earlyoom at 5 percent free, any co-tenant holding
//! a few GiB turned the gate into an earlyoom kill — the R115 round's
//! "parallel stall" (see docs/src/dev/testing.md and the workflow pitfalls).
//!
//! A [`heavy_mem`] lease admits a test through a share-counted gate sized by
//! its measured solo peak, and dropping it returns this thread's pooled
//! buffers and trims the worker-state pool, so a finished test pins nothing
//! past its lease. A test whose solo peak crosses 100 MiB takes a lease;
//! past 400 MiB it takes [`HeavyMem::CapScale`] and runs alone.

// The no-default-features suite links std only through the test tree's
// `extern crate std`, which does not reach a child module's bare `std::`.
extern crate std;

use std::sync::{Condvar, Mutex};

/// Shares the gate admits at once. Mid tests overlap four-wide; a cap-scale
/// row holds all of them and runs alone.
const SHARES: u32 = 4;

/// A heavy test's live-memory class, from its measured solo peak under the
/// paranoid profile (the table lives in docs/src/dev/testing.md).
pub(crate) enum HeavyMem {
    /// Past 400 MiB: the whole gate.
    CapScale,
    /// 100-400 MiB: one share.
    Mid,
}

impl HeavyMem {
    fn shares(self) -> u32 {
        match self {
            HeavyMem::CapScale => SHARES,
            HeavyMem::Mid => 1,
        }
    }
}

static HELD: Mutex<u32> = Mutex::new(0);
static FREED: Condvar = Condvar::new();

/// Admission lease for one heavy test: make
/// `let _mem = heavy_mem(HeavyMem::Mid);` the test's first statement.
/// Blocks until the class's shares are free, releases them on drop. A
/// guarded test must not call into another guarded path, so the gate stays
/// non-reentrant.
pub(crate) fn heavy_mem(class: HeavyMem) -> HeavyMemLease {
    let want = class.shares();
    let mut held = HELD.lock().unwrap();
    while *held + want > SHARES {
        held = FREED.wait(held).unwrap();
    }
    *held += want;
    HeavyMemLease { shares: want }
}

pub(crate) struct HeavyMemLease {
    shares: u32,
}

impl Drop for HeavyMemLease {
    fn drop(&mut self) {
        // Release what the test pinned before the shares go back, so the
        // next admission also bounds retained memory.
        release_pinned_pools();
        let mut held = HELD.lock().unwrap();
        *held -= self.shares;
        drop(held);
        FREED.notify_all();
    }
}

fn release_pinned_pools() {
    // The pools exist only under the std feature; the no-default-features
    // suite keeps the gate for concurrency bounding alone.
    #[cfg(feature = "std")]
    {
        crate::stream::encoder_mt::release_retained_buf_for_tests();
        crate::encoding::frame_compressor::clear_state_pool_for_tests();
    }
}
