//! USB definitions, setup packets and allocation-free descriptor validation.
#![no_std]
//! Class descriptors remain opaque. This is USB metadata, never USB/IP parsing.

// USB descriptor types, lengths and field offsets. Shared with the kernel
// validator so parsing and userspace topology construction use the same layout.
/// USB mass-storage class code used when checking capture consent.
pub const CLASS_MASS_STORAGE: u8 = 0x08;
/// Length of the type/length prefix common to USB descriptors.
pub const DESCRIPTOR_HEADER_BYTES: usize = 2;
pub const DESCRIPTOR_TYPE: usize = 1;
pub const DIRECTION_IN: u8 = 0x80;
pub const ENDPOINT_NUMBER_MASK: u8 = 0x0f;
pub const IN_ENDPOINT_BIT_OFFSET: u8 = 16;
pub const TRANSFER_BULK: u8 = 2;
pub const TRANSFER_INTERRUPT: u8 = 3;

pub mod device {
    pub const TYPE: u8 = 1;
    pub const BYTES: usize = 18;
    pub const USB_REVISION: usize = 2;
    pub const EP0_PACKET: usize = 7;
    pub const CLASS: usize = 4;
    pub const SUBCLASS: usize = 5;
    pub const PROTOCOL: usize = 6;
    pub const BCD: usize = 12;
    pub const VENDOR: usize = 8;
    pub const PRODUCT: usize = 10;
    pub const CONFIGURATIONS: usize = 17;
}
pub mod configuration {
    pub const TYPE: u8 = 2;
    pub const BYTES: usize = 9;
    pub const TOTAL_LENGTH: usize = 2;
    pub const INTERFACES: usize = 4;
    pub const VALUE: usize = 5;
    pub const ATTRIBUTES: usize = 7;
}
pub mod interface {
    pub const TYPE: u8 = 4;
    pub const BYTES: usize = 9;
    pub const NUMBER: usize = 2;
    pub const ALTERNATE: usize = 3;
    pub const ENDPOINTS: usize = 4;
    pub const CLASS: usize = 5;
    pub const SUBCLASS: usize = 6;
    pub const PROTOCOL: usize = 7;
}
pub mod endpoint {
    pub const TYPE: u8 = 5;
    pub const MIN_BYTES: usize = 7;
    pub const ADDRESS: usize = 2;
    pub const ATTRIBUTES: usize = 3;
    pub const MAX_PACKET: usize = 4;
    pub const INTERVAL: usize = 6;
}
/// SuperSpeed endpoint companion descriptor.
pub mod companion {
    pub const TYPE: u8 = 0x30;
    pub const BYTES: usize = 6;
    pub const MAX_BURST: usize = 2;
    pub const ATTRIBUTES: usize = 3;
    pub const BYTES_PER_INTERVAL: usize = 4;
    pub const MAX_BURST_VALUE: u8 = 15;
    pub const MAX_STREAMS_EXPONENT: u8 = 16;
    pub const MAX_MULT: u8 = 2;
}
const ASSOCIATION_TYPE: u8 = 11;
const ASSOCIATION_BYTES: usize = 8;
const ASSOCIATION_FIRST_INTERFACE: usize = 2;
const ASSOCIATION_INTERFACE_COUNT: usize = 3;
const USB_REVISION_3_0: u16 = 0x0300;
/// Required configuration attribute bit.
pub const CONFIG_REQUIRED_BIT: u8 = 0x80;
/// Configuration advertises remote wakeup.
pub const CONFIG_REMOTE_WAKEUP: u8 = 0x20;
/// Required and reserved configuration attribute bits.
pub const CONFIG_ATTRIBUTES_MASK: u8 = 0x9f;
/// Reserved endpoint address bits.
pub const ENDPOINT_RESERVED_MASK: u8 = 0x70;
/// Payload bits of wMaxPacketSize.
pub const PACKET_SIZE_MASK: u16 = 0x07ff;
/// Additional high-speed transactions field offset.
pub const TRANSACTIONS_SHIFT: u32 = 11;
const MAX_EXTRA_TRANSACTIONS: u16 = 2;
const LOW_SPEED_MAX_PACKET: u16 = 8;
const FULL_SPEED_MAX_PACKET: u16 = 64;
const FULL_SPEED_PACKET_SIZES: [u16; 4] = [8, 16, 32, 64];
const HIGH_SPEED_BULK_PACKET: u16 = 512;
const HIGH_SPEED_INTERRUPT_MAX_PACKET: u16 = 1024;
/// Largest exponent allowed for periodic endpoint intervals.
pub const MAX_PERIODIC_INTERVAL: u8 = 16;
/// High-speed microframes in one millisecond USB frame.
pub const MICROFRAMES_PER_FRAME: u32 = 8;
const FULL_SPEED_ISO_MAX_PACKET: u16 = 1023;
const SUPER_SPEED_MAX_PACKET: u16 = 1024;
const ISO_RESERVED_ATTRIBUTES: u8 = 0xc0;
const ISO_USAGE_MASK: u8 = 0x30;
const ISO_RESERVED_USAGE: u8 = 0x30;

