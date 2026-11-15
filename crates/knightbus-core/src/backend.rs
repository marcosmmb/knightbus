use crate::{Completion, Device, DeviceKey, Result, SessionId, Transfer, TransferId};
use std::task::{Poll, Waker};

/// Injected monotonic milliseconds. Never use wall clock or peer timestamps.
pub trait Clock {
    /// Current local monotonic time. Regressions cause engine shutdown.
    fn now(&self) -> u64;
}
/// Physical exporter backend contract. Methods never block or call the engine
/// reentrantly. All resources are scoped by session and physical generation.
/// A production backend's Drop must safely retain or retire outstanding native
/// storage on unexpected host destruction; dropping this Rust owner alone is
/// never evidence that native callbacks stopped. No callback may outlive storage.
pub trait ExporterBackend {
    /// Register a host wakeup for lifecycle, removal and completion changes.
    /// Signal existing work on registration and every later progress change.
    /// Wakeups only schedule work; None unregisters. Hosts do not poll for work.
    fn set_waker(&mut self, waker: Option<Waker>);
    /// A backend opting in validates requests against its live configuration and
    /// alternates, serializes topology changes, and retains old pipes until drain.
    /// Otherwise, the engine validates against alternate zero.
    fn manages_topology(&self) -> bool;
    /// Report a terminated physical generation, including idle devices. The
    /// engine revokes its availability and drains any affected leases.
    fn poll_removed(&mut self) -> Option<DeviceKey>;
    /// Poll acquisition, idempotent for the same session/device. Pending owns
    /// provisional resources; release must retire them even if claim never wins.
    fn claim(&mut self, session: SessionId, device: &Device) -> Poll<Result<()>>;
    /// Accept ownership or return it unchanged on rejection. Accept in call order
    /// per endpoint; independent transfers may all be outstanding concurrently.
    #[allow(clippy::result_large_err)] // Return ownership without allocating on admission failure.
    fn submit(
        &mut self,
        id: TransferId,
        transfer: Transfer,
    ) -> std::result::Result<(), (crate::Error, Transfer)>;
    /// Idempotent cancellation intent using existing operation storage. Must not
    /// allocate data credits or claim retirement until a completion is returned.
    fn cancel(&mut self, id: TransferId);
    /// Return one retired operation. No native access may remain to its buffer.
    fn poll_completion(&mut self) -> Option<(TransferId, Completion)>;
    /// Poll release of claims/provisional claims after all transfers retire.
    /// Ready(Err) retains the claim in quarantine and must be retryable.
    fn release(&mut self, session: SessionId, device: DeviceKey) -> Poll<Result<()>>;
}
/// Virtual USB attachment and request ownership.
/// Drop has the same native-storage retirement obligation as ExporterBackend.
pub trait ImporterBackend {
    /// Register a host wakeup for lifecycle, requests and cancellation changes.
    /// Signal existing work on registration and every later progress change.
    /// Wakeups only schedule work; None unregisters. Hosts do not poll for work.
    fn set_waker(&mut self, waker: Option<Waker>);
    /// Attach validated descriptors with a generation-scoped native identity.
    fn attach(&mut self, session: SessionId, device: &Device) -> Poll<Result<()>>;
    /// Deliver a bounded owned native request from this generation.
    fn next_transfer(
        &mut self,
        session: SessionId,
        budget: &crate::Budget,
    ) -> Option<Result<Transfer>>;
    /// Native cancellation of an already delivered request. The transport must
    /// unlink that sequence and keep its transfer owner until remote retirement.
    fn next_cancellation(&mut self, session: SessionId) -> Option<u32>;
    /// Consume a validated completion, retaining its credits until driver access
    /// retires. Failure returns ownership so the session can drain safely.
    #[allow(clippy::result_large_err)] // Preserve ownership without allocating on failure.
    fn complete(
        &mut self,
        session: SessionId,
        completion: Completion,
    ) -> std::result::Result<(), Completion>;
    /// Cancel all native requests and retire the virtual attachment.
    fn detach(&mut self, session: SessionId) -> Poll<Result<()>>;
}
/// Importer lifecycle tracker; native detach confirmation gates reuse.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ImporterState {
    /// No attachment.
    Idle,
    /// Authorization/import and native attach pending.
    Attaching,
    /// Native device attached.
    Active,
    /// Native removal/retirement pending.
    Draining,
    /// Retirement deadline exceeded; resources still held.
    Quarantined,
}
impl ImporterState {
    /// Begin a new native import, only after the previous generation retired.
    pub fn begin(&mut self) -> Result<()> {
        if *self != Self::Idle {
            return Err(crate::Error::State);
        }
        *self = Self::Attaching;
        Ok(())
    }
    /// Confirm native attachment; a completion after cancellation cannot revive it.
    pub fn attached(&mut self) -> Result<()> {
        if *self != Self::Attaching {
            return Err(crate::Error::State);
        }
        *self = Self::Active;
        Ok(())
    }
    /// Disconnect/removal/cancellation all converge on idempotent drain.
    pub fn disconnect(&mut self) {
        if matches!(self, Self::Attaching | Self::Active) {
            *self = Self::Draining;
        }
    }
    /// Mark an expired cleanup budget without pretending resources were freed.
    pub fn timeout(&mut self) {
        if *self == Self::Draining {
            *self = Self::Quarantined;
        }
    }
    /// Only a backend retirement acknowledgement permits a new attachment.
    pub fn retired(&mut self) -> Result<()> {
        if !matches!(self, Self::Draining | Self::Quarantined) {
            return Err(crate::Error::State);
        }
        *self = Self::Idle;
        Ok(())
    }
}

