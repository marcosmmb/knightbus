//! Bounded adapter from usbip-win2 requests to the shared importer worker.
use crate::{
    channel::{self, *},
    notification::Notification,
};
use knightbus_core::{
    Budget, Buffer, Completion, DescriptorSet, Error, ImporterBackend, Result, importer,
    topology::Speed,
};
use std::{collections::VecDeque, sync::Arc};

/// One attachment-owned upstream handle. Methods execute on the USB worker.
pub trait Driver: Send + 'static {
    /// Publish descriptors after budget admission; failure still requires detach.
    fn attach(&mut self, descriptors: &DescriptorSet, budget: &Budget) -> Result<()>;
    /// Read one complete native frame, or zero while the overlapped read is pending.
    fn read(&mut self, output: &mut [u8]) -> Result<usize>;
    /// Write a complete native reply, retaining storage until I/O completes.
    fn write(&mut self, input: &[u8]) -> Result<()>;
    /// Wake for native arrivals as well as replies and shutdown.
    fn notifications(&mut self, wake: Arc<crate::DriverWake>) -> Result<()>;
    /// Shut down only this handle's device and drain outstanding native I/O.
    fn detach(&mut self) -> Result<()>;
}
/// Connect the Windows channel to USB/IP discovery and forwarding.
pub fn connect<D: Driver>(
    stream: knightbus_transport::Stream,
    bus: knightbus_core::usbip::WireString<{ knightbus_core::usbip::BUS_ID_BYTES }>,
    session: knightbus_core::SessionId,
    budget: Budget,
    timeout: std::time::Duration,
    factory: impl FnOnce() -> Result<D> + Send + 'static,
) -> Result<knightbus_transport::importer::Forwarder<impl ImporterBackend>> {
    knightbus_transport::importer::connect(stream, bus, session, budget, timeout, move |_| {
        Ok(Channel::new(factory()?))
    })
}

/// Connect with a deadline/cancellation scope shared by all bootstrap exchanges.
pub fn connect_until<D: Driver, F: FnOnce() -> Result<D> + Send + 'static>(
    stream: knightbus_transport::Stream,
    bus: knightbus_core::usbip::WireString<{ knightbus_core::usbip::BUS_ID_BYTES }>,
    session: knightbus_core::SessionId,
    budget: Budget,
    timeout: std::time::Duration,
    operation: &knightbus_transport::operation::Operation<'_>,
    factory: F,
) -> Result<knightbus_transport::importer::Forwarder<impl ImporterBackend + use<D, F>>> {
    knightbus_transport::importer::connect_until(
        stream,
        bus,
        session,
        budget,
        timeout,
        operation,
        move |_| Ok(Channel::new(factory()?)),
    )
}

