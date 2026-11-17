//! Deterministic native retirement, policy and shared host contract scenarios.
use knightbus_core::{Device, Interface};
use knightbus_core::{usbip::*, *};
use knightbus_ipc::{IpcControl, Service, decode_request, encode_request};
use std::{
    cell::{Cell, RefCell},
    collections::{BTreeMap, BTreeSet, VecDeque},
    rc::Rc,
    task::Poll,
};

const PEER: PeerId = PeerId([1; 32]);
const OTHER: PeerId = PeerId([2; 32]);
const USER: Caller = Caller {
    peer: PEER,
    administrator: false,
};
const ADMIN: Caller = Caller {
    peer: PEER,
    administrator: true,
};
const KEY: DeviceKey = DeviceKey {
    id: DeviceId(1),
    generation: 1,
};
const END: u64 = 10_000;

#[derive(Clone, Default)]
struct Time(Rc<Cell<u64>>);
impl Clock for Time {
    fn now(&self) -> u64 {
        self.0.get()
    }
}
#[derive(Default)]
struct Native {
    operations: BTreeMap<TransferId, Transfer>,
    ready: VecDeque<TransferId>,
    cancelled: BTreeSet<TransferId>,
    submitted: Vec<TransferId>,
    claimed: BTreeSet<(SessionId, DeviceKey)>,
    released: Vec<DeviceKey>,
    fail_claim: Option<DeviceKey>,
    pending_claim: bool,
    pending_release: bool,
    fail_release: bool,
    fail_submit: bool,
    corrupt_iso: bool,
}
#[derive(Clone, Default)]
struct Mock(Rc<RefCell<Native>>);
impl ExporterBackend for Mock {
    fn manages_topology(&self) -> bool {
        false
    }
    fn poll_removed(&mut self) -> Option<knightbus_core::DeviceKey> {
        None
    }
    fn set_waker(&mut self, waker: Option<std::task::Waker>) {
        if let Some(waker) = waker {
            waker.wake();
        }
    }
    fn claim(&mut self, session: SessionId, device: &Device) -> Poll<knightbus_core::Result<()>> {
        let mut n = self.0.borrow_mut();
        n.claimed.insert((session, device.key));
        if n.pending_claim {
            Poll::Pending
        } else if n.fail_claim == Some(device.key) {
            Poll::Ready(Err(knightbus_core::Error::Unavailable))
        } else {
            Poll::Ready(Ok(()))
        }
    }
    fn submit(
        &mut self,
        id: TransferId,
        transfer: Transfer,
    ) -> std::result::Result<(), (knightbus_core::Error, Transfer)> {
        let mut n = self.0.borrow_mut();
        if n.fail_submit {
            return Err((knightbus_core::Error::Unavailable, transfer));
        }
        n.submitted.push(id);
        n.operations.insert(id, transfer);
        Ok(())
    }
    fn cancel(&mut self, id: TransferId) {
        self.0.borrow_mut().cancelled.insert(id);
    }
    fn poll_completion(&mut self) -> Option<(TransferId, Completion)> {
        let mut n = self.0.borrow_mut();
        let id = n.ready.pop_front()?;
        let mut transfer = n.operations.remove(&id)?;
        if n.corrupt_iso {
            transfer.packets_mut()[0].offset += 1;
        }
        let actual_length = if transfer.layout().is_empty() {
            transfer.header().transfer_buffer_length
        } else {
            0
        };
        Some((
            id,
            Completion {
                transfer,
                status: 0,
                actual_length,
                start_frame: 0,
            },
        ))
    }
    fn release(
        &mut self,
        session: SessionId,
        device: DeviceKey,
    ) -> Poll<knightbus_core::Result<()>> {
        let mut n = self.0.borrow_mut();
        if n.pending_release {
            return Poll::Pending;
        }
        if n.fail_release {
            return Poll::Ready(Err(knightbus_core::Error::Unavailable));
        }
        n.claimed.remove(&(session, device));
        n.released.push(device);
        Poll::Ready(Ok(()))
    }
}
type TestEngine = Engine<Mock, Time>;
fn device(key: DeviceKey) -> Device {
    Device {
        key,
        wire: knightbus_core::usbip::Device {
            path: WireString::new(b"/mock/device").unwrap(),
            bus_id: key.id.bus_id(),
            bus_number: 1,
            device_number: key.id.0 as u32,
            speed: 3,
            vendor_id: 0x1234,
            product_id: 0x5678,
            device_bcd: 0x100,
            class: 0,
            subclass: 0,
            protocol: 0,
            configuration: 1,
            configurations: 1,
            interfaces: 1,
        },
        configurations: vec![Configuration {
            value: 1,
            interfaces: vec![Interface {
                number: 0,
                alternate: 0,
                class: knightbus_core::usbip::Interface {
                    class: 0xff,
                    subclass: 0,
                    protocol: 0,
                },
                endpoints: vec![
                    Endpoint {
                        address: 0x81,
                        kind: TransferKind::Bulk,
                        max_packet_size: 512,
                        service_bytes: 512,
                        interval: 0,
                        allowed_flags: 0,
                    },
                    Endpoint {
                        address: 0x82,
                        kind: TransferKind::Isochronous,
                        max_packet_size: 512,
                        service_bytes: 512,
                        interval: 1,
                        allowed_flags: 0,
                    },
                ],
            }],
        }],
    }
}
fn fixture(config: Config) -> (TestEngine, Mock, Time) {
    let mock = Mock::default();
    let time = Time::default();
    let mut e = Engine::new(time.clone(), config, 7, |_| Ok(mock.clone())).unwrap();
    e.register(device(KEY)).unwrap();
    (e, mock, time)
}
fn grant(e: &mut TestEngine, key: DeviceKey, peer: PeerId) {
    e.execute(
        ADMIN,
        Command::Authorize {
            device: key,
            peer,
            allow: true,
        },
        END,
    )
    .unwrap();
}
fn attach(e: &mut TestEngine, keys: Vec<DeviceKey>) -> SessionId {
    let Reply::Session(s) = e.execute(USER, Command::Attach(keys), END).unwrap() else {
        panic!()
    };
    s.id
}
fn active(e: &mut TestEngine) -> SessionId {
    grant(e, KEY, PEER);
    let id = attach(e, vec![KEY]);
    e.tick();
    assert_eq!(status(e, id).state, SessionState::Active);
    id
}
fn status(e: &mut TestEngine, id: SessionId) -> Session {
    let Reply::Session(s) = e.execute(USER, Command::Status(id), u64::MAX).unwrap() else {
        panic!()
    };
    s
}
fn header(sequence: u32, size: u32) -> SubmitHeader {
    SubmitHeader {
        seqnum: sequence,
        device_id: 0x10001,
        direction: Direction::In,
        endpoint: 1,
        transfer_flags: 0,
        transfer_buffer_length: size,
        start_frame: 0,
        number_of_packets: 0,
        interval: 0,
        setup: [0; 8],
    }
}
fn transfer(e: &TestEngine, sequence: u32, size: u32) -> Transfer {
    Transfer::new(&e.budget(), header(sequence, size), &[]).unwrap()
}
fn ready(mock: &Mock, session: SessionId, sequence: u32) {
    mock.0
        .borrow_mut()
        .ready
        .push_back(TransferId { session, sequence });
}
fn error() -> knightbus_core::Error {
    knightbus_core::Error::Denied
}

#[test]
fn shared_control_suite_through_both_hosts() {
    // This exact scenario is run through both adapters, not separate imitations.
    fn suite(control: &mut dyn KnightBusControl) -> Vec<knightbus_core::Result<Reply>> {
        vec![
            control.call(Command::List, END),
            control.call(Command::Attach(vec![KEY]), END),
            control.call(
                Command::Authorize {
                    device: KEY,
                    peer: PEER,
                    allow: true,
                },
                END,
            ),
            control.call(Command::Shutdown, END),
            control.call(Command::List, 0),
        ]
    }
    let (mut direct, _, _) = fixture(Config::default());
    let (mut service, _, _) = fixture(Config::default());
    let a = suite(&mut ControlConnection::new(&mut direct, USER, CONTROL_VERSION).unwrap());
    let b = suite(&mut IpcControl::new(&mut service, USER, true, 7, CONTROL_VERSION).unwrap());
    assert_eq!(a, b);
    assert_eq!(
        a,
        vec![
            Ok(Reply::Devices(vec![])),
            Err(error()),
            Err(error()),
            Err(error()),
            Err(knightbus_core::Error::Deadline)
        ]
    );
    assert!(ControlConnection::new(&mut direct, USER, CONTROL_VERSION + 1).is_err());
    assert!(IpcControl::new(&mut service, USER, true, 7, CONTROL_VERSION + 1).is_err());
    assert!(IpcControl::new(&mut service, USER, false, 7, CONTROL_VERSION).is_err());
}

