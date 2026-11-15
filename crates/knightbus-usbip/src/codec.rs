use crate::*;

// Management opcodes: requests set the high bit; replies clear it.
const OP_REQ_IMPORT: u16 = 0x8003;
const OP_REP_IMPORT: u16 = 0x0003;
const OP_REQ_DEVLIST: u16 = 0x8005;
const OP_REP_DEVLIST: u16 = 0x0005;

// Transfer commands: submit/cancel a USB transfer and report its completion.
const USBIP_CMD_SUBMIT: u32 = 1;
const USBIP_CMD_UNLINK: u32 = 2;
const USBIP_RET_SUBMIT: u32 = 3;
const USBIP_RET_UNLINK: u32 = 4;

fn add(a: usize, b: usize) -> Result<usize, Error> {
    a.checked_add(b).ok_or(Error::Limit("size overflow"))
}
fn mul(a: usize, b: usize) -> Result<usize, Error> {
    a.checked_mul(b).ok_or(Error::Limit("size overflow"))
}
fn bounded(n: usize, max: usize, what: &'static str) -> Result<usize, Error> {
    if n > max {
        Err(Error::Limit(what))
    } else {
        Ok(n)
    }
}
fn need(bytes: &[u8], n: usize, limits: Limits) -> Result<(), Error> {
    bounded(n, limits.frame_bytes, "frame bytes")?;
    if bytes.len() < n {
        Err(Error::Incomplete(n))
    } else {
        Ok(())
    }
}
fn u16_at(b: &[u8], p: usize) -> u16 {
    u16::from_be_bytes([b[p], b[p + 1]])
}
fn u32_at(b: &[u8], p: usize) -> u32 {
    u32::from_be_bytes([b[p], b[p + 1], b[p + 2], b[p + 3]])
}
fn i32_at(b: &[u8], p: usize) -> i32 {
    u32_at(b, p) as i32
}
fn zero(b: &[u8]) -> Result<(), Error> {
    if b.iter().any(|v| *v != 0) {
        Err(Error::Invalid("reserved bytes"))
    } else {
        Ok(())
    }
}
fn packet_count(n: i32, limits: Limits) -> Result<usize, Error> {
    if n < -1 {
        return Err(Error::Invalid("negative ISO count"));
    }
    bounded(n.max(0) as usize, limits.iso_packets, "ISO packets")
}
fn transfer_length(n: u32, limits: Limits) -> Result<usize, Error> {
    if n > i32::MAX as u32 {
        return Err(Error::Invalid("negative transfer length"));
    }
    bounded(n as usize, limits.transfer_bytes, "transfer bytes")
}
fn context_for(c: Context, seqnum: u32, limits: Limits) -> Result<ReplyContext, Error> {
    match c {
        Context::Replies(Some(r)) if r.seqnum == seqnum => {
            transfer_length(r.transfer_buffer_length, limits)?;
            packet_count(r.number_of_packets, limits)?;
            Ok(r)
        }
        _ => Err(Error::Context(seqnum)),
    }
}
// Charge the full new allocation before growth, including the old allocation's
// possible overlap during reallocation. Admission owners may conservatively keep
// charges until both decoder and returned message storage have been released.
pub(crate) fn reserve_capacity<T>(
    v: &mut Vec<T>,
    n: usize,
    admit: &mut dyn FnMut(usize) -> Result<(), Error>,
) -> Result<(), Error> {
    if n > v.capacity() {
        let size = std::mem::size_of::<T>();
        admit(n.checked_mul(size).ok_or(Error::Allocation)?)?;
        v.try_reserve_exact(n - v.len())
            .map_err(|_| Error::Allocation)?;
        let extra = (v.capacity() - n)
            .checked_mul(size)
            .ok_or(Error::Allocation)?;
        if extra != 0
            && let Err(error) = admit(extra)
        {
            // Do not retain an allocation whose excess capacity was denied.
            *v = Vec::new();
            return Err(error);
        }
    }
    Ok(())
}
fn reserve<T>(
    n: usize,
    admit: &mut dyn FnMut(usize) -> Result<(), Error>,
) -> Result<Vec<T>, Error> {
    let mut v = Vec::new();
    reserve_capacity(&mut v, n, admit)?;
    Ok(v)
}
fn copy(b: &[u8], admit: &mut dyn FnMut(usize) -> Result<(), Error>) -> Result<Vec<u8>, Error> {
    let mut v = reserve(b.len(), admit)?;
    v.extend_from_slice(b);
    Ok(v)
}

