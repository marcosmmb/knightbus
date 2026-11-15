//! Bounded management framing, separate from USB/IP.
use crate::Stream;
use crate::operation::Operation;
use knightbus_core::{CONTROL_VERSION, Error, Event, Reply, Response, Result};
use std::{
    io::{Read, Write},
    time::{Duration, Instant},
};
/// Maximum management payload, including versioned IPC envelopes.
pub const MAX_FRAME: usize = 8192;
const LENGTH_PREFIX_BYTES: usize = size_of::<u32>();
const SOCKET: mio::Token = mio::Token(0);
const POLL_CAPACITY: usize = 2;
pub(crate) const TICKET_BYTES: usize = 32;
type Ticket = [u8; TICKET_BYTES];
pub(crate) const TIMEOUT: Duration = Duration::from_secs(5);

// Fixed tags and field widths belong to this protocol, not the session loops.
#[repr(u8)]
enum Tag {
    Hello = 1,
    Ping = 2,
    Pong = 3,
    Event = 4,
}

pub(crate) enum Message {
    Hello(Ticket),
    Ping(u64),
    Pong(u64),
    Event(Event),
}
impl Message {
    pub fn encode(&self) -> Result<Vec<u8>> {
        let mut bytes = Vec::new();
        match self {
            Self::Hello(ticket) => {
                bytes.push(Tag::Hello as u8);
                bytes.extend_from_slice(&CONTROL_VERSION.to_be_bytes());
                bytes.extend_from_slice(ticket);
            }
            Self::Ping(sequence) | Self::Pong(sequence) => {
                let tag = if matches!(self, Self::Ping(_)) {
                    Tag::Ping
                } else {
                    Tag::Pong
                };
                bytes.push(tag as u8);
                bytes.extend_from_slice(&sequence.to_be_bytes());
            }
            Self::Event(event) => {
                bytes.push(Tag::Event as u8);
                bytes.extend_from_slice(&knightbus_ipc::encode_response(&Response {
                    id: event.sequence,
                    result: Ok(Reply::Event(Some(*event))),
                })?);
            }
        }
        Ok(bytes)
    }

    pub fn decode(bytes: &[u8]) -> Result<Self> {
        let (&tag, payload) = bytes.split_first().ok_or(Error::Invalid)?;
        match tag {
            tag if tag == Tag::Hello as u8 => {
                const VERSION_BYTES: usize = size_of::<u16>();
                if payload.len() != VERSION_BYTES + TICKET_BYTES {
                    return Err(Error::Version);
                }
                let (version, ticket) = payload.split_at(VERSION_BYTES);
                if u16::from_be_bytes(version.try_into().unwrap()) != CONTROL_VERSION {
                    return Err(Error::Version);
                }
                Ok(Self::Hello(ticket.try_into().unwrap()))
            }
            tag if tag == Tag::Ping as u8 || tag == Tag::Pong as u8 => {
                let sequence = u64::from_be_bytes(payload.try_into().map_err(|_| Error::Invalid)?);
                Ok(if tag == Tag::Ping as u8 {
                    Self::Ping(sequence)
                } else {
                    Self::Pong(sequence)
                })
            }
            tag if tag == Tag::Event as u8 => {
                let response = knightbus_ipc::decode_response(payload)?;
                let Ok(Reply::Event(Some(event))) = response.result else {
                    return Err(Error::Invalid);
                };
                if response.id != event.sequence {
                    return Err(Error::Invalid);
                }
                Ok(Self::Event(event))
            }
            _ => Err(Error::Invalid),
        }
    }
}

