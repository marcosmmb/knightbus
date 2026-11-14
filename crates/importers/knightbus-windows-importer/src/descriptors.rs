//! Allocation-free descriptor validation shared verbatim with the kernel crate.
//! Class descriptors remain opaque. This is USB metadata, never USB/IP parsing.

// USB descriptor types, lengths and field offsets. Shared with the kernel
// validator so parsing and userspace topology construction use the same layout.
pub(crate) const DESCRIPTOR_HEADER_BYTES: usize = 2;
pub(crate) const DESCRIPTOR_TYPE: usize = 1;
pub(crate) const DIRECTION_IN: u8 = 0x80;
pub(crate) const ENDPOINT_NUMBER_MASK: u8 = 0x0f;
pub(crate) const IN_ENDPOINT_BIT_OFFSET: u8 = 16;
pub(crate) const TRANSFER_BULK: u8 = 2;
pub(crate) const TRANSFER_INTERRUPT: u8 = 3;

pub(crate) mod device {
    pub const TYPE: u8 = 1;
    pub const BYTES: usize = 18;
    pub const USB_REVISION: usize = 2;
    pub const EP0_PACKET: usize = 7;
    pub const VENDOR: usize = 8;
    pub const PRODUCT: usize = 10;
    pub const CONFIGURATIONS: usize = 17;
}
pub(crate) mod configuration {
    pub const TYPE: u8 = 2;
    pub const BYTES: usize = 9;
    pub const TOTAL_LENGTH: usize = 2;
    pub const INTERFACES: usize = 4;
    pub const VALUE: usize = 5;
    pub const ATTRIBUTES: usize = 7;
}
pub(crate) mod interface {
    pub const TYPE: u8 = 4;
    pub const BYTES: usize = 9;
    pub const NUMBER: usize = 2;
    pub const ALTERNATE: usize = 3;
    pub const ENDPOINTS: usize = 4;
}
pub(crate) mod endpoint {
    pub const TYPE: u8 = 5;
    pub const MIN_BYTES: usize = 7;
    pub const ADDRESS: usize = 2;
    pub const ATTRIBUTES: usize = 3;
    pub const MAX_PACKET: usize = 4;
    pub const INTERVAL: usize = 6;
}
const ASSOCIATION_TYPE: u8 = 11;
const ASSOCIATION_BYTES: usize = 8;
const ASSOCIATION_FIRST_INTERFACE: usize = 2;
const ASSOCIATION_INTERFACE_COUNT: usize = 3;
const USB_REVISION_3_0: u16 = 0x0300;
const CONFIG_REQUIRED_BIT: u8 = 0x80;
const CONFIG_RESERVED_MASK: u8 = 0x9f;
const CONFIG_REMOTE_WAKE: u8 = 0x20;
const ENDPOINT_RESERVED_MASK: u8 = 0x70;
const PACKET_SIZE_MASK: u16 = 0x07ff;
const TRANSACTIONS_SHIFT: u32 = 11;
const MAX_EXTRA_TRANSACTIONS: u16 = 2;
const LOW_SPEED_MAX_PACKET: u16 = 8;
const FULL_SPEED_MAX_PACKET: u16 = 64;
const FULL_SPEED_PACKET_SIZES: [u16; 4] = [8, 16, 32, 64];
const HIGH_SPEED_BULK_PACKET: u16 = 512;
const HIGH_SPEED_INTERRUPT_MAX_PACKET: u16 = 1024;
const HIGH_SPEED_MAX_INTERVAL: u8 = 16;

pub(crate) fn u16_at(bytes: &[u8], offset: usize) -> u16 {
    u16::from_le_bytes([bytes[offset], bytes[offset + 1]])
}

/// Maximum bytes in one configuration, before any allocation or kernel copy.
pub const MAX_CONFIGURATION: usize = 4096;
/// All nondefault USB endpoint addresses (15 in each direction).
pub const MAX_ENDPOINTS: usize = 30;
/// Maximum interface/alternate pairs in a configuration.
pub const MAX_SETTINGS: usize = 64;

/// Linux USB speed values supported by this native backend.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u32)]
pub enum Speed {
    /// USB low speed.
    Low = 1,
    /// USB full speed.
    Full = 2,
    /// USB high speed; SuperSpeed requires separate companion/BOS handling.
    High = 3,
}
impl TryFrom<u32> for Speed {
    type Error = &'static str;
    fn try_from(value: u32) -> Result<Self, Self::Error> {
        match value {
            v if v == Self::Low as u32 => Ok(Self::Low),
            v if v == Self::Full as u32 => Ok(Self::Full),
            v if v == Self::High as u32 => Ok(Self::High),
            _ => Err("unsupported USB speed"),
        }
    }
}

