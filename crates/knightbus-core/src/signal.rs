//! Standard callback notification for native APIs that can invoke Rust wakeups.
use crate::{Budget, MemoryReservation, Result, importer::Signal};
use std::sync::{Condvar, Mutex};

/// A charged, latched wake signal for native callback implementations.
pub struct ThreadSignal {
    pending: Mutex<bool>,
    ready: Condvar,
    _reservation: MemoryReservation,
}
impl Signal for ThreadSignal {
    fn new(budget: &Budget) -> Result<Self> {
        Ok(Self {
            pending: Mutex::new(false),
            ready: Condvar::new(),
            _reservation: budget
                .reserve_memory(std::mem::size_of::<Self>() + 2 * std::mem::size_of::<usize>())?,
        })
    }
    fn notify(&self) {
        *self.pending.lock().unwrap() = true;
        self.ready.notify_one();
    }
    fn wait(&self) -> Result<()> {
        let mut pending = self
            .ready
            .wait_while(self.pending.lock().unwrap(), |p| !*p)
            .unwrap();
        *pending = false;
        Ok(())
    }
}
impl std::task::Wake for ThreadSignal {
    fn wake(self: std::sync::Arc<Self>) {
        self.notify();
    }
    fn wake_by_ref(self: &std::sync::Arc<Self>) {
        self.notify();
    }
}
