//! Host-independent bounded worker. Native objects never cross thread boundaries.
use crate::constants::{limits, status, usb};
use crate::{MAX_ISO, MAX_TRANSFER, SLOTS};
use knightbus_core::{
    Budget, Completion, Device, DeviceKey, Error, ExporterBackend, MemoryReservation, Result,
    SessionId, Transfer, TransferId,
};
use std::{
    sync::{Arc, Condvar, Mutex},
    task::{Poll, Wake, Waker},
    thread,
};

/// Native admission result: only a busy endpoint may be retried in place.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SubmitError {
    /// Native ownership prevents admission until a completion/state notification.
    Busy,
    /// Terminal admission failure, including scratch-budget exhaustion.
    Failed(Error),
}
impl From<SubmitError> for Error {
    fn from(error: SubmitError) -> Self {
        match error {
            SubmitError::Busy => Self::Exhausted,
            SubmitError::Failed(error) => error,
        }
    }
}

// A latched notification uses its own lock: callbacks must never acquire the
// mailbox lock held while the host polls. Publish native state before waking.
#[derive(Default)]
struct Notification {
    pending: Mutex<bool>,
    ready: Condvar,
    #[cfg(test)]
    waits: std::sync::atomic::AtomicUsize,
}
impl Notification {
    fn notify(&self) {
        *self.pending.lock().unwrap() = true;
        self.ready.notify_one();
    }
    fn clear(&self) {
        *self.pending.lock().unwrap() = false;
    }
    fn wait(&self, retry: bool) {
        let pending = self.pending.lock().unwrap();
        if *pending {
            return;
        }
        #[cfg(test)]
        self.waits
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        if retry {
            drop(
                self.ready
                    .wait_timeout_while(pending, limits::CLEANUP_RETRY_INTERVAL, |p| !*p)
                    .unwrap(),
            );
        } else {
            drop(self.ready.wait_while(pending, |p| !*p).unwrap());
        }
    }
}
impl Wake for Notification {
    fn wake(self: Arc<Self>) {
        self.notify();
    }
    fn wake_by_ref(self: &Arc<Self>) {
        self.notify();
    }
}

fn pipe(transfer: &Transfer) -> usize {
    let header = transfer.header();
    let direction =
        if header.endpoint != 0 && header.direction == knightbus_core::usbip::Direction::In {
            usb::IN_ENDPOINT_OFFSET
        } else {
            0
        };
    header.endpoint as usize + direction
}

/// Native ownership boundary. Implementations retain transfers until terminal
/// callbacks, and must never unwind while native code can access freed storage.
pub trait Driver {
    /// Install the worker notification before polling/submitting. Wake after
    /// publishing completions, removal, or any state that makes Busy retryable.
    fn set_waker(&mut self, waker: Waker);
    /// Admit a request or return its unchanged owner. Called in endpoint order.
    #[allow(clippy::result_large_err)]
    fn submit(
        &mut self,
        id: TransferId,
        transfer: Transfer,
    ) -> std::result::Result<(), (SubmitError, Transfer)>;
    /// Request cancellation; only `completion` acknowledges retirement.
    /// An API error requests a timed retry without releasing native storage.
    fn cancel(&mut self, id: TransferId) -> Result<()>;
    /// Poll a terminal callback, including failed submissions.
    fn completion(&mut self) -> Option<(TransferId, Completion)>;
    /// Device termination; reported even when there are no transfers.
    fn removed(&self) -> bool;
    /// Release physical ownership after every transfer retired.
    fn release(&mut self) -> Result<()>;
}

enum Slot {
    Free,
    Queued(TransferId, Transfer),
    Running(TransferId, usize),
    Done(TransferId, Completion),
}
impl Slot {
    fn id(&self) -> Option<TransferId> {
        match self {
            Self::Free => None,
            Self::Queued(id, _) | Self::Running(id, _) | Self::Done(id, _) => Some(*id),
        }
    }
}
#[derive(Clone, Copy)]
enum State {
    Idle,
    Opening,
    Active,
    Failed(Error),
    Retired,
}
struct Mailbox {
    state: State,
    stop: bool,
    gone: bool,
    slots: [Slot; SLOTS],
    cancelled: [bool; SLOTS],
    order: [u64; SLOTS],
    next_order: u64,
    _credit: MemoryReservation,
}
struct Shared {
    mailbox: Mutex<Mailbox>,
    wake: Arc<Notification>,
}