// Reused only for an append-only frame with unchanged context and limits.
#[derive(Debug, Default)]
pub(crate) struct FrameCursor {
    listed_devices: usize,
    list_end: usize,
}

// This preflight does no heap allocation. In particular, a malicious list count
// cannot allocate a list before the complete bounded body arrives. The cursor
// avoids rescanning completed device records as more bytes arrive.
pub(crate) fn frame_len(
    b: &[u8],
    context: Context,
    limits: Limits,
    cursor: &mut FrameCursor,
) -> Result<usize, Error> {
    match context {
        Context::Management => management_frame_len(b, limits, cursor),
        Context::Requests | Context::Replies(_) => transfer_frame_len(b, context, limits),
    }
}

fn management_frame_len(
    b: &[u8],
    limits: Limits,
    cursor: &mut FrameCursor,
) -> Result<usize, Error> {
    need(b, MANAGEMENT_HEADER_BYTES, limits)?;
    if u16_at(b, 0) != VERSION {
        return Err(Error::Invalid("version"));
    }
    let status = u32_at(b, 4);
    match u16_at(b, 2) {
        OP_REQ_DEVLIST | OP_REQ_IMPORT => {
            if status != 0 {
                return Err(Error::Invalid("request status"));
            }
            let n = if u16_at(b, 2) == OP_REQ_IMPORT {
                IMPORT_REQUEST_BYTES
            } else {
                MANAGEMENT_HEADER_BYTES
            };
            need(b, n, limits)?;
            Ok(n)
        }
        OP_REP_IMPORT => {
            let n = if status == 0 {
                IMPORT_REPLY_BYTES
            } else {
                MANAGEMENT_HEADER_BYTES
            };
            need(b, n, limits)?;
            Ok(n)
        }
        OP_REP_DEVLIST => {
            if status != 0 {
                return Ok(MANAGEMENT_HEADER_BYTES);
            }
            need(b, DEVLIST_HEADER_BYTES, limits)?;
            let count = bounded(
                u32_at(b, MANAGEMENT_HEADER_BYTES) as usize,
                limits.devices,
                "devices",
            )?;
            bounded(
                add(DEVLIST_HEADER_BYTES, mul(count, DEVICE_RECORD_BYTES)?)?,
                limits.frame_bytes,
                "device records",
            )?;
            let mut pos = cursor.list_end.max(DEVLIST_HEADER_BYTES);
            for _ in cursor.listed_devices..count {
                need(b, add(pos, DEVICE_RECORD_BYTES)?, limits)?;
                let end = add(
                    add(pos, DEVICE_RECORD_BYTES)?,
                    mul(
                        b[pos + DEVICE_RECORD_BYTES - 1] as usize,
                        INTERFACE_RECORD_BYTES,
                    )?,
                )?;
                need(b, end, limits)?;
                pos = end;
                cursor.list_end = end;
                cursor.listed_devices += 1;
            }
            Ok(pos)
        }
        _ => Err(Error::Invalid("management opcode")),
    }
}

