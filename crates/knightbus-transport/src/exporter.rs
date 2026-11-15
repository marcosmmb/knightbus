//! Shared USB/IP serving and native cleanup. Secure hosts bind TLS identity and
//! control liveness before forwarding; raw serving is an explicit compatibility path.
use knightbus_core::{
    usbip::{Context, Direction, Frame, Limits, Message, ReturnHeader, TRANSFER_HEADER_LEN},
    *,
};
use std::time::{Duration, Instant};

const LIMITS: Limits = Limits {
    frame_bytes: 1024 * 1024 + TRANSFER_HEADER_LEN + 1024 * 16,
    transfer_bytes: 1024 * 1024,
    devices: 256,
    iso_packets: 1024,
};
const DISCOVERY_STORAGE_BYTES: usize = LIMITS.devices
    * (size_of::<usbip::ListedDevice>() + (u8::MAX as usize + 1) * size_of::<usbip::Interface>());
const HEARTBEAT_INTERVAL: Duration = Duration::from_secs(1);
const IDLE_WAIT: Duration = Duration::from_millis(10);
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
}
impl Wire {
    fn new(stream: crate::Stream, budget: Budget) -> Result<Self> {
        Ok(Self {
            connection: crate::Connection::new(stream, budget, LIMITS, TIMEOUT)?,
        })
    }
    fn completion(&mut self, done: Completion) -> Result<()> {
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
    fn receive_request<B: ExporterBackend, C: Clock>(
        &mut self,
        engine: &mut Engine<B, C>,
        caller: Caller,
        session: SessionId,
        wire_id: u32,
    ) -> Result<bool> {
        if !self.connection.writable() {
            return Ok(false);
        }
        let Some(message) = self.connection.receive(|_, _| Ok(Context::Requests))? else {
            return Ok(false);
        };
        match message.message() {
            Message::Submit(_) => {
                let transfer = message.into_transfer()?;
                // Data endpoints may wait indefinitely; UNLINK and leases still apply.
                let deadline = if transfer.header().endpoint == 0 {
                    engine.now() + TIMEOUT.as_millis() as u64
                } else {
                    u64::MAX
                };
                engine.submit(caller, session, transfer, deadline)?;
            }
            Message::Unlink {
                seqnum,
                device_id,
                target_seqnum,
            } if *device_id == wire_id => {
                if engine.unlink(caller, session, *seqnum, *target_seqnum)? == UnlinkState::TooLate
                {
                    self.connection.send(&Message::ReturnUnlink {
                        seqnum: *seqnum,
                        status: 0,
                    })?;
                }
            }
            _ => return Err(Error::Invalid),
        }
        Ok(true)
    }

    fn send_completion<B: ExporterBackend, C: Clock>(
        &mut self,
        engine: &mut Engine<B, C>,
        caller: Caller,
        session: SessionId,
    ) -> Result<bool> {
        if !self.connection.writable() {
            return Ok(false);
        }
        let Some(event) = engine.next_transfer_event(caller, session)? else {
            return Ok(false);
        };
        match event {
            TransferEvent::Complete(done) => self.completion(done)?,
            TransferEvent::Unlinked { sequence } => {
                self.connection.send(&Message::ReturnUnlink {
                    seqnum: sequence,
                    status: usbip::status::CONNECTION_RESET,
                })?
            }
            TransferEvent::Failed { error, .. } => return Err(error),
        }
        Ok(true)
    }
}
pub(crate) trait Access {
    type Backend: ExporterBackend;
    type Clock: Clock;
    fn with<T>(
        &mut self,
        f: impl FnOnce(&mut Engine<Self::Backend, Self::Clock>) -> Result<T>,
    ) -> Result<T>;
}
impl<B: ExporterBackend, C: Clock> Access for Engine<B, C> {
    type Backend = B;
    type Clock = C;
    fn with<T>(&mut self, f: impl FnOnce(&mut Self) -> Result<T>) -> Result<T> {
        f(self)
    }
}
impl<B: ExporterBackend, C: Clock> Access for &std::sync::Mutex<Engine<B, C>> {
    type Backend = B;
    type Clock = C;
    fn with<T>(&mut self, f: impl FnOnce(&mut Engine<B, C>) -> Result<T>) -> Result<T> {
        f(&mut *self.lock().map_err(|_| Error::Internal)?)
    }
}
/// Serve an explicitly authorized raw loopback connection. Secure peers use
/// [`crate::host::Host`], which binds identity and control liveness.
pub fn serve_insecure<B: ExporterBackend, C: Clock>(
    engine: &mut Engine<B, C>,
    stream: crate::Stream,
    caller: Caller,
) -> Result<()> {
    if stream.caller().is_some() {
        return Err(Error::Denied);
    }
    let wire = Wire::new(stream, engine.budget())?;
    engine.set_waker(Some(wire.connection.waker()));
    let result = serve_on(engine, wire, caller, None);
    engine.set_waker(None);
    result
}
pub(crate) fn authenticated<E: Access>(
    engine: &mut E,
    stream: crate::Stream,
    alive: &crate::host::Lease,
    wakes: &crate::host::Wakes,
) -> Result<()> {
    let caller = stream.caller().ok_or(Error::Denied)?;
    let wire = Wire::new(stream, engine.with(|e| Ok(e.budget()))?)?;
    let registration = wakes.register(wire.connection.waker())?;
    let result = serve_on(engine, wire, caller, Some(alive));
    wakes.unregister(registration);
    result
}
fn serve_on<E: Access>(
    engine: &mut E,
    mut wire: Wire,
    caller: Caller,
    alive: Option<&crate::host::Lease>,
) -> Result<()> {
    let mut session = None;
    let result = run(engine, &mut wire, caller, &mut session, alive);
    wire.connection.close();
    if let Some(id) = session {
        engine.with(|e| e.disconnect(id))?;
        let deadline = Instant::now() + TIMEOUT;
        loop {
            let closed = engine.with(|e| {
                e.tick();
                let Reply::Session(s) = e.execute(caller, Command::Status(id), u64::MAX)? else {
                    return Err(Error::Internal);
                };
                Ok(s.state == SessionState::Closed)
            })?;
            if closed {
                engine.with(|e| e.execute(caller, Command::Forget(id), u64::MAX))?;
                break;
            }
            if Instant::now() >= deadline {
                return Err(Error::Cleanup);
            }
            std::thread::sleep(POLL);
        }
    }
    result
}
fn run<E: Access>(
    engine: &mut E,
    wire: &mut Wire,
    caller: Caller,
    session: &mut Option<SessionId>,
    alive: Option<&crate::host::Lease>,
) -> Result<()> {
    let start = Instant::now();
    if let Some(alive) = alive {
        alive.check()?;
    }
    let request = loop {
        if let Some(alive) = alive {
            alive.check()?;
        }
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
            let _storage = engine.with(|e| e.budget().reserve_memory(DISCOVERY_STORAGE_BYTES))?;
            wire.connection.send(&Message::DevListReply {
                status: 0,
                devices: engine.with(|e| Ok(e.discover(caller)))?,
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
    let id = match engine.with(|e| e.import(caller, &bus, e.now() + TIMEOUT.as_millis() as u64)) {
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
        if let Some(alive) = alive {
            alive.check()?;
        }
        let Reply::Session(s) = engine.with(|e| {
            e.tick();
            e.execute(caller, Command::Status(id), u64::MAX)
        })?
        else {
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
    let _metadata = engine.with(|e| e.budget().reserve_memory(DISCOVERY_STORAGE_BYTES))?;
    let device = engine
        .with(|e| Ok(e.discover(caller)))?
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
    forward(engine, wire, caller, id, wire_id, alive)
}

fn forward<E: Access>(
    engine: &mut E,
    wire: &mut Wire,
    caller: Caller,
    id: SessionId,
    wire_id: u32,
    alive: Option<&crate::host::Lease>,
) -> Result<()> {
    let mut heartbeat = Instant::now();
    loop {
        if let Some(alive) = alive {
            alive.check()?;
        }
        let progress = engine.with(|engine| {
            engine.tick();
            wire.connection.flush()?;
            if heartbeat.elapsed() >= HEARTBEAT_INTERVAL {
                engine.execute(caller, Command::Heartbeat(id), u64::MAX)?;
                heartbeat = Instant::now();
            }
            // One ingress and one egress operation per turn keep UNLINK responsive.
            let progress = wire.receive_request(engine, caller, id, wire_id)?;
            let progress = wire.send_completion(engine, caller, id)? || progress;
            let Reply::Session(s) = engine.execute(caller, Command::Status(id), u64::MAX)? else {
                return Err(Error::Internal);
            };
            if s.state != SessionState::Active {
                return Err(s.error.unwrap_or(Error::Unavailable));
            }
            Ok(wire.connection.take_progress() || progress)
        })?;
        if progress {
            std::thread::yield_now();
        } else {
            wire.connection.wait(IDLE_WAIT)?;
        }
    }
}
