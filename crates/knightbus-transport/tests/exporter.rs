//! Real TCP framing/policy/cancellation against an injected backend, without USB.
use knightbus_core::{
    usbip::{self, Context, Direction, Limits, Message, SubmitHeader},
    *,
};
use knightbus_transport::exporter::{self as transport, Mono};
use std::{
    collections::VecDeque,
    io::Write,
    net::{TcpListener, TcpStream},
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    task::Poll,
    time::Duration,
};
const CALLER: Caller = Caller {
    peer: PeerId([1; 32]),
    administrator: false,
};
const KEY: DeviceKey = DeviceKey {
    id: DeviceId(1),
    generation: 1,
};
struct Mock {
    pending: VecDeque<(TransferId, Transfer)>,
    cancelled: Option<TransferId>,
    claims: Arc<AtomicUsize>,
}
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
    fn claim(&mut self, _: SessionId, _: &Device) -> Poll<Result<()>> {
        self.claims.fetch_add(1, Ordering::SeqCst);
        Poll::Ready(Ok(()))
    }
    fn release(&mut self, _: SessionId, _: DeviceKey) -> Poll<Result<()>> {
        self.claims.fetch_sub(1, Ordering::SeqCst);
        Poll::Ready(Ok(()))
    }
    fn submit(
        &mut self,
        id: TransferId,
        t: Transfer,
    ) -> std::result::Result<(), (Error, Transfer)> {
        self.pending.push_back((id, t));
        Ok(())
    }
    fn cancel(&mut self, id: TransferId) {
        self.cancelled = Some(id);
    }
    fn poll_completion(&mut self) -> Option<(TransferId, Completion)> {
        let index = self.pending.iter().position(|(id, t)| {
            !matches!(t.header().endpoint, 3 | 6) || self.cancelled == Some(*id)
        })?;
        let (id, mut t) = self.pending.remove(index)?;
        if t.header().direction == Direction::In {
            t.buffer_mut().fill(0x5a);
        } else if !t.buffer().is_empty() && t.layout().is_empty() {
            assert_eq!(t.buffer(), [1, 2, 3, 4]);
        }
        let stalled = t.header().endpoint == 0 && t.header().setup[1] == 0xff;
        let length = if !t.layout().is_empty() {
            t.packets_mut()[0].actual_length = 2;
            t.packets_mut()[1].actual_length = 1;
            t.packets_mut()[1].status = -71;
            3
        } else if stalled {
            0
        } else {
            t.header().transfer_buffer_length
        };
        Some((
            id,
            Completion {
                transfer: t,
                status: if stalled { usbip::status::PIPE } else { 0 },
                actual_length: length,
                start_frame: 0,
            },
        ))
    }
}
fn device() -> Device {
    Device {
        key: KEY,
        wire: usbip::Device {
            path: usbip::WireString::new(b"mock").unwrap(),
            bus_id: KEY.id.bus_id(),
            bus_number: 1,
            device_number: 1,
            speed: 3,
            vendor_id: 0x1209,
            product_id: 1,
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
                class: usbip::Interface {
                    class: 0xff,
                    subclass: 0,
                    protocol: 0,
                },
                endpoints: [1, 0x82, 0x83, 4, 0x84, 5, 0x85, 6, 0x86]
                    .into_iter()
                    .map(|address| Endpoint {
                        address,
                        kind: if matches!(address & 15, 5 | 6) {
                            TransferKind::Isochronous
                        } else if address & 15 == 4 {
                            TransferKind::Interrupt
                        } else {
                            TransferKind::Bulk
                        },
                        max_packet_size: 512,
                        service_bytes: 512,
                        interval: 0,
                        allowed_flags: if matches!(address & 15, 5 | 6) {
                            usbip::ISO_ASAP
                        } else {
                            0
                        },
                    })
                    .collect(),
            }],
        }],
    }
}
fn start(
    grant: bool,
) -> (
    TcpStream,
    std::thread::JoinHandle<Result<()>>,
    Arc<AtomicUsize>,
) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let peer = TcpStream::connect(listener.local_addr().unwrap()).unwrap();
    peer.set_read_timeout(Some(Duration::from_secs(3))).unwrap();
    let claims = Arc::new(AtomicUsize::new(0));
    let c = claims.clone();
    let task = std::thread::spawn(move || {
        let mut e = Engine::new(Mono::default(), Config::default(), 1, |_| {
            Ok(Mock {
                pending: VecDeque::new(),
                cancelled: None,
                claims: c,
            })
        })
        .unwrap();
        e.register(device()).unwrap();
        if grant {
            e.execute(
                Caller {
                    administrator: true,
                    ..CALLER
                },
                Command::Authorize {
                    device: KEY,
                    peer: CALLER.peer,
                    allow: true,
                },
                u64::MAX,
            )
            .unwrap();
        }
        let budget = e.budget();
        let result = transport::serve_insecure(
            &mut e,
            knightbus_transport::Stream::insecure_loopback(listener.accept().unwrap().0).unwrap(),
            CALLER,
        );
        e.shutdown();
        e.tick();
        assert!(e.shutdown_complete());
        drop(e);
        assert_eq!(budget.usage().bytes, 0);
        assert_eq!(budget.usage().requests, 0);
        result
    });
    (peer, task, claims)
}
fn read(peer: &mut TcpStream, context: Context) -> Message {
    usbip::read_message(peer, context, Limits::default(), &mut |_| Ok(()))
        .unwrap()
        .unwrap()
}
fn send(peer: &mut TcpStream, message: &Message) {
    usbip::write_message(peer, message, Limits::default(), &mut |_| Ok(())).unwrap();
}
fn header(seqnum: u32, endpoint: u32, direction: Direction) -> SubmitHeader {
    SubmitHeader {
        seqnum,
        device_id: 0x10001,
        direction,
        endpoint,
        transfer_flags: 0,
        transfer_buffer_length: 4,
        start_frame: 0,
        number_of_packets: 0,
        interval: 0,
        setup: [0; 8],
    }
}
#[test]
fn discovery_and_import_obey_exporter_grants_before_capture() {
    for grant in [false, true] {
        let (mut peer, task, claims) = start(grant);
        send(&mut peer, &Message::DevListRequest);
        let Message::DevListReply { status, devices } = read(&mut peer, Context::Management) else {
            panic!()
        };
        assert_eq!(status, 0);
        assert_eq!(devices.len(), usize::from(grant));
        task.join().unwrap().unwrap();
        assert_eq!(claims.load(Ordering::SeqCst), 0);
    }
    let (mut peer, task, claims) = start(false);
    send(&mut peer, &Message::ImportRequest(KEY.id.bus_id()));
    assert!(matches!(
        read(&mut peer, Context::Management),
        Message::ImportReply {
            status: 1,
            device: None
        }
    ));
    assert_eq!(task.join().unwrap(), Err(Error::Denied));
    assert_eq!(claims.load(Ordering::SeqCst), 0);
}
#[test]
fn fragmented_bidirectional_transfers_unlink_and_disconnect_retire() {
    let (mut peer, task, claims) = start(true);
    send(&mut peer, &Message::ImportRequest(KEY.id.bus_id()));
    assert!(matches!(
        read(&mut peer, Context::Management),
        Message::ImportReply { status: 0, .. }
    ));
    assert_eq!(claims.load(Ordering::SeqCst), 1);
    let out = header(1, 1, Direction::Out);
    let input = header(2, 2, Direction::In);
    let a = usbip::encode(
        &Message::Submit(out.with_payload(vec![1, 2, 3, 4], vec![])),
        Limits::default(),
        &mut |_| Ok(()),
    )
    .unwrap();
    let b = usbip::encode(
        &Message::Submit(input.with_payload(vec![], vec![])),
        Limits::default(),
        &mut |_| Ok(()),
    )
    .unwrap();
    peer.write_all(&a[..7]).unwrap();
    peer.write_all(&[&a[7..], &b].concat()).unwrap();
    let Message::ReturnSubmit(r) = read(&mut peer, Context::Replies(Some((&out).into()))) else {
        panic!()
    };
    assert_eq!(r.actual_length, 4);
    assert!(r.data.is_empty());
    let Message::ReturnSubmit(r) = read(&mut peer, Context::Replies(Some((&input).into()))) else {
        panic!()
    };
    assert_eq!(r.data, [0x5a; 4]);
    send(
        &mut peer,
        &Message::Submit(header(3, 3, Direction::In).with_payload(vec![], vec![])),
    );
    send(
        &mut peer,
        &Message::Unlink {
            seqnum: 4,
            device_id: 0x10001,
            target_seqnum: 3,
        },
    );
    assert_eq!(
        read(&mut peer, Context::Replies(None)),
        Message::ReturnUnlink {
            seqnum: 4,
            status: -104
        }
    );
    // EOF with another request pending must cancel it before native release.
    send(
        &mut peer,
        &Message::Submit(header(5, 3, Direction::In).with_payload(vec![], vec![])),
    );
    drop(peer);
    assert_eq!(task.join().unwrap(), Err(Error::Unavailable));
    assert_eq!(claims.load(Ordering::SeqCst), 0);
}