/// Class/vendor request-type bits.
pub const REQUEST_TYPE_MASK: u8 = 0x60;
/// Recipient bits in bmRequestType.
pub const RECIPIENT_MASK: u8 = 0x1f;
/// Standard request to the device.
pub const RECIPIENT_DEVICE: u8 = 0;
/// Standard request to an interface.
pub const RECIPIENT_INTERFACE: u8 = 1;
/// Standard request to an endpoint.
pub const RECIPIENT_ENDPOINT: u8 = 2;
/// CLEAR_FEATURE request code.
pub const CLEAR_FEATURE: u8 = 1;
/// SET_ADDRESS request code.
pub const SET_ADDRESS: u8 = 5;
/// GET_DESCRIPTOR request code.
pub const GET_DESCRIPTOR: u8 = 6;
/// GET_CONFIGURATION request code.
pub const GET_CONFIGURATION: u8 = 8;
/// SET_CONFIGURATION request code.
pub const SET_CONFIGURATION: u8 = 9;
/// GET_INTERFACE request code.
pub const GET_INTERFACE: u8 = 10;
/// SET_INTERFACE request code.
pub const SET_INTERFACE: u8 = 11;
/// ENDPOINT_HALT feature selector.
pub const ENDPOINT_HALT: u16 = 0;
/// Sixteen endpoint numbers in each direction.
pub const ENDPOINT_DIRECTION_SLOTS: usize = 32;
/// bmAttributes transfer-type bits.
pub const TRANSFER_TYPE_MASK: u8 = 3;
/// Isochronous transfer type.
pub const TRANSFER_ISOCHRONOUS: u8 = 1;
/// Additional high-speed transactions field mask.
pub const TRANSACTIONS_MASK: u16 = 3;
/// HID report descriptor type.
pub const DESCRIPTOR_HID_REPORT: u8 = 0x22;
/// USB control setup packet size.
pub const SETUP_SIZE: usize = 8;
/// bmRequestType offset.
pub const SETUP_TYPE: usize = 0;
/// bRequest offset.
pub const SETUP_REQUEST: usize = 1;
/// wValue little-endian offset.
pub const SETUP_VALUE: usize = 2;
/// wIndex little-endian offset.
pub const SETUP_INDEX: usize = 4;
/// wLength little-endian offset.
pub const SETUP_LENGTH: usize = 6;