/// One physical device backend, suitable for a shared engine instance. A factory
/// opens native objects on the worker only after the engine authorizes a claim.
pub struct Backend<F> {
    factory: Option<F>,
    device: Device,
    session: Option<SessionId>,
    shared: Arc<Shared>,
    removal_reported: bool,
    next: usize,
}
impl<F> Backend<F> {
    /// Reserve bounded worker metadata before accepting any native ownership.
    pub fn new(device: Device, budget: &Budget, factory: F) -> Result<Self> {
        let credit = budget.reserve_memory(
            std::mem::size_of::<Shared>()
                + device.storage_bytes()?
                + limits::WORKER_METADATA_ALLOWANCE,
        )?;
        Ok(Self {
            factory: Some(factory),
            device,
            session: None,
            shared: Arc::new(Shared {
                mailbox: Mutex::new(Mailbox {
                    state: State::Idle,
                    stop: false,
                    gone: false,
                    slots: std::array::from_fn(|_| Slot::Free),
                    cancelled: [false; SLOTS],
                    order: [0; SLOTS],
                    next_order: 0,
                    _credit: credit,
                }),
                wake: Arc::new(Notification::default()),
            }),
            removal_reported: false,
            next: 0,
        })
    }
    fn lock(&self) -> std::sync::MutexGuard<'_, Mailbox> {
        self.shared.mailbox.lock().unwrap()
    }
}
impl<D: Driver, F: FnOnce() -> Result<D> + Send + 'static> ExporterBackend for Backend<F> {
    fn manages_topology(&self) -> bool {
        true
    }
    fn claim(&mut self, session: SessionId, device: &Device) -> Poll<Result<()>> {
        if device != &self.device
            || session.epoch == 0
            || session.serial == 0
            || self.session.is_some_and(|s| s != session)
        {
            return Poll::Ready(Err(Error::Invalid));
        }
        if let Some(factory) = self.factory.take() {
            self.session = Some(session);
            self.lock().state = State::Opening;
            let shared = self.shared.clone();
            if thread::Builder::new()
                .name("knightbus-macos-usb".into())
                .spawn(move || worker(shared, factory))
                .is_err()
            {
                self.lock().state = State::Failed(Error::Unavailable);
            }
        }
        match self.lock().state {
            State::Active => Poll::Ready(Ok(())),
            State::Failed(e) => Poll::Ready(Err(e)),
            State::Retired => Poll::Ready(Err(Error::State)),
            _ => Poll::Pending,
        }
    }
    fn submit(
        &mut self,
        id: TransferId,
        transfer: Transfer,
    ) -> std::result::Result<(), (Error, Transfer)> {
        let mut m = self.lock();
        if self.session != Some(id.session) || !matches!(m.state, State::Active) || m.stop || m.gone
        {
            return Err((Error::Unavailable, transfer));
        }
        if id.sequence != transfer.header().seqnum
            || transfer.buffer().len() > MAX_TRANSFER
            || transfer.layout().len() > MAX_ISO
            || transfer.header().endpoint >= usb::IN_ENDPOINT_OFFSET as u32
        {
            return Err((Error::Invalid, transfer));
        }
        if m.slots.iter().any(|s| s.id() == Some(id)) {
            return Err((Error::Invalid, transfer));
        }
        let Some(i) = m.slots.iter().position(|s| matches!(s, Slot::Free)) else {
            return Err((Error::Exhausted, transfer));
        };
        let Some(next_order) = m.next_order.checked_add(1) else {
            return Err((Error::Exhausted, transfer));
        };
        m.order[i] = m.next_order;
        m.next_order = next_order;
        m.slots[i] = Slot::Queued(id, transfer);
        m.cancelled[i] = false;
        drop(m);
        self.shared.wake.notify();
        Ok(())
    }
    fn cancel(&mut self, id: TransferId) {
        let mut m = self.lock();
        if let Some(i) = m.slots.iter().position(|s| s.id() == Some(id)) {
            m.cancelled[i] = true;
        }
        drop(m);
        self.shared.wake.notify();
    }
    fn poll_completion(&mut self) -> Option<(TransferId, Completion)> {
        let mut m = self.lock();
        for n in 0..SLOTS {
            let i = (self.next + n) % SLOTS;
            if matches!(m.slots[i], Slot::Done(..)) {
                let Slot::Done(id, done) = std::mem::replace(&mut m.slots[i], Slot::Free) else {
                    unreachable!()
                };
                drop(m);
                self.next = (i + 1) % SLOTS;
                return Some((id, done));
            }
        }
        None
    }
    fn release(&mut self, session: SessionId, device: DeviceKey) -> Poll<Result<()>> {
        if self.session.is_none() {
            return Poll::Ready(Ok(()));
        }
        if self.session != Some(session) || device != self.device.key {
            return Poll::Ready(Err(Error::Invalid));
        }
        let mut m = self.lock();
        m.stop = true;
        let result = match m.state {
            State::Retired | State::Failed(_) => Poll::Ready(Ok(())),
            _ => Poll::Pending,
        };
        drop(m);
        self.shared.wake.notify();
        result
    }
    fn poll_removed(&mut self) -> Option<DeviceKey> {
        if !self.removal_reported && self.lock().gone {
            self.removal_reported = true;
            Some(self.device.key)
        } else {
            None
        }
    }
}
impl<F> Drop for Backend<F> {
    fn drop(&mut self) {
        self.lock().stop = true;
        self.shared.wake.notify();
    }
}
fn worker<D: Driver>(shared: Arc<Shared>, factory: impl FnOnce() -> Result<D>) {
    let mut driver = match factory() {
        Ok(d) => d,
        Err(e) => {
            shared.mailbox.lock().unwrap().state = State::Failed(e);
            return;
        }
    };
    driver.set_waker(Waker::from(shared.wake.clone()));
    shared.mailbox.lock().unwrap().state = State::Active;
    let mut active_barrier = None;
    loop {
        // Consume the latch BEFORE observing state. A callback/admission between
        // the last observation and wait leaves it set, so no wakeup is lost.
        shared.wake.clear();
        let mut progress = false;
        let mut retry_cleanup = false;
        let removed = driver.removed();
        let mut m = shared.mailbox.lock().unwrap();
        m.gone |= removed;
        if removed {
            m.stop = true;
        }
        let mut blocked = [false; usb::ENDPOINT_DIRECTION_SLOTS];
        for slot in &m.slots {
            if let Slot::Running(_, pipe) = slot {
                blocked[*pipe] = true;
            }
        }
        // Correlation sequence numbers need not be monotonic. Preserve actual
        // admission order across slot reuse and sequence-number wraparound.
        let pass_end = m.next_order;
        let mut indices: [usize; SLOTS] = std::array::from_fn(|i| i);
        indices.sort_unstable_by_key(|i| m.order[*i]);
        for i in indices {
            // New admissions while the lock was dropped belong to the next pass.
            if m.order[i] >= pass_end {
                continue;
            }
            let cancel = m.stop || m.cancelled[i];
            if !cancel && active_barrier.is_some() && matches!(m.slots[i], Slot::Queued(..)) {
                continue;
            }
            let barrier = match &m.slots[i] {
                Slot::Queued(_, t) => {
                    t.header().endpoint == 0
                        && t.header().setup[usb::SETUP_TYPE] & usb::REQUEST_TYPE_MASK == 0
                        && matches!(
                            t.header().setup[usb::SETUP_REQUEST],
                            usb::CLEAR_FEATURE | usb::SET_CONFIGURATION | usb::SET_INTERFACE
                        )
                }
                _ => false,
            };
            if barrier
                && !cancel
                && m.slots.iter().enumerate().any(|(j, slot)| {
                    matches!(slot, Slot::Running(..))
                        || (matches!(slot, Slot::Queued(..)) && m.order[j] < m.order[i])
                })
            {
                break;
            }
            if !cancel {
                if let Slot::Queued(_, transfer) = &m.slots[i] {
                    if blocked[pipe(transfer)] {
                        if barrier {
                            break;
                        }
                        continue;
                    }
                }
            }
            match std::mem::replace(&mut m.slots[i], Slot::Free) {
                Slot::Queued(id, transfer) if cancel => {
                    progress = true;
                    m.slots[i] = Slot::Done(
                        id,
                        Completion {
                            transfer,
                            status: if removed {
                                status::NO_DEVICE
                            } else {
                                status::CANCELLED
                            },
                            actual_length: 0,
                            start_frame: 0,
                        },
                    );
                }
                Slot::Queued(id, transfer) => {
                    let pipe = pipe(&transfer);
                    blocked[pipe] = true;
                    m.slots[i] = Slot::Running(id, pipe);
                    drop(m);
                    let result = driver.submit(id, transfer);
                    m = shared.mailbox.lock().unwrap();
                    match result {
                        Ok(()) => {
                            progress = true;
                            if barrier {
                                active_barrier = Some(id);
                            }
                        }
                        Err((SubmitError::Busy, transfer)) => {
                            // The head stays queued. Never offer a later request
                            // on this pipe, even if the driver would accept it.
                            m.slots[i] = Slot::Queued(id, transfer);
                        }
                        Err((SubmitError::Failed(error), transfer)) => {
                            // Allocation failure is terminal ENOMEM, not Busy.
                            progress = true;
                            blocked[pipe] = false;
                            m.slots[i] = Slot::Done(
                                id,
                                Completion {
                                    transfer,
                                    status: status(error),
                                    actual_length: 0,
                                    start_frame: 0,
                                },
                            );
                        }
                    }
                }
                Slot::Running(id, pipe) if cancel => {
                    m.slots[i] = Slot::Running(id, pipe);
                    drop(m);
                    retry_cleanup |= driver.cancel(id).is_err();
                    m = shared.mailbox.lock().unwrap();
                }
                other => m.slots[i] = other,
            }
            if barrier {
                break;
            }
        }
        drop(m);
        for _ in 0..SLOTS {
            let Some((id, done)) = driver.completion() else {
                break;
            };
            progress = true;
            if active_barrier == Some(id) {
                active_barrier = None;
            }
            let mut m = shared.mailbox.lock().unwrap();
            if let Some(i) = m
                .slots
                .iter()
                .position(|s| matches!(s, Slot::Running(token, _) if *token == id))
            {
                m.slots[i] = Slot::Done(id, done);
            } else {
                m.stop = true;
            }
        }
        let m = shared.mailbox.lock().unwrap();
        if m.stop
            && !m
                .slots
                .iter()
                .any(|s| matches!(s, Slot::Running(..) | Slot::Queued(..)))
        {
            drop(m);
            if driver.release().is_ok() {
                // Publish retirement only after native objects and their memory
                // reservations have actually been dropped.
                drop(driver);
                shared.mailbox.lock().unwrap().state = State::Retired;
                return;
            }
            // Only failed abort/release APIs need a bounded timer retry. Idle
            // workers and pending native I/O sleep until an explicit wakeup.
            retry_cleanup = true;
        } else {
            drop(m);
        }
        if progress {
            continue;
        }
        shared.wake.wait(retry_cleanup);
    }
}
/// Stable USB/IP status for admission errors. Native IOReturn mapping is separate.
pub fn status(error: Error) -> i32 {
    match error {
        Error::Denied => status::ACCESS,
        Error::NotFound | Error::Unavailable => status::NO_DEVICE,
        Error::Unsupported => status::UNSUPPORTED,
        Error::Deadline => status::TIMED_OUT,
        Error::Cancelled => status::CANCELLED,
        Error::Exhausted => status::NO_MEMORY,
        Error::Invalid => status::INVALID,
        _ => status::IO,
    }
}

#[cfg(test)]
#[path = "backend_tests.rs"]
mod tests;
