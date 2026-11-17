//! usbip-win 0.3.6 UDE ABI. Integers on the device handle are little-endian.
//! Network framing remains the responsibility of knightbus-transport.
use knightbus_core::{Budget, Buffer, DescriptorSet, Error, Result, topology as usb, usbip};

pub(crate) const WORD: usize = size_of::<u32>();
pub(crate) const HEADER: usize = usbip::TRANSFER_HEADER_LEN;
pub(crate) const PACKET_BYTES: usize = 4 * WORD;
pub(crate) const MAX_TRANSFER: usize = 1024 * 1024;
pub(crate) const MAX_PACKETS: usize = 1024;
pub(crate) const SLOTS: usize = 31; // EP0 plus fifteen endpoints in each direction.
pub(crate) const MAX_MESSAGE: usize = HEADER + MAX_TRANSFER + MAX_PACKETS * PACKET_BYTES;
pub(crate) const MAX_QUEUED: usize = SLOTS;
pub(crate) const LOCAL_DEVICE_ID: u32 = 1; // Private to this driver handle; never sent remotely.
pub(crate) const SHORT_NOT_OK: u32 = 1;
pub(crate) const SHORT_TRANSFER: i32 = -121; // Linux EREMOTEIO, understood by usbip-win.
pub(crate) const CMD_SUBMIT: u32 = 1;
pub(crate) const CMD_UNLINK: u32 = 2;
pub(crate) const RET_SUBMIT: u32 = 3;
pub(crate) const RET_UNLINK: u32 = 4;
// usbip_header fields, shared by the upstream read/write ABI and USB/IP.
pub(crate) mod field {
    pub const COMMAND: usize = 0;
    pub const SEQUENCE: usize = 4;
    pub const DEVICE: usize = 8;
    pub const DIRECTION: usize = 12;
    pub const ENDPOINT: usize = 16;
    pub const FLAGS_STATUS_TARGET: usize = 20;
    pub const LENGTH: usize = 24;
    pub const FRAME: usize = 28;
    pub const PACKETS: usize = 32;
    pub const INTERVAL_ERRORS: usize = 36;
    pub const SETUP: usize = 40;
}
// Layout of vhci_pluginfo_t in upstream include/usbip_vhci_api.h (Windows ABI).
pub(crate) const PLUG_SIZE: usize = 296;
pub(crate) const PLUG_DEVICE: usize = 4;
#[cfg(windows)]
pub(crate) const PLUG_PORT: usize = 8;
const PLUG_DESCRIPTOR: usize = 266;
const PLUG_CONFIGURATION: usize = PLUG_DESCRIPTOR + usb::device::BYTES;
const USB_1_0: u16 = 0x0100;
const USB_1_1: u16 = 0x0110;
const USB_2_0: u16 = 0x0200;
// The UDE ABI encodes link speed through bcdUSB, not a separate speed field.
pub(crate) fn virtual_revision(speed: usb::Speed) -> Result<u16> {
    match speed {
        usb::Speed::Low => Ok(USB_1_0),
        usb::Speed::Full => Ok(USB_1_1),
        usb::Speed::High => Ok(USB_2_0),
        _ => Err(Error::Unsupported),
    }
}
pub(crate) fn patch_revision(bytes: &mut [u8], revision: u16) {
    for (offset, byte) in revision.to_le_bytes().into_iter().enumerate() {
        if let Some(out) = bytes.get_mut(usb::device::USB_REVISION + offset) {
            *out = byte;
        }
    }
}

