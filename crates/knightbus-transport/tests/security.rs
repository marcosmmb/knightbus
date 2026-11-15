//! Real TLS authentication and unchanged USB/IP framing.
use knightbus_core::{
    Budget, Error, ResourceLimits,
    usbip::{self, Context, Message},
};
use knightbus_transport::{
    Connection, Stream,
    security::{Channel, Identity, Peers, fingerprint},
};
use std::{
    net::{TcpListener, TcpStream},
    time::{Duration, Instant},
};
const TIMEOUT: Duration = Duration::from_secs(3);
fn paired() -> (Identity, Identity, Peers) {
    let a = Identity::generate().unwrap();
    let b = Identity::generate().unwrap();
    let mut peers = Peers::default();
    for id in [&a, &b] {
        peers.pair(id.certificate().to_vec(), id.peer()).unwrap();
    }
    (a, b, peers)
}
#[test]
fn tls_carries_unmodified_usbip_and_recovers_credits() {
    let (a, b, peers) = paired();
    let peer = a.peer();
    let expected = b.peer();
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    let trust = peers.clone();
    let thread = std::thread::spawn(move || {
        let stream = Stream::accept(
            listener.accept().unwrap().0,
            &a,
            &trust,
            Channel::Usb,
            TIMEOUT,
        )
        .unwrap();
        assert_eq!(stream.caller().unwrap().peer, expected);
        assert!(!stream.caller().unwrap().administrator);
        let budget = Budget::new(ResourceLimits {
            bytes: 2 * 1024 * 1024,
            requests: 4,
        });
        let mut wire =
            Connection::new(stream, budget.clone(), usbip::Limits::default(), TIMEOUT).unwrap();
        let deadline = Instant::now() + TIMEOUT;
        loop {
            assert!(Instant::now() < deadline);
            if let Some(msg) = wire.receive(|_, _| Ok(Context::Management)).unwrap() {
                assert_eq!(msg.message(), &Message::DevListRequest);
                break;
            }
            wire.wait(Duration::from_millis(10)).unwrap();
        }
        wire.send(&Message::DevListReply {
            status: 0,
            devices: vec![],
        })
        .unwrap();
        while !wire.writable() {
            wire.flush().unwrap();
            wire.wait(Duration::from_millis(1)).unwrap();
        }
        drop(wire);
        assert_eq!(budget.usage().bytes, 0);
    });
    let mut stream = Stream::connect(
        TcpStream::connect(addr).unwrap(),
        &b,
        &peers,
        peer,
        Channel::Usb,
        TIMEOUT,
    )
    .unwrap();
    // The plaintext codec sees exactly the standard USB/IP stream.
    use std::io::{Read, Write};
    let request = usbip::encode(
        &Message::DevListRequest,
        usbip::Limits::default(),
        &mut |_| Ok(()),
    )
    .unwrap();
    stream.write_all(&request).unwrap();
    let deadline = Instant::now() + TIMEOUT;
    loop {
        match stream.flush() {
            Ok(()) => break,
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {}
            Err(e) => panic!("{e}"),
        }
        assert!(Instant::now() < deadline);
        std::thread::yield_now();
    }
    let mut response = vec![];
    let mut buf = [0; 64];
    while response.len() < 12 {
        assert!(Instant::now() < deadline);
        match stream.read(&mut buf) {
            Ok(n) if n > 0 => response.extend_from_slice(&buf[..n]),
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => std::thread::yield_now(),
            r => panic!("{r:?}"),
        }
    }
    assert_eq!(
        usbip::decode(
            &response,
            Context::Management,
            usbip::Limits::default(),
            &mut |_| Ok(())
        )
        .unwrap()
        .0,
        Message::DevListReply {
            status: 0,
            devices: vec![]
        }
    );
    thread.join().unwrap();
}
#[test]
fn wrong_peer_wrong_channel_and_unpaired_client_are_denied() {
    for mismatch in 0..3 {
        let (a, b, mut peers) = paired();
        let peer = a.peer();
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let mut trust = peers.clone();
        if mismatch == 0 {
            trust.revoke(b.peer());
        }
        if mismatch == 2 {
            let impostor = Identity::generate().unwrap();
            peers.revoke(peer);
            peers
                .pair(impostor.certificate().to_vec(), impostor.peer())
                .unwrap();
        }
        let thread = std::thread::spawn(move || {
            Stream::accept(
                listener.accept().unwrap().0,
                &a,
                &trust,
                Channel::Usb,
                TIMEOUT,
            )
            .is_err()
        });
        let result = Stream::connect(
            TcpStream::connect(addr).unwrap(),
            &b,
            &peers,
            peer,
            if mismatch == 1 {
                Channel::Control
            } else {
                Channel::Usb
            },
            TIMEOUT,
        );
        // TLS 1.3 client may finish before receiving the server's client-cert rejection.
        if mismatch != 0 {
            assert!(result.is_err());
        }
        drop(result);
        assert!(thread.join().unwrap());
    }
}
#[test]
fn pairing_requires_confirmation_and_revocation_removes_trust() {
    let a = Identity::generate().unwrap();
    let mut peers = Peers::default();
    assert_eq!(
        peers.pair(a.certificate().to_vec(), fingerprint(b"wrong")),
        Err(Error::Denied)
    );
    assert!(!peers.contains(a.peer()));
    peers.pair(a.certificate().to_vec(), a.peer()).unwrap();
    peers.pair(a.certificate().to_vec(), a.peer()).unwrap();
    peers.revoke(a.peer());
    assert!(!peers.contains(a.peer()));
}

