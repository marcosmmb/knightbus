use std::fmt;

/// USB/IP URB_ISO_ASAP transfer flag.
pub const ISO_ASAP: u32 = 2;
/// Linux URB_DIR_IN, redundant with the USB/IP direction field.
pub const DIR_IN: u32 = 0x0200;
/// Bus-number shift in the USB/IP device identifier.
pub const DEVICE_BUS_SHIFT: u32 = 16;

/// USB/IP 1.1.1 management version.
pub const VERSION: u16 = 0x0111;
/// Fixed transfer header size, including command-specific padding.
pub const TRANSFER_HEADER_LEN: usize = 48;

/// Common management header: version, operation and status.
pub const MANAGEMENT_HEADER_BYTES: usize = size_of::<u16>() + size_of::<u16>() + size_of::<u32>();
/// Fixed-width bus identifier, including its NUL terminator and padding.
pub const BUS_ID_BYTES: usize = 32;
/// Encoded device metadata, excluding trailing interface records.
pub const DEVICE_RECORD_BYTES: usize = 312;
/// Encoded interface metadata, including its reserved byte.
pub const INTERFACE_RECORD_BYTES: usize = 4;
/// DEVLIST reply prefix, including the device count.
pub const DEVLIST_HEADER_BYTES: usize = MANAGEMENT_HEADER_BYTES + size_of::<u32>();
/// Successful IMPORT reply, containing one device record.
pub const IMPORT_REPLY_BYTES: usize = MANAGEMENT_HEADER_BYTES + DEVICE_RECORD_BYTES;
/// IMPORT request, containing a fixed-width bus identifier.
pub const IMPORT_REQUEST_BYTES: usize = MANAGEMENT_HEADER_BYTES + BUS_ID_BYTES;
/// Interface records per device are counted by one unsigned byte.
pub const MAX_DEVICE_INTERFACES: usize = u8::MAX as usize;

/// Codec failure. A malformed stream must be closed, not resynchronized.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Error {
    /// More bytes are needed; minimum total prefix size, not additional bytes.
    Incomplete(usize),
    /// Invalid wire field or inconsistent message.
    Invalid(&'static str),
    /// Configured resource budget exceeded.
    Limit(&'static str),
    /// Allocation failed without consuming a message.
    Allocation,
    /// Original request context is absent or does not match this sequence.
    Context(u32),
    /// Decoder previously encountered a fatal error.
    Poisoned,
}
impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{self:?}")
    }
}
impl std::error::Error for Error {}

/// Per-frame resource budgets, independent of platform support.
#[derive(Clone, Copy, Debug)]
pub struct Limits {
    /// Maximum encoded frame bytes, including headers and descriptors.
    pub frame_bytes: usize,
    /// Maximum requested buffer or returned data bytes.
    pub transfer_bytes: usize,
    /// Maximum devices in a list reply.
    pub devices: usize,
    /// Maximum ISO descriptors per transfer.
    pub iso_packets: usize,
}
impl Default for Limits {
    fn default() -> Self {
        Self {
            frame_bytes: 2 * 1024 * 1024,
            transfer_bytes: 1024 * 1024,
            devices: 256,
            iso_packets: 1024,
        }
    }
}

/// Fixed-width, NUL-terminated, zero-padded byte string; not necessarily UTF-8.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WireString<const N: usize>([u8; N]);
impl<const N: usize> WireString<N> {
    /// Construct from bytes without a terminator. Embedded NUL is rejected.
    pub fn new(value: &[u8]) -> Result<Self, Error> {
        if value.len() >= N || value.contains(&0) {
            return Err(Error::Invalid("wire string"));
        }
        let mut bytes = [0; N];
        bytes[..value.len()].copy_from_slice(value);
        Ok(Self(bytes))
    }
    pub(crate) fn parse(bytes: &[u8]) -> Result<Self, Error> {
        let end = bytes
            .iter()
            .position(|b| *b == 0)
            .ok_or(Error::Invalid("unterminated string"))?;
        if bytes.len() != N || bytes[end..].iter().any(|b| *b != 0) {
            return Err(Error::Invalid("string padding"));
        }
        Self::new(&bytes[..end])
    }
    /// Meaningful bytes, excluding the terminator and padding.
    pub fn as_bytes(&self) -> &[u8] {
        let end = self.0.iter().position(|b| *b == 0).unwrap_or(N);
        &self.0[..end]
    }
    pub(crate) fn wire(&self) -> &[u8] {
        &self.0
    }
}