#[test]
fn control_bulk_interrupt_both_directions_and_stall_preserve_the_session() {
    let (mut peer, task, claims) = start(true);
    send(&mut peer, &Message::ImportRequest(KEY.id.bus_id()));
    assert!(matches!(
        read(&mut peer, Context::Management),
        Message::ImportReply { status: 0, .. }
    ));
    let mut sequence = 1;
    // Standard, class and vendor control requests pass through without class logic.
    for (endpoint, direction, setup_type) in [
        (0, Direction::In, 0x80),
        (0, Direction::Out, 0),
        (0, Direction::In, 0xa1),
        (0, Direction::Out, 0x21),
        (0, Direction::In, 0xc0),
        (0, Direction::Out, 0x40),
        (1, Direction::Out, 0),
        (2, Direction::In, 0),
        (4, Direction::Out, 0),
        (4, Direction::In, 0),
    ] {
        let mut h = header(sequence, endpoint, direction);
        if direction == Direction::In {
            h.transfer_flags = usbip::DIR_IN;
        }
        if endpoint == 0 {
            h.setup = [setup_type, 0x50, 0, 0, 0, 0, 4, 0];
        }
        let payload = if direction == Direction::Out {
            vec![1, 2, 3, 4]
        } else {
            vec![]
        };
        send(&mut peer, &Message::Submit(h.with_payload(payload, vec![])));
        let Message::ReturnSubmit(reply) = read(&mut peer, Context::Replies(Some((&h).into())))
        else {
            panic!()
        };
        assert_eq!(reply.status, 0);
        assert_eq!(reply.actual_length, 4);
        assert_eq!(
            reply.data,
            if direction == Direction::In {
                vec![0x5a; 4]
            } else {
                vec![]
            }
        );
        sequence += 1;
    }
    for request in [0xff, 0x50] {
        let mut h = header(sequence, 0, Direction::In);
        h.setup = [0xc0, request, 0, 0, 0, 0, 4, 0];
        send(&mut peer, &Message::Submit(h.with_payload(vec![], vec![])));
        let Message::ReturnSubmit(reply) = read(&mut peer, Context::Replies(Some((&h).into())))
        else {
            panic!()
        };
        assert_eq!(
            reply.status,
            if request == 0xff {
                usbip::status::PIPE
            } else {
                0
            }
        );
        assert_eq!(reply.actual_length, if request == 0xff { 0 } else { 4 });
        sequence += 1;
    }
    drop(peer);
    assert_eq!(task.join().unwrap(), Err(Error::Unavailable));
    assert_eq!(claims.load(Ordering::SeqCst), 0);
}

