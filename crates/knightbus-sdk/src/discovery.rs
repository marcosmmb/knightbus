//! Optional, untrusted LAN address hints. Discovery never pairs, grants, imports,
//! or fetches USB descriptors. Explicit fingerprint confirmation and TLS still apply.
use knightbus_core::{CONTROL_VERSION, Error, Result};
use mdns_sd::{DaemonEvent, Receiver, ServiceDaemon, ServiceEvent, ServiceInfo};
use std::{
    net::SocketAddr,
    time::{Duration, Instant},
};
const MAX_BROWSE_DURATION: Duration = Duration::from_secs(10);
const MAX_HINTS: usize = 64;
const SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(1);
const SERVICE: &str = "_knightbus._tcp.local.";
/// Unauthenticated addresses to copy into a configuration after verifying a peer.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct Hint {
    /// Advertised control address, never proof of identity.
    pub control: SocketAddr,
    /// Advertised USB data address, authenticated independently before use.
    pub usb: SocketAddr,
}
/// Browse for at most ten seconds, returning at most 64 unique IPv4 address pairs.
/// IPv6/manual addresses remain usable through explicit configuration.
pub fn browse(duration: Duration) -> Result<Vec<Hint>> {
    if duration.is_zero() || duration > MAX_BROWSE_DURATION {
        return Err(Error::Invalid);
    }
    let daemon = ServiceDaemon::new().map_err(|_| Error::Unavailable)?;
    let monitor = daemon.monitor().map_err(|_| Error::Unavailable)?;
    let guard = Announcement(daemon, monitor);
    let receiver = guard.0.browse(SERVICE).map_err(|_| Error::Unavailable)?;
    let end = Instant::now() + duration;
    let mut hints = std::collections::BTreeSet::new();
    let version = CONTROL_VERSION.to_string();
    while Instant::now() < end && hints.len() < MAX_HINTS {
        let Ok(event) = receiver.recv_timeout(end.saturating_duration_since(Instant::now())) else {
            break;
        };
        if let ServiceEvent::ServiceResolved(info) = event {
            if info.get_property_val_str("version") != Some(version.as_str())
                || info.get_port() == 0
            {
                continue;
            }
            let Some(port) = info
                .get_property_val_str("usb")
                .and_then(|s| s.parse::<u16>().ok())
                .filter(|p| *p != 0)
            else {
                continue;
            };
            for address in info.get_addresses_v4() {
                if address.is_unspecified() || address.is_multicast() || address.is_broadcast() {
                    continue;
                }
                hints.insert(Hint {
                    control: (address, info.get_port()).into(),
                    usb: (address, port).into(),
                });
                if hints.len() == MAX_HINTS {
                    break;
                }
            }
        }
    }
    guard.check()?;
    Ok(hints.into_iter().collect())
}
pub(crate) struct Announcement(ServiceDaemon, Receiver<DaemonEvent>);
impl Announcement {
    pub fn check(&self) -> Result<()> {
        while let Ok(event) = self.1.try_recv() {
            if matches!(event, DaemonEvent::Error(_)) {
                return Err(Error::Unavailable);
            }
        }
        Ok(())
    }
    pub fn new(name: &str, usb: SocketAddr, control: SocketAddr) -> Result<Self> {
        if usb.ip() != control.ip()
            || usb.ip().is_unspecified()
            || usb.ip().is_loopback()
            || usb.port() == 0
            || control.port() == 0
        {
            return Err(Error::Invalid);
        }
        let daemon = ServiceDaemon::new().map_err(|_| Error::Unavailable)?;
        let monitor = daemon.monitor().map_err(|_| Error::Unavailable)?;
        let instance = Self(daemon, monitor);
        let info = ServiceInfo::new(
            SERVICE,
            name,
            &format!("{name}.local."),
            usb.ip(),
            control.port(),
            &[
                ("version", CONTROL_VERSION.to_string()),
                ("usb", usb.port().to_string()),
            ][..],
        )
        .map_err(|_| Error::Invalid)?;
        instance.0.register(info).map_err(|_| Error::Unavailable)?;
        Ok(instance)
    }
}
impl Drop for Announcement {
    fn drop(&mut self) {
        if let Ok(receiver) = self.0.shutdown() {
            let _ = receiver.recv_timeout(SHUTDOWN_TIMEOUT);
        }
    }
}
