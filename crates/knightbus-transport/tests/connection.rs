//! Stream fragmentation, hostile correlation and slow-peer resource deadlines.
use knightbus_core::{
    Budget, Error, ResourceLimits,
    usbip::{self, Context, Limits, Message},
};
use knightbus_transport::Connection as Wire;
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
    (
        Wire::new(receiver, b.clone(), Limits::default(), timeout).unwrap(),
        sender,
        b,
    )
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
        &mut |_| Ok(()),
    )
    .unwrap();
    let c = usbip::encode(
        &Message::ReturnUnlink {
            seqnum: 10,
            status: 0,
        },
        Limits::default(),
        &mut |_| Ok(()),
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
