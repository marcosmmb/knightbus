//! Private driver ABI. Little-endian integers, exact lengths, no native pointers.
//! The driver independently checks ownership and request lifetimes.
use crate::descriptors::{self, Speed, Topology};
use knightbus_core::{Budget, Buffer, Error, Result};

#[cfg(windows)]
pub(crate) use knightbus_windows_protocol::wire::{
    ATTACH, DETACH, FETCH, REPLY, RETIREMENT_READY, STATS,
};
pub(crate) use knightbus_windows_protocol::wire::{HEADER, STATS_BYTES};
pub use knightbus_windows_protocol::wire::{MAX_TRANSFER, SLOTS};

// Offsets in the private driver ABI, defined by knightbus-windows-protocol.
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
const ATTACH_MAGIC: &[u8; 4] = b"KBWD";
const ATTACH_VERSION: usize = 4;
const ATTACH_SPEED: usize = 8;
const ATTACH_DEVICE_LENGTH: usize = 12;
const ATTACH_CONFIG_LENGTH: usize = 16;
const ATTACH_HEADER: usize = 20;
const ATTACH_CONFIG: usize = ATTACH_HEADER + descriptors::device::BYTES;

/// Validated raw descriptors and their charged driver attach representation.
pub struct Descriptors {
    pub(crate) bytes: Buffer,
    /// Allocation-free, validated metadata.
    pub topology: Topology,
    /// Actual physical speed, never inferred from USB revision.
    pub speed: Speed,
}
impl Descriptors {
    pub(crate) fn from_descriptor_set(
        budget: &Budget,
        descriptors: &knightbus_core::DescriptorSet,
    ) -> Result<Self> {
        Self::new(
            budget,
            descriptors.device(),
            descriptors.configuration(),
            descriptors.bos(),
            descriptors.speed(),
        )
    }

    /// Validate raw descriptors before allocation. Supply an empty BOS for USB2.
    pub fn new(
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
    pub setup: [u8; descriptors::SETUP_SIZE],
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
        let setup: [u8; descriptors::SETUP_SIZE] =
            bytes[transfer::SETUP..HEADER].try_into().unwrap();
        let input = if endpoint == 0 {
            setup[descriptors::SETUP_TYPE] & descriptors::DIRECTION_IN != 0
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
                    || setup != [0; descriptors::SETUP_SIZE]))
            || (endpoint == 0
                && u32::from(descriptors::u16_at(&setup, descriptors::SETUP_LENGTH)) != length)
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
