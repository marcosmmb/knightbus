//! Nonblocking engine adapter around an isolated native-I/O worker.
//! The worker owns every native handle and scratch buffer through retirement.
use crate::{
    channel::{
        ABI_VERSION, Descriptors, EP0_SLOT_MASK, HEADER, MAX_TRANSFER, Request, SLOTS, STATS_BYTES,
        setup, stats, transfer, u64_at,
    },
    descriptors::{self as usb, Topology},
};
use knightbus_core::{
    Budget, Completion, Device, Error, ImporterBackend, MemoryReservation, Result, SessionId,
    Transfer,
    usbip::{Direction, SubmitHeader},
};
use std::{
    sync::{Arc, Condvar, Mutex},
    task::Poll,
    thread,
    time::Duration,
};

// Conservative admission allowances for kernel slots and Windows lookup storage.
const NATIVE_SLOT_METADATA_BYTES: usize = 256;
const NATIVE_LOOKUP_BYTES: usize = 80 * 1024;
const ARC_COUNTERS: usize = 2; // Strong and weak reference counts.
const IDLE_POLL_INTERVAL: Duration = Duration::from_millis(1);
const LINUX_EPIPE: i32 = -32; // USB/IP uses Linux status values even on Windows.

/// Synchronous native channel, used only on the backend's dedicated worker.
/// An implementation must close/drain its native handle when dropped.
pub trait Driver: Send + 'static {
    /// Publish validated descriptors. Failure still requires `detach`/Drop.
    fn attach(&mut self, bytes: &[u8]) -> Result<()>;
    /// Nonblocking fetch; zero means no request is currently available.
    fn fetch(&mut self, mask: u32, output: &mut [u8]) -> Result<usize>;
    /// Complete one token, or acknowledge an already canceled token.
    fn reply(&mut self, input: &[u8]) -> Result<()>;
    /// Snapshot native token liveness, used to deliver cancellation once.
    fn snapshot(&mut self, output: &mut [u8]) -> Result<()>;
    /// Retire the child and native requests. Error means cleanup is not proven.
    fn detach(&mut self) -> Result<()>;
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum State {
    Idle,
    Attaching,
    Active,
    Draining,
    Retired,
    Failed(Error),
}
struct Mailbox {
    state: State,
    stop: bool,
    native_retired: bool,
    requests: [Option<Transfer>; SLOTS],
    next_request: usize,
    replies: [Option<Completion>; SLOTS],
    expected: [Option<SubmitHeader>; SLOTS],
    cancellations: [Option<u32>; SLOTS],
    completing: [bool; SLOTS],
    _reservation: MemoryReservation,
}
struct Shared {
    mailbox: Mutex<Mailbox>,
    wake: Condvar,
}
/// One native attachment per backend; instantiate multiple backends with a
/// shared Budget for multiple devices. Native calls never execute in `tick`.
/// Drop signals shutdown; a stalled native worker retains its own storage.
pub struct Backend<D: Driver, F: FnOnce() -> Result<D> + Send + 'static> {
    factory: Option<F>,
    descriptors: Option<Descriptors>,
    shared: Arc<Shared>,
    budget: Budget,
    session: Option<SessionId>,
}
impl<D: Driver, F: FnOnce() -> Result<D> + Send + 'static> Backend<D, F> {
    /// Prepare a bounded worker. Factory opens the native handle on its own
    /// thread, allowing the same backend in embedded and service hosts.
    pub fn new(factory: F, descriptors: Descriptors, budget: Budget) -> Result<Self> {
        let reservation = budget.reserve_memory(
            std::mem::size_of::<Shared>() + ARC_COUNTERS * std::mem::size_of::<usize>(),
        )?;
        Ok(Self {
            factory: Some(factory),
            descriptors: Some(descriptors),
            budget,
            session: None,
            shared: Arc::new(Shared {
                mailbox: Mutex::new(Mailbox {
                    state: State::Idle,
                    stop: false,
                    native_retired: true,
                    requests: std::array::from_fn(|_| None),
                    next_request: 0,
                    replies: std::array::from_fn(|_| None),
                    expected: [None; SLOTS],
                    completing: [false; SLOTS],
                    cancellations: [None; SLOTS],
                    _reservation: reservation,
                }),
                wake: Condvar::new(),
            }),
        })
    }
    fn lock(&self) -> std::sync::MutexGuard<'_, Mailbox> {
        self.shared.mailbox.lock().expect("native mailbox")
    }
}
impl<D: Driver, F: FnOnce() -> Result<D> + Send + 'static> ImporterBackend for Backend<D, F> {
    fn attach(&mut self, session: SessionId, device: &Device) -> Poll<Result<()>> {
        if self.session.is_some_and(|s| s != session) {
            return Poll::Ready(Err(Error::State));
        }
        if let Some(descriptors) = &self.descriptors {
            if session.epoch == 0
                || session.serial == 0
                || device.wire.vendor_id != descriptors.topology.vid
                || device.wire.product_id != descriptors.topology.pid
                || device.wire.speed != descriptors.speed as u32
                || device.wire.bus_number > u32::from(u16::MAX)
                || device.wire.device_number > u32::from(u16::MAX)
            {
                return Poll::Ready(Err(Error::Invalid));
            }
        }
        if let Some(factory) = self.factory.take() {
            self.session = Some(session);
            self.lock().state = State::Attaching;
            let descriptors = self.descriptors.take().unwrap();
            let shared = self.shared.clone();
            let budget = self.budget.clone();
            let device_id = (device.wire.bus_number << u16::BITS) | device.wire.device_number;
            if thread::Builder::new()
                .name("knightbus-usb".into())
                .spawn(move || {
                    let result = run(factory, descriptors, &budget, &shared, device_id);
                    let mut m = shared.mailbox.lock().expect("native mailbox");
                    m.state = match result {
                        Ok(()) => State::Retired,
                        Err(e) => State::Failed(e),
                    };
                })
                .is_err()
            {
                self.lock().state = State::Failed(Error::Unavailable);
            }
        }
        match self.lock().state {
            State::Active => Poll::Ready(Ok(())),
            State::Failed(e) => Poll::Ready(Err(e)),
            State::Retired | State::Draining => Poll::Ready(Err(Error::State)),
            _ => Poll::Pending,
        }
    }
    fn next_transfer(&mut self, session: SessionId, _budget: &Budget) -> Option<Result<Transfer>> {
        if self.session != Some(session) {
            return Some(Err(Error::State));
        }
        let mut m = self.lock();
        if let State::Failed(e) = m.state {
            return Some(Err(e));
        }
        for offset in 0..SLOTS {
            let index = (m.next_request + offset) % SLOTS;
            if let Some(transfer) = m.requests[index].take() {
                m.next_request = (index + 1) % SLOTS;
                return Some(Ok(transfer));
            }
        }
        None
    }
    fn next_cancellation(&mut self, session: SessionId) -> Option<u32> {
        if self.session != Some(session) {
            return None;
        }
        let mut m = self.lock();
        // A canceled request may still be waiting behind transport backpressure.
        // Deliver its intent only after next_transfer hands over its owner, so
        // the transport can correlate the cancellation instead of losing it.
        let index =
            (0..SLOTS).find(|i| m.requests[*i].is_none() && m.cancellations[*i].is_some())?;
        m.cancellations[index].take()
    }
    fn complete(
        &mut self,
        session: SessionId,
        completion: Completion,
    ) -> std::result::Result<(), Completion> {
        if self.session != Some(session) {
            return Err(completion);
        }
        let mut m = self.lock();
        if m.state != State::Active {
            return Err(completion);
        }
        let Some(index) = m
            .expected
            .iter()
            .position(|h| h.as_ref() == Some(completion.transfer.header()))
        else {
            return Err(completion);
        };
        if m.completing[index]
            || completion.actual_length > completion.transfer.header().transfer_buffer_length
            || !completion.transfer.layout().is_empty()
            || completion.status > 0
            || completion.transfer.buffer().len()
                != completion.transfer.header().transfer_buffer_length as usize
        {
            return Err(completion);
        }
        m.completing[index] = true;
        m.replies[index] = Some(completion);
        drop(m);
        self.shared.wake.notify_one();
        Ok(())
    }
    fn detach(&mut self, session: SessionId) -> Poll<Result<()>> {
        if self.session.is_some_and(|s| s != session) {
            return Poll::Ready(Err(Error::State));
        }
        let mut m = self.lock();
        m.stop = true;
        self.shared.wake.notify_one();
        if m.native_retired
            && !matches!(m.state, State::Attaching | State::Active | State::Draining)
        {
            return Poll::Ready(Ok(()));
        }
        match m.state {
            State::Idle | State::Retired => Poll::Ready(Ok(())),
            // An earlier parser/I/O failure must not hide failed native cleanup.
            // Successful retirement was handled above, independently of it.
            State::Failed(_) => Poll::Ready(Err(Error::Cleanup)),
            _ => {
                m.state = State::Draining;
                Poll::Pending
            }
        }
    }
}
impl<D: Driver, F: FnOnce() -> Result<D> + Send + 'static> Drop for Backend<D, F> {
    fn drop(&mut self) {
        self.lock().stop = true;
        self.shared.wake.notify_one();
    }
}
fn slot(topology: &Topology, endpoint: u8) -> Result<usize> {
    if endpoint == 0 {
        Ok(0)
    } else {
        topology
            .endpoints
            .iter()
            .flatten()
            .position(|e| e.address == endpoint)
            .map(|i| i + 1)
            .ok_or(Error::Invalid)
    }
}
fn run<D: Driver>(
    factory: impl FnOnce() -> Result<D>,
    descriptors: Descriptors,
    budget: &Budget,
    shared: &Shared,
    device_id: u32,
) -> Result<()> {
    // Charge fixed driver payload slots separately from userspace scratch/data.
    // Framework queues/OS allocations require the independent lifecycle gate.
    // Also cover the bounded interface-path/instance lookup overlap in the
    // Win32 factory (three UTF-16 buffers, at most 72 KiB, plus metadata).
    let _native = budget.reserve_memory(
        SLOTS * (MAX_TRANSFER + NATIVE_SLOT_METADATA_BYTES) + NATIVE_LOOKUP_BYTES,
    )?;
    let mut scratch = budget.buffer(HEADER + MAX_TRANSFER)?;
    let mut driver = factory()?;
    shared.mailbox.lock().unwrap().native_retired = false;
    let outcome = (|| {
        driver.attach(descriptors.bytes.as_slice())?;
        {
            let mut m = shared.mailbox.lock().unwrap();
            if !m.stop {
                m.state = State::Active;
            }
        }
        let mut tokens = [0u64; SLOTS];
        let mut canceled = [false; SLOTS];
        let mut sequence = 0u32;
        let mut alternates = [0u8; crate::descriptors::MAX_SETTINGS];
        let mut configured = true; // Wire::import selected and verified this configuration.
        let mut active_slots = descriptors.active_slots(&alternates);
        loop {
            if shared.mailbox.lock().unwrap().stop {
                return Ok(());
            }
            let mut progressed = false;
            for (index, token) in tokens.iter_mut().enumerate() {
                let done = shared.mailbox.lock().unwrap().replies[index].take();
                if let Some(done) = done {
                    progressed = true;
                    let header = done.transfer.header();
                    // UDE waits for successful control completion before using
                    // the new setting. Match Linux's interval units to the active
                    // endpoint descriptor (frames, or high-speed microframes).
                    if done.status == 0 && header.endpoint == 0 {
                        match (
                            header.setup[setup::REQUEST_TYPE],
                            header.setup[setup::REQUEST],
                        ) {
                            (setup::DEVICE_OUT, setup::SET_CONFIGURATION) => {
                                // SET_CONFIGURATION resets every alternate, including
                                // when configuration zero disables nondefault pipes.
                                let configuration = usb::u16_at(&header.setup, setup::VALUE);
                                if configuration != 0
                                    && configuration
                                        != u16::from(descriptors.topology.configuration)
                                {
                                    return Err(Error::Invalid);
                                }
                                alternates.fill(0);
                                configured = configuration != 0;
                                active_slots = if !configured {
                                    EP0_SLOT_MASK
                                } else {
                                    descriptors.active_slots(&alternates)
                                };
                            }
                            (setup::INTERFACE_OUT, setup::SET_INTERFACE) => {
                                // SET_INTERFACE.
                                if !configured {
                                    return Err(Error::Invalid);
                                }
                                let interface = usb::u16_at(&header.setup, setup::INDEX) as usize;
                                let alternate =
                                    u8::try_from(usb::u16_at(&header.setup, setup::VALUE))
                                        .map_err(|_| Error::Invalid)?;
                                *alternates.get_mut(interface).ok_or(Error::Invalid)? = alternate;
                                active_slots = descriptors.active_slots(&alternates);
                            }
                            _ => {}
                        }
                    }
                    let endpoint = header.endpoint as u8
                        | if header.direction == Direction::In && header.endpoint != 0 {
                            usb::DIRECTION_IN
                        } else {
                            0
                        };
                    let (status, actual) = if done.status == 0 {
                        (transfer::SUCCESS, done.actual_length)
                    } else {
                        (
                            if done.status == LINUX_EPIPE {
                                transfer::STALL
                            } else {
                                transfer::FAILURE
                            },
                            0,
                        )
                    };
                    let payload = if header.direction == Direction::In {
                        actual as usize
                    } else {
                        0
                    };
                    let bytes = &mut scratch.as_mut_slice()[..HEADER + payload];
                    bytes[..HEADER].fill(0);
                    for (offset, value) in [
                        (transfer::VERSION, ABI_VERSION),
                        (transfer::ENDPOINT, endpoint as u32),
                        (transfer::LENGTH, actual),
                        (transfer::PAYLOAD_LENGTH, payload as u32),
                        (transfer::STATUS, status),
                    ] {
                        bytes[offset..offset + size_of::<u32>()]
                            .copy_from_slice(&value.to_le_bytes());
                    }
                    bytes[transfer::TOKEN..transfer::TOKEN + size_of::<u64>()]
                        .copy_from_slice(&token.to_le_bytes());
                    bytes[HEADER..].copy_from_slice(&done.transfer.buffer()[..payload]);
                    driver.reply(bytes)?;
                    *token = 0;
                    let mut m = shared.mailbox.lock().unwrap();
                    m.expected[index] = None;
                    m.cancellations[index] = None;
                }
            }
            driver.snapshot(&mut scratch.as_mut_slice()[..STATS_BYTES])?;
            let b = scratch.as_slice();
            if u64_at(b, stats::VERSION) != u64::from(ABI_VERSION) {
                return Err(Error::Version);
            }
            let live = u64_at(b, stats::LIVE_MASK);
            {
                let mut m = shared.mailbox.lock().unwrap();
                for index in 0..SLOTS {
                    let current = u64_at(b, stats::TOKENS + index * size_of::<u64>());
                    if tokens[index] != 0
                        && !canceled[index]
                        && (live & (1 << index) == 0 || current != tokens[index])
                    {
                        progressed = true;
                        canceled[index] = true;
                        if let Some(header) = m.expected[index] {
                            m.cancellations[index] = Some(header.seqnum);
                        }
                    }
                }
            }
            // Do not fetch a replacement on a pipe until its previous remote
            // request retires; this preserves endpoint ordering after cancellation.
            let mut mask = tokens
                .iter()
                .enumerate()
                .fold(0u32, |mask, (i, t)| mask | if *t == 0 { 1 << i } else { 0 })
                & active_slots
                & live as u32;
            // An idle device needs only the small liveness snapshot, not a
            // 64 KiB METHOD_BUFFERED FETCH allocation on every polling turn.
            // At most one request per eligible pipe per turn. Drain a bounded
            // batch so a continuously busy low slot cannot
            // consume every turn. Never refetch a pipe pending remote retirement.
            for _ in 0..SLOTS {
                if mask == 0 || shared.mailbox.lock().unwrap().stop {
                    break;
                }
                let count = driver.fetch(mask, scratch.as_mut_slice())?;
                if count > scratch.as_slice().len() {
                    return Err(Error::Invalid);
                }
                if count == 0 {
                    break;
                }
                let request = Request::decode(&scratch.as_slice()[..count], &descriptors.topology)?;
                let index = slot(&descriptors.topology, request.endpoint)?;
                if tokens[index] != 0 || mask & (1 << index) == 0 {
                    return Err(Error::State);
                }
                mask &= !(1 << index);
                progressed = true;
                sequence = sequence.checked_add(1).ok_or(Error::Exhausted)?;
                let direction = if (if request.endpoint == 0 {
                    request.setup[setup::REQUEST_TYPE]
                } else {
                    request.endpoint
                }) & usb::DIRECTION_IN
                    != 0
                {
                    Direction::In
                } else {
                    Direction::Out
                };
                let header = SubmitHeader {
                    seqnum: sequence,
                    device_id,
                    direction,
                    endpoint: (request.endpoint & usb::ENDPOINT_NUMBER_MASK) as u32,
                    transfer_flags: 0,
                    transfer_buffer_length: request.length,
                    start_frame: 0,
                    number_of_packets: 0,
                    interval: descriptors.interval(request.endpoint, &alternates)?,
                    setup: request.setup,
                };
                let mut transfer = Transfer::new(budget, header, &[])?;
                if direction == Direction::Out {
                    transfer.buffer_mut().copy_from_slice(request.payload);
                }
                tokens[index] = request.token;
                canceled[index] = false;
                let mut m = shared.mailbox.lock().unwrap();
                m.expected[index] = Some(header);
                m.completing[index] = false;
                m.requests[index] = Some(transfer);
            }
            if progressed {
                // Each turn is bounded by SLOTS. Yield without imposing a timer
                // delay on immediately completing pipes, then check stop again.
                thread::yield_now();
            } else {
                let m = shared.mailbox.lock().unwrap();
                // The predicate shares the producer's lock, so completion/stop
                // cannot be lost between checking the mailbox and parking.
                // Native request arrival has no notification in this ABI; poll
                // it after an idle timeout while mailbox replies wake promptly.
                drop(
                    shared
                        .wake
                        .wait_timeout_while(m, IDLE_POLL_INTERVAL, |m| {
                            !m.stop && m.replies.iter().all(Option::is_none)
                        })
                        .unwrap(),
                );
            }
        }
    })();
    // No worker storage is released before native removal and handle teardown.
    let retired = driver.detach();
    drop(driver);
    let mut m = shared.mailbox.lock().unwrap();
    m.native_retired = retired.is_ok();
    m.requests.iter_mut().for_each(|v| *v = None);
    m.replies.iter_mut().for_each(|v| *v = None);
    if retired.is_err() {
        std::mem::forget(_native);
    }
    outcome.and(retired)
}
