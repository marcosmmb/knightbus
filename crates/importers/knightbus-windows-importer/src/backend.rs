//! Windows channel encoding for the importer worker.
use crate::{
    channel::{
        self, ABI_VERSION, HEADER, MAX_TRANSFER, Request, SLOTS, STATS_BYTES, stats, transfer,
        u64_at,
    },
    notification::Notification,
};
use knightbus_core::{
    Budget, Completion, DescriptorSet, Error, ImporterBackend, Result, importer, usbip::Direction,
};
use std::sync::Arc;
/// Synchronous native channel, used only on the backend's dedicated worker.
/// An implementation must close/drain its native handle when dropped.
pub trait Driver: Send + 'static {
    /// Publish validated descriptors. Failure still requires `detach`/Drop.
    fn attach(&mut self, bytes: &[u8]) -> Result<()>;
    /// Nonblocking fetch; zero means no request is currently available.
    fn fetch(&mut self, mask: u32, output: &mut [u8]) -> Result<usize>;
    /// Complete one token, or acknowledge an already canceled token.
    fn reply(&mut self, input: &[u8]) -> Result<()>;
    /// Snapshot native token liveness, used to deliver cancellation once.
    fn snapshot(&mut self, output: &mut [u8]) -> Result<()>;
    /// Signal existing work, then arrivals and cancellations, until detach.
    fn notifications(&mut self, wake: Arc<crate::DriverWake>) -> Result<()>;
    /// Retire the child and native requests. Error means cleanup is not proven.
    fn detach(&mut self) -> Result<()>;
}

/// Connect the Windows channel to USB/IP discovery and forwarding.
pub fn connect<D: Driver>(
    stream: std::net::TcpStream,
    bus: knightbus_core::usbip::WireString<32>,
    session: knightbus_core::SessionId,
    budget: Budget,
    timeout: std::time::Duration,
    factory: impl FnOnce() -> Result<D> + Send + 'static,
) -> Result<knightbus_transport::importer::Forwarder<impl ImporterBackend>> {
    knightbus_transport::importer::connect(stream, bus, session, budget, timeout, move |_| {
        Ok(Channel(factory()?))
    })
}

/// Bind the UDE channel to the importer worker and its admission budget.
pub fn new<D: Driver>(
    factory: impl FnOnce() -> Result<D> + Send + 'static,
    descriptors: DescriptorSet,
    budget: Budget,
) -> Result<impl ImporterBackend> {
    // Reject unsupported native descriptors before starting any worker.
    crate::descriptors::validate(
        descriptors.device(),
        descriptors.configuration(),
        descriptors.speed(),
    )
    .map_err(|_| Error::Unsupported)?;
    importer::Backend::new(move || Ok(Channel(factory()?)), descriptors, budget)
}
/// Features implemented by the Windows native channel.
pub const CAPABILITIES: knightbus_core::Capabilities = knightbus_core::Capabilities {
    transfer_bytes: 1024 * 1024,
    requests: 31,
    iso_packets: 0,
    speeds: 0b1101110,
    descriptor_bytes: 4096,
    configurations: 1,
    remote_wakeup: false,
};

struct Channel<D>(D);
impl<D: Driver> importer::Driver for Channel<D> {
    const CAPABILITIES: knightbus_core::Capabilities = CAPABILITIES;
    type Signal = Notification;
    const NATIVE_BYTES: usize = SLOTS * (MAX_TRANSFER + 256) + 80 * 1024;
    const SCRATCH_BYTES: usize = HEADER + MAX_TRANSFER;
    fn attach(&mut self, descriptors: &DescriptorSet, budget: &Budget) -> Result<()> {
        let native = channel::Descriptors::from_descriptor_set(budget, descriptors)?;
        self.0.attach(native.bytes.as_slice())
    }
    fn fetch<'a>(
        &mut self,
        mask: u32,
        topology: &knightbus_core::topology::Topology,
        scratch: &'a mut [u8],
    ) -> Result<Option<importer::Request<'a>>> {
        let count = self.0.fetch(mask, scratch)?;
        if count > scratch.len() {
            return Err(Error::Invalid);
        }
        if count == 0 {
            return Ok(None);
        }
        let r = Request::decode(&scratch[..count], topology)?;
        Ok(Some(importer::Request {
            token: r.token,
            endpoint: r.endpoint,
            length: r.length,
            setup: r.setup,
            payload: r.payload,
        }))
    }
    fn complete(&mut self, token: u64, done: &Completion, scratch: &mut [u8]) -> Result<()> {
        let header = done.transfer.header();
        let endpoint = header.endpoint as u8
            | if header.direction == Direction::In && header.endpoint != 0 {
                0x80
            } else {
                0
            };
        let (status, actual) = if done.status == 0 {
            (transfer::SUCCESS, done.actual_length)
        } else {
            (
                if done.status == knightbus_core::usbip::status::PIPE {
                    transfer::STALL
                } else {
                    transfer::FAILURE
                },
                0,
            )
        };
        let payload = if header.direction == Direction::In {
            actual as usize
        } else {
            0
        };
        let bytes = scratch.get_mut(..HEADER + payload).ok_or(Error::Invalid)?;
        bytes[..HEADER].fill(0);
        for (offset, value) in [
            (transfer::VERSION, ABI_VERSION),
            (transfer::ENDPOINT, endpoint as u32),
            (transfer::LENGTH, actual),
            (transfer::PAYLOAD_LENGTH, payload as u32),
            (transfer::STATUS, status),
        ] {
            bytes[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
        }
        bytes[transfer::TOKEN..transfer::TOKEN + 8].copy_from_slice(&token.to_le_bytes());
        bytes[HEADER..].copy_from_slice(&done.transfer.buffer()[..payload]);
        self.0.reply(bytes)
    }
    fn snapshot(&mut self) -> Result<importer::Snapshot> {
        let mut bytes = [0; STATS_BYTES];
        self.0.snapshot(&mut bytes)?;
        if u64_at(&bytes, stats::VERSION) != u64::from(ABI_VERSION) {
            return Err(Error::Version);
        }
        Ok(importer::Snapshot {
            live: u64_at(&bytes, stats::LIVE_MASK),
            tokens: std::array::from_fn(|i| u64_at(&bytes, stats::TOKENS + i * 8)),
        })
    }
    fn notifications(&mut self, wake: Arc<Notification>) -> Result<()> {
        self.0.notifications(wake)
    }
    fn detach(&mut self) -> Result<()> {
        self.0.detach()
    }
}
impl importer::Signal for Notification {
    fn new(budget: &Budget) -> Result<Self> {
        Notification::new(budget)
    }
    fn notify(&self) {
        Notification::notify(self)
    }
    fn wait(&self) -> Result<()> {
        Notification::wait(self)
    }
}
