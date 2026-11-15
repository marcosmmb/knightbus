use crate::operation::Operation;
use crate::security::{Channel, Identity, Peers};
use knightbus_core::{Caller, Error, PeerId, Result};
use std::{
    io::{self, Read, Write},
    net::{Shutdown, TcpStream},
    time::{Duration, Instant},
};

const SOCKET: mio::Token = mio::Token(0);
const POLL_CAPACITY: usize = 2;
const TLS_BUFFER_BYTES: usize = 64 * 1024;
const TLS_IO_STEPS: usize = 16;
/// Aggregate budget reservation for TLS buffers and handshake state per connection.
pub(crate) const TLS_STORAGE_BYTES: usize = 512 * 1024;

/// Raw compatibility stream or an authenticated encrypted stream.
/// Secure construction completes authentication before exposing any USB/IP bytes.
/// A plain socket cannot become a forwarding stream implicitly:
/// ```compile_fail
/// use knightbus_transport::Stream;
/// fn accidental_plaintext(socket: std::net::TcpStream) -> Stream {
///     socket.into()
/// }
/// ```
pub struct Stream {
    pub(crate) socket: mio::net::TcpStream,
    tls: Option<Box<rustls::Connection>>,
    caller: Option<Caller>,
}
impl Stream {
    /// Explicit unencrypted compatibility connection. Both addresses must be loopback;
    /// the caller must provide an authenticated tunnel for any remote peer.
    pub fn insecure_loopback(stream: TcpStream) -> Result<Self> {
        if !stream
            .peer_addr()
            .map_err(|_| Error::Unavailable)?
            .ip()
            .is_loopback()
            || !stream
                .local_addr()
                .map_err(|_| Error::Unavailable)?
                .ip()
                .is_loopback()
        {
            return Err(Error::Denied);
        }
        stream
            .set_nonblocking(true)
            .map_err(|_| Error::Unavailable)?;
        Ok(Self {
            socket: mio::net::TcpStream::from_std(stream),
            tls: None,
            caller: None,
        })
    }
    /// Authenticate an exporter using an explicitly paired certificate.
    pub fn connect(
        stream: TcpStream,
        identity: &Identity,
        peers: &Peers,
        peer: PeerId,
        channel: Channel,
        timeout: Duration,
    ) -> Result<Self> {
        if timeout.is_zero() {
            return Err(Error::Invalid);
        }
        let operation = Operation::new(
            Instant::now().checked_add(timeout).ok_or(Error::Invalid)?,
            &|| false,
        );
        Self::connect_until(stream, identity, peers, peer, channel, &operation)
    }
    /// Authenticate within the caller's shared connection/bootstrap operation.
    pub fn connect_until(
        stream: TcpStream,
        identity: &Identity,
        peers: &Peers,
        peer: PeerId,
        channel: Channel,
        operation: &Operation<'_>,
    ) -> Result<Self> {
        operation.check()?;
        let tls = peers.client(identity, peer, channel)?;
        let stream = Self::handshake(stream, tls, peers, channel, operation)?;
        if stream.caller.map(|c| c.peer) != Some(peer) {
            return Err(Error::Denied);
        }
        Ok(stream)
    }
    /// Require a paired importer certificate. Remote callers never become administrators.
    pub fn accept(
        stream: TcpStream,
        identity: &Identity,
        peers: &Peers,
        channel: Channel,
        timeout: Duration,
    ) -> Result<Self> {
        if timeout.is_zero() {
            return Err(Error::Invalid);
        }
        Self::handshake(
            stream,
            peers.server(identity, channel)?,
            peers,
            channel,
            &Operation::new(
                Instant::now().checked_add(timeout).ok_or(Error::Invalid)?,
                &|| false,
            ),
        )
    }
    fn handshake(
        stream: TcpStream,
        mut tls: rustls::Connection,
        peers: &Peers,
        channel: Channel,
        operation: &Operation<'_>,
    ) -> Result<Self> {
        operation.check()?;
        stream
            .set_nonblocking(true)
            .map_err(|_| Error::Unavailable)?;
        stream.set_nodelay(true).map_err(|_| Error::Unavailable)?;
        tls.set_buffer_limit(Some(TLS_BUFFER_BYTES));
        let mut stream = Self {
            socket: mio::net::TcpStream::from_std(stream),
            tls: Some(Box::new(tls)),
            caller: None,
        };
        let mut poll = mio::Poll::new().map_err(|_| Error::Unavailable)?;
        let mut events = mio::Events::with_capacity(POLL_CAPACITY);
        poll.registry()
            .register(
                &mut stream.socket,
                SOCKET,
                mio::Interest::READABLE | mio::Interest::WRITABLE,
            )
            .map_err(|_| Error::Unavailable)?;
        loop {
            operation.check()?;
            stream.pump().map_err(|_| Error::Denied)?;
            if !stream.tls.as_ref().unwrap().is_handshaking() && !stream.pending_write() {
                break;
            }
            let interests = if stream.pending_write() {
                mio::Interest::READABLE | mio::Interest::WRITABLE
            } else {
                mio::Interest::READABLE
            };
            poll.registry()
                .reregister(&mut stream.socket, SOCKET, interests)
                .map_err(|_| Error::Unavailable)?;
            poll.poll(&mut events, Some(operation.wait()?))
                .map_err(|_| Error::Unavailable)?;
        }
        operation.check()?;
        stream.caller = Some(peers.verify(stream.tls.as_ref().unwrap(), channel)?);
        poll.registry()
            .deregister(&mut stream.socket)
            .map_err(|_| Error::Unavailable)?;
        Ok(stream)
    }
    /// Authentication result, absent for explicitly raw compatibility streams.
    pub fn caller(&self) -> Option<Caller> {
        self.caller
    }
    pub(crate) fn storage_bytes(&self) -> usize {
        if self.tls.is_some() {
            TLS_STORAGE_BYTES
        } else {
            0
        }
    }
    pub(crate) fn pending_write(&self) -> bool {
        self.tls.as_ref().is_some_and(|tls| tls.wants_write())
    }
    pub(crate) fn close(&mut self) {
        let _ = self.socket.shutdown(Shutdown::Both);
    }
    // Bound each turn, including malicious handshakes and TLS key-update traffic.
    pub(crate) fn pump(&mut self) -> io::Result<bool> {
        let Some(tls) = &mut self.tls else {
            return Ok(false);
        };
        let mut progress = false;
        for _ in 0..TLS_IO_STEPS {
            let mut step = false;
            if tls.wants_write() {
                match tls.write_tls(&mut self.socket) {
                    Ok(0) => return Err(io::ErrorKind::WriteZero.into()),
                    Ok(_) => step = true,
                    Err(e) if e.kind() == io::ErrorKind::WouldBlock => {}
                    Err(e) => return Err(e),
                }
            }
            if tls.wants_read() {
                match tls.read_tls(&mut self.socket) {
                    Ok(0) => return Err(io::ErrorKind::UnexpectedEof.into()),
                    Ok(_) => {
                        let state = tls
                            .process_new_packets()
                            .map_err(|_| io::ErrorKind::InvalidData)?;
                        if state.plaintext_bytes_to_read() != 0 {
                            return Ok(true);
                        }
                        step = true;
                    }
                    Err(e) if e.kind() == io::ErrorKind::WouldBlock => {}
                    Err(e) => return Err(e),
                }
            }
            progress |= step;
            if !step {
                break;
            }
        }
        Ok(progress)
    }
}
impl Read for Stream {
    fn read(&mut self, output: &mut [u8]) -> io::Result<usize> {
        if self.tls.is_none() {
            return self.socket.read(output);
        }
        // Drain decrypted bytes before reading another TLS record.
        match self.tls.as_mut().unwrap().reader().read(output) {
            Err(e) if e.kind() == io::ErrorKind::WouldBlock => {}
            result => return result,
        }
        self.pump()?;
        self.tls.as_mut().unwrap().reader().read(output)
    }
}
impl Write for Stream {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        if self.tls.is_none() {
            return self.socket.write(bytes);
        }
        self.pump()?;
        let count = self
            .tls
            .as_mut()
            .unwrap()
            .writer()
            .write(&bytes[..bytes.len().min(TLS_BUFFER_BYTES)])?;
        if count == 0 && !bytes.is_empty() {
            return Err(io::ErrorKind::WouldBlock.into());
        }
        // Once accepted, report plaintext consumption even if the socket blocks.
        Ok(count)
    }
    fn flush(&mut self) -> io::Result<()> {
        self.pump()?;
        if self.pending_write() {
            Err(io::ErrorKind::WouldBlock.into())
        } else {
            Ok(())
        }
    }
}
impl Drop for Stream {
    fn drop(&mut self) {
        self.close();
    }
}
