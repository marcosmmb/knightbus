//! Versioned host administration. Identifiers use decimal strings across JSON so
//! web clients cannot round 64-bit physical generations or session identifiers.
use serde::{Deserialize, Serialize};

/// Maximum number of local physical devices in a host inventory.
pub const MAX_PHYSICAL_DEVICES: usize = 128;
/// Maximum UTF-8 bytes in an OS product name, keeping inventory frames bounded.
pub const MAX_DEVICE_NAME_BYTES: usize = 128;
/// Keep computer discovery names within a single DNS TXT property.
pub const MAX_COMPUTER_NAME_BYTES: usize = 192;
/// Bound untrusted discovery results and the resulting control frame.
pub const MAX_NEARBY_COMPUTERS: usize = 64;
/// Length of an explicitly opened, short-lived pairing PIN.
pub const PAIRING_PIN_DIGITS: usize = 6;

/// Pairing secret. Debug output must never disclose its value.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct PairingPin(pub String);
impl std::fmt::Debug for PairingPin {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("PairingPin([redacted])")
    }
}

/// Local administrator setup request. Pairing never grants device access.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "operation", rename_all = "snake_case", deny_unknown_fields)]
pub enum Request {
    /// Identity, paired peers and current backend state.
    Status,
    /// Enumerate physical devices without capture.
    Physical,
    /// Inspect one physical generation without capture.
    Inspect {
        /// Decimal OS generation.
        registry: String,
    },
    /// Explicitly enroll a certificate after comparing its full fingerprint.
    Pair {
        /// Public DER certificate encoded as hex.
        certificate: String,
        /// Full confirmed fingerprint.
        fingerprint: String,
    },
    /// Revoke persistent trust; running transports drain this peer.
    Revoke {
        /// Full fingerprint.
        fingerprint: String,
    },
    /// Select an exporter device. Does not grant it to any peer.
    Export {
        /// Decimal OS generation.
        registry: String,
        /// USB listener address.
        usb: String,
        /// Control listener address.
        control: String,
        /// Deliberate capture consent.
        capture: bool,
        /// All storage volumes were cleanly unmounted.
        storage_unmounted: bool,
        /// Advertise only these listener addresses as untrusted LAN hints.
        #[serde(default)]
        announce: bool,
    },
    /// Select a paired exporter. Does not attach anything.
    Import {
        /// Paired exporter fingerprint.
        fingerprint: String,
        /// USB destination.
        usb: String,
        /// Control destination.
        control: String,
    },
    /// Retire a selected runtime, after all sessions are closed.
    Clear,
    /// Bounded untrusted address hints; never enrolls a peer.
    Browse,
    /// Publish computer presence independently of USB backend selection.
    Advertise {
        /// Remove the announcement when false.
        enabled: bool,
        /// Untrusted display name, never an authorization identity.
        name: String,
        /// Configured USB address; no listener or capture is started.
        usb: String,
        /// Configured management address; no listener is started.
        control: String,
    },
    /// Discover untrusted computer identities, including idle importers.
    Discover,
    /// Open a one-time PIN pairing window. Never selects or shares USB devices.
    PairingStart {
        /// This computer's display name.
        name: String,
        /// This computer's configured USB address.
        usb: String,
        /// This computer's configured control address.
        control: String,
    },
    /// Authenticate a remote pairing window using its displayed PIN.
    PairingJoin {
        /// Remote ephemeral pairing listener, discovered or entered explicitly.
        address: String,
        /// Short-lived secret read from the other computer.
        pin: PairingPin,
        /// This computer's display name.
        name: String,
        /// This computer's configured USB address.
        usb: String,
        /// This computer's configured control address.
        control: String,
    },
    /// Read progress without disclosing the PIN.
    PairingStatus,
    /// Stop the local pairing attempt and close its listener.
    PairingCancel,
}
/// Unverified LAN presence; pairing still requires independent fingerprint checking.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NearbyComputer {
    /// Display name supplied by the remote computer.
    pub name: String,
    /// Claimed fingerprint; not evidence that its sender owns the private key.
    pub fingerprint: String,
    /// Configured USB endpoint, which may not be listening yet.
    pub usb: String,
    /// Configured management endpoint, which may not be listening yet.
    pub control: String,
    /// Temporary pairing listener, present only during an explicit pairing window.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pairing_port: Option<u16>,
}

