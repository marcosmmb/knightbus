//! Host-independent bounded worker. Native objects never cross thread boundaries.
use crate::usbip::status;
use knightbus_usb as usb;
const SLOTS: usize = 64;
mod limits {
    pub const WORKER_METADATA_ALLOWANCE: usize = 4096;
    pub const CLEANUP_RETRY_INTERVAL: std::time::Duration = std::time::Duration::from_millis(10);
}
use crate::{
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
    let direction = if header.endpoint != 0 && header.direction == crate::usbip::Direction::In {
        usb::IN_ENDPOINT_BIT_OFFSET as usize
    } else {
        0
    };
    header.endpoint as usize + direction
}

/// Native ownership boundary. Implementations retain transfers until terminal
/// callbacks, and must never unwind while native code can access freed storage.
pub trait Driver {
    /// Native feature and resource limits.
    const CAPABILITIES: crate::Capabilities;
    /// Install the worker notification before polling/submitting. Wake after
    /// publishing completions, removal, or any state that makes Busy retryable.
    fn set_waker(&mut self, waker: Waker);
    /// Report the endpoint currently selected by the OS; None means absent.
    fn endpoint(&self, address: u8) -> Option<crate::Endpoint>;
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
    host: crate::WakeSource,
}

impl Shared {
    fn notify_host(&self) {
        self.host.notify();
    }
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
    pub fn new<D: Driver>(device: Device, budget: &Budget, factory: F) -> Result<Self>
    where
        F: FnOnce() -> Result<D>,
    {
        D::CAPABILITIES.device(&device)?;
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
                host: crate::WakeSource::default(),
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
    fn set_waker(&mut self, waker: Option<Waker>) {
        self.shared.host.register(waker);
    }
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
            let device_id = self.device.wire_id();
            if thread::Builder::new()
                .name("knightbus-exporter".into())
                .spawn(move || worker(shared, factory, device_id))
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
            || transfer.buffer().len() > D::CAPABILITIES.transfer_bytes
            || transfer.layout().len() > D::CAPABILITIES.iso_packets
            || transfer.header().endpoint >= usb::IN_ENDPOINT_BIT_OFFSET as usize as u32
        {
            return Err((Error::Invalid, transfer));
        }
        if m.slots.iter().any(|s| s.id() == Some(id)) {
            return Err((Error::Invalid, transfer));
        }
        let Some(i) = m
            .slots
            .iter()
            .take(D::CAPABILITIES.requests)
            .position(|s| matches!(s, Slot::Free))
        else {
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
fn worker<D: Driver>(shared: Arc<Shared>, factory: impl FnOnce() -> Result<D>, device_id: u32) {
    let mut driver = match factory() {
        Ok(d) => d,
        Err(e) => {
            shared.mailbox.lock().unwrap().state = State::Failed(e);
            shared.notify_host();
            return;
        }
    };
    driver.set_waker(Waker::from(shared.wake.clone()));
    shared.mailbox.lock().unwrap().state = State::Active;
    shared.notify_host();
    Worker {
        shared,
        driver,
        active_barrier: None,
        device_id,
    }
    .run();
}

struct Worker<D> {
    shared: Arc<Shared>,
    driver: D,
    active_barrier: Option<TransferId>,
    device_id: u32,
}
impl<D: Driver> Worker<D> {
    fn run(mut self) {
        loop {
            // Clear before observing state so callbacks between observation and wait stay latched.
            self.shared.wake.clear();
            let (mut progress, mut retry_cleanup) = self.dispatch();
            progress |= self.retire_completions();
            if progress || self.shared.mailbox.lock().unwrap().gone {
                self.shared.notify_host();
            }
            let m = self.shared.mailbox.lock().unwrap();
            if m.stop
                && !m
                    .slots
                    .iter()
                    .any(|s| matches!(s, Slot::Running(..) | Slot::Queued(..)))
            {
                drop(m);
                if self.driver.release().is_ok() {
                    // Publish retirement only after native objects and their memory
                    // reservations have actually been dropped.
                    drop(self.driver);
                    self.shared.mailbox.lock().unwrap().state = State::Retired;
                    self.shared.notify_host();
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
            self.shared.wake.wait(retry_cleanup);
        }
    }
    fn dispatch(&mut self) -> (bool, bool) {
        let mut progress = false;
        let mut retry_cleanup = false;
        let removed = self.driver.removed();
        let mut m = self.shared.mailbox.lock().unwrap();
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
            if !cancel && self.active_barrier.is_some() && matches!(m.slots[i], Slot::Queued(..)) {
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
                    let endpoint = self.driver.endpoint(
                        transfer.header().endpoint as u8
                            | if transfer.header().direction == crate::usbip::Direction::In {
                                0x80
                            } else {
                                0
                            },
                    );
                    let result = match transfer.validate_native(
                        self.device_id,
                        endpoint.as_ref(),
                        D::CAPABILITIES,
                    ) {
                        Ok(_) => self.driver.submit(id, transfer),
                        Err(error) => Err((SubmitError::Failed(error), transfer)),
                    };
                    m = self.shared.mailbox.lock().unwrap();
                    match result {
                        Ok(()) => {
                            progress = true;
                            if barrier {
                                self.active_barrier = Some(id);
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
                    retry_cleanup |= self.driver.cancel(id).is_err();
                    m = self.shared.mailbox.lock().unwrap();
                }
                other => m.slots[i] = other,
            }
            if barrier {
                break;
            }
        }
        drop(m);
        (progress, retry_cleanup)
    }
    fn retire_completions(&mut self) -> bool {
        let mut progress = false;
        for _ in 0..SLOTS {
            let Some((id, done)) = self.driver.completion() else {
                break;
            };
            progress = true;
            if self.active_barrier == Some(id) {
                self.active_barrier = None;
            }
            let mut m = self.shared.mailbox.lock().unwrap();
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
        progress
    }
}
/// Stable USB/IP status for admission errors.
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
mod tests {
    //! Scheduling checks with direct visibility of waits, not timing thresholds.
    use super::*;
    use crate::{
        DeviceId, ResourceLimits,
        usbip::{Direction, SubmitHeader},
    };
    use std::sync::atomic::Ordering;
    use std::time::{Duration, Instant};

    struct HostWake(std::sync::atomic::AtomicUsize);
    impl std::task::Wake for HostWake {
        fn wake(self: Arc<Self>) {
            self.0.fetch_add(1, Ordering::Relaxed);
        }
        fn wake_by_ref(self: &Arc<Self>) {
            self.0.fetch_add(1, Ordering::Relaxed);
        }
    }

    struct Immediate {
        pending: Option<(TransferId, Transfer)>,
        wake: Option<Waker>,
    }
    impl Driver for Immediate {
        fn endpoint(&self, address: u8) -> Option<crate::Endpoint> {
            if !matches!(address, 0x81 | 0x82) {
                return None;
            }
            Some(crate::Endpoint {
                address,
                kind: crate::TransferKind::Bulk,
                max_packet_size: 64,
                interval: 0,
                allowed_flags: 0,
            })
        }
        const CAPABILITIES: crate::Capabilities = crate::Capabilities {
            transfer_bytes: 1024 * 1024,
            requests: 64,
            iso_packets: 1024,
            speeds: 0b1101110,
            descriptor_bytes: 65535,
            configurations: 16,
            remote_wakeup: true,
        };
        fn set_waker(&mut self, waker: Waker) {
            self.wake = Some(waker);
        }
        fn submit(
            &mut self,
            id: TransferId,
            transfer: Transfer,
        ) -> std::result::Result<(), (SubmitError, Transfer)> {
            assert!(self.pending.is_none(), "one active request per pipe");
            self.pending = Some((id, transfer));
            // Models completion inside enqueue, before the worker reaches wait.
            self.wake.as_ref().unwrap().wake_by_ref();
            Ok(())
        }
        fn cancel(&mut self, _: TransferId) -> Result<()> {
            Ok(())
        }
        fn completion(&mut self) -> Option<(TransferId, Completion)> {
            let (id, transfer) = self.pending.take()?;
            Some((
                id,
                Completion {
                    transfer,
                    status: 0,
                    actual_length: 0,
                    start_frame: 0,
                },
            ))
        }
        fn removed(&self) -> bool {
            false
        }
        fn release(&mut self) -> Result<()> {
            assert!(self.pending.is_none());
            Ok(())
        }
    }

    #[test]
    fn sixty_four_immediate_completions_drain_without_inter_request_waits() {
        let budget = Budget::new(ResourceLimits {
            bytes: 4 * 1024 * 1024,
            requests: 64,
        });
        let descriptor = [18, 1, 0, 2, 0, 0, 0, 64, 9, 0x12, 1, 0, 0, 1, 0, 0, 0, 1];
        let config = vec![
            9, 2, 25, 0, 1, 1, 0, 0x80, 50, 9, 4, 0, 0, 1, 0xff, 0, 0, 0, 7, 5, 0x81, 2, 64, 0, 0,
        ];
        let device = crate::descriptors::device(
            DeviceKey {
                id: DeviceId(1),
                generation: 1,
            },
            crate::descriptors::Address {
                path: b"test",
                bus: 1,
                device: 0x1000,
            },
            2,
            &descriptor,
            &[config],
            1,
        )
        .unwrap();
        let factory = || {
            Ok(Immediate {
                pending: None,
                wake: None,
            })
        };
        let mut backend = Backend::new(device, &budget, factory).unwrap();
        let host = Arc::new(HostWake(std::sync::atomic::AtomicUsize::new(0)));
        backend.set_waker(Some(Waker::from(host.clone())));
        let session = SessionId {
            epoch: 1,
            serial: 1,
        };
        // Preload a complete burst before starting the worker, eliminating host
        // scheduling races from the assertion about inter-request waits.
        backend.session = Some(session);
        backend.lock().state = State::Active;
        for sequence in 1..=SLOTS as u32 {
            let transfer = Transfer::new(
                &budget,
                SubmitHeader {
                    seqnum: sequence,
                    device_id: 0x00011000,
                    direction: Direction::In,
                    endpoint: 1,
                    transfer_flags: 0,
                    transfer_buffer_length: 8,
                    start_frame: 0,
                    number_of_packets: 0,
                    interval: 0,
                    setup: [0; 8],
                },
                &[],
            )
            .unwrap();
            backend
                .submit(TransferId { session, sequence }, transfer)
                .unwrap();
        }
        let shared = backend.shared.clone();
        let worker_shared = shared.clone();
        let factory = backend.factory.take().unwrap();
        let start = Instant::now();
        let thread = std::thread::spawn(move || worker(worker_shared, factory, 0x00011000));
        while backend
            .lock()
            .slots
            .iter()
            .filter(|s| matches!(s, Slot::Done(..)))
            .count()
            != SLOTS
        {
            assert!(start.elapsed() < Duration::from_secs(3));
            std::thread::yield_now();
        }
        assert!(
            host.0.load(Ordering::Relaxed) > 1,
            "native progress must wake the host"
        );
        backend.set_waker(None);
        let before = host.0.load(Ordering::Relaxed);
        backend.shared.notify_host();
        assert_eq!(host.0.load(Ordering::Relaxed), before);
        // Zero waits while dispatching the burst; one idle wait is allowed after it.
        assert!(shared.wake.waits.load(Ordering::Relaxed) <= 1);
        eprintln!(
            "64 immediate requests retired in {:?}; no inter-request waits",
            start.elapsed()
        );
        drop(backend);
        thread.join().unwrap();
        drop(shared);
        assert_eq!(budget.usage().bytes, 0);
    }

    #[test]
    fn notification_before_wait_is_latched() {
        let wake = Arc::new(Notification::default());
        let cloned = wake.clone();
        let (tx, rx) = std::sync::mpsc::channel();
        let thread = std::thread::spawn(move || {
            cloned.clear();
            Waker::from(cloned.clone()).wake();
            cloned.wait(false);
            tx.send(()).unwrap();
        });
        rx.recv_timeout(Duration::from_secs(3)).unwrap();
        thread.join().unwrap();
        assert_eq!(wake.waits.load(Ordering::Relaxed), 0);
    }
}
