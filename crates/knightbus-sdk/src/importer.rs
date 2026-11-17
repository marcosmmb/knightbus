//! Importer hosting. Only lifecycle metadata crosses control IPC; the existing
//! transport Forwarder and native backend own every USB request.
#[cfg(windows)]
use crate::policy::ATTACH_TIMEOUT_MS;
use crate::{
    config::{Configuration, Role},
    policy::CONTROL_TIMEOUT_MS,
};
use knightbus_core::*;
use knightbus_transport::operation::Operation;
use knightbus_transport::{host::Remote, security::Store};
use std::time::{Duration, Instant};

// Covers the full inventory, including conservative decoder reallocation charges
// and TLS storage. This is an admission ceiling, not an upfront allocation.
const DISCOVERY_RESOURCES: ResourceLimits = ResourceLimits {
    bytes: 32 * 1024 * 1024,
    requests: 4,
};
// Discovery carries metadata only and never admits native transfer work.
const DISCOVERY_CAPABILITIES: Capabilities = Capabilities {
    transfer_bytes: 1024,
    requests: 1,
    iso_packets: 0,
    speeds: knightbus_core::topology::Speed::ALL_MASK,
    descriptor_bytes: 4096,
    configurations: 1,
    remote_wakeup: false,
};

pub(crate) fn connect(
    config: &Configuration,
    operation: &Operation<'_>,
) -> Result<(knightbus_transport::Stream, Remote)> {
    let Role::Importer { peer } = config.role else {
        return Err(Error::Invalid);
    };
    let store = Store::open(&config.state)?;
    let identity = store.identity()?;
    let peers = store.peers()?;
    operation.check()?;
    let mut remote = Remote::connect_until(
        operation.connect(config.control)?,
        &identity,
        &peers,
        peer,
        operation,
    )?;
    let stream = remote.usb_until(operation.connect(config.usb)?, &identity, &peers, operation)?;
    Ok((stream, remote))
}
/// Authenticated discovery at explicitly configured addresses. No device is claimed.
pub(crate) fn discover(config: &Configuration) -> Result<Vec<usbip::ListedDevice>> {
    discover_until(
        config,
        &Operation::new(
            Instant::now() + Duration::from_millis(CONTROL_TIMEOUT_MS),
            &|| false,
        ),
    )
}
fn discover_until(
    config: &Configuration,
    operation: &Operation<'_>,
) -> Result<Vec<usbip::ListedDevice>> {
    let (stream, _remote) = connect(config, operation)?;
    inventory(stream, operation)
}
#[cfg(windows)]
fn discover_named_until(
    config: &Configuration,
    operation: &Operation<'_>,
) -> Result<(Vec<usbip::ListedDevice>, Vec<DeviceInfo>)> {
    let (stream, remote) = connect(config, operation)?;
    let devices = inventory(stream, operation)?;
    Ok((devices, remote.inventory().to_vec()))
}
fn inventory(
    stream: knightbus_transport::Stream,
    operation: &Operation<'_>,
) -> Result<Vec<usbip::ListedDevice>> {
    knightbus_transport::importer::Wire::new(
        stream,
        Budget::new(DISCOVERY_RESOURCES),
        Duration::from_millis(CONTROL_TIMEOUT_MS),
        DISCOVERY_CAPABILITIES,
    )?
    .list_until(operation)
}
#[cfg(not(windows))]
pub(crate) fn start(_: &Configuration) -> Result<crate::Runtime> {
    Err(Error::Unsupported)
}
#[cfg(windows)]
pub(crate) use native::start;

#[cfg(windows)]
mod native {
    use super::*;
    use crate::runtime::{Managed, Runtime};
    use std::{
        collections::{BTreeMap, VecDeque},
        sync::{Arc, Condvar, Mutex},
        task::Poll,
        time::Instant,
    };
    const MAX_EVENTS: usize = 64;
    const MAX_RETAINED_SESSIONS: usize = 64;
    const MAX_SUBSCRIPTIONS: usize = 32;
    const WORKER_REAP_INTERVAL: Duration = Duration::from_millis(100);
    const TRUST_REFRESH_INTERVAL: Duration = Duration::from_secs(1);
    const FORWARDING_RESOURCES: ResourceLimits = ResourceLimits {
        bytes: 96 * 1024 * 1024,
        requests: 64,
    };

