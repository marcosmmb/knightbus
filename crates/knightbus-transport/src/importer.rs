//! Standard USB/IP stream, bounded framing and request correlation. The caller
//! supplies an authorized TCP connection (normally through its authenticated
//! tunnel). Production peer authentication belongs to the hosting layer.
use knightbus_core::{
    DescriptorSet as Descriptors,
    topology::{self as usb, Speed},
};
const MAX_TRANSFER: usize = 1024 * 1024;
const SLOTS: usize = 31;
use knightbus_core::{
    Budget, Buffer, Completion, DecodedMessage, Device, DeviceId, DeviceKey, Error,
    ImporterBackend, MemoryReservation, Result, SessionId, Transfer,
    usbip::{self, Context, Direction, Limits, Message, ReplyContext, SubmitHeader},
};
use std::{
    net::TcpStream,
    task::Poll,
    time::{Duration, Instant},
};
// USB/IP fixed fields inspected before handing a frame to the codec.
const USBIP_TRANSFER_HEADER_BYTES: usize = 48;
const USBIP_BUS_ID_BYTES: usize = 32;
const USBIP_RET_SUBMIT: u32 = 3;
const USBIP_RET_UNLINK: u32 = 4;
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
/// Import and attach a device with protocol and policy checks.
/// The native factory receives the same admission budget used by every layer.
pub fn connect<D: knightbus_core::importer::Driver>(
    stream: TcpStream,
    bus_id: usbip::WireString<USBIP_BUS_ID_BYTES>,
    session: SessionId,
    budget: Budget,
    timeout: Duration,
    factory: impl FnOnce(&Budget) -> Result<D> + Send + 'static,
) -> Result<Forwarder<impl ImporterBackend>> {
    let mut wire = Wire::new(stream, budget.clone(), timeout, D::CAPABILITIES)?;
    let (device, descriptors) = wire.import(bus_id)?;
    let native_budget = budget.clone();
    let backend = knightbus_core::importer::Backend::new(
        move || factory(&native_budget),
        descriptors,
        budget,
    )?;
    Forwarder::new(backend, wire, device, session)
}

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
/// An authorized stream used to import and validate a remote device before publication.
pub struct Wire {
    connection: crate::Connection,
    budget: Budget,
    timeout: Duration,
    imported: bool,
    capabilities: knightbus_core::Capabilities,
}
impl Wire {
    /// Use the same aggregate budget for framing, descriptors and native requests.
    pub fn new(
        stream: TcpStream,
        budget: Budget,
        timeout: Duration,
        capabilities: knightbus_core::Capabilities,
    ) -> Result<Self> {
        capabilities.validate()?;
        if capabilities.requests > SLOTS || capabilities.iso_packets != 0 {
            return Err(Error::Unsupported);
        }
        let limits = Limits {
            transfer_bytes: capabilities.transfer_bytes,
            frame_bytes: capabilities
                .transfer_bytes
                .checked_add(USBIP_TRANSFER_HEADER_BYTES)
                .ok_or(Error::Exhausted)?,
            ..LIMITS
        };
        Ok(Self {
            connection: crate::Connection::new(stream, budget.clone(), limits, timeout)?,
            budget,
            timeout,
            imported: false,
            capabilities,
        })
    }
    fn exchange(&mut self, message: &Message, context: Context) -> Result<DecodedMessage> {
        self.connection.send(message)?;
        let deadline = Instant::now() + self.timeout;
        loop {
            if Instant::now() >= deadline {
                return Err(Error::Deadline);
            }
            self.connection.flush()?;
            if let Some(reply) = self.connection.receive(|_, _| Ok(context))? {
                return Ok(reply);
            }
            if self.connection.take_progress() {
                std::thread::yield_now();
            } else {
                self.connection.wait(EXCHANGE_POLL_INTERVAL)?;
            }
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
            self.connection.close();
        }
        result
    }
    fn import_inner(
        &mut self,
        bus_id: usbip::WireString<USBIP_BUS_ID_BYTES>,
    ) -> Result<(ImportedDevice, Descriptors)> {
        let reply = self.exchange(&Message::ImportRequest(bus_id.clone()), Context::Management)?;
        let metadata = match reply.message() {
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
        let speed = Speed::try_from(metadata.speed).map_err(|_| Error::Unsupported)?;
        let (descriptors, config) = self.read_descriptors(id, speed)?;
        self.capabilities.descriptors(&descriptors)?;
        if descriptors.topology().vid != metadata.vendor_id
            || descriptors.topology().pid != metadata.product_id
            // Unconfigured devices advertise no active interfaces. Their raw
            // descriptors still describe the configurations we can select.
            || (metadata.configuration == 0 && metadata.interfaces != 0)
            || (metadata.configuration != 0
                && (descriptors.topology().interfaces != metadata.interfaces
                    || descriptors.topology().configuration != metadata.configuration))
            || metadata.configurations != 1
        {
            return Err(Error::Invalid);
        }
        let mut device = self.inventory(id, metadata, config.as_slice())?;
        self.select_configuration(id, speed, descriptors.topology().configuration)?;
        device.device.wire.configuration = descriptors.topology().configuration;
        device.device.wire.interfaces = descriptors.topology().interfaces;
        Ok((device, descriptors))
    }
    fn read_descriptors(&mut self, id: u32, speed: Speed) -> Result<(Descriptors, Buffer)> {
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
        if !(usb::configuration::BYTES..=self.capabilities.descriptor_bytes)
            .contains(&usize::from(length))
        {
            return Err(Error::Unsupported);
        }
        let config = self.descriptor(
            id,
            SEQ_CONFIG_DESCRIPTOR,
            u16::from(usb::configuration::TYPE) << DESCRIPTOR_TYPE_SHIFT,
            length,
        )?;
        let bos = if speed.superspeed() {
            let header = self.descriptor(
                id,
                SEQ_BOS_HEADER,
                u16::from(usb::BOS_TYPE) << DESCRIPTOR_TYPE_SHIFT,
                usb::BOS_HEADER as u16,
            )?;
            let length = usb::u16_at(header.as_slice(), 2);
            if !(usb::BOS_HEADER..=self.capabilities.descriptor_bytes)
                .contains(&usize::from(length))
            {
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
        let descriptors = Descriptors::new(
            &self.budget,
            device.as_slice(),
            config.as_slice(),
            bos.as_slice(),
            speed,
        )?;
        Ok((descriptors, config))
    }
    fn inventory(
        &self,
        id: u32,
        mut metadata: usbip::Device,
        config: &[u8],
    ) -> Result<ImportedDevice> {
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
                + usb::MAX_SETTINGS * std::mem::size_of::<knightbus_core::Interface>()
                + (config.len() / usb::endpoint::MIN_BYTES)
                    * std::mem::size_of::<knightbus_core::Endpoint>(),
        )?;
        let configurations = configurations(config)?;
        let device = Device {
            key,
            wire: metadata,
            configurations,
        };
        if device.storage_bytes()? > storage.bytes() {
            return Err(Error::Exhausted);
        }
        Ok(ImportedDevice {
            device,
            _storage: storage,
        })
    }
    fn select_configuration(&mut self, id: u32, speed: Speed, configuration: u8) -> Result<()> {
        // Establish and verify configuration/alternate-zero state before
        // publishing native endpoints, regardless of native startup ordering.
        self.control(
            id,
            SEQ_SET_CONFIGURATION + if speed.superspeed() { 2 } else { 0 },
            usb::setup(
                usb::RECIPIENT_DEVICE,
                usb::SET_CONFIGURATION,
                configuration.into(),
                0,
                0,
            ),
            0,
        )?;
        if self
            .control(
                id,
                SEQ_GET_CONFIGURATION + if speed.superspeed() { 2 } else { 0 },
                usb::READ_CONFIGURATION,
                1,
            )?
            .as_slice()
            != [configuration]
        {
            return Err(Error::Unavailable);
        }
        Ok(())
    }
    fn descriptor(&mut self, id: u32, sequence: u32, value: u16, length: u16) -> Result<Buffer> {
        self.control(
            id,
            sequence,
            usb::setup(usb::DIRECTION_IN, usb::GET_DESCRIPTOR, value, 0, length),
            length,
        )
    }
    fn control(
        &mut self,
        id: u32,
        sequence: u32,
        setup: [u8; usb::SETUP_SIZE],
        length: u16,
    ) -> Result<Buffer> {
        let header = SubmitHeader {
            seqnum: sequence,
            device_id: id,
            direction: if setup[usb::SETUP_TYPE] & usb::DIRECTION_IN != 0 {
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
    backend: knightbus_core::Importer<B, crate::exporter::Mono>,
    wire: Wire,
    device_id: u32,
    pending: [Option<Pending>; SLOTS],
    sequence: u32,
    idle: bool,
    error: Option<Error>,
    timeout: Duration,
    _metadata: MemoryReservation,
}
impl<B: ImporterBackend> Forwarder<B> {
    /// Compose an authorized imported stream with its native attachment. Uses
    /// wire sequences after the descriptor/configuration requests in `import`.
    /// Timeout bounds attach, control requests, cancellation and cleanup. Idle
    /// bulk/interrupt requests await native cancellation; hosts must separately
    /// enforce session leases and aggregate resource/connection limits.
    pub fn new(backend: B, wire: Wire, device: ImportedDevice, session: SessionId) -> Result<Self> {
        let budget = wire.budget.clone();
        let timeout = wire.timeout;
        if session.epoch == 0 || session.serial == 0 {
            return Err(Error::Invalid);
        }
        let metadata = budget.reserve_memory(std::mem::size_of::<Self>())?;
        let sequence = SEQ_GET_CONFIGURATION
            + if matches!(device.device().wire.speed, 5 | 6) {
                2
            } else {
                0
            };
        let mut backend = knightbus_core::Importer::new(
            backend,
            crate::exporter::Mono::default(),
            timeout.as_millis() as u64,
            budget.clone(),
        )?;
        backend.set_waker(Some(wire.connection.waker()));
        let device_id = (device.device.wire.bus_number << 16) | device.device.wire.device_number;
        backend.attach(session, device.device, timeout.as_millis() as u64)?;
        Ok(Self {
            backend,
            wire,
            device_id,
            pending: std::array::from_fn(|_| None),
            sequence,
            idle: true,
            error: None,
            timeout,
            _metadata: metadata,
        })
    }
    /// Validated metadata while the attachment remains owned.
    pub fn device(&self) -> Option<&Device> {
        self.backend.device()
    }
    /// Stop admission and request native retirement, including a partial attach.
    pub fn disconnect(&mut self) {
        self.backend.disconnect();
        self.wire.connection.close();
    }
    /// Standalone scheduling between ticks. Ready work yields; idle work waits
    /// for socket/backend events, at most 10 ms for host stop/deadline checks.
    /// Cleanup retains bounded retries until native retirement is confirmed.
    pub fn wait(&mut self) {
        if !self.idle {
            std::thread::yield_now();
        } else if matches!(
            self.backend.state(),
            knightbus_core::ImporterState::Draining | knightbus_core::ImporterState::Quarantined
        ) {
            std::thread::sleep(Duration::from_millis(1));
        } else if let Err(error) = self.wire.connection.wait(Duration::from_millis(10)) {
            self.error = Some(error);
            self.disconnect();
        }
    }
    /// Advance a bounded amount of I/O. Ready means native removal was confirmed;
    /// an error never implies that a stalled backend released its resources.
    pub fn tick(&mut self) -> Poll<Result<()>> {
        use knightbus_core::ImporterState;
        self.backend.tick();
        if self.backend.state() == ImporterState::Active {
            if let Err(error) = self.advance() {
                self.error = Some(error);
                self.disconnect();
                self.backend.tick();
            }
        }
        match self.backend.state() {
            ImporterState::Idle => {
                self.pending.iter_mut().for_each(|p| *p = None);
                return Poll::Ready(self.error.or(self.backend.error()).map_or(Ok(()), Err));
            }
            ImporterState::Quarantined => return Poll::Ready(Err(Error::Cleanup)),
            _ => {}
        }
        self.idle = !self.wire.connection.take_progress();
        Poll::Pending
    }
    fn next_sequence(&mut self) -> Result<u32> {
        self.sequence = self.sequence.checked_add(1).ok_or(Error::Exhausted)?;
        Ok(self.sequence)
    }
    fn advance(&mut self) -> Result<()> {
        self.wire.connection.flush()?;
        self.send_cancellations()?;
        self.send_next_transfer()?;
        self.receive_completions()?;
        self.check_deadlines()?;
        self.wire.connection.flush()
    }

    fn send_cancellations(&mut self) -> Result<()> {
        // Collect and send cancellation before competing for the single output
        // slot with a new SUBMIT. An already queued frame still finishes first.
        for _ in 0..SLOTS {
            let Some(seq) = self.backend.next_cancellation() else {
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
        if self.wire.connection.writable() {
            if let Some(index) = self.pending.iter().position(|p| {
                p.as_ref()
                    .is_some_and(|p| p.cancel && p.unlink.is_none() && p.transfer.is_some())
            }) {
                let seq = self.next_sequence()?;
                let p = self.pending[index].as_mut().unwrap();
                self.wire.connection.send(&Message::Unlink {
                    seqnum: seq,
                    device_id: self.device_id,
                    target_seqnum: p.context.seqnum,
                })?;
                p.unlink = Some(seq);
            }
        }
        Ok(())
    }

    fn send_next_transfer(&mut self) -> Result<()> {
        // At most one new native request per tick; the socket admits one frame.
        if self.wire.connection.writable() {
            if let Some(slot) = self.pending.iter().position(Option::is_none) {
                if let Some(transfer) = self.backend.next_transfer() {
                    let transfer = transfer?;
                    let mut header = *transfer.header();
                    header.seqnum = self.next_sequence()?;
                    let context = (&header).into();
                    self.wire.connection.send(usbip::Frame::Submit {
                        header: &header,
                        data: if header.direction == Direction::Out {
                            transfer.buffer()
                        } else {
                            &[]
                        },
                        packets: transfer.layout(),
                    })?;
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
        Ok(())
    }

    fn receive_completions(&mut self) -> Result<()> {
        for _ in 0..SLOTS {
            let pending = &self.pending;
            let Some(message) = self.wire.connection.receive(|command, seq| match command {
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
                Message::ReturnSubmit(reply) => self.complete_submit(reply)?,
                Message::ReturnUnlink { seqnum, status } => {
                    self.complete_unlink(*seqnum, *status)?
                }
                _ => return Err(Error::Invalid),
            }
        }
        Ok(())
    }

    fn check_deadlines(&self) -> Result<()> {
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
        Ok(())
    }

    fn complete_submit(&mut self, reply: &usbip::ReturnSubmit) -> Result<()> {
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
            .complete(Completion {
                transfer,
                status: reply.status,
                actual_length: reply.actual_length,
                start_frame: reply.start_frame,
            })
            .map_err(|_| Error::Internal)?;
        if p.unlink.is_none() {
            self.pending[index] = None;
        }
        Ok(())
    }
    fn complete_unlink(&mut self, seqnum: u32, status: i32) -> Result<()> {
        let index = self
            .pending
            .iter()
            .position(|p| p.as_ref().is_some_and(|p| p.unlink == Some(seqnum)))
            .ok_or(Error::Invalid)?;
        let p = self.pending[index].as_mut().unwrap();
        p.unlink = None;
        if status == usbip::status::CONNECTION_RESET {
            let transfer = p.transfer.take().ok_or(Error::Invalid)?;
            self.backend
                .complete(Completion {
                    transfer,
                    status: usbip::status::CONNECTION_RESET,
                    actual_length: 0,
                    start_frame: 0,
                })
                .map_err(|_| Error::Internal)?;
        } else if status > 0 {
            return Err(Error::Invalid);
        }
        // A failed unlink leaves the submit alive; never send repeated
        // unlinks or free its owner before the eventual reply.
        p.cancel = false;
        if p.transfer.is_none() {
            self.pending[index] = None;
        }
        Ok(())
    }
}
impl<B: ImporterBackend> Drop for Forwarder<B> {
    fn drop(&mut self) {
        self.disconnect();
        self.backend.set_waker(None);
    }
}

// Preserve exact vector capacity within the pre-admitted metadata bound.
fn configurations(bytes: &[u8]) -> Result<Vec<knightbus_core::Configuration>> {
    let configuration = knightbus_core::descriptors::configuration(bytes)?;
    let mut result = Vec::new();
    result.try_reserve_exact(1).map_err(|_| Error::Exhausted)?;
    result.push(configuration);
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Read;
    use std::net::TcpListener;
    const CAPS: knightbus_core::Capabilities = knightbus_core::Capabilities {
        transfer_bytes: 1024 * 1024,
        requests: 31,
        iso_packets: 0,
        speeds: 0b1101110,
        descriptor_bytes: 65535,
        configurations: 1,
        remote_wakeup: false,
    };

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
                configurations: vec![knightbus_core::Configuration {
                    value: 1,
                    interfaces: vec![knightbus_core::Interface {
                        number: 0,
                        alternate: 0,
                        class: usbip::Interface {
                            class: 0xff,
                            subclass: 0,
                            protocol: 0,
                        },
                        endpoints: vec![
                            knightbus_core::Endpoint {
                                address: 0x81,
                                kind: knightbus_core::TransferKind::Interrupt,
                                max_packet_size: 64,
                                interval: 8,
                                allowed_flags: 0,
                            },
                            knightbus_core::Endpoint {
                                address: 1,
                                kind: knightbus_core::TransferKind::Bulk,
                                max_packet_size: 64,
                                interval: 0,
                                allowed_flags: 0,
                            },
                        ],
                    }],
                }],
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
    fn cancellation_precedes_continuously_ready_submissions() {
        struct Native {
            delivered: std::sync::Arc<std::sync::atomic::AtomicU32>,
            canceled: bool,
        }
        impl ImporterBackend for Native {
            fn set_waker(&mut self, waker: Option<std::task::Waker>) {
                if let Some(waker) = waker {
                    waker.wake();
                }
            }
            fn attach(&mut self, _: SessionId, _: &Device) -> Poll<Result<()>> {
                Poll::Ready(Ok(()))
            }
            fn next_transfer(&mut self, _: SessionId, budget: &Budget) -> Option<Result<Transfer>> {
                let sequence = self
                    .delivered
                    .fetch_add(1, std::sync::atomic::Ordering::SeqCst)
                    + 1;
                Some(Transfer::new(
                    budget,
                    test_header(sequence, Direction::Out, 0),
                    &[],
                ))
            }
            fn next_cancellation(&mut self, _: SessionId) -> Option<u32> {
                if self.delivered.load(std::sync::atomic::Ordering::SeqCst) == 0 || self.canceled {
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
        let wire = Wire::new(stream, budget.clone(), Duration::from_secs(2), CAPS).unwrap();
        let delivered = std::sync::Arc::new(std::sync::atomic::AtomicU32::new(0));
        let mut f = Forwarder::new(
            Native {
                delivered: delivered.clone(),
                canceled: false,
            },
            wire,
            test_device(&budget),
            SessionId {
                epoch: 1,
                serial: 1,
            },
        )
        .unwrap();
        assert!(f.tick().is_pending());
        assert!(!f.idle, "submitting work must not add a timer delay");
        let Some(Message::Submit(first)) =
            usbip::read_message(&mut peer, Context::Requests, LIMITS, &mut |_| Ok(())).unwrap()
        else {
            panic!("first SUBMIT")
        };
        assert!(f.tick().is_pending());
        assert!(!f.idle, "cancellation is active work");
        let Some(Message::Unlink { target_seqnum, .. }) =
            usbip::read_message(&mut peer, Context::Requests, LIMITS, &mut |_| Ok(())).unwrap()
        else {
            panic!("cancellation was starved by a new SUBMIT")
        };
        assert_eq!(target_seqnum, first.seqnum);
        assert_eq!(
            delivered.load(std::sync::atomic::Ordering::SeqCst),
            1,
            "new work must wait behind cancellation"
        );
        f.disconnect();
        assert_eq!(f.tick(), Poll::Ready(Ok(())));
        drop(f);
        assert_eq!(budget.usage().bytes, 0);
        assert_eq!(budget.usage().requests, 0);
    }

    #[test]
    fn cancellation_races_keep_original_owner_until_remote_retirement() {
        use std::sync::{Arc, Mutex};
        struct Native {
            delivered: bool,
            canceled: Arc<std::sync::atomic::AtomicBool>,
            results: Arc<Mutex<Vec<(i32, u32)>>>,
        }
        impl ImporterBackend for Native {
            fn set_waker(&mut self, waker: Option<std::task::Waker>) {
                if let Some(waker) = waker {
                    waker.wake();
                }
            }
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
                if !self.delivered || self.canceled.load(std::sync::atomic::Ordering::SeqCst) {
                    return None;
                }
                self.canceled
                    .store(true, std::sync::atomic::Ordering::SeqCst);
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
                    usbip::read_message(&mut peer, Context::Requests, LIMITS, &mut |_| Ok(()))
                        .unwrap()
                else {
                    panic!("submit")
                };
                let Some(Message::Unlink {
                    seqnum,
                    target_seqnum,
                    ..
                }) = usbip::read_message(&mut peer, Context::Requests, LIMITS, &mut |_| Ok(()))
                    .unwrap()
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
                    usbip::write_message(&mut peer, &reply, LIMITS, &mut |_| Ok(())).unwrap();
                }
                usbip::write_message(&mut peer, &unlink, LIMITS, &mut |_| Ok(())).unwrap();
                if order >= 2 {
                    usbip::write_message(&mut peer, &reply, LIMITS, &mut |_| Ok(())).unwrap();
                }
                assert_eq!(peer.read(&mut [0]).unwrap(), 0);
            });
            let budget = Budget::new(knightbus_core::ResourceLimits {
                bytes: 1024 * 1024,
                requests: 4,
            });
            let device = test_device(&budget);
            let results = Arc::new(Mutex::new(vec![]));
            let canceled = Arc::new(std::sync::atomic::AtomicBool::new(order >= 3));
            let native = Native {
                delivered: false,
                canceled: canceled.clone(),
                results: results.clone(),
            };
            let timeout = Duration::from_secs(2);
            let wire = Wire::new(stream, budget.clone(), timeout, CAPS).unwrap();
            let mut forwarder = Forwarder::new(
                native,
                wire,
                device,
                SessionId {
                    epoch: 1,
                    serial: 1,
                },
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
                assert!(forwarder.idle, "idle requests must not busy-spin");
                canceled.store(false, std::sync::atomic::Ordering::SeqCst);
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
