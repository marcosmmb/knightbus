use crate::{Completion, Device, DeviceKey, Result, SessionId, Transfer, TransferId};
use std::task::Poll;

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
/// Virtual USB importer backend contract. Production driver implementations are
/// later milestones; neither adapter handles USB requests or native pointers.
/// Drop has the same native-storage retirement obligation as ExporterBackend.
pub trait ImporterBackend {
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
    fn next_cancellation(&mut self, _session: SessionId) -> Option<u32> {
        None
    }
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
/// Data forwarding uses [`ImporterBackend`] directly in later backend milestones.
pub struct Importer<B, C> {
    backend: B,
    clock: C,
    state: ImporterState,
    attachment: Option<(SessionId, Device, crate::resource::Credit)>,
    budget: crate::Budget,
    deadline: u64,
    cleanup_ms: u64,
    error: Option<crate::Error>,
}
impl<B: ImporterBackend, C: Clock> Importer<B, C> {
    /// Construct an idle importer; cleanup deadlines never imply native retirement.
    pub fn new(backend: B, clock: C, cleanup_ms: u64, budget: crate::Budget) -> Result<Self> {
        if cleanup_ms == 0 {
            return Err(crate::Error::Invalid);
        }
        Ok(Self {
            backend,
            clock,
            state: ImporterState::Idle,
            attachment: None,
            budget,
            deadline: 0,
            cleanup_ms,
            error: None,
        })
    }
    /// Begin native attachment after authenticated remote import authorization.
    pub fn attach(&mut self, session: SessionId, device: Device, deadline: u64) -> Result<()> {
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
    /// Current native lifecycle, independent of exporter lease lifetime.
    pub fn state(&self) -> ImporterState {
        self.state
    }
    /// Sanitized attachment/cleanup diagnostic.
    pub fn error(&self) -> Option<crate::Error> {
        self.error
    }
}
