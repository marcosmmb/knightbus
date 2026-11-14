//! Bounded standalone USB/IP host around the shared engine. The caller supplies
//! an authorized connection and peer identity; this module never authenticates it.
use knightbus_core::{
    usbip::{Context, Direction, Limits, Message, ReturnSubmit, TRANSFER_HEADER_LEN},
    *,
};
use std::{
    io::{self, Read, Write},
    net::{Shutdown, TcpStream},
    time::{Duration, Instant},
};

const LIMITS: Limits = Limits {
    frame_bytes: crate::MAX_TRANSFER + TRANSFER_HEADER_LEN,
    transfer_bytes: crate::MAX_TRANSFER,
    devices: 1,
    iso_packets: 0,
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
    stream: TcpStream,
    budget: Budget,
    input: Buffer,
    start: usize,
    end: usize,
    decoder: Option<IncomingDecoder>,
    frame_started: Option<Instant>,
    output: Option<(Buffer, usize, Instant)>,
    trace: bool,
}
impl Wire {
    fn new(stream: TcpStream, budget: Budget) -> Result<Self> {
        stream.set_nonblocking(true).map_err(net)?;
        stream.set_nodelay(true).map_err(net)?;
        Ok(Self {
            stream,
            input: budget.buffer(16384)?,
            budget,
            start: 0,
            end: 0,
            decoder: None,
            frame_started: None,
            output: None,
            trace: std::env::var_os("KNIGHTBUS_TRACE_USB").is_some(),
        })
    }
    fn send(&mut self, message: &Message) -> Result<()> {
        if self.output.is_some() {
            return Err(Error::State);
        }
        let bytes = self
            .budget
            .encode(message, LIMITS)
            .map_err(|_| Error::Invalid)?;
        self.output = Some((bytes, 0, Instant::now()));
        Ok(())
    }
    fn flush(&mut self) -> Result<()> {
        if let Some((bytes, offset, admitted)) = &mut self.output {
            if admitted.elapsed() >= TIMEOUT {
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
                Err(e) if transient(&e) => {}
                Err(e) => return Err(net(e)),
            }
        }
        Ok(())
    }
    fn receive(&mut self, context: Context) -> Result<Option<DecodedMessage>> {
        if self.frame_started.is_some_and(|t| t.elapsed() >= TIMEOUT) {
            return Err(Error::Deadline);
        }
        if self.start == self.end {
            match self.stream.read(self.input.as_mut_slice()) {
                Ok(0) => return Err(Error::Unavailable),
                Ok(n) => {
                    self.start = 0;
                    self.end = n;
                }
                Err(e) if transient(&e) => return Ok(None),
                Err(e) => return Err(net(e)),
            }
        }
        if self.decoder.is_none() {
            self.decoder = Some(self.budget.decoder(context, LIMITS)?);
            self.frame_started = Some(Instant::now());
        }
        let (used, message) = self
            .decoder
            .as_mut()
            .unwrap()
            .push(&self.input.as_slice()[self.start..self.end])
            .map_err(|_| Error::Invalid)?;
        self.start += used;
        if message.is_some() {
            self.decoder = None;
            self.frame_started = None;
        }
        Ok(message)
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
        // Admit the intermediate codec model while the native owner remains charged.
        // ISO is explicitly outside this first standalone transport's capability.
        if !done.transfer.layout().is_empty() {
            return Err(Error::Unsupported);
        }
        let payload = if done.transfer.header().direction == Direction::In {
            &done.transfer.buffer()[..done.actual_length as usize]
        } else {
            &[]
        };
        let _storage = self.budget.reserve_memory(payload.len())?;
        self.send(&Message::ReturnSubmit(ReturnSubmit {
            request: done.reply_context(),
            status: done.status,
            actual_length: done.actual_length,
            start_frame: done.start_frame,
            number_of_packets: done.transfer.header().number_of_packets,
            error_count: 0,
            data: payload.to_vec(),
            iso_packets: Vec::new(),
        }))
    }
}
impl Drop for Wire {
    fn drop(&mut self) {
        let _ = self.stream.shutdown(Shutdown::Both);
    }
}
fn transient(e: &io::Error) -> bool {
    matches!(
        e.kind(),
        io::ErrorKind::WouldBlock | io::ErrorKind::Interrupted
    )
}
fn net(_: io::Error) -> Error {
    Error::Unavailable
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
    let start = Instant::now();
    let request = loop {
        if let Some(request) = wire.receive(Context::Management)? {
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
            wire.send(&Message::DevListReply {
                status: 0,
                devices: engine.discover(caller),
            })?;
            while wire.output.is_some() {
                wire.flush()?;
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
            wire.send(&Message::ImportReply {
                status: 1,
                device: None,
            })?;
            while wire.output.is_some() {
                wire.flush()?;
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
    wire.send(&Message::ImportReply {
        status: 0,
        device: Some(Box::new(device)),
    })?;
    eprintln!("exporter: physical acquisition complete; USB/IP imported");
    let mut heartbeat = Instant::now();
    loop {
        engine.tick();
        wire.flush()?;
        if heartbeat.elapsed() >= Duration::from_secs(1) {
            engine.execute(caller, Command::Heartbeat(id), u64::MAX)?;
            heartbeat = Instant::now();
        }
        // Bound each turn, interleaving ingress and egress so an interrupt
        // producer cannot starve an UNLINK. Retired replies retain engine credits.
        let mut progress = false;
        if wire.output.is_none() {
            if let Some(message) = wire.receive(Context::Requests)? {
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
                            wire.send(&Message::ReturnUnlink {
                                seqnum: *seqnum,
                                status: 0,
                            })?;
                        }
                    }
                    _ => return Err(Error::Invalid),
                }
            }
        }
        if wire.output.is_none() {
            if let Some(event) = engine.next_transfer_event(caller, id)? {
                progress = true;
                match event {
                    TransferEvent::Complete(done) => wire.completion(done)?,
                    TransferEvent::Unlinked { sequence } => wire.send(&Message::ReturnUnlink {
                        seqnum: sequence,
                        status: -104,
                    })?,
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
        if progress {
            std::thread::yield_now();
        } else {
            std::thread::sleep(POLL);
        }
    }
}