/// Construct a control setup packet without opaque byte-array literals.
pub const fn setup(
    request_type: u8,
    request: u8,
    value: u16,
    index: u16,
    length: u16,
) -> [u8; SETUP_SIZE] {
    let value = value.to_le_bytes();
    let index = index.to_le_bytes();
    let length = length.to_le_bytes();
    [
        request_type,
        request,
        value[0],
        value[1],
        index[0],
        index[1],
        length[0],
        length[1],
    ]
}
/// Read the complete device descriptor.
pub const READ_DEVICE_DESCRIPTOR: [u8; SETUP_SIZE] = setup(
    DIRECTION_IN,
    GET_DESCRIPTOR,
    (device::TYPE as u16) << u8::BITS,
    0,
    device::BYTES as u16,
);
/// Read the currently selected configuration.
pub const READ_CONFIGURATION: [u8; SETUP_SIZE] = setup(DIRECTION_IN, GET_CONFIGURATION, 0, 0, 1);

/// Read a little-endian word from a previously bounds-checked field.
pub fn u16_at(bytes: &[u8], offset: usize) -> u16 {
    u16::from_le_bytes([bytes[offset], bytes[offset + 1]])
}

/// Maximum bytes in one configuration, before any allocation or kernel copy.
pub const MAX_CONFIGURATION: usize = u16::MAX as usize;
/// Maximum BOS descriptor tree passed to the native controller.
pub const MAX_BOS: usize = u16::MAX as usize;
/// Binary Object Store descriptor type.
pub const BOS_TYPE: u8 = 15;
/// BOS header size.
pub const BOS_HEADER: usize = 5;
/// All nondefault USB endpoint addresses (15 in each direction).
pub const MAX_ENDPOINTS: usize = 30;
/// Maximum interface/alternate pairs in a configuration.
pub const MAX_SETTINGS: usize = 64;

/// Negotiated USB speed, with the numeric codes used by USB/IP and the native channel.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u32)]
pub enum Speed {
    /// USB low speed.
    Low = 1,
    /// USB full speed.
    Full = 2,
    /// USB high speed; SuperSpeed requires separate companion/BOS handling.
    High = 3,
    /// SuperSpeed USB.
    Super = 5,
    /// Physical SuperSpeedPlus.
    SuperPlus = 6,
}
impl TryFrom<u32> for Speed {
    type Error = &'static str;
    fn try_from(value: u32) -> Result<Self, Self::Error> {
        match value {
            v if v == Self::Low as u32 => Ok(Self::Low),
            v if v == Self::Full as u32 => Ok(Self::Full),
            v if v == Self::High as u32 => Ok(Self::High),
            v if v == Self::Super as u32 => Ok(Self::Super),
            v if v == Self::SuperPlus as u32 => Ok(Self::SuperPlus),
            _ => Err("unsupported USB speed"),
        }
    }
}
impl Speed {
    /// Capability bitmap containing every defined speed.
    pub const ALL_MASK: u32 = (1 << Self::Low as u32)
        | (1 << Self::Full as u32)
        | (1 << Self::High as u32)
        | (1 << Self::Super as u32)
        | (1 << Self::SuperPlus as u32);
    /// Whether SuperSpeed descriptor rules apply.
    pub fn superspeed(self) -> bool {
        matches!(self, Self::Super | Self::SuperPlus)
    }
}

/// Validated endpoint identity, independent of its selected alternate setting.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Endpoint {
    /// USB address including direction bit.
    pub address: u8,
    /// USB transfer type: isochronous 1, bulk 2 or interrupt 3.
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
    /// Endpoints advertising streams; selection requires an unsupported capability.
    pub stream_endpoints: u32,
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
    /// ISO endpoint addresses across all alternate settings.
    pub iso_endpoints: u32,
    /// Allowed alternate settings.
    pub settings: [Option<Setting>; MAX_SETTINGS],
}

