//! Private driver ABI. Little-endian integers, exact lengths, no native pointers.
//! The driver independently checks ownership and request lifetimes.
use crate::descriptors::{self, Speed, Topology};
use knightbus_core::{Budget, Buffer, Error, Result};

#[cfg(windows)]
pub(crate) const ATTACH: u32 = 0x22e424;
#[cfg(windows)]
pub(crate) const FETCH: u32 = 0x22e404;
#[cfg(windows)]
pub(crate) const REPLY: u32 = 0x22e408;
#[cfg(windows)]
pub(crate) const DETACH: u32 = 0x22e40c;
#[cfg(windows)]
pub(crate) const STATS: u32 = 0x22e410;
#[cfg(windows)]
pub(crate) const LIFECYCLE: u32 = 0x22e414;
#[cfg(windows)]
// The previous code (0x22e428) only guaranteed a squattable global event.
pub(crate) const RETIREMENT_READY: u32 = 0x22e42c;
/// Maximum native payload per request. Larger URBs fail before publication.
pub const MAX_TRANSFER: usize = 1024 * 1024;
/// Default endpoint plus all legal nondefault endpoint addresses.
pub const SLOTS: usize = descriptors::MAX_ENDPOINTS + 1;
pub(crate) const HEADER: usize = 32;
pub(crate) const STATS_BYTES: usize = stats::TOKENS + SLOTS * size_of::<u64>();

// Offsets in the private driver ABI; keep these aligned with windows-driver-ipc.md.
pub(crate) const ABI_VERSION: u32 = 1;
pub(crate) mod transfer {
    pub const VERSION: usize = 0;
    pub const ENDPOINT: usize = 4;
    pub const TOKEN: usize = 8;
    pub const LENGTH: usize = 16;
    pub const PAYLOAD_LENGTH: usize = 20;
    pub const SETUP: usize = 24;
    pub const STATUS: usize = 24; // REPLY uses this field instead of FETCH setup.
    pub const SUCCESS: u32 = 0;
    pub const STALL: u32 = 1;
    pub const FAILURE: u32 = 2;
}
pub(crate) mod stats {
    pub const VERSION: usize = 0;
    pub const LIVE_MASK: usize = 56;
    pub const TOKENS: usize = 64;
}
pub(crate) mod setup {
    pub const BYTES: usize = 8;
    pub const REQUEST_TYPE: usize = 0;
    pub const REQUEST: usize = 1;
    pub const VALUE: usize = 2;
    pub const INDEX: usize = 4;
    pub const LENGTH: usize = 6;
    pub const DEVICE_OUT: u8 = 0x00;
    pub const DEVICE_IN: u8 = 0x80;
    pub const INTERFACE_OUT: u8 = 0x01;
    pub const GET_DESCRIPTOR: u8 = 6;
    pub const GET_CONFIGURATION: u8 = 8;
    pub const SET_CONFIGURATION: u8 = 9;
    pub const SET_INTERFACE: u8 = 11;
}
const ATTACH_MAGIC: &[u8; 4] = b"KBWD";
const ATTACH_VERSION: usize = 4;
const ATTACH_SPEED: usize = 8;
const ATTACH_DEVICE_LENGTH: usize = 12;
const ATTACH_CONFIG_LENGTH: usize = 16;
const ATTACH_HEADER: usize = 20;
const ATTACH_CONFIG: usize = ATTACH_HEADER + descriptors::device::BYTES;
pub(crate) const EP0_SLOT_MASK: u32 = 1;