#[test]
fn lifecycle_event_and_shutdown_parity() {
    for ipc in [false, true] {
        let (mut e, mock, _) = fixture(Config::default());
        grant(&mut e, KEY, PEER);
        let id;
        {
            let mut c: Box<dyn KnightBusControl + '_> = if ipc {
                Box::new(IpcControl::new(&mut e, USER, true, 7, CONTROL_VERSION).unwrap())
            } else {
                Box::new(ControlConnection::new(&mut e, USER, CONTROL_VERSION).unwrap())
            };
            let Reply::Subscription(sub) = c.call(Command::Subscribe, END).unwrap() else {
                panic!()
            };
            let Reply::Session(s) = c.call(Command::Attach(vec![KEY]), END).unwrap() else {
                panic!()
            };
            id = s.id;
            assert_eq!(s.state, SessionState::Acquiring);
            assert!(matches!(
                c.call(Command::Poll(sub), END),
                Ok(Reply::Event(Some(Event {
                    kind: EventKind::Session,
                    ..
                })))
            ));
            // Cancel before native acquisition; no claim may leak.
            c.call(Command::Detach(id), END).unwrap();
            c.call(Command::Detach(id), END).unwrap();
            c.call(Command::Unsubscribe(sub), END).unwrap();
            assert_eq!(c.call(Command::Poll(sub), END), Err(error()));
        }
        e.tick();
        assert_eq!(status(&mut e, id).state, SessionState::Closed);
        assert!(mock.0.borrow().claimed.is_empty());
        let id = attach(&mut e, vec![KEY]);
        e.tick();
        assert_eq!(status(&mut e, id).state, SessionState::Active);
        {
            let mut c: Box<dyn KnightBusControl + '_> = if ipc {
                Box::new(IpcControl::new(&mut e, ADMIN, true, 7, CONTROL_VERSION).unwrap())
            } else {
                Box::new(ControlConnection::new(&mut e, ADMIN, CONTROL_VERSION).unwrap())
            };
            c.call(Command::Shutdown, END).unwrap();
        }
        e.tick();
        assert!(e.shutdown_complete());
    }
}

#[test]
fn authorization_exclusivity_revoke_and_replug() {
    let (mut e, mock, _) = fixture(Config::default());
    let id = active(&mut e);
    assert_eq!(
        e.execute(USER, Command::Attach(vec![KEY]), END),
        Err(knightbus_core::Error::State)
    );
    let stranger = Caller {
        peer: OTHER,
        administrator: false,
    };
    assert_eq!(e.execute(stranger, Command::Status(id), END), Err(error()));
    assert_eq!(e.execute(stranger, Command::Detach(id), END), Err(error()));
    assert_eq!(
        e.execute(stranger, Command::Attach(vec![KEY]), END),
        Err(error())
    );
    e.execute(
        ADMIN,
        Command::Authorize {
            device: KEY,
            peer: PEER,
            allow: false,
        },
        END,
    )
    .unwrap();
    assert_eq!(status(&mut e, id).state, SessionState::Draining);
    assert_eq!(
        e.execute(USER, Command::List, END),
        Ok(Reply::Devices(vec![]))
    );
    e.tick();
    assert!(mock.0.borrow().claimed.is_empty());
    e.remove(KEY).unwrap();
    assert!(e.register(device(KEY)).is_err());
    let next = DeviceKey {
        generation: 2,
        ..KEY
    };
    e.register(device(next)).unwrap();
    assert_eq!(
        e.execute(USER, Command::Attach(vec![next]), END),
        Err(error())
    );
    grant(&mut e, next, PEER);
    assert_eq!(
        e.execute(USER, Command::Attach(vec![KEY]), END),
        Err(error())
    );
}

#[test]
fn transactional_failure_and_pending_claim_cancel_roll_back_in_reverse() {
    let (mut e, mock, _) = fixture(Config::default());
    let second = DeviceKey {
        id: DeviceId(2),
        generation: 1,
    };
    e.register(device(second)).unwrap();
    grant(&mut e, KEY, PEER);
    grant(&mut e, second, PEER);
    mock.0.borrow_mut().fail_claim = Some(second);
    let id = attach(&mut e, vec![KEY, second]);
    e.tick();
    e.tick();
    e.tick();
    assert_eq!(status(&mut e, id).state, SessionState::Closed);
    assert_eq!(mock.0.borrow().released, vec![second, KEY]);
    assert!(mock.0.borrow().claimed.is_empty());
    mock.0.borrow_mut().pending_claim = true;
    let id = attach(&mut e, vec![KEY]);
    e.tick();
    assert_eq!(status(&mut e, id).state, SessionState::Acquiring);
    e.execute(USER, Command::Detach(id), END).unwrap();
    e.tick();
    assert!(mock.0.borrow().claimed.is_empty());
}

#[test]
fn concurrency_order_correlation_and_owned_buffer_handoff() {
    let (mut e, mock, _) = fixture(Config::default());
    let id = active(&mut e);
    let baseline = e.budget().usage();
    let t = transfer(&e, 1, 1024);
    let pointer = t.buffer().as_ptr();
    e.submit(USER, id, t, END).unwrap();
    e.submit(USER, id, transfer(&e, 2, 2048), END).unwrap();
    assert_eq!(e.budget().usage().requests, 2);
    assert_eq!(
        mock.0
            .borrow()
            .submitted
            .iter()
            .map(|id| id.sequence)
            .collect::<Vec<_>>(),
        vec![1, 2]
    );
    ready(&mock, id, 2);
    ready(&mock, id, 1);
    e.tick();
    assert_eq!(e.budget().usage().requests, 0);
    let Some(TransferEvent::Complete(second)) = e.next_transfer_event(USER, id).unwrap() else {
        panic!()
    };
    let Some(TransferEvent::Complete(first)) = e.next_transfer_event(USER, id).unwrap() else {
        panic!()
    };
    assert_eq!(second.reply_context().seqnum, 2);
    assert_eq!(first.reply_context().seqnum, 1);
    assert_eq!(first.transfer.buffer().as_ptr(), pointer);
    assert!(e.budget().usage().bytes > baseline.bytes);
    drop((first, second));
    assert_eq!(e.budget().usage(), baseline);
    assert!(e.submit(USER, id, transfer(&e, 1, 1), END).is_err());
}

#[test]
fn cancellation_and_timeout_retain_credits_until_retirement() {
    let (mut e, mock, time) = fixture(Config::default());
    let id = active(&mut e);
    let baseline = e.budget().usage();
    e.submit(USER, id, transfer(&e, 1, 1024), END).unwrap();
    let charged = e.budget().usage();
    assert_eq!(e.unlink(USER, id, 2, 1), Ok(UnlinkState::Pending));
    e.tick();
    assert_eq!(e.budget().usage(), charged);
    assert!(e.next_transfer_event(USER, id).unwrap().is_none());
    ready(&mock, id, 1);
    e.tick();
    assert!(matches!(
        e.next_transfer_event(USER, id).unwrap(),
        Some(TransferEvent::Unlinked { sequence: 2 })
    ));
    assert_eq!(e.budget().usage(), baseline);
    e.submit(USER, id, transfer(&e, 3, 512), 10).unwrap();
    time.0.set(10);
    e.tick();
    assert_eq!(e.budget().usage().requests, 1);
    ready(&mock, id, 3);
    e.tick();
    assert!(matches!(
        e.next_transfer_event(USER, id).unwrap(),
        Some(TransferEvent::Failed {
            sequence: 3,
            error: knightbus_core::Error::Deadline
        })
    ));
    assert_eq!(e.budget().usage(), baseline);
}

