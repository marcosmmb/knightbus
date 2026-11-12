use knightbus_core::*;
const MAX_FRAME: usize = 8192;
const MAX_ITEMS: usize = 256;

// Fixed widths in this provisional control schema, independent of Rust layouts.
const LENGTH_PREFIX_BYTES: usize = 4;
const DEVICE_KEY_BYTES: usize = 16; // device ID + generation, both u64
const DEVICE_INFO_BYTES: usize = DEVICE_KEY_BYTES + 1; // key + busy boolean

// Request and reply tags have separate namespaces; preserve their v1 wire values.
const CMD_LIST: u8 = 0;
const CMD_AUTHORIZE: u8 = 1;
const CMD_ATTACH: u8 = 2;
const CMD_DETACH: u8 = 3;
const CMD_STATUS: u8 = 4;
const CMD_HEARTBEAT: u8 = 5;
const CMD_SUBSCRIBE: u8 = 6;
const CMD_POLL: u8 = 7;
const CMD_UNSUBSCRIBE: u8 = 8;
const CMD_FORGET: u8 = 9;
const CMD_SHUTDOWN: u8 = 10;

const REPLY_DONE: u8 = 0;
const REPLY_DEVICES: u8 = 1;
const REPLY_SESSION: u8 = 2;
const REPLY_SUBSCRIPTION: u8 = 3;
const REPLY_EVENT: u8 = 4;

const STATE_ACQUIRING: u8 = 0;
const STATE_ACTIVE: u8 = 1;
const STATE_DRAINING: u8 = 2;
const STATE_QUARANTINED: u8 = 3;
const STATE_CLOSED: u8 = 4;

const EVENT_INVENTORY: u8 = 0;
const EVENT_SESSION: u8 = 1;
const EVENT_RESYNC: u8 = 2;

// Error discriminants are owned by the shared control contract.
const STATUS_OK: u16 = 0;
const STATUS_INVALID: u16 = Error::Invalid as u16;
const STATUS_STATE: u16 = Error::State as u16;
const STATUS_UNSUPPORTED: u16 = Error::Unsupported as u16;
const STATUS_UNAVAILABLE: u16 = Error::Unavailable as u16;
const STATUS_DENIED: u16 = Error::Denied as u16;
const STATUS_NOT_FOUND: u16 = Error::NotFound as u16;
const STATUS_VERSION: u16 = Error::Version as u16;
const STATUS_DEADLINE: u16 = Error::Deadline as u16;
const STATUS_CANCELLED: u16 = Error::Cancelled as u16;
const STATUS_EXHAUSTED: u16 = Error::Exhausted as u16;
const STATUS_INTERNAL: u16 = Error::Internal as u16;
const STATUS_CLEANUP: u16 = Error::Cleanup as u16;

