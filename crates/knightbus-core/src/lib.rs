//! Host-neutral policy, sessions and bounded transfer ownership.
//!
//! Drive [`Engine::tick`] with an injected monotonic clock. Backend methods are
//! nonblocking polls; no runtime, socket, platform USB API or callback is owned
//! here. Control adapters call this same engine. See the repository's M6 design
//! for trust boundaries and retirement guarantees.
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
