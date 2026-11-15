//! A single deadline and cancellation signal for a multi-step network operation.
use knightbus_core::{Error, Result};
use std::{
    net::{SocketAddr, TcpStream},
    time::{Duration, Instant},
};

const CANCELLATION_POLL_INTERVAL: Duration = Duration::from_millis(10);
const POLL_CAPACITY: usize = 2;

/// Borrowed operation scope; progress never extends its deadline.
pub struct Operation<'a> {
    deadline: Instant,
    cancelled: &'a dyn Fn() -> bool,
}
impl<'a> Operation<'a> {
    /// Bound all connection and bootstrap stages with the same clock and signal.
    pub fn new(deadline: Instant, cancelled: &'a dyn Fn() -> bool) -> Self {
        Self {
            deadline,
            cancelled,
        }
    }
    /// Fail before starting more work, including native publication.
    pub fn check(&self) -> Result<()> {
        if (self.cancelled)() {
            return Err(Error::Cancelled);
        }
        if Instant::now() >= self.deadline {
            return Err(Error::Deadline);
        }
        Ok(())
    }
    /// Cap socket waits at 10 ms so cancellation is checked promptly.
    pub(crate) fn wait(&self) -> Result<Duration> {
        self.check()?;
        Ok(self
            .deadline
            .saturating_duration_since(Instant::now())
            .min(CANCELLATION_POLL_INTERVAL))
    }
    /// Connect without blocking the worker for the OS's TCP timeout.
    pub fn connect(&self, address: SocketAddr) -> Result<TcpStream> {
        self.check()?;
        let mut socket = mio::net::TcpStream::connect(address).map_err(|_| Error::Unavailable)?;
        let mut poll = mio::Poll::new().map_err(|_| Error::Unavailable)?;
        let mut events = mio::Events::with_capacity(POLL_CAPACITY);
        poll.registry()
            .register(&mut socket, mio::Token(0), mio::Interest::WRITABLE)
            .map_err(|_| Error::Unavailable)?;
        loop {
            poll.poll(&mut events, Some(self.wait()?))
                .map_err(|_| Error::Unavailable)?;
            self.check()?;
            if socket
                .take_error()
                .map_err(|_| Error::Unavailable)?
                .is_some()
            {
                return Err(Error::Unavailable);
            }
            if socket.peer_addr().is_ok() {
                poll.registry()
                    .deregister(&mut socket)
                    .map_err(|_| Error::Unavailable)?;
                return Ok(socket.into());
            }
        }
    }
}
