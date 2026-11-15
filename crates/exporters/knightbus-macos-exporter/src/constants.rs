//! Native resource policy.
/// Resource and timeout limits for native USB operations.
pub mod limits {
    use std::time::Duration;
    /// Bounded configuration descriptor count.
    pub const MAX_CONFIGURATIONS: usize = 16;
    /// Bounded total interface/alternate descriptors.
    pub const MAX_INTERFACE_ALTERNATES: usize = 64;
    /// Bounded enumeration results.
    pub const MAX_ENUMERATED_DEVICES: usize = 256;
    /// Budget allowance for worker bookkeeping.
    pub const WORKER_METADATA_ALLOWANCE: usize = 4096;
    /// Budget multiplier for cloned descriptor inventories.
    pub const NATIVE_METADATA_MULTIPLIER: usize = 4;
    /// Budget allowance for native owner bookkeeping.
    pub const NATIVE_METADATA_ALLOWANCE: usize = 16384;
    /// Budget allowance per native operation.
    pub const TRANSFER_METADATA_ALLOWANCE: usize = 256;
    /// Retry interval only after failed abort/release APIs.
    pub const CLEANUP_RETRY_INTERVAL: Duration = Duration::from_millis(10);
    /// Native control completion timeout.
    pub const CONTROL_TIMEOUT_SECONDS: f64 = 2.0;
    /// No native pipe deadline; cancellation is engine-owned.
    pub const PIPE_TIMEOUT_SECONDS: f64 = 0.0;
    /// Maximum interface enumeration/reclaim wait.
    pub const INTERFACE_CLAIM_TIMEOUT: Duration = Duration::from_secs(2);
    /// Interface reclaim retry interval.
    pub const INTERFACE_CLAIM_RETRY: Duration = Duration::from_millis(5);
    /// Standalone Native drop callback-drain interval.
    pub const DIRECT_DRAIN_RETRY: Duration = Duration::from_millis(1);
}
