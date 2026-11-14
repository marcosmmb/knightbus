//! Stream fragmentation, hostile correlation and slow-peer resource deadlines.
use knightbus_core::{
    Budget, Error, ResourceLimits,
    usbip::{self, Context, Limits, Message},
};
use knightbus_windows_importer::transport::Wire;
use std::{
    io::{Read, Write},
    net::{TcpListener, TcpStream},
    time::{Duration, Instant},
};
fn pair(timeout: Duration) -> (Wire, TcpStream, Budget) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let sender = TcpStream::connect(listener.local_addr().unwrap()).unwrap();
    let (receiver, _) = listener.accept().unwrap();
    let b = Budget::new(ResourceLimits {
        bytes: 1024 * 1024,
        requests: 32,
    });
    (Wire::new(receiver, b.clone(), timeout).unwrap(), sender, b)
}
#[test]
fn fragmented_and_coalesced_frames_preserve_boundaries() {
    let (mut wire, mut sender, b) = pair(Duration::from_secs(1));
    let a = usbip::encode(
        &Message::ReturnUnlink {
            seqnum: 9,
            status: -104,
        },
        Limits::default(),
    )
    .unwrap();
    let c = usbip::encode(
        &Message::ReturnUnlink {
            seqnum: 10,
            status: 0,
        },
        Limits::default(),
    )
    .unwrap();
    sender.write_all(&a[..3]).unwrap();
    assert!(
        wire.receive(|_, _| Ok(Context::Replies(None)))
            .unwrap()
            .is_none()
    );
    sender.write_all(&[&a[3..], &c].concat()).unwrap();
    let mut received = vec![];
    let deadline = Instant::now() + Duration::from_secs(1);
    while received.len() < 2 {
        assert!(Instant::now() < deadline);
        if let Some(m) = wire
            .receive(|command, seq| {
                assert_eq!(command, 4);
                assert!(seq == 9 || seq == 10);
                Ok(Context::Replies(None))
            })
            .unwrap()
        {
            received.push(m.message().clone());
        }
    }
    assert_eq!(
        received,
        vec![
            Message::ReturnUnlink {
                seqnum: 9,
                status: -104
            },
            Message::ReturnUnlink {
                seqnum: 10,
                status: 0
            }
        ]
    );
    drop(wire);
    assert_eq!(b.usage().bytes, 0);
}
#[test]
fn unknown_reply_is_rejected_before_declared_body_reservation() {
    let (mut wire, mut sender, b) = pair(Duration::from_secs(1));
    let baseline = b.usage().bytes;
    sender.write_all(&[0, 0, 0, 3, 0, 0, 0, 99]).unwrap();
    let deadline = Instant::now() + Duration::from_secs(1);
    loop {
        assert!(Instant::now() < deadline);
        match wire.receive(|_, _| Err(Error::Invalid)) {
            Err(Error::Invalid) => break,
            Ok(None) => {}
            _ => panic!("unexpected receive"),
        }
    }
    assert_eq!(b.usage().bytes, baseline);
    assert!(!wire.writable());
    assert!(matches!(
        wire.receive(|_, _| Ok(Context::Replies(None))),
        Err(Error::State)
    ));
    assert!(matches!(wire.flush(), Err(Error::State)));
}
#[test]
fn write_deadline_starts_at_admission_and_poisons_connection() {
    let (mut wire, mut peer, b) = pair(Duration::from_millis(30));
    let baseline = b.usage().bytes;
    wire.send(&Message::DevListRequest).unwrap();
    assert!(b.usage().bytes > baseline);
    // No flush yet: admission, not the first WouldBlock, starts the deadline.
    std::thread::sleep(Duration::from_millis(50));
    assert_eq!(wire.flush(), Err(Error::Deadline));
    assert_eq!(b.usage().bytes, baseline, "discard poisoned output storage");
    assert_eq!(wire.flush(), Err(Error::State));
    assert_eq!(wire.send(&Message::DevListRequest), Err(Error::State));
    peer.set_read_timeout(Some(Duration::from_secs(1))).unwrap();
    assert_eq!(peer.read(&mut [0]).unwrap(), 0, "connection must close");
    drop(wire);
    assert_eq!(b.usage().bytes, 0);
}
#[test]
fn nonreading_peer_cannot_keep_outbound_frame_alive() {
    use usbip::{Direction, SubmitHeader};
    let (mut wire, _peer, b) = pair(Duration::from_millis(50));
    let header = SubmitHeader {
        seqnum: 1,
        device_id: 1,
        direction: Direction::Out,
        endpoint: 1,
        transfer_flags: 0,
        transfer_buffer_length: 65536,
        start_frame: 0,
        number_of_packets: 0,
        interval: 0,
        setup: [0; 8],
    };
    let message = Message::Submit(header.with_payload(vec![42; 65536], vec![]));
    let deadline = Instant::now() + Duration::from_secs(5);
    let mut backpressured = false;
    loop {
        assert!(Instant::now() < deadline, "stalled frame never expired");
        if wire.writable() {
            wire.send(&message).unwrap();
        }
        match wire.flush() {
            Ok(()) => {
                if !wire.writable() {
                    backpressured = true;
                    std::thread::sleep(Duration::from_millis(1));
                }
            }
            Err(Error::Deadline) => break,
            result => panic!("unexpected write result: {result:?}"),
        }
    }
    assert!(backpressured, "must exercise short writes/WouldBlock");
    assert!(!wire.writable());
    assert_eq!(wire.flush(), Err(Error::State));
    drop(wire);
    assert_eq!(b.usage().bytes, 0);
}
#[test]
fn unconfigured_linux_gadget_requires_verified_configuration_and_charged_topology() {
    check_configuration(0);
    check_configuration(1);
}
fn check_configuration(reported_configuration: u8) {
    use usbip::{ReturnSubmit, WireString};
    let (mut wire, mut peer, budget) = pair(Duration::from_secs(2));
    let server = std::thread::spawn(move || {
        let Some(Message::ImportRequest(bus_id)) =
            usbip::read_message(&mut peer, Context::Management, Limits::default()).unwrap()
        else {
            panic!("import request")
        };
        usbip::write_message(
            &mut peer,
            &Message::ImportReply {
                status: 0,
                device: Some(Box::new(usbip::Device {
                    path: WireString::new(b"original-test-gadget").unwrap(),
                    bus_id,
                    bus_number: 0,
                    device_number: 0,
                    speed: 2,
                    vendor_id: 0x1209,
                    product_id: 1,
                    device_bcd: 0x100,
                    class: 0,
                    subclass: 0,
                    protocol: 0,
                    // Stock usbip-vudc advertises current, unconfigured state.
                    configuration: 0,
                    configurations: 1,
                    interfaces: 0,
                })),
            },
            Limits::default(),
        )
        .unwrap();
        let device = [18, 1, 0, 2, 0, 0, 0, 64, 9, 0x12, 1, 0, 0, 1, 0, 0, 0, 1];
        let config = [
            9, 2, 25, 0, 1, 1, 0, 0x80, 50, 9, 4, 0, 0, 1, 0xff, 0, 0, 0, 7, 5, 0x81, 3, 8, 0, 10,
        ];
        for (index, payload) in [
            &device[..],
            &config[..9],
            &config[..],
            &[][..],
            &[reported_configuration][..],
        ]
        .into_iter()
        .enumerate()
        {
            let Some(Message::Submit(request)) =
                usbip::read_message(&mut peer, Context::Requests, Limits::default()).unwrap()
            else {
                panic!("descriptor request")
            };
            assert_eq!(request.transfer_buffer_length as usize, payload.len());
            if index == 3 {
                assert_eq!(request.setup, [0, 9, 1, 0, 0, 0, 0, 0]);
            }
            if index == 4 {
                assert_eq!(request.setup, [0x80, 8, 0, 0, 0, 0, 1, 0]);
            }
            usbip::write_message(
                &mut peer,
                &Message::ReturnSubmit(ReturnSubmit {
                    request: (&request).into(),
                    status: 0,
                    actual_length: payload.len() as u32,
                    start_frame: 0,
                    number_of_packets: 0,
                    error_count: 0,
                    data: payload.to_vec(),
                    iso_packets: vec![],
                }),
                Limits::default(),
            )
            .unwrap();
        }
    });
    let result = wire.import(WireString::new(b"usbip-vudc.0").unwrap());
    if reported_configuration == 0 {
        assert!(matches!(result, Err(Error::Unavailable)));
        assert!(!wire.writable());
        server.join().unwrap();
        drop(wire);
        assert_eq!(budget.usage().bytes, 0);
        return;
    }
    let (device, descriptors) = result.unwrap();
    assert_eq!(device.device().wire.configuration, 1);
    assert_eq!(
        device.device().configurations[0].interfaces[0].endpoints[0].address,
        0x81
    );
    assert_eq!(descriptors.topology.interfaces, 1);
    server.join().unwrap();
    drop(wire);
    drop(descriptors);
    assert!(
        budget.usage().bytes > 0,
        "inventory must retain its storage charge"
    );
    drop(device);
    assert_eq!(budget.usage().bytes, 0);
}
