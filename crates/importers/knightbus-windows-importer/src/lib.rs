//! Windows importer. USB/IP and policy stay in userspace; the original UDE
//! driver owns native requests. Portable validation is also tested off Windows.
#![deny(unsafe_op_in_unsafe_fn)]
pub mod backend;
pub mod channel;
pub mod descriptors;

#[cfg(windows)]
pub mod native;

pub mod transport;
