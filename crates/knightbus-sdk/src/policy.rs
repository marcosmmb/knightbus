//! Shared operator timeouts for CLI requests and native importer setup.

/// Default relative deadline for operator control requests.
pub const CONTROL_TIMEOUT_MS: u64 = 5_000;
/// Relative deadline for accepting a native attachment.
pub const ATTACH_TIMEOUT_MS: u64 = 30_000;

/// Ten requests per second leaves room for negotiation under the IPC rate limit.
pub const CONTROL_POLL_INTERVAL: std::time::Duration = std::time::Duration::from_millis(100);
