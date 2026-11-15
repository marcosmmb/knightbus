//! Bounded nonblocking USB/IP transport, shared by all OS adapters.
//! TLS authenticates paired peers; raw streams require explicit compatibility opt-in.
//! Reply context is resolved before allocation.
use knightbus_core::{
    Budget, Buffer, DecodedMessage, Error, IncomingDecoder, MemoryReservation, Result,
    usbip::{self, Context, Limits},
};
/// OS-authenticated local management endpoint.
pub use knightbus_ipc::local;
use std::{
    io::{self, Read, Write},
    sync::Arc,
    task::{Wake, Waker},
    time::{Duration, Instant},
};
pub mod security;
mod stream;
pub use stream::Stream;

struct ReadyWake {
    wake: mio::Waker,
    // Fixed allowance for one socket registration, event storage and wake owner.
    // Keep it charged if a backend retains the wakeup after the stream closes.
    _storage: MemoryReservation,
}
impl Wake for ReadyWake {
    fn wake(self: Arc<Self>) {
        self.wake_by_ref();
    }
    fn wake_by_ref(self: &Arc<Self>) {
        let _ = self.wake.wake();
    }
}
const INPUT_BYTES: usize = 16 * 1024;
const IO_STEPS: usize = 16;
const IO_BYTES: usize = INPUT_BYTES * IO_STEPS;
const USBIP_PREFIX_BYTES: usize = 8;
const USBIP_SEQUENCE_OFFSET: usize = 4;
/// Bounded stream adapter. A partial frame has an absolute deadline; slow byte
/// delivery cannot keep renewing it. A failed write poisons the whole connection.
pub struct Connection {
    stream: Stream,
    _tls_storage: MemoryReservation,
    poll: mio::Poll,
    events: mio::Events,
    wake: Arc<ReadyWake>,
    interests: mio::Interest,
    read_blocked: bool,
    write_blocked: bool,
    budget: Budget,
    input: Buffer,
    start: usize,
    end: usize,
    prefix: [u8; USBIP_PREFIX_BYTES],
    prefix_len: usize,
    decoder: Option<IncomingDecoder>,
    frame_started: Option<Instant>,
    timeout: Duration,
    output: Option<(Buffer, usize, Instant)>,
    failed: bool,
    progressed: bool,
    limits: Limits,
}
impl Connection {
    /// Wrap an already authorized connection. This does not authenticate a peer.
    pub fn new(stream: Stream, budget: Budget, limits: Limits, timeout: Duration) -> Result<Self> {
        if timeout.is_zero() {
            return Err(Error::Invalid);
        }
        let mut stream = stream;
        stream.socket.set_nodelay(true).map_err(net_error)?;
        let tls_storage = budget.reserve_memory(stream.storage_bytes())?;
        let input = budget.buffer(INPUT_BYTES)?;
        let storage = budget.reserve_memory(16 * 1024)?;
        let poll = mio::Poll::new().map_err(net_error)?;
        poll.registry()
            .register(&mut stream.socket, mio::Token(0), mio::Interest::READABLE)
            .map_err(net_error)?;
        let wake = Arc::new(ReadyWake {
            wake: mio::Waker::new(poll.registry(), mio::Token(1)).map_err(net_error)?,
            _storage: storage,
        });
        Ok(Self {
            stream,
            _tls_storage: tls_storage,
            poll,
            events: mio::Events::with_capacity(4),
            wake,
            interests: mio::Interest::READABLE,
            read_blocked: true,
            write_blocked: false,
            budget,
            input,
            start: 0,
            end: 0,
            prefix: [0; USBIP_PREFIX_BYTES],
            prefix_len: 0,
            decoder: None,
            frame_started: None,
            timeout,
            output: None,
            failed: false,
            progressed: false,
            limits,
        })
    }
    /// Wake this connection's event wait when backend work becomes ready.
    pub fn waker(&self) -> Waker {
        Waker::from(self.wake.clone())
    }
    /// Wait for socket readiness or a backend wakeup, bounded by host housekeeping.
    /// Call only after draining ready work. Readiness is advisory: retry I/O.
    pub fn wait(&mut self, timeout: Duration) -> Result<()> {
        if self.failed {
            return Err(Error::State);
        }
        // Subscribe to writable readiness only after backpressure. A writable
        // idle socket otherwise keeps Windows polling instead of parking.
        let interests = if self.write_blocked || self.stream.pending_write() {
            if self.read_blocked {
                mio::Interest::READABLE | mio::Interest::WRITABLE
            } else {
                mio::Interest::WRITABLE
            }
        } else {
            mio::Interest::READABLE
        };
        if interests != self.interests {
            if let Err(error) =
                self.poll
                    .registry()
                    .reregister(&mut self.stream.socket, mio::Token(0), interests)
            {
                self.close();
                return Err(net_error(error));
            }
            self.interests = interests;
        }
        let timeout = self.output.as_ref().map_or(timeout, |(_, _, admitted)| {
            timeout.min(self.timeout.saturating_sub(admitted.elapsed()))
        });
        match self.poll.poll(&mut self.events, Some(timeout)) {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == io::ErrorKind::Interrupted => Ok(()),
            Err(e) => {
                self.close();
                Err(net_error(e))
            }
        }
    }
    /// Whether another outbound message can be admitted without queue growth.
    pub fn writable(&self) -> bool {
        !self.failed && self.output.is_none() && !self.stream.pending_write()
    }
    /// Take and clear activity since the last check. Partial reads/writes count;
    /// callers should wait only when false, and yield after bounded busy turns.
    pub fn take_progress(&mut self) -> bool {
        std::mem::take(&mut self.progressed)
    }
    /// Admit one outgoing frame. Backpressure leaves the caller's message intact.
    pub fn send<'a>(&mut self, message: impl Into<usbip::Frame<'a>>) -> Result<()> {
        if self.failed {
            return Err(Error::State);
        }
        if !self.writable() {
            return Err(Error::Exhausted);
        }
        let bytes = self
            .budget
            .encode(message, self.limits)
            .map_err(|_| Error::Invalid)?;
        self.output = Some((bytes, 0, Instant::now()));
        self.progressed = true;
        Ok(())
    }
    /// Advance short writes without blocking. No whole-frame retries after errors.
    pub fn flush(&mut self) -> Result<()> {
        if self.failed {
            return Err(Error::State);
        }
        let result = self
            .flush_inner(|stream, bytes| stream.write(bytes))
            .and_then(|()| {
                self.progressed |= self.stream.pump().map_err(net_error)?;
                if !self.stream.pending_write()
                    && self
                        .output
                        .as_ref()
                        .is_some_and(|(bytes, offset, _)| *offset == bytes.as_slice().len())
                {
                    self.output = None;
                }
                Ok(())
            });
        if result.is_err() {
            self.close();
        }
        result
    }
    /// Close the socket and reject further I/O. Pending input charges remain until drop.
    pub fn close(&mut self) {
        self.failed = true;
        self.stream.close();
        self.output = None;
    }
    fn flush_inner(
        &mut self,
        mut write: impl FnMut(&mut Stream, &[u8]) -> io::Result<usize>,
    ) -> Result<()> {
        let mut remaining = IO_BYTES;
        self.write_blocked = false;
        for _ in 0..IO_STEPS {
            let Some((bytes, offset, admitted)) = &mut self.output else {
                break;
            };
            // Progress never renews the deadline, even on the final write.
            if admitted.elapsed() >= self.timeout {
                return Err(Error::Deadline);
            }
            if *offset == bytes.as_slice().len() {
                if !self.stream.pending_write() {
                    self.output = None;
                }
                break;
            }
            let end = bytes.as_slice().len().min(*offset + remaining);
            match write(&mut self.stream, &bytes.as_slice()[*offset..end]) {
                Ok(0) => return Err(Error::Unavailable),
                Ok(n) => {
                    *offset += n;
                    remaining -= n;
                    self.progressed = true;
                    if *offset == bytes.as_slice().len() && !self.stream.pending_write() {
                        self.output = None;
                    }
                    if remaining == 0 {
                        break;
                    }
                }
                Err(e) if e.kind() == io::ErrorKind::WouldBlock => {
                    self.write_blocked = true;
                    break;
                }
                Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
                Err(e) => return Err(net_error(e)),
            }
        }
        Ok(())
    }
    /// Receive one frame. The resolver sees only the fixed command/sequence
    /// prefix and must return original context before body allocation. Drains a
    /// bounded batch of ready bytes; partial progress is reported by take_progress.
    pub fn receive(
        &mut self,
        context: impl FnMut(u32, u32) -> Result<Context>,
    ) -> Result<Option<DecodedMessage>> {
        if self.failed {
            return Err(Error::State);
        }
        let result = self.receive_with(context, |stream, bytes| stream.read(bytes));
        if result.is_err() {
            self.close();
        }
        result
    }
    fn receive_with(
        &mut self,
        mut context: impl FnMut(u32, u32) -> Result<Context>,
        mut read: impl FnMut(&mut Stream, &mut [u8]) -> io::Result<usize>,
    ) -> Result<Option<DecodedMessage>> {
        for _ in 0..IO_STEPS {
            let (message, progressed) = self.receive_step(&mut context, &mut read)?;
            self.progressed |= progressed;
            if message.is_some() || !progressed {
                return Ok(message);
            }
        }
        Ok(None)
    }
    fn receive_step(
        &mut self,
        context: &mut impl FnMut(u32, u32) -> Result<Context>,
        read: &mut impl FnMut(&mut Stream, &mut [u8]) -> io::Result<usize>,
    ) -> Result<(Option<DecodedMessage>, bool)> {
        if self
            .frame_started
            .is_some_and(|t| t.elapsed() >= self.timeout)
        {
            return Err(Error::Deadline);
        }
        if self.start == self.end {
            match read(&mut self.stream, self.input.as_mut_slice()) {
                Ok(0) => return Err(Error::Unavailable),
                Ok(n) => {
                    self.read_blocked = false;
                    self.start = 0;
                    self.end = n;
                }
                Err(e)
                    if matches!(
                        e.kind(),
                        io::ErrorKind::WouldBlock | io::ErrorKind::Interrupted
                    ) =>
                {
                    self.read_blocked = true;
                    return Ok((None, false));
                }
                Err(e) => return Err(net_error(e)),
            }
        }
        if self.decoder.is_none() {
            if self.prefix_len == 0 {
                self.frame_started = Some(Instant::now());
            }
            let n = (self.prefix.len() - self.prefix_len).min(self.end - self.start);
            self.prefix[self.prefix_len..self.prefix_len + n]
                .copy_from_slice(&self.input.as_slice()[self.start..self.start + n]);
            self.start += n;
            self.prefix_len += n;
            if self.prefix_len < self.prefix.len() {
                return Ok((None, true));
            }
            let command =
                u32::from_be_bytes(self.prefix[..USBIP_SEQUENCE_OFFSET].try_into().unwrap());
            let seq = u32::from_be_bytes(self.prefix[USBIP_SEQUENCE_OFFSET..].try_into().unwrap());
            let mut decoder = self.budget.decoder(context(command, seq)?, self.limits)?;
            let (_, message) = decoder.push(&self.prefix).map_err(|_| Error::Invalid)?;
            if message.is_some() {
                self.prefix_len = 0;
                self.frame_started = None;
                return Ok((message, true));
            }
            self.decoder = Some(decoder);
        }
        let (n, message) = self
            .decoder
            .as_mut()
            .unwrap()
            .push(&self.input.as_slice()[self.start..self.end])
            .map_err(|_| Error::Invalid)?;
        self.start += n;
        if message.is_some() {
            self.decoder = None;
            self.prefix_len = 0;
            self.frame_started = None;
        }
        Ok((message, true))
    }
}
impl Drop for Connection {
    fn drop(&mut self) {
        self.close();
    }
}
fn net_error(_: io::Error) -> Error {
    Error::Unavailable
}

