//! USB/IP completion codes, independent of the host OS errno values.
/// Successful USB/IP completion.
pub const SUCCESS: i32 = 0;
/// Linux EIO.
pub const IO: i32 = -5;
/// Linux ENOMEM.
pub const NO_MEMORY: i32 = -12;
/// Linux EACCES.
pub const ACCESS: i32 = -13;
/// Linux ENODEV.
pub const NO_DEVICE: i32 = -19;
/// Linux EINVAL.
pub const INVALID: i32 = -22;
/// Linux EPIPE (endpoint stall).
pub const PIPE: i32 = -32;
/// Linux EPROTO.
pub const PROTOCOL: i32 = -71;
/// Linux EOVERFLOW.
pub const OVERFLOW: i32 = -75;
/// Linux EOPNOTSUPP.
pub const UNSUPPORTED: i32 = -95;
/// Linux ETIMEDOUT.
pub const TIMED_OUT: i32 = -110;
/// Linux ECANCELED.
pub const CANCELLED: i32 = -125;
/// Linux ECONNRESET; successful USB/IP unlink cancellation.
pub const CONNECTION_RESET: i32 = -104;