/// Validated endpoint identity, independent of its selected alternate setting.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Endpoint {
    /// USB address including direction bit.
    pub address: u8,
    /// USB transfer type: bulk 2 or interrupt 3.
    pub kind: u8,
    /// wMaxPacketSize, including high-bandwidth transaction bits.
    pub maximum_packet: u16,
    /// bInterval.
    pub interval: u8,
    /// Owning interface number.
    pub interface: u8,
}
/// One allowed interface alternate setting and its endpoint addresses.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Setting {
    /// Interface number.
    pub interface: u8,
    /// Alternate setting number.
    pub alternate: u8,
    /// Bit n represents endpoint n OUT; bit n+16 represents n IN.
    pub endpoints: u32,
}
/// Fixed-capacity topology. No class driver or endpoint is inferred from VID/PID.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Topology {
    /// Vendor ID, metadata only.
    pub vid: u16,
    /// Product ID, metadata only.
    pub pid: u16,
    /// Nonzero bConfigurationValue.
    pub configuration: u8,
    /// Number of distinct interfaces.
    pub interfaces: u8,
    /// Unique endpoint addresses across all alternate settings.
    pub endpoints: [Option<Endpoint>; MAX_ENDPOINTS],
    /// Allowed alternate settings.
    pub settings: [Option<Setting>; MAX_SETTINGS],
}