/// Bind the UDE channel to the importer worker and its admission budget.
pub fn new<D: Driver>(
    factory: impl FnOnce() -> Result<D> + Send + 'static,
    descriptors: DescriptorSet,
    budget: Budget,
) -> Result<impl ImporterBackend> {
    // Reject unsupported native descriptors before starting any worker.
    CAPABILITIES.descriptors(&descriptors)?;
    importer::Backend::new(move || Ok(Channel::new(factory()?)), descriptors, budget)
}
/// Limits of the adapter and pinned upstream UDE package.
/// SuperSpeed uses the USB 3 port; BOS requests are forwarded to the exporter.
pub const CAPABILITIES: knightbus_core::Capabilities = knightbus_core::Capabilities {
    transfer_bytes: MAX_TRANSFER,
    requests: SLOTS,
    iso_packets: MAX_PACKETS,
    speeds: Speed::ALL_MASK,
    descriptor_bytes: 4096,
    configurations: 1,
    remote_wakeup: false,
};
struct Queued {
    header: Header,
    bytes: Buffer,
}
struct Slot {
    header: Header,
    bytes: Option<Buffer>,
    cancelled: bool,
}
struct Channel<D> {
    driver: D,
    budget: Option<Budget>,
    topology: Option<knightbus_core::topology::Topology>,
    input: Option<Buffer>,
    waiting: VecDeque<Queued>,
    slots: [Option<Slot>; SLOTS],
    packets: [knightbus_core::usbip::IsoPacket; MAX_PACKETS],
    wake: Option<Arc<Notification>>,
}
impl<D> Channel<D> {
    fn new(driver: D) -> Self {
        Self {
            driver,
            budget: None,
            topology: None,
            input: None,
            waiting: VecDeque::with_capacity(MAX_QUEUED),
            slots: std::array::from_fn(|_| None),
            packets: std::array::from_fn(|_| knightbus_core::usbip::IsoPacket {
                offset: 0,
                length: 0,
                actual_length: 0,
                status: 0,
            }),
            wake: None,
        }
    }
}
impl<D: Driver> Channel<D> {
    fn receive(&mut self) -> Result<()> {
        for _ in 0..SLOTS {
            // Leave excess requests in the native driver until replies free space.
            if self.waiting.len() == MAX_QUEUED {
                break;
            }
            let buffer = self.input.as_mut().ok_or(Error::State)?;
            let count = self.driver.read(buffer.as_mut_slice())?;
            if count == 0 {
                break;
            }
            let bytes = buffer.as_slice().get(..count).ok_or(Error::Invalid)?;
            let frame = channel::decode(bytes)?;
            let sequence = match frame {
                Frame::Submit(h) => h.sequence,
                Frame::Unlink { sequence, .. } => sequence,
            };
            if self.waiting.iter().any(|q| q.header.sequence == sequence)
                || self
                    .slots
                    .iter()
                    .flatten()
                    .any(|s| s.header.sequence == sequence)
            {
                return Err(Error::Invalid);
            }
            match frame {
                Frame::Submit(header) => {
                    let mut owned = self.budget.as_ref().unwrap().buffer(count)?;
                    owned.as_mut_slice().copy_from_slice(bytes);
                    self.waiting.push_back(Queued {
                        header,
                        bytes: owned,
                    });
                }
                Frame::Unlink { sequence, target } => {
                    self.waiting.retain(|q| q.header.sequence != target);
                    for slot in self.slots.iter_mut().flatten() {
                        if slot.header.sequence == target {
                            slot.cancelled = true;
                            // Undelivered requests need no remote cancellation.
                        }
                    }
                    let mut reply = [0; HEADER];
                    put(&mut reply, field::COMMAND, RET_UNLINK);
                    put(&mut reply, field::SEQUENCE, sequence);
                    self.driver.write(&reply)?;
                }
            }
            if let Some(wake) = &self.wake {
                wake.notify();
            }
        }
        for slot in &mut self.slots {
            if slot
                .as_ref()
                .is_some_and(|s| s.cancelled && s.bytes.is_some())
            {
                *slot = None;
            }
        }
        let topology = self.topology.as_ref().ok_or(Error::State)?;
        let endpoint_count = topology.endpoints.iter().flatten().count();
        let mut index = 0;
        while index < self.waiting.len() {
            let h = self.waiting[index].header;
            let endpoint_slot = if h.endpoint == 0 {
                0
            } else {
                topology
                    .endpoints
                    .iter()
                    .flatten()
                    .position(|e| e.address == h.endpoint)
                    .ok_or(Error::Invalid)?
                    + 1
            };
            let slot = if self.slots[endpoint_slot].is_none() {
                Some(endpoint_slot)
            } else if h.packets != 0 {
                (endpoint_count + 1..SLOTS).find(|&i| self.slots[i].is_none())
            } else {
                None
            };
            if let Some(slot) = slot {
                let queued = self.waiting.remove(index).unwrap();
                self.slots[slot] = Some(Slot {
                    header: queued.header,
                    bytes: Some(queued.bytes),
                    cancelled: false,
                });
            } else {
                index += 1;
            }
        }
        Ok(())
    }
}
impl<D: Driver> importer::Driver for Channel<D> {
    const CAPABILITIES: knightbus_core::Capabilities = CAPABILITIES;
    type Signal = Notification;
    // Adapter bookkeeping only. Upstream kernel allocation is managed by Windows,
    // not bounded by KnightBus's userspace admission budget.
    const NATIVE_BYTES: usize = size_of::<Self>() + MAX_QUEUED * size_of::<Queued>();
    const SCRATCH_BYTES: usize = MAX_MESSAGE;
    fn attach(&mut self, descriptors: &DescriptorSet, budget: &Budget) -> Result<()> {
        CAPABILITIES.descriptors(descriptors)?;
        self.input = Some(budget.buffer(MAX_MESSAGE)?);
        self.budget = Some(budget.clone());
        self.topology = Some(*descriptors.topology());
        self.driver.attach(descriptors, budget)
    }
    fn fetch<'a>(
        &'a mut self,
        mask: u32,
        _: &knightbus_core::topology::Topology,
        scratch: &'a mut [u8],
    ) -> Result<Option<importer::Request<'a>>> {
        let Some(index) = self.slots.iter().enumerate().position(|(i, s)| {
            mask & (1 << i) != 0
                && s.as_ref()
                    .is_some_and(|s| !s.cancelled && s.bytes.is_some())
        }) else {
            return Ok(None);
        };
        let slot = self.slots[index].as_mut().unwrap();
        let h = slot.header;
        let bytes = slot.bytes.take().unwrap();
        scratch[..bytes.as_slice().len()].copy_from_slice(bytes.as_slice());
        let layout = &scratch[HEADER + h.payload..bytes.as_slice().len()];
        for (p, bytes) in self
            .packets
            .iter_mut()
            .zip(layout.as_chunks::<PACKET_BYTES>().0.iter())
        {
            *p = knightbus_core::usbip::IsoPacket {
                offset: word(bytes, packet::OFFSET),
                length: word(bytes, packet::LENGTH),
                actual_length: word(bytes, packet::ACTUAL_LENGTH),
                status: word(bytes, packet::STATUS) as i32,
            };
        }
        Ok(Some(importer::Request {
            slot: index,
            token: u64::from(h.sequence),
            endpoint: h.endpoint,
            length: h.length,
            setup: h.setup,
            payload: &scratch[HEADER..HEADER + h.payload],
            packets: &self.packets[..h.packets],
            flags: h.flags & !SHORT_NOT_OK,
            start_frame: 0,
        }))
    }
    fn complete(&mut self, token: u64, done: &Completion, scratch: &mut [u8]) -> Result<()> {
        let index = self
            .slots
            .iter()
            .position(|s| {
                s.as_ref()
                    .is_some_and(|s| u64::from(s.header.sequence) == token)
            })
            .ok_or(Error::State)?;
        let slot = self.slots[index].as_ref().unwrap();
        let h = slot.header;
        if !slot.cancelled {
            let payload = if h.input {
                done.actual_length as usize
            } else {
                0
            };
            let size = HEADER + payload + h.packets * PACKET_BYTES;
            let bytes = scratch.get_mut(..size).ok_or(Error::Invalid)?;
            bytes[..HEADER].fill(0);
            // Short packets are normal for ISO; their individual statuses apply.
            let status = if h.packets == 0
                && done.status == 0
                && h.flags & SHORT_NOT_OK != 0
                && done.actual_length < h.length
            {
                SHORT_TRANSFER
            } else {
                done.status
            };
            for (offset, value) in [
                (field::COMMAND, RET_SUBMIT),
                (field::SEQUENCE, h.sequence),
                (field::DEVICE, LOCAL_DEVICE_ID),
                (field::DIRECTION, u32::from(h.input)),
                (
                    field::ENDPOINT,
                    u32::from(h.endpoint & knightbus_core::topology::ENDPOINT_NUMBER_MASK),
                ),
                (field::FLAGS_STATUS_TARGET, status as u32),
                (field::LENGTH, done.actual_length),
                (field::FRAME, done.start_frame as u32),
                (field::PACKETS, h.packets as u32),
                (
                    field::INTERVAL_ERRORS,
                    done.transfer
                        .packets()
                        .iter()
                        .filter(|p| p.status != 0)
                        .count() as u32,
                ),
            ] {
                put(bytes, offset, value);
            }
            done.copy_payload(&mut bytes[HEADER..HEADER + payload])?;
            for (bytes, p) in bytes[HEADER + payload..]
                .as_chunks_mut::<PACKET_BYTES>()
                .0
                .iter_mut()
                .zip(done.transfer.packets())
            {
                for (offset, value) in [
                    (packet::OFFSET, p.offset),
                    (packet::LENGTH, p.length),
                    (packet::ACTUAL_LENGTH, p.actual_length),
                    (packet::STATUS, p.status as u32),
                ] {
                    put(bytes, offset, value);
                }
            }
            self.driver.write(bytes)?;
        }
        self.slots[index] = None;
        if let Some(wake) = &self.wake {
            wake.notify();
        }
        Ok(())
    }
    fn snapshot(&mut self) -> Result<importer::Snapshot> {
        self.receive()?;
        Ok(importer::Snapshot {
            live: self.slots.iter().enumerate().fold(0, |mask, (i, s)| {
                mask | if s.as_ref().is_some_and(|s| !s.cancelled) {
                    1 << i
                } else {
                    0
                }
            }),
            tokens: std::array::from_fn(|i| {
                self.slots[i]
                    .as_ref()
                    .map_or(0, |s| u64::from(s.header.sequence))
            }),
        })
    }
    fn notifications(&mut self, wake: Arc<Notification>) -> Result<()> {
        self.wake = Some(wake.clone());
        self.driver.notifications(wake)
    }
    fn detach(&mut self) -> Result<()> {
        self.driver.detach()
    }
}
impl importer::Signal for Notification {
    fn new(budget: &Budget) -> Result<Self> {
        Notification::new(budget)
    }
    fn notify(&self) {
        Notification::notify(self)
    }
    fn wait(&self) -> Result<()> {
        Notification::wait(self)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use knightbus_core::{
        ResourceLimits,
        importer::Driver as _,
        topology::{self as usb, Speed},
        usbip,
    };
    const DEVICE: [u8; 18] = [18, 1, 0, 2, 0, 0, 0, 64, 9, 18, 1, 0, 0, 1, 0, 0, 0, 1];
    const CONFIG: [u8; 32] = [
        9, 2, 32, 0, 1, 1, 0, 128, 50, 9, 4, 0, 0, 2, 255, 0, 0, 0, 7, 5, 2, 2, 0, 2, 0, 7, 5, 130,
        2, 0, 2, 0,
    ];
    #[derive(Default)]
    struct Mock {
        incoming: VecDeque<Vec<u8>>,
        outgoing: Vec<Vec<u8>>,
        detached: bool,
    }
    impl Driver for Mock {
        fn attach(&mut self, _: &DescriptorSet, _: &Budget) -> Result<()> {
            Ok(())
        }
        fn read(&mut self, out: &mut [u8]) -> Result<usize> {
            if let Some(bytes) = self.incoming.pop_front() {
                out[..bytes.len()].copy_from_slice(&bytes);
                Ok(bytes.len())
            } else {
                Ok(0)
            }
        }
        fn write(&mut self, bytes: &[u8]) -> Result<()> {
            self.outgoing.push(bytes.to_vec());
            Ok(())
        }
        fn notifications(&mut self, _: Arc<DriverWake>) -> Result<()> {
            Ok(())
        }
        fn detach(&mut self) -> Result<()> {
            self.detached = true;
            Ok(())
        }
    }
    use crate::DriverWake;
    fn setup() -> (Channel<Mock>, DescriptorSet, Budget) {
        let budget = Budget::new(ResourceLimits {
            bytes: 96 * 1024 * 1024,
            requests: 64,
        });
        let descriptors = DescriptorSet::new(&budget, &DEVICE, &CONFIG, &[], Speed::High).unwrap();
        let mut channel = Channel::new(Mock::default());
        channel.attach(&descriptors, &budget).unwrap();
        (channel, descriptors, budget)
    }
    fn submit(sequence: u32, endpoint: u8, length: u32) -> Vec<u8> {
        let input = endpoint & usb::DIRECTION_IN != 0;
        let mut bytes = vec![0; HEADER + if input { 0 } else { length as usize }];
        for (offset, value) in [
            (field::COMMAND, CMD_SUBMIT),
            (field::SEQUENCE, sequence),
            (field::DEVICE, LOCAL_DEVICE_ID),
            (field::DIRECTION, u32::from(input)),
            (
                field::ENDPOINT,
                u32::from(endpoint & usb::ENDPOINT_NUMBER_MASK),
            ),
            (field::LENGTH, length),
        ] {
            put(&mut bytes, offset, value);
        }
        bytes
    }
    fn unlink(sequence: u32, target: u32) -> Vec<u8> {
        let mut bytes = vec![0; HEADER];
        for (offset, value) in [
            (field::COMMAND, CMD_UNLINK),
            (field::SEQUENCE, sequence),
            (field::DEVICE, LOCAL_DEVICE_ID),
            (field::FLAGS_STATUS_TARGET, target),
        ] {
            put(&mut bytes, offset, value);
        }
        bytes
    }
    fn completion(budget: &Budget, header: Header, data: &[u8]) -> Completion {
        let mut transfer = knightbus_core::Transfer::new(
            budget,
            usbip::SubmitHeader {
                seqnum: 55,
                device_id: 0x10002,
                direction: if header.input {
                    usbip::Direction::In
                } else {
                    usbip::Direction::Out
                },
                endpoint: u32::from(header.endpoint & usb::ENDPOINT_NUMBER_MASK),
                transfer_flags: 0,
                transfer_buffer_length: header.length,
                start_frame: 0,
                number_of_packets: 0,
                interval: 0,
                setup: header.setup,
            },
            &[],
        )
        .unwrap();
        transfer.buffer_mut()[..data.len()].copy_from_slice(data);
        Completion {
            transfer,
            status: 0,
            actual_length: data.len() as u32,
            start_frame: 0,
        }
    }
    #[test]
    fn native_sequences_are_preserved_and_short_reads_are_reported() {
        let (mut channel, descriptors, budget) = setup();
        let mut input = submit(7, 130, 8);
        put(&mut input, field::FLAGS_STATUS_TARGET, SHORT_NOT_OK);
        // Upstream does not initialize setup for data transfers.
        input[field::SETUP..HEADER].fill(0xaa);
        channel.driver.incoming.push_back(input);
        let snapshot = channel.snapshot().unwrap();
        assert_eq!(snapshot.tokens[2], 7);
        let mut scratch = vec![0; MAX_MESSAGE];
        let r = channel
            .fetch(u32::MAX, descriptors.topology(), &mut scratch)
            .unwrap()
            .unwrap();
        assert_eq!((r.token, r.flags, r.setup), (7, 0, [0; 8]));
        let header = channel.slots[2].as_ref().unwrap().header;
        channel
            .complete(7, &completion(&budget, header, &[1, 2, 3]), &mut scratch)
            .unwrap();
        let reply = &channel.driver.outgoing[0];
        assert_eq!(word(reply, field::SEQUENCE), 7);
        assert_eq!(
            word(reply, field::FLAGS_STATUS_TARGET) as i32,
            SHORT_TRANSFER
        );
        assert_eq!(&reply[HEADER..], &[1, 2, 3]);
        channel.detach().unwrap();
        assert!(channel.driver.detached);
        drop(channel);
        drop(descriptors);
        assert_eq!(budget.usage().bytes, 0);
    }
    #[test]
    fn cancellation_retires_only_its_request_and_does_not_reuse_a_busy_slot() {
        let (mut channel, descriptors, budget) = setup();
        channel.driver.incoming.push_back(submit(1, 130, 4));
        channel.snapshot().unwrap();
        let mut scratch = vec![0; MAX_MESSAGE];
        channel
            .fetch(u32::MAX, descriptors.topology(), &mut scratch)
            .unwrap()
            .unwrap();
        let header = channel.slots[2].as_ref().unwrap().header;
        channel
            .driver
            .incoming
            .extend([submit(2, 130, 4), unlink(3, 1)]);
        let snapshot = channel.snapshot().unwrap();
        assert_eq!(snapshot.live & (1 << 2), 0);
        assert_eq!(snapshot.tokens[2], 1);
        assert!(
            channel
                .fetch(u32::MAX, descriptors.topology(), &mut scratch)
                .unwrap()
                .is_none()
        );
        channel
            .complete(1, &completion(&budget, header, &[]), &mut scratch)
            .unwrap();
        assert_eq!(channel.driver.outgoing.len(), 1); // Only RET_UNLINK; no reply to a cancelled URB.
        assert_eq!(
            word(&channel.driver.outgoing[0], field::COMMAND),
            RET_UNLINK
        );
        assert_eq!(channel.snapshot().unwrap().tokens[2], 2);
    }
    #[test]
    fn cancellation_before_delivery_never_reaches_the_network() {
        let (mut channel, _, _) = setup();
        channel
            .driver
            .incoming
            .extend([submit(1, 130, 4), unlink(2, 1)]);
        assert_eq!(channel.snapshot().unwrap().live, 0);
        assert!(channel.waiting.is_empty());
    }
    #[test]
    fn malformed_or_repeated_native_frames_fail_closed() {
        let (mut channel, _, _) = setup();
        channel
            .driver
            .incoming
            .extend([submit(1, 130, 4), submit(1, 130, 4)]);
        assert!(matches!(channel.snapshot(), Err(Error::Invalid)));
        let mut bytes = submit(2, 130, 4);
        put(&mut bytes, field::PACKETS, u32::MAX);
        assert!(decode(&bytes).is_ok());
        put(&mut bytes, field::PACKETS, MAX_PACKETS as u32 + 1);
        assert!(decode(&bytes).is_err());
        bytes.truncate(HEADER - 1);
        assert!(decode(&bytes).is_err());
    }
    #[test]
    fn out_of_order_native_sequences_are_accepted_but_live_duplicates_are_not() {
        let (mut channel, _, _) = setup();
        channel
            .driver
            .incoming
            .extend([submit(9, 130, 4), submit(3, 130, 4)]);
        channel.snapshot().unwrap();
        assert_eq!(
            channel.slots.iter().flatten().count() + channel.waiting.len(),
            2
        );
        channel.driver.incoming.push_back(submit(3, 130, 4));
        assert!(matches!(channel.snapshot(), Err(Error::Invalid)));
    }
    #[test]
    fn backlog_pauses_native_reads_and_resumes_after_completions() {
        const BACKLOG: usize = SLOTS * 3;
        const DELAYED_POLLS: usize = 20;
        let (mut channel, descriptors, budget) = setup();
        channel
            .driver
            .incoming
            .extend((1..=BACKLOG as u32).map(|seq| submit(seq, 130, 4)));
        channel.snapshot().unwrap();
        channel.snapshot().unwrap();
        assert_eq!(channel.waiting.len(), MAX_QUEUED);
        let retained = channel.driver.incoming.len();
        for _ in 0..DELAYED_POLLS {
            channel.snapshot().unwrap();
            assert_eq!(channel.driver.incoming.len(), retained);
            assert!(!channel.driver.detached);
        }
        let mut scratch = vec![0; MAX_MESSAGE];
        for sequence in 1..=BACKLOG as u32 {
            channel.snapshot().unwrap();
            let request = channel
                .fetch(u32::MAX, descriptors.topology(), &mut scratch)
                .unwrap()
                .unwrap();
            assert_eq!(request.token, u64::from(sequence));
            let header = channel.slots[request.slot].as_ref().unwrap().header;
            channel
                .complete(
                    u64::from(sequence),
                    &completion(&budget, header, &[1, 2, 3, 4]),
                    &mut scratch,
                )
                .unwrap();
        }
        assert!(channel.driver.incoming.is_empty());
        assert!(channel.waiting.is_empty());
        assert_eq!(channel.driver.outgoing.len(), BACKLOG);
        drop(channel);
        drop(descriptors);
        assert_eq!(budget.usage().bytes, 0);
    }
    const SUPER_DEVICE: [u8; 18] = [18, 1, 0x20, 3, 0, 0, 0, 9, 9, 18, 1, 0, 0, 1, 0, 0, 0, 1];
    const SUPER_CONFIG: [u8; 44] = [
        9, 2, 44, 0, 1, 1, 0, 128, 50, 9, 4, 0, 0, 2, 255, 0, 0, 0, 7, 5, 2, 2, 0, 4, 0, 6, 48, 0,
        0, 0, 0, 7, 5, 130, 2, 0, 4, 0, 6, 48, 0, 0, 0, 0,
    ];
    const SUPER_BOS: [u8; 22] = [
        5, 15, 22, 0, 2, 7, 16, 2, 0, 0, 0, 0, 10, 16, 3, 0, 14, 0, 1, 0, 0, 0,
    ];

    #[test]
    fn superspeed_publication_and_ep0_preserve_companions_and_bos() {
        use knightbus_core::topology as usb;
        for speed in [Speed::Super, Speed::SuperPlus] {
            let (_, _, budget) = setup();
            {
                let descriptors =
                    DescriptorSet::new(&budget, &SUPER_DEVICE, &SUPER_CONFIG, &SUPER_BOS, speed)
                        .unwrap();
                CAPABILITIES.descriptors(&descriptors).unwrap();
                assert_eq!(descriptors.device(), &SUPER_DEVICE);
                assert_eq!(descriptors.speed(), speed);
                let mut channel = Channel::new(Mock::default());
                channel.attach(&descriptors, &budget).unwrap();
                let mut scratch = vec![0; MAX_MESSAGE];
                for (index, (kind, physical, expected)) in [
                    (usb::device::TYPE, &SUPER_DEVICE[..], &SUPER_DEVICE[..]),
                    (
                        usb::configuration::TYPE,
                        &SUPER_CONFIG[..],
                        &SUPER_CONFIG[..],
                    ),
                    (usb::BOS_TYPE, &SUPER_BOS[..], &SUPER_BOS[..]),
                ]
                .into_iter()
                .enumerate()
                {
                    let sequence = index as u32 + 1;
                    let mut request = submit(sequence, usb::DIRECTION_IN, physical.len() as u32);
                    request[field::SETUP..HEADER].copy_from_slice(&usb::setup(
                        usb::DIRECTION_IN,
                        usb::GET_DESCRIPTOR,
                        u16::from(kind) << u8::BITS,
                        0,
                        physical.len() as u16,
                    ));
                    channel.driver.incoming.push_back(request);
                    channel.receive().unwrap();
                    channel
                        .fetch(u32::MAX, descriptors.topology(), &mut scratch)
                        .unwrap()
                        .unwrap();
                    let header = channel.slots[0].as_ref().unwrap().header;
                    let done = completion(&budget, header, physical);
                    channel
                        .complete(u64::from(sequence), &done, &mut scratch)
                        .unwrap();
                    assert_eq!(&channel.driver.outgoing.last().unwrap()[HEADER..], expected);
                    assert_eq!(&done.transfer.buffer()[..physical.len()], physical);
                }
            }
            assert_eq!(budget.usage().bytes, 0);
        }
    }

    #[test]
    fn native_reads_request_header_then_bounded_out_payload() {
        const STORAGE_COMMAND_BYTES: usize = 31;
        const LARGE_STORAGE_TRANSFER: usize = 397312;
        for length in [
            0,
            STORAGE_COMMAND_BYTES,
            LARGE_STORAGE_TRANSFER,
            MAX_TRANSFER,
        ] {
            let mut frame = ReadFrame::default();
            let bytes = submit(1, 2, length as u32);
            assert_eq!(frame.remaining(), 0..HEADER);
            let ready = frame.advance(&bytes, HEADER).unwrap();
            if length == 0 {
                assert_eq!(ready, Some(HEADER));
            } else {
                assert_eq!(ready, None);
                assert_eq!(frame.remaining(), HEADER..bytes.len());
                assert_eq!(frame.advance(&bytes, length).unwrap(), Some(bytes.len()));
            }
            assert_eq!(frame.remaining(), 0..HEADER);
            assert!(decode(&bytes).is_ok());
        }
        let mut frame = ReadFrame::default();
        let mut header = submit(1, 130, 0);
        put(&mut header, field::LENGTH, MAX_TRANSFER as u32 + 1);
        assert!(matches!(
            frame.advance(&header, HEADER),
            Err(Error::Unsupported)
        ));
        assert!(ReadFrame::default().advance(&header, 0).is_err());
        assert_eq!(
            ReadFrame::default().advance(&header, HEADER - 1).unwrap(),
            None
        );
        assert!(ReadFrame::default().advance(&header, HEADER + 1).is_err());
        let bytes = submit(1, 2, STORAGE_COMMAND_BYTES as u32);
        let mut frame = ReadFrame::default();
        assert_eq!(frame.advance(&bytes, HEADER).unwrap(), None);
        assert_eq!(
            frame.advance(&bytes, STORAGE_COMMAND_BYTES - 1).unwrap(),
            None
        );
        assert_eq!(frame.advance(&bytes, 1).unwrap(), Some(bytes.len()));
    }

    #[test]
    fn native_reads_iso_packet_layout_even_for_input() {
        const PACKETS: usize = 2;
        let mut bytes = submit(1, 130, 8);
        put(&mut bytes, field::FLAGS_STATUS_TARGET, usbip::ISO_ASAP);
        put(&mut bytes, field::PACKETS, PACKETS as u32);
        bytes.resize(HEADER + PACKETS * PACKET_BYTES, 0);
        let mut frame = ReadFrame::default();
        assert_eq!(frame.advance(&bytes, HEADER).unwrap(), None);
        assert_eq!(frame.remaining(), HEADER..bytes.len());
        assert_eq!(
            frame.advance(&bytes, PACKETS * PACKET_BYTES).unwrap(),
            Some(bytes.len())
        );
    }

    #[test]
    fn iso_packet_layout_and_asap_flags_are_decoded_without_changing_payload() {
        let mut bytes = submit(1, 130, 8);
        put(&mut bytes, field::FLAGS_STATUS_TARGET, usbip::ISO_ASAP);
        put(&mut bytes, field::PACKETS, 2);
        bytes.resize(HEADER + 2 * PACKET_BYTES, 0);
        put(&mut bytes, HEADER + packet::LENGTH, 4);
        put(&mut bytes, HEADER + PACKET_BYTES, 4);
        put(&mut bytes, HEADER + PACKET_BYTES + packet::LENGTH, 4);
        let Frame::Submit(header) = decode(&bytes).unwrap() else {
            panic!()
        };
        assert_eq!((header.packets, header.payload), (2, 0));
        put(&mut bytes, field::FLAGS_STATUS_TARGET, 0);
        assert!(matches!(decode(&bytes), Err(Error::Unsupported)));
    }

    #[test]
    fn iso_completion_packs_sparse_data_and_keeps_per_packet_results() {
        const OUT_DESCRIPTOR: usize = usb::configuration::BYTES + usb::interface::BYTES;
        const IN_DESCRIPTOR: usize = OUT_DESCRIPTOR + usb::endpoint::MIN_BYTES;
        const MAX_PACKET: u16 = 64;
        const INTERVAL: u8 = 1;
        const NATIVE_SEQUENCE: u32 = 9;
        const ENDPOINT: u8 = 2;
        const PACKET_COUNT: usize = 2;
        const PACKET_CAPACITY: u32 = 4;
        const TRANSFER_BYTES: u32 = PACKET_COUNT as u32 * PACKET_CAPACITY;
        const PACKED_DATA: &[u8] = &[1, 2, 3, 4, 5];
        for flags in [usbip::ISO_ASAP, usbip::ISO_ASAP | SHORT_NOT_OK] {
            for packet_status in [0, usbip::status::PIPE] {
                let (_, _, budget) = setup();
                let mut config = CONFIG;
                for offset in [OUT_DESCRIPTOR, IN_DESCRIPTOR] {
                    config[offset + usb::endpoint::ATTRIBUTES] = usb::TRANSFER_ISOCHRONOUS;
                    let max_packet = offset + usb::endpoint::MAX_PACKET;
                    config[max_packet..max_packet + size_of::<u16>()]
                        .copy_from_slice(&MAX_PACKET.to_le_bytes());
                    config[offset + usb::endpoint::INTERVAL] = INTERVAL;
                }
                let descriptors =
                    DescriptorSet::new(&budget, &DEVICE, &config, &[], Speed::High).unwrap();
                let mut channel = Channel::new(Mock::default());
                channel.attach(&descriptors, &budget).unwrap();
                let mut bytes = submit(
                    NATIVE_SEQUENCE,
                    usb::DIRECTION_IN | ENDPOINT,
                    TRANSFER_BYTES,
                );
                put(&mut bytes, field::FLAGS_STATUS_TARGET, flags);
                put(&mut bytes, field::PACKETS, PACKET_COUNT as u32);
                bytes.resize(HEADER + PACKET_COUNT * PACKET_BYTES, 0);
                put(&mut bytes, HEADER + packet::LENGTH, PACKET_CAPACITY);
                put(
                    &mut bytes,
                    HEADER + PACKET_BYTES + packet::OFFSET,
                    PACKET_CAPACITY,
                );
                put(
                    &mut bytes,
                    HEADER + PACKET_BYTES + packet::LENGTH,
                    PACKET_CAPACITY,
                );
                channel.driver.incoming.push_back(bytes);
                channel.snapshot().unwrap();
                let mut scratch = vec![0; MAX_MESSAGE];
                let request = channel
                    .fetch(u32::MAX, descriptors.topology(), &mut scratch)
                    .unwrap()
                    .unwrap();
                let mut transfer = knightbus_core::Transfer::new(
                    &budget,
                    usbip::SubmitHeader {
                        seqnum: 1,
                        device_id: 0x10002,
                        direction: usbip::Direction::In,
                        endpoint: u32::from(ENDPOINT),
                        transfer_flags: usbip::ISO_ASAP,
                        transfer_buffer_length: TRANSFER_BYTES,
                        start_frame: 0,
                        number_of_packets: PACKET_COUNT as i32,
                        interval: i32::from(INTERVAL),
                        setup: [0; usb::SETUP_SIZE],
                    },
                    request.packets,
                )
                .unwrap();
                transfer
                    .buffer_mut()
                    .copy_from_slice(&[1, 2, 0, 0, 3, 4, 5, 0]);
                transfer.packets_mut()[0].actual_length = 2;
                transfer.packets_mut()[1].actual_length = 3;
                transfer.packets_mut()[1].status = packet_status;
                channel
                    .complete(
                        u64::from(NATIVE_SEQUENCE),
                        &Completion {
                            transfer,
                            status: 0,
                            actual_length: PACKED_DATA.len() as u32,
                            start_frame: 0,
                        },
                        &mut scratch,
                    )
                    .unwrap();
                let reply = &channel.driver.outgoing[0];
                assert_eq!(
                    word(reply, field::FLAGS_STATUS_TARGET),
                    0,
                    "short ISO packets must not fail the transfer"
                );
                assert_eq!(
                    word(reply, field::INTERVAL_ERRORS),
                    u32::from(packet_status != 0)
                );
                assert_eq!(&reply[HEADER..HEADER + PACKED_DATA.len()], PACKED_DATA);
                let layout = &reply[HEADER + PACKED_DATA.len()..];
                assert_eq!(word(layout, packet::ACTUAL_LENGTH), 2);
                assert_eq!(word(layout, PACKET_BYTES + packet::OFFSET), PACKET_CAPACITY);
                assert_eq!(word(layout, PACKET_BYTES + packet::ACTUAL_LENGTH), 3);
                assert_eq!(
                    word(layout, PACKET_BYTES + packet::STATUS) as i32,
                    packet_status
                );
            }
        }
    }

    #[test]
    fn out_data_and_control_setup_survive_native_decoding() {
        let (mut channel, descriptors, _) = setup();
        let mut bytes = submit(1, 2, 4);
        bytes[HEADER..].copy_from_slice(&[3, 1, 4, 1]);
        channel.driver.incoming.push_back(bytes);
        channel.snapshot().unwrap();
        let mut scratch = vec![0; MAX_MESSAGE];
        let request = channel
            .fetch(u32::MAX, descriptors.topology(), &mut scratch)
            .unwrap()
            .unwrap();
        assert_eq!(request.payload, &[3, 1, 4, 1]);
        let mut control = submit(2, 0, 4);
        control[field::SETUP..HEADER].copy_from_slice(&[0x40, 0x51, 0, 0, 0, 0, 4, 0]);
        let Frame::Submit(header) = decode(&control).unwrap() else {
            panic!()
        };
        assert_eq!(header.setup, [0x40, 0x51, 0, 0, 0, 0, 4, 0]);
        control[field::SETUP] = 0x80;
        assert!(decode(&control).is_err(), "direction disagrees with setup");
    }

    #[test]
    fn descriptor_replies_preserve_physical_revision_including_short_reads() {
        use knightbus_core::topology as usb;
        let (mut channel, descriptors, budget) = setup();
        let mut scratch = vec![0; MAX_MESSAGE];
        let mut sequence = 0;
        for request_type in [
            usb::DIRECTION_IN,
            usb::DIRECTION_IN | usb::REQUEST_TYPE_MASK,
        ] {
            for length in [0, 2, 3, 4, usb::device::BYTES] {
                sequence += 1;
                let mut request = submit(sequence, usb::DIRECTION_IN, length as u32);
                let setup = usb::setup(
                    request_type,
                    usb::GET_DESCRIPTOR,
                    u16::from(usb::device::TYPE) << u8::BITS,
                    0,
                    length as u16,
                );
                request[field::SETUP..HEADER].copy_from_slice(&setup);
                channel.driver.incoming.push_back(request);
                channel.receive().unwrap();
                channel
                    .fetch(u32::MAX, descriptors.topology(), &mut scratch)
                    .unwrap()
                    .unwrap();
                let header = channel.slots[0].as_ref().unwrap().header;
                let done = completion(&budget, header, &DEVICE[..length]);
                channel
                    .complete(u64::from(sequence), &done, &mut scratch)
                    .unwrap();
                let expected = DEVICE;
                assert_eq!(
                    &channel.driver.outgoing.last().unwrap()[HEADER..],
                    &expected[..length]
                );
                assert_eq!(&done.transfer.buffer()[..length], &DEVICE[..length]);
            }
        }
    }
}
