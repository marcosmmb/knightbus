//! USB/IP framing for the signed usbip-win2 driver on a private loopback socket.
use knightbus_core::{Error, Result, topology as usb, usbip};

pub(crate) const WORD: usize = size_of::<u32>();
pub(crate) const HEADER: usize = usbip::TRANSFER_HEADER_LEN;
pub(crate) const PACKET_BYTES: usize = packet::BYTES;
// usbip_iso_packet_descriptor fields, in network byte order.
pub(crate) mod packet {
    use super::WORD;
    pub const OFFSET: usize = 0;
    pub const LENGTH: usize = OFFSET + WORD;
    pub const ACTUAL_LENGTH: usize = LENGTH + WORD;
    pub const STATUS: usize = ACTUAL_LENGTH + WORD;
    pub const BYTES: usize = STATUS + WORD;
}
pub(crate) const MAX_TRANSFER: usize = 1024 * 1024;
pub(crate) const MAX_PACKETS: usize = 1024;
pub(crate) const SLOTS: usize = 31; // EP0 plus fifteen endpoints in each direction.
pub(crate) const MAX_MESSAGE: usize = HEADER + MAX_TRANSFER + MAX_PACKETS * PACKET_BYTES;
pub(crate) const MAX_QUEUED: usize = SLOTS;
pub(crate) const LOCAL_DEVICE_ID: u32 = 1; // Private to this loopback connection; never sent remotely.
pub(crate) const SHORT_NOT_OK: u32 = 1;
pub(crate) const SHORT_TRANSFER: i32 = -121; // Linux EREMOTEIO, understood by usbip-win2.
const NON_ISO_PACKETS: u32 = (-1i32) as u32; // usbip-win2 non-ISO sentinel.
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
pub(crate) fn word(bytes: &[u8], offset: usize) -> u32 {
    u32::from_be_bytes(bytes[offset..offset + WORD].try_into().unwrap())
}
pub(crate) fn put(bytes: &mut [u8], offset: usize, value: u32) {
    bytes[offset..offset + WORD].copy_from_slice(&value.to_be_bytes());
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
    let frame = decode_header(bytes)?;
    if bytes.len() != frame_length(&frame) {
        return Err(Error::Invalid);
    }
    Ok(frame)
}
fn frame_length(frame: &Frame) -> usize {
    match frame {
        Frame::Submit(h) => HEADER + h.payload + h.packets * PACKET_BYTES,
        Frame::Unlink { .. } => HEADER,
    }
}
fn decode_header(bytes: &[u8]) -> Result<Frame> {
    use field::*;
    if bytes.len() < HEADER || word(bytes, DEVICE) != LOCAL_DEVICE_ID || word(bytes, SEQUENCE) == 0
    {
        return Err(Error::Invalid);
    }
    let sequence = word(bytes, SEQUENCE);
    match word(bytes, COMMAND) {
        CMD_UNLINK if word(bytes, FLAGS_STATUS_TARGET) != 0 => Ok(Frame::Unlink {
            sequence,
            target: word(bytes, FLAGS_STATUS_TARGET),
        }),
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
            // usbip-win2 uses -1 for a non-isochronous request.
            let packets = match word(bytes, PACKETS) {
                NON_ISO_PACKETS => 0,
                count => count as usize,
            };
            let flags = word(bytes, FLAGS_STATUS_TARGET);
            let payload = if input { 0 } else { length as usize };
            if length as usize > MAX_TRANSFER
                || packets > MAX_PACKETS
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

/// Accumulate fragmented TCP headers and bounded payloads.
#[cfg(any(windows, test))]
pub(crate) struct ReadFrame {
    filled: usize,
    length: usize,
}
#[cfg(any(windows, test))]
impl Default for ReadFrame {
    fn default() -> Self {
        Self {
            filled: 0,
            length: HEADER,
        }
    }
}
#[cfg(any(windows, test))]
impl ReadFrame {
    pub fn remaining(&self) -> std::ops::Range<usize> {
        self.filled..self.length
    }
    pub fn advance(&mut self, buffer: &[u8], count: usize) -> Result<Option<usize>> {
        if count == 0 || count > self.length - self.filled || self.length > buffer.len() {
            return Err(Error::Invalid);
        }
        self.filled += count;
        if self.length == HEADER && self.filled == HEADER {
            self.length = frame_length(&decode_header(&buffer[..HEADER])?);
            if self.length > buffer.len() {
                return Err(Error::Invalid);
            }
        }
        if self.filled == self.length {
            let length = self.length;
            *self = Self::default();
            Ok(Some(length))
        } else {
            Ok(None)
        }
    }
}
