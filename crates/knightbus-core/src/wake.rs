//! Backend-to-host notification contract.
use std::{sync::Mutex, task::Waker};

/// Stores one host scheduler. Notifications never run while the lock is held.
/// Register before checking backend state; a wake schedules another bounded turn.
#[derive(Default)]
pub struct WakeSource(Mutex<Option<Waker>>);
impl WakeSource {
    /// Replace or remove the scheduler. Always wake a new scheduler so work
    /// published before registration cannot be missed.
    pub fn register(&self, waker: Option<Waker>) {
        let wake = waker.clone();
        *self.0.lock().unwrap() = waker;
        if let Some(wake) = wake {
            wake.wake();
        }
    }
    /// Call after publishing a state change, outside backend state locks.
    pub fn notify(&self) {
        let wake = self.0.lock().unwrap().clone();
        if let Some(wake) = wake {
            wake.wake();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        sync::{
            Arc,
            atomic::{AtomicUsize, Ordering},
        },
        task::Wake,
    };
    #[derive(Default)]
    struct Counter(AtomicUsize);
    impl Wake for Counter {
        fn wake(self: Arc<Self>) {
            self.0.fetch_add(1, Ordering::SeqCst);
        }
    }
    #[test]
    fn registration_covers_earlier_work_and_replacement_stops_old_notifications() {
        let source = WakeSource::default();
        let first = Arc::new(Counter::default());
        let second = Arc::new(Counter::default());
        source.notify();
        source.register(Some(first.clone().into()));
        source.notify();
        source.register(Some(second.clone().into()));
        source.notify();
        source.register(None);
        source.notify();
        assert_eq!(first.0.load(Ordering::SeqCst), 2);
        assert_eq!(second.0.load(Ordering::SeqCst), 2);
    }
    #[test]
    fn scheduler_can_unregister_during_a_wakeup() {
        struct Reentrant(Arc<WakeSource>);
        impl Wake for Reentrant {
            fn wake(self: Arc<Self>) {
                self.0.register(None);
            }
        }
        let source = Arc::new(WakeSource::default());
        source.register(Some(Arc::new(Reentrant(source.clone())).into()));
        source.notify();
    }
}
