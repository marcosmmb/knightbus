//! Optional, untrusted LAN address hints. Discovery never pairs, grants, imports,
//! or fetches USB descriptors. Explicit fingerprint confirmation and TLS still apply.
use knightbus_core::{CONTROL_VERSION, Error, Result};
use mdns_sd::{DaemonEvent, Receiver, ResolvedService, ServiceDaemon, ServiceEvent, ServiceInfo};
use std::{
    net::SocketAddr,
    time::{Duration, Instant},
};
const MAX_BROWSE_DURATION: Duration = Duration::from_secs(10);
const MAX_HINTS: usize = 64;
const SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(1);
const SERVICE: &str = "_knightbus._tcp.local.";
// Computer presence is distinct from an active USB exporter listener.
const COMPUTER_SERVICE: &str = "_knightbus-host._tcp.local.";
const INSTANCE_FINGERPRINT_HEX_LENGTH: usize = knightbus_core::PEER_ID_BYTES;
use knightbus_core::host::{MAX_COMPUTER_NAME_BYTES, MAX_NEARBY_COMPUTERS, NearbyComputer};
use knightbus_transport::security::parse_fingerprint;

fn valid_name(name: &str) -> bool {
    !name.trim().is_empty()
        && name.len() <= MAX_COMPUTER_NAME_BYTES
        && !name.chars().any(char::is_control)
}

fn computer_info(
    name: &str,
    fingerprint: &str,
    usb: SocketAddr,
    control: SocketAddr,
    pairing_port: Option<u16>,
) -> Result<ServiceInfo> {
    validate_addresses(usb, control)?;
    if !valid_name(name) {
        return Err(Error::Invalid);
    }
    parse_fingerprint(fingerprint)?;
    // DNS labels have limited space; TXT retains the complete fingerprint.
    let instance = format!(
        "knightbus-{}",
        &fingerprint[..INSTANCE_FINGERPRINT_HEX_LENGTH]
    );
    let mut properties = vec![
        ("version", CONTROL_VERSION.to_string()),
        ("usb", usb.port().to_string()),
        ("name", name.to_owned()),
        ("fingerprint", fingerprint.to_owned()),
    ];
    if let Some(port) = pairing_port.filter(|p| *p != 0) {
        properties.push(("pairing", port.to_string()));
    }
    ServiceInfo::new(
        COMPUTER_SERVICE,
        &instance,
        &format!("{instance}.local."),
        usb.ip(),
        control.port(),
        &properties[..],
    )
    .map_err(|_| Error::Invalid)
}

fn resolved_computers(info: &ResolvedService) -> Vec<NearbyComputer> {
    let Some(name) = info.get_property_val_str("name").filter(|s| valid_name(s)) else {
        return vec![];
    };
    let Some(fingerprint) = info
        .get_property_val_str("fingerprint")
        .filter(|s| parse_fingerprint(s).is_ok())
    else {
        return vec![];
    };
    if info.get_property_val_str("version") != Some(CONTROL_VERSION.to_string().as_str())
        || info.get_port() == 0
    {
        return vec![];
    }
    let Some(port) = info
        .get_property_val_str("usb")
        .and_then(|s| s.parse::<u16>().ok())
        .filter(|p| *p != 0)
    else {
        return vec![];
    };
    info.get_addresses_v4()
        .into_iter()
        .filter(|ip| {
            !ip.is_loopback() && !ip.is_unspecified() && !ip.is_multicast() && !ip.is_broadcast()
        })
        .take(MAX_NEARBY_COMPUTERS)
        .map(|ip| NearbyComputer {
            name: name.to_owned(),
            fingerprint: fingerprint.to_lowercase(),
            usb: SocketAddr::from((ip, port)).to_string(),
            control: SocketAddr::from((ip, info.get_port())).to_string(),
            pairing_port: info
                .get_property_val_str("pairing")
                .and_then(|s| s.parse::<u16>().ok())
                .filter(|p| *p != 0),
        })
        .collect()
}

/// Discover bounded, unverified computer presence without selecting a USB role.
pub fn computers(duration: Duration) -> Result<Vec<NearbyComputer>> {
    if duration.is_zero() || duration > MAX_BROWSE_DURATION {
        return Err(Error::Invalid);
    }
    let daemon = ServiceDaemon::new().map_err(|_| Error::Unavailable)?;
    let monitor = daemon.monitor().map_err(|_| Error::Unavailable)?;
    let guard = Announcement(daemon, monitor);
    let receiver = guard
        .0
        .browse(COMPUTER_SERVICE)
        .map_err(|_| Error::Unavailable)?;
    let end = Instant::now() + duration;
    let mut computers = std::collections::BTreeMap::new();
    while Instant::now() < end && computers.len() < MAX_NEARBY_COMPUTERS {
        let Ok(event) = receiver.recv_timeout(end.saturating_duration_since(Instant::now())) else {
            break;
        };
        if let ServiceEvent::ServiceResolved(info) = event {
            for computer in resolved_computers(&info) {
                // A TXT update replaces the old PIN port/name for this address.
                computers.insert(
                    (computer.fingerprint.clone(), computer.control.clone()),
                    computer,
                );
                if computers.len() == MAX_NEARBY_COMPUTERS {
                    break;
                }
            }
        }
    }
    guard.check()?;
    Ok(computers.into_values().collect())
}