#[test]
fn persistent_pairing_survives_restart_and_never_replaces_an_identity() {
    use knightbus_transport::security::{Store, display_fingerprint, parse_fingerprint};
    let path = std::env::temp_dir().join(format!(
        "kb-trust-{}-{}",
        std::process::id(),
        knightbus_transport::security::epoch().unwrap()
    ));
    let peer = Store::initialize(&path).unwrap();
    assert!(Store::initialize(&path).is_err());
    let store = Store::open(&path).unwrap();
    assert_eq!(store.identity().unwrap().peer(), peer);
    let other = Identity::generate().unwrap();
    store
        .pair(other.certificate().to_vec(), other.peer())
        .unwrap();
    store
        .pair(other.certificate().to_vec(), other.peer())
        .unwrap();
    drop(store);
    let store = Store::open(&path).unwrap();
    assert!(store.peers().unwrap().contains(other.peer()));
    store.revoke(other.peer()).unwrap();
    drop(store);
    assert!(
        !Store::open(&path)
            .unwrap()
            .peers()
            .unwrap()
            .contains(other.peer())
    );
    assert_eq!(parse_fingerprint(&display_fingerprint(peer)).unwrap(), peer);
    assert!(parse_fingerprint("01").is_err());
    assert!(parse_fingerprint(&"ø".repeat(32)).is_err());
    std::fs::remove_dir_all(path).unwrap();
}
#[test]
fn silent_handshake_is_bounded_without_a_plaintext_fallback() {
    let (identity, _, peers) = paired();
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let _silent = TcpStream::connect(listener.local_addr().unwrap()).unwrap();
    let start = Instant::now();
    assert!(matches!(
        Stream::accept(
            listener.accept().unwrap().0,
            &identity,
            &peers,
            Channel::Usb,
            Duration::from_millis(30)
        ),
        Err(Error::Deadline)
    ));
    assert!(start.elapsed() < Duration::from_secs(1));
}