    struct State {
        stopped: bool,
        cleanup_failed: bool,
        generation: u64,
        inventory: BTreeMap<DeviceKey, usbip::ListedDevice>,
        sessions: BTreeMap<SessionId, Session>,
        descriptions: BTreeMap<DeviceKey, Device>,
        next: u64,
        subscription: u64,
        events: BTreeMap<u64, (u64, VecDeque<Event>)>,
        jobs: VecDeque<(SessionId, DeviceKey, u64)>,
    }
    struct Host {
        config: Configuration,
        epoch: u64,
        clock: Instant,
        state: Mutex<State>,
        wake: Condvar,
        budget: Budget,
    }
    impl Host {
        fn now(&self) -> u64 {
            self.clock.elapsed().as_millis() as u64
        }
        fn emit(state: &mut State, kind: EventKind, session: Option<SessionId>) {
            for (sequence, queue) in state.events.values_mut() {
                *sequence = sequence.saturating_add(1);
                if queue.len() >= MAX_EVENTS {
                    queue.clear();
                    queue.push_back(Event {
                        sequence: *sequence,
                        kind: EventKind::Resync,
                        session: None,
                    });
                } else {
                    queue.push_back(Event {
                        sequence: *sequence,
                        kind,
                        session,
                    });
                }
            }
        }
        fn update(&self, id: SessionId, lifecycle: SessionState, error: Option<Error>) {
            if let Ok(mut state) = self.state.lock() {
                let session = state.sessions.get_mut(&id).unwrap();
                // Cancellation cannot be undone by a late native attachment.
                if (matches!(
                    session.state,
                    SessionState::Draining | SessionState::Quarantined
                ) && matches!(lifecycle, SessionState::Acquiring | SessionState::Active))
                    || (session.state == SessionState::Quarantined
                        && lifecycle == SessionState::Draining)
                    || (session.state == SessionState::Closed && lifecycle != SessionState::Closed)
                {
                    return;
                }
                let error = if lifecycle == SessionState::Quarantined {
                    Some(Error::Cleanup)
                } else {
                    session.error.or(error)
                };
                if session.state != lifecycle || session.error != error {
                    session.state = lifecycle;
                    session.error = error;
                    Self::emit(&mut state, EventKind::Session, Some(id));
                }
            }
        }
        fn cancelled(&self, id: SessionId) -> bool {
            self.state.lock().map_or(true, |s| {
                s.stopped || s.sessions[&id].state == SessionState::Draining
            })
        }
        fn run(&self) -> Result<()> {
            self.run_jobs(|id, key, deadline| self.forward(id, key, deadline))
        }
        fn run_jobs(
            &self,
            forward: impl Fn(SessionId, DeviceKey, u64) -> Result<()> + Sync,
        ) -> Result<()> {
            std::thread::scope(|scope| {
                let mut workers: Vec<std::thread::ScopedJoinHandle<'_, ()>> = Vec::new();
                loop {
                    let mut index = 0;
                    while index < workers.len() {
                        if workers[index].is_finished() {
                            workers
                                .swap_remove(index)
                                .join()
                                .map_err(|_| Error::Cleanup)?;
                        } else {
                            index += 1;
                        }
                    }
                    let mut state = self.state.lock().map_err(|_| Error::Internal)?;
                    if state.stopped && state.jobs.is_empty() && workers.is_empty() {
                        return if state.cleanup_failed {
                            Err(Error::Cleanup)
                        } else {
                            Ok(())
                        };
                    }
                    if workers.len() < MAX_IMPORT_ATTACHMENTS
                        && let Some((id, key, deadline)) = state.jobs.pop_front()
                    {
                        drop(state);
                        if self.cancelled(id) {
                            self.update(id, SessionState::Closed, Some(Error::Cancelled));
                            continue;
                        }
                        let forward = &forward;
                        match std::thread::Builder::new()
                            .name("usb-import".into())
                            .spawn_scoped(scope, move || {
                                // A panic cannot establish native retirement either.
                                let result =
                                    std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                                        forward(id, key, deadline)
                                    }))
                                    .unwrap_or(Err(Error::Cleanup));
                                if result == Err(Error::Cleanup) {
                                    if let Ok(mut state) = self.state.lock() {
                                        state.cleanup_failed = true;
                                    }
                                    self.update(
                                        id,
                                        SessionState::Quarantined,
                                        Some(Error::Cleanup),
                                    );
                                } else {
                                    self.update(id, SessionState::Closed, result.err());
                                }
                                self.wake.notify_all();
                            }) {
                            Ok(worker) => workers.push(worker),
                            Err(_) => {
                                self.update(id, SessionState::Closed, Some(Error::Unavailable))
                            }
                        }
                        continue;
                    }
                    // Completion can notify just before the thread becomes joinable.
                    drop(
                        self.wake
                            .wait_timeout(state, WORKER_REAP_INTERVAL)
                            .map_err(|_| Error::Internal)?,
                    );
                }
            })
        }
        fn forward(&self, id: SessionId, key: DeviceKey, deadline: u64) -> Result<()> {
            let cancelled = || self.cancelled(id);
            let operation = Operation::new(
                self.clock
                    .checked_add(Duration::from_millis(deadline))
                    .ok_or(Error::Invalid)?,
                &cancelled,
            );
            let (stream, remote) = connect(&self.config, &operation)?;
            let budget = self.budget.clone();
            let mut forwarding = knightbus_windows_importer::backend::connect_until(
                stream,
                key.id.bus_id(),
                id,
                budget,
                Duration::from_millis(deadline.saturating_sub(self.now()).min(ATTACH_TIMEOUT_MS)),
                &operation,
                || Ok(knightbus_windows_importer::native::Controller::owned()),
            )?;
            let mut checked = Instant::now();
            loop {
                let expired =
                    self.now() >= deadline && forwarding.state() == ImporterState::Attaching;
                let mut failure = remote.check().err();
                if checked.elapsed() >= TRUST_REFRESH_INTERVAL {
                    checked = Instant::now();
                    let Role::Importer { peer } = self.config.role else {
                        unreachable!()
                    };
                    match Store::open(&self.config.state).and_then(|store| store.peers()) {
                        Ok(peers) if peers.contains(peer) => {}
                        Ok(_) => failure = Some(Error::Denied),
                        Err(error) => failure = Some(error),
                    }
                }
                if self.cancelled(id) || expired || failure.is_some() {
                    self.update(
                        id,
                        SessionState::Draining,
                        failure.or(if expired {
                            Some(Error::Deadline)
                        } else {
                            Some(Error::Cancelled)
                        }),
                    );
                    forwarding.disconnect();
                }
                if let Poll::Ready(result) = forwarding.tick() {
                    return result;
                }
                let state = match forwarding.state() {
                    ImporterState::Active => SessionState::Active,
                    ImporterState::Attaching => SessionState::Acquiring,
                    ImporterState::Draining => SessionState::Draining,
                    ImporterState::Quarantined => SessionState::Quarantined,
                    ImporterState::Idle => SessionState::Closed,
                };
                if let Some(device) = forwarding.device() {
                    let mut shared = self.state.lock().map_err(|_| Error::Internal)?;
                    shared.descriptions.entry(key).or_insert_with(|| {
                        let mut d = device.clone();
                        d.key = key;
                        d
                    });
                }
                self.update(
                    id,
                    state,
                    (state == SessionState::Quarantined).then_some(Error::Cleanup),
                );
                forwarding.wait();
            }
        }
    }
    impl Managed for Host {
        fn execute(&self, caller: Caller, command: Command, deadline: u64) -> Result<Reply> {
            if !caller.administrator {
                return Err(Error::Denied);
            }
            if self.now() >= deadline {
                return Err(Error::Deadline);
            }
            // Discovery authenticates before the remote exporter reveals its inventory.
            let discovered = if command == Command::List {
                Some(discover_named_until(
                    &self.config,
                    &Operation::new(
                        self.clock
                            .checked_add(Duration::from_millis(deadline))
                            .ok_or(Error::Invalid)?,
                        &|| self.state.lock().map_or(true, |s| s.stopped),
                    ),
                )?)
            } else {
                None
            };
            let mut s = self.state.lock().map_err(|_| Error::Internal)?;
            if self.now() >= deadline {
                return Err(Error::Deadline);
            }
            match command {
                Command::Info => Ok(Reply::Info {
                    version: CONTROL_VERSION,
                    epoch: self.epoch,
                    now: self.now(),
                }),
                Command::List => {
                    if s.stopped {
                        return Err(Error::State);
                    }
                    // Every refresh invalidates earlier selection tokens. USB/IP has no
                    // exporter generation field; these are local discovery generations.
                    s.generation = s.generation.checked_add(1).ok_or(Error::Exhausted)?;
                    let generation = s.generation;
                    s.inventory.clear();
                    let (discovered, metadata) = discovered.unwrap();
                    for d in discovered {
                        let bus = std::str::from_utf8(d.device.bus_id.as_bytes())
                            .map_err(|_| Error::Invalid)?;
                        let id = u64::from_str_radix(
                            bus.strip_prefix("kb-").ok_or(Error::Unsupported)?,
                            16,
                        )
                        .map_err(|_| Error::Invalid)?;
                        s.inventory.insert(
                            DeviceKey {
                                id: DeviceId(id),
                                generation,
                            },
                            d,
                        );
                    }
                    Self::emit(&mut s, EventKind::Inventory, None);
                    Ok(Reply::Devices(
                        s.inventory
                            .iter()
                            .map(|(key, entry)| DeviceInfo {
                                name: metadata
                                    .iter()
                                    .find(|d| {
                                        d.key.id == key.id
                                            && d.vendor == entry.device.vendor_id
                                            && d.product == entry.device.product_id
                                    })
                                    .and_then(|d| d.name.clone()),
                                key: *key,
                                vendor: entry.device.vendor_id,
                                product: entry.device.product_id,
                                speed: entry.device.speed,
                                class: entry.device.class,
                                shared: true,
                                busy: s.sessions.values().any(|session| {
                                    session.state != SessionState::Closed
                                        && session.devices.iter().any(|d| d.id == key.id)
                                }),
                            })
                            .collect(),
                    ))
                }
                Command::Inspect(key) => s
                    .descriptions
                    .get(&key)
                    .cloned()
                    .map(|d| Reply::Inspection(Box::new(d)))
                    .ok_or(Error::Unavailable),
                Command::Attach(keys) => {
                    if s.stopped {
                        return Err(Error::State);
                    }
                    if keys.len() != 1 || !s.inventory.contains_key(&keys[0]) {
                        return Err(Error::NotFound);
                    }
                    let active: Vec<_> = s
                        .sessions
                        .values()
                        .filter(|v| v.state != SessionState::Closed)
                        .collect();
                    if active
                        .iter()
                        .any(|v| v.devices.iter().any(|d| d.id == keys[0].id))
                    {
                        return Err(Error::State);
                    }
                    if active.len() >= MAX_IMPORT_ATTACHMENTS {
                        return Err(Error::Exhausted);
                    }
                    if s.sessions.len() >= MAX_RETAINED_SESSIONS {
                        let oldest = *s
                            .sessions
                            .iter()
                            .find(|(_, v)| v.state == SessionState::Closed)
                            .ok_or(Error::Exhausted)?
                            .0;
                        let retired = s.sessions.remove(&oldest).unwrap();
                        for key in retired.devices {
                            s.descriptions.remove(&key);
                        }
                    }
                    s.next = s.next.checked_add(1).ok_or(Error::Exhausted)?;
                    let id = SessionId {
                        epoch: self.epoch,
                        serial: s.next,
                    };
                    let Role::Importer { peer } = self.config.role else {
                        unreachable!()
                    };
                    let session = Session {
                        id,
                        peer,
                        devices: keys.clone(),
                        state: SessionState::Acquiring,
                        error: None,
                    };
                    s.sessions.insert(id, session.clone());
                    s.jobs.push_back((id, keys[0], deadline));
                    Self::emit(&mut s, EventKind::Session, Some(id));
                    self.wake.notify_all();
                    Ok(Reply::Session(session))
                }
                Command::Sessions => Ok(Reply::Sessions(s.sessions.keys().copied().collect())),
                Command::Status(id) => s
                    .sessions
                    .get(&id)
                    .cloned()
                    .map(Reply::Session)
                    .ok_or(Error::NotFound),
                Command::Detach(id) => {
                    let session = s.sessions.get_mut(&id).ok_or(Error::NotFound)?;
                    if !matches!(
                        session.state,
                        SessionState::Closed | SessionState::Quarantined
                    ) {
                        session.state = SessionState::Draining;
                        Self::emit(&mut s, EventKind::Session, Some(id));
                    }
                    self.wake.notify_all();
                    Ok(Reply::Done)
                }
                Command::Forget(id) => {
                    if s.sessions.get(&id).ok_or(Error::NotFound)?.state != SessionState::Closed {
                        return Err(Error::State);
                    }
                    s.sessions.remove(&id);
                    Ok(Reply::Done)
                }
                Command::Subscribe => {
                    if s.events.len() >= MAX_SUBSCRIPTIONS {
                        return Err(Error::Exhausted);
                    }
                    s.subscription = s.subscription.checked_add(1).ok_or(Error::Exhausted)?;
                    let id = s.subscription;
                    s.events.insert(id, (0, VecDeque::new()));
                    Ok(Reply::Subscription(id))
                }
                Command::Poll(id) => Ok(Reply::Event(
                    s.events.get_mut(&id).ok_or(Error::Denied)?.1.pop_front(),
                )),
                Command::Unsubscribe(id) => {
                    s.events.remove(&id).ok_or(Error::Denied)?;
                    Ok(Reply::Done)
                }
                Command::Shutdown => {
                    s.stopped = true;
                    self.wake.notify_all();
                    Ok(Reply::Done)
                }
                _ => Err(Error::Unsupported),
            }
        }
        fn shutdown(&self) {
            if let Ok(mut s) = self.state.lock() {
                s.stopped = true;
                self.wake.notify_all();
            }
        }
    }
    pub(crate) fn start(config: &Configuration) -> Result<Runtime> {
        let Role::Importer { peer } = config.role else {
            return Err(Error::Invalid);
        };
        let store = Store::open(&config.state)?;
        store.identity()?;
        if !store.peers()?.contains(peer) {
            return Err(Error::Denied);
        }
        let host = Arc::new(Host {
            config: config.clone(),
            epoch: knightbus_transport::security::epoch()?,
            clock: Instant::now(),
            state: Mutex::new(State {
                stopped: false,
                cleanup_failed: false,
                generation: 0,
                inventory: BTreeMap::new(),
                sessions: BTreeMap::new(),
                descriptions: BTreeMap::new(),
                next: 0,
                subscription: 0,
                events: BTreeMap::new(),
                jobs: VecDeque::new(),
            }),
            wake: Condvar::new(),
            budget: Budget::new(FORWARDING_RESOURCES),
        });
        let runner = host.clone();
        Runtime::spawn(host, move || runner.run())
    }
    #[cfg(test)]
    mod tests {
        use super::*;
        fn test_host() -> Host {
            let id = SessionId {
                epoch: 1,
                serial: 1,
            };
            Host {
                config: Configuration {
                    role: Role::Importer {
                        peer: PeerId([1; 32]),
                    },
                    state: std::env::temp_dir(),
                    usb: "127.0.0.1:1".parse().unwrap(),
                    control: "127.0.0.1:2".parse().unwrap(),
                    mdns_name: None,
                },
                epoch: 1,
                clock: Instant::now(),
                wake: Condvar::new(),
                budget: Budget::new(FORWARDING_RESOURCES),
                state: Mutex::new(State {
                    stopped: false,
                    cleanup_failed: false,
                    generation: 0,
                    inventory: BTreeMap::new(),
                    descriptions: BTreeMap::new(),
                    next: 1,
                    subscription: 1,
                    jobs: VecDeque::new(),
                    sessions: [(
                        id,
                        Session {
                            id,
                            peer: PeerId([1; 32]),
                            devices: vec![],
                            state: SessionState::Acquiring,
                            error: None,
                        },
                    )]
                    .into(),
                    events: [(1, (0, VecDeque::new()))].into(),
                }),
            }
        }
        fn admit(host: &Host, number: u64) -> SessionId {
            let key = DeviceKey {
                id: DeviceId(number),
                generation: 1,
            };
            host.state.lock().unwrap().inventory.insert(
                key,
                usbip::ListedDevice {
                    device: usbip::Device {
                        path: usbip::WireString::new(b"test").unwrap(),
                        bus_id: key.id.bus_id(),
                        bus_number: 1,
                        device_number: number as u32,
                        speed: 2,
                        vendor_id: 1,
                        product_id: 1,
                        device_bcd: 1,
                        class: 0,
                        subclass: 0,
                        protocol: 0,
                        configuration: 1,
                        configurations: 1,
                        interfaces: 0,
                    },
                    interfaces: vec![],
                },
            );
            let Reply::Session(session) = host
                .execute(crate::runtime::ADMIN, Command::Attach(vec![key]), u64::MAX)
                .unwrap()
            else {
                panic!()
            };
            session.id
        }
        #[test]
        fn independent_imports_detach_and_shutdown_without_blocking_each_other() {
            use std::sync::mpsc;
            const TEST_TIMEOUT: Duration = Duration::from_secs(5);
            let host = test_host();
            host.state.lock().unwrap().sessions.clear();
            let first = admit(&host, 1);
            let second = admit(&host, 2);
            let (started, arrivals) = mpsc::channel();
            let (closed, departures) = mpsc::channel();
            std::thread::scope(|scope| {
                let runner = scope.spawn(|| {
                    host.run_jobs(|id, _, _| {
                        host.update(id, SessionState::Active, None);
                        started.send(id).unwrap();
                        let end = Instant::now() + TEST_TIMEOUT;
                        while !host.cancelled(id) && Instant::now() < end {
                            std::thread::sleep(Duration::from_millis(1));
                        }
                        assert!(host.cancelled(id), "worker did not receive cancellation");
                        closed.send(id).unwrap();
                        Ok(())
                    })
                });
                let mut active = [
                    arrivals.recv_timeout(TEST_TIMEOUT).unwrap(),
                    arrivals.recv_timeout(TEST_TIMEOUT).unwrap(),
                ];
                active.sort();
                assert_eq!(active, [first, second]);
                host.execute(crate::runtime::ADMIN, Command::Detach(first), u64::MAX)
                    .unwrap();
                assert_eq!(departures.recv_timeout(TEST_TIMEOUT).unwrap(), first);
                assert_eq!(
                    host.state.lock().unwrap().sessions[&second].state,
                    SessionState::Active
                );
                host.shutdown();
                assert_eq!(departures.recv_timeout(TEST_TIMEOUT).unwrap(), second);
                assert_eq!(runner.join().unwrap(), Ok(()));
            });
            assert!(
                host.state
                    .lock()
                    .unwrap()
                    .sessions
                    .values()
                    .all(|s| s.state == SessionState::Closed)
            );
            assert_eq!(host.budget.usage().bytes, 0);
        }
        #[test]
        fn admission_bounds_active_imports_and_retains_quarantined_devices() {
            let host = test_host();
            host.state.lock().unwrap().sessions.clear();
            let first = admit(&host, 1);
            for number in 2..=MAX_IMPORT_ATTACHMENTS as u64 {
                admit(&host, number);
            }
            let old = DeviceKey {
                id: DeviceId(1),
                generation: 1,
            };
            let fresh = DeviceKey {
                generation: 2,
                ..old
            };
            {
                let mut state = host.state.lock().unwrap();
                let entry = state.inventory[&old].clone();
                state.inventory.insert(fresh, entry);
            }
            assert_eq!(
                host.execute(
                    crate::runtime::ADMIN,
                    Command::Attach(vec![fresh]),
                    u64::MAX
                ),
                Err(Error::State)
            );
            host.update(first, SessionState::Quarantined, Some(Error::Cleanup));
            assert_eq!(
                host.execute(crate::runtime::ADMIN, Command::Forget(first), u64::MAX),
                Err(Error::State)
            );
            let other = DeviceKey {
                id: DeviceId(100),
                generation: 1,
            };
            {
                let mut state = host.state.lock().unwrap();
                let entry = state.inventory[&old].clone();
                state.inventory.insert(other, entry);
            }
            assert_eq!(
                host.execute(
                    crate::runtime::ADMIN,
                    Command::Attach(vec![other]),
                    u64::MAX
                ),
                Err(Error::Exhausted)
            );
        }
        #[test]
        fn cancellation_and_quarantine_are_monotonic_and_events_do_not_spin() {
            let id = SessionId {
                epoch: 1,
                serial: 1,
            };
            let host = test_host();
            host.update(id, SessionState::Draining, Some(Error::Cancelled));
            host.update(id, SessionState::Active, None);
            for _ in 0..100 {
                host.update(id, SessionState::Draining, None);
            }
            {
                let s = host.state.lock().unwrap();
                assert_eq!(s.sessions[&id].state, SessionState::Draining);
                assert_eq!(s.sessions[&id].error, Some(Error::Cancelled));
                assert_eq!(s.events[&1].1.len(), 1);
            }
            host.update(id, SessionState::Quarantined, Some(Error::Cleanup));
            host.update(id, SessionState::Draining, Some(Error::Cancelled));
            let admin = Caller {
                peer: PeerId([0; 32]),
                administrator: true,
            };
            host.execute(admin, Command::Detach(id), u64::MAX).unwrap();
            assert_eq!(
                host.execute(admin, Command::Forget(id), u64::MAX),
                Err(Error::State)
            );
            let s = host.state.lock().unwrap();
            assert_eq!(s.sessions[&id].state, SessionState::Quarantined);
            assert_eq!(s.sessions[&id].error, Some(Error::Cleanup));
            assert_eq!(s.events[&1].1.len(), 2);
        }
    }
}

