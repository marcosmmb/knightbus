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
        let index = self
            .pending
            .iter()
            .position(|(id, t)| t.header().endpoint != 3 || self.cancelled == Some(*id))?;
        let (id, mut t) = self.pending.remove(index)?;
        if t.header().direction == Direction::In {
            t.buffer_mut().fill(0x5a);
        } else {
            assert_eq!(t.buffer(), [1, 2, 3, 4]);
        }
        let length = t.header().transfer_buffer_length;
        Some((
            id,
            Completion {
                transfer: t,
                status: 0,
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
                endpoints: [1, 0x82, 0x83]
                    .into_iter()
                    .map(|address| Endpoint {
                        address,
                        kind: TransferKind::Bulk,
                        max_packet_size: 512,
                        interval: 0,
                        allowed_flags: 0,
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
        let result = transport::serve(&mut e, listener.accept().unwrap().0, CALLER);
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
