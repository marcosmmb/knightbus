//! Independent USB/IP 1.1.1 codec. No networking, USB access, or session ownership.
//!
//! Replies require original-request context: their on-wire direction is zero.
//! [`decode`] accepts one frame prefix and returns its consumed length. Stream
//! adapters support fragmentation, coalescing and short writes without sockets.
//!
//! Allocation admission is explicit. Hosts with a shared budget retain charges
//! until all decoded/encoded storage is dropped. This small example uses only
//! per-frame limits; KnightBus hosts use the core budget wrapper.
//!
//! ```
//! use knightbus_usbip::{Context, Limits, Message, decode, encode};
//! let bytes = encode(&Message::DevListRequest, Limits::default(), &mut |_| Ok(()))?;
//! assert_eq!(bytes, [1, 17, 128, 5, 0, 0, 0, 0]);
//! let (message, consumed) = decode(&bytes, Context::Management, Limits::default(), &mut |_| Ok(()))?;
//! assert_eq!(message, Message::DevListRequest);
//! assert_eq!(consumed, bytes.len());
//! # Ok::<(), knightbus_usbip::Error>(())
//! ```
mod codec;
mod model;
mod stream;
pub use codec::{decode, encode};
pub use model::*;
pub use stream::{Decoder, read_message, write_message};

/// USB/IP completion status values.
pub mod status;