fn transfer_frame_len(b: &[u8], context: Context, limits: Limits) -> Result<usize, Error> {
    need(b, TRANSFER_HEADER_LEN, limits)?;
    let command = u32_at(b, 0);
    let reply = matches!(context, Context::Replies(_));
    if !matches!(
        (command, reply),
        (USBIP_CMD_SUBMIT | USBIP_CMD_UNLINK, false) | (USBIP_RET_SUBMIT | USBIP_RET_UNLINK, true)
    ) {
        return Err(Error::Invalid("transfer command or phase"));
    }
    let direction = Direction::parse(u32_at(b, 12))?;
    if u32_at(b, 16) > 15 {
        return Err(Error::Invalid("endpoint"));
    }
    if reply {
        zero(&b[8..20])?;
    }
    if matches!(command, USBIP_CMD_UNLINK | USBIP_RET_UNLINK) {
        zero(&b[12..20])?;
        zero(&b[24..48])?;
        return Ok(48);
    }
    let length = transfer_length(u32_at(b, 24), limits)?;
    let count = packet_count(i32_at(b, 32), limits)?;
    let payload_len = if reply {
        zero(&b[40..48])?;
        let r = context_for(context, u32_at(b, 4), limits)?;
        if length > r.transfer_buffer_length as usize {
            return Err(Error::Invalid("completion exceeds request"));
        }
        if count != packet_count(r.number_of_packets, limits)? {
            return Err(Error::Invalid("completion ISO count"));
        }
        if i32_at(b, 36) < 0 || u32_at(b, 36) as usize > count {
            return Err(Error::Invalid("ISO error count"));
        }
        if r.direction == Direction::In {
            length
        } else {
            0
        }
    } else if direction == Direction::Out {
        length
    } else {
        0
    };
    let n = add(add(48, payload_len)?, mul(count, 16)?)?;
    need(b, n, limits)?;
    Ok(n)
}

fn device(b: &[u8]) -> Result<Device, Error> {
    Ok(Device {
        path: WireString::parse(&b[..256])?,
        bus_id: WireString::parse(&b[256..288])?,
        bus_number: u32_at(b, 288),
        device_number: u32_at(b, 292),
        speed: u32_at(b, 296),
        vendor_id: u16_at(b, 300),
        product_id: u16_at(b, 302),
        device_bcd: u16_at(b, 304),
        class: b[306],
        subclass: b[307],
        protocol: b[308],
        configuration: b[309],
        configurations: b[310],
        interfaces: b[311],
    })
}
fn iso(
    b: &[u8],
    capacity: u32,
    reply_length: Option<u32>,
    admit: &mut dyn FnMut(usize) -> Result<(), Error>,
) -> Result<Vec<IsoPacket>, Error> {
    let mut sum = 0u32;
    // Validate every descriptor before allocating its representation.
    for p in b.as_chunks::<16>().0 {
        let offset = u32_at(p, 0);
        let length = u32_at(p, 4);
        let actual = u32_at(p, 8);
        if offset.checked_add(length).is_none_or(|end| end > capacity) || actual > length {
            return Err(Error::Invalid("ISO packet range"));
        }
        sum = sum
            .checked_add(actual)
            .ok_or(Error::Invalid("ISO actual sum"))?;
    }
    if !b.is_empty() && reply_length.is_some_and(|n| n != sum) {
        return Err(Error::Invalid("ISO actual sum"));
    }
    let mut packets = reserve(b.len() / 16, admit)?;
    for p in b.as_chunks::<16>().0 {
        packets.push(IsoPacket {
            offset: u32_at(p, 0),
            length: u32_at(p, 4),
            actual_length: u32_at(p, 8),
            status: i32_at(p, 12),
        });
    }
    Ok(packets)
}

/// Decode one complete frame prefix, returning the message and bytes consumed.
///
/// `Incomplete(n)` means at least `n` total bytes are needed for the next parsing
/// step. No body allocation occurs until a complete, bounded frame is present.
/// Bytes after the frame are left to the caller. Other errors are fatal to the
/// connection; do not scan ahead for a plausible header.
/// Decode with caller-owned aggregate admission. The hook receives additional
/// allocation bytes before each allocation/growth, including parsing metadata.
/// Keep its charges alive through all returned storage; failures may also have
/// allocated storage. Existing input storage is the caller's separate charge.
/// Requested capacities are admitted first; allocator-reported excess is charged
/// before exposure. Allocator bookkeeping is not included.
pub fn decode(
    bytes: &[u8],
    context: Context,
    limits: Limits,
    admit: &mut dyn FnMut(usize) -> Result<(), Error>,
) -> Result<(Message, usize), Error> {
    let n = frame_len(bytes, context, limits, &mut FrameCursor::default())?;
    let b = &bytes[..n];
    let message = match context {
        Context::Management => management(b, admit)?,
        Context::Requests | Context::Replies(_) => {
            let body = &b[TRANSFER_HEADER_LEN..];
            transfer(
                &b[..TRANSFER_HEADER_LEN],
                body,
                context,
                limits,
                copy,
                admit,
            )?
        }
    };
    Ok((message, n))
}