/// Encode a bounded M6 management request, with a little-endian u32 body length.
/// This provisional harness format is independent of standard USB/IP framing.
pub fn encode_request(request: &Request) -> Result<Vec<u8>> {
    let mut b = Vec::new();
    b.extend_from_slice(&[0; LENGTH_PREFIX_BYTES]);
    b.extend_from_slice(&request.version.to_le_bytes());
    b.extend_from_slice(&request.id.to_le_bytes());
    b.extend_from_slice(&request.deadline.to_le_bytes());
    let mut put = |tag: u8| b.push(tag);
    match &request.command {
        Command::List => put(CMD_LIST),
        Command::Authorize {
            device,
            peer,
            allow,
        } => {
            put(CMD_AUTHORIZE);
            key(&mut b, *device);
            b.extend_from_slice(&peer.0);
            b.push(u8::from(*allow));
        }
        Command::Attach(keys) => {
            if keys.is_empty() || keys.len() > MAX_ITEMS {
                return Err(Error::Invalid);
            }
            put(CMD_ATTACH);
            b.extend_from_slice(&(keys.len() as u16).to_le_bytes());
            for k in keys {
                key(&mut b, *k);
            }
        }
        Command::Detach(id) => {
            put(CMD_DETACH);
            session(&mut b, *id);
        }
        Command::Status(id) => {
            put(CMD_STATUS);
            session(&mut b, *id);
        }
        Command::Heartbeat(id) => {
            put(CMD_HEARTBEAT);
            session(&mut b, *id);
        }
        Command::Subscribe => put(CMD_SUBSCRIBE),
        Command::Poll(id) => {
            put(CMD_POLL);
            b.extend_from_slice(&id.to_le_bytes());
        }
        Command::Unsubscribe(id) => {
            put(CMD_UNSUBSCRIBE);
            b.extend_from_slice(&id.to_le_bytes());
        }
        Command::Forget(id) => {
            put(CMD_FORGET);
            session(&mut b, *id);
        }
        Command::Shutdown => put(CMD_SHUTDOWN),
    }
    let length = b.len() - LENGTH_PREFIX_BYTES;
    b[..LENGTH_PREFIX_BYTES].copy_from_slice(&(length as u32).to_le_bytes());
    Ok(b)
}
fn key(b: &mut Vec<u8>, key: DeviceKey) {
    b.extend_from_slice(&key.id.0.to_le_bytes());
    b.extend_from_slice(&key.generation.to_le_bytes());
}
fn session(b: &mut Vec<u8>, id: SessionId) {
    b.extend_from_slice(&id.epoch.to_le_bytes());
    b.extend_from_slice(&id.serial.to_le_bytes());
}
struct Reader<'a>(&'a [u8]);
impl Reader<'_> {
    fn take<const N: usize>(&mut self) -> Result<[u8; N]> {
        let v = self
            .0
            .get(..N)
            .ok_or(Error::Invalid)?
            .try_into()
            .map_err(|_| Error::Invalid)?;
        self.0 = &self.0[N..];
        Ok(v)
    }
    fn u64(&mut self) -> Result<u64> {
        Ok(u64::from_le_bytes(self.take()?))
    }
    fn key(&mut self) -> Result<DeviceKey> {
        Ok(DeviceKey {
            id: DeviceId(self.u64()?),
            generation: self.u64()?,
        })
    }
    fn session(&mut self) -> Result<SessionId> {
        Ok(SessionId {
            epoch: self.u64()?,
            serial: self.u64()?,
        })
    }
}
/// Decode exactly one complete frame. Rejects trailing bytes, unknown tags,
/// noncanonical booleans and impossible counts before allocating a device list.
pub fn decode_request(frame: &[u8]) -> Result<Request> {
    if frame.len() > MAX_FRAME {
        return Err(Error::Exhausted);
    }
    let mut r = Reader(frame);
    let length = u32::from_le_bytes(r.take()?) as usize;
    if length != r.0.len() {
        return Err(Error::Invalid);
    }
    let version = u16::from_le_bytes(r.take()?);
    let id = r.u64()?;
    let deadline = r.u64()?;
    let command = match r.take::<1>()?[0] {
        CMD_LIST => Command::List,
        CMD_AUTHORIZE => {
            let device = r.key()?;
            let peer = PeerId(r.take()?);
            let allow = r.boolean()?;
            Command::Authorize {
                device,
                peer,
                allow,
            }
        }
        CMD_ATTACH => {
            let count = u16::from_le_bytes(r.take()?) as usize;
            if count == 0 || count > MAX_ITEMS || r.0.len() != count * DEVICE_KEY_BYTES {
                return Err(Error::Invalid);
            }
            let mut keys = Vec::with_capacity(count);
            for _ in 0..count {
                keys.push(r.key()?);
            }
            Command::Attach(keys)
        }
        CMD_DETACH => Command::Detach(r.session()?),
        CMD_STATUS => Command::Status(r.session()?),
        CMD_HEARTBEAT => Command::Heartbeat(r.session()?),
        CMD_SUBSCRIBE => Command::Subscribe,
        CMD_POLL => Command::Poll(r.u64()?),
        CMD_UNSUBSCRIBE => Command::Unsubscribe(r.u64()?),
        CMD_FORGET => Command::Forget(r.session()?),
        CMD_SHUTDOWN => Command::Shutdown,
        _ => return Err(Error::Invalid),
    };
    if !r.0.is_empty() || id == 0 {
        return Err(Error::Invalid);
    }
    Ok(Request {
        version,
        id,
        deadline,
        command,
    })
}