/// Portable importer attachment lifecycle. The transport supplies authorized
/// remote metadata and a fresh session ID; the native driver is injected.
/// Forwarding is gated by the same attachment state and admission budget.
pub struct Importer<B: ImporterBackend, C: Clock> {
    backend: B,
    clock: C,
    state: ImporterState,
    attachment: Option<(SessionId, Device, crate::resource::Credit)>,
    budget: crate::Budget,
    deadline: u64,
    cleanup_ms: u64,
    error: Option<crate::Error>,
    last_now: u64,
    clock_failed: bool,
}
impl<B: ImporterBackend, C: Clock> Importer<B, C> {
    /// Construct an idle importer; cleanup deadlines never imply native retirement.
    pub fn new(backend: B, clock: C, cleanup_ms: u64, budget: crate::Budget) -> Result<Self> {
        if cleanup_ms == 0 {
            return Err(crate::Error::Invalid);
        }
        let last_now = clock.now();
        Ok(Self {
            backend,
            clock,
            state: ImporterState::Idle,
            attachment: None,
            budget,
            deadline: 0,
            cleanup_ms,
            error: None,
            last_now,
            clock_failed: false,
        })
    }
    /// Begin native attachment after authenticated remote import authorization.
    pub fn attach(&mut self, session: SessionId, device: Device, deadline: u64) -> Result<()> {
        self.observe_clock();
        if self.clock_failed {
            return Err(crate::Error::State);
        }
        device.validate()?;
        if session.epoch == 0 || session.serial == 0 {
            return Err(crate::Error::Invalid);
        }
        if self.clock.now() >= deadline {
            return Err(crate::Error::Deadline);
        }
        let bytes = device
            .storage_bytes()?
            .checked_add(std::mem::size_of::<Device>())
            .ok_or(crate::Error::Exhausted)?;
        let credit = self.budget.reserve(bytes, 0)?;
        self.state.begin()?;
        self.attachment = Some((session, device, credit));
        self.deadline = deadline;
        self.error = None;
        Ok(())
    }
    /// Idempotent disconnect. Native detach also retires a partial attachment.
    pub fn disconnect(&mut self) {
        if matches!(self.state, ImporterState::Attaching | ImporterState::Active) {
            self.state.disconnect();
            self.deadline = self.clock.now().saturating_add(self.cleanup_ms);
        }
    }
    /// Advance nonblocking native attachment/removal, retaining quarantined state
    /// on cleanup failure and retrying until the backend confirms retirement.
    pub fn tick(&mut self) {
        self.observe_clock();
        let Some((session, device, _credit)) = &self.attachment else {
            return;
        };
        if self.state == ImporterState::Attaching {
            if self.clock.now() >= self.deadline {
                self.error = Some(crate::Error::Deadline);
                self.disconnect();
            } else {
                match self.backend.attach(*session, device) {
                    Poll::Pending => {}
                    Poll::Ready(Ok(())) => {
                        let _ = self.state.attached();
                    }
                    Poll::Ready(Err(error)) => {
                        self.error = Some(error);
                        self.disconnect();
                    }
                }
            }
        }
        if matches!(
            self.state,
            ImporterState::Draining | ImporterState::Quarantined
        ) {
            if self.clock.now() >= self.deadline {
                self.state.timeout();
                self.error = Some(crate::Error::Cleanup);
            }
            let session = self.attachment.as_ref().unwrap().0;
            match self.backend.detach(session) {
                Poll::Pending => {}
                Poll::Ready(Err(_)) => {
                    self.state.timeout();
                    self.error = Some(crate::Error::Cleanup);
                }
                Poll::Ready(Ok(())) => {
                    let _ = self.state.retired();
                    self.attachment = None;
                }
            }
        }
    }
    /// Register the host scheduler for all native progress.
    pub fn set_waker(&mut self, waker: Option<Waker>) {
        self.backend.set_waker(waker);
    }
    /// Fetch work only from the live attachment and its admission budget.
    pub fn next_transfer(&mut self) -> Option<Result<Transfer>> {
        if self.state != ImporterState::Active {
            return None;
        }
        let result = self
            .backend
            .next_transfer(self.attachment.as_ref()?.0, &self.budget)?;
        Some(result.and_then(|transfer| {
            if transfer.budget_matches(&self.budget) {
                Ok(transfer)
            } else {
                self.disconnect();
                Err(crate::Error::Invalid)
            }
        }))
    }
    /// Cancellation intent for the live attachment.
    pub fn next_cancellation(&mut self) -> Option<u32> {
        if self.state != ImporterState::Active {
            return None;
        }
        self.backend.next_cancellation(self.attachment.as_ref()?.0)
    }
    /// Return a completion to the native owner while its attachment is active.
    #[allow(clippy::result_large_err)]
    pub fn complete(&mut self, completion: Completion) -> std::result::Result<(), Completion> {
        if self.state != ImporterState::Active || !completion.transfer.budget_matches(&self.budget)
        {
            return Err(completion);
        }
        let Some((session, _, _)) = &self.attachment else {
            return Err(completion);
        };
        self.backend.complete(*session, completion)
    }
    /// Validated inventory retained until native retirement.
    pub fn device(&self) -> Option<&Device> {
        self.attachment.as_ref().map(|(_, device, _)| device)
    }
    /// Current native lifecycle, independent of exporter lease lifetime.
    pub fn state(&self) -> ImporterState {
        self.state
    }
    /// Sanitized attachment/cleanup diagnostic.
    pub fn error(&self) -> Option<crate::Error> {
        self.error
    }
    fn observe_clock(&mut self) {
        let now = self.clock.now();
        if now < self.last_now {
            self.clock_failed = true;
            self.error = Some(crate::Error::State);
            self.disconnect();
        }
        self.last_now = now;
    }
}

impl<B: ImporterBackend, C: Clock> Drop for Importer<B, C> {
    fn drop(&mut self) {
        self.backend.set_waker(None);
        if let Some((session, _, _)) = &self.attachment {
            let _ = self.backend.detach(*session);
        }
    }
}
