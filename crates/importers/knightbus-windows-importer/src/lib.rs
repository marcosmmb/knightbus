//! Windows importer. USB/IP and policy stay in userspace; the original UDE
//! driver owns native requests. Portable validation is also tested off Windows.
#![deny(unsafe_op_in_unsafe_fn)]
pub mod backend;
pub mod channel;
mod notification;
pub use knightbus_windows_protocol::descriptors;
pub use notification::Notification as DriverWake;

#[cfg(windows)]
pub mod native;
