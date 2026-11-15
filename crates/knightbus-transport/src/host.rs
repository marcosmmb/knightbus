//! Authenticated sessions, peer liveness and shared exporter hosting.
use crate::{
    Stream,
    control::{Framed, Message, Rate, TICKET_BYTES, TIMEOUT},
    security::{Channel, Identity, Peers},
};
use knightbus_core::*;
use ring::rand::SecureRandom;
use std::{
    collections::BTreeMap,
    net::{TcpListener, TcpStream},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    task::{Wake, Waker},
    time::{Duration, Instant},
};
const MAX_CONNECTIONS: usize = 16;
const MANAGEMENT_RESERVE: usize = 2;
const MAX_LINKS_PER_PEER: usize = 4;
const HEARTBEAT: Duration = Duration::from_secs(3);
const LEASE_TIMEOUT: Duration = Duration::from_secs(15);
const HOUSEKEEPING: Duration = Duration::from_secs(1);
const MAX_AUDIT: usize = 256;
const MAX_EVENTS: usize = 64;
const CLEANUP_TIMEOUT: Duration = Duration::from_secs(30);
const CLEANUP_INTERVAL: Duration = Duration::from_millis(1);
const USB_LISTENER: mio::Token = mio::Token(0);
const CONTROL_LISTENER: mio::Token = mio::Token(1);
const LISTENER_COUNT: usize = 2;
const LISTENER_EVENTS: usize = 4;

