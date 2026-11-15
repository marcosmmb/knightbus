//! Nonblocking engine adapter around an isolated native-I/O worker.
//! The worker owns every native handle and scratch buffer through retirement.
use crate::{
    DescriptorSet as Descriptors,
    topology::{self as usb, Topology},
};
// Endpoint zero plus fifteen endpoints in each direction.
const SLOTS: usize = 31;
const EP0_SLOT_MASK: u32 = 1;
use crate::{
    Budget, Completion, Device, Error, ImporterBackend, MemoryReservation, Result, SessionId,
    Transfer,
    usbip::{Direction, SubmitHeader},
};
use std::{
    sync::{Arc, Mutex},
    task::{Poll, Waker},
    thread,
};

const ARC_COUNTERS: usize = 2;
/// Native wake primitive. It must latch a signal that precedes a wait.
pub trait Signal: Send + Sync + 'static {
    /// Allocate using the attachment budget.
    fn new(budget: &Budget) -> Result<Self>
    where
        Self: Sized;
    /// Wake the worker after publishing state.
    fn notify(&self);
    /// Park until signaled; normal work has no polling timer.
    fn wait(&self) -> Result<()>;
}
/// One native request, borrowing scratch storage until the next native call.
pub struct Request<'a> {
    /// Snapshot slot. ISO may use its endpoint slot or an unused descriptor slot.
    pub slot: usize,
    /// Native identity, never reused while outstanding.
    pub token: u64,
    /// Endpoint address including direction.
    pub endpoint: u8,
    /// Requested bytes.
    pub length: u32,
    /// Setup packet for endpoint zero.
    pub setup: [u8; 8],
    /// Initialized OUT bytes; empty for IN.
    pub payload: &'a [u8],
    /// Original ISO packet ranges; empty for other transfer types.
    pub packets: &'a [crate::usbip::IsoPacket],
    /// USB/IP transfer behavior flags.
    pub flags: u32,
    /// USB/IP frame metadata; ASAP requests use zero.
    pub start_frame: i32,
}
/// Native request liveness indexed by the validated descriptor slots.
pub struct Snapshot {
    /// Slots holding requests.
    pub live: u64,
    /// Native identity for each slot; zero means absent.
    pub tokens: [u64; SLOTS],
}
/// Native importer operations, invoked only on the device worker.
pub trait Driver: Send + 'static {
    /// Native feature and resource limits.
    const CAPABILITIES: crate::Capabilities;
    /// OS notification primitive shared with reply and stop signals.
    type Signal: Signal;
    /// Bounded native allocation allowance, reserved before factory invocation.
    const NATIVE_BYTES: usize;
    /// Required scratch bytes, including native encoding overhead.
    const SCRATCH_BYTES: usize;
    /// Publish the validated descriptor set. Failure still requires detach.
    fn attach(&mut self, descriptors: &Descriptors, budget: &Budget) -> Result<()>;
    /// Return one request from the admitted slot mask, without blocking.
    fn fetch<'a>(
        &'a mut self,
        mask: u32,
        topology: &Topology,
        scratch: &'a mut [u8],
    ) -> Result<Option<Request<'a>>>;
    /// Complete or acknowledge cancellation of the exact native token.
    fn complete(&mut self, token: u64, completion: &Completion, scratch: &mut [u8]) -> Result<()>;
    /// Observe request identities for cancellation correlation.
    fn snapshot(&mut self) -> Result<Snapshot>;
    /// Signal existing work, then arrivals and cancellations until detach.
    fn notifications(&mut self, wake: Arc<Self::Signal>) -> Result<()>;
    /// Prove native retirement; error retains native admission credits.
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
struct Shared<S: Signal> {
    mailbox: Mutex<Mailbox>,
    wake: Arc<S>,
    host: crate::WakeSource,
}
impl<S: Signal> Shared<S> {
    fn notify_host(&self) {
        self.host.notify();
    }
}
/// One native attachment per backend; instantiate multiple backends with a
/// shared Budget for multiple devices. Native calls never execute in `tick`.
/// Drop signals shutdown; a stalled native worker retains its own storage.
pub struct Backend<D: Driver, F: FnOnce() -> Result<D> + Send + 'static> {
    factory: Option<F>,
    descriptors: Option<Descriptors>,
    shared: Arc<Shared<D::Signal>>,
    budget: Budget,
    session: Option<SessionId>,
}
impl<D: Driver, F: FnOnce() -> Result<D> + Send + 'static> Backend<D, F> {
    /// Prepare a bounded worker. Factory opens the native handle on its own
    /// thread, allowing the same backend in embedded and service hosts.
    pub fn new(factory: F, descriptors: Descriptors, budget: Budget) -> Result<Self> {
        D::CAPABILITIES.descriptors(&descriptors)?;
        if D::CAPABILITIES.requests > SLOTS {
            return Err(Error::Unsupported);
        }
        let reservation = budget.reserve_memory(
            std::mem::size_of::<Shared<D::Signal>>() + ARC_COUNTERS * std::mem::size_of::<usize>(),
        )?;
        let wake = Arc::new(D::Signal::new(&budget)?);
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
                wake,
                host: crate::WakeSource::default(),
            }),
        })
    }
    fn lock(&self) -> std::sync::MutexGuard<'_, Mailbox> {
        self.shared.mailbox.lock().expect("native mailbox")
    }
}
impl<D: Driver, F: FnOnce() -> Result<D> + Send + 'static> ImporterBackend for Backend<D, F> {
    fn set_waker(&mut self, waker: Option<Waker>) {
        self.shared.host.register(waker);
    }
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
                .name("knightbus-importer".into())
                .spawn(move || {
                    let result = run(factory, descriptors, &budget, &shared, device_id);
                    let mut m = shared.mailbox.lock().expect("native mailbox");
                    m.state = match result {
                        Ok(()) => State::Retired,
                        Err(e) => State::Failed(e),
                    };
                    drop(m);
                    shared.notify_host();
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
    fn next_transfer(&mut self, session: SessionId, budget: &Budget) -> Option<Result<Transfer>> {
        if self.session != Some(session) || !self.budget.same(budget) {
            return Some(Err(Error::State));
        }
        let mut m = self.lock();
        if let State::Failed(e) = m.state {
            return Some(Err(e));
        }
        for offset in 0..SLOTS {
            let index = (m.next_request + offset) % SLOTS;
            if let Some(t) = &m.requests[index] {
                if !t.layout().is_empty()
                    && m.requests.iter().flatten().any(|earlier| {
                        earlier.header().endpoint == t.header().endpoint
                            && earlier.header().direction == t.header().direction
                            && earlier.header().seqnum < t.header().seqnum
                    })
                {
                    continue;
                }
            }
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
        if self.session != Some(session) || !completion.transfer.budget_matches(&self.budget) {
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
            || m.requests[index].is_some()
            || completion.actual_length > completion.transfer.header().transfer_buffer_length
            || completion.validate().is_err()
            || completion.status > 0
            || completion.transfer.buffer().len()
                != completion.transfer.header().transfer_buffer_length as usize
        {
            return Err(completion);
        }
        m.completing[index] = true;
        m.replies[index] = Some(completion);
        drop(m);
        self.shared.wake.notify();
        Ok(())
    }
    fn detach(&mut self, session: SessionId) -> Poll<Result<()>> {
        if self.session.is_some_and(|s| s != session) {
            return Poll::Ready(Err(Error::State));
        }
        let mut m = self.lock();
        m.stop = true;
        self.shared.wake.notify();
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
        self.shared.wake.notify();
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
    shared: &Shared<D::Signal>,
    device_id: u32,
) -> Result<()> {
    let _native = budget.reserve_memory(D::NATIVE_BYTES)?;
    let mut scratch = budget.buffer(D::SCRATCH_BYTES)?;
    let mut driver = factory()?;
    shared.mailbox.lock().unwrap().native_retired = false;
    let outcome = (|| {
        driver.attach(&descriptors, budget)?;
        {
            let mut m = shared.mailbox.lock().unwrap();
            if !m.stop {
                m.state = State::Active;
            }
        }
        shared.notify_host();
        Pump::new(
            &mut driver,
            descriptors,
            budget,
            shared,
            device_id,
            &mut scratch,
        )
        .run()
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

// Worker-local request correlation and selected interface state.
struct Pump<'a, D: Driver> {
    driver: &'a mut D,
    descriptors: Descriptors,
    budget: &'a Budget,
    shared: &'a Shared<D::Signal>,
    device_id: u32,
    scratch: &'a mut crate::Buffer,
    tokens: [u64; SLOTS],
    canceled: [bool; SLOTS],
    sequence: u32,
    alternates: [u8; usb::MAX_SETTINGS],
    configured: bool,
    active_slots: u32,
}
impl<'a, D: Driver> Pump<'a, D> {
    fn new(
        driver: &'a mut D,
        descriptors: Descriptors,
        budget: &'a Budget,
        shared: &'a Shared<D::Signal>,
        device_id: u32,
        scratch: &'a mut crate::Buffer,
    ) -> Self {
        let alternates = [0; usb::MAX_SETTINGS];
        let active_slots = descriptors.active_slots(&alternates);
        Self {
            driver,
            descriptors,
            budget,
            shared,
            device_id,
            scratch,
            tokens: [0; SLOTS],
            canceled: [false; SLOTS],
            sequence: 0,
            alternates,
            configured: true,
            active_slots,
        }
    }
    fn run(&mut self) -> Result<()> {
        self.driver.notifications(self.shared.wake.clone())?;
        loop {
            if self.shared.mailbox.lock().unwrap().stop {
                return Ok(());
            }
            let mut progressed = self.complete()?;
            let (live, canceled) = self.cancellations()?;
            progressed |= canceled;
            progressed |= self.fetch(live)?;
            if progressed {
                self.shared.notify_host();
                // Each turn is bounded by SLOTS. Yield without imposing a timer
                // delay on immediately completing pipes, then check stop again.
                thread::yield_now();
            } else {
                // Notifications stay latched across the state check and wait.
                // Arrivals, cancellations, replies and stop all wake this event.
                self.shared.wake.wait()?;
            }
        }
    }
    fn complete(&mut self) -> Result<bool> {
        let mut progressed = false;
        for (index, token) in self.tokens.iter_mut().enumerate() {
            let done = self.shared.mailbox.lock().unwrap().replies[index].take();
            if let Some(done) = done {
                progressed = true;
                // Commit successful setting changes before admitting more work.
                Self::update_setting(
                    &self.descriptors,
                    &mut self.alternates,
                    &mut self.configured,
                    &mut self.active_slots,
                    &done,
                )?;
                self.driver
                    .complete(*token, &done, self.scratch.as_mut_slice())?;
                *token = 0;
                let mut m = self.shared.mailbox.lock().unwrap();
                m.expected[index] = None;
                m.cancellations[index] = None;
            }
        }
        Ok(progressed)
    }
    fn update_setting(
        descriptors: &Descriptors,
        alternates: &mut [u8; usb::MAX_SETTINGS],
        configured: &mut bool,
        active_slots: &mut u32,
        done: &Completion,
    ) -> Result<()> {
        let header = done.transfer.header();
        if done.status == 0 && header.endpoint == 0 {
            match (
                header.setup[usb::SETUP_TYPE],
                header.setup[usb::SETUP_REQUEST],
            ) {
                (usb::RECIPIENT_DEVICE, usb::SET_CONFIGURATION) => {
                    // SET_CONFIGURATION resets every alternate, including
                    // when configuration zero disables nondefault pipes.
                    let configuration = usb::u16_at(&header.setup, usb::SETUP_VALUE);
                    if configuration != 0
                        && configuration != u16::from(descriptors.topology.configuration)
                    {
                        return Err(Error::Invalid);
                    }
                    alternates.fill(0);
                    *configured = configuration != 0;
                    *active_slots = if !*configured {
                        EP0_SLOT_MASK
                    } else {
                        descriptors.active_slots(alternates)
                    };
                }
                (usb::RECIPIENT_INTERFACE, usb::SET_INTERFACE) => {
                    if !*configured {
                        return Err(Error::Invalid);
                    }
                    let interface = usb::u16_at(&header.setup, usb::SETUP_INDEX) as usize;
                    let alternate = u8::try_from(usb::u16_at(&header.setup, usb::SETUP_VALUE))
                        .map_err(|_| Error::Invalid)?;
                    if !descriptors
                        .topology
                        .settings
                        .iter()
                        .flatten()
                        .any(|s| usize::from(s.interface) == interface && s.alternate == alternate)
                    {
                        return Err(Error::Invalid);
                    }
                    *alternates.get_mut(interface).ok_or(Error::Invalid)? = alternate;
                    *active_slots = descriptors.active_slots(alternates);
                }
                _ => {}
            }
        }
        Ok(())
    }
    fn cancellations(&mut self) -> Result<(u64, bool)> {
        let mut progressed = false;
        let snapshot = self.driver.snapshot()?;
        let live = snapshot.live;
        if live & !((1u64 << SLOTS) - 1) != 0
            || snapshot
                .tokens
                .iter()
                .enumerate()
                .any(|(i, t)| live & (1 << i) != 0 && *t == 0)
        {
            return Err(Error::Invalid);
        }
        {
            let mut m = self.shared.mailbox.lock().unwrap();
            for index in 0..SLOTS {
                let current = snapshot.tokens[index];
                if self.tokens[index] != 0
                    && !self.canceled[index]
                    && (live & (1 << index) == 0 || current != self.tokens[index])
                {
                    progressed = true;
                    self.canceled[index] = true;
                    if let Some(header) = m.expected[index] {
                        m.cancellations[index] = Some(header.seqnum);
                    }
                }
            }
        }
        Ok((live, progressed))
    }
    fn fetch(&mut self, live: u64) -> Result<bool> {
        let mut progressed = false;
        // Do not fetch a replacement on a pipe until its previous remote
        // request retires; this prevents slot reuse while cancellation is in flight.
        let mut mask = self
            .tokens
            .iter()
            .enumerate()
            .fold(0u32, |mask, (i, t)| mask | if *t == 0 { 1 << i } else { 0 })
            & self.active_slots
            & live as u32;
        // An idle device needs only its small liveness snapshot.
        // At most one request per eligible slot per turn. Drain a bounded
        // batch so a continuously busy low slot cannot
        // consume every turn. Never refetch a pipe pending remote retirement.
        for _ in 0..SLOTS {
            if mask == 0
                || self.shared.mailbox.lock().unwrap().stop
                || self.tokens.iter().filter(|t| **t != 0).count() >= D::CAPABILITIES.requests
            {
                break;
            }
            let Some(request) = self.driver.fetch(
                mask,
                &self.descriptors.topology,
                self.scratch.as_mut_slice(),
            )?
            else {
                break;
            };
            let input = if request.endpoint == 0 {
                request.setup[0]
            } else {
                request.endpoint
            } & usb::DIRECTION_IN
                != 0;
            if request.token == 0
                || request.length as usize > D::CAPABILITIES.transfer_bytes
                || request.packets.len() > D::CAPABILITIES.iso_packets
                || request.payload.len() != if input { 0 } else { request.length as usize }
                || (request.endpoint != 0 && request.setup != [0; 8])
                || (request.endpoint == 0
                    && u32::from(usb::u16_at(&request.setup, 6)) != request.length)
            {
                return Err(Error::Invalid);
            }
            let index = request.slot;
            let endpoint_slot = slot(&self.descriptors.topology, request.endpoint)?;
            let spare = index > self.descriptors.topology.endpoints.iter().flatten().count();
            if index >= SLOTS || (index != endpoint_slot && (request.packets.is_empty() || !spare))
            {
                return Err(Error::Invalid);
            }
            if self.tokens[index] != 0 || mask & (1 << index) == 0 {
                return Err(Error::State);
            }
            mask &= !(1 << index);
            progressed = true;
            self.sequence = self.sequence.checked_add(1).ok_or(Error::Exhausted)?;
            let direction = if (if request.endpoint == 0 {
                request.setup[usb::SETUP_TYPE]
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
                seqnum: self.sequence,
                device_id: self.device_id,
                direction,
                endpoint: (request.endpoint & usb::ENDPOINT_NUMBER_MASK) as u32,
                transfer_flags: request.flags,
                transfer_buffer_length: request.length,
                start_frame: request.start_frame,
                number_of_packets: request.packets.len() as i32,
                interval: self
                    .descriptors
                    .interval(request.endpoint, &self.alternates)?,
                setup: request.setup,
            };
            let mut transfer = Transfer::new(self.budget, header, request.packets)?;
            let endpoint = if request.endpoint == 0 {
                None
            } else {
                Some(
                    self.descriptors
                        .endpoint(request.endpoint, &self.alternates)?,
                )
            };
            transfer.validate_native(self.device_id, endpoint.as_ref(), D::CAPABILITIES)?;
            if direction == Direction::Out {
                transfer.buffer_mut().copy_from_slice(request.payload);
            }
            self.tokens[index] = request.token;
            self.canceled[index] = false;
            let mut m = self.shared.mailbox.lock().unwrap();
            m.expected[index] = Some(header);
            m.completing[index] = false;
            m.requests[index] = Some(transfer);
        }
        Ok(progressed)
    }
}
