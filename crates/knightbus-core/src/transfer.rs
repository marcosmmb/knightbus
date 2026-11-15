use crate::{
    Budget, Buffer, Device, Error, Result, TransferKind,
    resource::Credit,
    usbip::{self, Direction, IsoPacket, SubmitHeader},
};

/// Owned native request. Header uses USB/IP types; payload and ISO storage are
/// admitted before allocation. No clone or uncharged ownership extraction exists.
#[derive(Debug)]
pub struct Transfer {
    header: SubmitHeader,
    token: Option<crate::TransferId>,
    buffer: Buffer,
    layout: Vec<IsoPacket>,
    packets: Vec<IsoPacket>,
    credit: Credit,
}
impl Transfer {
    /// Prepare native storage from allocation-free metadata. Supply the original
    /// ISO layout separately; it is frozen.
    /// IN storage starts zeroed; OUT callers fill `buffer_mut` before submission.
    pub fn new(budget: &Budget, header: SubmitHeader, layout: &[IsoPacket]) -> Result<Self> {
        Self::prepare(budget, header, layout, None)
    }
    /// Move already charged transport/parser storage into a native transfer,
    /// without copying its payload or releasing its byte credits in between.
    /// The initialized length must match the requested native buffer size.
    pub fn from_buffer(
        budget: &Budget,
        header: SubmitHeader,
        layout: &[IsoPacket],
        buffer: Buffer,
    ) -> Result<Self> {
        if !buffer.budget_matches(budget)
            || buffer.as_slice().len() != header.transfer_buffer_length as usize
        {
            return Err(Error::Invalid);
        }
        Self::prepare(budget, header, layout, Some(buffer))
    }
    fn prepare(
        budget: &Budget,
        header: SubmitHeader,
        layout: &[IsoPacket],
        buffer: Option<Buffer>,
    ) -> Result<Self> {
        if header.seqnum == 0
            || header.endpoint > 15
            || header.number_of_packets < -1
            || header.number_of_packets.max(0) as usize != layout.len()
            || header.transfer_buffer_length > i32::MAX as u32
            || (header.transfer_flags & usbip::DIR_IN != 0 && header.direction != Direction::In)
        {
            return Err(Error::Invalid);
        }
        let mut end = 0u32;
        for p in layout {
            let next = p.offset.checked_add(p.length).ok_or(Error::Invalid)?;
            if p.offset < end
                || next > header.transfer_buffer_length
                || p.actual_length != 0
                || !matches!(p.status, 0 | usbip::status::NOT_ACCESSED)
            {
                return Err(Error::Invalid);
            }
            end = next;
        }
        let metadata = layout
            .len()
            .checked_mul(2 * std::mem::size_of::<IsoPacket>())
            .and_then(|n| n.checked_add(std::mem::size_of::<Self>()))
            .ok_or(Error::Exhausted)?;
        let mut credit = budget.reserve(metadata, 1)?;
        let buffer = match buffer {
            Some(buffer) => buffer,
            None => budget.buffer(header.transfer_buffer_length as usize)?,
        };
        let mut original = Vec::new();
        let mut packets = Vec::new();
        original
            .try_reserve_exact(layout.len())
            .map_err(|_| Error::Exhausted)?;
        packets
            .try_reserve_exact(layout.len())
            .map_err(|_| Error::Exhausted)?;
        credit.grow(
            (original.capacity() + packets.capacity() - 2 * layout.len())
                * std::mem::size_of::<IsoPacket>(),
        )?;
        // Linux initializes pending ISO packets to -EXDEV before submission.
        // Only packet ranges are input; native result fields start empty.
        original.extend(layout.iter().cloned().map(|mut p| {
            p.status = 0;
            p
        }));
        packets.extend_from_slice(&original);
        Ok(Self {
            header,
            token: None,
            buffer,
            layout: original,
            packets,
            credit,
        })
    }
    /// Immutable original wire metadata, without payload or packet allocations.
    pub fn header(&self) -> &SubmitHeader {
        &self.header
    }
    /// Original immutable ISO layout. Completions must preserve these identities.
    pub fn layout(&self) -> &[IsoPacket] {
        &self.layout
    }
    /// Native buffer, in original ISO offset layout (not wire-packed).
    pub fn buffer(&self) -> &[u8] {
        self.buffer.as_slice()
    }
    /// Exclusive initialized buffer access. Backend must retain this owner while
    /// any native operation can access its storage.
    pub fn buffer_mut(&mut self) -> &mut [u8] {
        self.buffer.as_mut_slice()
    }
    /// Charge platform scratch storage to this transfer's aggregate budget.
    /// Keep the returned guard until all native access has retired.
    pub fn reserve_native_memory(&self, bytes: usize) -> Result<crate::MemoryReservation> {
        self.credit.budget.reserve_memory(bytes)
    }
    /// ISO completion descriptors; identity/bounds rechecked at retirement.
    pub fn packets_mut(&mut self) -> &mut [IsoPacket] {
        &mut self.packets
    }
    /// Completed ISO descriptors in original request order.
    pub fn packets(&self) -> &[IsoPacket] {
        &self.packets
    }
    /// Validate a native submission against the currently selected endpoint.
    /// The device worker orders topology changes before this check. Native
    /// adapters provide their actual selected endpoint and preserve OS ownership.
    pub fn validate_native(
        &self,
        wire_id: u32,
        endpoint: Option<&crate::Endpoint>,
        capabilities: crate::Capabilities,
    ) -> Result<u8> {
        use knightbus_usb as usb;
        let t = self;
        let h = t.header();
        if h.endpoint > u32::from(usb::ENDPOINT_NUMBER_MASK)
            || h.transfer_buffer_length as usize != t.buffer().len()
            || t.buffer().len() > capabilities.transfer_bytes
            || t.layout().len() > capabilities.iso_packets
            || h.device_id != wire_id
        {
            return Err(Error::Invalid);
        }
        if h.endpoint == 0 {
            if h.transfer_flags & !usbip::DIR_IN != 0
                || !t.layout().is_empty()
                || usize::from(u16::from_le_bytes([
                    h.setup[usb::SETUP_LENGTH],
                    h.setup[usb::SETUP_LENGTH + 1],
                ])) != t.buffer().len()
                || (h.setup[usb::SETUP_TYPE] & usb::DIRECTION_IN != 0)
                    != (h.direction == Direction::In)
            {
                return Err(Error::Invalid);
            }
            if h.setup[usb::SETUP_TYPE] & usb::REQUEST_TYPE_MASK == 0
                && h.setup[usb::SETUP_REQUEST] == usb::SET_ADDRESS
            {
                return Err(Error::Unsupported);
            }
            return Ok(0);
        }
        let address = h.endpoint as u8
            | if h.direction == Direction::In {
                usb::DIRECTION_IN
            } else {
                0
            };
        let e = endpoint
            .filter(|e| e.address == address)
            .ok_or(Error::Invalid)?;
        if h.setup != [0; usb::SETUP_SIZE]
            || h.transfer_flags & !(e.allowed_flags | usbip::DIR_IN) != 0
            || (e.kind == TransferKind::Isochronous) == t.layout().is_empty()
        {
            return Err(Error::Unsupported);
        }
        if e.kind == TransferKind::Isochronous {
            // USB/IP frame numbers are importer-local. Only ASAP is safely
            // translatable without an explicit cross-host frame-clock contract.
            if h.transfer_flags & crate::usbip::ISO_ASAP == 0 {
                return Err(Error::Unsupported);
            }
            if t.layout().iter().any(|p| p.length > e.service_bytes) {
                return Err(Error::Invalid);
            }
        }
        Ok(address)
    }
    pub(crate) fn bind(&mut self, token: crate::TransferId) -> Result<()> {
        if self.token.is_some() {
            return Err(Error::State);
        }
        self.token = Some(token);
        Ok(())
    }
    pub(crate) fn token(&self) -> Option<crate::TransferId> {
        self.token
    }
    pub(crate) fn budget_matches(&self, budget: &Budget) -> bool {
        self.credit.budget.same(budget)
    }
    pub(crate) fn retire(&mut self) {
        self.credit.retire();
    }
    pub(crate) fn validate(&self, device: &Device, managed: bool) -> Result<()> {
        let s = &self.header;
        if s.device_id != device.wire_id() {
            return Err(Error::Invalid);
        }
        // Dynamic backends validate endpoint existence/type/flags against their
        // live topology at ordered submission, including queued configuration
        // changes. Authorization, budgets and correlation remain engine-owned.
        if managed && s.endpoint != 0 {
            return if s.setup == [0; 8] {
                Ok(())
            } else {
                Err(Error::Invalid)
            };
        }
        let (kind, flags) = if s.endpoint == 0 {
            (TransferKind::Control, 0)
        } else {
            let e = device
                .endpoint(s.endpoint, s.direction)
                .ok_or(Error::Invalid)?;
            (e.kind, e.allowed_flags)
        };
        if s.transfer_flags & !(flags | usbip::DIR_IN) != 0
            || ((kind == TransferKind::Isochronous) == self.layout.is_empty())
        {
            return Err(Error::Unsupported);
        }
        if kind == TransferKind::Control {
            let direction = if s.setup[0] & 0x80 != 0 {
                Direction::In
            } else {
                Direction::Out
            };
            if direction != s.direction
                || u32::from(u16::from_le_bytes([s.setup[6], s.setup[7]]))
                    != s.transfer_buffer_length
            {
                return Err(Error::Invalid);
            }
            // Configuration/alternate changes require topology coordination, not
            // a raw transfer that could invalidate outstanding endpoint requests.
            if s.setup[0] & 0x60 == 0
                && (s.setup[1] == 5 || (!managed && matches!(s.setup[1], 9 | 11)))
            {
                return Err(Error::Unsupported);
            }
        } else if s.setup != [0; 8] {
            return Err(Error::Invalid);
        }
        Ok(())
    }
}
/// Backend retirement: returning this owner guarantees no native callback or
/// DMA/transport operation can still access it. Cancellation alone cannot do so.
#[derive(Debug)]
pub struct Completion {
    /// Original owned transfer, including frozen correlation and layout.
    pub transfer: Transfer,
    /// Native status normalized to USB/IP signed status.
    pub status: i32,
    /// Total actual length (packed length for ISO).
    pub actual_length: u32,
    /// Result frame number.
    pub start_frame: i32,
}
impl Completion {
    pub(crate) fn validate(&self) -> Result<()> {
        let t = &self.transfer;
        if self.status > 0 || self.actual_length > t.header.transfer_buffer_length {
            return Err(Error::Invalid);
        }
        let mut total = 0u32;
        for (p, original) in t.packets.iter().zip(&t.layout) {
            if p.offset != original.offset
                || p.length != original.length
                || p.actual_length > p.length
                || p.status > 0
            {
                return Err(Error::Invalid);
            }
            total = total.checked_add(p.actual_length).ok_or(Error::Invalid)?;
        }
        if !t.layout.is_empty() && total != self.actual_length {
            return Err(Error::Invalid);
        }
        Ok(())
    }
    /// Validate reply data and scatter packed ISO input into its native ranges.
    /// The caller resolves wire sequence IDs, which may differ from native IDs.
    pub fn from_reply(mut transfer: Transfer, reply: &usbip::ReturnSubmit) -> Result<Self> {
        let h = transfer.header();
        if reply.status > 0
            || reply.actual_length > h.transfer_buffer_length
            || reply.request.number_of_packets.max(0) != h.number_of_packets.max(0)
            || reply.request.direction != h.direction
            || reply.request.transfer_buffer_length != h.transfer_buffer_length
            || reply.number_of_packets < -1
            || reply.error_count
                != reply.iso_packets.iter().filter(|p| p.status != 0).count() as u32
            || reply.number_of_packets.max(0) as usize != transfer.layout().len()
            || reply.iso_packets.len() != transfer.layout().len()
            || reply.data.len()
                != if h.direction == Direction::In {
                    reply.actual_length as usize
                } else {
                    0
                }
        {
            return Err(Error::Invalid);
        }
        for (packet, original) in reply.iso_packets.iter().zip(transfer.layout()) {
            if packet.offset != original.offset
                || packet.length != original.length
                || packet.actual_length > packet.length
            {
                return Err(Error::Invalid);
            }
        }
        transfer.packets_mut().clone_from_slice(&reply.iso_packets);
        let mut done = Self {
            transfer,
            status: reply.status,
            actual_length: reply.actual_length,
            start_frame: reply.start_frame,
        };
        done.validate()?;
        if done.transfer.header().direction == Direction::In {
            if done.transfer.layout().is_empty() {
                done.transfer.buffer_mut()[..reply.data.len()].copy_from_slice(&reply.data);
            } else {
                let mut packed = 0;
                for i in 0..done.transfer.packets().len() {
                    let p = &done.transfer.packets()[i];
                    let (offset, count) = (p.offset as usize, p.actual_length as usize);
                    done.transfer.buffer_mut()[offset..offset + count]
                        .copy_from_slice(&reply.data[packed..packed + count]);
                    packed += count;
                }
            }
        }
        Ok(done)
    }
    /// Copy completed input bytes in USB/IP packet order, omitting unused gaps.
    pub fn copy_payload(&self, output: &mut [u8]) -> Result<()> {
        self.validate()?;
        let input = self.transfer.header().direction == Direction::In;
        if output.len()
            != if input {
                self.actual_length as usize
            } else {
                0
            }
        {
            return Err(Error::Invalid);
        }
        if !input {
            return Ok(());
        }
        if self.transfer.layout().is_empty() {
            output.copy_from_slice(&self.transfer.buffer()[..output.len()]);
        } else {
            let mut cursor = 0;
            for p in self.transfer.packets() {
                let count = p.actual_length as usize;
                output[cursor..cursor + count].copy_from_slice(
                    &self.transfer.buffer()[p.offset as usize..p.offset as usize + count],
                );
                cursor += count;
            }
        }
        Ok(())
    }
    /// Original context for the unchanged USB/IP codec.
    pub fn reply_context(&self) -> usbip::ReplyContext {
        self.transfer.header().into()
    }
}
/// Engine result after native retirement; canceled targets have no submit reply.
#[derive(Debug)]
#[allow(clippy::large_enum_variant)] // Inline in a prepaid bounded queue; no completion-time allocation.
pub enum TransferEvent {
    /// Validated normal completion; storage credits follow its owner to transport.
    Complete(Completion),
    /// Successful UNLINK, emitted only after the native target retired.
    Unlinked {
        /// UNLINK's own sequence, not the target sequence.
        sequence: u32,
    },
    /// Transfer deadline/cancellation without a wire unlink.
    Failed {
        /// Original submit sequence.
        sequence: u32,
        /// Stable sanitized cause.
        error: Error,
    },
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ResourceLimits;