pub(crate) struct Lease {
    last: Mutex<Instant>,
    open: AtomicBool,
}
impl Lease {
    fn new() -> Self {
        Self {
            last: Mutex::new(Instant::now()),
            open: AtomicBool::new(true),
        }
    }
    pub fn check(&self) -> Result<()> {
        if !self.open.load(Ordering::Acquire) {
            return Err(Error::Unavailable);
        }
        if self.last.lock().map_err(|_| Error::Internal)?.elapsed() >= LEASE_TIMEOUT {
            return Err(Error::Deadline);
        }
        Ok(())
    }
    fn renew(&self) -> Result<()> {
        self.check()?;
        *self.last.lock().map_err(|_| Error::Internal)? = Instant::now();
        Ok(())
    }
    fn close(&self) {
        self.open.store(false, Ordering::Release);
    }
}
#[derive(Default)]
pub(crate) struct Wakes(Mutex<Vec<Option<Waker>>>);
impl Wakes {
    pub fn register(&self, waker: Waker) -> Result<usize> {
        let mut slots = self.0.lock().map_err(|_| Error::Internal)?;
        if let Some(index) = slots.iter().position(Option::is_none) {
            slots[index] = Some(waker);
            return Ok(index);
        }
        if slots.len() >= MAX_CONNECTIONS {
            return Err(Error::Exhausted);
        }
        slots.push(Some(waker));
        Ok(slots.len() - 1)
    }
    pub fn unregister(&self, index: usize) {
        if let Ok(mut slots) = self.0.lock() {
            slots[index] = None;
        }
    }
    fn signal(&self) {
        if let Ok(slots) = self.0.lock() {
            for w in slots.iter().flatten() {
                w.wake_by_ref();
            }
        }
    }
}
impl Wake for Wakes {
    fn wake(self: Arc<Self>) {
        self.signal()
    }
    fn wake_by_ref(self: &Arc<Self>) {
        self.signal()
    }
}
struct Link {
    peer: PeerId,
    ticket: Option<[u8; TICKET_BYTES]>,
    issued: Instant,
    lease: Arc<Lease>,
}
struct State {
    peers: Peers,
    links: BTreeMap<u64, Link>,
    next: u64,
}
impl State {
    fn open_link(
        &mut self,
        peer: PeerId,
        ticket: [u8; TICKET_BYTES],
        lease: Arc<Lease>,
    ) -> Result<u64> {
        if !self.peers.contains(peer) {
            return Err(Error::Denied);
        }
        if self.links.values().filter(|link| link.peer == peer).count() >= MAX_LINKS_PER_PEER {
            return Err(Error::Exhausted);
        }
        self.next = self.next.checked_add(1).ok_or(Error::Exhausted)?;
        self.links.insert(
            self.next,
            Link {
                peer,
                ticket: Some(ticket),
                issued: Instant::now(),
                lease,
            },
        );
        Ok(self.next)
    }
    fn consume_ticket(&mut self, peer: PeerId, ticket: &[u8]) -> Result<Arc<Lease>> {
        if !self.peers.contains(peer) {
            return Err(Error::Denied);
        }
        let link = self
            .links
            .values_mut()
            .find(|link| {
                link.peer == peer && link.ticket.as_ref().is_some_and(|t| t.as_slice() == ticket)
            })
            .ok_or(Error::Denied)?;
        link.lease.check()?;
        if link.issued.elapsed() >= TIMEOUT {
            return Err(Error::Deadline);
        }
        link.ticket = None;
        Ok(link.lease.clone())
    }
}
/// Metadata-only security audit. Consumers must drain promptly; overflow is explicit.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AuditKind {
    /// Explicit peer enrollment.
    Paired,
    /// Trust revoked; active streams are being drained.
    Revoked,
    /// Authentication completed.
    Connected,
    /// Connection ended, including its sanitized error.
    Disconnected,
    /// An administrator changed a device grant.
    Permission,
    /// An authenticated data connection passed its one-use ticket check.
    DataConnected,
    /// Earlier audit records were lost; resnapshot state.
    Overflow,
}
/// Bounded audit record. No certificates, credentials, USB descriptors or payloads.
#[derive(Clone, Copy, Debug)]
pub struct Audit {
    /// Action being recorded.
    pub kind: AuditKind,
    /// Authenticated identity, when available.
    pub peer: Option<PeerId>,
    /// Sanitized failure category.
    pub error: Option<Error>,
}
/// One authoritative engine shared by management and USB connections.
/// Native callbacks wake all attached streams; engine calls remain serialized.
pub struct Host<B, C> {
    engine: Mutex<Engine<B, C>>,
    identity: Identity,
    state: Mutex<State>,
    wakes: Arc<Wakes>,
    audit: Mutex<std::collections::VecDeque<Audit>>,
    stop: knightbus_ipc::local::Cancellation,
}
impl<B: ExporterBackend, C: Clock> Host<B, C> {
    /// Install authenticated transport around an existing engine and explicit trust store.
    pub fn new(mut engine: Engine<B, C>, identity: Identity, peers: Peers) -> Self {
        let wakes = Arc::new(Wakes::default());
        engine.set_waker(Some(Waker::from(wakes.clone())));
        Self {
            engine: Mutex::new(engine),
            identity,
            state: Mutex::new(State {
                peers,
                links: BTreeMap::new(),
                next: 0,
            }),
            wakes,
            audit: Mutex::new(std::collections::VecDeque::new()),
            stop: Default::default(),
        }
    }
    fn record(&self, kind: AuditKind, peer: Option<PeerId>, error: Option<Error>) {
        if let Ok(mut queue) = self.audit.lock() {
            if queue.len() >= MAX_AUDIT {
                queue.clear();
                queue.push_back(Audit {
                    kind: AuditKind::Overflow,
                    peer: None,
                    error: None,
                });
            }
            queue.push_back(Audit { kind, peer, error });
        }
    }
    /// Drain one audit record; the host decides whether to persist it.
    pub fn audit(&self) -> Option<Audit> {
        self.audit.lock().ok()?.pop_front()
    }
    /// Enroll an explicitly confirmed public certificate. Device access stays denied.
    pub fn pair(&self, certificate: Vec<u8>, confirmed: PeerId) -> Result<()> {
        self.state
            .lock()
            .map_err(|_| Error::Internal)?
            .peers
            .pair(certificate, confirmed)?;
        self.record(AuditKind::Paired, Some(confirmed), None);
        Ok(())
    }
    /// Revoke trust and end every live control/data connection for this peer.
    pub fn revoke(&self, peer: PeerId) -> Result<()> {
        let mut state = self.state.lock().map_err(|_| Error::Internal)?;
        state.peers.revoke(peer);
        for link in state.links.values().filter(|link| link.peer == peer) {
            link.lease.close();
        }
        drop(state);
        self.engine
            .lock()
            .map_err(|_| Error::Internal)?
            .revoke_peer(peer);
        self.wakes.signal();
        self.record(AuditKind::Revoked, Some(peer), None);
        Ok(())
    }
    /// Execute locally authenticated management. Callers come from OS credentials,
    /// never from a network message. Both hosts retain the same core contract.
    pub fn execute(&self, caller: Caller, command: Command, deadline: u64) -> Result<Reply> {
        let permission = matches!(command, Command::Authorize { .. });
        let shutdown = matches!(command, Command::Shutdown);
        let result = self
            .engine
            .lock()
            .map_err(|_| Error::Internal)?
            .execute(caller, command, deadline);
        if permission {
            self.record(
                AuditKind::Permission,
                Some(caller.peer),
                result.as_ref().err().copied(),
            );
        }
        if shutdown && result.is_ok() {
            self.stop.cancel();
        }
        self.wakes.signal();
        result
    }
    /// Reload protected enrollment records. Removed peers lose grants and live leases.
    /// Replacing the local identity requires a restart, never a silent key switch.
    pub fn refresh(&self, store: &crate::security::Store) -> Result<()> {
        if store.identity()?.peer() != self.identity.peer() {
            return Err(Error::Denied);
        }
        let peers = store.peers()?;
        let removed: Vec<_> = self
            .state
            .lock()
            .map_err(|_| Error::Internal)?
            .peers
            .identities()
            .filter(|p| !peers.contains(*p))
            .collect();
        for peer in removed {
            self.revoke(peer)?;
        }
        let mut state = self.state.lock().map_err(|_| Error::Internal)?;
        for peer in peers.identities().filter(|p| !state.peers.contains(*p)) {
            self.record(AuditKind::Paired, Some(peer), None);
        }
        state.peers = peers;
        Ok(())
    }
    /// Stop admission and drain all native ownership. Idempotent.
    pub fn shutdown(&self) {
        self.stop.cancel();
        if let Ok(state) = self.state.lock() {
            for link in state.links.values() {
                link.lease.close();
            }
        }
        if let Ok(mut engine) = self.engine.lock() {
            engine.shutdown();
        }
        self.wakes.signal();
    }
    fn control(&self, stream: TcpStream) -> Result<()> {
        let peers = self
            .state
            .lock()
            .map_err(|_| Error::Internal)?
            .peers
            .clone();
        let stream = Stream::accept(stream, &self.identity, &peers, Channel::Control, TIMEOUT)?;
        let caller = stream.caller().ok_or(Error::Denied)?;
        let mut ticket = [0; TICKET_BYTES];
        ring::rand::SystemRandom::new()
            .fill(&mut ticket)
            .map_err(|_| Error::Internal)?;
        let lease = Arc::new(Lease::new());
        let id = self.state.lock().map_err(|_| Error::Internal)?.open_link(
            caller.peer,
            ticket,
            lease.clone(),
        )?;
        self.record(AuditKind::Connected, Some(caller.peer), None);
        let result = self.control_loop(stream, caller, &lease, ticket);
        lease.close();
        self.wakes.signal();
        self.state
            .lock()
            .map_err(|_| Error::Internal)?
            .links
            .remove(&id);
        self.record(
            AuditKind::Disconnected,
            Some(caller.peer),
            result.as_ref().err().copied(),
        );
        result
    }
    fn control_loop(
        &self,
        stream: Stream,
        caller: Caller,
        lease: &Lease,
        ticket: [u8; TICKET_BYTES],
    ) -> Result<()> {
        let mut wire = Framed::new(stream)?;
        let Reply::Subscription(subscription) =
            self.execute(caller, Command::Subscribe, u64::MAX)?
        else {
            return Err(Error::Internal);
        };
        let result = self.control_messages(&mut wire, caller, lease, ticket, subscription);
        let _ = self.execute(caller, Command::Unsubscribe(subscription), u64::MAX);
        result
    }

