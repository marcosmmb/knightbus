//! Private METHOD_BUFFERED ABI. All integers are little endian.
use crate::{
    descriptors::{self, Speed},
    iso,
};

pub const MAX_TRANSFER: usize = 1024 * 1024;
pub const SLOTS: usize = descriptors::MAX_ENDPOINTS + 1;
pub const HEADER: usize = 32;
pub const STATS_BYTES: usize = (8 + SLOTS) * 8;
pub const SLOT_MASK: u32 = (1 << SLOTS) - 1;
pub const ATTACH_HEADER: usize = 20;
pub const ATTACH_CONFIG: usize = ATTACH_HEADER + 18;
pub const ATTACH_MAX: usize = ATTACH_CONFIG + descriptors::MAX_CONFIGURATION + descriptors::MAX_BOS;

// FILE_DEVICE_UNKNOWN, METHOD_BUFFERED, read and write access required.
pub const FETCH: u32 = 0x0022_e404;
pub const REPLY: u32 = 0x0022_e408;
pub const DETACH: u32 = 0x0022_e40c;
pub const STATS: u32 = 0x0022_e410;
pub const ATTACH: u32 = 0x0022_e424;
pub const RETIREMENT_READY: u32 = 0x0022_e42c;
/// Register an owner-only wake event (u64 handle); zero unregisters it.
pub const NOTIFY: u32 = 0x0022_e430;
/// Require bounded ISO envelopes and final-destruction acknowledgement.
pub const TRANSFER_READY: u32 = 0x0022_e434;

/// Invalid message size or field. Native status mapping belongs to the caller.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Error {
    Length,
    Fields,
}

/// Complete validated attachment, borrowing the caller's descriptor storage.
pub struct Attachment<'a> {
    pub device: &'a [u8],
    pub configuration: &'a [u8],
    pub bos: &'a [u8],
    pub speed: Speed,
    /// Endpoint addresses by native slot; slot zero is the default pipe.
    pub endpoints: [u8; SLOTS],
    pub iso_slots: [u32; SLOTS],
    pub has_iso: bool,
}
impl<'a> Attachment<'a> {
    pub fn decode(bytes: &'a [u8]) -> Result<Self, Error> {
        if !(ATTACH_CONFIG + 9..=ATTACH_MAX).contains(&bytes.len()) {
            return Err(Error::Length);
        }
        let version = u32_at(bytes, 4);
        let config_len = u32_at(bytes, 16) as usize;
        if &bytes[..4] != b"KBWD"
            || !matches!(version, 1 | 2)
            || u32_at(bytes, 12) != 18
            || config_len > descriptors::MAX_CONFIGURATION
            || ATTACH_CONFIG + config_len > bytes.len()
            || (version == 1 && ATTACH_CONFIG + config_len != bytes.len())
        {
            return Err(Error::Fields);
        }
        let speed = Speed::try_from(u32_at(bytes, 8)).map_err(|_| Error::Fields)?;
        let device = &bytes[ATTACH_HEADER..ATTACH_CONFIG];
        let configuration = &bytes[ATTACH_CONFIG..ATTACH_CONFIG + config_len];
        let bos = &bytes[ATTACH_CONFIG + config_len..];
        // Keep the large topology local; only the endpoint map crosses this boundary.
        let topology = match descriptors::validate(device, configuration, speed) {
            Ok(topology) => topology,
            Err(_) => return Err(Error::Fields),
        };
        let mut endpoints = [0; SLOTS];
        for (slot, endpoint) in topology.endpoints.iter().flatten().enumerate() {
            endpoints[slot + 1] = endpoint.address;
        }
        descriptors::validate_bos(bos, speed).map_err(|_| Error::Fields)?;
        Ok(Self {
            device,
            configuration,
            bos,
            speed,
            endpoints,
            iso_slots: iso::slot_masks(&topology),
            has_iso: topology.iso_endpoints != 0,
        })
    }
}

/// Validated completion envelope. Slot identity and length are checked separately
/// while the driver holds its cancellation lock.
pub struct Reply<'a> {
    pub endpoint: u32,
    pub token: u64,
    pub completed: usize,
    pub status: u32,
    pub payload: &'a [u8],
    pub packets: Option<iso::Packets<'a>>,
    pub start_frame: u32,
    pub errors: u32,
}
impl<'a> Reply<'a> {
    /// Hold successful ISO results until their reserved end frame. Failed URBs
    /// and ordinary transfers complete immediately; mixed packet results retain
    /// their timing because the overall ISO transfer still succeeded.
    pub fn completion_timing(&self, due_frame: u32) -> iso::CompletionTiming {
        if self.status == 0
            && self
                .packets
                .is_some_and(|packets| (self.errors as usize) < packets.len())
        {
            iso::CompletionTiming::Scheduled(due_frame)
        } else {
            iso::CompletionTiming::Immediate
        }
    }
    pub fn decode(bytes: &'a [u8]) -> Result<Self, Error> {
        if !(HEADER..=iso::MAX_MESSAGE).contains(&bytes.len()) {
            return Err(Error::Length);
        }
        let payload = u32_at(bytes, 20) as usize;
        let status = u32_at(bytes, 24);
        let version = u32_at(bytes, 0);
        let (header, packets, start_frame, errors) = match version {
            1 => (HEADER, None, 0, 0),
            iso::VERSION if bytes.len() >= iso::HEADER => {
                let count = u32_at(bytes, iso::field::PACKET_COUNT) as usize;
                if count == 0
                    || count > iso::MAX_PACKETS
                    || u32_at(bytes, iso::field::FLAGS) != 0
                    || payload > MAX_TRANSFER
                    || iso::HEADER + payload + count * iso::PACKET_BYTES != bytes.len()
                {
                    return Err(Error::Fields);
                }
                (
                    iso::HEADER,
                    Some(iso::Packets::decode(
                        &bytes[iso::HEADER + payload..],
                        MAX_TRANSFER,
                        true,
                    )?),
                    u32_at(bytes, iso::field::START_FRAME),
                    u32_at(bytes, iso::field::ERRORS),
                )
            }
            _ => return Err(Error::Fields),
        };
        if payload > MAX_TRANSFER
            || (packets.is_none() && header + payload != bytes.len())
            || status > 2
            || bytes[28..32] != [0; 4]
        {
            return Err(Error::Fields);
        }
        Ok(Self {
            endpoint: u32_at(bytes, 4),
            token: u64::from_le_bytes(bytes[8..16].try_into().unwrap()),
            completed: u32_at(bytes, 16) as usize,
            status,
            payload: &bytes[header..header + payload],
            packets,
            start_frame,
            errors,
        })
    }
    pub fn validate_length(&self, requested: usize, input: bool) -> Result<(), Error> {
        if self.completed > requested
            || (self.packets.is_none()
                && self.status != 0
                && (self.completed != 0 || !self.payload.is_empty()))
            || (input && self.completed != self.payload.len())
            || (!input && !self.payload.is_empty())
        {
            return Err(Error::Length);
        }
        Ok(())
    }
}

pub fn fetch_mask(bytes: &[u8]) -> Result<u32, Error> {
    if bytes.is_empty() {
        return Ok(SLOT_MASK);
    }
    if bytes.len() != 4 {
        return Err(Error::Length);
    }
    let mask = u32_at(bytes, 0);
    if mask == 0 || mask & !SLOT_MASK != 0 {
        return Err(Error::Fields);
    }
    Ok(mask)
}
fn u32_at(bytes: &[u8], offset: usize) -> u32 {
    u32::from_le_bytes(bytes[offset..offset + 4].try_into().unwrap())
}