/// Validated raw descriptors and their charged driver attach representation.
pub struct Descriptors {
    pub(crate) bytes: Buffer,
    /// Allocation-free, validated metadata.
    pub topology: Topology,
    /// Actual physical speed, never inferred from USB revision.
    pub speed: Speed,
}
impl Descriptors {
    /// Native pipes can receive work before the setting-change notification.
    /// Keep those requests in their native queues until the remote setting is active.
    pub(crate) fn active_slots(&self, alternates: &[u8; descriptors::MAX_SETTINGS]) -> u32 {
        self.topology.endpoints.iter().flatten().enumerate().fold(
            EP0_SLOT_MASK, // Endpoint zero must remain available to change settings.
            |mask, (index, endpoint)| {
                let bit = 1u32
                    << ((endpoint.address & descriptors::ENDPOINT_NUMBER_MASK)
                        + if endpoint.address & descriptors::DIRECTION_IN != 0 {
                            descriptors::IN_ENDPOINT_BIT_OFFSET
                        } else {
                            0
                        });
                if self.topology.settings.iter().flatten().any(|setting| {
                    setting.interface == endpoint.interface
                        && setting.alternate == alternates[endpoint.interface as usize]
                        && setting.endpoints & bit != 0
                }) {
                    mask | (1 << (index + 1))
                } else {
                    mask
                }
            },
        )
    }
    pub(crate) fn interval(
        &self,
        address: u8,
        alternates: &[u8; descriptors::MAX_SETTINGS],
    ) -> Result<i32> {
        if address == 0 {
            return Ok(0);
        }
        let length = u32_at(self.bytes.as_slice(), ATTACH_CONFIG_LENGTH) as usize;
        let config = &self.bytes.as_slice()[ATTACH_CONFIG..ATTACH_CONFIG + length];
        let mut offset = descriptors::configuration::BYTES;
        let mut selected = false;
        while offset < config.len() {
            let d = &config[offset..offset + config[offset] as usize];
            if d[descriptors::DESCRIPTOR_TYPE] == descriptors::interface::TYPE {
                selected = alternates[d[descriptors::interface::NUMBER] as usize]
                    == d[descriptors::interface::ALTERNATE];
            }
            if d[descriptors::DESCRIPTOR_TYPE] == descriptors::endpoint::TYPE
                && selected
                && d[descriptors::endpoint::ADDRESS] == address
            {
                return Ok(
                    if d[descriptors::endpoint::ATTRIBUTES] == descriptors::TRANSFER_BULK {
                        0
                    } else if self.speed == Speed::High || self.speed.superspeed() {
                        1 << (d[descriptors::endpoint::INTERVAL] - 1)
                    } else {
                        d[descriptors::endpoint::INTERVAL].into()
                    },
                );
            }
            offset += d.len();
        }
        Err(Error::Invalid)
    }
    /// Validate before allocating; preserve class descriptors verbatim.
    pub fn new(budget: &Budget, device: &[u8], config: &[u8], speed: Speed) -> Result<Self> {
        Self::with_bos(budget, device, config, &[], speed)
    }
    /// Preserve the complete physical BOS tree for SuperSpeed publication.
    pub fn with_bos(
        budget: &Budget,
        device: &[u8],
        config: &[u8],
        bos: &[u8],
        speed: Speed,
    ) -> Result<Self> {
        let topology =
            descriptors::validate(device, config, speed).map_err(|_| Error::Unsupported)?;
        descriptors::validate_bos(bos, speed).map_err(|_| Error::Unsupported)?;
        let mut bytes = budget.buffer(ATTACH_HEADER + device.len() + config.len() + bos.len())?;
        let b = bytes.as_mut_slice();
        b[..ATTACH_MAGIC.len()].copy_from_slice(ATTACH_MAGIC);
        for (offset, value) in [
            (ATTACH_VERSION, if bos.is_empty() { ABI_VERSION } else { 2 }),
            (ATTACH_SPEED, speed as u32),
            (ATTACH_DEVICE_LENGTH, descriptors::device::BYTES as u32),
            (ATTACH_CONFIG_LENGTH, config.len() as u32),
        ] {
            b[offset..offset + size_of::<u32>()].copy_from_slice(&value.to_le_bytes());
        }
        b[ATTACH_HEADER..ATTACH_CONFIG].copy_from_slice(device);
        b[ATTACH_CONFIG..ATTACH_CONFIG + config.len()].copy_from_slice(config);
        b[ATTACH_CONFIG + config.len()..].copy_from_slice(bos);
        Ok(Self {
            bytes,
            topology,
            speed,
        })
    }
}