/// Validate a complete device/configuration descriptor before publication.
/// Validates one configuration, including composite interfaces and alternates.
/// Backend capabilities determine which transfers and settings can be used.
pub fn validate(device: &[u8], config: &[u8], speed: Speed) -> Result<Topology, &'static str> {
    if device.len() != device::BYTES
        || device[..DESCRIPTOR_HEADER_BYTES] != [device::BYTES as u8, device::TYPE]
        || device[device::CONFIGURATIONS] == 0
    {
        return Err("expected complete device descriptor and a configuration");
    }
    let ep0 = match speed {
        Speed::Low => u16::from(device[device::EP0_PACKET]) == LOW_SPEED_MAX_PACKET,
        Speed::Full => FULL_SPEED_PACKET_SIZES.contains(&u16::from(device[device::EP0_PACKET])),
        Speed::High => u16::from(device[device::EP0_PACKET]) == FULL_SPEED_MAX_PACKET,
        Speed::Super | Speed::SuperPlus => device[device::EP0_PACKET] == 9,
    };
    let revision = u16_at(device, device::USB_REVISION);
    if !ep0 || (speed.superspeed() != (revision >= USB_REVISION_3_0)) || revision > 0x0320 {
        return Err("invalid endpoint zero or unsupported USB revision");
    }
    if !(configuration::BYTES..=MAX_CONFIGURATION).contains(&config.len())
        || config[..DESCRIPTOR_HEADER_BYTES] != [configuration::BYTES as u8, configuration::TYPE]
        || u16_at(config, configuration::TOTAL_LENGTH) as usize != config.len()
        || config[configuration::INTERFACES] == 0
        || config[configuration::VALUE] == 0
        || config[configuration::ATTRIBUTES] & CONFIG_ATTRIBUTES_MASK != CONFIG_REQUIRED_BIT
    {
        return Err("invalid configuration descriptor");
    }
    let topology = Topology {
        vid: u16_at(device, device::VENDOR),
        pid: u16_at(device, device::PRODUCT),
        configuration: config[configuration::VALUE],
        interfaces: config[configuration::INTERFACES],
        endpoints: [None; MAX_ENDPOINTS],
        settings: [None; MAX_SETTINGS],
        iso_endpoints: 0,
    };
    ConfigurationParser::new(topology, speed).parse(&config[configuration::BYTES..])
}

