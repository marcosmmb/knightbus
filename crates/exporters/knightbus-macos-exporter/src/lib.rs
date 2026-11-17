//! Physical USB ownership and bounded asynchronous IOUSBHost operations.
//! Implements the physical-device driver contract.
pub mod constants;
pub mod descriptors;
#[cfg(target_os = "macos")]
pub mod native;
#[cfg(target_os = "macos")]
pub mod storage_access;

/// Maximum simultaneous requests, including queued and completed owners.
pub const SLOTS: usize = 64;
/// Maximum bytes in one physical transfer.
pub const MAX_TRANSFER: usize = 1024 * 1024;
/// Maximum ISO transactions per request.
pub const MAX_ISO: usize = 1024;
