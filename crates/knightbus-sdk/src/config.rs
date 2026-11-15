//! Explicit host configuration. Unknown and duplicate settings fail closed.
use knightbus_core::{Error, PeerId, Result};
use std::{
    collections::BTreeMap,
    net::SocketAddr,
    path::{Path, PathBuf},
};

/// Maximum UTF-8 configuration size, shared with C ABI validation.
pub const MAX_CONFIG_BYTES: usize = 16 * 1024;
const MAX_STARTUP_GRANTS: usize = 64;
const MAX_MDNS_NAME_BYTES: usize = 48;

/// One native role and its independently authorized transport configuration.
#[derive(Clone, Debug)]
pub struct Configuration {
    /// Native role.
    pub role: Role,
    /// Protected identity and enrollment directory.
    pub state: PathBuf,
    /// USB data listener (exporter) or destination (importer).
    pub usb: SocketAddr,
    /// Authenticated control listener or destination.
    pub control: SocketAddr,
    /// Opt-in exporter DNS-SD instance name; contains no inventory or identity.
    pub mdns_name: Option<String>,
}
/// Platform-specific consent and selection; identities are certificate fingerprints.
#[derive(Clone, Debug)]
pub enum Role {
    /// One physical macOS generation, initially unshared unless grants are explicit.
    Exporter {
        /// IORegistry generation, never VID/PID.
        registry: u64,
        /// Explicit consent to capture when an authorized peer imports.
        capture: bool,
        /// Operator confirmed every storage volume was cleanly unmounted.
        storage_unmounted: bool,
        /// Explicit startup grants; live grants remain ephemeral.
        grants: Vec<PeerId>,
    },
    /// Native Windows importer. Starting the host does not attach a device.
    Importer {
        /// Expected exporter certificate fingerprint, already paired in state.
        peer: PeerId,
    },
}
impl Configuration {
    /// Read a bounded UTF-8 configuration. It contains paths and public identities,
    /// never private keys. Key material is opened through the protected Store.
    pub fn read(path: &Path) -> Result<Self> {
        use std::io::Read;
        let mut text = String::new();
        std::fs::File::open(path)
            .map_err(|_| Error::NotFound)?
            .take((MAX_CONFIG_BYTES + 1) as u64)
            .read_to_string(&mut text)
            .map_err(|_| Error::Invalid)?;
        Self::parse(&text)
    }
    /// Parse strict `key=value` lines. Repeat only `grant`; `#` begins a whole-line comment.
    pub fn parse(text: &str) -> Result<Self> {
        if text.len() > MAX_CONFIG_BYTES {
            return Err(Error::Exhausted);
        }
        let mut fields = BTreeMap::new();
        let mut grants = Vec::new();
        for line in text
            .lines()
            .map(str::trim)
            .filter(|l| !l.is_empty() && !l.starts_with('#'))
        {
            let (key, value) = line.split_once('=').ok_or(Error::Invalid)?;
            let (key, value) = (key.trim(), value.trim());
            if key == "grant" {
                let peer = knightbus_transport::security::parse_fingerprint(value)?;
                if grants.contains(&peer) || grants.len() >= MAX_STARTUP_GRANTS {
                    return Err(Error::Invalid);
                }
                grants.push(peer);
            } else if value.is_empty() || fields.insert(key, value).is_some() {
                return Err(Error::Invalid);
            }
        }
        let mut take = |key| fields.remove(key).ok_or(Error::Invalid);
        let role = take("role")?;
        let state = PathBuf::from(take("state")?);
        if !state.is_absolute() {
            return Err(Error::Invalid);
        }
        let usb = take("usb")?.parse().map_err(|_| Error::Invalid)?;
        let control = take("control")?.parse().map_err(|_| Error::Invalid)?;
        let role = match role {
            "exporter" => {
                let registry = number(take("registry")?)?;
                let capture = boolean(take("capture")?)?;
                let storage_unmounted = fields
                    .remove("storage_unmounted")
                    .map(boolean)
                    .transpose()?
                    .unwrap_or(false);
                if registry == 0 || !capture {
                    return Err(Error::Denied);
                }
                Role::Exporter {
                    registry,
                    capture,
                    storage_unmounted,
                    grants,
                }
            }
            "importer" if grants.is_empty() => Role::Importer {
                peer: knightbus_transport::security::parse_fingerprint(take("peer")?)?,
            },
            _ => return Err(Error::Invalid),
        };
        let mdns_name = fields.remove("mdns_name").map(str::to_owned);
        if let Some(name) = &mdns_name
            && (!matches!(role, Role::Exporter { .. })
                || name.is_empty()
                || name.len() > MAX_MDNS_NAME_BYTES
                || !name.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-'))
        {
            return Err(Error::Invalid);
        }
        if !fields.is_empty() {
            return Err(Error::Invalid);
        }
        Ok(Self {
            role,
            state,
            usb,
            control,
            mdns_name,
        })
    }
}
fn boolean(value: &str) -> Result<bool> {
    match value {
        "true" => Ok(true),
        "false" => Ok(false),
        _ => Err(Error::Invalid),
    }
}
/// Parse a decimal number or an explicitly `0x` prefixed hexadecimal number.
pub fn number(value: &str) -> Result<u64> {
    if let Some(hex) = value.strip_prefix("0x") {
        u64::from_str_radix(hex, 16)
    } else {
        value.parse()
    }
    .map_err(|_| Error::Invalid)
}