#[cfg(test)]
mod tests {
    use super::*;
    use knightbus_core::Transfer;
    use std::net::{TcpListener, TcpStream};
    use usbip::{Direction, Message, SubmitHeader};
    const MAX_TRANSFER: usize = 1024 * 1024;
    const LIMITS: Limits = Limits {
        frame_bytes: MAX_TRANSFER + 48,
        transfer_bytes: MAX_TRANSFER,
        devices: 128,
        iso_packets: 0,
    };
    fn test_header(seqnum: u32, direction: Direction, length: u32) -> SubmitHeader {
        SubmitHeader {
            seqnum,
            device_id: 0x10001,
            direction,
            endpoint: 1,
            transfer_flags: 0,
            transfer_buffer_length: length,
            start_frame: 0,
            number_of_packets: 0,
            interval: 0,
            setup: [0; 8],
        }
    }

    fn test_connection() -> (Connection, TcpStream, Budget) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let peer = TcpStream::connect(listener.local_addr().unwrap()).unwrap();
        let (stream, _) = listener.accept().unwrap();
        let budget = Budget::new(knightbus_core::ResourceLimits {
            bytes: 4 * MAX_TRANSFER,
            requests: 4,
        });
        let connection = Connection::new(
            Stream::insecure_loopback(stream).unwrap(),
            budget.clone(),
            LIMITS,
            Duration::from_secs(10),
        )
        .unwrap();
        (connection, peer, budget)
    }

    #[test]
    fn final_tls_plaintext_keeps_its_deadline_until_ciphertext_drains() {
        use crate::security::{Channel, Identity, Peers};
        let server_identity = Identity::generate().unwrap();
        let client_identity = Identity::generate().unwrap();
        let server_peer = server_identity.peer();
        let mut peers = Peers::default();
        for identity in [&server_identity, &client_identity] {
            peers
                .pair(identity.certificate().to_vec(), identity.peer())
                .unwrap();
        }
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let trust = peers.clone();
        let (done, hold) = std::sync::mpsc::channel();
        let (ready, authenticated) = std::sync::mpsc::channel();
        let worker = std::thread::spawn(move || {
            let _stream = Stream::accept(
                listener.accept().unwrap().0,
                &server_identity,
                &trust,
                Channel::Usb,
                Duration::from_secs(5),
            )
            .unwrap();
            ready.send(()).unwrap();
            // Complete authentication, but never read USB data.
            let _ = hold.recv_timeout(Duration::from_secs(10));
        });
        let stream = Stream::connect(
            TcpStream::connect(address).unwrap(),
            &client_identity,
            &peers,
            server_peer,
            Channel::Usb,
            Duration::from_secs(5),
        )
        .unwrap();
        authenticated.recv_timeout(Duration::from_secs(5)).unwrap();
        let budget = Budget::new(knightbus_core::ResourceLimits {
            bytes: 2 * MAX_TRANSFER,
            requests: 4,
        });
        let mut connection =
            Connection::new(stream, budget.clone(), LIMITS, Duration::from_secs(1)).unwrap();
        connection.send(&Message::DevListRequest).unwrap();
        // Stop at rustls acceptance, before allowing ciphertext to drain.
        connection
            .flush_inner(|stream, bytes| stream.write(bytes))
            .unwrap();
        assert!(connection.stream.pending_write());
        let (bytes, offset, admitted) = connection
            .output
            .as_mut()
            .expect("TLS still owns the frame deadline");
        assert_eq!(*offset, bytes.as_slice().len());
        *admitted = Instant::now() - Duration::from_secs(1);
        assert!(!connection.writable());
        assert_eq!(
            connection.send(&Message::DevListRequest),
            Err(Error::Exhausted)
        );
        assert_eq!(connection.flush(), Err(Error::Deadline));
        assert_eq!(connection.flush(), Err(Error::State));
        drop(connection);
        assert_eq!(budget.usage().bytes, 0);
        done.send(()).unwrap();
        worker.join().unwrap();
    }

    #[test]
    fn backend_wake_is_latched_and_its_storage_outlives_the_connection() {
        let (mut connection, _peer, budget) = test_connection();
        let wake = connection.waker();
        for _ in 0..3 {
            wake.wake_by_ref();
            let deadline = Instant::now() + Duration::from_secs(3);
            loop {
                assert!(Instant::now() < deadline, "backend notification was lost");
                connection.wait(Duration::from_secs(1)).unwrap();
                if connection.events.iter().any(|e| e.token() == mio::Token(1)) {
                    break;
                }
            }
        }
        drop(connection);
        assert!(
            budget.usage().bytes > 0,
            "a retained wake owner stays charged"
        );
        wake.wake_by_ref(); // Late callback after stream retirement must be safe.
        drop(wake);
        assert_eq!(budget.usage().bytes, 0);
    }

    #[test]
    fn writable_idle_socket_does_not_generate_busy_wakeups() {
        let (mut connection, _peer, _) = test_connection();
        assert!(
            connection
                .receive(|_, _| Ok(Context::Management))
                .unwrap()
                .is_none()
        );
        connection.wait(Duration::from_millis(20)).unwrap();
        assert!(
            connection.events.is_empty(),
            "idle writable readiness must not spin"
        );
        assert!(!connection.take_progress());
    }

    #[test]
    fn writable_event_resumes_output_after_peer_backpressure() {
        let (mut connection, mut peer, budget) = test_connection();
        let message = Message::Submit(
            test_header(1, Direction::Out, 65536).with_payload(vec![0xa5; 65536], vec![]),
        );
        let mut sent = 0;
        let deadline = Instant::now() + Duration::from_secs(5);
        while !connection.write_blocked {
            assert!(Instant::now() < deadline, "peer never applied backpressure");
            if connection.writable() {
                connection.send(&message).unwrap();
                sent += 1;
            }
            connection.flush().unwrap();
        }
        let reader = std::thread::spawn(move || {
            peer.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
            let mut count = 0;
            while let Some(decoded) =
                usbip::read_message(&mut peer, Context::Requests, LIMITS, &mut |_| Ok(())).unwrap()
            {
                assert_eq!(decoded, message);
                count += 1;
            }
            count
        });
        while !connection.writable() {
            assert!(Instant::now() < deadline, "writable notification was lost");
            connection.wait(Duration::from_secs(1)).unwrap();
            connection.flush().unwrap();
        }
        // Returning to idle must remove writable interest again.
        connection.wait(Duration::ZERO).unwrap();
        assert_eq!(connection.interests, mio::Interest::READABLE);
        drop(connection);
        assert_eq!(reader.join().unwrap(), sent);
        assert_eq!(budget.usage().bytes, 0);
    }

    #[test]
    fn socket_event_wakes_a_waiting_receiver_without_timer_polling() {
        let (mut connection, mut peer, budget) = test_connection();
        let sender = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(10));
            usbip::write_message(&mut peer, &Message::DevListRequest, LIMITS, &mut |_| Ok(()))
                .unwrap();
            // Keep the stream open until the receiver acknowledges the frame.
            peer.set_read_timeout(Some(Duration::from_secs(3))).unwrap();
            assert_eq!(peer.read(&mut [0]).unwrap(), 0);
        });
        let deadline = Instant::now() + Duration::from_secs(3);
        loop {
            assert!(Instant::now() < deadline, "socket readiness was lost");
            if let Some(message) = connection.receive(|_, _| Ok(Context::Management)).unwrap() {
                assert_eq!(message.message(), &Message::DevListRequest);
                break;
            }
            connection.wait(Duration::from_secs(1)).unwrap();
        }
        drop(connection);
        sender.join().unwrap();
        assert_eq!(budget.usage().bytes, 0);
    }

    #[test]
    fn ready_large_frame_drains_without_waiting_between_chunks() {
        for length in [65536, MAX_TRANSFER] {
            let (mut connection, _peer, budget) = test_connection();
            let message = Message::Submit(
                test_header(42, Direction::Out, length as u32)
                    .with_payload(vec![0xa5; length], vec![]),
            );
            let bytes = usbip::encode(&message, LIMITS, &mut |_| Ok(())).unwrap();
            let mut source = io::Cursor::new(bytes);
            let mut contexts = 0;
            let mut turns = 0;
            let decoded = loop {
                turns += 1;
                assert!(turns <= 5, "ready data must not wait once per 16 KiB chunk");
                let before = source.position();
                let result = connection
                    .receive_with(
                        |command, seq| {
                            assert_eq!((command, seq), (1, 42));
                            contexts += 1;
                            Ok(Context::Requests)
                        },
                        |_, buffer| source.read(buffer),
                    )
                    .unwrap();
                assert!(
                    source.position() - before <= IO_BYTES as u64,
                    "a busy peer must return control within the byte budget"
                );
                assert!(connection.take_progress(), "partial frames are progress");
                assert!(!connection.take_progress(), "activity is consumed once");
                if let Some(decoded) = result {
                    break decoded;
                }
            };
            assert_eq!(turns, if length == 65536 { 1 } else { 5 });
            assert_eq!(contexts, 1, "resolve before allocating, once per frame");
            assert_eq!(decoded.message(), &message);
            assert!(
                connection
                    .receive_with(
                        |_, _| panic!("no prefix"),
                        |_, _| { Err(io::ErrorKind::WouldBlock.into()) }
                    )
                    .unwrap()
                    .is_none()
            );
            assert!(
                !connection.take_progress(),
                "an idle socket must allow sleep"
            );
            drop(decoded);
            drop(connection);
            assert_eq!(budget.usage().bytes, 0);
        }
    }

    #[test]
    fn short_reads_and_buffered_frames_preserve_progress_and_bound_work() {
        let (mut connection, _peer, _) = test_connection();
        let message = Message::ReturnUnlink {
            seqnum: 9,
            status: 0,
        };
        let bytes = usbip::encode(&message, LIMITS, &mut |_| Ok(())).unwrap();
        let mut source = io::Cursor::new(bytes.repeat(2));
        let mut calls = 0;
        assert!(
            connection
                .receive_with(
                    |_, _| Ok(Context::Replies(None)),
                    |_, output| {
                        calls += 1;
                        source.read(&mut output[..1])
                    }
                )
                .unwrap()
                .is_none()
        );
        assert_eq!(calls, IO_STEPS, "short reads cannot monopolize a turn");
        assert!(connection.take_progress());
        let first = connection
            .receive_with(
                |_, _| Ok(Context::Replies(None)),
                |_, output| source.read(output),
            )
            .unwrap()
            .unwrap();
        assert_eq!(first.message(), &message);
        assert!(connection.take_progress());
        let second = connection
            .receive_with(
                |_, _| Ok(Context::Replies(None)),
                |_, _| panic!("coalesced frame is already buffered"),
            )
            .unwrap()
            .unwrap();
        assert_eq!(second.message(), &message);
        assert!(
            connection.take_progress(),
            "buffered consumption also counts"
        );
    }

    #[test]
    fn short_writes_drain_with_byte_and_attempt_bounds_without_renewing_deadline() {
        let (mut connection, _peer, budget) = test_connection();
        let message = Message::Submit(
            test_header(42, Direction::Out, MAX_TRANSFER as u32)
                .with_payload(vec![0xa5; MAX_TRANSFER], vec![]),
        );
        let expected = usbip::encode(&message, LIMITS, &mut |_| Ok(())).unwrap();
        connection.send(&message).unwrap();
        assert!(connection.take_progress());
        let admitted = connection.output.as_ref().unwrap().2;
        let mut bytes = vec![];
        let mut calls = 0;
        connection
            .flush_inner(|_, part| {
                calls += 1;
                bytes.extend_from_slice(&part[..3]);
                Ok(3)
            })
            .unwrap();
        assert_eq!(calls, IO_STEPS);
        assert_eq!(bytes.len(), 3 * IO_STEPS);
        assert!(connection.take_progress());
        assert_eq!(connection.output.as_ref().unwrap().2, admitted);
        connection
            .flush_inner(|_, _| Err(io::ErrorKind::WouldBlock.into()))
            .unwrap();
        assert!(!connection.take_progress(), "backpressure must allow sleep");
        let mut interrupted = 0;
        connection
            .flush_inner(|_, _| {
                interrupted += 1;
                Err(io::ErrorKind::Interrupted.into())
            })
            .unwrap();
        assert_eq!(interrupted, IO_STEPS);
        assert!(!connection.take_progress());
        while !connection.writable() {
            let before = bytes.len();
            connection
                .flush_inner(|_, part| {
                    bytes.extend_from_slice(part);
                    Ok(part.len())
                })
                .unwrap();
            assert!(bytes.len() - before <= IO_BYTES);
            assert!(connection.take_progress());
        }
        assert_eq!(
            bytes, expected,
            "no duplication after short writes or backpressure"
        );
        drop(connection);
        assert_eq!(budget.usage().bytes, 0);
    }

    #[test]
    fn native_out_encoding_needs_only_the_owner_and_final_frame() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let stream = TcpStream::connect(listener.local_addr().unwrap()).unwrap();
        let (_peer, _) = listener.accept().unwrap();
        let budget = Budget::new(knightbus_core::ResourceLimits {
            // Input scratch, native payload/metadata, one output frame. There is
            // deliberately no room for another temporary payload copy.
            bytes: 3 * MAX_TRANSFER + 96 + std::mem::size_of::<Transfer>() + 1024,
            requests: 1,
        });
        let mut wire = Connection::new(
            Stream::insecure_loopback(stream).unwrap(),
            budget.clone(),
            LIMITS,
            Duration::from_secs(1),
        )
        .unwrap();
        let mut transfer = Transfer::new(
            &budget,
            test_header(71, Direction::Out, MAX_TRANSFER as u32),
            &[],
        )
        .unwrap();
        transfer.buffer_mut().fill(42);
        let baseline = budget.usage();
        let mut header = *transfer.header();
        header.seqnum = 6; // Wire correlation must not mutate native identity.
        wire.send(usbip::Frame::Submit {
            header: &header,
            data: transfer.buffer(),
            packets: transfer.layout(),
        })
        .unwrap();
        let output = &wire.output.as_ref().unwrap().0;
        assert_eq!(budget.usage().bytes, baseline.bytes + output.capacity());
        assert_eq!(budget.usage().requests, 1);
        let (Message::Submit(decoded), _) = usbip::decode(
            output.as_slice(),
            Context::Requests,
            LIMITS,
            &mut |_| Ok(()),
        )
        .unwrap() else {
            panic!("SUBMIT")
        };
        assert_eq!(decoded.seqnum, 6);
        assert_eq!(decoded.data, transfer.buffer());
        assert_eq!(transfer.header().seqnum, 71);
        drop(wire);
        assert_eq!(
            budget.usage().requests,
            1,
            "encoding does not retire native ownership"
        );
        drop(transfer);
        assert_eq!(budget.usage().bytes, 0);
    }

    #[test]
    fn partial_frame_deadline_does_not_restart_on_progress() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let mut sender = TcpStream::connect(listener.local_addr().unwrap()).unwrap();
        let (receiver, _) = listener.accept().unwrap();
        let mut wire = Connection::new(
            Stream::insecure_loopback(receiver).unwrap(),
            Budget::new(knightbus_core::ResourceLimits {
                bytes: 1024 * 1024,
                requests: 32,
            }),
            LIMITS,
            Duration::from_secs(10),
        )
        .unwrap();
        sender.write_all(&[0]).unwrap();
        let deadline = Instant::now() + Duration::from_secs(2);
        while wire.frame_started.is_none() {
            assert!(Instant::now() < deadline);
            assert!(
                wire.receive(|_, _| Ok(Context::Replies(None)))
                    .unwrap()
                    .is_none()
            );
            std::thread::yield_now();
        }
        // Control elapsed time without scheduler-dependent millisecond sleeps.
        let started = Instant::now() - Duration::from_secs(5);
        wire.frame_started = Some(started);
        sender.write_all(&[0]).unwrap();
        while wire.prefix_len != 2 {
            assert!(Instant::now() < deadline);
            assert!(
                wire.receive(|_, _| Ok(Context::Replies(None)))
                    .unwrap()
                    .is_none()
            );
            std::thread::yield_now();
        }
        assert_eq!(wire.frame_started, Some(started));
        wire.frame_started = Some(Instant::now() - Duration::from_secs(11));
        assert!(matches!(
            wire.receive(|_, _| Ok(Context::Replies(None))),
            Err(Error::Deadline)
        ));
    }
}

/// USB/IP discovery, import and forwarding.
pub mod exporter;

/// USB/IP import, descriptor bootstrap and forwarding.
pub mod importer;

pub mod control;
pub mod host;

/// Shared deadline and cancellation for network setup.
pub mod operation;
