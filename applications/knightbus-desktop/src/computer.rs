//! Read-only defaults for the local computer; user overrides stay in the UI.
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, UdpSocket};

const MAX_NAME_CHARACTERS: usize = 64;
// Documentation-only destinations select a route. UDP connect sends no packets.
const ROUTE_PROBE_PORT: u16 = 9;
const IPV4_ROUTE_PROBE: Ipv4Addr = Ipv4Addr::new(192, 0, 2, 1);
const IPV6_ROUTE_PROBE: Ipv6Addr = Ipv6Addr::new(0x2001, 0xdb8, 0, 0, 0, 0, 0, 1);

fn clean_name(value: String) -> String {
    value
        .trim()
        .chars()
        .filter(|c| !c.is_control())
        .take(MAX_NAME_CHARACTERS)
        .collect()
}
pub fn name() -> String {
    #[cfg(target_os = "macos")]
    let value = std::process::Command::new("/usr/sbin/scutil")
        .args(["--get", "ComputerName"])
        .output()
        .ok()
        .filter(|result| result.status.success())
        .map(|result| String::from_utf8_lossy(&result.stdout).into_owned())
        .unwrap_or_default();
    #[cfg(windows)]
    let value = std::env::var("COMPUTERNAME").unwrap_or_default();
    #[cfg(not(any(target_os = "macos", windows)))]
    let value = std::fs::read_to_string("/etc/hostname").unwrap_or_default();
    clean_name(value)
}
pub fn address() -> String {
    for (bind, remote) in [
        (
            IpAddr::V4(Ipv4Addr::UNSPECIFIED),
            IpAddr::V4(IPV4_ROUTE_PROBE),
        ),
        (
            IpAddr::V6(Ipv6Addr::UNSPECIFIED),
            IpAddr::V6(IPV6_ROUTE_PROBE),
        ),
    ] {
        let Ok(socket) = UdpSocket::bind((bind, 0)) else {
            continue;
        };
        if socket.connect((remote, ROUTE_PROBE_PORT)).is_err() {
            continue;
        }
        if let Ok(local) = socket.local_addr() {
            let ip = local.ip();
            if !ip.is_loopback() && !ip.is_unspecified() {
                return ip.to_string();
            }
        }
    }
    String::new()
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn names_are_bounded_and_control_characters_removed() {
        assert_eq!(clean_name("  User’s Mac\n".into()), "User’s Mac");
        assert_eq!(
            clean_name("a".repeat(MAX_NAME_CHARACTERS + 1)).len(),
            MAX_NAME_CHARACTERS
        );
        assert_eq!(clean_name("bad\0name".into()), "badname");
    }
}
