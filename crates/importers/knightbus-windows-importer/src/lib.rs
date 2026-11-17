//! Windows importer using the unmodified Microsoft-signed usbip-win2 UDE package.
//! USB/IP validation, pairing and encrypted transport remain in userspace.
#![deny(unsafe_op_in_unsafe_fn)]
pub mod backend;
mod channel;
#[cfg(any(windows, test))]
mod driver_abi;
mod notification;
pub use notification::Notification as DriverWake;
#[cfg(windows)]
pub mod native;