pub(crate) struct Framed {
    stream: Stream,
    poll: mio::Poll,
    events: mio::Events,
    buffer: [u8; MAX_FRAME + LENGTH_PREFIX_BYTES],
    used: usize,
    expected: usize,
    started: Option<Instant>,
}
impl Framed {
    pub fn new(mut stream: Stream) -> Result<Self> {
        let poll = mio::Poll::new().map_err(|_| Error::Unavailable)?;
        poll.registry()
            .register(&mut stream.socket, SOCKET, mio::Interest::READABLE)
            .map_err(|_| Error::Unavailable)?;
        Ok(Self {
            stream,
            poll,
            events: mio::Events::with_capacity(POLL_CAPACITY),
            buffer: [0; MAX_FRAME + LENGTH_PREFIX_BYTES],
            used: 0,
            expected: LENGTH_PREFIX_BYTES,
            started: None,
        })
    }
    pub fn into_stream(mut self) -> Result<Stream> {
        if self.used != 0 {
            return Err(Error::State);
        }
        self.poll
            .registry()
            .deregister(&mut self.stream.socket)
            .map_err(|_| Error::Unavailable)?;
        Ok(self.stream)
    }
    fn wait(&mut self, duration: Duration) -> Result<()> {
        let interest = if self.stream.pending_write() {
            mio::Interest::READABLE | mio::Interest::WRITABLE
        } else {
            mio::Interest::READABLE
        };
        self.poll
            .registry()
            .reregister(&mut self.stream.socket, SOCKET, interest)
            .map_err(|_| Error::Unavailable)?;
        self.poll
            .poll(&mut self.events, Some(duration))
            .map_err(|_| Error::Unavailable)
    }
    pub fn send(&mut self, payload: &[u8]) -> Result<()> {
        self.send_until(
            payload,
            &Operation::new(Instant::now() + TIMEOUT, &|| false),
        )
    }
    pub fn send_until(&mut self, payload: &[u8], operation: &Operation<'_>) -> Result<()> {
        if payload.is_empty() || payload.len() > MAX_FRAME {
            return Err(Error::Invalid);
        }
        let mut output = [0; MAX_FRAME + LENGTH_PREFIX_BYTES];
        output[..LENGTH_PREFIX_BYTES].copy_from_slice(&(payload.len() as u32).to_be_bytes());
        output[LENGTH_PREFIX_BYTES..LENGTH_PREFIX_BYTES + payload.len()].copy_from_slice(payload);
        let mut offset = 0;
        loop {
            operation.check()?;
            if offset < LENGTH_PREFIX_BYTES + payload.len() {
                match self
                    .stream
                    .write(&output[offset..LENGTH_PREFIX_BYTES + payload.len()])
                {
                    Ok(0) => return Err(Error::Unavailable),
                    Ok(n) => offset += n,
                    Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {}
                    Err(_) => return Err(Error::Unavailable),
                }
            }
            self.stream.pump().map_err(|_| Error::Unavailable)?;
            if offset == LENGTH_PREFIX_BYTES + payload.len() && !self.stream.pending_write() {
                operation.check()?;
                return Ok(());
            }
            self.wait(operation.wait()?)?;
        }
    }
    pub fn receive(&mut self, timeout: Duration) -> Result<Option<Vec<u8>>> {
        self.receive_until(
            timeout,
            &Operation::new(Instant::now() + timeout + TIMEOUT, &|| false),
        )
    }
    pub fn receive_until(
        &mut self,
        timeout: Duration,
        operation: &Operation<'_>,
    ) -> Result<Option<Vec<u8>>> {
        let deadline = Instant::now() + timeout;
        loop {
            operation.check()?;
            if self.started.is_some_and(|t| t.elapsed() >= TIMEOUT) {
                return Err(Error::Deadline);
            }
            match self.stream.read(&mut self.buffer[self.used..self.expected]) {
                Ok(0) => return Err(Error::Unavailable),
                Ok(n) => {
                    self.started.get_or_insert_with(Instant::now);
                    self.used += n;
                    if self.used == LENGTH_PREFIX_BYTES && self.expected == LENGTH_PREFIX_BYTES {
                        let size = u32::from_be_bytes(
                            self.buffer[..LENGTH_PREFIX_BYTES].try_into().unwrap(),
                        ) as usize;
                        if size == 0 || size > MAX_FRAME {
                            return Err(Error::Invalid);
                        }
                        self.expected = LENGTH_PREFIX_BYTES + size;
                    } else if self.used == self.expected {
                        let bytes = self.buffer[LENGTH_PREFIX_BYTES..self.used].to_vec();
                        self.used = 0;
                        self.expected = LENGTH_PREFIX_BYTES;
                        self.started = None;
                        operation.check()?;
                        return Ok(Some(bytes));
                    }
                }
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                    if Instant::now() >= deadline {
                        return Ok(None);
                    }
                    let remaining = deadline.saturating_duration_since(Instant::now());
                    self.wait(
                        remaining.min(operation.wait()?).min(
                            self.started
                                .map_or(TIMEOUT, |t| TIMEOUT.saturating_sub(t.elapsed())),
                        ),
                    )?;
                }
                Err(_) => return Err(Error::Unavailable),
            }
        }
    }
}
/// Token bucket for low-frequency management work. USB transfers are not throttled.
pub(crate) struct Rate {
    tokens: u32,
    updated: Instant,
}
impl Rate {
    const BURST: u32 = 32;
    const REFILL: Duration = Duration::from_millis(100);
    pub fn new() -> Self {
        Self {
            tokens: Self::BURST,
            updated: Instant::now(),
        }
    }
    pub fn admit(&mut self) -> Result<()> {
        let elapsed = self.updated.elapsed().as_millis() / Self::REFILL.as_millis();
        if elapsed > 0 {
            self.tokens = self
                .tokens
                .saturating_add(elapsed.min(u32::MAX as u128) as u32)
                .min(Self::BURST);
            self.updated = Instant::now();
        }
        if self.tokens == 0 {
            return Err(Error::Exhausted);
        }
        self.tokens -= 1;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn control_messages_preserve_wire_tags_and_reject_malformed_fields() {
        let mut hello = vec![1];
        hello.extend_from_slice(&CONTROL_VERSION.to_be_bytes());
        hello.extend_from_slice(&[42; TICKET_BYTES]);
        assert_eq!(Message::Hello([42; TICKET_BYTES]).encode().unwrap(), hello);
        assert!(matches!(
            Message::decode(&hello).unwrap(),
            Message::Hello([42, ..])
        ));
        hello[1] ^= 1;
        assert!(matches!(Message::decode(&hello), Err(Error::Version)));

        for (message, tag) in [(Message::Ping(17), 2), (Message::Pong(17), 3)] {
            let mut expected = vec![tag];
            expected.extend_from_slice(&17u64.to_be_bytes());
            assert_eq!(message.encode().unwrap(), expected);
            assert!(matches!(
                Message::decode(&expected).unwrap(),
                Message::Ping(17) | Message::Pong(17)
            ));
            expected.pop();
            assert!(matches!(Message::decode(&expected), Err(Error::Invalid)));
            expected.extend_from_slice(&[0, 0]);
            assert!(matches!(Message::decode(&expected), Err(Error::Invalid)));
        }
        assert!(matches!(Message::decode(&[]), Err(Error::Invalid)));
        assert!(matches!(Message::decode(&[u8::MAX]), Err(Error::Invalid)));
    }

    #[test]
    fn events_require_a_correlated_nonzero_sequence() {
        let event = Event {
            kind: knightbus_core::EventKind::Inventory,
            sequence: 9,
            session: None,
        };
        let bytes = Message::Event(event).encode().unwrap();
        assert_eq!(bytes[0], 4);
        assert!(matches!(
            Message::decode(&bytes).unwrap(),
            Message::Event(Event { sequence: 9, .. })
        ));
        let mut forged = vec![4];
        forged.extend_from_slice(
            &knightbus_ipc::encode_response(&Response {
                id: 8,
                result: Ok(Reply::Event(Some(event))),
            })
            .unwrap(),
        );
        assert!(matches!(Message::decode(&forged), Err(Error::Invalid)));
        let zero = Message::Event(Event {
            sequence: 0,
            ..event
        })
        .encode()
        .unwrap();
        assert!(Message::decode(&zero).is_err());
    }

    #[test]
    fn management_rate_is_bounded_and_refills_without_growing_state() {
        let mut rate = Rate::new();
        for _ in 0..Rate::BURST {
            rate.admit().unwrap();
        }
        assert_eq!(rate.admit(), Err(Error::Exhausted));
        rate.updated = Instant::now() - Rate::REFILL;
        rate.admit().unwrap();
        assert_eq!(rate.admit(), Err(Error::Exhausted));
    }
}

#[cfg(test)]
mod framing_tests {
    use super::*;
    use std::net::{TcpListener, TcpStream};
    fn connected() -> (Framed, TcpStream) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let peer = TcpStream::connect(listener.local_addr().unwrap()).unwrap();
        (
            Framed::new(Stream::insecure_loopback(listener.accept().unwrap().0).unwrap()).unwrap(),
            peer,
        )
    }
    #[test]
    fn oversized_management_frames_are_rejected_before_reading_the_body() {
        let (mut wire, mut peer) = connected();
        peer.write_all(&u32::MAX.to_be_bytes()).unwrap();
        assert_eq!(wire.receive(TIMEOUT), Err(Error::Invalid));
        assert_eq!(wire.used, 4);
    }
    #[test]
    fn partial_management_frames_have_an_absolute_deadline() {
        let (mut wire, mut peer) = connected();
        peer.write_all(&[0]).unwrap();
        let end = Instant::now() + Duration::from_secs(1);
        while wire.used == 0 {
            assert!(Instant::now() < end);
            assert_eq!(wire.receive(Duration::ZERO).unwrap(), None);
        }
        wire.started = Some(Instant::now() - TIMEOUT);
        peer.write_all(&[0]).unwrap();
        assert_eq!(wire.receive(TIMEOUT), Err(Error::Deadline));
    }
}