/// Borrowed driver request; the payload is consumed before scratch reuse.
#[derive(Debug)]
pub struct Request<'a> {
    /// Monotonic native token, scoped to the owning driver handle.
    pub token: u64,
    /// USB endpoint address.
    pub endpoint: u8,
    /// Requested transfer length.
    pub length: u32,
    /// USB setup bytes, zero for nondefault endpoints.
    pub setup: [u8; setup::BYTES],
    /// OUT data only.
    pub payload: &'a [u8],
}
impl<'a> Request<'a> {
    /// Reject malformed native responses before constructing engine work.
    pub fn decode(bytes: &'a [u8], topology: &Topology) -> Result<Self> {
        if bytes.len() < HEADER || u32_at(bytes, transfer::VERSION) != ABI_VERSION {
            return Err(Error::Invalid);
        }
        let endpoint =
            u8::try_from(u32_at(bytes, transfer::ENDPOINT)).map_err(|_| Error::Invalid)?;
        let token = u64_at(bytes, transfer::TOKEN);
        let length = u32_at(bytes, transfer::LENGTH);
        let payload = u32_at(bytes, transfer::PAYLOAD_LENGTH) as usize;
        let setup: [u8; setup::BYTES] = bytes[transfer::SETUP..HEADER].try_into().unwrap();
        let input = if endpoint == 0 {
            setup[setup::REQUEST_TYPE] & descriptors::DIRECTION_IN != 0
        } else {
            endpoint & descriptors::DIRECTION_IN != 0
        };
        if token == 0
            || length as usize > MAX_TRANSFER
            || bytes.len() - HEADER != payload
            || payload != if input { 0 } else { length as usize }
            || (endpoint != 0
                && (!topology
                    .endpoints
                    .iter()
                    .flatten()
                    .any(|e| e.address == endpoint)
                    || setup != [0; setup::BYTES]))
            || (endpoint == 0 && u32::from(descriptors::u16_at(&setup, setup::LENGTH)) != length)
        {
            return Err(Error::Invalid);
        }
        Ok(Self {
            token,
            endpoint,
            length,
            setup,
            payload: &bytes[HEADER..],
        })
    }
}
pub(crate) fn u32_at(bytes: &[u8], offset: usize) -> u32 {
    u32::from_le_bytes(bytes[offset..offset + size_of::<u32>()].try_into().unwrap())
}

pub(crate) fn u64_at(bytes: &[u8], offset: usize) -> u64 {
    u64::from_le_bytes(bytes[offset..offset + size_of::<u64>()].try_into().unwrap())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn interrupt_period_tracks_speed_and_selected_alternate() {
        let budget = Budget::new(knightbus_core::ResourceLimits {
            bytes: 4096,
            requests: 4,
        });
        let device = [18, 1, 0, 2, 0, 0, 0, 64, 9, 0x12, 1, 0, 0, 1, 0, 0, 0, 1];
        let mut config = [
            9, 2, 41, 0, 1, 1, 0, 0x80, 50, 9, 4, 0, 0, 1, 0xff, 0, 0, 0, 7, 5, 0x81, 3, 0, 4, 7,
            9, 4, 0, 1, 1, 0xff, 0, 0, 0, 7, 5, 0x81, 3, 0, 4, 4,
        ];
        let d = Descriptors::new(&budget, &device, &config, Speed::High).unwrap();
        let mut alternates = [0; descriptors::MAX_SETTINGS];
        assert_eq!(d.interval(0x81, &alternates), Ok(64));
        alternates[0] = 1;
        assert_eq!(d.interval(0x81, &alternates), Ok(8));
        alternates[0] = 2;
        assert_eq!(d.interval(0x81, &alternates), Err(Error::Invalid));
        assert_eq!(d.interval(0, &alternates), Ok(0));
        config[22] = 8;
        config[23] = 0;
        config[38] = 8;
        config[39] = 0;
        let d = Descriptors::new(&budget, &device, &config, Speed::Full).unwrap();
        alternates[0] = 0;
        assert_eq!(d.interval(0x81, &alternates), Ok(7));
    }
}