    fn control_messages(
        &self,
        wire: &mut Framed,
        caller: Caller,
        lease: &Lease,
        ticket: [u8; TICKET_BYTES],
        subscription: u64,
    ) -> Result<()> {
        wire.send(&Message::Hello(ticket).encode()?)?;
        let mut sequence = 0u64;
        let mut rate = Rate::new();
        loop {
            lease.check()?;
            if self.stop.is_cancelled() {
                return Ok(());
            }
            if let Some(frame) = wire.receive(HOUSEKEEPING)? {
                rate.admit()?;
                let Message::Ping(next) = Message::decode(&frame)? else {
                    return Err(Error::Invalid);
                };
                if next != sequence.checked_add(1).ok_or(Error::Exhausted)? {
                    return Err(Error::Invalid);
                }
                sequence = next;
                lease.renew()?;
                wire.send(&Message::Pong(next).encode()?)?;
            }
            if let Reply::Event(Some(event)) =
                self.execute(caller, Command::Poll(subscription), u64::MAX)?
            {
                wire.send(&Message::Event(event).encode()?)?;
            }
        }
    }
    fn usb(&self, stream: TcpStream) -> Result<()> {
        let peers = self
            .state
            .lock()
            .map_err(|_| Error::Internal)?
            .peers
            .clone();
        let stream = Stream::accept(stream, &self.identity, &peers, Channel::Usb, TIMEOUT)?;
        let caller = stream.caller().ok_or(Error::Denied)?;
        let mut wire = Framed::new(stream)?;
        let ticket = wire.receive(TIMEOUT)?.ok_or(Error::Deadline)?;
        let lease = self
            .state
            .lock()
            .map_err(|_| Error::Internal)?
            .consume_ticket(caller.peer, &ticket)?;
        self.record(AuditKind::DataConnected, Some(caller.peer), None);
        let result = crate::exporter::authenticated(
            &mut &self.engine,
            wire.into_stream()?,
            &lease,
            &self.wakes,
        );
        self.record(
            AuditKind::Disconnected,
            Some(caller.peer),
            result.as_ref().err().copied(),
        );
        result
    }
}
impl<B: ExporterBackend + Send, C: Clock + Send> Host<B, C> {
    /// Serve separate authenticated control and USB ports. Admission bounds worker
    /// count before spawning; slow handshakes expire without reaching native USB.
    /// Call `shutdown` from an authorized control endpoint to stop and drain.
    pub fn run(
        &self,
        usb: TcpListener,
        control: TcpListener,
        management: Option<&knightbus_ipc::local::Listener>,
        store: Option<&crate::security::Store>,
    ) -> Result<()> {
        let mut listeners = Listeners::new(usb, control)?;
        let result = std::thread::scope(|scope| -> Result<()> {
            struct StopOnExit<'a, B: ExporterBackend, C: Clock>(&'a Host<B, C>);
            impl<B: ExporterBackend, C: Clock> Drop for StopOnExit<'_, B, C> {
                fn drop(&mut self) {
                    self.0.shutdown();
                }
            }
            let _stop = StopOnExit(self);
            let mut workers = Vec::new();
            let mut rate = Rate::new();
            while !self.stop.is_cancelled() {
                workers
                    .retain(|worker: &std::thread::ScopedJoinHandle<'_, ()>| !worker.is_finished());
                if let Some(store) = store {
                    self.refresh(store)?;
                }
                if let Some(management) = management
                    && workers.len() < MAX_CONNECTIONS
                    && let Some(connection) = management.accept()?
                {
                    workers.push(scope.spawn(move || {
                        let _ = connection.serve(&self.stop, |caller, command, deadline| {
                            self.execute(caller, command, deadline)
                        });
                    }));
                }
                // Tick leases even when no socket is active.
                self.engine.lock().map_err(|_| Error::Internal)?.tick();
                let capacity = (MAX_CONNECTIONS - MANAGEMENT_RESERVE).saturating_sub(workers.len());
                for (channel, stream) in listeners.accept(capacity, &mut rate)? {
                    workers.push(scope.spawn(move || {
                        let result = match channel {
                            Channel::Usb => self.usb(stream),
                            Channel::Control => self.control(stream),
                        };
                        if let Err(error) = result {
                            self.record(AuditKind::Disconnected, None, Some(error));
                        }
                    }));
                }
            }
            self.shutdown();
            Ok(())
        });
        self.drain()?;
        result
    }

    fn drain(&self) -> Result<()> {
        self.shutdown();
        let deadline = Instant::now() + CLEANUP_TIMEOUT;
        loop {
            let mut engine = self.engine.lock().map_err(|_| Error::Internal)?;
            engine.tick();
            if engine.shutdown_complete() {
                break;
            }
            drop(engine);
            if Instant::now() >= deadline {
                return Err(Error::Cleanup);
            }
            std::thread::sleep(CLEANUP_INTERVAL);
        }
        Ok(())
    }
}