#[test]
fn completion_wins_and_sequence_wrap_is_rejected() {
    let (mut e, mock, _) = fixture(Config::default());
    let id = active(&mut e);
    e.submit(USER, id, transfer(&e, 1, 8), END).unwrap();
    ready(&mock, id, 1);
    e.tick();
    assert_eq!(e.unlink(USER, id, 2, 1), Ok(UnlinkState::TooLate));
    assert!(matches!(
        e.next_transfer_event(USER, id).unwrap(),
        Some(TransferEvent::Complete(_))
    ));
    e.submit(USER, id, transfer(&e, u32::MAX, 1), END).unwrap();
    assert!(e.submit(USER, id, transfer(&e, 3, 1), END).is_err());
    assert!(e.unlink(USER, id, 4, u32::MAX).is_err());
}

#[test]
fn saturated_data_and_slow_consumer_do_not_block_cleanup() {
    let config = Config {
        resources: ResourceLimits {
            bytes: 8192,
            requests: 2,
        },
        ..Config::default()
    };
    let (mut e, mock, _) = fixture(config);
    let id = active(&mut e);
    e.submit(USER, id, transfer(&e, 1, 100), END).unwrap();
    e.submit(USER, id, transfer(&e, 2, 100), END).unwrap();
    assert!(Transfer::new(&e.budget(), header(3, 100), &[]).is_err());
    ready(&mock, id, 1);
    ready(&mock, id, 2);
    e.tick();
    assert_eq!(e.budget().usage().requests, 0);
    assert_eq!(
        e.submit(USER, id, transfer(&e, 3, 100), END),
        Err(knightbus_core::Error::Exhausted)
    );
    e.execute(USER, Command::Detach(id), END).unwrap();
    e.tick();
    assert_eq!(status(&mut e, id).state, SessionState::Closed);
    let id = attach(&mut e, vec![KEY]);
    e.tick();
    e.submit(USER, id, transfer(&e, 1, 100), END).unwrap();
    let remaining = config.resources.bytes - e.budget().usage().bytes;
    let scratch = e.budget().buffer(remaining).unwrap();
    assert_eq!(e.budget().usage().bytes, config.resources.bytes);
    assert_eq!(e.unlink(USER, id, 2, 1), Ok(UnlinkState::Pending));
    ready(&mock, id, 1);
    e.tick();
    drop(scratch);
    assert!(matches!(
        e.next_transfer_event(USER, id).unwrap(),
        Some(TransferEvent::Unlinked { .. })
    ));
}

#[test]
fn aggregate_admission_precedes_allocation_and_counts_capacity() {
    let b = Budget::new(ResourceLimits {
        bytes: 1024,
        requests: 1,
    });
    assert!(b.buffer(usize::MAX).is_err());
    assert_eq!(b.usage(), Usage::default());
    let buffer = b.buffer(1000).unwrap();
    assert_eq!(b.usage().bytes, buffer.capacity());
    assert!(Transfer::new(&b, header(1, 2048), &[]).is_err());
    assert_eq!(b.usage().requests, 0);
    drop(buffer);
    assert_eq!(b.usage(), Usage::default());
    let t = Transfer::new(&b, header(1, 512), &[]).unwrap();
    assert!(b.buffer(512).is_err());
    drop(t);
    assert_eq!(b.usage(), Usage::default());
}

#[test]
fn removal_quarantines_and_releases_only_after_callback_and_claim_retirement() {
    let config = Config {
        cleanup_ms: 5,
        ..Config::default()
    };
    let (mut e, mock, time) = fixture(config);
    let id = active(&mut e);
    e.submit(USER, id, transfer(&e, 1, 100), END).unwrap();
    e.remove(KEY).unwrap();
    time.0.set(6);
    e.tick();
    assert_eq!(status(&mut e, id).state, SessionState::Quarantined);
    assert_eq!(e.budget().usage().requests, 1);
    assert!(mock.0.borrow().released.is_empty());
    let key = DeviceKey {
        generation: 2,
        ..KEY
    };
    assert!(e.register(device(key)).is_err());
    mock.0.borrow_mut().pending_release = true;
    ready(&mock, id, 1);
    e.tick();
    assert_eq!(status(&mut e, id).state, SessionState::Quarantined);
    assert_eq!(e.budget().usage().requests, 0);
    mock.0.borrow_mut().pending_release = false;
    e.tick();
    assert_eq!(status(&mut e, id).state, SessionState::Closed);
    e.register(device(key)).unwrap();
}

#[test]
fn leases_expire_reconnect_tokens_stale_and_shutdown_drains() {
    let config = Config {
        lease_ms: 20,
        ..Config::default()
    };
    let (mut e, mock, time) = fixture(config);
    let first = active(&mut e);
    time.0.set(10);
    e.execute(USER, Command::Heartbeat(first), END).unwrap();
    time.0.set(20);
    e.tick();
    assert_eq!(status(&mut e, first).state, SessionState::Active);
    time.0.set(30);
    assert_eq!(
        e.execute(USER, Command::Heartbeat(first), END),
        Err(knightbus_core::Error::Deadline)
    );
    e.tick();
    assert_eq!(status(&mut e, first).state, SessionState::Closed);
    let second = attach(&mut e, vec![KEY]);
    e.tick();
    assert_ne!(first, second);
    assert!(e.submit(USER, first, transfer(&e, 1, 8), END).is_err());
    e.submit(USER, second, transfer(&e, 1, 8), END).unwrap();
    e.shutdown();
    assert!(!e.shutdown_complete());
    assert!(e.execute(USER, Command::Attach(vec![KEY]), END).is_err());
    ready(&mock, second, 1);
    e.tick();
    assert!(e.shutdown_complete());
    let mut restarted = Engine::new(time, Config::default(), 8, |_| Ok(Mock::default())).unwrap();
    assert_eq!(
        restarted.execute(USER, Command::Status(second), END),
        Err(error())
    );
}

#[test]
fn malformed_endpoint_setup_flags_iso_identity_and_foreign_budget_rejected() {
    let (mut e, mock, _) = fixture(Config::default());
    let id = active(&mut e);
    for mutate in [0, 1, 2] {
        let mut h = header(1, 8);
        match mutate {
            0 => h.endpoint = 9,
            1 => h.transfer_flags = u32::MAX,
            _ => h.setup[0] = 1,
        }
        let t = Transfer::new(&e.budget(), h, &[]).unwrap();
        assert!(e.submit(USER, id, t, END).is_err());
    }
    assert!(mock.0.borrow().submitted.is_empty());
    let b = Budget::new(Config::default().resources);
    assert!(
        e.submit(USER, id, Transfer::new(&b, header(1, 8), &[]).unwrap(), END)
            .is_err()
    );
    let mut h = header(1, 32);
    h.endpoint = 2;
    h.number_of_packets = 2;
    let layout = vec![
        IsoPacket {
            offset: 0,
            length: 8,
            actual_length: 0,
            status: 0,
        },
        IsoPacket {
            offset: 16,
            length: 8,
            actual_length: 0,
            status: 0,
        },
    ];
    let t = Transfer::new(&e.budget(), h, &layout).unwrap();
    e.submit(USER, id, t, END).unwrap();
    mock.0.borrow_mut().corrupt_iso = true;
    ready(&mock, id, 1);
    e.tick();
    assert!(matches!(
        e.next_transfer_event(USER, id).unwrap(),
        Some(TransferEvent::Failed {
            error: knightbus_core::Error::Invalid,
            ..
        })
    ));
}

