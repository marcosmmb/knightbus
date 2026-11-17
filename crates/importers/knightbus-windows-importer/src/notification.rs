//! Latched worker notification shared by native arrivals, replies and stop.
use knightbus_core::Result;

/// Latched signal shared by the worker and its native driver.
pub struct Notification {
    signal: platform::Signal,
    _reservation: knightbus_core::MemoryReservation,
}
impl Notification {
    pub(crate) fn new(budget: &knightbus_core::Budget) -> Result<Self> {
        let reservation = budget
            .reserve_memory(std::mem::size_of::<Self>() + 2 * std::mem::size_of::<usize>())?;
        Ok(Self {
            signal: platform::Signal::new()?,
            _reservation: reservation,
        })
    }
    /// Publish work before signaling. Multiple notifications may coalesce.
    pub fn notify(&self) {
        self.signal.notify();
    }
    pub(crate) fn wait(&self) -> Result<()> {
        self.signal.wait()
    }
    #[cfg(windows)]
    pub(crate) fn handle(&self) -> windows_sys::Win32::Foundation::HANDLE {
        self.signal.handle()
    }
}

#[cfg(not(windows))]
mod platform {
    use super::*;
    use std::sync::{Condvar, Mutex};

    /// Latched wake signal for a native worker.
    pub struct Signal {
        pending: Mutex<bool>,
        ready: Condvar,
    }
    impl Signal {
        pub(crate) fn new() -> Result<Self> {
            Ok(Self {
                pending: Mutex::new(false),
                ready: Condvar::new(),
            })
        }
        pub(crate) fn wait(&self) -> Result<()> {
            let mut pending = self
                .ready
                .wait_while(self.pending.lock().unwrap(), |p| !*p)
                .unwrap();
            *pending = false;
            Ok(())
        }
        pub fn notify(&self) {
            *self.pending.lock().unwrap() = true;
            self.ready.notify_one();
        }
    }
}

#[cfg(windows)]
mod platform {
    use super::*;
    use knightbus_core::Error;
    use std::ptr;
    use windows_sys::Win32::{
        Foundation::{CloseHandle, HANDLE, WAIT_OBJECT_0},
        System::Threading::{CreateEventW, INFINITE, SetEvent, WaitForMultipleObjects},
    };

    struct Handle(HANDLE);
    // These owned kernel handles support cross-thread signaling/waiting. Drop
    // runs only after their owner is gone; no wait retains a raw handle afterward.
    unsafe impl Send for Handle {}
    unsafe impl Sync for Handle {}
    impl Handle {
        fn new(handle: HANDLE) -> Result<Self> {
            if handle.is_null() {
                Err(Error::Unavailable)
            } else {
                Ok(Self(handle))
            }
        }
    }
    impl Drop for Handle {
        fn drop(&mut self) {
            unsafe {
                CloseHandle(self.0);
            }
        }
    }

    /// Latched wake signal for a native worker.
    pub struct Signal {
        wake: Handle,
        native: Handle,
    }
    impl Signal {
        pub(crate) fn new() -> Result<Self> {
            // Unnamed, non-inherited, auto-reset event. Signals survive until
            // the single worker consumes them, including signals before wait.
            Ok(Self {
                wake: Handle::new(unsafe { CreateEventW(ptr::null(), 0, 0, ptr::null()) })?,
                native: Handle::new(unsafe { CreateEventW(ptr::null(), 0, 0, ptr::null()) })?,
            })
        }
        pub(crate) fn handle(&self) -> HANDLE {
            self.native.0
        }
        pub(crate) fn wait(&self) -> Result<()> {
            let handles = [self.wake.0, self.native.0];
            match unsafe {
                WaitForMultipleObjects(handles.len() as u32, handles.as_ptr(), 0, INFINITE)
            } {
                status if status < WAIT_OBJECT_0 + handles.len() as u32 => Ok(()),
                _ => Err(Error::Unavailable),
            }
        }
        pub fn notify(&self) {
            // Shared ownership keeps this event valid through worker shutdown.
            unsafe {
                SetEvent(self.wake.0);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use knightbus_core::{Budget, ResourceLimits};
    use std::{
        sync::{Arc, mpsc},
        thread,
        time::Duration,
    };
    #[test]
    fn wakes_survive_before_wait_and_between_state_check_and_wait() {
        let budget = Budget::new(ResourceLimits {
            bytes: 4096,
            requests: 1,
        });
        let wake = Arc::new(Notification::new(&budget).unwrap());
        let (ready_tx, ready_rx) = mpsc::sync_channel(0);
        let (done_tx, done_rx) = mpsc::sync_channel(0);
        let worker = wake.clone();
        wake.notify();
        wake.notify();
        let join = thread::spawn(move || {
            worker.wait().unwrap();
            for _ in 0..100 {
                ready_tx.send(()).unwrap(); // State was checked; signal may race wait.
                worker.wait().unwrap();
                done_tx.send(()).unwrap();
            }
        });
        for _ in 0..100 {
            ready_rx.recv_timeout(Duration::from_secs(2)).unwrap();
            wake.notify();
            done_rx.recv_timeout(Duration::from_secs(2)).unwrap();
        }
        join.join().unwrap();
        assert!(budget.usage().bytes > 0);
        drop(wake);
        assert_eq!(budget.usage().bytes, 0);
    }
}