pub(crate) fn plugin(descriptors: &DescriptorSet, budget: &Budget) -> Result<Buffer> {
    let revision = virtual_revision(descriptors.speed())?;
    if !descriptors.bos().is_empty() {
        return Err(Error::Unsupported);
    }
    let size = PLUG_SIZE + descriptors.configuration().len() - usb::configuration::BYTES;
    let mut bytes = budget.buffer(size)?;
    let out = bytes.as_mut_slice();
    out.fill(0);
    put(out, 0, size as u32);
    put(out, PLUG_DEVICE, LOCAL_DEVICE_ID);
    out[PLUG_DESCRIPTOR..PLUG_CONFIGURATION].copy_from_slice(descriptors.device());
    patch_revision(&mut out[PLUG_DESCRIPTOR..PLUG_CONFIGURATION], revision);
    out[PLUG_CONFIGURATION..PLUG_CONFIGURATION + descriptors.configuration().len()]
        .copy_from_slice(descriptors.configuration());
    Ok(bytes)
}
pub(crate) fn word(bytes: &[u8], offset: usize) -> u32 {
    u32::from_le_bytes(bytes[offset..offset + WORD].try_into().unwrap())
}
pub(crate) fn put(bytes: &mut [u8], offset: usize, value: u32) {
    bytes[offset..offset + WORD].copy_from_slice(&value.to_le_bytes());
}
#[derive(Clone, Copy, Debug)]
pub(crate) struct Header {
    pub sequence: u32,
    pub endpoint: u8,
    pub input: bool,
    pub length: u32,
    pub flags: u32,
    pub packets: usize,
    pub payload: usize,
    pub setup: [u8; usb::SETUP_SIZE],
}
pub(crate) enum Frame {
    Submit(Header),
    Unlink { sequence: u32, target: u32 },
}
pub(crate) fn decode(bytes: &[u8]) -> Result<Frame> {
    use field::*;
    if bytes.len() < HEADER || word(bytes, DEVICE) != LOCAL_DEVICE_ID || word(bytes, SEQUENCE) == 0
    {
        return Err(Error::Invalid);
    }
    let sequence = word(bytes, SEQUENCE);
    match word(bytes, COMMAND) {
        CMD_UNLINK if bytes.len() == HEADER && word(bytes, FLAGS_STATUS_TARGET) != 0 => {
            Ok(Frame::Unlink {
                sequence,
                target: word(bytes, FLAGS_STATUS_TARGET),
            })
        }
        CMD_SUBMIT => {
            let input = match word(bytes, DIRECTION) {
                0 => false,
                1 => true,
                _ => return Err(Error::Invalid),
            };
            let number = word(bytes, ENDPOINT);
            if number > u32::from(usb::ENDPOINT_NUMBER_MASK) {
                return Err(Error::Invalid);
            }
            let endpoint = number as u8
                | if input && number != 0 {
                    usb::DIRECTION_IN
                } else {
                    0
                };
            let length = word(bytes, LENGTH);
            let packets = word(bytes, PACKETS) as usize;
            let flags = word(bytes, FLAGS_STATUS_TARGET);
            let payload = if input { 0 } else { length as usize };
            if length as usize > MAX_TRANSFER
                || packets > MAX_PACKETS
                || bytes.len() != HEADER + payload + packets * PACKET_BYTES
                || flags & !(SHORT_NOT_OK | usbip::ISO_ASAP) != 0
                || (packets != 0 && (endpoint == 0 || flags & usbip::ISO_ASAP == 0))
                || (packets == 0 && flags & usbip::ISO_ASAP != 0)
            {
                return Err(Error::Unsupported);
            }
            // The upstream driver leaves setup/padding uninitialized on data endpoints.
            let setup = if endpoint == 0 {
                bytes[SETUP..HEADER].try_into().unwrap()
            } else {
                [0; usb::SETUP_SIZE]
            };
            if endpoint == 0
                && ((setup[usb::SETUP_TYPE] & usb::DIRECTION_IN != 0) != input
                    || u32::from(usb::u16_at(&setup, usb::SETUP_LENGTH)) != length)
            {
                return Err(Error::Invalid);
            }
            Ok(Frame::Submit(Header {
                sequence,
                endpoint,
                input,
                length,
                flags,
                packets,
                payload,
                setup,
            }))
        }
        _ => Err(Error::Invalid),
    }
}
