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
        {
            return Err(Error::Invalid);
        }
        let mut end = 0u32;
        for p in layout {
            let next = p.offset.checked_add(p.length).ok_or(Error::Invalid)?;
            if p.offset < end
                || next > header.transfer_buffer_length
                || p.actual_length != 0
                || p.status != 0
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
        original.extend_from_slice(layout);
        packets.extend_from_slice(layout);
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
    /// ISO completion descriptors; identity/bounds rechecked at retirement.
    pub fn packets_mut(&mut self) -> &mut [IsoPacket] {
        &mut self.packets
    }
    /// Completed ISO descriptors in original request order.
    pub fn packets(&self) -> &[IsoPacket] {
        &self.packets
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
    pub(crate) fn validate(&self, device: &Device) -> Result<()> {
        let s = &self.header;
        if s.device_id != device.wire_id() {
            return Err(Error::Invalid);
        }
        let (kind, flags) = if s.endpoint == 0 {
            (TransferKind::Control, 0)
        } else {
            let e = device
                .endpoint(s.endpoint, s.direction)
                .ok_or(Error::Invalid)?;
            (e.kind, e.allowed_flags)
        };
        if s.transfer_flags & !flags != 0
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
            if s.setup[0] & 0x60 == 0 && matches!(s.setup[1], 5 | 9 | 11) {
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
        if self.actual_length > t.header.transfer_buffer_length {
            return Err(Error::Invalid);
        }
        let mut total = 0u32;
        for (p, original) in t.packets.iter().zip(&t.layout) {
            if p.offset != original.offset
                || p.length != original.length
                || p.actual_length > p.length
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