#[test]
fn tls_large_transfers_preserve_bytes_and_bound_backpressure() {
    let (a, b, peers) = paired();
    let peer = a.peer();
    let trust = peers.clone();
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    let limits = usbip::Limits {
        frame_bytes: 1024 * 1024 + 48,
        transfer_bytes: 1024 * 1024,
        devices: 1,
        iso_packets: 0,
    };
    let budget = || {
        Budget::new(ResourceLimits {
            bytes: 4 * 1024 * 1024,
            requests: 4,
        })
    };
    let task = std::thread::spawn(move || {
        let stream = Stream::accept(
            listener.accept().unwrap().0,
            &a,
            &trust,
            Channel::Usb,
            TIMEOUT,
        )
        .unwrap();
        let credits = budget();
        let mut wire = Connection::new(stream, credits.clone(), limits, TIMEOUT).unwrap();
        for _ in 0..8 {
            let deadline = Instant::now() + TIMEOUT;
            let message = loop {
                assert!(Instant::now() < deadline);
                wire.flush().unwrap();
                if let Some(m) = wire.receive(|_, _| Ok(Context::Requests)).unwrap() {
                    break m;
                }
                if !wire.take_progress() {
                    wire.wait(Duration::from_millis(10)).unwrap();
                }
            };
            wire.send(message.message()).unwrap();
            drop(message);
            while !wire.writable() {
                wire.flush().unwrap();
                assert!(Instant::now() < deadline);
                if !wire.take_progress() {
                    wire.wait(Duration::from_millis(10)).unwrap();
                }
            }
        }
        drop(wire);
        assert_eq!(credits.usage().bytes, 0);
    });
    let stream = Stream::connect(
        TcpStream::connect(address).unwrap(),
        &b,
        &peers,
        peer,
        Channel::Usb,
        TIMEOUT,
    )
    .unwrap();
    let credits = budget();
    let mut wire = Connection::new(stream, credits.clone(), limits, TIMEOUT).unwrap();
    for sequence in 1..=8 {
        let header = usbip::SubmitHeader {
            seqnum: sequence,
            device_id: 0x10001,
            direction: usbip::Direction::Out,
            endpoint: 1,
            transfer_flags: 0,
            transfer_buffer_length: 1024 * 1024,
            start_frame: 0,
            number_of_packets: 0,
            interval: 0,
            setup: [0; 8],
        };
        let message =
            Message::Submit(header.with_payload(vec![sequence as u8; 1024 * 1024], vec![]));
        wire.send(&message).unwrap();
        let deadline = Instant::now() + TIMEOUT;
        loop {
            assert!(Instant::now() < deadline);
            wire.flush().unwrap();
            if let Some(reply) = wire.receive(|_, _| Ok(Context::Requests)).unwrap() {
                assert_eq!(reply.message(), &message);
                break;
            }
            if !wire.take_progress() {
                wire.wait(Duration::from_millis(10)).unwrap();
            }
        }
    }
    task.join().unwrap();
    drop(wire);
    assert_eq!(credits.usage().bytes, 0);
}