#[test]
fn iso_tcp_replies_pack_sparse_input_and_preserve_packet_errors_in_both_directions() {
    let (mut peer, task, claims) = start(true);
    send(&mut peer, &Message::ImportRequest(KEY.id.bus_id()));
    assert!(matches!(
        read(&mut peer, Context::Management),
        Message::ImportReply { status: 0, .. }
    ));
    for (sequence, direction) in [(1, Direction::In), (2, Direction::Out)] {
        let mut h = header(sequence, 5, direction);
        h.transfer_buffer_length = 16;
        h.number_of_packets = 2;
        h.transfer_flags = usbip::ISO_ASAP;
        let packets = vec![
            usbip::IsoPacket {
                offset: 0,
                length: 4,
                actual_length: 0,
                status: 0,
            },
            usbip::IsoPacket {
                offset: 12,
                length: 4,
                actual_length: 0,
                status: 0,
            },
        ];
        send(
            &mut peer,
            &Message::Submit(h.with_payload(
                if direction == Direction::Out {
                    vec![0; 16]
                } else {
                    vec![]
                },
                packets.clone(),
            )),
        );
        let Message::ReturnSubmit(reply) = read(&mut peer, Context::Replies(Some((&h).into())))
        else {
            panic!()
        };
        assert_eq!(reply.actual_length, 3);
        assert_eq!(reply.error_count, 1);
        assert_eq!(reply.iso_packets[1].status, -71);
        assert_eq!(reply.iso_packets[1].offset, 12);
        assert_eq!(
            reply.data,
            if direction == Direction::In {
                vec![0x5a; 3]
            } else {
                vec![]
            }
        );
    }
    drop(peer);
    assert_eq!(task.join().unwrap(), Err(Error::Unavailable));
    assert_eq!(claims.load(Ordering::SeqCst), 0);
}

