use crate::*;

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
fn reserve<T>(n: usize) -> Result<Vec<T>, Error> {
    let mut v = Vec::new();
    v.try_reserve_exact(n).map_err(|_| Error::Allocation)?;
    Ok(v)
}
fn copy(b: &[u8]) -> Result<Vec<u8>, Error> {
    let mut v = reserve(b.len())?;
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
    if context == Context::Management {
        need(b, 8, limits)?;
        if u16_at(b, 0) != VERSION {
            return Err(Error::Invalid("version"));
        }
        let status = u32_at(b, 4);
        match u16_at(b, 2) {
            0x8005 | 0x8003 => {
                if status != 0 {
                    return Err(Error::Invalid("request status"));
                }
                let n = if u16_at(b, 2) == 0x8003 { 40 } else { 8 };
                need(b, n, limits)?;
                Ok(n)
            }
            3 => {
                let n = if status == 0 { 320 } else { 8 };
                need(b, n, limits)?;
                Ok(n)
            }
            5 => {
                if status != 0 {
                    return Ok(8);
                }
                need(b, 12, limits)?;
                let count = bounded(u32_at(b, 8) as usize, limits.devices, "devices")?;
                bounded(
                    add(12, mul(count, 312)?)?,
                    limits.frame_bytes,
                    "device records",
                )?;
                let mut pos = cursor.list_end.max(12);
                for _ in cursor.listed_devices..count {
                    need(b, add(pos, 312)?, limits)?;
                    let end = add(add(pos, 312)?, mul(b[pos + 311] as usize, 4)?)?;
                    need(b, end, limits)?;
                    pos = end;
                    cursor.list_end = end;
                    cursor.listed_devices += 1;
                }
                Ok(pos)
            }
            _ => Err(Error::Invalid("management opcode")),
        }
    } else {
        need(b, TRANSFER_HEADER_LEN, limits)?;
        let command = u32_at(b, 0);
        let reply = matches!(context, Context::Replies(_));
        if !matches!((command, reply), (1 | 2, false) | (3 | 4, true)) {
            return Err(Error::Invalid("transfer command or phase"));
        }
        let direction = Direction::parse(u32_at(b, 12))?;
        if u32_at(b, 16) > 15 {
            return Err(Error::Invalid("endpoint"));
        }
        if reply {
            zero(&b[8..20])?;
        }
        if command == 2 || command == 4 {
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
fn iso(b: &[u8], capacity: u32, reply_length: Option<u32>) -> Result<Vec<IsoPacket>, Error> {
    let mut sum = 0u32;
    // Validate every descriptor before allocating its representation.
    for p in b.chunks_exact(16) {
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
    let mut packets = reserve(b.len() / 16)?;
    for p in b.chunks_exact(16) {
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
pub fn decode(bytes: &[u8], context: Context, limits: Limits) -> Result<(Message, usize), Error> {
    let n = frame_len(bytes, context, limits, &mut FrameCursor::default())?;
    let b = &bytes[..n];
    let message = if context == Context::Management {
        let status = u32_at(b, 4);
        match u16_at(b, 2) {
            0x8005 => Message::DevListRequest,
            0x8003 => Message::ImportRequest(WireString::parse(&b[8..40])?),
            3 => Message::ImportReply {
                status,
                device: if status == 0 {
                    Some(Box::new(device(&b[8..320])?))
                } else {
                    None
                },
            },
            5 => {
                let count = if status == 0 {
                    u32_at(b, 8) as usize
                } else {
                    0
                };
                let mut devices = reserve(count)?;
                let mut pos = 12;
                for _ in 0..count {
                    let d = device(&b[pos..pos + 312])?;
                    pos += 312;
                    let mut interfaces = reserve(d.interfaces as usize)?;
                    for _ in 0..d.interfaces {
                        zero(&b[pos + 3..pos + 4])?;
                        interfaces.push(Interface {
                            class: b[pos],
                            subclass: b[pos + 1],
                            protocol: b[pos + 2],
                        });
                        pos += 4;
                    }
                    devices.push(ListedDevice {
                        device: d,
                        interfaces,
                    });
                }
                Message::DevListReply { status, devices }
            }
            _ => unreachable!(),
        }
    } else {
        let body = &b[TRANSFER_HEADER_LEN..];
        transfer(&b[..TRANSFER_HEADER_LEN], body, context, limits, copy)?
    };
    Ok((message, n))
}

// Framing has validated header and body. Parse metadata before assigning data.
#[inline]
fn transfer(
    b: &[u8],
    body: &[u8],
    context: Context,
    limits: Limits,
    payload: impl FnOnce(&[u8]) -> Result<Vec<u8>, Error>,
) -> Result<Message, Error> {
    let seqnum = u32_at(b, 4);
    let device_id = u32_at(b, 8);
    let message = match u32_at(b, 0) {
        1 => {
            let direction = Direction::parse(u32_at(b, 12))?;
            let length = u32_at(b, 24);
            let payload_len = if direction == Direction::Out {
                length as usize
            } else {
                0
            };
            let packets = iso(&body[payload_len..], length, None)?;
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
                data: payload(&body[..payload_len])?,
                iso_packets: packets,
            })
        }
        3 => {
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
            )?;
            Message::ReturnSubmit(ReturnSubmit {
                request,
                status: i32_at(b, 20),
                actual_length,
                start_frame: i32_at(b, 28),
                number_of_packets: i32_at(b, 32),
                error_count: u32_at(b, 36),
                data: payload(&body[..payload_len])?,
                iso_packets: packets,
            })
        }
        2 => Message::Unlink {
            seqnum,
            device_id,
            target_seqnum: u32_at(b, 20),
        },
        4 => Message::ReturnUnlink {
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
pub(crate) fn decode_owned_transfer(
    header: &[u8; TRANSFER_HEADER_LEN],
    mut body: Vec<u8>,
    context: Context,
    limits: Limits,
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
    let mut message = transfer(header, &body, context, limits, |data| {
        payload_len = data.len();
        Ok(Vec::new())
    })?;
    if payload_len != 0 {
        // Keep ownership for substantial payloads. Copy small/sparse results
        // when unused capacity would exceed 1/8 of their useful payload size.
        let data = if body.capacity() - payload_len > payload_len / 8 {
            copy(&body[..payload_len])?
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
    fn message(&mut self, message: &Message, limits: Limits) -> Result<(), Error> {
        match message {
            Message::DevListRequest => self.management(0x8005, 0)?,
            Message::ImportRequest(id) => {
                self.management(0x8003, 0)?;
                self.put(id.wire())?;
            }
            Message::DevListReply { status, devices } => {
                bounded(devices.len(), limits.devices, "devices")?;
                self.management(5, *status)?;
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
                self.management(3, *status)?;
                match (*status, device) {
                    (0, Some(d)) => self.device(d)?,
                    (0, None) | (_, Some(_)) => return Err(Error::Invalid("import reply body")),
                    (_, None) => (),
                }
            }
            Message::Submit(s) => {
                transfer_length(s.transfer_buffer_length, limits)?;
                self.basic(1, s.seqnum, s.device_id, s.direction, s.endpoint)?;
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
                if s.data.len() != expected {
                    return Err(Error::Invalid("submit payload length"));
                }
                self.put(&s.data)?;
                self.iso(
                    &s.iso_packets,
                    s.number_of_packets,
                    s.transfer_buffer_length,
                    None,
                    limits,
                )?;
            }
            Message::ReturnSubmit(s) => {
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
                self.basic(3, s.request.seqnum, 0, Direction::Out, 0)?;
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
                if s.data.len() != expected {
                    return Err(Error::Invalid("completion payload length"));
                }
                self.put(&s.data)?;
                self.iso(
                    &s.iso_packets,
                    s.number_of_packets,
                    s.request.transfer_buffer_length,
                    Some(s.actual_length),
                    limits,
                )?;
            }
            Message::Unlink {
                seqnum,
                device_id,
                target_seqnum,
            } => {
                self.basic(2, *seqnum, *device_id, Direction::Out, 0)?;
                self.u32(*target_seqnum)?;
                self.put(&[0; 24])?;
            }
            Message::ReturnUnlink { seqnum, status } => {
                self.basic(4, *seqnum, 0, Direction::Out, 0)?;
                self.i32(*status)?;
                self.put(&[0; 24])?;
            }
        }
        Ok(())
    }
}
/// Encode a validated message in canonical network order. No partial output is
/// returned on error. Encoding allocates only after size and semantic validation.
pub fn encode(message: &Message, limits: Limits) -> Result<Vec<u8>, Error> {
    let mut writer = Writer {
        bytes: None,
        count: 0,
        limit: limits.frame_bytes,
    };
    writer.message(message, limits)?;
    writer.bytes = Some(reserve(writer.count)?);
    writer.count = 0;
    writer.message(message, limits)?;
    Ok(writer.bytes.unwrap_or_default())
}
