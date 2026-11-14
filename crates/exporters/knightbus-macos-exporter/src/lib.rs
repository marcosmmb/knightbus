//! Physical USB ownership and bounded asynchronous IOUSBHost operations.
//! The same [`Backend`] implements the shared engine contract for either host.
pub mod backend;
pub mod constants;
pub mod descriptors;
#[cfg(target_os = "macos")]
pub mod journal;
#[cfg(target_os = "macos")]
pub mod native;
pub mod transport;
pub use backend::{Backend, Driver, SubmitError};

/// Maximum simultaneous requests, including queued and completed owners.
pub const SLOTS: usize = 64;
/// Maximum bytes in one physical transfer.
pub const MAX_TRANSFER: usize = 1024 * 1024;
/// Maximum ISO transactions per request.
pub const MAX_ISO: usize = 1024;