struct Listeners {
    usb: mio::net::TcpListener,
    control: mio::net::TcpListener,
    poll: mio::Poll,
    events: mio::Events,
    ready: [bool; LISTENER_COUNT],
}
impl Listeners {
    fn new(usb: TcpListener, control: TcpListener) -> Result<Self> {
        usb.set_nonblocking(true).map_err(|_| Error::Unavailable)?;
        control
            .set_nonblocking(true)
            .map_err(|_| Error::Unavailable)?;
        let mut usb = mio::net::TcpListener::from_std(usb);
        let mut control = mio::net::TcpListener::from_std(control);
        let poll = mio::Poll::new().map_err(|_| Error::Unavailable)?;
        poll.registry()
            .register(&mut usb, USB_LISTENER, mio::Interest::READABLE)
            .map_err(|_| Error::Unavailable)?;
        poll.registry()
            .register(&mut control, CONTROL_LISTENER, mio::Interest::READABLE)
            .map_err(|_| Error::Unavailable)?;
        Ok(Self {
            usb,
            control,
            poll,
            events: mio::Events::with_capacity(LISTENER_EVENTS),
            ready: [false; LISTENER_COUNT],
        })
    }

    fn accept(&mut self, capacity: usize, rate: &mut Rate) -> Result<Vec<(Channel, TcpStream)>> {
        let timeout = if self.ready.iter().any(|ready| *ready) {
            Duration::ZERO
        } else {
            HOUSEKEEPING
        };
        self.poll
            .poll(&mut self.events, Some(timeout))
            .map_err(|_| Error::Unavailable)?;
        for event in &self.events {
            match event.token() {
                USB_LISTENER => self.ready[USB_LISTENER.0] = true,
                CONTROL_LISTENER => self.ready[CONTROL_LISTENER.0] = true,
                _ => return Err(Error::Internal),
            }
        }
        let mut accepted = Vec::new();
        for (token, listener, channel) in [
            (USB_LISTENER, &self.usb, Channel::Usb),
            (CONTROL_LISTENER, &self.control, Channel::Control),
        ] {
            if !self.ready[token.0] {
                continue;
            }
            for _ in 0..MAX_CONNECTIONS {
                let (stream, _) = match listener.accept() {
                    Ok(pair) => pair,
                    Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                        self.ready[token.0] = false;
                        break;
                    }
                    Err(_) => return Err(Error::Unavailable),
                };
                if accepted.len() < capacity && rate.admit().is_ok() {
                    accepted.push((channel, stream.into()));
                }
            }
        }
        Ok(accepted)
    }
}

