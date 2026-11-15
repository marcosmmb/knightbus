//! Bounded packet records for the private ISO transfer envelope.
use crate::wire::{Error, MAX_TRANSFER};
use knightbus_usb as usb;

pub const VERSION: u32 = 2;
pub const HEADER: usize = 48;
pub const MAX_PACKETS: usize = 1024;
pub const PACKET_BYTES: usize = 16;
pub const MAX_MESSAGE: usize = HEADER + MAX_TRANSFER + MAX_PACKETS * PACKET_BYTES;
pub const ASAP: u32 = 2;
/// Exclusive upper bound for a requested start frame, in milliseconds.
const START_FRAME_WINDOW: i32 = 1024;
const MAX_TRANSFER_FRAMES: u32 = 1024;
const MAX_REQUESTS_PER_PIPE: u32 = 8;
const WORD_BYTES: usize = core::mem::size_of::<u32>();

/// Fields appended to the ordinary transfer header.
pub mod field {
    pub const PACKET_COUNT: usize = 32;
    pub const FLAGS: usize = 36;
    pub const START_FRAME: usize = 40;
    pub const SLOT: usize = 44;
    pub const ERRORS: usize = SLOT; // Replies replace the request slot with an error count.
}
mod packet_field {
    pub const OFFSET: usize = 0;
    pub const LENGTH: usize = 4;
    pub const ACTUAL: usize = 8;
    pub const STATUS: usize = 12;
}

/// Windows accepts frame numbers within 1024 milliseconds of its bus clock.
/// Wrapping subtraction also handles the 32-bit clock rollover.
pub fn start_frame(now: u32, requested: Option<u32>) -> Result<u32, Error> {
    let frame = requested.unwrap_or_else(|| now.wrapping_add(1));
    if !(0..START_FRAME_WINDOW).contains(&(frame.wrapping_sub(now) as i32)) {
        return Err(Error::Fields);
    }
    Ok(frame)
}

/// Milliseconds until a virtual bus frame, used to hold early completions.
pub fn delay(now: u32, frame: u32) -> u32 {
    (frame.wrapping_sub(now) as i32).max(0) as u32
}

