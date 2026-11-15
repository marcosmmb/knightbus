//! Bounded standalone USB/IP host. The caller supplies
//! an authorized connection and peer identity; this module never authenticates it.
use knightbus_core::{
    usbip::{Context, Direction, Frame, Limits, Message, ReturnHeader, TRANSFER_HEADER_LEN},
    *,
};
use std::{
    net::TcpStream,
    time::{Duration, Instant},
};

const LIMITS: Limits = Limits {
    frame_bytes: 1024 * 1024 + TRANSFER_HEADER_LEN + 1024 * 16,
    transfer_bytes: 1024 * 1024,
    devices: 1,
    iso_packets: 1024,
};
const TIMEOUT: Duration = Duration::from_secs(30);
const POLL: Duration = Duration::from_millis(1);

/// Monotonic clock for either standalone hosting adapter.
pub struct Mono(Instant);
impl Default for Mono {
    fn default() -> Self {
        Self(Instant::now())
    }
}
impl Clock for Mono {
    fn now(&self) -> u64 {
        self.0.elapsed().as_millis() as u64
    }
}

struct Wire {
    connection: crate::Connection,
    trace: bool,
}
impl Wire {
    fn new(stream: TcpStream, budget: Budget) -> Result<Self> {
        Ok(Self {
            connection: crate::Connection::new(stream, budget, LIMITS, TIMEOUT)?,
            trace: std::env::var_os("KNIGHTBUS_TRACE_USB").is_some(),
        })
    }
    fn completion(&mut self, done: Completion) -> Result<()> {
        if done.transfer.header().endpoint == 0
            || done.status != 0
            || (self.trace && done.transfer.header().seqnum <= 256)
        {
            eprintln!(
                "usb completion seq={} ep={} setup={:02x?} status={} actual={} requested={}",
                done.transfer.header().seqnum,
                done.transfer.header().endpoint,
                done.transfer.header().setup,
                done.status,
                done.actual_length,
                done.transfer.header().transfer_buffer_length
            );
        }
        // The native owner stays charged while encoding its borrowed payload.
        let mut packed;
        let payload = if done.transfer.header().direction == Direction::In {
            if done.transfer.layout().is_empty() {
                &done.transfer.buffer()[..done.actual_length as usize]
            } else {
                packed = self.connection.budget.buffer(done.actual_length as usize)?;
                done.copy_payload(packed.as_mut_slice())?;
                packed.as_slice()
            }
        } else {
            &[]
        };
        self.connection.send(Frame::ReturnSubmit {
            header: ReturnHeader {
                request: done.reply_context(),
                status: done.status,
                actual_length: done.actual_length,
                start_frame: done.start_frame,
                number_of_packets: done.transfer.header().number_of_packets,
                error_count: done
                    .transfer
                    .packets()
                    .iter()
                    .filter(|p| p.status != 0)
                    .count() as u32,
            },
            data: payload,
            packets: done.transfer.packets(),
        })
    }
}
/// Serve one standard discovery/import connection. Import goes through the
/// engine grant and lease checks; success is sent only after physical acquisition.
/// On every exit the imported lease drains before return. A cleanup failure
/// retains ownership in the engine; the host must keep driving its shutdown.
pub fn serve<B: ExporterBackend, C: Clock>(
    engine: &mut Engine<B, C>,
    stream: TcpStream,
    caller: Caller,
) -> Result<()> {
    let mut session = None;
    let result = run(engine, stream, caller, &mut session);
    engine.set_waker(None);
    if let Some(id) = session {
        engine.disconnect(id)?;
        let deadline = Instant::now() + TIMEOUT;
        loop {
            engine.tick();
            let Reply::Session(s) = engine.execute(caller, Command::Status(id), u64::MAX)? else {
                return Err(Error::Internal);
            };
            if s.state == SessionState::Closed {
                break;
            }
            if Instant::now() >= deadline {
                return Err(Error::Cleanup);
            }
            std::thread::sleep(POLL);
        }
        eprintln!("exporter: native lease retired");
    }
    result
}
fn run<B: ExporterBackend, C: Clock>(
    engine: &mut Engine<B, C>,
    stream: TcpStream,
    caller: Caller,
    session: &mut Option<SessionId>,
) -> Result<()> {
    let mut wire = Wire::new(stream, engine.budget())?;
    engine.set_waker(Some(wire.connection.waker()));
    let start = Instant::now();
    let request = loop {
        if let Some(request) = wire.connection.receive(|_, _| Ok(Context::Management))? {
            break request;
        }
        if start.elapsed() >= TIMEOUT {
            return Err(Error::Deadline);
        }
        std::thread::sleep(POLL);
    };
    let bus = match request.message() {
        Message::DevListRequest => {
            // Discovery metadata remains bounded by the host's inventory limit.
            let _storage = engine.budget().reserve_memory(4096)?;
            wire.connection.send(&Message::DevListReply {
                status: 0,
                devices: engine.discover(caller),
            })?;
            while !wire.connection.writable() {
                wire.connection.flush()?;
                std::thread::sleep(POLL);
            }
            return Ok(());
        }
        Message::ImportRequest(bus) => bus.clone(),
        _ => return Err(Error::Invalid),
    };
    drop(request);
    let id = match engine.import(caller, &bus, engine.now() + TIMEOUT.as_millis() as u64) {
        Ok(id) => id,
        Err(error) => {
            wire.connection.send(&Message::ImportReply {
                status: 1,
                device: None,
            })?;
            while !wire.connection.writable() {
                wire.connection.flush()?;
                std::thread::sleep(POLL);
            }
            return Err(error);
        }
    };
    *session = Some(id);
    loop {
        engine.tick();
        let Reply::Session(s) = engine.execute(caller, Command::Status(id), u64::MAX)? else {
            return Err(Error::Internal);
        };
        if let Some(error) = s.error {
            return Err(error);
        }
        if s.state == SessionState::Active {
            break;
        }
        if start.elapsed() >= TIMEOUT {
            return Err(Error::Deadline);
        }
        std::thread::sleep(POLL);
    }
    let _metadata = engine.budget().reserve_memory(4096)?;
    let device = engine
        .discover(caller)
        .into_iter()
        .find(|d| d.device.bus_id == bus)
        .ok_or(Error::Denied)?
        .device;
    let wire_id = (device.bus_number << 16) | device.device_number;
    wire.connection.send(&Message::ImportReply {
        status: 0,
        device: Some(Box::new(device)),
    })?;
    eprintln!("exporter: physical acquisition complete; USB/IP imported");
    forward(engine, &mut wire, caller, id, wire_id)
}