fn validate_addresses(usb: SocketAddr, control: SocketAddr) -> Result<()> {
    if usb.ip() != control.ip()
        || usb.ip().is_unspecified()
        || usb.ip().is_loopback()
        || usb.ip().is_multicast()
        || usb.port() == 0
        || control.port() == 0
        || matches!(usb.ip(), std::net::IpAddr::V4(ip) if ip.is_broadcast())
    {
        return Err(Error::Invalid);
    }
    Ok(())
}
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
    pub fn computer(
        name: &str,
        fingerprint: &str,
        usb: SocketAddr,
        control: SocketAddr,
        pairing_port: Option<u16>,
    ) -> Result<Self> {
        let info = computer_info(name, fingerprint, usb, control, pairing_port)?;
        let daemon = ServiceDaemon::new().map_err(|_| Error::Unavailable)?;
        let monitor = daemon.monitor().map_err(|_| Error::Unavailable)?;
        let instance = Self(daemon, monitor);
        instance.0.register(info).map_err(|_| Error::Unavailable)?;
        Ok(instance)
    }
    pub fn update_computer(
        &self,
        name: &str,
        fingerprint: &str,
        usb: SocketAddr,
        control: SocketAddr,
        pairing_port: Option<u16>,
    ) -> Result<()> {
        self.check()?;
        self.0
            .register(computer_info(
                name,
                fingerprint,
                usb,
                control,
                pairing_port,
            )?)
            .map_err(|_| Error::Unavailable)
    }
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

#[cfg(test)]
mod tests {
    use super::*;
    fn presence() -> ServiceInfo {
        computer_info(
            "Hagrid",
            &"a".repeat(knightbus_core::PEER_ID_BYTES * 2),
            "192.168.1.20:3337".parse().unwrap(),
            "192.168.1.20:3338".parse().unwrap(),
            None,
        )
        .unwrap()
    }
    #[test]
    fn idle_presence_has_only_public_computer_metadata() {
        let info = presence();
        let records = resolved_computers(&info.clone().as_resolved_service());
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].name, "Hagrid");
        assert_eq!(records[0].usb, "192.168.1.20:3337");
        assert_eq!(records[0].control, "192.168.1.20:3338");
        assert_eq!(info.get_properties().len(), 4);
        assert!(info.get_property_val_str("certificate").is_none());
        assert!(info.get_property_val_str("devices").is_none());
    }
    #[test]
    fn rejects_malformed_untrusted_identity_and_address_records() {
        for properties in [
            vec![
                ("name", "Hagrid"),
                ("version", "2"),
                ("usb", "3337"),
                ("fingerprint", "not-a-fingerprint"),
            ],
            vec![
                ("name", "bad\nname"),
                ("version", "2"),
                ("usb", "3337"),
                ("fingerprint", &"a".repeat(64)),
            ],
            vec![
                ("name", "Hagrid"),
                ("version", "0"),
                ("usb", "3337"),
                ("fingerprint", &"a".repeat(64)),
            ],
            vec![
                ("name", "Hagrid"),
                ("version", "2"),
                ("usb", "0"),
                ("fingerprint", &"a".repeat(64)),
            ],
        ] {
            let info = ServiceInfo::new(
                COMPUTER_SERVICE,
                "test",
                "test.local.",
                "192.168.1.20",
                3338,
                &properties[..],
            )
            .unwrap();
            assert!(resolved_computers(&info.as_resolved_service()).is_empty());
        }
        let info = ServiceInfo::new(
            COMPUTER_SERVICE,
            "test",
            "test.local.",
            "127.0.0.1",
            3338,
            presence()
                .get_properties()
                .iter()
                .cloned()
                .collect::<Vec<_>>(),
        )
        .unwrap();
        assert!(resolved_computers(&info.as_resolved_service()).is_empty());
    }
    #[test]
    fn invalid_local_presence_cannot_start_advertising() {
        let usb = "192.168.1.20:3337".parse().unwrap();
        let control = "192.168.1.20:3338".parse().unwrap();
        let fingerprint = "a".repeat(64);
        for name in [
            String::new(),
            "bad\nname".into(),
            "x".repeat(MAX_COMPUTER_NAME_BYTES + 1),
        ] {
            assert!(computer_info(&name, &fingerprint, usb, control, None).is_err());
        }
        for address in [
            "0.0.0.0:3337",
            "127.0.0.1:3337",
            "224.0.0.251:3337",
            "255.255.255.255:3337",
            "192.168.1.20:0",
        ] {
            assert!(
                computer_info(
                    "Hagrid",
                    &fingerprint,
                    address.parse().unwrap(),
                    control,
                    None
                )
                .is_err()
            );
        }
    }
}
