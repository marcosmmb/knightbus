//! Host-neutral policy, sessions and bounded transfer ownership.
//!
//! Drive [`Engine::tick`] with an injected monotonic clock. Backend methods are
//! nonblocking polls. Device workers own scheduling and retirement; native
//! drivers supply OS operations. No socket, platform FFI or async runtime lives here.
mod backend;
mod control;
mod engine;
mod model;
mod queue;
mod resource;
mod transfer;
pub use backend::*;
pub use control::*;
pub use engine::*;
pub use knightbus_usbip as usbip;
pub use model::*;
pub use resource::*;
pub use transfer::*;

mod wake;
pub use wake::WakeSource;

/// Portable physical-device worker and native contract.
pub mod exporter;

/// Checked USB descriptor parsing and inventory construction.
pub mod descriptors;

/// Allocation-free descriptor types shared with native drivers.
pub use knightbus_usb as topology;

mod descriptor_set;
pub use descriptor_set::DescriptorSet;

/// Virtual-device worker and native operation contract.
pub mod importer;

mod capabilities;
pub use capabilities::Capabilities;

mod signal;
pub use signal::ThreadSignal;

/// Administrator-only host setup contract shared by every management client.
pub mod host;
