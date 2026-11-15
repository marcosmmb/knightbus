use crate::usbip;

/// Stable error categories shared by both hosting adapters (control API v1).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u16)]
pub enum Error {
    /// Invalid fields, topology or sequence.
    Invalid = 1,
    /// Operation incompatible with lifecycle state.
    State = 2,
    /// Backend cannot support this operation.
    Unsupported = 3,
    /// Backend disconnected or unavailable.
    Unavailable = 4,
    /// Hidden resources and unauthorized operations use the same error.
    Denied = 5,
    /// An authorized resource no longer exists.
    NotFound = 6,
    /// Incompatible control version.
    Version = 7,
    /// Monotonic deadline expired.
    Deadline = 8,
    /// Cancellation won before completion was committed.
    Cancelled = 9,
    /// Admission rejected without starting work.
    Exhausted = 10,
    /// Sanitized backend or invariant failure.
    Internal = 11,
    /// Cleanup deadline expired; resources remain quarantined.
    Cleanup = 12,
}
impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{self:?}")
    }
}
impl std::error::Error for Error {}
/// Engine result with a stable, payload-free error.
pub type Result<T> = std::result::Result<T, Error>;

/// Authenticated peer key fingerprint, supplied by a trusted host, never a request.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct PeerId(pub [u8; 32]);
/// Persisted exporter-assigned device identity; not VID/PID or a USB address.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct DeviceId(pub u64);
/// Physical incarnation; old attach requests cannot select a replugged device.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct DeviceKey {
    /// Stable inventory identity.
    pub id: DeviceId,
    /// Strictly increasing physical generation.
    pub generation: u64,
}
/// Unique lease within a host epoch. Hosts must use a new epoch on restart.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct SessionId {
    /// Persisted monotonic or cryptographically random nonzero host epoch.
    pub epoch: u64,
    /// Monotonic engine-local counter.
    pub serial: u64,
}
/// Native operation correlation, including restart and connection generations.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct TransferId {
    /// Owning lease.
    pub session: SessionId,
    /// USB/IP request sequence; never reused in a lease.
    pub sequence: u32,
}
/// Verified local caller authority. Only trusted host composition creates it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Caller {
    /// Authenticated remote/importer identity bound by the host.
    pub peer: PeerId,
    /// Exporter administrative authority from local OS policy.
    pub administrator: bool,
}
/// USB endpoint transfer kind; obtained from validated descriptors.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TransferKind {
    /// Control endpoint, including setup packet.
    Control,
    /// Bulk endpoint.
    Bulk,
    /// Interrupt endpoint.
    Interrupt,
    /// Isochronous endpoint.
    Isochronous,
}
/// Portable endpoint descriptor and backend capability.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Endpoint {
    /// USB address including IN direction bit; endpoint zero is implicit.
    pub address: u8,
    /// Descriptor transfer type.
    pub kind: TransferKind,
    /// Descriptor maximum packet size, including high-bandwidth bits.
    pub max_packet_size: u16,
    /// Maximum bytes per service interval, including periodic bursts.
    pub service_bytes: u32,
    /// Descriptor interval.
    pub interval: u8,
    /// Explicit supported USB/IP flags, default deny all others.
    pub allowed_flags: u32,
}
/// An interface alternate setting.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Interface {
    /// Interface number.
    pub number: u8,
    /// Alternate setting.
    pub alternate: u8,
    /// Class/subclass/protocol from the descriptor.
    pub class: usbip::Interface,
    /// Endpoints for this alternate setting.
    pub endpoints: Vec<Endpoint>,
}
/// Configuration metadata, including all interface alternate settings.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Configuration {
    /// Nonzero bConfigurationValue.
    pub value: u8,
    /// Interface alternate settings.
    pub interfaces: Vec<Interface>,
}
/// Physical inventory metadata; wire records reuse the protocol crate.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Device {
    /// Persisted identity plus current incarnation.
    pub key: DeviceKey,
    /// Standard USB/IP device metadata. Bus ID must equal [`DeviceId::bus_id`].
    pub wire: usbip::Device,
    /// Parsed configurations, bounded and checked on registration.
    pub configurations: Vec<Configuration>,
}
impl DeviceId {
    /// Stable, collision-free USB/IP bus ID in the exporter's identity namespace.
    pub fn bus_id(self) -> usbip::WireString<32> {
        usbip::WireString::new(format!("kb-{:016x}", self.0).as_bytes()).expect("fixed-size bus ID")
    }
}
impl Device {
    /// Allocated descriptor-topology capacity, excluding this fixed-size value.
    /// Backend owners use this to reserve shared metadata credits before cloning.
    pub fn storage_bytes(&self) -> Result<usize> {
        let mut bytes = self
            .configurations
            .capacity()
            .checked_mul(std::mem::size_of::<Configuration>())
            .ok_or(Error::Exhausted)?;
        for c in &self.configurations {
            bytes = bytes
                .checked_add(
                    c.interfaces
                        .capacity()
                        .checked_mul(std::mem::size_of::<Interface>())
                        .ok_or(Error::Exhausted)?,
                )
                .ok_or(Error::Exhausted)?;
            for i in &c.interfaces {
                bytes = bytes
                    .checked_add(
                        i.endpoints
                            .capacity()
                            .checked_mul(std::mem::size_of::<Endpoint>())
                            .ok_or(Error::Exhausted)?,
                    )
                    .ok_or(Error::Exhausted)?;
            }
        }
        Ok(bytes)
    }
    pub(crate) fn validate(&self) -> Result<()> {
        if self.key.id.0 == 0
            || self.key.generation == 0
            || self.wire.bus_id != self.key.id.bus_id()
            || self.wire.bus_number > u16::MAX.into()
            || self.wire.device_number > u16::MAX.into()
            || self.configurations.is_empty()
            || self.configurations.len() > crate::descriptors::limits::MAX_CONFIGURATIONS
            || self.configurations.len() != usize::from(self.wire.configurations)
        {
            return Err(Error::Invalid);
        }
        let mut configurations = std::collections::BTreeSet::new();
        for c in &self.configurations {
            if c.value == 0
                || !configurations.insert(c.value)
                || c.interfaces.len() > crate::descriptors::limits::MAX_INTERFACE_ALTERNATES
            {
                return Err(Error::Invalid);
            }
            let mut interfaces = std::collections::BTreeSet::new();
            let mut active_endpoints = std::collections::BTreeSet::new();
            for i in &c.interfaces {
                if !interfaces.insert((i.number, i.alternate)) || i.endpoints.len() > 30 {
                    return Err(Error::Invalid);
                }
                let mut endpoints = std::collections::BTreeSet::new();
                for e in &i.endpoints {
                    if e.address & knightbus_usb::ENDPOINT_RESERVED_MASK != 0
                        || e.address & knightbus_usb::ENDPOINT_NUMBER_MASK == 0
                        || e.max_packet_size == 0
                        || e.service_bytes == 0
                        || e.kind == TransferKind::Control
                        || !endpoints.insert(e.address)
                        || (i.alternate == 0 && !active_endpoints.insert(e.address))
                    {
                        return Err(Error::Invalid);
                    }
                }
            }
            if c.interfaces
                .iter()
                .any(|i| !interfaces.contains(&(i.number, 0)))
            {
                return Err(Error::Invalid);
            }
        }
        let active = self
            .configurations
            .iter()
            .find(|c| c.value == self.wire.configuration)
            .ok_or(Error::Invalid)?;
        if active
            .interfaces
            .iter()
            .filter(|i| i.alternate == 0)
            .count()
            != usize::from(self.wire.interfaces)
        {
            return Err(Error::Invalid);
        }
        Ok(())
    }
    pub(crate) fn endpoint(&self, number: u32, direction: usbip::Direction) -> Option<&Endpoint> {
        let address = u8::try_from(number).ok()?
            | if direction == usbip::Direction::In {
                0x80
            } else {
                0
            };
        self.configurations
            .iter()
            .find(|c| c.value == self.wire.configuration)?
            .interfaces
            .iter()
            .filter(|i| i.alternate == 0)
            .flat_map(|i| &i.endpoints)
            .find(|e| e.address == address)
    }
    pub(crate) fn wire_id(&self) -> u32 {
        (self.wire.bus_number << usbip::DEVICE_BUS_SHIFT) | self.wire.device_number
    }
}
/// Exporter lease lifecycle. Draining and quarantined leases retain ownership.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SessionState {
    /// Transactional acquisition in progress.
    Acquiring,
    /// All devices claimed, transfers permitted.
    Active,
    /// Cancellation/rollback and native retirement in progress.
    Draining,
    /// Cleanup exceeded its deadline; cannot reuse resources yet.
    Quarantined,
    /// All callbacks and device claims retired.
    Closed,
}
/// Sanitized authorized session snapshot.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Session {
    /// Lease identity.
    pub id: SessionId,
    /// Owner identity.
    pub peer: PeerId,
    /// Transaction devices.
    pub devices: Vec<DeviceKey>,
    /// Current lifecycle.
    pub state: SessionState,
    /// Terminal cause or cleanup diagnostic, without native payloads.
    pub error: Option<Error>,
}