/// PIN returned only to the local administrator who opened the pairing window.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PairingOffer {
    /// Secret to read on the other computer; never advertised or persisted.
    pub pin: PairingPin,
    /// Pairing listener address for manual entry when discovery is unavailable.
    pub address: String,
    /// Remaining lifetime in seconds.
    pub expires_in: u64,
}
/// Local pairing progress. Success enrolls identities, never device permissions.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case", deny_unknown_fields)]
pub enum PairingProgress {
    /// No active attempt.
    Idle,
    /// An explicit pairing window is waiting for the other computer.
    Waiting {
        /// Remaining lifetime in seconds.
        expires_in: u64,
        /// Remaining online attempts before this PIN is invalidated.
        attempts_left: usize,
    },
    /// A PIN exchange is in progress.
    Connecting,
    /// Both peers confirmed enrollment in this exchange.
    Paired {
        /// Metadata authenticated by the PIN exchange.
        computer: NearbyComputer,
    },
    /// An exchange failed; it may have been interrupted after one side enrolled.
    Failed {
        /// Stable error, never raw protocol data or PINs.
        error: String,
    },
}
/// Physical metadata. Descriptor inspection and capture can fail independently.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Physical {
    /// Stable device selector, distinct from a capture generation.
    #[serde(default)]
    pub id: String,
    /// Exact OS incarnation, never VID/PID authorization.
    pub registry: String,
    /// Display vendor ID.
    pub vendor: u16,
    /// Display product ID.
    pub product: u16,
    /// Optional OS product name for display only, never authorization.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
}
/// Read-only Windows driver-store preflight; never creates a virtual device.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DriverStatus {
    /// No compatible package is staged.
    Missing,
    /// A compatible package is staged; ABI and capture are checked at attachment.
    Staged,
    /// The OS could not inspect its driver store.
    Unavailable,
}
/// Host metadata. Capability strings describe the installed build, not a USB claim.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Status {
    /// Public identity fingerprint.
    pub fingerprint: String,
    /// Public certificate; safe to exchange, unlike identity.bin.
    pub certificate: String,
    /// Persistently paired peers; pairing alone grants nothing.
    pub peers: Vec<String>,
    /// Native exporter implementation present.
    pub exporter: bool,
    /// Native importer implementation present.
    pub importer: bool,
    /// Whether this service can advertise and browse LAN addresses through mDNS.
    #[serde(default)]
    pub discovery: bool,
    /// Whether short-lived PIN pairing is supported by the service.
    #[serde(default)]
    pub pin_pairing: bool,
    /// Host process, used to wait for complete service shutdown.
    #[serde(default)]
    pub process_id: u32,
    /// macOS service has the privileges needed for explicit device capture.
    #[serde(default)]
    pub capture_ready: bool,
    /// The desktop helper was started in the app's user session for privacy consent.
    #[serde(default)]
    pub storage_consent_ready: bool,
    /// Driver-store result at service startup, absent outside Windows.
    #[serde(default)]
    pub driver: Option<DriverStatus>,
    /// Selected role, or idle.
    pub role: String,
    /// Selected endpoint/device description.
    pub selection: String,
    /// Explicit native prerequisites and limitations.
    pub guidance: String,
}
/// Bounded host result.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(
    tag = "kind",
    content = "value",
    rename_all = "snake_case",
    deny_unknown_fields
)]
pub enum Reply {
    /// Current service setup.
    Status(Status),
    /// Physical inventory, without capture.
    Physical(Vec<Physical>),
    /// Untrusted pairs of USB/control addresses.
    Hints(Vec<(String, String)>),
    /// Unverified nearby computers. No pairing or access is implied.
    Nearby(Vec<NearbyComputer>),
    /// Newly opened PIN window, sent over authenticated local IPC only.
    PairingOffer(PairingOffer),
    /// Pairing job status; contains no PIN.
    PairingProgress(PairingProgress),
}