#[test]
fn events_overflow_are_bounded_and_subscription_owners_are_checked() {
    let config = Config {
        events: 1,
        subscriptions: 1,
        admin_subscriptions: 0,
        ..Config::default()
    };
    let (mut e, _, _) = fixture(config);
    let baseline = e.budget().usage();
    let Reply::Subscription(sub) = e.execute(USER, Command::Subscribe, END).unwrap() else {
        panic!()
    };
    let subscription_bytes = e.budget().usage().bytes - baseline.bytes;
    assert_eq!(
        e.execute(USER, Command::Subscribe, END),
        Err(knightbus_core::Error::Exhausted)
    );
    grant(&mut e, KEY, PEER);
    let id = attach(&mut e, vec![KEY]);
    e.tick();
    assert!(matches!(
        e.execute(USER, Command::Poll(sub), END),
        Ok(Reply::Event(Some(Event {
            kind: EventKind::Resync,
            ..
        })))
    ));
    let stranger = Caller {
        peer: OTHER,
        administrator: false,
    };
    assert_eq!(e.execute(stranger, Command::Poll(sub), END), Err(error()));
    let before_unsubscribe = e.budget().usage().bytes;
    e.execute(USER, Command::Unsubscribe(sub), END).unwrap();
    assert_eq!(
        e.budget().usage().bytes,
        before_unsubscribe - subscription_bytes
    );
    e.execute(USER, Command::Detach(id), END).unwrap();
    e.tick();
    e.execute(USER, Command::Forget(id), END).unwrap();
    e.execute(
        ADMIN,
        Command::Authorize {
            device: KEY,
            peer: PEER,
            allow: false,
        },
        END,
    )
    .unwrap();
    assert_eq!(e.budget().usage(), baseline);
}

#[test]
fn ipc_request_ids_cancellation_deadlines_access_and_restart() {
    let (mut e, _, time) = fixture(Config::default());
    let mut service = Service::connect(&mut e, USER, true, 7, CONTROL_VERSION, 1).unwrap();
    let frame = |id, deadline| {
        encode_request(&Request {
            version: CONTROL_VERSION,
            id,
            deadline,
            command: Command::List,
        })
        .unwrap()
    };
    service.enqueue(7, &frame(1, END)).unwrap();
    assert_eq!(
        service.enqueue(7, &frame(2, END)),
        Err(knightbus_core::Error::Exhausted)
    );
    service.cancel(7, 1).unwrap();
    assert_eq!(
        service.response(7, 1).unwrap().unwrap().result,
        Err(knightbus_core::Error::Cancelled)
    );
    assert_eq!(
        service.enqueue(7, &frame(1, END)),
        Err(knightbus_core::Error::Invalid)
    );
    service.enqueue(7, &frame(2, 10)).unwrap();
    time.0.set(10);
    service.dispatch();
    assert_eq!(
        service.response(7, 2).unwrap().unwrap().result,
        Err(knightbus_core::Error::Deadline)
    );
    assert_eq!(
        service.enqueue(8, &frame(3, END)),
        Err(knightbus_core::Error::State)
    );
    service.close();
    assert_eq!(
        service.enqueue(7, &frame(3, END)),
        Err(knightbus_core::Error::State)
    );
    assert_eq!(
        service.enqueue(0, &frame(3, END)),
        Err(knightbus_core::Error::State)
    );
}

#[test]
fn ipc_framing_checks_all_truncations_tags_counts_and_trailing_bytes() {
    let commands = vec![
        Command::List,
        Command::Authorize {
            device: KEY,
            peer: PEER,
            allow: true,
        },
        Command::Attach(vec![KEY]),
        Command::Detach(SessionId {
            epoch: 7,
            serial: 1,
        }),
        Command::Status(SessionId {
            epoch: 7,
            serial: 1,
        }),
        Command::Heartbeat(SessionId {
            epoch: 7,
            serial: 1,
        }),
        Command::Subscribe,
        Command::Poll(1),
        Command::Unsubscribe(1),
        Command::Forget(SessionId {
            epoch: 7,
            serial: 1,
        }),
        Command::Shutdown,
    ];
    for command in commands {
        let r = Request {
            version: CONTROL_VERSION,
            id: u64::MAX,
            deadline: END,
            command,
        };
        let bytes = encode_request(&r).unwrap();
        assert_eq!(decode_request(&bytes), Ok(r));
        for n in 0..bytes.len() {
            assert!(decode_request(&bytes[..n]).is_err());
        }
        let mut extra = bytes.clone();
        extra.push(0);
        assert!(decode_request(&extra).is_err());
        let mut bad_tag = bytes;
        bad_tag[22] = 255;
        assert!(decode_request(&bad_tag).is_err());
    }
    assert!(decode_request(&vec![0; 8193]).is_err());
    let r = Request {
        version: CONTROL_VERSION,
        id: 1,
        deadline: END,
        command: Command::Attach(vec![KEY; 257]),
    };
    assert!(encode_request(&r).is_err());
}

#[test]
fn importer_state_transitions_cannot_revive_after_cancel_or_timeout() {
    let states = [
        ImporterState::Idle,
        ImporterState::Attaching,
        ImporterState::Active,
        ImporterState::Draining,
        ImporterState::Quarantined,
    ];
    for state in states {
        let mut s = state;
        assert_eq!(s.begin().is_ok(), state == ImporterState::Idle);
        let mut s = state;
        assert_eq!(s.attached().is_ok(), state == ImporterState::Attaching);
        let mut s = state;
        assert_eq!(
            s.retired().is_ok(),
            matches!(state, ImporterState::Draining | ImporterState::Quarantined)
        );
        let mut s = state;
        s.disconnect();
        s.disconnect();
        assert!(!matches!(
            s,
            ImporterState::Active | ImporterState::Attaching
        ));
        s.timeout();
        if s == ImporterState::Quarantined {
            assert!(s.attached().is_err());
            s.retired().unwrap();
            assert_eq!(s, ImporterState::Idle);
        }
    }
}

#[test]
fn topology_generation_and_capacity_validation() {
    let (mut e, _, _) = fixture(Config::default());
    let mut d = device(DeviceKey {
        id: DeviceId(2),
        generation: 1,
    });
    let duplicate = d.configurations[0].interfaces[0].endpoints[0].clone();
    d.configurations[0].interfaces[0].endpoints.push(duplicate);
    assert!(e.register(d).is_err());
    let mut d = device(DeviceKey {
        id: DeviceId(2),
        generation: 1,
    });
    d.wire.bus_id = KEY.id.bus_id();
    assert!(e.register(d).is_err());
    grant(&mut e, KEY, PEER);
    assert!(
        e.execute(USER, Command::Attach(vec![KEY, KEY]), END)
            .is_err()
    );
}

#[test]
fn fabricated_usbip_import_cannot_bypass_exporter_policy() {
    let (mut e, _, _) = fixture(Config::default());
    assert!(e.discover(USER).is_empty());
    assert_eq!(e.import(USER, &KEY.id.bus_id(), END), Err(error()));
    assert_eq!(e.import(USER, &DeviceId(99).bus_id(), END), Err(error()));
    grant(&mut e, KEY, PEER);
    let wire = Message::DevListReply {
        status: 0,
        devices: e.discover(USER),
    };
    let bytes = encode(&wire, Limits::default(), &mut |_| Ok(())).unwrap();
    assert_eq!(
        decode(&bytes, Context::Management, Limits::default(), &mut |_| Ok(
            ()
        ))
        .unwrap()
        .0,
        wire
    );
    let id = e.import(USER, &KEY.id.bus_id(), END).unwrap();
    assert_eq!(status(&mut e, id).state, SessionState::Acquiring);
    e.tick();
    assert_eq!(status(&mut e, id).state, SessionState::Active);
}

