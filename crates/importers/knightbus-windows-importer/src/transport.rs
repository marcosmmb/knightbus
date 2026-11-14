//! Standard USB/IP stream, bounded framing and request correlation. The caller
//! supplies an authorized TCP connection (normally through its authenticated
//! tunnel). Production peer authentication belongs to the hosting layer.
use crate::{
    channel::{Descriptors, MAX_TRANSFER, SLOTS, setup},
    descriptors::{self as usb, Speed},
};
use knightbus_core::{
    Budget, Buffer, Completion, DecodedMessage, Device, DeviceId, DeviceKey, Error,
    ImporterBackend, IncomingDecoder, MemoryReservation, Result, SessionId, Transfer,
    usbip::{self, Context, Direction, Limits, Message, ReplyContext, SubmitHeader},
};
use std::{
    io::{self, Read, Write},
    net::{Shutdown, TcpStream},
    task::Poll,
    time::{Duration, Instant},
};
// USB/IP fixed fields inspected before handing a frame to the codec.
const USBIP_TRANSFER_HEADER_BYTES: usize = 48;
const USBIP_PREFIX_BYTES: usize = 2 * size_of::<u32>();
const USBIP_SEQUENCE_OFFSET: usize = size_of::<u32>();
const USBIP_BUS_ID_BYTES: usize = 32;
const USBIP_RET_SUBMIT: u32 = 3;
const USBIP_RET_UNLINK: u32 = 4;
const LINUX_ECONNRESET: i32 = -104; // Successful USB/IP cancellation status.
const EXCHANGE_POLL_INTERVAL: Duration = Duration::from_millis(1);
// Bootstrap request sequences; the forwarder starts after the last one.
const SEQ_DEVICE_DESCRIPTOR: u32 = 1;
const SEQ_CONFIG_HEADER: u32 = 2;
const SEQ_CONFIG_DESCRIPTOR: u32 = 3;
const SEQ_BOS_HEADER: u32 = 4;
const SEQ_BOS_DESCRIPTOR: u32 = 5;
const SEQ_SET_CONFIGURATION: u32 = 4;
const SEQ_GET_CONFIGURATION: u32 = 5;
const DESCRIPTOR_TYPE_SHIFT: u32 = 8; // High byte of GET_DESCRIPTOR wValue.
const LIMITS: Limits = Limits {
    frame_bytes: MAX_TRANSFER + USBIP_TRANSFER_HEADER_BYTES,
    transfer_bytes: MAX_TRANSFER,
    devices: 128,
    iso_packets: 0,
};
/// Imported inventory and the budget credits for its heap-backed topology.
/// Keep this owner alive while using its device metadata.
pub struct ImportedDevice {
    device: Device,
    _storage: MemoryReservation,
}
impl ImportedDevice {
    /// Borrow validated inventory without releasing its storage charge.
    pub fn device(&self) -> &Device {
        &self.device
    }
}
/// Bounded stream adapter. A partial frame has an absolute deadline; slow byte
/// delivery cannot keep renewing it. A failed write poisons the whole connection.
pub struct Wire {
    stream: TcpStream,
    budget: Budget,
    input: Buffer,
    start: usize,
    end: usize,
    prefix: [u8; USBIP_PREFIX_BYTES],
    prefix_len: usize,
    decoder: Option<IncomingDecoder>,
    frame_started: Option<Instant>,
    timeout: Duration,
    output: Option<(Buffer, usize, Instant)>,
    failed: bool,
    imported: bool,
}
impl Wire {
    /// Wrap an already authorized connection. This does not authenticate a peer.
    pub fn new(stream: TcpStream, budget: Budget, timeout: Duration) -> Result<Self> {
        if timeout.is_zero() {
            return Err(Error::Invalid);
        }
        stream.set_nonblocking(true).map_err(net_error)?;
        stream.set_nodelay(true).map_err(net_error)?;
        let input = budget.buffer(16384)?;
        Ok(Self {
            stream,
            budget,
            input,
            start: 0,
            end: 0,
            prefix: [0; USBIP_PREFIX_BYTES],
            prefix_len: 0,
            decoder: None,
            frame_started: None,
            timeout,
            output: None,
            failed: false,
            imported: false,
        })
    }
    /// Whether another outbound message can be admitted without queue growth.
    pub fn writable(&self) -> bool {
        !self.failed && self.output.is_none()
    }
    /// Admit one outgoing frame. Backpressure leaves the caller's message intact.
    pub fn send(&mut self, message: &Message) -> Result<()> {
        if self.failed {
            return Err(Error::State);
        }
        if self.output.is_some() {
            return Err(Error::Exhausted);
        }
        let bytes = self
            .budget
            .encode(message, LIMITS)
            .map_err(|_| Error::Invalid)?;
        self.output = Some((bytes, 0, Instant::now()));
        Ok(())
    }
    fn send_transfer(&mut self, header: &SubmitHeader, transfer: &Transfer) -> Result<()> {
        if self.failed {
            return Err(Error::State);
        }
        if self.output.is_some() {
            return Err(Error::Exhausted);
        }
        let payload = if header.direction == Direction::Out {
            transfer.buffer()
        } else {
            &[]
        };
        // Borrow the native owner's storage. The codec validates and admits the
        // final frame before copying directly into its one output allocation.
        let bytes = self
            .budget
            .encode_submit(header, payload, transfer.layout(), LIMITS)
            .map_err(|_| Error::Invalid)?;
        self.output = Some((bytes, 0, Instant::now()));
        Ok(())
    }
    /// Advance short writes without blocking. No whole-frame retries after errors.
    pub fn flush(&mut self) -> Result<()> {
        if self.failed {
            return Err(Error::State);
        }
        let result = self.flush_inner();
        if result.is_err() {
            self.poison();
        }
        result
    }
    fn poison(&mut self) {
        self.failed = true;
        let _ = self.stream.shutdown(Shutdown::Both);
        self.output = None;
    }
    fn flush_inner(&mut self) -> Result<()> {
        if let Some((bytes, offset, admitted)) = &mut self.output {
            // Partial progress and WouldBlock must not renew a frame's lease.
            // Check before writing, even if this attempt could finish the frame.
            if admitted.elapsed() >= self.timeout {
                return Err(Error::Deadline);
            }
            match self.stream.write(&bytes.as_slice()[*offset..]) {
                Ok(0) => return Err(Error::Unavailable),
                Ok(n) => {
                    *offset += n;
                    if *offset == bytes.as_slice().len() {
                        self.output = None;
                    }
                }
                Err(e)
                    if matches!(
                        e.kind(),
                        io::ErrorKind::WouldBlock | io::ErrorKind::Interrupted
                    ) => {}
                Err(e) => return Err(net_error(e)),
            }
        }
        Ok(())
    }
    /// Receive one frame. The resolver sees only the fixed command/sequence
    /// prefix and must return original context before body allocation.
    pub fn receive(
        &mut self,
        context: impl FnMut(u32, u32) -> Result<Context>,
    ) -> Result<Option<DecodedMessage>> {
        if self.failed {
            return Err(Error::State);
        }
        let result = self.receive_inner(context);
        if result.is_err() {
            self.poison();
        }
        result
    }
    fn receive_inner(
        &mut self,
        mut context: impl FnMut(u32, u32) -> Result<Context>,
    ) -> Result<Option<DecodedMessage>> {
        if self
            .frame_started
            .is_some_and(|t| t.elapsed() >= self.timeout)
        {
            return Err(Error::Deadline);
        }
        if self.start == self.end {
            match self.stream.read(self.input.as_mut_slice()) {
                Ok(0) => return Err(Error::Unavailable),
                Ok(n) => {
                    self.start = 0;
                    self.end = n;
                }
                Err(e)
                    if matches!(
                        e.kind(),
                        io::ErrorKind::WouldBlock | io::ErrorKind::Interrupted
                    ) =>
                {
                    return Ok(None);
                }
                Err(e) => return Err(net_error(e)),
            }
        }
        if self.decoder.is_none() {
            if self.prefix_len == 0 {
                self.frame_started = Some(Instant::now());
            }
            let n = (self.prefix.len() - self.prefix_len).min(self.end - self.start);
            self.prefix[self.prefix_len..self.prefix_len + n]
                .copy_from_slice(&self.input.as_slice()[self.start..self.start + n]);
            self.start += n;
            self.prefix_len += n;
            if self.prefix_len < self.prefix.len() {
                return Ok(None);
            }
            let command =
                u32::from_be_bytes(self.prefix[..USBIP_SEQUENCE_OFFSET].try_into().unwrap());
            let seq = u32::from_be_bytes(self.prefix[USBIP_SEQUENCE_OFFSET..].try_into().unwrap());
            let mut decoder = self.budget.decoder(context(command, seq)?, LIMITS)?;
            let (_, message) = decoder.push(&self.prefix).map_err(|_| Error::Invalid)?;
            if message.is_some() {
                self.prefix_len = 0;
                self.frame_started = None;
                return Ok(message);
            }
            self.decoder = Some(decoder);
        }
        let (n, message) = self
            .decoder
            .as_mut()
            .unwrap()
            .push(&self.input.as_slice()[self.start..self.end])
            .map_err(|_| Error::Invalid)?;
        self.start += n;
        if message.is_some() {
            self.decoder = None;
            self.prefix_len = 0;
            self.frame_started = None;
        }
        Ok(message)
    }
    fn exchange(&mut self, message: &Message, context: Context) -> Result<DecodedMessage> {
        self.send(message)?;
        let deadline = Instant::now() + self.timeout;
        loop {
            if Instant::now() >= deadline {
                return Err(Error::Deadline);
            }
            self.flush()?;
            if let Some(reply) = self.receive(|_, _| Ok(context))? {
                return Ok(reply);
            }
            std::thread::sleep(EXCHANGE_POLL_INTERVAL);
        }
    }
    /// Exclusively import an authorized device, validate its EP0 descriptors,
    /// then select and verify its sole supported configuration before native
    /// publication. This changes the remote device's configuration/alternate state.
    pub fn import(
        &mut self,
        bus_id: usbip::WireString<USBIP_BUS_ID_BYTES>,
    ) -> Result<(ImportedDevice, Descriptors)> {
        if self.imported {
            return Err(Error::State);
        }
        self.imported = true;
        let result = self.import_inner(bus_id);
        if result.is_err() {
            self.poison();
        }
        result
    }
    fn import_inner(
        &mut self,
        bus_id: usbip::WireString<USBIP_BUS_ID_BYTES>,
    ) -> Result<(ImportedDevice, Descriptors)> {
        let reply = self.exchange(&Message::ImportRequest(bus_id.clone()), Context::Management)?;
        let mut metadata = match reply.message() {
            Message::ImportReply {
                status: 0,
                device: Some(d),
            } if d.bus_id == bus_id => (**d).clone(),
            _ => return Err(Error::Denied),
        };
        if metadata.bus_number > u32::from(u16::MAX) || metadata.device_number > u32::from(u16::MAX)
        {
            return Err(Error::Invalid);
        }
        let id = (metadata.bus_number << u16::BITS) | metadata.device_number;
        let device = self.descriptor(
            id,
            SEQ_DEVICE_DESCRIPTOR,
            u16::from(usb::device::TYPE) << DESCRIPTOR_TYPE_SHIFT,
            usb::device::BYTES as u16,
        )?;
        let config_header = self.descriptor(
            id,
            SEQ_CONFIG_HEADER,
            u16::from(usb::configuration::TYPE) << DESCRIPTOR_TYPE_SHIFT,
            usb::configuration::BYTES as u16,
        )?;
        if config_header.as_slice().len() != usb::configuration::BYTES {
            return Err(Error::Invalid);
        }
        let length = usb::u16_at(config_header.as_slice(), usb::configuration::TOTAL_LENGTH);
        if !(usb::configuration::BYTES..=usb::MAX_CONFIGURATION).contains(&usize::from(length)) {
            return Err(Error::Unsupported);
        }
        let config = self.descriptor(
            id,
            SEQ_CONFIG_DESCRIPTOR,
            u16::from(usb::configuration::TYPE) << DESCRIPTOR_TYPE_SHIFT,
            length,
        )?;
        let speed = Speed::try_from(metadata.speed).map_err(|_| Error::Unsupported)?;
        let bos = if speed.superspeed() {
            let header = self.descriptor(
                id,
                SEQ_BOS_HEADER,
                u16::from(usb::BOS_TYPE) << DESCRIPTOR_TYPE_SHIFT,
                usb::BOS_HEADER as u16,
            )?;
            let length = usb::u16_at(header.as_slice(), 2);
            if !(usb::BOS_HEADER..=usb::MAX_BOS).contains(&usize::from(length)) {
                return Err(Error::Invalid);
            }
            self.descriptor(
                id,
                SEQ_BOS_DESCRIPTOR,
                u16::from(usb::BOS_TYPE) << DESCRIPTOR_TYPE_SHIFT,
                length,
            )?
        } else {
            self.budget.buffer(0)?
        };
        let descriptors = Descriptors::with_bos(
            &self.budget,
            device.as_slice(),
            config.as_slice(),
            bos.as_slice(),
            speed,
        )?;
        if descriptors.topology.vid != metadata.vendor_id
            || descriptors.topology.pid != metadata.product_id
            // Linux usbip-vudc exports an unconfigured gadget with zero active
            // interfaces. The raw configuration still describes what Windows
            // may select later; do not confuse active state with topology.
            || (metadata.configuration == 0 && metadata.interfaces != 0)
            || (metadata.configuration != 0
                && (descriptors.topology.interfaces != metadata.interfaces
                    || descriptors.topology.configuration != metadata.configuration))
            || metadata.configurations != 1
        {
            return Err(Error::Invalid);
        }
        // Internal inventory key is independent of the exporter's textual bus ID.
        let key = DeviceKey {
            id: DeviceId(u64::from(id) + 1),
            generation: 1,
        };
        metadata.bus_id = key.id.bus_id();
        // Reserve the descriptor-bounded maximum before constructing any Vec.
        // Exact reservations below avoid geometric growth in nested vectors.
        let storage = self.budget.reserve_memory(
            std::mem::size_of::<knightbus_core::Configuration>()
                + crate::descriptors::MAX_SETTINGS
                    * std::mem::size_of::<knightbus_core::Interface>()
                + (usb::MAX_CONFIGURATION / usb::endpoint::MIN_BYTES)
                    * std::mem::size_of::<knightbus_core::Endpoint>(),
        )?;
        let configurations = configurations(config.as_slice())?;
        let actual = configurations.capacity()
            * std::mem::size_of::<knightbus_core::Configuration>()
            + configurations
                .iter()
                .map(|c| {
                    c.interfaces.capacity() * std::mem::size_of::<knightbus_core::Interface>()
                        + c.interfaces
                            .iter()
                            .map(|i| {
                                i.endpoints.capacity()
                                    * std::mem::size_of::<knightbus_core::Endpoint>()
                            })
                            .sum::<usize>()
                })
                .sum::<usize>();
        if actual > storage.bytes() {
            return Err(Error::Exhausted);
        }
        // UDE can start with an interface-setting notification rather than a
        // configuration change. Establish the sole supported configuration and
        // alt-zero state before publishing any native endpoints.
        let configuration = descriptors.topology.configuration;
        self.control(
            id,
            SEQ_SET_CONFIGURATION + if speed.superspeed() { 2 } else { 0 },
            [
                setup::DEVICE_OUT,
                setup::SET_CONFIGURATION,
                configuration,
                0,
                0,
                0,
                0,
                0,
            ],
            0,
        )?;
        if self
            .control(
                id,
                SEQ_GET_CONFIGURATION + if speed.superspeed() { 2 } else { 0 },
                [setup::DEVICE_IN, setup::GET_CONFIGURATION, 0, 0, 0, 0, 1, 0],
                1,
            )?
            .as_slice()
            != [configuration]
        {
            return Err(Error::Unavailable);
        }
        metadata.configuration = configuration;
        metadata.interfaces = descriptors.topology.interfaces;
        Ok((
            ImportedDevice {
                device: Device {
                    key,
                    wire: metadata,
                    configurations,
                },
                _storage: storage,
            },
            descriptors,
        ))
    }
    fn descriptor(&mut self, id: u32, sequence: u32, value: u16, length: u16) -> Result<Buffer> {
        let v = value.to_le_bytes();
        let n = length.to_le_bytes();
        self.control(
            id,
            sequence,
            [
                setup::DEVICE_IN,
                setup::GET_DESCRIPTOR,
                v[0],
                v[1],
                0,
                0,
                n[0],
                n[1],
            ],
            length,
        )
    }
    fn control(
        &mut self,
        id: u32,
        sequence: u32,
        setup: [u8; setup::BYTES],
        length: u16,
    ) -> Result<Buffer> {
        let header = SubmitHeader {
            seqnum: sequence,
            device_id: id,
            direction: if setup[setup::REQUEST_TYPE] & usb::DIRECTION_IN != 0 {
                Direction::In
            } else {
                Direction::Out
            },
            endpoint: 0,
            transfer_flags: 0,
            transfer_buffer_length: length.into(),
            start_frame: 0,
            number_of_packets: 0,
            interval: 0,
            setup,
        };
        let reply = self.exchange(
            &Message::Submit(header.with_payload(vec![], vec![])),
            Context::Replies(Some((&header).into())),
        )?;
        match reply.message() {
            Message::ReturnSubmit(r)
                if r.status == 0
                    && r.request.seqnum == sequence
                    && r.data.len() == length as usize =>
            {
                let mut b = self.budget.buffer(length.into())?;
                b.as_mut_slice().copy_from_slice(&r.data);
                Ok(b)
            }
            _ => Err(Error::Unavailable),
        }
    }
}
impl Drop for Wire {
    fn drop(&mut self) {
        let _ = self.stream.shutdown(Shutdown::Both);
    }
}
fn net_error(_: io::Error) -> Error {
    Error::Unavailable
}
struct Pending {
    transfer: Option<Transfer>,
    context: ReplyContext,
    unlink: Option<u32>,
    cancel: bool,
    started: Instant,
    cancel_started: Option<Instant>,
}
/// One active importer data plane, driven with bounded work per tick. Both host
/// modes use this type and the same native backend. Nothing passes through GUI IPC.
pub struct Forwarder<B: ImporterBackend> {
    backend: B,
    wire: Wire,
    device: ImportedDevice,
    session: SessionId,
    budget: Budget,
    pending: [Option<Pending>; SLOTS],
    sequence: u32,
    attached: bool,
    draining: bool,
    error: Option<Error>,
    deadline: Instant,
    timeout: Duration,
    _metadata: MemoryReservation,
}
impl<B: ImporterBackend> Forwarder<B> {
    /// Compose an authorized imported stream with its native attachment. Uses
    /// wire sequences after the descriptor/configuration requests in `import`.
    /// Timeout bounds attach, control requests, cancellation and cleanup. Idle
    /// bulk/interrupt requests await native cancellation; hosts must separately
    /// enforce session leases and aggregate resource/connection limits.
    pub fn new(
        backend: B,
        wire: Wire,
        device: ImportedDevice,
        session: SessionId,
        budget: Budget,
        timeout: Duration,
    ) -> Result<Self> {
        if timeout.is_zero() || session.epoch == 0 || session.serial == 0 {
            return Err(Error::Invalid);
        }
        let metadata = budget.reserve_memory(std::mem::size_of::<Self>())?;
        let sequence = SEQ_GET_CONFIGURATION
            + if matches!(device.device().wire.speed, 5 | 6) {
                2
            } else {
                0
            };
        Ok(Self {
            backend,
            wire,
            device,
            session,
            budget,
            pending: std::array::from_fn(|_| None),
            sequence,
            attached: false,
            draining: false,
            error: None,
            deadline: Instant::now() + timeout,
            timeout,
            _metadata: metadata,
        })
    }
    /// Stop admission and request native retirement, including a partial attach.
    pub fn disconnect(&mut self) {
        if !self.draining {
            self.draining = true;
            self.deadline = Instant::now() + self.timeout;
            let _ = self.wire.stream.shutdown(Shutdown::Both);
        }
    }
    /// Advance a bounded amount of I/O. Ready means native removal was confirmed;
    /// an error never implies that a stalled backend released its resources.
    pub fn tick(&mut self) -> Poll<Result<()>> {
        if !self.draining {
            if let Err(e) = self.advance() {
                self.error = Some(e);
                self.disconnect();
            }
        }
        if self.draining {
            match self.backend.detach(self.session) {
                Poll::Ready(Ok(())) => {
                    self.pending.iter_mut().for_each(|p| *p = None);
                    return Poll::Ready(self.error.map_or(Ok(()), Err));
                }
                Poll::Ready(Err(e)) => return Poll::Ready(Err(e)),
                Poll::Pending => {
                    if Instant::now() >= self.deadline {
                        return Poll::Ready(Err(Error::Cleanup));
                    }
                }
            }
        }
        Poll::Pending
    }
    fn next_sequence(&mut self) -> Result<u32> {
        self.sequence = self.sequence.checked_add(1).ok_or(Error::Exhausted)?;
        Ok(self.sequence)
    }
    fn advance(&mut self) -> Result<()> {
        if !self.attached {
            if Instant::now() >= self.deadline {
                return Err(Error::Deadline);
            }
            match self.backend.attach(self.session, self.device.device()) {
                Poll::Ready(result) => {
                    result?;
                    self.attached = true
                }
                Poll::Pending => return Ok(()),
            }
        }
        self.wire.flush()?;
        // Collect and send cancellation before competing for the single output
        // slot with a new SUBMIT. An already queued frame still finishes first.
        for _ in 0..SLOTS {
            let Some(seq) = self.backend.next_cancellation(self.session) else {
                break;
            };
            if let Some(p) = self.pending.iter_mut().flatten().find(|p| {
                p.transfer
                    .as_ref()
                    .is_some_and(|t| t.header().seqnum == seq)
            }) {
                p.cancel = true;
                p.cancel_started.get_or_insert_with(Instant::now);
            }
        }
        if self.wire.writable() {
            if let Some(index) = self.pending.iter().position(|p| {
                p.as_ref()
                    .is_some_and(|p| p.cancel && p.unlink.is_none() && p.transfer.is_some())
            }) {
                let seq = self.next_sequence()?;
                let p = self.pending[index].as_mut().unwrap();
                self.wire.send(&Message::Unlink {
                    seqnum: seq,
                    device_id: (self.device.device.wire.bus_number << u16::BITS)
                        | self.device.device.wire.device_number,
                    target_seqnum: p.context.seqnum,
                })?;
                p.unlink = Some(seq);
            }
        }
        // At most one new native request per tick; the socket admits one frame.
        if self.wire.writable() {
            if let Some(slot) = self.pending.iter().position(Option::is_none) {
                if let Some(transfer) = self.backend.next_transfer(self.session, &self.budget) {
                    let transfer = transfer?;
                    let mut header = *transfer.header();
                    header.seqnum = self.next_sequence()?;
                    let context = (&header).into();
                    self.wire.send_transfer(&header, &transfer)?;
                    self.pending[slot] = Some(Pending {
                        transfer: Some(transfer),
                        context,
                        unlink: None,
                        cancel: false,
                        started: Instant::now(),
                        cancel_started: None,
                    });
                }
            }
        }
        for _ in 0..SLOTS {
            let pending = &self.pending;
            let Some(message) = self.wire.receive(|command, seq| match command {
                USBIP_RET_SUBMIT => pending
                    .iter()
                    .flatten()
                    .find(|p| p.context.seqnum == seq && p.transfer.is_some())
                    .map(|p| Context::Replies(Some(p.context)))
                    .ok_or(Error::Invalid),
                USBIP_RET_UNLINK if pending.iter().flatten().any(|p| p.unlink == Some(seq)) => {
                    Ok(Context::Replies(None))
                }
                _ => Err(Error::Invalid),
            })?
            else {
                break;
            };
            match message.message() {
                Message::ReturnSubmit(reply) => {
                    let index = self
                        .pending
                        .iter()
                        .position(|p| {
                            p.as_ref()
                                .is_some_and(|p| p.context.seqnum == reply.request.seqnum)
                        })
                        .ok_or(Error::Invalid)?;
                    let p = self.pending[index].as_mut().unwrap();
                    let mut transfer = p.transfer.take().ok_or(Error::Invalid)?;
                    if reply.actual_length > transfer.header().transfer_buffer_length
                        || reply.number_of_packets > 0
                        || reply.status > 0
                    {
                        return Err(Error::Invalid);
                    }
                    if transfer.header().direction == Direction::In {
                        transfer.buffer_mut()[..reply.data.len()].copy_from_slice(&reply.data);
                    }
                    self.backend
                        .complete(
                            self.session,
                            Completion {
                                transfer,
                                status: reply.status,
                                actual_length: reply.actual_length,
                                start_frame: reply.start_frame,
                            },
                        )
                        .map_err(|_| Error::Internal)?;
                    if p.unlink.is_none() {
                        self.pending[index] = None;
                    }
                }
                Message::ReturnUnlink { seqnum, status } => {
                    let index = self
                        .pending
                        .iter()
                        .position(|p| p.as_ref().is_some_and(|p| p.unlink == Some(*seqnum)))
                        .ok_or(Error::Invalid)?;
                    let p = self.pending[index].as_mut().unwrap();
                    p.unlink = None;
                    if *status == LINUX_ECONNRESET {
                        let transfer = p.transfer.take().ok_or(Error::Invalid)?;
                        self.backend
                            .complete(
                                self.session,
                                Completion {
                                    transfer,
                                    status: LINUX_ECONNRESET,
                                    actual_length: 0,
                                    start_frame: 0,
                                },
                            )
                            .map_err(|_| Error::Internal)?;
                    } else if *status > 0 {
                        return Err(Error::Invalid);
                    }
                    // A failed unlink leaves the submit alive; never send repeated
                    // unlinks or free its owner before the eventual reply.
                    p.cancel = false;
                    if p.transfer.is_none() {
                        self.pending[index] = None;
                    }
                }
                _ => return Err(Error::Invalid),
            }
        }
        if self.pending.iter().flatten().any(|p| {
            // An idle HID interrupt or bulk read can legitimately wait for
            // data indefinitely. Only EP0 and a requested cancellation have
            // a device-response deadline; framing/write deadlines remain in Wire.
            let start = p.cancel_started.or_else(|| {
                p.transfer
                    .as_ref()
                    .filter(|t| t.header().endpoint == 0)
                    .map(|_| p.started)
            });
            start.is_some_and(|start| start.elapsed() >= self.timeout)
        }) {
            return Err(Error::Deadline);
        }
        self.wire.flush()
    }
}
impl<B: ImporterBackend> Drop for Forwarder<B> {
    fn drop(&mut self) {
        self.disconnect();
        let _ = self.backend.detach(self.session);
    }
}