/// Timing for a completed native request, independent of the OS timer.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CompletionTiming {
    Immediate,
    Scheduled(u32),
}
impl CompletionTiming {
    pub fn waiting(self, now: u32) -> bool {
        matches!(self, Self::Scheduled(frame) if delay(now, frame) != 0)
    }
    /// Rearm even if the deadline expired after the completion scan.
    /// None means no timed completion; Some is a positive millisecond delay.
    pub fn wake_after(self, now: u32) -> Option<u32> {
        match self {
            Self::Immediate => None,
            Self::Scheduled(frame) => Some(delay(now, frame).max(1)),
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Packet {
    pub offset: u32,
    pub length: u32,
    pub actual: u32,
    pub status: i32,
}
impl Packet {
    pub fn write(self, bytes: &mut [u8]) {
        for (chunk, value) in bytes[..PACKET_BYTES].chunks_exact_mut(WORD_BYTES).zip([
            self.offset,
            self.length,
            self.actual,
            self.status as u32,
        ]) {
            chunk.copy_from_slice(&value.to_le_bytes());
        }
    }
}
#[derive(Clone, Copy, Debug)]
pub struct Packets<'a>(&'a [u8]);
impl<'a> Packets<'a> {
    pub fn decode(bytes: &'a [u8], capacity: usize, reply: bool) -> Result<Self, Error> {
        if bytes.is_empty()
            || bytes.len() % PACKET_BYTES != 0
            || bytes.len() / PACKET_BYTES > MAX_PACKETS
            || capacity > MAX_TRANSFER
        {
            return Err(Error::Length);
        }
        let packets = Self(bytes);
        let mut end = 0;
        for p in packets.iter() {
            let next = p.offset.checked_add(p.length).ok_or(Error::Fields)?;
            if p.offset < end
                || next as usize > capacity
                || p.actual > p.length
                || p.status > 0
                || (!reply && (p.actual != 0 || p.status != 0))
            {
                return Err(Error::Fields);
            }
            end = next;
        }
        Ok(packets)
    }
    pub fn len(self) -> usize {
        self.0.len() / PACKET_BYTES
    }
    pub fn is_empty(self) -> bool {
        self.0.is_empty()
    }
    pub fn iter(self) -> impl Iterator<Item = Packet> + 'a {
        self.0.chunks_exact(PACKET_BYTES).map(|p| Packet {
            offset: word(p, packet_field::OFFSET),
            length: word(p, packet_field::LENGTH),
            actual: word(p, packet_field::ACTUAL),
            status: word(p, packet_field::STATUS) as i32,
        })
    }
    pub fn validate_reply(
        self,
        original: &[Packet],
        actual: usize,
        errors: u32,
    ) -> Result<(), Error> {
        if self.len() != original.len() {
            return Err(Error::Fields);
        }
        let mut total = 0usize;
        let mut failed = 0;
        for (p, request) in self.iter().zip(original) {
            if p.offset != request.offset || p.length != request.length {
                return Err(Error::Fields);
            }
            total = total.checked_add(p.actual as usize).ok_or(Error::Length)?;
            failed += u32::from(p.status != 0);
        }
        if total != actual || failed != errors {
            return Err(Error::Fields);
        }
        Ok(())
    }
}
pub(crate) fn word(bytes: &[u8], offset: usize) -> u32 {
    u32::from_le_bytes(bytes[offset..offset + WORD_BYTES].try_into().unwrap())
}

/// Reserve one slot per endpoint, then distribute spare slots among ISO pipes.
/// An alternate may change an address's type; its original slot remains reserved.
pub fn slot_masks(topology: &crate::descriptors::Topology) -> [u32; crate::wire::SLOTS] {
    let mut masks = [0u32; crate::wire::SLOTS];
    let mut iso = [false; crate::wire::SLOTS];
    masks[0] = 1;
    let mut count = 1;
    for (i, endpoint) in topology.endpoints.iter().flatten().enumerate() {
        count += 1;
        masks[i + 1] = 1 << (i + 1);
        let bit = (endpoint.address & usb::ENDPOINT_NUMBER_MASK)
            + if endpoint.address & usb::DIRECTION_IN != 0 {
                usb::IN_ENDPOINT_BIT_OFFSET
            } else {
                0
            };
        iso[i + 1] = topology.iso_endpoints & (1 << bit) != 0;
    }
    let mut spare = count;
    while spare < masks.len() {
        let before = spare;
        for i in 1..count {
            if spare == masks.len() {
                break;
            }
            if iso[i] && masks[i].count_ones() < MAX_REQUESTS_PER_PIPE {
                masks[i] |= 1 << spare;
                spare += 1;
            }
        }
        if spare == before {
            break;
        }
    }
    masks
}

/// Controller-local scheduling state for one native endpoint queue.
#[derive(Default)]
pub struct Schedule {
    next: Option<u32>,
}
impl Schedule {
    pub fn reserve(
        &mut self,
        now: u32,
        requested: Option<u32>,
        packets: u32,
        interval: u8,
        high_speed: bool,
    ) -> Result<(u32, u32), Error> {
        if packets == 0
            || packets as usize > MAX_PACKETS
            || !(1..=usb::MAX_PERIODIC_INTERVAL).contains(&interval)
        {
            return Err(Error::Fields);
        }
        let ticks = packets << (interval - 1);
        let frames = if high_speed {
            ticks.div_ceil(usb::MICROFRAMES_PER_FRAME)
        } else {
            ticks
        };
        if frames > MAX_TRANSFER_FRAMES {
            return Err(Error::Fields);
        }
        let requested =
            requested.or_else(|| self.next.filter(|next| (next.wrapping_sub(now) as i32) > 0));
        // A future tail must survive even outside the start window. Reject the
        // new request without overwriting it; only an expired tail starts anew.
        let start = start_frame(now, requested)?;
        let end = start.wrapping_add(frames);
        self.next = Some(end);
        Ok((start, end))
    }
}