#[cfg(test)]
mod discovery_tests {
    use super::*;
    use std::net::{TcpListener, TcpStream};

    #[test]
    fn discovery_budget_covers_the_full_inventory_and_interface_limits() {
        for count in [4, 128, 129] {
            let listener = TcpListener::bind("127.0.0.1:0").unwrap();
            let socket = TcpStream::connect(listener.local_addr().unwrap()).unwrap();
            let (mut peer, _) = listener.accept().unwrap();
            let server = std::thread::spawn(move || {
                let limits = usbip::Limits {
                    devices: 129,
                    ..usbip::Limits::default()
                };
                assert_eq!(
                    usbip::read_message(
                        &mut peer,
                        usbip::Context::Management,
                        limits,
                        &mut |_| Ok(())
                    )
                    .unwrap(),
                    Some(usbip::Message::DevListRequest)
                );
                let devices = (0..count)
                    .map(|id| usbip::ListedDevice {
                        device: usbip::Device {
                            path: usbip::WireString::new(b"fixture").unwrap(),
                            bus_id: DeviceId(id).bus_id(),
                            bus_number: 1,
                            device_number: id as u32,
                            speed: 2,
                            vendor_id: 1,
                            product_id: 1,
                            device_bcd: 0x100,
                            class: 0,
                            subclass: 0,
                            protocol: 0,
                            configuration: 1,
                            configurations: 1,
                            interfaces: 255,
                        },
                        interfaces: vec![
                            usbip::Interface {
                                class: 3,
                                subclass: 0,
                                protocol: 0
                            };
                            255
                        ],
                    })
                    .collect();
                // Oversized input can be rejected before the server finishes writing.
                let _ = usbip::write_message(
                    &mut peer,
                    &usbip::Message::DevListReply { status: 0, devices },
                    limits,
                    &mut |_| Ok(()),
                );
            });
            let stream = knightbus_transport::Stream::insecure_loopback(socket).unwrap();
            let operation = Operation::new(Instant::now() + Duration::from_secs(5), &|| false);
            let result = inventory(stream, &operation);
            if count <= 128 {
                let devices = result.unwrap();
                assert_eq!(devices.len(), count as usize);
                assert!(devices.iter().all(|device| device.interfaces.len() == 255));
            } else {
                assert_eq!(result, Err(Error::Invalid));
            }
            server.join().unwrap();
        }
    }
}