struct ConfigurationParser {
    topology: Topology,
    speed: Speed,
    settings: usize,
    endpoints: usize,
    expected: u8,
    seen: u8,
    companion: Option<(u8, u8, u16)>,
}
impl ConfigurationParser {
    fn new(topology: Topology, speed: Speed) -> Self {
        Self {
            topology,
            speed,
            settings: 0,
            endpoints: 0,
            expected: 0,
            seen: 0,
            companion: None,
        }
    }
    fn parse(mut self, mut bytes: &[u8]) -> Result<Topology, &'static str> {
        while !bytes.is_empty() {
            if bytes.len() < DESCRIPTOR_HEADER_BYTES {
                return Err("truncated descriptor header");
            }
            let length = bytes[0] as usize;
            if length < DESCRIPTOR_HEADER_BYTES || length > bytes.len() {
                return Err("invalid descriptor length");
            }
            let (d, remaining) = bytes.split_at(length);
            if self.companion.is_some() && d[DESCRIPTOR_TYPE] != companion::TYPE {
                return Err("missing SuperSpeed endpoint companion");
            }
            match d[DESCRIPTOR_TYPE] {
                interface::TYPE => self.interface(d)?,
                endpoint::TYPE => self.endpoint(d)?,
                companion::TYPE => self.companion(d)?,
                ASSOCIATION_TYPE => self.association(d)?,
                device::TYPE | configuration::TYPE => return Err("nested device or configuration"),
                _ if self.settings == 0 => return Err("class descriptor before interface"),
                _ => {}
            }
            bytes = remaining;
        }
        self.finish()
    }
    fn interface(&mut self, d: &[u8]) -> Result<(), &'static str> {
        let length = d.len();
        // INTERFACE: close the preceding setting before opening another.
        if length != interface::BYTES
            || self.settings == MAX_SETTINGS
            || self.seen != self.expected
            || d[interface::ENDPOINTS] as usize > MAX_ENDPOINTS
            || d[interface::NUMBER] >= self.topology.interfaces
            || self.topology.settings.iter().flatten().any(|s| {
                s.interface == d[interface::NUMBER] && s.alternate == d[interface::ALTERNATE]
            })
        {
            return Err("invalid or duplicate interface setting");
        }
        self.topology.settings[self.settings] = Some(Setting {
            interface: d[interface::NUMBER],
            alternate: d[interface::ALTERNATE],
            endpoints: 0,
            stream_endpoints: 0,
        });
        self.settings += 1;
        self.expected = d[interface::ENDPOINTS];
        self.seen = 0;
        Ok(())
    }
    fn endpoint(&mut self, d: &[u8]) -> Result<(), &'static str> {
        let length = d.len();
        // ENDPOINT belongs to the immediately preceding interface.
        if self.settings == 0
            || length < endpoint::MIN_BYTES
            || d[endpoint::ADDRESS] & ENDPOINT_RESERVED_MASK != 0
            || d[endpoint::ADDRESS] & ENDPOINT_NUMBER_MASK == 0
            || !valid_attributes(d[endpoint::ATTRIBUTES])
            || self.seen >= self.expected
        {
            return Err("invalid endpoint or unsupported transfer type");
        }
        let packet = u16_at(d, endpoint::MAX_PACKET);
        validate_packet(
            self.speed,
            d[endpoint::ATTRIBUTES] & TRANSFER_TYPE_MASK,
            packet,
            d[endpoint::INTERVAL],
        )?;
        let setting = self.topology.settings[self.settings - 1].as_mut().unwrap();
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
        if d[endpoint::ATTRIBUTES] & TRANSFER_TYPE_MASK == TRANSFER_ISOCHRONOUS {
            self.topology.iso_endpoints |= bit;
        }
        if let Some(old) = self
            .topology
            .endpoints
            .iter()
            .flatten()
            .find(|e| e.address == d[endpoint::ADDRESS])
        {
            if old.interface != setting.interface {
                return Err("endpoint shared by independent interfaces");
            }
        } else {
            if self.endpoints == MAX_ENDPOINTS {
                return Err("endpoint limit exceeded");
            }
            self.topology.endpoints[self.endpoints] = Some(Endpoint {
                address: d[endpoint::ADDRESS],
                kind: d[endpoint::ATTRIBUTES] & TRANSFER_TYPE_MASK,
                maximum_packet: packet,
                interval: d[endpoint::INTERVAL],
                interface: setting.interface,
            });
            self.endpoints += 1;
        }
        self.seen += 1;
        if self.speed.superspeed() {
            self.companion = Some((
                d[endpoint::ADDRESS],
                d[endpoint::ATTRIBUTES] & TRANSFER_TYPE_MASK,
                packet,
            ));
        }
        Ok(())
    }
    fn companion(&mut self, d: &[u8]) -> Result<(), &'static str> {
        let length = d.len();
        let (address, kind, packet) = self.companion.take().ok_or("unexpected companion")?;
        if length != companion::BYTES || d[companion::MAX_BURST] > companion::MAX_BURST_VALUE {
            return Err("invalid endpoint companion");
        }
        let bytes_per_interval = u16_at(d, companion::BYTES_PER_INTERVAL);
        if kind == TRANSFER_BULK {
            if d[companion::ATTRIBUTES] > companion::MAX_STREAMS_EXPONENT || bytes_per_interval != 0
            {
                return Err("invalid bulk companion");
            }
            if d[companion::ATTRIBUTES] != 0 {
                let bit = (address & ENDPOINT_NUMBER_MASK)
                    + if address & DIRECTION_IN != 0 {
                        IN_ENDPOINT_BIT_OFFSET
                    } else {
                        0
                    };
                self.topology.settings[self.settings - 1]
                    .as_mut()
                    .unwrap()
                    .stream_endpoints |= 1 << bit;
            }
        } else {
            let mult = if kind == TRANSFER_ISOCHRONOUS {
                d[companion::ATTRIBUTES]
            } else {
                0
            };
            if mult > companion::MAX_MULT
                || (kind == TRANSFER_INTERRUPT && d[companion::ATTRIBUTES] != 0)
                || bytes_per_interval == 0
                || u32::from(bytes_per_interval)
                    > u32::from(packet)
                        * (u32::from(d[companion::MAX_BURST]) + 1)
                        * (u32::from(mult) + 1)
            {
                return Err("invalid periodic endpoint companion");
            }
        }
        Ok(())
    }
    fn association(&mut self, d: &[u8]) -> Result<(), &'static str> {
        let length = d.len();
        // INTERFACE_ASSOCIATION: preserve composite-function grouping.
        if length != ASSOCIATION_BYTES
            || d[ASSOCIATION_INTERFACE_COUNT] == 0
            || u16::from(d[ASSOCIATION_FIRST_INTERFACE]) + u16::from(d[ASSOCIATION_INTERFACE_COUNT])
                > u16::from(self.topology.interfaces)
        {
            return Err("invalid interface association");
        }
        Ok(())
    }
    fn finish(self) -> Result<Topology, &'static str> {
        if self.companion.is_some()
            || self.seen != self.expected
            || (0..self.topology.interfaces).any(|number| {
                !self
                    .topology
                    .settings
                    .iter()
                    .flatten()
                    .any(|s| s.interface == number && s.alternate == 0)
            })
        {
            return Err("missing interface, alternate zero or endpoint");
        }
        Ok(self.topology)
    }
}