#[test]
fn response_frames_preserve_errors_sessions_events_and_wide_ids() {
    use knightbus_ipc::{decode_response, encode_response};
    let mut results = vec![
        Ok(Reply::Done),
        Ok(Reply::Devices(vec![DeviceInfo {
            shared: false,
            name: None,
            key: KEY,
            busy: true,
            vendor: 0x046d,
            product: 0xc215,
            speed: 2,
            class: 0,
        }])),
        Ok(Reply::Subscription(u64::MAX)),
        Ok(Reply::Event(None)),
    ];
    for state in [
        SessionState::Acquiring,
        SessionState::Active,
        SessionState::Draining,
        SessionState::Quarantined,
        SessionState::Closed,
    ] {
        results.push(Ok(Reply::Session(Session {
            id: SessionId {
                epoch: u64::MAX,
                serial: u64::MAX,
            },
            peer: PEER,
            devices: vec![KEY],
            state,
            error: Some(knightbus_core::Error::Cleanup),
        })));
    }
    for kind in [EventKind::Inventory, EventKind::Session, EventKind::Resync] {
        results.push(Ok(Reply::Event(Some(Event {
            sequence: u64::MAX,
            kind,
            session: Some(SessionId {
                epoch: 7,
                serial: 1,
            }),
        }))));
    }
    use knightbus_core::Error as E;
    for error in [
        E::Invalid,
        E::State,
        E::Unsupported,
        E::Unavailable,
        E::Denied,
        E::NotFound,
        E::Version,
        E::Deadline,
        E::Cancelled,
        E::Exhausted,
        E::Internal,
        E::Cleanup,
    ] {
        results.push(Err(error));
    }
    for result in results {
        let r = Response {
            id: u64::MAX,
            result,
        };
        let bytes = encode_response(&r).unwrap();
        assert_eq!(decode_response(&bytes), Ok(r));
        for len in 0..bytes.len() {
            assert!(decode_response(&bytes[..len]).is_err());
        }
        let mut extra = bytes;
        extra.push(0);
        assert!(decode_response(&extra).is_err());
    }
}

#[derive(Clone, Default)]
struct Virtual(Rc<Cell<bool>>, Rc<RefCell<Option<Transfer>>>);
impl ImporterBackend for Virtual {
    fn next_cancellation(&mut self, _: SessionId) -> Option<u32> {
        None
    }
    fn set_waker(&mut self, waker: Option<std::task::Waker>) {
        if let Some(waker) = waker {
            waker.wake();
        }
    }
    fn attach(&mut self, _: SessionId, _: &Device) -> Poll<knightbus_core::Result<()>> {
        Poll::Ready(Ok(()))
    }
    fn next_transfer(
        &mut self,
        _: SessionId,
        _: &Budget,
    ) -> Option<knightbus_core::Result<Transfer>> {
        self.1.borrow_mut().take().map(Ok)
    }
    fn complete(&mut self, _: SessionId, _: Completion) -> std::result::Result<(), Completion> {
        Ok(())
    }
    fn detach(&mut self, _: SessionId) -> Poll<knightbus_core::Result<()>> {
        if self.0.get() {
            Poll::Ready(Ok(()))
        } else {
            Poll::Pending
        }
    }
}
#[test]
fn importer_native_retirement_gates_reattachment() {
    let backend = Virtual::default();
    let time = Time::default();
    let mut importer = Importer::new(
        backend.clone(),
        time.clone(),
        5,
        Budget::new(Config::default().resources),
    )
    .unwrap();
    let id = SessionId {
        epoch: 7,
        serial: 1,
    };
    importer.attach(id, device(KEY), END).unwrap();
    importer.tick();
    assert_eq!(importer.state(), ImporterState::Active);
    importer.disconnect();
    time.0.set(6);
    importer.tick();
    assert_eq!(importer.state(), ImporterState::Quarantined);
    assert!(importer.attach(id, device(KEY), END).is_err());
    backend.0.set(true);
    importer.tick();
    assert_eq!(importer.state(), ImporterState::Idle);
    importer
        .attach(SessionId { serial: 2, ..id }, device(KEY), END)
        .unwrap();
    importer.disconnect();
    importer.tick();
    assert_eq!(importer.state(), ImporterState::Idle);
}

#[test]
fn importer_clock_regression_drains_without_permitting_reuse() {
    let backend = Virtual::default();
    let time = Time::default();
    let budget = Budget::new(Config::default().resources);
    let mut importer = Importer::new(backend.clone(), time.clone(), 5, budget.clone()).unwrap();
    let session = SessionId {
        epoch: 1,
        serial: 1,
    };
    importer.attach(session, device(KEY), END).unwrap();
    time.0.set(10);
    importer.tick();
    assert_eq!(importer.state(), ImporterState::Active);
    time.0.set(9);
    importer.tick();
    assert_eq!(importer.state(), ImporterState::Draining);
    assert_eq!(importer.error(), Some(knightbus_core::Error::State));
    assert!(budget.usage().bytes > 0);
    assert!(importer.next_transfer().is_none());
    backend.0.set(true);
    importer.tick();
    assert_eq!(importer.state(), ImporterState::Idle);
    assert_eq!(budget.usage(), Usage::default());
    time.0.set(11);
    assert_eq!(
        importer.attach(session, device(KEY), END),
        Err(knightbus_core::Error::State)
    );
}

#[test]
fn importer_rejects_foreign_transfer_credits_and_drains() {
    let backend = Virtual::default();
    let budget = Budget::new(Config::default().resources);
    let foreign = Budget::new(Config::default().resources);
    *backend.1.borrow_mut() = Some(Transfer::new(&foreign, header(1, 8), &[]).unwrap());
    let mut importer = Importer::new(backend.clone(), Time::default(), 5, budget.clone()).unwrap();
    importer
        .attach(
            SessionId {
                epoch: 1,
                serial: 1,
            },
            device(KEY),
            END,
        )
        .unwrap();
    importer.tick();
    assert!(matches!(
        importer.next_transfer(),
        Some(Err(knightbus_core::Error::Invalid))
    ));
    assert_eq!(importer.state(), ImporterState::Draining);
    assert_eq!(foreign.usage(), Usage::default());
    backend.0.set(true);
    importer.tick();
    assert_eq!(budget.usage(), Usage::default());
}

#[test]
fn acquisition_revocation_cleanup_errors_and_clock_regression_fail_closed() {
    let (mut e, mock, time) = fixture(Config::default());
    grant(&mut e, KEY, PEER);
    mock.0.borrow_mut().pending_claim = true;
    let id = attach(&mut e, vec![KEY]);
    e.tick();
    e.execute(
        ADMIN,
        Command::Authorize {
            device: KEY,
            peer: PEER,
            allow: false,
        },
        END,
    )
    .unwrap();
    mock.0.borrow_mut().fail_release = true;
    e.tick();
    assert_eq!(status(&mut e, id).state, SessionState::Quarantined);
    mock.0.borrow_mut().fail_release = false;
    e.tick();
    assert_eq!(status(&mut e, id).state, SessionState::Closed);
    mock.0.borrow_mut().pending_claim = false;
    grant(&mut e, KEY, PEER);
    let id = attach(&mut e, vec![KEY]);
    e.tick();
    mock.0.borrow_mut().fail_submit = true;
    let baseline = e.budget().usage();
    assert_eq!(
        e.submit(USER, id, transfer(&e, 1, 64), END),
        Err(knightbus_core::Error::Unavailable)
    );
    assert_eq!(e.budget().usage(), baseline);
    time.0.set(5);
    e.tick();
    time.0.set(4);
    e.tick();
    assert!(e.shutdown_complete());
}

#[test]
fn stalled_unlink_quarantines_session_without_releasing_native_memory() {
    let (mut e, mock, time) = fixture(Config {
        cleanup_ms: 5,
        ..Config::default()
    });
    let id = active(&mut e);
    e.submit(USER, id, transfer(&e, 1, 128), END).unwrap();
    e.unlink(USER, id, 2, 1).unwrap();
    time.0.set(6);
    e.tick();
    assert_eq!(status(&mut e, id).state, SessionState::Quarantined);
    assert_eq!(e.budget().usage().requests, 1);
    ready(&mock, id, 1);
    e.tick();
    assert_eq!(status(&mut e, id).state, SessionState::Closed);
    assert_eq!(e.budget().usage().requests, 0);
}

#[test]
fn host_drop_retires_events_and_does_not_release_engine_owned_leases() {
    for ipc in [false, true] {
        let (mut e, _, _) = fixture(Config::default());
        let id = active(&mut e);
        let baseline = e.budget().usage();
        {
            let mut c: Box<dyn KnightBusControl + '_> = if ipc {
                Box::new(IpcControl::new(&mut e, USER, true, 7, CONTROL_VERSION).unwrap())
            } else {
                Box::new(ControlConnection::new(&mut e, USER, CONTROL_VERSION).unwrap())
            };
            c.call(Command::Subscribe, END).unwrap();
        }
        assert_eq!(e.budget().usage(), baseline);
        assert_eq!(status(&mut e, id).state, SessionState::Active);
        e.disconnect(id).unwrap();
        e.tick();
        assert_eq!(status(&mut e, id).state, SessionState::Closed);
    }
}