// Raw descriptors have already passed the shared allocation-free validator.
// Build only the portable topology required by the existing engine contract.
fn configurations(bytes: &[u8]) -> Result<Vec<knightbus_core::Configuration>> {
    use knightbus_core::{Configuration, Endpoint, Interface, TransferKind};
    // Class fields are opaque to the shared kernel validator.
    const INTERFACE_CLASS: usize = 5;
    const INTERFACE_SUBCLASS: usize = 6;
    const INTERFACE_PROTOCOL: usize = 7;
    let mut interfaces: Vec<Interface> = Vec::new();
    interfaces
        .try_reserve_exact(crate::descriptors::MAX_SETTINGS)
        .map_err(|_| Error::Exhausted)?;
    let mut offset = usb::configuration::BYTES;
    while offset < bytes.len() {
        let d = &bytes[offset..offset + bytes[offset] as usize];
        match d[usb::DESCRIPTOR_TYPE] {
            usb::interface::TYPE => {
                let mut endpoints = Vec::new();
                endpoints
                    .try_reserve_exact(d[usb::interface::ENDPOINTS].into())
                    .map_err(|_| Error::Exhausted)?;
                interfaces.push(Interface {
                    number: d[usb::interface::NUMBER],
                    alternate: d[usb::interface::ALTERNATE],
                    class: usbip::Interface {
                        class: d[INTERFACE_CLASS],
                        subclass: d[INTERFACE_SUBCLASS],
                        protocol: d[INTERFACE_PROTOCOL],
                    },
                    endpoints,
                })
            }
            usb::endpoint::TYPE => {
                interfaces
                    .last_mut()
                    .ok_or(Error::Invalid)?
                    .endpoints
                    .push(Endpoint {
                        address: d[usb::endpoint::ADDRESS],
                        kind: if d[usb::endpoint::ATTRIBUTES] == usb::TRANSFER_BULK {
                            TransferKind::Bulk
                        } else {
                            TransferKind::Interrupt
                        },
                        max_packet_size: usb::u16_at(d, usb::endpoint::MAX_PACKET),
                        interval: d[usb::endpoint::INTERVAL],
                        allowed_flags: 0,
                    })
            }
            _ => {}
        }
        offset += d.len();
    }
    let mut result = Vec::new();
    result.try_reserve_exact(1).map_err(|_| Error::Exhausted)?;
    result.push(Configuration {
        value: bytes[usb::configuration::VALUE],
        interfaces,
    });
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::TcpListener;

    fn test_device(budget: &Budget) -> ImportedDevice {
        let key = DeviceKey {
            id: DeviceId(1),
            generation: 1,
        };
        ImportedDevice {
            device: Device {
                key,
                wire: usbip::Device {
                    path: usbip::WireString::new(b"original-test").unwrap(),
                    bus_id: key.id.bus_id(),
                    bus_number: 1,
                    device_number: 1,
                    speed: 2,
                    vendor_id: 0x1209,
                    product_id: 1,
                    device_bcd: 0x100,
                    class: 0,
                    subclass: 0,
                    protocol: 0,
                    configuration: 1,
                    configurations: 1,
                    interfaces: 1,
                },
                configurations: vec![],
            },
            _storage: budget.reserve_memory(0).unwrap(),
        }
    }
    fn test_header(seqnum: u32, direction: Direction, length: u32) -> SubmitHeader {
        SubmitHeader {
            seqnum,
            device_id: 0x10001,
            direction,
            endpoint: 1,
            transfer_flags: 0,
            transfer_buffer_length: length,
            start_frame: 0,
            number_of_packets: 0,
            interval: 0,
            setup: [0; 8],
        }
    }

    #[test]
    fn native_out_encoding_needs_only_the_owner_and_final_frame() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let stream = TcpStream::connect(listener.local_addr().unwrap()).unwrap();
        let (_peer, _) = listener.accept().unwrap();
        let budget = Budget::new(knightbus_core::ResourceLimits {
            // Input scratch, native payload/metadata, one output frame. There is
            // deliberately no room for another temporary 64 KiB payload copy.
            bytes: 3 * MAX_TRANSFER + 96 + std::mem::size_of::<Transfer>() + 1024,
            requests: 1,
        });
        let mut wire = Wire::new(stream, budget.clone(), Duration::from_secs(1)).unwrap();
        let mut transfer = Transfer::new(
            &budget,
            test_header(71, Direction::Out, MAX_TRANSFER as u32),
            &[],
        )
        .unwrap();
        transfer.buffer_mut().fill(42);
        let baseline = budget.usage();
        let mut header = *transfer.header();
        header.seqnum = 6; // Wire correlation must not mutate native identity.
        wire.send_transfer(&header, &transfer).unwrap();
        let output = &wire.output.as_ref().unwrap().0;
        assert_eq!(budget.usage().bytes, baseline.bytes + output.capacity());
        assert_eq!(budget.usage().requests, 1);
        let (Message::Submit(decoded), _) =
            usbip::decode(output.as_slice(), Context::Requests, LIMITS).unwrap()
        else {
            panic!("SUBMIT")
        };
        assert_eq!(decoded.seqnum, 6);
        assert_eq!(decoded.data, transfer.buffer());
        assert_eq!(transfer.header().seqnum, 71);
        drop(wire);
        assert_eq!(
            budget.usage().requests,
            1,
            "encoding does not retire native ownership"
        );
        drop(transfer);
        assert_eq!(budget.usage().bytes, 0);
    }

    #[test]
    fn cancellation_precedes_continuously_ready_submissions() {
        struct Native {
            delivered: u32,
            canceled: bool,
        }
        impl ImporterBackend for Native {
            fn attach(&mut self, _: SessionId, _: &Device) -> Poll<Result<()>> {
                Poll::Ready(Ok(()))
            }
            fn next_transfer(&mut self, _: SessionId, budget: &Budget) -> Option<Result<Transfer>> {
                self.delivered += 1;
                Some(Transfer::new(
                    budget,
                    test_header(self.delivered, Direction::Out, 0),
                    &[],
                ))
            }
            fn next_cancellation(&mut self, _: SessionId) -> Option<u32> {
                if self.delivered == 0 || self.canceled {
                    return None;
                }
                self.canceled = true;
                Some(1)
            }
            fn complete(
                &mut self,
                _: SessionId,
                _: Completion,
            ) -> std::result::Result<(), Completion> {
                Ok(())
            }
            fn detach(&mut self, _: SessionId) -> Poll<Result<()>> {
                Poll::Ready(Ok(()))
            }
        }
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let stream = TcpStream::connect(listener.local_addr().unwrap()).unwrap();
        let (mut peer, _) = listener.accept().unwrap();
        peer.set_read_timeout(Some(Duration::from_secs(2))).unwrap();
        let budget = Budget::new(knightbus_core::ResourceLimits {
            bytes: 1024 * 1024,
            requests: 32,
        });
        let wire = Wire::new(stream, budget.clone(), Duration::from_secs(2)).unwrap();
        let mut f = Forwarder::new(
            Native {
                delivered: 0,
                canceled: false,
            },
            wire,
            test_device(&budget),
            SessionId {
                epoch: 1,
                serial: 1,
            },
            budget.clone(),
            Duration::from_secs(2),
        )
        .unwrap();
        assert!(f.tick().is_pending());
        let Some(Message::Submit(first)) =
            usbip::read_message(&mut peer, Context::Requests, LIMITS).unwrap()
        else {
            panic!("first SUBMIT")
        };
        assert!(f.tick().is_pending());
        let Some(Message::Unlink { target_seqnum, .. }) =
            usbip::read_message(&mut peer, Context::Requests, LIMITS).unwrap()
        else {
            panic!("cancellation was starved by a new SUBMIT")
        };
        assert_eq!(target_seqnum, first.seqnum);
        assert_eq!(
            f.backend.delivered, 1,
            "new work must wait behind cancellation"
        );
        f.disconnect();
        assert_eq!(f.tick(), Poll::Ready(Ok(())));
        drop(f);
        assert_eq!(budget.usage().bytes, 0);
        assert_eq!(budget.usage().requests, 0);
    }

    #[test]
    fn partial_frame_deadline_does_not_restart_on_progress() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let mut sender = TcpStream::connect(listener.local_addr().unwrap()).unwrap();
        let (receiver, _) = listener.accept().unwrap();
        let mut wire = Wire::new(
            receiver,
            Budget::new(knightbus_core::ResourceLimits {
                bytes: 1024 * 1024,
                requests: 32,
            }),
            Duration::from_secs(10),
        )
        .unwrap();
        sender.write_all(&[0]).unwrap();
        let deadline = Instant::now() + Duration::from_secs(2);
        while wire.frame_started.is_none() {
            assert!(Instant::now() < deadline);
            assert!(
                wire.receive(|_, _| Ok(Context::Replies(None)))
                    .unwrap()
                    .is_none()
            );
            std::thread::yield_now();
        }
        // Control elapsed time without scheduler-dependent millisecond sleeps.
        let started = Instant::now() - Duration::from_secs(5);
        wire.frame_started = Some(started);
        sender.write_all(&[0]).unwrap();
        while wire.prefix_len != 2 {
            assert!(Instant::now() < deadline);
            assert!(
                wire.receive(|_, _| Ok(Context::Replies(None)))
                    .unwrap()
                    .is_none()
            );
            std::thread::yield_now();
        }
        assert_eq!(wire.frame_started, Some(started));
        wire.frame_started = Some(Instant::now() - Duration::from_secs(11));
        assert!(matches!(
            wire.receive(|_, _| Ok(Context::Replies(None))),
            Err(Error::Deadline)
        ));
    }

    #[test]
    fn cancellation_races_keep_original_owner_until_remote_retirement() {
        use std::sync::{Arc, Mutex};
        struct Native {
            delivered: bool,
            canceled: bool,
            results: Arc<Mutex<Vec<(i32, u32)>>>,
        }
        impl ImporterBackend for Native {
            fn attach(&mut self, _: SessionId, _: &Device) -> Poll<Result<()>> {
                Poll::Ready(Ok(()))
            }
            fn next_transfer(&mut self, _: SessionId, budget: &Budget) -> Option<Result<Transfer>> {
                if self.delivered {
                    return None;
                }
                self.delivered = true;
                Some(Transfer::new(
                    budget,
                    SubmitHeader {
                        seqnum: 71,
                        device_id: 0x10001,
                        direction: Direction::In,
                        endpoint: 1,
                        transfer_flags: 0,
                        transfer_buffer_length: 8,
                        start_frame: 0,
                        number_of_packets: 0,
                        interval: 8,
                        setup: [0; 8],
                    },
                    &[],
                ))
            }
            fn next_cancellation(&mut self, _: SessionId) -> Option<u32> {
                if !self.delivered || self.canceled {
                    return None;
                }
                self.canceled = true;
                Some(71)
            }
            fn complete(
                &mut self,
                _: SessionId,
                completion: Completion,
            ) -> std::result::Result<(), Completion> {
                assert_eq!(completion.transfer.header().seqnum, 71);
                if completion.status == 0 {
                    assert_eq!(&completion.transfer.buffer()[..4], &[6; 4]);
                }
                self.results
                    .lock()
                    .unwrap()
                    .push((completion.status, completion.actual_length));
                Ok(())
            }
            fn detach(&mut self, _: SessionId) -> Poll<Result<()>> {
                Poll::Ready(Ok(()))
            }
        }
        for order in 0..5 {
            let listener = TcpListener::bind("127.0.0.1:0").unwrap();
            let stream = TcpStream::connect(listener.local_addr().unwrap()).unwrap();
            let (mut peer, _) = listener.accept().unwrap();
            peer.set_read_timeout(Some(Duration::from_secs(3))).unwrap();
            let server = std::thread::spawn(move || {
                let Some(Message::Submit(request)) =
                    usbip::read_message(&mut peer, Context::Requests, LIMITS).unwrap()
                else {
                    panic!("submit")
                };
                let Some(Message::Unlink {
                    seqnum,
                    target_seqnum,
                    ..
                }) = usbip::read_message(&mut peer, Context::Requests, LIMITS).unwrap()
                else {
                    panic!("unlink")
                };
                assert_eq!(request.seqnum, 6);
                assert_eq!(target_seqnum, request.seqnum);
                if order == 4 {
                    // Peer ignores cancellation; the client must close.
                    assert_eq!(peer.read(&mut [0]).unwrap(), 0);
                    return;
                }
                let reply = Message::ReturnSubmit(usbip::ReturnSubmit {
                    request: (&request).into(),
                    status: 0,
                    actual_length: 4,
                    start_frame: 0,
                    number_of_packets: 0,
                    error_count: 0,
                    data: vec![6; 4],
                    iso_packets: vec![],
                });
                let unlink = Message::ReturnUnlink {
                    seqnum,
                    status: if order == 0 { -104 } else { 0 },
                };
                if order == 1 {
                    usbip::write_message(&mut peer, &reply, LIMITS).unwrap();
                }
                usbip::write_message(&mut peer, &unlink, LIMITS).unwrap();
                if order >= 2 {
                    usbip::write_message(&mut peer, &reply, LIMITS).unwrap();
                }
                assert_eq!(peer.read(&mut [0]).unwrap(), 0);
            });
            let budget = Budget::new(knightbus_core::ResourceLimits {
                bytes: 1024 * 1024,
                requests: 4,
            });
            let device = test_device(&budget);
            let results = Arc::new(Mutex::new(vec![]));
            let native = Native {
                delivered: false,
                canceled: order >= 3,
                results: results.clone(),
            };
            let timeout = Duration::from_secs(2);
            let wire = Wire::new(stream, budget.clone(), timeout).unwrap();
            let mut forwarder = Forwarder::new(
                native,
                wire,
                device,
                SessionId {
                    epoch: 1,
                    serial: 1,
                },
                budget.clone(),
                timeout,
            )
            .unwrap();
            if order >= 3 {
                assert!(forwarder.tick().is_pending());
                forwarder.pending[0].as_mut().unwrap().started =
                    Instant::now() - Duration::from_secs(20);
                assert!(
                    forwarder.tick().is_pending(),
                    "an idle native read is not a slow frame"
                );
                forwarder.backend.canceled = false;
                if order == 4 {
                    assert!(forwarder.tick().is_pending());
                    let pending = forwarder.pending[0].as_mut().unwrap();
                    assert!(pending.cancel_started.unwrap().elapsed() < timeout);
                    pending.cancel_started = Some(Instant::now() - Duration::from_secs(20));
                    assert!(matches!(
                        forwarder.tick(),
                        Poll::Ready(Err(Error::Deadline))
                    ));
                    drop(forwarder);
                    server.join().unwrap();
                    assert_eq!(budget.usage().bytes, 0);
                    assert_eq!(budget.usage().requests, 0);
                    continue;
                }
            }
            let deadline = Instant::now() + timeout;
            loop {
                assert!(Instant::now() < deadline);
                assert!(forwarder.tick().is_pending());
                if !results.lock().unwrap().is_empty()
                    && forwarder.pending.iter().all(Option::is_none)
                {
                    break;
                }
                std::thread::yield_now();
            }
            assert_eq!(
                *results.lock().unwrap(),
                vec![if order == 0 { (-104, 0) } else { (0, 4) }]
            );
            forwarder.disconnect();
            assert!(matches!(forwarder.tick(), Poll::Ready(Ok(()))));
            drop(forwarder);
            server.join().unwrap();
            assert_eq!(budget.usage().bytes, 0);
            assert_eq!(budget.usage().requests, 0);
        }
    }
}