/// Validate bounded BOS framing and required SuperSpeed capability before native publication.
pub fn validate_bos(bytes: &[u8], speed: Speed) -> Result<(), &'static str> {
    if bytes.is_empty() && !speed.superspeed() {
        return Ok(());
    }
    if !(BOS_HEADER..=MAX_BOS).contains(&bytes.len())
        || bytes[..2] != [BOS_HEADER as u8, BOS_TYPE]
        || usize::from(u16_at(bytes, 2)) != bytes.len()
    {
        return Err("invalid BOS header");
    }
    let mut offset = BOS_HEADER;
    let mut count = 0;
    let mut super_capability = false;
    while offset < bytes.len() {
        if bytes.len() - offset < 3 {
            return Err("truncated capability");
        }
        let length = usize::from(bytes[offset]);
        if length < 3 || length > bytes.len() - offset || bytes[offset + 1] != 16 {
            return Err("invalid capability");
        }
        let d = &bytes[offset..offset + length];
        if d[2] == 3 {
            if length != 10 || super_capability || u16_at(d, 4) & 8 == 0 || d[6] > 3 {
                return Err("invalid SuperSpeed capability");
            }
            super_capability = true;
        }
        count += 1;
        offset += length;
    }
    if count != usize::from(bytes[4]) || (speed.superspeed() && !super_capability) {
        return Err("missing BOS capability");
    }
    Ok(())
}

fn valid_attributes(attributes: u8) -> bool {
    match attributes & TRANSFER_TYPE_MASK {
        TRANSFER_ISOCHRONOUS => {
            attributes & ISO_RESERVED_ATTRIBUTES == 0
                && attributes & ISO_USAGE_MASK != ISO_RESERVED_USAGE
        }
        TRANSFER_BULK | TRANSFER_INTERRUPT => attributes & !TRANSFER_TYPE_MASK == 0,
        _ => false,
    }
}