#[test]
fn all_exporter_states_reject_illegal_ingress_and_accept_idempotent_detach() {
    for wanted in [
        SessionState::Acquiring,
        SessionState::Active,
        SessionState::Draining,
        SessionState::Quarantined,
        SessionState::Closed,
    ] {
        let (mut e, mock, time) = fixture(Config {
            cleanup_ms: 5,
            ..Config::default()
        });
        grant(&mut e, KEY, PEER);
        let id = attach(&mut e, vec![KEY]);
        if wanted != SessionState::Acquiring {
            e.tick();
        }
        if matches!(
            wanted,
            SessionState::Draining | SessionState::Quarantined | SessionState::Closed
        ) {
            mock.0.borrow_mut().pending_release = wanted != SessionState::Closed;
            e.execute(USER, Command::Detach(id), END).unwrap();
        }
        if wanted == SessionState::Quarantined {
            time.0.set(6);
            e.tick();
        }
        if wanted == SessionState::Closed {
            e.tick();
        }
        assert_eq!(status(&mut e, id).state, wanted);
        let can_submit = e.submit(USER, id, transfer(&e, 1, 0), END).is_ok();
        assert_eq!(can_submit, wanted == SessionState::Active);
        assert_eq!(
            e.execute(USER, Command::Heartbeat(id), END).is_ok(),
            wanted == SessionState::Active
        );
        assert_eq!(
            e.execute(USER, Command::Forget(id), END).is_ok(),
            wanted == SessionState::Closed
        );
        if wanted != SessionState::Closed {
            e.execute(USER, Command::Detach(id), END).unwrap();
            e.execute(USER, Command::Detach(id), END).unwrap();
        }
    }
}

#[test]
fn charged_transport_storage_moves_into_native_request_without_a_payload_copy() {
    let budget = Budget::new(ResourceLimits {
        bytes: 4096,
        requests: 1,
    });
    let mut buffer = budget.buffer(1024).unwrap();
    buffer.as_mut_slice()[0] = 42;
    let pointer = buffer.as_slice().as_ptr();
    let t = Transfer::from_buffer(&budget, header(1, 1024), &[], buffer).unwrap();
    assert_eq!(t.buffer().as_ptr(), pointer);
    assert_eq!(t.buffer()[0], 42);
    assert_eq!(budget.usage().requests, 1);
    assert!(budget.usage().bytes >= 1024);
    drop(t);
    assert_eq!(budget.usage(), Usage::default());
}

#[test]
fn retired_sessions_are_reclaimed_without_reusing_tokens_or_evicting_live_claims() {
    let (mut e, mock, time) = fixture(Config {
        sessions: 2,
        cleanup_ms: 5,
        ..Config::default()
    });
    let second = DeviceKey {
        id: DeviceId(2),
        generation: 1,
    };
    e.register(device(second)).unwrap();
    grant(&mut e, second, OTHER);
    grant(&mut e, KEY, PEER);
    let mut first = None;
    for _ in 0..80 {
        let id = attach(&mut e, vec![KEY]);
        first.get_or_insert(id);
        e.execute(USER, Command::Detach(id), END).unwrap();
        e.tick();
    }
    assert_eq!(
        e.execute(USER, Command::Status(first.unwrap()), END),
        Err(error())
    );
    let other = Caller {
        peer: OTHER,
        administrator: false,
    };
    let Reply::Session(neighbor) = e
        .execute(other, Command::Attach(vec![second]), END)
        .unwrap()
    else {
        panic!()
    };
    e.tick();
    let id = attach(&mut e, vec![KEY]);
    e.tick();
    mock.0.borrow_mut().pending_release = true;
    e.disconnect(id).unwrap();
    time.0.set(6);
    e.tick();
    assert_eq!(status(&mut e, id).state, SessionState::Quarantined);
    assert!(matches!(
        e.execute(other, Command::Status(neighbor.id), END),
        Ok(Reply::Session(Session {
            state: SessionState::Active,
            ..
        }))
    ));
    assert_eq!(
        e.execute(USER, Command::Attach(vec![KEY]), END),
        Err(knightbus_core::Error::State)
    );
    // Byte pressure also reclaims a fully closed record before rejecting admission.
    mock.0.borrow_mut().pending_release = false;
    e.tick();
    let remaining = Config::default().resources.bytes - e.budget().usage().bytes;
    let storage = e.budget().buffer(remaining).unwrap();
    let renewed = attach(&mut e, vec![KEY]);
    assert_ne!(renewed, id);
    assert_eq!(e.execute(USER, Command::Status(id), END), Err(error()));
    drop(storage);
}

#[derive(Clone)]
struct SharedTarget(Rc<RefCell<TestEngine>>);
impl ControlTarget for SharedTarget {
    fn now(&self) -> u64 {
        self.0.borrow().now()
    }
    fn execute(
        &mut self,
        caller: Caller,
        command: Command,
        deadline: u64,
    ) -> knightbus_core::Result<Reply> {
        self.0.borrow_mut().execute(caller, command, deadline)
    }
}
#[test]
fn subscription_quotas_span_connections_and_admin_queue_survives_full_data_budget() {
    for ipc in [false, true] {
        let config = Config {
            subscriptions: 3,
            subscriptions_per_peer: 1,
            admin_subscriptions: 1,
            ..Config::default()
        };
        let (e, _, _) = fixture(config);
        let shared = Rc::new(RefCell::new(e));
        let baseline = shared.borrow().budget().usage();
        let mut a = SharedTarget(shared.clone());
        let mut b = a.clone();
        let mut c = a.clone();
        let mut d = a.clone();
        let peer_b = Caller {
            peer: OTHER,
            administrator: false,
        };
        let admin = Caller {
            peer: PEER,
            administrator: true,
        };
        let adapter = |target, caller| -> Box<dyn KnightBusControl + '_> {
            if ipc {
                Box::new(IpcControl::new(target, caller, true, 7, CONTROL_VERSION).unwrap())
            } else {
                Box::new(ControlConnection::new(target, caller, CONTROL_VERSION).unwrap())
            }
        };
        let mut first = adapter(&mut a, USER);
        let mut duplicate = adapter(&mut b, USER);
        let mut neighbor = adapter(&mut c, peer_b);
        let mut administrator = adapter(&mut d, admin);
        first.call(Command::Subscribe, END).unwrap();
        assert_eq!(
            duplicate.call(Command::Subscribe, END),
            Err(knightbus_core::Error::Exhausted)
        );
        neighbor.call(Command::Subscribe, END).unwrap();
        let budget = shared.borrow().budget();
        let storage = budget
            .buffer(config.resources.bytes - budget.usage().bytes)
            .unwrap();
        let Reply::Subscription(id) = administrator.call(Command::Subscribe, END).unwrap() else {
            panic!()
        };
        assert_eq!(budget.usage().bytes, config.resources.bytes);
        administrator.call(Command::Unsubscribe(id), END).unwrap();
        administrator.call(Command::Subscribe, END).unwrap(); // prepaid storage is reusable
        drop((first, duplicate, neighbor, administrator, storage));
        assert_eq!(budget.usage(), baseline);
    }
}

