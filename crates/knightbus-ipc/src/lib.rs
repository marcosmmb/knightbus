//! Initial Service host contract and deterministic bounded IPC harness.
//!
//! Requests cross a strict binary framing boundary; service and embedded adapters
//! delegate to the same control target. No sockets, OS authentication, daemon or
//! C ABI are claimed here: M12 must bind an authenticated OS principal and replace
//! the local link. USB transfers never enter this management protocol.
mod wire;
use knightbus_core::*;
use std::collections::{BTreeMap, BTreeSet};
pub use wire::{decode_request, decode_response, encode_request, encode_response};

/// One authenticated management connection. Host code supplies verified authority
/// and connection epoch, never data decoded from a request. There is no implicit
/// reconnection/replay; a restart requires a newly constructed service connection.
pub struct Service<'a, T: ControlTarget> {
    target: &'a mut T,
    caller: Caller,
    epoch: u64,
    last_id: u64,
    capacity: usize,
    pending: BTreeMap<u64, Request>,
    replies: BTreeMap<u64, Response>,
    subscriptions: BTreeSet<u64>,
}
impl<'a, T: ControlTarget> Service<'a, T> {
    /// Bind trusted OS peer authorization. A rejected peer gets no engine access.
    /// The bounded harness accepts at most 256 outstanding control requests.
    pub fn connect(
        target: &'a mut T,
        caller: Caller,
        access: bool,
        epoch: u64,
        version: u16,
        capacity: usize,
    ) -> Result<Self> {
        if !access {
            return Err(Error::Denied);
        }
        if version != CONTROL_VERSION {
            return Err(Error::Version);
        }
        if epoch == 0 || capacity == 0 || capacity > 256 {
            return Err(Error::Invalid);
        }
        Ok(Self {
            target,
            caller,
            epoch,
            last_id: 0,
            capacity,
            pending: BTreeMap::new(),
            replies: BTreeMap::new(),
            subscriptions: BTreeSet::new(),
        })
    }
    /// Admit one complete length-prefixed request after framing. Malformed frames
    /// are rejected before dispatch. Stream framing/OS credentials belong to M12.
    pub fn enqueue(&mut self, epoch: u64, frame: &[u8]) -> Result<()> {
        if self.epoch == 0 || epoch != self.epoch {
            return Err(Error::State);
        }
        if self.pending.len() + self.replies.len() >= self.capacity {
            return Err(Error::Exhausted);
        }
        let request = decode_request(frame)?;
        if request.id <= self.last_id {
            return Err(Error::Invalid);
        }
        self.last_id = request.id;
        if request.version != CONTROL_VERSION {
            return Err(Error::Version);
        }
        self.pending.insert(request.id, request);
        Ok(())
    }
    /// Cancel only undispatched work using its already reserved queue slot.
    /// Dispatched attach cancellation uses Detach and native retirement semantics.
    pub fn cancel(&mut self, epoch: u64, id: u64) -> Result<()> {
        if self.epoch == 0 || epoch != self.epoch {
            return Err(Error::State);
        }
        if self.pending.remove(&id).is_none() {
            return Err(Error::State);
        }
        self.replies.insert(
            id,
            Response {
                id,
                result: Err(Error::Cancelled),
            },
        );
        Ok(())
    }
    /// Dispatch one request in increasing ID order, preserving management order.
    pub fn dispatch(&mut self) {
        let Some((id, request)) = self.pending.pop_first() else {
            return;
        };
        let owned = match request.command {
            Command::Poll(sub) | Command::Unsubscribe(sub) => self.subscriptions.contains(&sub),
            _ => true,
        };
        let result = if !owned {
            Err(Error::Denied)
        } else {
            self.target
                .execute(self.caller, request.command.clone(), request.deadline)
        };
        if let Ok(Reply::Subscription(sub)) = &result {
            self.subscriptions.insert(*sub);
        }
        if let (Command::Unsubscribe(sub), Ok(_)) = (request.command, &result) {
            self.subscriptions.remove(&sub);
        }
        self.replies.insert(id, Response { id, result });
    }
    /// Take a correlated result; slow consumers occupy a bounded control slot.
    pub fn response(&mut self, epoch: u64, id: u64) -> Result<Option<Response>> {
        if self.epoch == 0 || epoch != self.epoch {
            return Err(Error::State);
        }
        Ok(self.replies.remove(&id))
    }
    /// Retire all connection-local subscriptions and queued commands. Session
    /// leases belong to the engine, and survive a management UI disconnect.
    pub fn close(&mut self) {
        for sub in std::mem::take(&mut self.subscriptions) {
            let _ = self
                .target
                .execute(self.caller, Command::Unsubscribe(sub), u64::MAX);
        }
        self.pending.clear();
        self.replies.clear();
        self.epoch = 0;
    }
}
/// Service adapter over the deterministic framed local link. Production IPC will
/// implement this same control contract after OS peer verification in M12.
pub struct IpcControl<'a, T: ControlTarget> {
    service: Service<'a, T>,
    epoch: u64,
    next_id: u64,
}
impl<'a, T: ControlTarget> IpcControl<'a, T> {
    /// Create a bounded mock service connection with trusted host authority.
    pub fn new(
        target: &'a mut T,
        caller: Caller,
        access: bool,
        epoch: u64,
        version: u16,
    ) -> Result<Self> {
        Ok(Self {
            service: Service::connect(target, caller, access, epoch, version, 16)?,
            epoch,
            next_id: 0,
        })
    }
    /// Explicitly retire subscriptions and queued work before releasing the host.
    pub fn close(&mut self) {
        self.service.close();
    }
}
impl<T: ControlTarget> KnightBusControl for IpcControl<'_, T> {
    fn call(&mut self, command: Command, deadline: u64) -> Result<Reply> {
        self.next_id = self.next_id.checked_add(1).ok_or(Error::Exhausted)?;
        let request = Request {
            version: CONTROL_VERSION,
            id: self.next_id,
            deadline,
            command,
        };
        let frame = encode_request(&request)?;
        self.service.enqueue(self.epoch, &frame)?;
        self.service.dispatch();
        let response = self
            .service
            .response(self.epoch, self.next_id)?
            .ok_or(Error::Internal)?;
        let response = decode_response(&encode_response(&response)?)?;
        if response.id != self.next_id {
            return Err(Error::Internal);
        }
        response.result
    }
}

impl<T: ControlTarget> Drop for Service<'_, T> {
    fn drop(&mut self) {
        self.close();
    }
}