// Framing has validated the opcode and complete message length.
fn management(
    b: &[u8],
    admit: &mut dyn FnMut(usize) -> Result<(), Error>,
) -> Result<Message, Error> {
    let status = u32_at(b, 4);
    let message = match u16_at(b, 2) {
        OP_REQ_DEVLIST => Message::DevListRequest,
        OP_REQ_IMPORT => Message::ImportRequest(WireString::parse(
            &b[MANAGEMENT_HEADER_BYTES..IMPORT_REQUEST_BYTES],
        )?),
        OP_REP_IMPORT => Message::ImportReply {
            status,
            device: if status == 0 {
                let device = device(&b[MANAGEMENT_HEADER_BYTES..IMPORT_REPLY_BYTES])?;
                admit(std::mem::size_of::<Device>())?;
                Some(Box::new(device))
            } else {
                None
            },
        },
        OP_REP_DEVLIST => {
            let count = if status == 0 {
                u32_at(b, MANAGEMENT_HEADER_BYTES) as usize
            } else {
                0
            };
            let mut devices = reserve(count, admit)?;
            let mut pos = DEVLIST_HEADER_BYTES;
            for _ in 0..count {
                let d = device(&b[pos..pos + DEVICE_RECORD_BYTES])?;
                pos += DEVICE_RECORD_BYTES;
                let mut interfaces = reserve(d.interfaces as usize, admit)?;
                for _ in 0..d.interfaces {
                    zero(&b[pos + 3..pos + 4])?;
                    interfaces.push(Interface {
                        class: b[pos],
                        subclass: b[pos + 1],
                        protocol: b[pos + 2],
                    });
                    pos += INTERFACE_RECORD_BYTES;
                }
                devices.push(ListedDevice {
                    device: d,
                    interfaces,
                });
            }
            Message::DevListReply { status, devices }
        }
        _ => unreachable!(),
    };
    Ok(message)
}