    #[test]
    fn iso_completion_preserves_packet_layout_errors_and_total_in_both_directions() {
        for direction in [Direction::In, Direction::Out] {
            let budget = Budget::new(ResourceLimits {
                bytes: 4096,
                requests: 4,
            });
            let layout = [
                IsoPacket {
                    offset: 0,
                    length: 8,
                    actual_length: 0,
                    status: 0,
                },
                IsoPacket {
                    offset: 16,
                    length: 8,
                    actual_length: 0,
                    status: 0,
                },
                IsoPacket {
                    offset: 24,
                    length: 8,
                    actual_length: 0,
                    status: 0,
                },
            ];
            let header = SubmitHeader {
                seqnum: 1,
                device_id: 1,
                direction,
                endpoint: 1,
                transfer_flags: usbip::ISO_ASAP,
                transfer_buffer_length: 32,
                start_frame: 0,
                number_of_packets: 3,
                interval: 1,
                setup: [0; 8],
            };
            let mut transfer = Transfer::new(&budget, header, &layout).unwrap();
            transfer.packets_mut()[0].actual_length = 3;
            transfer.packets_mut()[1].status = usbip::status::PROTOCOL;
            transfer.packets_mut()[2].actual_length = 8;
            let mut done = Completion {
                transfer,
                status: 0,
                actual_length: 11,
                start_frame: 123,
            };
            assert_eq!(done.validate(), Ok(()));
            assert_eq!(done.transfer.layout(), layout);
            assert_eq!(done.transfer.packets()[1].status, usbip::status::PROTOCOL);
            done.actual_length = 12;
            assert_eq!(done.validate(), Err(Error::Invalid));
            done.actual_length = 11;
            done.transfer.packets_mut()[0].offset = 1;
            assert_eq!(done.validate(), Err(Error::Invalid));
            done.transfer.packets_mut()[0].offset = 0;
            done.transfer.packets_mut()[0].length = 7;
            assert_eq!(done.validate(), Err(Error::Invalid));
            done.transfer.packets_mut()[0].length = 8;
            done.transfer.packets_mut()[2].actual_length = 9;
            done.actual_length = 12;
            assert_eq!(done.validate(), Err(Error::Invalid));
            drop(done);
            assert_eq!(budget.usage().bytes, 0);
            assert_eq!(budget.usage().requests, 0);
        }
    }
    #[test]
    fn iso_wire_roundtrip_scatter_and_compaction_preserve_gaps() {
        for direction in [Direction::In, Direction::Out] {
            let b = Budget::new(ResourceLimits {
                bytes: 8192,
                requests: 8,
            });
            let layout = [
                IsoPacket {
                    offset: 4,
                    length: 8,
                    actual_length: 0,
                    status: 0,
                },
                IsoPacket {
                    offset: 20,
                    length: 8,
                    actual_length: 0,
                    status: 0,
                },
            ];
            let h = SubmitHeader {
                seqnum: 1,
                device_id: 1,
                direction,
                endpoint: 1,
                transfer_flags: usbip::ISO_ASAP,
                transfer_buffer_length: 32,
                start_frame: 0,
                number_of_packets: 2,
                interval: 1,
                setup: [0; 8],
            };
            let mut packets = layout.to_vec();
            packets[0].actual_length = 3;
            packets[1].actual_length = 2;
            packets[1].status = -71;
            let reply = usbip::ReturnSubmit {
                request: (&h).into(),
                status: 0,
                actual_length: 5,
                start_frame: 42,
                number_of_packets: 2,
                error_count: 1,
                data: if direction == Direction::In {
                    vec![1, 2, 3, 4, 5]
                } else {
                    vec![]
                },
                iso_packets: packets,
            };
            let done =
                Completion::from_reply(Transfer::new(&b, h, &layout).unwrap(), &reply).unwrap();
            if direction == Direction::In {
                assert_eq!(&done.transfer.buffer()[4..7], &[1, 2, 3]);
                assert_eq!(&done.transfer.buffer()[20..22], &[4, 5]);
                assert!(done.transfer.buffer()[7..20].iter().all(|b| *b == 0));
            }
            let mut packed = vec![0; reply.data.len()];
            done.copy_payload(&mut packed).unwrap();
            assert_eq!(packed, reply.data);
            for mutation in 0..8 {
                let mut bad = reply.clone();
                match mutation {
                    0 => bad.iso_packets[1].offset = 21,
                    1 => bad.iso_packets[1].length = 7,
                    2 => bad.iso_packets[0].actual_length = 9,
                    3 => bad.error_count = 0,
                    4 => bad.actual_length = 4,
                    5 => bad.iso_packets[0].status = 1,
                    6 => {
                        bad.request.direction = if direction == Direction::In {
                            Direction::Out
                        } else {
                            Direction::In
                        }
                    }
                    _ => bad.request.number_of_packets = 1,
                }
                assert!(
                    Completion::from_reply(Transfer::new(&b, h, &layout).unwrap(), &bad).is_err()
                );
            }
            drop(done);
            assert_eq!(b.usage().bytes, 0);
        }
    }
    #[test]
    fn linux_pending_iso_status_is_normalized_before_native_admission() {
        let b = Budget::new(ResourceLimits {
            bytes: 4096,
            requests: 4,
        });
        let h = SubmitHeader {
            seqnum: 1,
            device_id: 1,
            direction: Direction::Out,
            endpoint: 1,
            transfer_flags: usbip::ISO_ASAP,
            transfer_buffer_length: 64,
            start_frame: 0,
            number_of_packets: 1,
            interval: 1,
            setup: [0; 8],
        };
        let mut packet = IsoPacket {
            offset: 0,
            length: 64,
            actual_length: 0,
            status: usbip::status::NOT_ACCESSED,
        };
        let t = Transfer::new(&b, h, std::slice::from_ref(&packet)).unwrap();
        assert_eq!(t.packets()[0].status, 0);
        assert_eq!(t.layout()[0].status, 0);
        drop(t);
        for bad in [-71, -1, 1] {
            packet.status = bad;
            assert!(Transfer::new(&b, h, std::slice::from_ref(&packet)).is_err());
        }
        packet.status = usbip::status::NOT_ACCESSED;
        packet.actual_length = 1;
        assert!(Transfer::new(&b, h, std::slice::from_ref(&packet)).is_err());
        assert_eq!(b.usage().bytes, 0);
    }
}