#[test]
fn event_sequences_and_inventory_notifications_do_not_reveal_hidden_peers() {
    let (mut e, _, _) = fixture(Config {
        events: 1,
        ..Config::default()
    });
    let Reply::Subscription(sub) = e.execute(USER, Command::Subscribe, END).unwrap() else {
        panic!()
    };
    let second = DeviceKey {
        id: DeviceId(2),
        generation: 1,
    };
    e.register(device(second)).unwrap();
    grant(&mut e, second, OTHER);
    let other = Caller {
        peer: OTHER,
        administrator: false,
    };
    let Reply::Session(hidden) = e
        .execute(other, Command::Attach(vec![second]), END)
        .unwrap()
    else {
        panic!()
    };
    e.tick();
    e.execute(other, Command::Detach(hidden.id), END).unwrap();
    e.tick();
    assert_eq!(
        e.execute(USER, Command::Poll(sub), END),
        Ok(Reply::Event(None))
    );
    grant(&mut e, KEY, PEER);
    assert_eq!(
        e.execute(USER, Command::Poll(sub), END),
        Ok(Reply::Event(Some(Event {
            sequence: 1,
            kind: EventKind::Inventory,
            session: None
        })))
    );
    let Reply::Subscription(late) = e.execute(USER, Command::Subscribe, END).unwrap() else {
        panic!()
    };
    e.remove(second).unwrap();
    e.execute(
        ADMIN,
        Command::Authorize {
            device: KEY,
            peer: PEER,
            allow: false,
        },
        END,
    )
    .unwrap();
    assert_eq!(
        e.execute(USER, Command::Poll(sub), END),
        Ok(Reply::Event(Some(Event {
            sequence: 2,
            kind: EventKind::Inventory,
            session: None
        })))
    );
    assert_eq!(
        e.execute(USER, Command::Poll(late), END),
        Ok(Reply::Event(Some(Event {
            sequence: 1,
            kind: EventKind::Inventory,
            session: None
        })))
    );
    // Only this subscriber's visible events contribute to overflow/resync IDs.
    grant(&mut e, KEY, PEER);
    e.remove(KEY).unwrap();
    assert_eq!(
        e.execute(USER, Command::Poll(sub), END),
        Ok(Reply::Event(Some(Event {
            sequence: 4,
            kind: EventKind::Resync,
            session: None
        })))
    );
}

#[test]
fn interleaved_completion_slots_reuse_capacity_and_cleanup_only_their_session() {
    let (mut e, mock, _) = fixture(Config {
        resources: ResourceLimits {
            bytes: 1024 * 1024,
            requests: 8,
        },
        ..Config::default()
    });
    let a = active(&mut e);
    let key_b = DeviceKey {
        id: DeviceId(2),
        generation: 1,
    };
    e.register(device(key_b)).unwrap();
    grant(&mut e, key_b, PEER);
    let b = attach(&mut e, vec![key_b]);
    e.tick();
    let baseline = e.budget().usage();
    for i in 1..=4 {
        e.submit(USER, a, transfer(&e, i, 64), END).unwrap();
        let mut h = header(i, 64);
        h.device_id = 0x10002;
        e.submit(USER, b, Transfer::new(&e.budget(), h, &[]).unwrap(), END)
            .unwrap();
        ready(&mock, b, i);
        ready(&mock, a, i);
    }
    e.tick();
    assert_eq!(
        e.submit(USER, a, transfer(&e, 5, 64), END),
        Err(knightbus_core::Error::Exhausted)
    );
    for i in 1..=4 {
        let Some(TransferEvent::Complete(done)) = e.next_transfer_event(USER, a).unwrap() else {
            panic!()
        };
        assert_eq!(done.reply_context().seqnum, i);
    }
    // Reuse freed slots while another session still has queued completions.
    e.submit(USER, a, transfer(&e, 5, 64), END).unwrap();
    e.execute(USER, Command::Detach(b), END).unwrap();
    e.tick();
    assert!(!mock.0.borrow().cancelled.contains(&TransferId {
        session: a,
        sequence: 5
    }));
    ready(&mock, a, 5);
    e.tick();
    drop(e.next_transfer_event(USER, a).unwrap());
    assert_eq!(e.budget().usage(), baseline);
    assert_eq!(status(&mut e, b).state, SessionState::Closed);
}

#[test]
fn admitted_wire_storage_moves_to_native_and_stays_charged_through_completion() {
    let mock = Mock::default();
    let mut e = Engine::new(Time::default(), Config::default(), 7, |_| Ok(mock.clone())).unwrap();
    let mut d = device(KEY);
    let mut out = d.configurations[0].interfaces[0].endpoints[0].clone();
    out.address = 1;
    d.configurations[0].interfaces[0].endpoints.push(out);
    e.register(d).unwrap();
    let id = active(&mut e);
    let budget = e.budget();
    let baseline = budget.usage();
    let mut h = header(1, 4096);
    h.direction = Direction::Out;
    let request = Message::Submit(h.with_payload(vec![42; 4096], vec![]));
    let wire = encode(&request, Limits::default(), &mut |_| Ok(())).unwrap();
    let mut decoder = budget
        .decoder(Context::Requests, Limits::default())
        .unwrap();
    let (used, decoded) = decoder.push(&wire).unwrap();
    assert_eq!(used, wire.len());
    let decoded = decoded.unwrap();
    let Message::Submit(s) = decoded.message() else {
        panic!()
    };
    let pointer = s.data.as_ptr();
    let charge = budget.usage().bytes;
    drop(decoder);
    assert_eq!(budget.usage().bytes, charge);
    let native = decoded.into_transfer().unwrap();
    assert_eq!(native.buffer().as_ptr(), pointer);
    assert_eq!(native.buffer(), &[42; 4096]);
    assert_eq!(budget.usage().requests, 1);
    assert!(budget.usage().bytes >= 4096);
    e.submit(USER, id, native, END).unwrap();
    ready(&mock, id, 1);
    e.tick();
    assert_eq!(budget.usage().requests, 0);
    let Some(TransferEvent::Complete(completion)) = e.next_transfer_event(USER, id).unwrap() else {
        panic!()
    };
    assert_eq!(completion.transfer.buffer().as_ptr(), pointer);
    assert!(budget.usage().bytes >= baseline.bytes + 4096);
    drop(completion);
    assert_eq!(budget.usage(), baseline);
    let encoded = budget.encode(&request, Limits::default()).unwrap();
    assert_eq!(encoded.as_slice(), wire);
    assert_eq!(budget.usage().bytes, baseline.bytes + encoded.capacity());
    drop(encoded);
    assert_eq!(budget.usage(), baseline);
}

#[test]
fn admitted_decoder_rejects_large_bodies_and_releases_truncation_and_conversion_failures() {
    let budget = Budget::new(ResourceLimits {
        bytes: 4096,
        requests: 0,
    });
    let mut h = header(1, 8192);
    h.direction = Direction::Out;
    let wire = encode(
        &Message::Submit(h.with_payload(vec![0; 8192], vec![])),
        Limits::default(),
        &mut |_| Ok(()),
    )
    .unwrap();
    let mut decoder = budget
        .decoder(Context::Requests, Limits::default())
        .unwrap();
    assert!(matches!(
        decoder.push(&wire[..49]),
        Err(knightbus_core::usbip::Error::Limit("aggregate bytes"))
    ));
    assert!(budget.usage().bytes < 4096);
    drop(decoder);
    assert_eq!(budget.usage(), Usage::default());
    let mut decoder = budget
        .decoder(Context::Requests, Limits::default())
        .unwrap();
    decoder.push(&wire[..20]).unwrap();
    assert!(decoder.finish().is_err());
    drop(decoder);
    assert_eq!(budget.usage(), Usage::default());
    let wire = encode(
        &Message::Submit(header(1, 8192).with_payload(vec![], vec![])),
        Limits::default(),
        &mut |_| Ok(()),
    )
    .unwrap();
    let mut decoder = budget
        .decoder(Context::Requests, Limits::default())
        .unwrap();
    let (_, decoded) = decoder.push(&wire).unwrap();
    assert!(decoded.unwrap().into_transfer().is_err()); // request admission precedes native IN allocation
    drop(decoder);
    assert_eq!(budget.usage(), Usage::default());
}

#[test]
fn heartbeat_renews_lease_without_changing_independent_transfer_deadlines() {
    let (mut e, mock, time) = fixture(Config {
        lease_ms: 20,
        ..Config::default()
    });
    let id = active(&mut e);
    e.submit(USER, id, transfer(&e, 1, 8), 100).unwrap();
    e.submit(USER, id, transfer(&e, 2, 8), 15).unwrap();
    time.0.set(10);
    e.execute(USER, Command::Heartbeat(id), END).unwrap();
    time.0.set(20);
    e.tick();
    assert_eq!(status(&mut e, id).state, SessionState::Active);
    assert!(!mock.0.borrow().cancelled.contains(&TransferId {
        session: id,
        sequence: 1
    }));
    assert!(mock.0.borrow().cancelled.contains(&TransferId {
        session: id,
        sequence: 2
    }));
    // The transfer deadline still applies while the current lease is alive.
    time.0.set(25);
    e.execute(USER, Command::Heartbeat(id), END).unwrap();
    ready(&mock, id, 2);
    e.tick();
    assert!(matches!(
        e.next_transfer_event(USER, id).unwrap(),
        Some(TransferEvent::Failed {
            sequence: 2,
            error: knightbus_core::Error::Deadline
        })
    ));
    // Once the renewed lease actually expires, it cancels the longer transfer.
    time.0.set(45);
    e.tick();
    assert!(mock.0.borrow().cancelled.contains(&TransferId {
        session: id,
        sequence: 1
    }));
    assert_eq!(status(&mut e, id).state, SessionState::Draining);
    ready(&mock, id, 1);
    e.tick();
    assert_eq!(status(&mut e, id).state, SessionState::Closed);
}