#[test]
fn iso_unlink_and_disconnect_retire_in_both_directions() {
    for direction in [Direction::In, Direction::Out] {
        let (mut peer, task, claims) = start(true);
        send(&mut peer, &Message::ImportRequest(KEY.id.bus_id()));
        assert!(matches!(
            read(&mut peer, Context::Management),
            Message::ImportReply { status: 0, .. }
        ));
        for sequence in [1, 3] {
            let mut h = header(sequence, 6, direction);
            h.transfer_buffer_length = 16;
            h.number_of_packets = 2;
            h.transfer_flags = usbip::ISO_ASAP;
            let packets = [0, 12]
                .into_iter()
                .map(|offset| usbip::IsoPacket {
                    offset,
                    length: 4,
                    actual_length: 0,
                    status: 0,
                })
                .collect();
            send(
                &mut peer,
                &Message::Submit(h.with_payload(
                    if direction == Direction::Out {
                        vec![0; 16]
                    } else {
                        vec![]
                    },
                    packets,
                )),
            );
            if sequence == 1 {
                send(
                    &mut peer,
                    &Message::Unlink {
                        seqnum: 2,
                        device_id: 0x10001,
                        target_seqnum: 1,
                    },
                );
                assert_eq!(
                    read(&mut peer, Context::Replies(None)),
                    Message::ReturnUnlink {
                        seqnum: 2,
                        status: -104
                    }
                );
            }
        }
        drop(peer);
        assert_eq!(task.join().unwrap(), Err(Error::Unavailable));
        assert_eq!(claims.load(Ordering::SeqCst), 0);
    }
}