/// USB/IP device record (312 bytes). IMPORT has no trailing interface records.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Device {
    /// Exporter path.
    pub path: WireString<256>,
    /// Exporter bus ID.
    pub bus_id: WireString<BUS_ID_BYTES>,
    /// USB bus number.
    pub bus_number: u32,
    /// USB device number.
    pub device_number: u32,
    /// USB speed code, preserved for forward compatibility.
    pub speed: u32,
    /// Vendor identifier.
    pub vendor_id: u16,
    /// Product identifier.
    pub product_id: u16,
    /// BCD device revision.
    pub device_bcd: u16,
    /// Device class.
    pub class: u8,
    /// Device subclass.
    pub subclass: u8,
    /// Device protocol.
    pub protocol: u8,
    /// Active configuration.
    pub configuration: u8,
    /// Number of configurations.
    pub configurations: u8,
    /// Number of interfaces in the active configuration.
    pub interfaces: u8,
}
/// Four-byte device-list interface record (reserved fourth byte is zero).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Interface {
    /// Interface class.
    pub class: u8,
    /// Interface subclass.
    pub subclass: u8,
    /// Interface protocol.
    pub protocol: u8,
}
/// Device-list entry with exactly the advertised interface count.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ListedDevice {
    /// Fixed device record.
    pub device: Device,
    /// Interface records.
    pub interfaces: Vec<Interface>,
}
/// USB transfer direction, independent of which peer sends the frame.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Direction {
    /// Host to device.
    Out,
    /// Device to host.
    In,
}
impl Direction {
    pub(crate) fn wire(self) -> u32 {
        u32::from(self == Self::In)
    }
    pub(crate) fn parse(n: u32) -> Result<Self, Error> {
        match n {
            0 => Ok(Self::Out),
            1 => Ok(Self::In),
            _ => Err(Error::Invalid("direction")),
        }
    }
}
/// ISO descriptor. Offsets address the original transfer buffer, not packed data.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct IsoPacket {
    /// Offset in original buffer.
    pub offset: u32,
    /// Requested packet length.
    pub length: u32,
    /// Completed packet length.
    pub actual_length: u32,
    /// Signed USB status, preserved without mapping to host OS errors.
    pub status: i32,
}
/// Request fields for control, bulk, interrupt or ISO transfers.
/// Transfer type comes from endpoint metadata; USB/IP has no transfer-type field.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Submit {
    /// Connection-local request sequence.
    pub seqnum: u32,
    /// (bus number << 16) | device number.
    pub device_id: u32,
    /// Transfer direction.
    pub direction: Direction,
    /// Endpoint number, without direction bit (0..=15).
    pub endpoint: u32,
    /// Raw USB/IP URB flags. Backend must validate supported flags separately.
    pub transfer_flags: u32,
    /// Requested buffer size, including ISO offset space.
    pub transfer_buffer_length: u32,
    /// Scheduling field; signed, including sentinel values used by peers.
    pub start_frame: i32,
    /// -1 or 0 for non-ISO; positive descriptor count for ISO.
    pub number_of_packets: i32,
    /// Scheduling interval, preserved as a signed wire value.
    pub interval: i32,
    /// Opaque USB setup bytes (USB little-endian fields are not swapped).
    pub setup: [u8; 8],
    /// OUT bytes only; IN requests carry no payload.
    pub data: Vec<u8>,
    /// ISO descriptors in wire order.
    pub iso_packets: Vec<IsoPacket>,
}
/// Allocation-free SUBMIT metadata, shared by the wire and native transfer paths.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SubmitHeader {
    /// Connection-local request sequence.
    pub seqnum: u32,
    /// (bus number << 16) | device number.
    pub device_id: u32,
    /// Transfer direction.
    pub direction: Direction,
    /// Endpoint number, without direction bit (0..=15).
    pub endpoint: u32,
    /// Raw USB/IP URB flags. Backend must validate supported flags separately.
    pub transfer_flags: u32,
    /// Requested buffer size, including ISO offset space.
    pub transfer_buffer_length: u32,
    /// Scheduling field; signed, including sentinel values used by peers.
    pub start_frame: i32,
    /// -1 or 0 for non-ISO; positive descriptor count for ISO.
    pub number_of_packets: i32,
    /// Scheduling interval, preserved as a signed wire value.
    pub interval: i32,
    /// Opaque USB setup bytes (USB little-endian fields are not swapped).
    pub setup: [u8; 8],
}
impl From<&Submit> for SubmitHeader {
    fn from(s: &Submit) -> Self {
        Self {
            seqnum: s.seqnum,
            device_id: s.device_id,
            direction: s.direction,
            endpoint: s.endpoint,
            transfer_flags: s.transfer_flags,
            transfer_buffer_length: s.transfer_buffer_length,
            start_frame: s.start_frame,
            number_of_packets: s.number_of_packets,
            interval: s.interval,
            setup: s.setup,
        }
    }
}
impl SubmitHeader {
    /// Combine metadata with already owned payload/ISO storage without copying.
    pub fn with_payload(self, data: Vec<u8>, iso_packets: Vec<IsoPacket>) -> Submit {
        Submit {
            seqnum: self.seqnum,
            device_id: self.device_id,
            direction: self.direction,
            endpoint: self.endpoint,
            transfer_flags: self.transfer_flags,
            transfer_buffer_length: self.transfer_buffer_length,
            start_frame: self.start_frame,
            number_of_packets: self.number_of_packets,
            interval: self.interval,
            setup: self.setup,
            data,
            iso_packets,
        }
    }
}
impl Submit {
    /// Split ownership from metadata without cloning any payload or ISO storage.
    pub fn into_parts(self) -> (SubmitHeader, Vec<u8>, Vec<IsoPacket>) {
        ((&self).into(), self.data, self.iso_packets)
    }
}
/// Original submit metadata required to frame and bound a completion.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ReplyContext {
    /// Expected submit sequence.
    pub seqnum: u32,
    /// Original transfer direction.
    pub direction: Direction,
    /// Original requested capacity.
    pub transfer_buffer_length: u32,
    /// Original ISO count (0 and -1 are equivalent non-ISO representations).
    pub number_of_packets: i32,
}
impl From<&Submit> for ReplyContext {
    fn from(s: &Submit) -> Self {
        Self::from(&SubmitHeader::from(s))
    }
}
impl From<&SubmitHeader> for ReplyContext {
    fn from(s: &SubmitHeader) -> Self {
        Self {
            seqnum: s.seqnum,
            direction: s.direction,
            transfer_buffer_length: s.transfer_buffer_length,
            number_of_packets: s.number_of_packets,
        }
    }
}
/// Completed submit. Device, direction and endpoint fields encode as zero.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ReturnSubmit {
    /// Original request context, not additional wire fields.
    pub request: ReplyContext,
    /// Signed completion status.
    pub status: i32,
    /// Actual transferred length, including for OUT completions without data.
    pub actual_length: u32,
    /// Completion scheduling field.
    pub start_frame: i32,
    /// Wire descriptor count; -1 and 0 are preserved.
    pub number_of_packets: i32,
    /// ISO error count.
    pub error_count: u32,
    /// IN completion data only, packed for ISO.
    pub data: Vec<u8>,
    /// ISO completion descriptors.
    pub iso_packets: Vec<IsoPacket>,
}
/// Completion fields without owned payload or ISO storage.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ReturnHeader {
    /// Original request bounds and sequence.
    pub request: ReplyContext,
    /// Signed USB completion status.
    pub status: i32,
    /// Bytes transferred, including OUT completions.
    pub actual_length: u32,
    /// Completion scheduling field.
    pub start_frame: i32,
    /// Wire ISO count; -1 and 0 both mean non-ISO.
    pub number_of_packets: i32,
    /// Number of failed ISO packets.
    pub error_count: u32,
}
impl From<&ReturnSubmit> for ReturnHeader {
    fn from(reply: &ReturnSubmit) -> Self {
        Self {
            request: reply.request,
            status: reply.status,
            actual_length: reply.actual_length,
            start_frame: reply.start_frame,
            number_of_packets: reply.number_of_packets,
            error_count: reply.error_count,
        }
    }
}
/// All eight standard messages. Management error replies have no body.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Message {
    /// List exported devices.
    DevListRequest,
    /// List result; nonzero status requires an empty device list.
    DevListReply {
        /// Zero for success, otherwise a peer-defined error status.
        status: u32,
        /// Advertised device and interface records.
        devices: Vec<ListedDevice>,
    },
    /// Import this bus ID.
    ImportRequest(WireString<BUS_ID_BYTES>),
    /// Import result; device is present exactly when status is zero.
    ImportReply {
        /// Zero for success, otherwise a peer-defined error status.
        status: u32,
        /// Imported device; no interface records follow this record.
        device: Option<Box<Device>>,
    },
    /// Submit a transfer.
    Submit(Submit),
    /// Complete a transfer.
    ReturnSubmit(ReturnSubmit),
    /// Cancel target sequence; this message has its own sequence.
    Unlink {
        /// Sequence of this unlink request.
        seqnum: u32,
        /// Imported device identifier.
        device_id: u32,
        /// Sequence of the submit to cancel.
        target_seqnum: u32,
    },
    /// Cancel result. -104 means canceled; 0 means completion won the race.
    ReturnUnlink {
        /// Sequence of the unlink request, not its target submit.
        seqnum: u32,
        /// Signed cancellation result.
        status: i32,
    },
}
/// Expected stream phase and peer direction. No automatic phase guessing.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Context {
    /// Device-list/import exchange, before transfer mode.
    Management,
    /// Importer-to-exporter submit/unlink stream.
    Requests,
    /// Exporter-to-importer completions; context needed for RET_SUBMIT only.
    Replies(Option<ReplyContext>),
}

/// Borrowed encoding input. Native transfers can supply their existing buffers.
pub enum Frame<'a> {
    /// Any complete protocol message.
    Message(&'a Message),
    /// A SUBMIT without copying its payload into an owned message.
    Submit {
        /// Original request fields.
        header: &'a SubmitHeader,
        /// OUT data; empty for IN requests.
        data: &'a [u8],
        /// ISO layout; empty for other transfer types.
        packets: &'a [IsoPacket],
    },
    /// A completion borrowing a retired transfer's storage.
    ReturnSubmit {
        /// Completion fields and original request bounds.
        header: ReturnHeader,
        /// IN bytes in wire order; empty for OUT completions.
        data: &'a [u8],
        /// Completed ISO packet descriptors.
        packets: &'a [IsoPacket],
    },
}
impl<'a> From<&'a Message> for Frame<'a> {
    fn from(message: &'a Message) -> Self {
        Self::Message(message)
    }
}