/// Validate a complete device/configuration descriptor before publication.
/// One configuration is supported; composite interfaces and alternate settings
/// are supported. ISO and SuperSpeed fail explicitly until their native paths
/// are qualified. No attacker-controlled allocation occurs here.
pub fn validate(device: &[u8], config: &[u8], speed: Speed) -> Result<Topology, &'static str> {
    if device.len() != device::BYTES
        || device[..DESCRIPTOR_HEADER_BYTES] != [device::BYTES as u8, device::TYPE]
        || device[device::CONFIGURATIONS] != 1
    {
        return Err("expected complete device descriptor and one configuration");
    }
    let ep0 = match speed {
        Speed::Low => u16::from(device[device::EP0_PACKET]) == LOW_SPEED_MAX_PACKET,
        Speed::Full => FULL_SPEED_PACKET_SIZES.contains(&u16::from(device[device::EP0_PACKET])),
        Speed::High => u16::from(device[device::EP0_PACKET]) == FULL_SPEED_MAX_PACKET,
    };
    if !ep0 || u16_at(device, device::USB_REVISION) >= USB_REVISION_3_0 {
        return Err("invalid endpoint zero or unsupported USB revision");
    }
    if !(configuration::BYTES..=MAX_CONFIGURATION).contains(&config.len())
        || config[..DESCRIPTOR_HEADER_BYTES] != [configuration::BYTES as u8, configuration::TYPE]
        || u16_at(config, configuration::TOTAL_LENGTH) as usize != config.len()
        || config[configuration::INTERFACES] == 0
        || config[configuration::VALUE] == 0
        || config[configuration::ATTRIBUTES] & CONFIG_RESERVED_MASK != CONFIG_REQUIRED_BIT
    {
        return Err("invalid configuration descriptor");
    }
    if config[configuration::ATTRIBUTES] & CONFIG_REMOTE_WAKE != 0 {
        return Err("remote wake requires native power callbacks not yet qualified");
    }
    let mut result = Topology {
        vid: u16_at(device, device::VENDOR),
        pid: u16_at(device, device::PRODUCT),
        configuration: config[configuration::VALUE],
        interfaces: config[configuration::INTERFACES],
        endpoints: [None; MAX_ENDPOINTS],
        settings: [None; MAX_SETTINGS],
    };
    let mut offset = configuration::BYTES;
    let mut settings = 0;
    let mut endpoints = 0;
    let mut expected = 0;
    let mut seen = 0;
    while offset < config.len() {
        if config.len() - offset < DESCRIPTOR_HEADER_BYTES {
            return Err("truncated descriptor header");
        }
        let length = config[offset] as usize;
        if length < DESCRIPTOR_HEADER_BYTES || length > config.len() - offset {
            return Err("invalid descriptor length");
        }
        let d = &config[offset..offset + length];
        match d[DESCRIPTOR_TYPE] {
            interface::TYPE => {
                // INTERFACE: close the preceding setting before opening another.
                if length != interface::BYTES
                    || settings == MAX_SETTINGS
                    || seen != expected
                    || d[interface::ENDPOINTS] as usize > MAX_ENDPOINTS
                    || d[interface::NUMBER] >= result.interfaces
                    || result.settings.iter().flatten().any(|s| {
                        s.interface == d[interface::NUMBER]
                            && s.alternate == d[interface::ALTERNATE]
                    })
                {
                    return Err("invalid or duplicate interface setting");
                }
                result.settings[settings] = Some(Setting {
                    interface: d[interface::NUMBER],
                    alternate: d[interface::ALTERNATE],
                    endpoints: 0,
                });
                settings += 1;
                expected = d[interface::ENDPOINTS];
                seen = 0;
            }
            endpoint::TYPE => {
                // ENDPOINT belongs to the immediately preceding interface.
                if settings == 0
                    || length < endpoint::MIN_BYTES
                    || d[endpoint::ADDRESS] & ENDPOINT_RESERVED_MASK != 0
                    || d[endpoint::ADDRESS] & ENDPOINT_NUMBER_MASK == 0
                    || !matches!(d[endpoint::ATTRIBUTES], TRANSFER_BULK | TRANSFER_INTERRUPT)
                    || seen >= expected
                {
                    return Err("invalid endpoint or unsupported transfer type");
                }
                let packet = u16_at(d, endpoint::MAX_PACKET);
                let size = packet & PACKET_SIZE_MASK;
                let valid = match (speed, d[endpoint::ATTRIBUTES]) {
                    (Speed::Low, TRANSFER_INTERRUPT) => {
                        packet <= LOW_SPEED_MAX_PACKET && d[endpoint::INTERVAL] > 0
                    }
                    (Speed::Full, TRANSFER_BULK) => FULL_SPEED_PACKET_SIZES.contains(&packet),
                    (Speed::Full, TRANSFER_INTERRUPT) => {
                        packet <= FULL_SPEED_MAX_PACKET && d[endpoint::INTERVAL] > 0
                    }
                    (Speed::High, TRANSFER_BULK) => packet == HIGH_SPEED_BULK_PACKET,
                    (Speed::High, TRANSFER_INTERRUPT) => {
                        size <= HIGH_SPEED_INTERRUPT_MAX_PACKET
                            && (packet >> TRANSACTIONS_SHIFT) <= MAX_EXTRA_TRANSACTIONS
                            && (1..=HIGH_SPEED_MAX_INTERVAL).contains(&d[endpoint::INTERVAL])
                    }
                    _ => false,
                };
                if size == 0 || !valid {
                    return Err("invalid endpoint packet size or interval");
                }
                let setting = result.settings[settings - 1].as_mut().unwrap();
                let bit = 1u32
                    << ((d[endpoint::ADDRESS] & ENDPOINT_NUMBER_MASK)
                        + if d[endpoint::ADDRESS] & DIRECTION_IN != 0 {
                            IN_ENDPOINT_BIT_OFFSET
                        } else {
                            0
                        });
                if setting.endpoints & bit != 0 {
                    return Err("duplicate endpoint in setting");
                }
                setting.endpoints |= bit;
                if let Some(old) = result
                    .endpoints
                    .iter()
                    .flatten()
                    .find(|e| e.address == d[endpoint::ADDRESS])
                {
                    if old.interface != setting.interface {
                        return Err("endpoint shared by independent interfaces");
                    }
                } else {
                    if endpoints == MAX_ENDPOINTS {
                        return Err("endpoint limit exceeded");
                    }
                    result.endpoints[endpoints] = Some(Endpoint {
                        address: d[endpoint::ADDRESS],
                        kind: d[endpoint::ATTRIBUTES],
                        maximum_packet: packet,
                        interval: d[endpoint::INTERVAL],
                        interface: setting.interface,
                    });
                    endpoints += 1;
                }
                seen += 1;
            }
            ASSOCIATION_TYPE => {
                // INTERFACE_ASSOCIATION: preserve composite-function grouping.
                if length != ASSOCIATION_BYTES
                    || d[ASSOCIATION_INTERFACE_COUNT] == 0
                    || u16::from(d[ASSOCIATION_FIRST_INTERFACE])
                        + u16::from(d[ASSOCIATION_INTERFACE_COUNT])
                        > u16::from(result.interfaces)
                {
                    return Err("invalid interface association");
                }
            }
            device::TYPE | configuration::TYPE => return Err("nested device or configuration"),
            _ => {
                if settings == 0 {
                    return Err("class descriptor before interface");
                }
            }
        }
        offset += length;
    }
    if seen != expected
        || (0..result.interfaces).any(|number| {
            !result
                .settings
                .iter()
                .flatten()
                .any(|s| s.interface == number && s.alternate == 0)
        })
    {
        return Err("missing interface, alternate zero or endpoint");
    }
    Ok(result)
}
