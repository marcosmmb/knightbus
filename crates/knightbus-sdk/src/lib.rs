//! Standalone and embedded hosting of the existing KnightBus engine and native
//! backends. Management clients carry metadata only; USB streams stay in the host.
pub mod cli;
pub mod config;
#[cfg(feature = "mdns")]
pub mod discovery;
mod importer;
mod pairing;
pub mod physical;
mod policy;
pub use policy::{ATTACH_TIMEOUT_MS, CONTROL_POLL_INTERVAL, CONTROL_TIMEOUT_MS};
mod runtime;
pub use knightbus_core as core;
use knightbus_core::*;
pub use knightbus_ipc as ipc;
pub use runtime::{Endpoint, Runtime};
use std::{path::Path, time::Instant};

/// OS-authenticated local SDK client with negotiated host-clock deadlines.
pub struct Client {
    control: knightbus_ipc::local::Client,
    clock: HostClock,
}
/// A direct connection owns subscriptions and retains its host until closed.
pub struct Embedded {
    control: ControlConnection<Endpoint>,
    clock: HostClock,
}
struct HostClock {
    now: u64,
    sampled: Instant,
}
impl HostClock {
    fn negotiate(control: &mut impl KnightBusControl) -> Result<Self> {
        match control.call(Command::Info, u64::MAX)? {
            Reply::Info {
                version: CONTROL_VERSION,
                now,
                ..
            } => Ok(Self {
                now,
                sampled: Instant::now(),
            }),
            _ => Err(Error::Version),
        }
    }
    fn deadline(&self, timeout_ms: u64) -> u64 {
        // Adding elapsed time since negotiation completed is conservative: transport
        // transit consumes the caller's timeout rather than extending it.
        self.now
            .saturating_add(self.sampled.elapsed().as_millis() as u64)
            .saturating_add(timeout_ms)
    }
}
impl Client {
    /// Connect to a separately installed macOS root service. Its peer UID is verified.
    #[cfg(target_os = "macos")]
    pub fn connect_system(path: &Path) -> Result<Self> {
        let mut control = knightbus_ipc::local::Client::connect_system(path)?;
        let clock = HostClock::negotiate(&mut control)?;
        Ok(Self { control, clock })
    }
    /// Connect and negotiate before exposing mutations. There is no automatic replay.
    pub fn connect(path: &Path) -> Result<Self> {
        let mut control = knightbus_ipc::local::Client::connect(path)?;
        let clock = HostClock::negotiate(&mut control)?;
        Ok(Self { control, clock })
    }
    /// Call with a relative timeout in milliseconds; detach cancels an accepted attach.
    pub fn request(&mut self, command: Command, timeout_ms: u64) -> Result<Reply> {
        self.control.call(command, self.clock.deadline(timeout_ms))
    }
}
impl Embedded {
    /// Bind a local administrator to the same runtime hosted by knightbusd.
    /// This is process authority, not a remote identity supplied by a request.
    pub fn connect(runtime: &Runtime) -> Result<Self> {
        let mut control =
            ControlConnection::new(runtime.endpoint(), runtime::ADMIN, CONTROL_VERSION)?;
        let clock = HostClock::negotiate(&mut control)?;
        Ok(Self { control, clock })
    }
    /// Call with the same timeout and cancellation semantics as the IPC SDK.
    pub fn request(&mut self, command: Command, timeout_ms: u64) -> Result<Reply> {
        self.control.call(command, self.clock.deadline(timeout_ms))
    }
}
impl KnightBusControl for Client {
    fn call(&mut self, command: Command, deadline: u64) -> Result<Reply> {
        self.control.call(command, deadline)
    }
}
impl KnightBusControl for Embedded {
    fn call(&mut self, command: Command, deadline: u64) -> Result<Reply> {
        self.control.call(command, deadline)
    }
}
/// Actionable, payload-free guidance for stable control errors.
pub fn guidance(error: Error) -> &'static str {
    match error {
        Error::Invalid => "Check command fields, device generation, and configuration syntax.",
        Error::State => "Refresh session status; wait for native retirement before retrying.",
        Error::Unsupported => "This OS, backend, descriptor topology, or operation is unsupported.",
        Error::Unavailable => {
            "Check the daemon, network, and native driver. An enumerated USB interface may still be owned by an OS driver; inspect platform access permissions."
        }
        Error::Denied => {
            "Check endpoint ownership, paired certificate fingerprints, exporter grants, and capture permissions."
        }
        Error::NotFound => "The selected resource is absent; list again after removal or replug.",
        Error::Version => "Install matching daemon, SDK and embedded library control versions.",
        Error::Deadline => "The deadline expired; inspect status before retrying a mutation.",
        Error::Cancelled => "The operation was cancelled; wait for device release.",
        Error::Exhausted => {
            "A bounded queue, descriptor reply, or resource budget is full; reduce concurrency."
        }
        Error::Internal => {
            "The host encountered an internal failure; collect metadata diagnostics."
        }
        Error::Cleanup => {
            "Native cleanup has not completed; the device remains quarantined. Do not recapture it."
        }
    }
}

/// Idle-capable service administration using the common control contract.
pub mod service;

/// Installed macOS service endpoint, protected by the root installer.
#[cfg(target_os = "macos")]
pub const SYSTEM_ENDPOINT: &str = "/var/run/knightbus/control";
/// Connect to the installed system service or an explicitly selected same-account endpoint.
pub fn connect_local(path: &Path) -> Result<Client> {
    #[cfg(target_os = "macos")]
    if path == Path::new(SYSTEM_ENDPOINT) {
        return Client::connect_system(path);
    }
    Client::connect(path)
}