#[test]
fn release_failure_notifies_quarantine_once_and_later_notifies_retirement() {
    let (mut e, mock, time) = fixture(Config {
        cleanup_ms: 100,
        ..Config::default()
    });
    let id = active(&mut e);
    let Reply::Subscription(sub) = e.execute(USER, Command::Subscribe, END).unwrap() else {
        panic!()
    };
    let event = |sequence| {
        Reply::Event(Some(Event {
            sequence,
            kind: EventKind::Session,
            session: Some(id),
        }))
    };
    e.execute(USER, Command::Detach(id), END).unwrap();
    assert_eq!(e.execute(USER, Command::Poll(sub), END), Ok(event(1)));
    mock.0.borrow_mut().fail_release = true;
    e.tick();
    assert_eq!(status(&mut e, id).state, SessionState::Quarantined);
    assert_eq!(e.execute(USER, Command::Poll(sub), END), Ok(event(2)));
    e.tick();
    time.0.set(100);
    e.tick();
    assert_eq!(
        e.execute(USER, Command::Poll(sub), END),
        Ok(Reply::Event(None))
    );
    mock.0.borrow_mut().fail_release = false;
    e.tick();
    assert_eq!(status(&mut e, id).state, SessionState::Closed);
    assert_eq!(e.execute(USER, Command::Poll(sub), END), Ok(event(3)));
}

#[test]
fn engine_validates_configuration_before_constructing_native_backend() {
    let called = std::cell::Cell::new(false);
    let result = Engine::new(Time::default(), Config::default(), 0, |_| {
        called.set(true);
        Ok(Mock::default())
    });
    assert!(matches!(result, Err(knightbus_core::Error::Invalid)));
    assert!(!called.get());
}

#[test]
fn revoking_a_peer_clears_all_grants_and_drains_active_and_partial_claims() {
    for partial in [false, true] {
        let (mut engine, native, _) = fixture(Config::default());
        grant(&mut engine, KEY, PEER);
        grant(&mut engine, KEY, OTHER);
        assert_eq!(
            engine.execute(USER, Command::Permissions(KEY), END),
            Err(knightbus_core::Error::Denied)
        );
        assert_eq!(
            engine.execute(ADMIN, Command::Permissions(KEY), END),
            Ok(Reply::Peers(vec![PEER, OTHER]))
        );
        native.0.borrow_mut().pending_claim = partial;
        let session = attach(&mut engine, vec![KEY]);
        engine.tick();
        engine.revoke_peer(PEER);
        engine.revoke_peer(PEER);
        native.0.borrow_mut().pending_claim = false;
        engine.tick();
        assert_eq!(status(&mut engine, session).state, SessionState::Closed);
        assert!(engine.discover(USER).is_empty());
        assert_eq!(
            engine.execute(ADMIN, Command::Permissions(KEY), END),
            Ok(Reply::Peers(vec![OTHER]))
        );
        assert!(
            !engine
                .discover(Caller {
                    peer: OTHER,
                    administrator: false
                })
                .is_empty()
        );
        let request = Request {
            version: CONTROL_VERSION,
            id: 1,
            deadline: END,
            command: Command::Permissions(KEY),
        };
        assert_eq!(
            decode_request(&encode_request(&request).unwrap()).unwrap(),
            request
        );
        let response = Response {
            id: 1,
            result: Ok(Reply::Peers(vec![OTHER])),
        };
        assert_eq!(
            knightbus_ipc::decode_response(&knightbus_ipc::encode_response(&response).unwrap())
                .unwrap(),
            response
        );
    }
}

#[test]
fn every_admitted_permission_list_fits_the_control_protocol() {
    let (mut engine, _, _) = fixture(Config::default());
    let peer = |index: usize| {
        let mut bytes = [0; 32];
        bytes[..8].copy_from_slice(&(index as u64).to_be_bytes());
        PeerId(bytes)
    };
    for index in 0..MAX_DEVICE_GRANTS {
        grant(&mut engine, KEY, peer(index));
    }
    grant(&mut engine, KEY, peer(0)); // Idempotent even at capacity.
    assert_eq!(
        engine.execute(
            ADMIN,
            Command::Authorize {
                device: KEY,
                peer: peer(MAX_DEVICE_GRANTS),
                allow: true
            },
            END
        ),
        Err(knightbus_core::Error::Exhausted)
    );
    let reply = engine
        .execute(ADMIN, Command::Permissions(KEY), END)
        .unwrap();
    let response = Response {
        id: 1,
        result: Ok(reply),
    };
    let bytes = knightbus_ipc::encode_response(&response).unwrap();
    assert_eq!(knightbus_ipc::decode_response(&bytes).unwrap(), response);
    engine
        .execute(
            ADMIN,
            Command::Authorize {
                device: KEY,
                peer: peer(0),
                allow: false,
            },
            END,
        )
        .unwrap();
    grant(&mut engine, KEY, peer(MAX_DEVICE_GRANTS));
}

#[test]
fn unshare_notifies_only_administrators_and_former_grantees() {
    let (mut e, _, _) = fixture(Config::default());
    let former = Caller {
        peer: OTHER,
        administrator: false,
    };
    grant(&mut e, KEY, OTHER);
    let subscribe = |e: &mut Engine<Mock, Time>, caller| {
        let Reply::Subscription(id) = e.execute(caller, Command::Subscribe, END).unwrap() else {
            panic!()
        };
        id
    };
    let hidden = subscribe(&mut e, USER);
    let affected = subscribe(&mut e, former);
    let admin = subscribe(&mut e, ADMIN);
    assert_eq!(
        e.execute(USER, Command::List, END),
        Ok(Reply::Devices(vec![]))
    );
    e.execute(ADMIN, Command::Unshare(KEY), END).unwrap();
    for (caller, subscription) in [(ADMIN, admin), (former, affected)] {
        assert_eq!(
            e.execute(caller, Command::Poll(subscription), END),
            Ok(Reply::Event(Some(Event {
                sequence: 1,
                kind: EventKind::Inventory,
                session: None,
            })))
        );
    }
    assert_eq!(
        e.execute(USER, Command::Poll(hidden), END),
        Ok(Reply::Event(None))
    );
    // Hidden activity must not leave a gap in this peer's sequence.
    grant(&mut e, KEY, PEER);
    assert_eq!(
        e.execute(USER, Command::Poll(hidden), END),
        Ok(Reply::Event(Some(Event {
            sequence: 1,
            kind: EventKind::Inventory,
            session: None,
        })))
    );
    e.execute(ADMIN, Command::Unshare(KEY), END).unwrap();
    assert_eq!(
        e.execute(former, Command::Poll(affected), END),
        Ok(Reply::Event(None))
    );
}

#[test]
fn sharing_state_tracks_grants_and_can_be_enabled_again() {
    let (mut engine, _, _) = fixture(Config::default());
    let shared = |engine: &mut TestEngine| {
        let Reply::Devices(devices) = engine.execute(ADMIN, Command::List, END).unwrap() else {
            panic!()
        };
        devices.iter().find(|d| d.key == KEY).unwrap().shared
    };
    assert!(!shared(&mut engine));
    grant(&mut engine, KEY, PEER);
    assert!(shared(&mut engine));
    engine.execute(ADMIN, Command::Unshare(KEY), END).unwrap();
    assert!(!shared(&mut engine));
    grant(&mut engine, KEY, PEER);
    assert!(shared(&mut engine));
}
