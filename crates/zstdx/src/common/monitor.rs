//! The mutex-condvar monitor: one condvar per mutex, carried in one struct.

use core::time::Duration;
use std::sync::{Condvar, Mutex, MutexGuard, WaitTimeoutResult};

/// A mutex with a private condvar, paired one to one.
///
/// POSIX requires a condvar to be waited on with exactly one mutex across
/// every wait: glibc silently tolerates one condvar covering several
/// mutexes, macOS pthread aborts the process on the second mutex. Carrying
/// both in one struct with these methods as the only entry points makes the
/// pairing structural — the condvar is unreachable on its own, and `wait`
/// only accepts a guard of this monitor's guarded type, so a foreign
/// waiter class cannot share it.
///
/// A waiter whose predicate also observes other state (a poison slot,
/// atomics) re-checks it between waits; a publisher changing that other
/// state must broadcast to every monitor whose waiters re-check it, so no
/// waiter class sleeps through the change (see the mt pools' poison
/// broadcasts).
pub(crate) struct Monitor<T> {
    state: Mutex<T>,
    ready: Condvar,
}

impl<T> Monitor<T> {
    pub(crate) fn new(state: T) -> Self {
        Self {
            state: Mutex::new(state),
            ready: Condvar::new(),
        }
    }

    /// Lock the guarded state. Every mutation happens under this guard.
    pub(crate) fn lock(&self) -> MutexGuard<'_, T> {
        self.state.lock().unwrap()
    }

    /// Park until a notify on this monitor, atomically releasing and
    /// re-acquiring `guard`.
    pub(crate) fn wait<'a>(&self, guard: MutexGuard<'a, T>) -> MutexGuard<'a, T> {
        self.ready.wait(guard).unwrap()
    }

    /// [`Self::wait`] under a bound. The caller's loop re-checks its
    /// predicate either way, so the timeout result goes unobserved.
    pub(crate) fn wait_timeout<'a>(
        &self,
        guard: MutexGuard<'a, T>,
        limit: Duration,
    ) -> (MutexGuard<'a, T>, WaitTimeoutResult) {
        self.ready.wait_timeout(guard, limit).unwrap()
    }

    /// Wake every waiter parked on this monitor.
    pub(crate) fn notify_all(&self) {
        self.ready.notify_all();
    }
}
