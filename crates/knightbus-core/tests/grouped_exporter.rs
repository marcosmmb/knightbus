//! Adding and unsharing devices must not replace another device's native owner.
use knightbus_core::*;
use std::{
    sync::{Arc, Mutex},
    task::{Poll, Waker},
};

const ADMIN: Caller = Caller {
    peer: PeerId([1; PEER_ID_BYTES]),
    administrator: true,
};
const PEER: Caller = Caller {
    administrator: false,
    ..ADMIN
};
struct Time;
impl Clock for Time {
    fn now(&self) -> u64 {
        0
    }
}
#[derive(Default)]
struct State {
    submitted: Vec<TransferId>,
    cancelled: Vec<TransferId>,
    pending: std::collections::VecDeque<(TransferId, Transfer)>,
    claims: Vec<DeviceKey>,
    releases: Vec<DeviceKey>,
    wakes: usize,
}
struct Native(Arc<Mutex<State>>);
impl ExporterBackend for Native {
    fn set_waker(&mut self, _: Option<Waker>) {
        self.0.lock().unwrap().wakes += 1;
    }
    fn manages_topology(&self) -> bool {
        true
    }
    fn poll_removed(&mut self) -> Option<DeviceKey> {
        None
    }
    fn claim(&mut self, _: SessionId, device: &Device) -> Poll<Result<()>> {
        self.0.lock().unwrap().claims.push(device.key);
        Poll::Ready(Ok(()))
    }
    fn submit(
        &mut self,
        id: TransferId,
        transfer: Transfer,
    ) -> std::result::Result<(), (Error, Transfer)> {
        let mut state = self.0.lock().unwrap();
        state.submitted.push(id);
        state.pending.push_back((id, transfer));
        Ok(())
    }
    fn cancel(&mut self, id: TransferId) {
        self.0.lock().unwrap().cancelled.push(id);
    }
    fn poll_completion(&mut self) -> Option<(TransferId, Completion)> {
        self.0
            .lock()
            .unwrap()
            .pending
            .pop_front()
            .map(|(id, transfer)| {
                (
                    id,
                    Completion {
                        transfer,
                        status: 0,
                        actual_length: 0,
                        start_frame: 0,
                    },
                )
            })
    }
    fn release(&mut self, _: SessionId, key: DeviceKey) -> Poll<Result<()>> {
        self.0.lock().unwrap().releases.push(key);
        Poll::Ready(Ok(()))
    }
}
fn device(id: u64) -> Device {
    let key = DeviceKey {
        id: DeviceId(id),
        generation: 1,
    };
    Device {
        key,
        wire: usbip::Device {
            path: usbip::WireString::new(b"/test").unwrap(),
            bus_id: key.id.bus_id(),
            bus_number: 1,
            device_number: id as u32,
            speed: 3,
            vendor_id: 1,
            product_id: 2,
            device_bcd: 0x200,
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
                class: usbip::Interface {
                    class: 0xff,
                    subclass: 0,
                    protocol: 0,
                },
                endpoints: vec![],
            }],
        }],
    }
}
fn attach(engine: &mut Engine<exporter::Group, Time>, key: DeviceKey) -> SessionId {
    engine
        .execute(
            ADMIN,
            Command::Authorize {
                device: key,
                peer: PEER.peer,
                allow: true,
            },
            u64::MAX,
        )
        .unwrap();
    let Reply::Session(session) = engine
        .execute(PEER, Command::Attach(vec![key]), u64::MAX)
        .unwrap()
    else {
        panic!()
    };
    engine.tick();
    session.id
}
#[test]
fn adding_and_unsharing_a_second_device_preserves_the_first_native_session() {
    let mut engine = Engine::new(
        Time,
        Config {
            devices: 2,
            ..Config::default()
        },
        1,
        |_| Ok(exporter::Group::default()),
    )
    .unwrap();
    let budget = engine.budget();
    let first = device(1);
    let second = device(2);
    let a = Arc::new(Mutex::new(State::default()));
    let b = Arc::new(Mutex::new(State::default()));
    engine
        .register_backend(first.clone(), Some("Headset".into()), |group| {
            group.insert(&first, Box::new(Native(a.clone())), &budget)
        })
        .unwrap();
    let active = attach(&mut engine, first.key);
    engine
        .register_backend(second.clone(), Some("Webcam".into()), |group| {
            group.insert(&second, Box::new(Native(b.clone())), &budget)
        })
        .unwrap();
    assert!(a.lock().unwrap().releases.is_empty());
    assert!(matches!(
        engine
            .execute(PEER, Command::Status(active), u64::MAX)
            .unwrap(),
        Reply::Session(Session {
            state: SessionState::Active,
            ..
        })
    ));
    let Reply::Devices(visible) = engine.execute(PEER, Command::List, u64::MAX).unwrap() else {
        panic!()
    };
    assert_eq!(visible.len(), 1);
    assert_eq!(visible[0].name.as_deref(), Some("Headset"));
    let other = attach(&mut engine, second.key);
    engine
        .execute(ADMIN, Command::Unshare(second.key), u64::MAX)
        .unwrap();
    engine.tick();
    assert_eq!(b.lock().unwrap().releases, vec![second.key]);
    assert!(a.lock().unwrap().releases.is_empty());
    assert!(matches!(
        engine
            .execute(PEER, Command::Status(other), u64::MAX)
            .unwrap(),
        Reply::Session(Session {
            state: SessionState::Closed,
            ..
        })
    ));
    // A failed third registration cannot disturb either existing generation.
    let third = device(3);
    assert_eq!(
        engine.register_backend(third, None, |_| panic!(
            "capacity checked before installation"
        )),
        Err(Error::Exhausted)
    );
    engine
        .execute(ADMIN, Command::Unshare(first.key), u64::MAX)
        .unwrap();
    engine.tick();
    assert_eq!(a.lock().unwrap().releases, vec![first.key]);
    assert_eq!(a.lock().unwrap().claims.len(), 1);
}