/// An importer control link. A worker sends heartbeats and receives bounded events;
/// dropping it closes the link and causes the exporter to drain its device lease.
pub struct Remote {
    ticket: Option<[u8; TICKET_BYTES]>,
    alive: Arc<Lease>,
    events: Arc<Mutex<std::collections::VecDeque<Event>>>,
    worker: Option<std::thread::JoinHandle<()>>,
    peer: PeerId,
}
impl Remote {
    /// Authenticate and negotiate the versioned control link.
    pub fn connect(
        stream: TcpStream,
        identity: &Identity,
        peers: &Peers,
        peer: PeerId,
    ) -> Result<Self> {
        let stream = Stream::connect(stream, identity, peers, peer, Channel::Control, TIMEOUT)?;
        let mut wire = Framed::new(stream)?;
        let hello = wire.receive(TIMEOUT)?.ok_or(Error::Deadline)?;
        let Message::Hello(ticket) = Message::decode(&hello)? else {
            return Err(Error::Version);
        };
        let alive = Arc::new(Lease::new());
        let events = Arc::new(Mutex::new(std::collections::VecDeque::new()));
        let worker_alive = alive.clone();
        let worker_events = events.clone();
        let worker = std::thread::Builder::new()
            .name("knightbus-control".into())
            .spawn(move || {
                let _ = Self::heartbeat(&mut wire, &worker_alive, &worker_events);
                worker_alive.close();
            })
            .map_err(|_| Error::Unavailable)?;
        Ok(Self {
            ticket: Some(ticket),
            alive,
            events,
            worker: Some(worker),
            peer,
        })
    }
    fn heartbeat(
        wire: &mut Framed,
        alive: &Lease,
        events: &Mutex<std::collections::VecDeque<Event>>,
    ) -> Result<()> {
        let mut sequence = 0u64;
        let mut acknowledged = 0u64;
        let mut event_sequence = 0u64;
        let mut sent = Instant::now() - HEARTBEAT;
        loop {
            alive.check()?;
            if sent.elapsed() >= HEARTBEAT {
                sequence = sequence.checked_add(1).ok_or(Error::Exhausted)?;
                wire.send(&Message::Ping(sequence).encode()?)?;
                sent = Instant::now();
            }
            if let Some(frame) = wire.receive(HOUSEKEEPING)? {
                match Message::decode(&frame)? {
                    Message::Pong(pong) => {
                        if pong != acknowledged.checked_add(1).ok_or(Error::Exhausted)?
                            || pong > sequence
                        {
                            return Err(Error::Invalid);
                        }
                        acknowledged = pong;
                        alive.renew()?;
                    }
                    Message::Event(event) => {
                        if event.sequence <= event_sequence {
                            return Err(Error::Invalid);
                        }
                        event_sequence = event.sequence;
                        Self::push_event(events, event)?;
                    }
                    _ => return Err(Error::Invalid),
                }
            }
        }
    }
    fn push_event(events: &Mutex<std::collections::VecDeque<Event>>, event: Event) -> Result<()> {
        let mut queue = events.lock().map_err(|_| Error::Internal)?;
        if queue.len() >= MAX_EVENTS {
            queue.clear();
            queue.push_back(Event {
                kind: EventKind::Resync,
                sequence: event.sequence,
                session: None,
            });
        } else {
            queue.push_back(event);
        }
        Ok(())
    }
    /// Bind one new USB stream to this link using its one-use capability.
    pub fn usb(&mut self, stream: TcpStream, identity: &Identity, peers: &Peers) -> Result<Stream> {
        self.alive.check()?;
        let ticket = self.ticket.take().ok_or(Error::State)?;
        let stream = Stream::connect(stream, identity, peers, self.peer, Channel::Usb, TIMEOUT)?;
        let mut wire = Framed::new(stream)?;
        wire.send(&ticket)?;
        wire.into_stream()
    }
    /// Fail promptly if the authenticated control link has stopped making progress.
    pub fn check(&self) -> Result<()> {
        self.alive.check()
    }
    /// Take the next exporter-originated notification; overflow requests a resnapshot.
    pub fn event(&self) -> Option<Event> {
        self.events.lock().ok()?.pop_front()
    }
}
impl Drop for Remote {
    fn drop(&mut self) {
        self.alive.close();
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

/// Shared local management commands for native runners. No USB payload enters IPC.
pub fn command(args: &[String]) -> Option<std::result::Result<(), Box<dyn std::error::Error>>> {
    if args.first().is_none_or(|s| s != "manage") {
        return None;
    }
    Some((|| {
        let [_, path, operation, rest @ ..] = args else {
            return Err("usage: manage SOCKET list|permissions DEVICE_ID GENERATION|grant DEVICE_ID GENERATION PEER|deny DEVICE_ID GENERATION PEER|shutdown".into());
        };
        let key = |id: &str,
                   generation: &str|
         -> std::result::Result<DeviceKey, Box<dyn std::error::Error>> {
            Ok(DeviceKey {
                id: DeviceId(u64::from_str_radix(id.trim_start_matches("0x"), 16)?),
                generation: generation.parse()?,
            })
        };
        let command = match (operation.as_str(), rest) {
            ("list", []) => Command::List,
            ("shutdown", []) => Command::Shutdown,
            ("permissions", [id, generation]) => Command::Permissions(key(id, generation)?),
            ("grant" | "deny", [id, generation, peer]) => Command::Authorize {
                device: key(id, generation)?,
                peer: crate::security::parse_fingerprint(peer)?,
                allow: operation == "grant",
            },
            _ => return Err("invalid management command".into()),
        };
        let mut client = knightbus_ipc::local::Client::connect(std::path::Path::new(path))?;
        println!("{:?}", client.call(command, u64::MAX)?);
        Ok(())
    })())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn queued_connections_survive_an_exhausted_readiness_batch() {
        let usb = TcpListener::bind("127.0.0.1:0").unwrap();
        let control = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = usb.local_addr().unwrap();
        let mut listeners = Listeners::new(usb, control).unwrap();
        let clients: Vec<_> = (0..MAX_CONNECTIONS + 1)
            .map(|_| TcpStream::connect(address).unwrap())
            .collect();
        let mut rate = Rate::new();
        assert_eq!(
            listeners.accept(MAX_CONNECTIONS, &mut rate).unwrap().len(),
            MAX_CONNECTIONS
        );
        // No new connection arrives to produce another edge.
        assert_eq!(
            listeners.accept(MAX_CONNECTIONS, &mut rate).unwrap().len(),
            1
        );
        drop(clients);
    }

    #[test]
    fn links_require_pairing_and_respect_the_per_peer_limit() {
        let identity = Identity::generate().unwrap();
        let peer = identity.peer();
        let mut state = State {
            peers: Peers::default(),
            links: BTreeMap::new(),
            next: 0,
        };
        let open =
            |state: &mut State| state.open_link(peer, [42; TICKET_BYTES], Arc::new(Lease::new()));
        assert_eq!(open(&mut state), Err(Error::Denied));
        state
            .peers
            .pair(identity.certificate().to_vec(), peer)
            .unwrap();
        for _ in 0..MAX_LINKS_PER_PEER {
            open(&mut state).unwrap();
        }
        assert_eq!(open(&mut state), Err(Error::Exhausted));
        state.links.remove(&1);
        open(&mut state).unwrap();
    }

    #[test]
    fn excess_notifications_request_a_resnapshot() {
        let events = Mutex::new(std::collections::VecDeque::new());
        for sequence in 1..=MAX_EVENTS as u64 + 1 {
            Remote::push_event(
                &events,
                Event {
                    kind: EventKind::Inventory,
                    sequence,
                    session: None,
                },
            )
            .unwrap();
        }
        let queue = events.lock().unwrap();
        assert_eq!(queue.len(), 1);
        assert_eq!(queue[0].kind, EventKind::Resync);
        assert_eq!(queue[0].sequence, MAX_EVENTS as u64 + 1);
    }

    #[test]
    fn remote_admission_cannot_use_reserved_management_slots() {
        let usb = TcpListener::bind("127.0.0.1:0").unwrap();
        let control = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = usb.local_addr().unwrap();
        let mut listeners = Listeners::new(usb, control).unwrap();
        let clients: Vec<_> = (0..MAX_CONNECTIONS)
            .map(|_| TcpStream::connect(address).unwrap())
            .collect();
        let capacity = MAX_CONNECTIONS - MANAGEMENT_RESERVE;
        let accepted = listeners.accept(capacity, &mut Rate::new()).unwrap();
        assert_eq!(accepted.len(), capacity);
        assert!(accepted.iter().all(|(channel, _)| *channel == Channel::Usb));
        drop(clients);
    }

    #[test]
    fn tickets_are_peer_bound_expiring_and_consumed_once() {
        let first = Identity::generate().unwrap();
        let second = Identity::generate().unwrap();
        let mut peers = Peers::default();
        for identity in [&first, &second] {
            peers
                .pair(identity.certificate().to_vec(), identity.peer())
                .unwrap();
        }
        let mut state = State {
            peers,
            links: BTreeMap::new(),
            next: 1,
        };
        state.links.insert(
            1,
            Link {
                peer: first.peer(),
                ticket: Some([42; TICKET_BYTES]),
                issued: Instant::now(),
                lease: Arc::new(Lease::new()),
            },
        );
        assert!(matches!(
            state.consume_ticket(second.peer(), &[42; TICKET_BYTES]),
            Err(Error::Denied)
        ));
        state
            .consume_ticket(first.peer(), &[42; TICKET_BYTES])
            .unwrap();
        assert!(matches!(
            state.consume_ticket(first.peer(), &[42; TICKET_BYTES]),
            Err(Error::Denied)
        ));
        let link = state.links.get_mut(&1).unwrap();
        link.ticket = Some([43; TICKET_BYTES]);
        link.issued = Instant::now() - TIMEOUT;
        assert!(matches!(
            state.consume_ticket(first.peer(), &[43; TICKET_BYTES]),
            Err(Error::Deadline)
        ));
        state.peers.revoke(first.peer());
        assert!(matches!(
            state.consume_ticket(first.peer(), &[43; TICKET_BYTES]),
            Err(Error::Denied)
        ));
    }
    #[test]
    fn an_expired_heartbeat_cannot_resurrect_a_lease() {
        let lease = Lease::new();
        *lease.last.lock().unwrap() = Instant::now() - LEASE_TIMEOUT;
        assert_eq!(lease.renew(), Err(Error::Deadline));
        assert_eq!(lease.check(), Err(Error::Deadline));
        let lease = Lease::new();
        lease.close();
        lease.close();
        assert_eq!(lease.renew(), Err(Error::Unavailable));
    }
}