// Framing has validated header and body. Parse metadata before assigning data.
#[inline]
fn transfer(
    b: &[u8],
    body: &[u8],
    context: Context,
    limits: Limits,
    payload: impl FnOnce(&[u8], &mut dyn FnMut(usize) -> Result<(), Error>) -> Result<Vec<u8>, Error>,
    admit: &mut dyn FnMut(usize) -> Result<(), Error>,
) -> Result<Message, Error> {
    let seqnum = u32_at(b, 4);
    let device_id = u32_at(b, 8);
    let message = match u32_at(b, 0) {
        USBIP_CMD_SUBMIT => {
            let direction = Direction::parse(u32_at(b, 12))?;
            let length = u32_at(b, 24);
            let payload_len = if direction == Direction::Out {
                length as usize
            } else {
                0
            };
            let packets = iso(&body[payload_len..], length, None, admit)?;
            let mut setup = [0; 8];
            setup.copy_from_slice(&b[40..48]);
            Message::Submit(Submit {
                seqnum,
                device_id,
                direction,
                endpoint: u32_at(b, 16),
                transfer_flags: u32_at(b, 20),
                transfer_buffer_length: length,
                start_frame: i32_at(b, 28),
                number_of_packets: i32_at(b, 32),
                interval: i32_at(b, 36),
                setup,
                data: payload(&body[..payload_len], admit)?,
                iso_packets: packets,
            })
        }
        USBIP_RET_SUBMIT => {
            let request = context_for(context, seqnum, limits)?;
            let actual_length = u32_at(b, 24);
            let payload_len = if request.direction == Direction::In {
                actual_length as usize
            } else {
                0
            };
            let packets = iso(
                &body[payload_len..],
                request.transfer_buffer_length,
                Some(actual_length),
                admit,
            )?;
            Message::ReturnSubmit(ReturnSubmit {
                request,
                status: i32_at(b, 20),
                actual_length,
                start_frame: i32_at(b, 28),
                number_of_packets: i32_at(b, 32),
                error_count: u32_at(b, 36),
                data: payload(&body[..payload_len], admit)?,
                iso_packets: packets,
            })
        }
        USBIP_CMD_UNLINK => Message::Unlink {
            seqnum,
            device_id,
            target_seqnum: u32_at(b, 20),
        },
        USBIP_RET_UNLINK => Message::ReturnUnlink {
            seqnum,
            status: i32_at(b, 20),
        },
        _ => unreachable!(),
    };
    Ok(message)
}
fn set_payload(message: &mut Message, data: Vec<u8>) {
    match message {
        Message::Submit(s) => s.data = data,
        Message::ReturnSubmit(s) => s.data = data,
        _ => debug_assert!(data.is_empty()),
    }
}
// Unlike borrowed decode(), this takes ownership of the stream's body buffer
// so its allocation can become the payload. Both paths share transfer().
pub(crate) fn decode_owned_transfer(
    header: &[u8; TRANSFER_HEADER_LEN],
    mut body: Vec<u8>,
    context: Context,
    limits: Limits,
    admit: &mut dyn FnMut(usize) -> Result<(), Error>,
) -> Result<Message, Error> {
    if context == Context::Management {
        return Err(Error::Invalid("transfer phase"));
    }
    let n = match frame_len(header, context, limits, &mut FrameCursor::default()) {
        Ok(n) | Err(Error::Incomplete(n)) => n,
        Err(e) => return Err(e),
    };
    if n != add(TRANSFER_HEADER_LEN, body.len())? {
        return Err(Error::Invalid("transfer body length"));
    }
    let mut payload_len = 0;
    let mut message = transfer(
        header,
        &body,
        context,
        limits,
        |data, _| {
            payload_len = data.len();
            Ok(Vec::new())
        },
        admit,
    )?;
    if payload_len != 0 {
        // Keep ownership for substantial payloads. Copy small/sparse results
        // when unused capacity would exceed 1/8 of their useful payload size.
        let data = if body.capacity() - payload_len > payload_len / 8 {
            copy(&body[..payload_len], admit)?
        } else {
            body.truncate(payload_len);
            body
        };
        set_payload(&mut message, data);
    }
    Ok(message)
}