#[test]
fn transfers_cancellation_and_release_are_scoped_and_completions_are_fair() {
    let mut group = exporter::Group::default();
    let budget = Budget::new(Config::default().resources);
    let first = device(1);
    let second = device(2);
    let a = Arc::new(Mutex::new(State::default()));
    let b = Arc::new(Mutex::new(State::default()));
    group
        .insert(&first, Box::new(Native(a.clone())), &budget)
        .unwrap();
    group
        .insert(&second, Box::new(Native(b.clone())), &budget)
        .unwrap();
    let one = SessionId {
        epoch: 1,
        serial: 1,
    };
    let two = SessionId {
        epoch: 1,
        serial: 2,
    };
    assert!(matches!(group.claim(one, &first), Poll::Ready(Ok(()))));
    assert!(matches!(group.claim(two, &second), Poll::Ready(Ok(()))));
    for (owner, wire, sequence) in [(one, 0x10001, 1), (one, 0x10001, 2), (two, 0x10002, 1)] {
        let transfer = Transfer::new(
            &budget,
            usbip::SubmitHeader {
                seqnum: sequence,
                device_id: wire,
                direction: usbip::Direction::In,
                endpoint: 1,
                transfer_flags: 0,
                transfer_buffer_length: 1,
                start_frame: 0,
                number_of_packets: 0,
                interval: 0,
                setup: [0; 8],
            },
            &[],
        )
        .unwrap();
        assert!(
            group
                .submit(
                    TransferId {
                        session: owner,
                        sequence
                    },
                    transfer
                )
                .is_ok()
        );
    }
    assert_eq!(a.lock().unwrap().submitted.len(), 2);
    assert_eq!(b.lock().unwrap().submitted.len(), 1);
    let cancel = TransferId {
        session: one,
        sequence: 1,
    };
    group.cancel(cancel);
    assert_eq!(a.lock().unwrap().cancelled, vec![cancel]);
    assert!(b.lock().unwrap().cancelled.is_empty());
    assert_eq!(group.poll_completion().unwrap().0.session, one);
    assert_eq!(group.poll_completion().unwrap().0.session, two);
    assert_eq!(group.poll_completion().unwrap().0.session, one);
    let mut replacement = first.clone();
    replacement.key.generation += 1;
    assert_eq!(
        group.insert(&replacement, Box::new(Native(a.clone())), &budget),
        Err(Error::State)
    );
    assert!(matches!(
        group.release(one, second.key),
        Poll::Ready(Err(Error::State))
    ));
    assert!(b.lock().unwrap().releases.is_empty());
    assert!(matches!(group.release(one, first.key), Poll::Ready(Ok(()))));
    assert!(matches!(
        group.release(two, second.key),
        Poll::Ready(Ok(()))
    ));
    drop(group);
    assert_eq!(budget.usage().bytes, 0);
}