fn forward<B: ExporterBackend, C: Clock>(
    engine: &mut Engine<B, C>,
    wire: &mut Wire,
    caller: Caller,
    id: SessionId,
    wire_id: u32,
) -> Result<()> {
    let mut heartbeat = Instant::now();
    loop {
        engine.tick();
        wire.connection.flush()?;
        if heartbeat.elapsed() >= Duration::from_secs(1) {
            engine.execute(caller, Command::Heartbeat(id), u64::MAX)?;
            heartbeat = Instant::now();
        }
        // Bound each turn, interleaving ingress and egress so an interrupt
        // producer cannot starve an UNLINK. Retired replies retain engine credits.
        let mut progress = false;
        if wire.connection.writable() {
            if let Some(message) = wire.connection.receive(|_, _| Ok(Context::Requests))? {
                progress = true;
                match message.message() {
                    Message::Submit(_) => {
                        let transfer = message.into_transfer()?;
                        if wire.trace && transfer.header().seqnum <= 256 {
                            eprintln!(
                                "usb submit seq={} ep={} direction={:?} length={}",
                                transfer.header().seqnum,
                                transfer.header().endpoint,
                                transfer.header().direction,
                                transfer.header().transfer_buffer_length
                            );
                        }
                        // Endpoint requests may wait for native data indefinitely;
                        // disconnect, lease expiry and UNLINK remain authoritative.
                        let deadline = if transfer.header().endpoint == 0 {
                            engine.now() + TIMEOUT.as_millis() as u64
                        } else {
                            u64::MAX
                        };
                        engine.submit(caller, id, transfer, deadline)?;
                    }
                    Message::Unlink {
                        seqnum,
                        device_id,
                        target_seqnum,
                    } if *device_id == wire_id => {
                        if engine.unlink(caller, id, *seqnum, *target_seqnum)?
                            == UnlinkState::TooLate
                        {
                            wire.connection.send(&Message::ReturnUnlink {
                                seqnum: *seqnum,
                                status: 0,
                            })?;
                        }
                    }
                    _ => return Err(Error::Invalid),
                }
            }
        }
        if wire.connection.writable() {
            if let Some(event) = engine.next_transfer_event(caller, id)? {
                progress = true;
                match event {
                    TransferEvent::Complete(done) => wire.completion(done)?,
                    TransferEvent::Unlinked { sequence } => {
                        wire.connection.send(&Message::ReturnUnlink {
                            seqnum: sequence,
                            status: usbip::status::CONNECTION_RESET,
                        })?
                    }
                    TransferEvent::Failed { error, .. } => return Err(error),
                }
            }
        }
        let Reply::Session(s) = engine.execute(caller, Command::Status(id), u64::MAX)? else {
            return Err(Error::Internal);
        };
        if s.state != SessionState::Active {
            return Err(s.error.unwrap_or(Error::Unavailable));
        }
        progress |= wire.connection.take_progress();
        if progress {
            std::thread::yield_now();
        } else {
            wire.connection.wait(Duration::from_millis(10))?;
        }
    }
}