fn validate_packet(speed: Speed, kind: u8, packet: u16, interval: u8) -> Result<(), &'static str> {
    let size = packet & PACKET_SIZE_MASK;
    let valid = match (speed, kind) {
        (Speed::Low, TRANSFER_INTERRUPT) => packet <= LOW_SPEED_MAX_PACKET && interval > 0,
        (Speed::Full, TRANSFER_BULK) => FULL_SPEED_PACKET_SIZES.contains(&packet),
        (Speed::Full, TRANSFER_INTERRUPT) => packet <= FULL_SPEED_MAX_PACKET && interval > 0,
        (Speed::Full, TRANSFER_ISOCHRONOUS) => {
            packet <= FULL_SPEED_ISO_MAX_PACKET && (1..=MAX_PERIODIC_INTERVAL).contains(&interval)
        }
        (Speed::High, TRANSFER_BULK) => packet == HIGH_SPEED_BULK_PACKET,
        (Speed::High, TRANSFER_INTERRUPT | TRANSFER_ISOCHRONOUS) => {
            size <= HIGH_SPEED_INTERRUPT_MAX_PACKET
                && (packet >> TRANSACTIONS_SHIFT) <= MAX_EXTRA_TRANSACTIONS
                && (1..=MAX_PERIODIC_INTERVAL).contains(&interval)
        }
        (Speed::Super | Speed::SuperPlus, TRANSFER_BULK) => packet == SUPER_SPEED_MAX_PACKET,
        (Speed::Super | Speed::SuperPlus, TRANSFER_INTERRUPT | TRANSFER_ISOCHRONOUS) => {
            packet <= SUPER_SPEED_MAX_PACKET && (1..=MAX_PERIODIC_INTERVAL).contains(&interval)
        }
        _ => false,
    };
    if size == 0 || !valid {
        return Err("invalid endpoint packet size or interval");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    extern crate std;
    use super::*;
    use std::vec;

    #[test]
    fn setup_packets_preserve_little_endian_fields() {
        let packet = setup(0xc1, 0x45, 0xabcd, 0x1234, 0x5678);
        assert_eq!(packet, [0xc1, 0x45, 0xcd, 0xab, 0x34, 0x12, 0x78, 0x56]);
        assert_eq!(u16_at(&packet, SETUP_VALUE), 0xabcd);
        assert_eq!(u16_at(&packet, SETUP_INDEX), 0x1234);
        assert_eq!(u16_at(&packet, SETUP_LENGTH), 0x5678);
        assert_eq!(READ_DEVICE_DESCRIPTOR, [0x80, 6, 0, 1, 0, 0, 18, 0]);
        assert_eq!(READ_CONFIGURATION, [0x80, 8, 0, 0, 0, 0, 1, 0]);
    }
    #[test]
    fn iso_descriptor_speed_attributes_and_burst_limits() {
        let mut device = [18, 1, 0, 2, 0, 0, 0, 64, 9, 0x12, 1, 0, 0, 1, 0, 0, 0, 1];
        let mut config = vec![
            9, 2, 25, 0, 1, 1, 0, 0x80, 50, 9, 4, 0, 0, 1, 0xff, 0, 0, 0, 7, 5, 0x81, 5, 0, 4, 1,
        ];
        assert!(validate(&device, &config, Speed::Full).is_err());
        config[22..24].copy_from_slice(&1023u16.to_le_bytes());
        assert!(validate(&device, &config, Speed::Full).is_ok());
        assert!(validate(&device, &config, Speed::Low).is_err());
        for attributes in [1, 5, 9, 13, 17, 21, 25, 29, 33, 37, 41, 45] {
            config[21] = attributes;
            assert!(validate(&device, &config, Speed::High).is_ok());
        }
        for attributes in [49, 53, 57, 61, 65, 129] {
            config[21] = attributes;
            assert!(validate(&device, &config, Speed::High).is_err());
        }
        config[21] = 5;
        config[22..24].copy_from_slice(&0x1400u16.to_le_bytes());
        assert!(validate(&device, &config, Speed::High).is_ok());
        config[23] = 0x1c;
        assert!(validate(&device, &config, Speed::High).is_err());
        config[23] = 4;
        device[2..4].copy_from_slice(&0x300u16.to_le_bytes());
        device[7] = 9;
        config.extend_from_slice(&[6, 0x30, 1, 2, 0, 24]);
        config[2] = 31;
        assert!(validate(&device, &config, Speed::Super).is_ok());
        config[30] = 25;
        assert!(validate(&device, &config, Speed::Super).is_err());
        config[30] = 24;
        config[28] = 3;
        assert!(validate(&device, &config, Speed::Super).is_err());
        config[28] = 2;
        config[24] = 0;
        assert!(validate(&device, &config, Speed::Super).is_err());
    }
}