// One writer runs first in count-only mode, then in output mode. Size budgets and
// semantic invariants are checked before reserving any encoded body memory.
struct Writer {
    bytes: Option<Vec<u8>>,
    count: usize,
    limit: usize,
}
impl Writer {
    fn put(&mut self, b: &[u8]) -> Result<(), Error> {
        self.count = bounded(add(self.count, b.len())?, self.limit, "frame bytes")?;
        if let Some(v) = &mut self.bytes {
            v.extend_from_slice(b);
        }
        Ok(())
    }
    fn u32(&mut self, n: u32) -> Result<(), Error> {
        self.put(&n.to_be_bytes())
    }
    fn i32(&mut self, n: i32) -> Result<(), Error> {
        self.put(&n.to_be_bytes())
    }
    fn management(&mut self, code: u16, status: u32) -> Result<(), Error> {
        self.put(&VERSION.to_be_bytes())?;
        self.put(&code.to_be_bytes())?;
        self.u32(status)
    }
    fn basic(
        &mut self,
        code: u32,
        seq: u32,
        devid: u32,
        dir: Direction,
        ep: u32,
    ) -> Result<(), Error> {
        if ep > 15 {
            return Err(Error::Invalid("endpoint"));
        }
        for n in [code, seq, devid, dir.wire(), ep] {
            self.u32(n)?;
        }
        Ok(())
    }
    fn device(&mut self, d: &Device) -> Result<(), Error> {
        self.put(d.path.wire())?;
        self.put(d.bus_id.wire())?;
        for n in [d.bus_number, d.device_number, d.speed] {
            self.u32(n)?;
        }
        for n in [d.vendor_id, d.product_id, d.device_bcd] {
            self.put(&n.to_be_bytes())?;
        }
        self.put(&[
            d.class,
            d.subclass,
            d.protocol,
            d.configuration,
            d.configurations,
            d.interfaces,
        ])
    }
    fn iso(
        &mut self,
        packets: &[IsoPacket],
        count: i32,
        capacity: u32,
        actual: Option<u32>,
        limits: Limits,
    ) -> Result<(), Error> {
        if packet_count(count, limits)? != packets.len() {
            return Err(Error::Invalid("ISO count"));
        }
        let mut sum = 0u32;
        for p in packets {
            if p.offset
                .checked_add(p.length)
                .is_none_or(|end| end > capacity)
                || p.actual_length > p.length
            {
                return Err(Error::Invalid("ISO packet range"));
            }
            sum = sum
                .checked_add(p.actual_length)
                .ok_or(Error::Invalid("ISO actual sum"))?;
            self.u32(p.offset)?;
            self.u32(p.length)?;
            self.u32(p.actual_length)?;
            self.i32(p.status)?;
        }
        if !packets.is_empty() && actual.is_some_and(|a| a != sum) {
            return Err(Error::Invalid("ISO actual sum"));
        }
        Ok(())
    }
    fn submit(
        &mut self,
        s: &SubmitHeader,
        data: &[u8],
        packets: &[IsoPacket],
        limits: Limits,
    ) -> Result<(), Error> {
        transfer_length(s.transfer_buffer_length, limits)?;
        self.basic(
            USBIP_CMD_SUBMIT,
            s.seqnum,
            s.device_id,
            s.direction,
            s.endpoint,
        )?;
        self.u32(s.transfer_flags)?;
        self.u32(s.transfer_buffer_length)?;
        self.i32(s.start_frame)?;
        self.i32(s.number_of_packets)?;
        self.i32(s.interval)?;
        self.put(&s.setup)?;
        let expected = if s.direction == Direction::Out {
            s.transfer_buffer_length as usize
        } else {
            0
        };
        if data.len() != expected {
            return Err(Error::Invalid("submit payload length"));
        }
        self.put(data)?;
        self.iso(
            packets,
            s.number_of_packets,
            s.transfer_buffer_length,
            None,
            limits,
        )?;
        Ok(())
    }
    fn return_submit(
        &mut self,
        s: &ReturnHeader,
        data: &[u8],
        packets: &[IsoPacket],
        limits: Limits,
    ) -> Result<(), Error> {
        transfer_length(s.request.transfer_buffer_length, limits)?;
        transfer_length(s.actual_length, limits)?;
        let count = packet_count(s.number_of_packets, limits)?;
        if count != packet_count(s.request.number_of_packets, limits)? {
            return Err(Error::Invalid("completion ISO count"));
        }
        if s.actual_length > s.request.transfer_buffer_length {
            return Err(Error::Invalid("completion exceeds request"));
        }
        if s.error_count as usize > count {
            return Err(Error::Invalid("ISO error count"));
        }
        self.basic(USBIP_RET_SUBMIT, s.request.seqnum, 0, Direction::Out, 0)?;
        self.i32(s.status)?;
        self.u32(s.actual_length)?;
        self.i32(s.start_frame)?;
        self.i32(s.number_of_packets)?;
        self.u32(s.error_count)?;
        self.put(&[0; 8])?;
        let expected = if s.request.direction == Direction::In {
            s.actual_length as usize
        } else {
            0
        };
        if data.len() != expected {
            return Err(Error::Invalid("completion payload length"));
        }
        self.put(data)?;
        self.iso(
            packets,
            s.number_of_packets,
            s.request.transfer_buffer_length,
            Some(s.actual_length),
            limits,
        )?;
        Ok(())
    }
    fn message(&mut self, message: &Message, limits: Limits) -> Result<(), Error> {
        match message {
            Message::DevListRequest => self.management(OP_REQ_DEVLIST, 0)?,
            Message::ImportRequest(id) => {
                self.management(OP_REQ_IMPORT, 0)?;
                self.put(id.wire())?;
            }
            Message::DevListReply { status, devices } => {
                bounded(devices.len(), limits.devices, "devices")?;
                self.management(OP_REP_DEVLIST, *status)?;
                if *status != 0 {
                    if !devices.is_empty() {
                        return Err(Error::Invalid("error reply body"));
                    }
                } else {
                    self.u32(
                        u32::try_from(devices.len()).map_err(|_| Error::Limit("device count"))?,
                    )?;
                    for d in devices {
                        if d.interfaces.len() != d.device.interfaces as usize {
                            return Err(Error::Invalid("interface count"));
                        }
                        self.device(&d.device)?;
                        for i in &d.interfaces {
                            self.put(&[i.class, i.subclass, i.protocol, 0])?;
                        }
                    }
                }
            }
            Message::ImportReply { status, device } => {
                self.management(OP_REP_IMPORT, *status)?;
                match (*status, device) {
                    (0, Some(d)) => self.device(d)?,
                    (0, None) | (_, Some(_)) => return Err(Error::Invalid("import reply body")),
                    (_, None) => (),
                }
            }
            Message::Submit(s) => self.submit(&s.into(), &s.data, &s.iso_packets, limits)?,
            Message::ReturnSubmit(s) => {
                self.return_submit(&s.into(), &s.data, &s.iso_packets, limits)?
            }
            Message::Unlink {
                seqnum,
                device_id,
                target_seqnum,
            } => {
                self.basic(USBIP_CMD_UNLINK, *seqnum, *device_id, Direction::Out, 0)?;
                self.u32(*target_seqnum)?;
                self.put(&[0; 24])?;
            }
            Message::ReturnUnlink { seqnum, status } => {
                self.basic(USBIP_RET_UNLINK, *seqnum, 0, Direction::Out, 0)?;
                self.i32(*status)?;
                self.put(&[0; 24])?;
            }
        }
        Ok(())
    }
}
/// Validate and encode a frame in network order. Admission runs before allocation;
/// the caller retains its charge until the returned buffer is dropped.
/// Invalid messages produce no output and consume no allocation credits.
pub fn encode<'a>(
    frame: impl Into<Frame<'a>>,
    limits: Limits,
    admit: &mut dyn FnMut(usize) -> Result<(), Error>,
) -> Result<Vec<u8>, Error> {
    let frame = frame.into();
    encode_frame(limits, admit, |writer| match frame {
        Frame::Message(message) => writer.message(message, limits),
        Frame::ReturnSubmit {
            header,
            data,
            packets,
        } => writer.return_submit(&header, data, packets, limits),
        Frame::Submit {
            header,
            data,
            packets,
        } => writer.submit(header, data, packets, limits),
    })
}
fn encode_frame(
    limits: Limits,
    admit: &mut dyn FnMut(usize) -> Result<(), Error>,
    write: impl Fn(&mut Writer) -> Result<(), Error>,
) -> Result<Vec<u8>, Error> {
    let mut writer = Writer {
        bytes: None,
        count: 0,
        limit: limits.frame_bytes,
    };
    // With bytes: None, validate and count without storing output.
    write(&mut writer)?;
    // Allocate the measured size; Some(buffer) enables writing in put().
    writer.bytes = Some(reserve(writer.count, admit)?);
    writer.count = 0;
    // Repeat the same checks while writing into the reserved buffer.
    write(&mut writer)?;
    Ok(writer.bytes.unwrap_or_default())
}