/// Encode a bounded response. Events and metadata use the control channel;
/// transfer data has no representation here.
pub fn encode_response(response: &Response) -> Result<Vec<u8>> {
    let mut b = vec![0; LENGTH_PREFIX_BYTES];
    b.extend_from_slice(&response.id.to_le_bytes());
    match &response.result {
        Err(error) => b.extend_from_slice(&(*error as u16).to_le_bytes()),
        Ok(reply) => {
            b.extend_from_slice(&STATUS_OK.to_le_bytes());
            match reply {
                Reply::Done => b.push(REPLY_DONE),
                Reply::Devices(devices) => {
                    if devices.len() > MAX_ITEMS {
                        return Err(Error::Exhausted);
                    }
                    b.push(REPLY_DEVICES);
                    b.extend_from_slice(&(devices.len() as u16).to_le_bytes());
                    for d in devices {
                        key(&mut b, d.key);
                        b.push(u8::from(d.busy));
                    }
                }
                Reply::Session(s) => {
                    if s.devices.len() > MAX_ITEMS {
                        return Err(Error::Exhausted);
                    }
                    b.push(REPLY_SESSION);
                    session(&mut b, s.id);
                    b.extend_from_slice(&s.peer.0);
                    b.push(match s.state {
                        SessionState::Acquiring => STATE_ACQUIRING,
                        SessionState::Active => STATE_ACTIVE,
                        SessionState::Draining => STATE_DRAINING,
                        SessionState::Quarantined => STATE_QUARANTINED,
                        SessionState::Closed => STATE_CLOSED,
                    });
                    b.extend_from_slice(&s.error.map_or(STATUS_OK, |e| e as u16).to_le_bytes());
                    b.extend_from_slice(&(s.devices.len() as u16).to_le_bytes());
                    for d in &s.devices {
                        key(&mut b, *d);
                    }
                }
                Reply::Subscription(id) => {
                    b.push(REPLY_SUBSCRIPTION);
                    b.extend_from_slice(&id.to_le_bytes());
                }
                Reply::Event(event) => {
                    b.push(REPLY_EVENT);
                    b.push(u8::from(event.is_some()));
                    if let Some(e) = event {
                        b.extend_from_slice(&e.sequence.to_le_bytes());
                        b.push(match e.kind {
                            EventKind::Inventory => EVENT_INVENTORY,
                            EventKind::Session => EVENT_SESSION,
                            EventKind::Resync => EVENT_RESYNC,
                        });
                        b.push(u8::from(e.session.is_some()));
                        if let Some(id) = e.session {
                            session(&mut b, id);
                        }
                    }
                }
            }
        }
    }
    let length = b.len() - LENGTH_PREFIX_BYTES;
    b[..LENGTH_PREFIX_BYTES].copy_from_slice(&(length as u32).to_le_bytes());
    Ok(b)
}
impl Reader<'_> {
    fn boolean(&mut self) -> Result<bool> {
        match self.take::<1>()?[0] {
            0 => Ok(false),
            1 => Ok(true),
            _ => Err(Error::Invalid),
        }
    }
    fn count(&mut self, width: usize) -> Result<usize> {
        let count = u16::from_le_bytes(self.take()?) as usize;
        if count > MAX_ITEMS || self.0.len() < count * width {
            return Err(Error::Invalid);
        }
        Ok(count)
    }
    fn error(&mut self) -> Result<Option<Error>> {
        Ok(match u16::from_le_bytes(self.take()?) {
            STATUS_OK => None,
            STATUS_INVALID => Some(Error::Invalid),
            STATUS_STATE => Some(Error::State),
            STATUS_UNSUPPORTED => Some(Error::Unsupported),
            STATUS_UNAVAILABLE => Some(Error::Unavailable),
            STATUS_DENIED => Some(Error::Denied),
            STATUS_NOT_FOUND => Some(Error::NotFound),
            STATUS_VERSION => Some(Error::Version),
            STATUS_DEADLINE => Some(Error::Deadline),
            STATUS_CANCELLED => Some(Error::Cancelled),
            STATUS_EXHAUSTED => Some(Error::Exhausted),
            STATUS_INTERNAL => Some(Error::Internal),
            STATUS_CLEANUP => Some(Error::Cleanup),
            _ => return Err(Error::Invalid),
        })
    }
}
/// Decode exactly one bounded response, preserving all 64-bit identifiers.
pub fn decode_response(frame: &[u8]) -> Result<Response> {
    if frame.len() > MAX_FRAME {
        return Err(Error::Exhausted);
    }
    let mut r = Reader(frame);
    let length = u32::from_le_bytes(r.take()?) as usize;
    if length != r.0.len() {
        return Err(Error::Invalid);
    }
    let id = r.u64()?;
    if id == 0 {
        return Err(Error::Invalid);
    }
    let result = if let Some(error) = r.error()? {
        Err(error)
    } else {
        Ok(match r.take::<1>()?[0] {
            REPLY_DONE => Reply::Done,
            REPLY_DEVICES => {
                let count = r.count(DEVICE_INFO_BYTES)?;
                let mut devices = Vec::with_capacity(count);
                for _ in 0..count {
                    devices.push(DeviceInfo {
                        key: r.key()?,
                        busy: r.boolean()?,
                    });
                }
                Reply::Devices(devices)
            }
            REPLY_SESSION => {
                let id = r.session()?;
                let peer = PeerId(r.take()?);
                let state = match r.take::<1>()?[0] {
                    STATE_ACQUIRING => SessionState::Acquiring,
                    STATE_ACTIVE => SessionState::Active,
                    STATE_DRAINING => SessionState::Draining,
                    STATE_QUARANTINED => SessionState::Quarantined,
                    STATE_CLOSED => SessionState::Closed,
                    _ => return Err(Error::Invalid),
                };
                let error = r.error()?;
                let count = r.count(DEVICE_KEY_BYTES)?;
                let mut devices = Vec::with_capacity(count);
                for _ in 0..count {
                    devices.push(r.key()?);
                }
                Reply::Session(Session {
                    id,
                    peer,
                    state,
                    error,
                    devices,
                })
            }
            REPLY_SUBSCRIPTION => Reply::Subscription(r.u64()?),
            REPLY_EVENT => {
                let event = if r.boolean()? {
                    let sequence = r.u64()?;
                    let kind = match r.take::<1>()?[0] {
                        EVENT_INVENTORY => EventKind::Inventory,
                        EVENT_SESSION => EventKind::Session,
                        EVENT_RESYNC => EventKind::Resync,
                        _ => return Err(Error::Invalid),
                    };
                    let session = if r.boolean()? {
                        Some(r.session()?)
                    } else {
                        None
                    };
                    Some(Event {
                        sequence,
                        kind,
                        session,
                    })
                } else {
                    None
                };
                Reply::Event(event)
            }
            _ => return Err(Error::Invalid),
        })
    };
    if !r.0.is_empty() {
        return Err(Error::Invalid);
    }
    Ok(Response { id, result })
}
