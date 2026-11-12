use crate::{Caller, DeviceKey, Error, PeerId, Result, Session, SessionId};

/// Major version of the mock-host control contract (independent of USB/IP).
pub const CONTROL_VERSION: u16 = 1;
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
    /// Correlation ID (represented as a full u64 in the M6 typed IPC harness).
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