#[test]
fn secure_host_requires_grants_and_control_liveness_and_releases_on_revoke() {
    use knightbus_transport::{
        Connection,
        host::{Host, Remote},
        security::{Identity, Peers},
    };
    use std::time::Instant;
    let exporter = Identity::generate().unwrap();
    let importer = Identity::generate().unwrap();
    let exporter_id = exporter.peer();
    let importer_id = importer.peer();
    let mut peers = Peers::default();
    for id in [&exporter, &importer] {
        peers.pair(id.certificate().to_vec(), id.peer()).unwrap();
    }
    let claims = Arc::new(AtomicUsize::new(0));
    let mut engine = Engine::new(Mono::default(), Config::default(), 1, |_| {
        Ok(Mock {
            pending: VecDeque::new(),
            cancelled: None,
            claims: claims.clone(),
        })
    })
    .unwrap();
    engine.register(device()).unwrap();
    let budget = engine.budget();
    let host = Arc::new(Host::new(engine, exporter, peers.clone()));
    let usb = TcpListener::bind("127.0.0.1:0").unwrap();
    let usb_address = usb.local_addr().unwrap();
    let control = TcpListener::bind("127.0.0.1:0").unwrap();
    let control_address = control.local_addr().unwrap();
    let management_dir = std::env::temp_dir().join(format!(
        "kb-host-stop-{}-{}",
        std::process::id(),
        knightbus_transport::security::epoch().unwrap()
    ));
    let private = knightbus_ipc::private::Directory::open(&management_dir).unwrap();
    #[cfg(unix)]
    let management_path = management_dir.join("socket");
    #[cfg(windows)]
    let management_path = std::path::PathBuf::from(format!(
        r"\\.\pipe\{}",
        management_dir.file_name().unwrap().to_str().unwrap()
    ));
    let management = knightbus_transport::local::Listener::bind(&management_path).unwrap();
    let server = host.clone();
    let (finished, stopped) = std::sync::mpsc::channel();
    let task = std::thread::spawn(move || {
        let result = server.run(usb, control, Some(&management), None);
        finished.send(()).unwrap();
        result
    });
    let mut idle = knightbus_transport::local::Client::connect(&management_path).unwrap();
    assert!(matches!(
        idle.call(Command::Subscribe, u64::MAX),
        Ok(Reply::Subscription(_))
    ));
    let open = || {
        let mut remote = Remote::connect(
            TcpStream::connect(control_address).unwrap(),
            &importer,
            &peers,
            exporter_id,
        )
        .unwrap();
        let stream = remote
            .usb(TcpStream::connect(usb_address).unwrap(), &importer, &peers)
            .unwrap();
        // The ticket is consumed even if the import subsequently fails.
        assert!(matches!(
            remote.usb(TcpStream::connect(usb_address).unwrap(), &importer, &peers),
            Err(Error::State)
        ));
        let wire = Connection::new(
            stream,
            Budget::new(ResourceLimits {
                bytes: 4 * 1024 * 1024,
                requests: 32,
            }),
            Limits::default(),
            Duration::from_secs(5),
        )
        .unwrap();
        (remote, wire)
    };
    let exchange = |wire: &mut Connection, message: &Message, context| {
        wire.send(message).unwrap();
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            assert!(Instant::now() < deadline);
            wire.flush().unwrap();
            if let Some(message) = wire.receive(|_, _| Ok(context)).unwrap() {
                return message;
            }
            wire.wait(Duration::from_millis(10)).unwrap();
        }
    };
    let (remote, mut wire) = open();
    assert!(
        matches!(exchange(&mut wire,&Message::DevListRequest,Context::Management).message(),Message::DevListReply {devices,..} if devices.is_empty())
    );
    drop(wire);
    drop(remote);
    assert_eq!(claims.load(Ordering::SeqCst), 0);
    let admin = Caller {
        administrator: true,
        peer: importer_id,
    };
    host.execute(
        admin,
        Command::Authorize {
            device: KEY,
            peer: importer_id,
            allow: true,
        },
        u64::MAX,
    )
    .unwrap();
    // A control-link loss must retire native ownership even while USB TCP remains open.
    let (remote, mut wire) = open();
    assert!(matches!(
        exchange(
            &mut wire,
            &Message::ImportRequest(KEY.id.bus_id()),
            Context::Management
        )
        .message(),
        Message::ImportReply { status: 0, .. }
    ));
    assert_eq!(claims.load(Ordering::SeqCst), 1);
    // Keep the device through pushed lifecycle events and another heartbeat.
    // Notifications use valid control response IDs, so receiving one cannot kill the lease.
    let start = Instant::now();
    let mut events = Vec::new();
    while start.elapsed() < Duration::from_millis(3300) {
        remote.check().unwrap();
        if let Some(event) = remote.event() {
            events.push(event);
        }
        assert_eq!(claims.load(Ordering::SeqCst), 1);
        std::thread::sleep(Duration::from_millis(10));
    }
    assert!(events.iter().any(|event| event.kind == EventKind::Session));
    assert!(
        events
            .windows(2)
            .all(|pair| pair[0].sequence < pair[1].sequence)
    );
    drop(remote);
    let end = Instant::now() + Duration::from_secs(5);
    while claims.load(Ordering::SeqCst) != 0 {
        assert!(Instant::now() < end);
        std::thread::sleep(Duration::from_millis(5));
    }
    drop(wire);
    let (remote, mut wire) = open();
    assert!(matches!(
        exchange(
            &mut wire,
            &Message::ImportRequest(KEY.id.bus_id()),
            Context::Management
        )
        .message(),
        Message::ImportReply { status: 0, .. }
    ));
    host.revoke(importer_id).unwrap();
    let end = Instant::now() + Duration::from_secs(5);
    while claims.load(Ordering::SeqCst) != 0 {
        assert!(Instant::now() < end);
        std::thread::sleep(Duration::from_millis(5));
    }
    // Re-enrollment never restores the revoked device grants.
    host.pair(importer.certificate().to_vec(), importer_id)
        .unwrap();
    let Reply::Devices(devices) = host
        .execute(
            Caller {
                administrator: false,
                peer: importer_id,
            },
            Command::List,
            u64::MAX,
        )
        .unwrap()
    else {
        panic!()
    };
    assert!(devices.is_empty());
    drop(remote);
    drop(wire);
    let (ready, busy) = std::sync::mpsc::channel();
    let busy_client = std::thread::spawn(move || {
        let mut client = knightbus_transport::local::Client::connect(&management_path).unwrap();
        client.call(Command::List, u64::MAX).unwrap();
        ready.send(()).unwrap();
        while client.call(Command::List, u64::MAX).is_ok() {
            std::thread::sleep(Duration::from_millis(100));
        }
    });
    busy.recv_timeout(Duration::from_secs(5)).unwrap();
    host.shutdown();
    stopped
        .recv_timeout(Duration::from_secs(5))
        .expect("idle and busy management clients must not delay shutdown");
    task.join().unwrap().unwrap();
    busy_client.join().unwrap();
    drop(idle);
    drop(private);
    std::fs::remove_dir_all(management_dir).unwrap();
    drop(host);
    assert_eq!(budget.usage().bytes, 0);
    assert_eq!(budget.usage().requests, 0);
}