#[test]
fn tls_peer_that_stops_reading_cannot_hold_output_past_its_deadline() {
    let (server_identity, client_identity, peers) = paired();
    let server_peer = server_identity.peer();
    let trust = peers.clone();
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    let (ready, authenticated) = std::sync::mpsc::channel();
    let (done, hold) = std::sync::mpsc::channel();
    let worker = std::thread::spawn(move || {
        let _stream = Stream::accept(
            listener.accept().unwrap().0,
            &server_identity,
            &trust,
            Channel::Usb,
            TIMEOUT,
        )
        .unwrap();
        ready.send(()).unwrap();
        let _ = hold.recv_timeout(Duration::from_secs(15));
    });
    let stream = Stream::connect(
        TcpStream::connect(address).unwrap(),
        &client_identity,
        &peers,
        server_peer,
        Channel::Usb,
        TIMEOUT,
    )
    .unwrap();
    authenticated.recv_timeout(TIMEOUT).unwrap();
    let credits = Budget::new(ResourceLimits {
        bytes: 2 * 1024 * 1024,
        requests: 4,
    });
    let limits = usbip::Limits::default();
    let mut wire =
        Connection::new(stream, credits.clone(), limits, Duration::from_millis(250)).unwrap();
    let header = usbip::SubmitHeader {
        seqnum: 1,
        device_id: 0x10001,
        direction: usbip::Direction::Out,
        endpoint: 1,
        transfer_flags: 0,
        transfer_buffer_length: 16 * 1024,
        start_frame: 0,
        number_of_packets: 0,
        interval: 0,
        setup: [0; 8],
    };
    let message = Message::Submit(header.with_payload(vec![42; 16 * 1024], vec![]));
    let mut expired = false;
    let deadline = Instant::now() + Duration::from_secs(10);
    // Bounded traffic fills the kernel queue; the peer keeps its socket open.
    'frames: for _ in 0..4096 {
        wire.send(&message).unwrap();
        while !wire.writable() {
            assert!(
                Instant::now() < deadline,
                "TLS backpressure lost its deadline"
            );
            match wire.flush() {
                Err(Error::Deadline) => {
                    expired = true;
                    break 'frames;
                }
                Err(error) => panic!("unexpected TLS failure: {error:?}"),
                Ok(()) => {}
            }
            if !wire.take_progress() {
                wire.wait(Duration::from_millis(10)).unwrap();
            }
        }
    }
    assert!(
        expired,
        "the nonreading peer must eventually exhaust socket capacity"
    );
    assert_eq!(wire.flush(), Err(Error::State));
    drop(wire);
    assert_eq!(credits.usage().bytes, 0);
    done.send(()).unwrap();
    worker.join().unwrap();
}

#[test]
fn connection_setup_observes_deadline_and_cancellation() {
    use knightbus_transport::{host::Remote, operation::Operation};
    use std::sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc,
    };
    // Cover a silent TLS handshake and an authenticated peer withholding Hello.
    for authenticate in [false, true] {
        for cancel in [false, true] {
            let (server_identity, client_identity, peers) = paired();
            let peer = server_identity.peer();
            let listener = TcpListener::bind("127.0.0.1:0").unwrap();
            let address = listener.local_addr().unwrap();
            let trust = peers.clone();
            let cancelled = Arc::new(AtomicBool::new(false));
            let signal = cancelled.clone();
            let (done, release) = mpsc::channel();
            let (connected, wait_connected) = mpsc::channel();
            let server = std::thread::spawn(move || {
                let socket = listener.accept().unwrap().0;
                wait_connected.recv_timeout(TIMEOUT).unwrap();
                let _stream = if authenticate {
                    Stream::accept(socket, &server_identity, &trust, Channel::Control, TIMEOUT)
                        .unwrap()
                } else {
                    Stream::insecure_loopback(socket).unwrap()
                };
                if cancel {
                    signal.store(true, Ordering::SeqCst);
                }
                release.recv_timeout(TIMEOUT).unwrap();
            });
            let started = Instant::now();
            let check = || cancelled.load(Ordering::SeqCst);
            let operation = Operation::new(
                started
                    + if cancel {
                        TIMEOUT
                    } else {
                        Duration::from_millis(200)
                    },
                &check,
            );
            let socket = operation.connect(address).unwrap();
            connected.send(()).unwrap();
            let result = Remote::connect_until(socket, &client_identity, &peers, peer, &operation);
            assert!(
                matches!(result, Err(e) if e == if cancel { Error::Cancelled } else { Error::Deadline })
            );
            assert!(started.elapsed() < Duration::from_millis(500));
            done.send(()).unwrap();
            server.join().unwrap();
        }
    }
}

#[test]
fn expired_or_cancelled_operations_never_open_tcp_connections() {
    use knightbus_transport::operation::Operation;
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    listener.set_nonblocking(true).unwrap();
    let address = listener.local_addr().unwrap();
    assert!(matches!(
        Operation::new(Instant::now(), &|| false).connect(address),
        Err(Error::Deadline)
    ));
    assert!(matches!(
        Operation::new(Instant::now() + TIMEOUT, &|| true).connect(address),
        Err(Error::Cancelled)
    ));
    assert!(matches!(listener.accept(), Err(e) if e.kind() == std::io::ErrorKind::WouldBlock));
}
