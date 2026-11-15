use crate::{Caller, DeviceKey, Error, PeerId, Result, Session, SessionId};

/// Major version of the control contract (independent of USB/IP).
pub const CONTROL_VERSION: u16 = 1;
/// Maximum peers granted one device; fits a complete control response.
pub const MAX_DEVICE_GRANTS: usize = 255;
/// Host-neutral management request. Contains no USB payloads or credentials.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Command {
    /// Authorized device snapshot.
    List,
    /// Exporter administrator changes a peer grant; revoke drains affected leases.
    Authorize {
        /// Physical identity being granted.
        device: DeviceKey,
        /// Authenticated peer identity being granted access.
        peer: PeerId,
        /// Enable or revoke the grant.
        allow: bool,
    },
    /// Exporter administrator inspects the explicit grants for a device.
    Permissions(DeviceKey),
    /// Transactional exclusive acquisition, completed by engine ticks/events.
    Attach(Vec<DeviceKey>),
    /// Idempotent drain, also cancels a pending acquisition.
    Detach(SessionId),
    /// Inspect an owned session, including failed cleanup.
    Status(SessionId),
    /// Renew an active lease using the exporter's monotonic clock.
    Heartbeat(SessionId),
    /// Bounded event subscription; overflow requires a new snapshot.
    Subscribe,
    /// Poll one event outside engine/backend mutation.
    Poll(u64),
    /// Retire the event subscription. No callbacks are retained.
    Unsubscribe(u64),
    /// Reclaim a closed session record; stale tokens never become valid again.
    Forget(SessionId),
    /// Exporter administrator freezes admission and drains all sessions.
    Shutdown,
}
/// Filtered discovery entry; stable IDs are selectors, not credentials.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DeviceInfo {
    /// Current physical generation.
    pub key: DeviceKey,
    /// Whether an exclusive lease currently owns this device.
    pub busy: bool,
}
/// Control result, identical in direct and service hosts.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Reply {
    /// Mutation accepted; inspect lifecycle/events for retirement.
    Done,
    /// Filtered device snapshot.
    Devices(Vec<DeviceInfo>),
    /// Identities explicitly granted access to the requested device.
    Peers(Vec<PeerId>),
    /// Authorized session snapshot.
    Session(Session),
    /// Subscription ID scoped to the current host epoch/connection.
    Subscription(u64),
    /// Next event, if any.
    Event(Option<Event>),
}
/// Fixed-size event kinds. No raw USB data or unbounded diagnostic strings.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EventKind {
    /// Inventory or grant changed; refresh the authorized snapshot.
    Inventory,
    /// Session lifecycle changed.
    Session,
    /// A slow subscriber lost events; refresh snapshots before continuing.
    Resync,
}
/// Bounded optional control notification, separate from the USB/IP data stream.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Event {
    /// Monotonic subscription-local sequence; gaps require snapshot reconciliation.
    pub sequence: u64,
    /// Event meaning.
    pub kind: EventKind,
    /// Session when relevant. Inventory events intentionally disclose no devices.
    pub session: Option<SessionId>,
}
/// Both hosts implement this contract. Caller authority is bound on construction,
/// outside request arguments. Deadlines use the injected exporter's clock.
pub trait KnightBusControl {
    /// Execute a management request. Attach/detach return before native retirement.
    fn call(&mut self, command: Command, deadline: u64) -> Result<Reply>;
}
/// Injectable host-neutral engine endpoint used by thin adapters.
pub trait ControlTarget {
    /// Current monotonic host time.
    fn now(&self) -> u64;
    /// Execute using trusted host-supplied authority, never request-supplied flags.
    fn execute(&mut self, caller: Caller, command: Command, deadline: u64) -> Result<Reply>;
}
/// Control request envelope; request IDs strictly increase per authenticated link.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Request {
    /// Control protocol version.
    pub version: u16,
    /// Connection-local correlation ID.
    pub id: u64,
    /// Absolute local deadline; the receiving host owns this clock.
    pub deadline: u64,
    /// Management operation.
    pub command: Command,
}
/// Correlated result; cancellation before dispatch has no engine side effects.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Response {
    /// Original request ID.
    pub id: u64,
    /// Stable operation result.
    pub result: std::result::Result<Reply, Error>,
}

/// A control connection bound to trusted caller authority. Owns subscriptions,
/// checks their scope, and releases them on close without ending device leases.
pub struct ControlConnection<'a, T: ControlTarget> {
    target: &'a mut T,
    caller: Caller,
    subscriptions: std::collections::BTreeSet<u64>,
}
impl<'a, T: ControlTarget> ControlConnection<'a, T> {
    /// Check the protocol version before exposing engine operations.
    pub fn new(target: &'a mut T, caller: Caller, version: u16) -> Result<Self> {
        if version != CONTROL_VERSION {
            return Err(Error::Version);
        }
        Ok(Self {
            target,
            caller,
            subscriptions: Default::default(),
        })
    }
    /// Release connection-local subscriptions. Device leases remain engine-owned.
    pub fn close(&mut self) {
        for id in std::mem::take(&mut self.subscriptions) {
            let _ = self
                .target
                .execute(self.caller, Command::Unsubscribe(id), u64::MAX);
        }
    }
}
impl<T: ControlTarget> KnightBusControl for ControlConnection<'_, T> {
    fn call(&mut self, command: Command, deadline: u64) -> Result<Reply> {
        let unsubscribe = match command {
            Command::Poll(id) | Command::Unsubscribe(id) => {
                if !self.subscriptions.contains(&id) {
                    return Err(Error::Denied);
                }
                if matches!(command, Command::Unsubscribe(_)) {
                    Some(id)
                } else {
                    None
                }
            }
            _ => None,
        };
        let result = self.target.execute(self.caller, command, deadline);
        if let Ok(Reply::Subscription(id)) = &result {
            self.subscriptions.insert(*id);
        }
        if result.is_ok()
            && let Some(id) = unsubscribe
        {
            self.subscriptions.remove(&id);
        }
        result
    }
}
impl<T: ControlTarget> Drop for ControlConnection<'_, T> {
    fn drop(&mut self) {
        self.close();
    }
}
