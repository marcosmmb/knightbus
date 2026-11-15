use crate::queue::{ReplyPool, ReplyQueue};
use crate::resource::Credit;
use crate::*;
use std::{
    collections::{BTreeMap, BTreeSet, VecDeque},
    task::Poll,
};

/// Runtime-independent limits. Control and cancellation use separate bounded
/// metadata, so exhausted data credits cannot prevent cleanup.
#[derive(Clone, Copy, Debug)]
pub struct Config {
    /// Shared transfer/scratch/event storage and request budget.
    pub resources: ResourceLimits,
    /// Inventory entries, including absent generation tombstones.
    pub devices: usize,
    /// Live and retained terminal sessions.
    pub sessions: usize,
    /// Maximum peer/device grants.
    pub grants: usize,
    /// Devices per transactional attach.
    pub batch: usize,
    /// Concurrent event subscribers.
    pub subscriptions: usize,
    /// Ordinary slots per authenticated peer across connections. Administrators
    /// may additionally use one reserved slot per peer.
    pub subscriptions_per_peer: usize,
    /// Subscription slots and queue memory reserved exclusively for administrators.
    pub admin_subscriptions: usize,
    /// Fixed-size events per subscriber.
    pub events: usize,
    /// Lease heartbeat interval allowance in milliseconds.
    pub lease_ms: u64,
    /// Cleanup deadline in milliseconds, after which resources quarantine.
    pub cleanup_ms: u64,
}
impl Default for Config {
    fn default() -> Self {
        Self {
            resources: ResourceLimits {
                bytes: 16 * 1024 * 1024,
                requests: 256,
            },
            devices: 256,
            sessions: 64,
            grants: 1024,
            batch: 16,
            subscriptions: 32,
            subscriptions_per_peer: 4,
            admin_subscriptions: 1,
            events: 64,
            lease_ms: 20_000,
            cleanup_ms: 5_000,
        }
    }
}
struct Inventory {
    device: Device,
    present: bool,
    _credit: Credit,
}
struct Lease {
    view: Session,
    expires: u64,
    cleanup: u64,
    attempted: usize,
    claimed: usize,
    last_sequence: u32,
    replies: ReplyQueue,
    _credit: Credit,
}
struct Pending {
    deadline: u64,
    cancel: Option<Error>,
    unlink: Option<u32>,
    retirement_deadline: Option<u64>,
    _credit: Credit,
}
struct Subscription {
    caller: Caller,
    events: VecDeque<Event>,
    overflow: bool,
    sequence: u64,
    reserved: bool,
    _credit: Credit,
}
/// Deterministic, single-owner engine. Hosts serialize calls and drive
/// ticks; they must retain the engine/backend while shutdown is still draining.
pub struct Engine<B, C> {
    backend: B,
    clock: C,
    config: Config,
    epoch: u64,
    serial: u64,
    inventory: BTreeMap<DeviceId, Inventory>,
    grants: BTreeMap<(DeviceId, PeerId), Credit>,
    sessions: BTreeMap<SessionId, Lease>,
    pending: BTreeMap<TransferId, Pending>,
    replies: ReplyPool,
    admin_queues: Vec<(VecDeque<Event>, Credit)>,
    _admin_credit: Credit,
    budget: Budget,
    subscribers: BTreeMap<u64, Subscription>,
    subscription: u64,
    stopped: bool,
    last_now: u64,
}
/// UNLINK resolution. Pending is not wire success until native retirement.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum UnlinkState {
    /// Native target is retiring; a later event authorizes RET_UNLINK -104.
    Pending,
    /// Normal completion already committed; RET_UNLINK status is zero.
    TooLate,
}
impl<B: ExporterBackend, C: Clock> Engine<B, C> {
    /// Build a backend against the same aggregate admission budget as the
    /// engine, before any platform handles or asynchronous work are acquired.
    pub fn new(
        clock: C,
        config: Config,
        epoch: u64,
        factory: impl FnOnce(&Budget) -> Result<B>,
    ) -> Result<Self> {
        let budget = Budget::new(config.resources);
        if epoch == 0
            || config.events == 0
            || config.batch == 0
            || config.batch > 256
            || config.devices > 256
            || config.lease_ms == 0
            || config.cleanup_ms == 0
            || config.subscriptions_per_peer == 0
            || config.admin_subscriptions > config.subscriptions
        {
            return Err(Error::Invalid);
        }
        let backend = factory(&budget)?;
        let now = clock.now();
        let replies = ReplyPool::new(&budget, config.resources.requests)?;
        let size = std::mem::size_of::<(VecDeque<Event>, Credit)>();
        let mut admin_credit = budget.reserve(
            config
                .admin_subscriptions
                .checked_mul(size)
                .ok_or(Error::Exhausted)?,
            0,
        )?;
        let mut admin_queues = Vec::new();
        admin_queues
            .try_reserve_exact(config.admin_subscriptions)
            .map_err(|_| Error::Exhausted)?;
        admin_credit.grow((admin_queues.capacity() - config.admin_subscriptions) * size)?;
        for _ in 0..config.admin_subscriptions {
            admin_queues.push(Self::event_queue(&budget, config.events)?);
        }
        Ok(Self {
            backend,
            clock,
            config,
            epoch,
            serial: 0,
            inventory: BTreeMap::new(),
            grants: BTreeMap::new(),
            sessions: BTreeMap::new(),
            pending: BTreeMap::new(),
            budget,
            replies,
            admin_queues,
            _admin_credit: admin_credit,
            subscribers: BTreeMap::new(),
            subscription: 0,
            stopped: false,
            last_now: now,
        })
    }
    /// Shared data admission authority for parsers, native storage and transport.
    pub fn budget(&self) -> Budget {
        self.budget.clone()
    }
    /// Bind a host scheduler to native progress.
    pub fn set_waker(&mut self, waker: Option<std::task::Waker>) {
        self.backend.set_waker(waker)
    }
    /// Register trusted backend inventory after descriptor validation. Replug
    /// requires a greater generation and clears grants (explicit reauthorization).
    pub fn register(&mut self, device: Device) -> Result<()> {
        if self.stopped {
            return Err(Error::State);
        }
        device.validate()?;
        if let Some(old) = self.inventory.get(&device.key.id) {
            if old.present
                || device.key.generation <= old.device.key.generation
                || self.busy(old.device.key)
            {
                return Err(Error::State);
            }
        } else if self.inventory.len() >= self.config.devices {
            return Err(Error::Exhausted);
        }
        if self
            .inventory
            .values()
            .any(|d| d.present && d.device.wire_id() == device.wire_id())
        {
            return Err(Error::Invalid);
        }
        let credit = self.budget.reserve(
            device
                .storage_bytes()?
                .checked_add(std::mem::size_of::<Inventory>())
                .ok_or(Error::Exhausted)?,
            0,
        )?;
        self.grants.retain(|(id, _), _| *id != device.key.id);
        let device_id = device.key.id;
        self.inventory.insert(
            device.key.id,
            Inventory {
                device,
                present: true,
                _credit: credit,
            },
        );
        self.emit_inventory(device_id, None);
        Ok(())
    }
    /// Build the standard USB/IP device list after exporter authorization. The
    /// wire codec remains in `knightbus-usbip`; hidden devices never enter output.
    pub fn discover(&self, caller: Caller) -> Vec<usbip::ListedDevice> {
        self.inventory
            .values()
            .filter(|d| self.allowed(caller, d.device.key))
            .map(|d| {
                let interfaces = d
                    .device
                    .configurations
                    .iter()
                    .filter(|c| c.value == d.device.wire.configuration)
                    .flat_map(|c| &c.interfaces)
                    .filter(|i| i.alternate == 0)
                    .map(|i| i.class.clone())
                    .collect();
                usbip::ListedDevice {
                    device: d.device.wire.clone(),
                    interfaces,
                }
            })
            .collect()
    }
    /// Handle a standard USB/IP IMPORT selector using the same exporter-side
    /// authorization and lease transaction as control attach. Send IMPORT success
    /// only after the returned session becomes Active, never on mere acceptance.
    pub fn import(
        &mut self,
        caller: Caller,
        bus_id: &usbip::WireString<32>,
        deadline: u64,
    ) -> Result<SessionId> {
        if self.clock.now() >= deadline {
            return Err(Error::Deadline);
        }
        let key = self
            .inventory
            .values()
            .find(|d| d.present && &d.device.wire.bus_id == bus_id)
            .map(|d| d.device.key)
            .ok_or(Error::Denied)?;
        let Reply::Session(session) = self.attach(caller, vec![key], deadline)? else {
            unreachable!()
        };
        Ok(session.id)
    }
    /// Physical removal immediately freezes affected sessions, then drains them.
    pub fn remove(&mut self, key: DeviceKey) -> Result<()> {
        let entry = self.inventory.get_mut(&key.id).ok_or(Error::NotFound)?;
        if entry.device.key != key {
            return Err(Error::NotFound);
        }
        entry.present = false;
        let mut cursor = None;
        while let Some(id) = self.next_session(cursor) {
            cursor = Some(id);
            if self.sessions[&id].view.devices.contains(&key) {
                self.drain(id, Error::Unavailable);
            }
        }
        self.emit_inventory(key.id, None);
        Ok(())
    }
    fn allowed(&self, caller: Caller, key: DeviceKey) -> bool {
        self.inventory.get(&key.id).is_some_and(|d| {
            d.present
                && d.device.key == key
                && (caller.administrator || self.grants.contains_key(&(key.id, caller.peer)))
        })
    }
    fn busy(&self, key: DeviceKey) -> bool {
        self.sessions
            .values()
            .any(|l| l.view.state != SessionState::Closed && l.view.devices.contains(&key))
    }
    fn owner(&self, caller: Caller, id: SessionId) -> Result<()> {
        if self
            .sessions
            .get(&id)
            .is_some_and(|l| caller.administrator || l.view.peer == caller.peer)
        {
            Ok(())
        } else {
            Err(Error::Denied)
        }
    }
    fn attach(&mut self, caller: Caller, keys: Vec<DeviceKey>, deadline: u64) -> Result<Reply> {
        if self.stopped {
            return Err(Error::State);
        }
        if keys.is_empty() || keys.len() > self.config.batch {
            return Err(Error::Invalid);
        }
        if keys.iter().any(|key| {
            !self.allowed(
                Caller {
                    peer: caller.peer,
                    administrator: false,
                },
                *key,
            )
        }) {
            return Err(Error::Denied);
        }
        if keys.iter().copied().collect::<BTreeSet<_>>().len() != keys.len() {
            return Err(Error::Invalid);
        }
        if keys.iter().any(|key| self.busy(*key)) {
            return Err(Error::State);
        }
        while self.sessions.len() >= self.config.sessions {
            if !self.reclaim_closed() {
                return Err(Error::Exhausted);
            }
        }
        let bytes = keys
            .len()
            .checked_mul(std::mem::size_of::<DeviceKey>())
            .and_then(|n| n.checked_add(std::mem::size_of::<Lease>()))
            .ok_or(Error::Exhausted)?;
        let credit = loop {
            match self.budget.reserve(bytes, 0) {
                Ok(credit) => break credit,
                Err(Error::Exhausted) if self.reclaim_closed() => continue,
                Err(error) => return Err(error),
            }
        };
        self.serial = self.serial.checked_add(1).ok_or(Error::Exhausted)?;
        let id = SessionId {
            epoch: self.epoch,
            serial: self.serial,
        };
        let view = Session {
            id,
            peer: caller.peer,
            devices: keys,
            state: SessionState::Acquiring,
            error: None,
        };
        self.sessions.insert(
            id,
            Lease {
                view: view.clone(),
                expires: deadline.min(self.clock.now().saturating_add(self.config.lease_ms)),
                cleanup: 0,
                attempted: 0,
                claimed: 0,
                last_sequence: 0,
                replies: ReplyQueue::default(),
                _credit: credit,
            },
        );
        self.emit_session(id);
        Ok(Reply::Session(view))
    }
    fn drain(&mut self, id: SessionId, reason: Error) {
        let l = self.sessions.get_mut(&id).expect("session exists");
        if matches!(
            l.view.state,
            SessionState::Closed | SessionState::Draining | SessionState::Quarantined
        ) {
            return;
        }
        l.view.state = SessionState::Draining;
        l.view.error = Some(reason);
        l.cleanup = self.clock.now().saturating_add(self.config.cleanup_ms);
        // Drop queued data only after native retirement; live backend owners stay.
        while self.replies.pop(&mut l.replies).is_some() {}
        for (token, p) in self.pending.range_mut(Self::transfer_range(id)) {
            if p.cancel.is_none() {
                p.cancel = Some(reason);
            }
            self.backend.cancel(*token);
        }
        self.emit_session(id);
    }
    /// Connection loss affects this generation only. A reconnect must attach anew.
    pub fn disconnect(&mut self, id: SessionId) -> Result<()> {
        if !self.sessions.contains_key(&id) {
            return Err(Error::NotFound);
        }
        self.drain(id, Error::Unavailable);
        Ok(())
    }
    /// Submit concurrently within global credits, preserving backend call order
    /// per endpoint. Sequence numbers must increase strictly across submit/unlink.
    pub fn submit(
        &mut self,
        caller: Caller,
        id: SessionId,
        mut transfer: Transfer,
        deadline: u64,
    ) -> Result<()> {
        self.owner(caller, id)?;
        let l = &self.sessions[&id];
        if self.stopped || l.view.state != SessionState::Active {
            return Err(Error::State);
        }
        if self.clock.now() >= deadline || self.clock.now() >= l.expires {
            return Err(Error::Deadline);
        }
        if !transfer.budget_matches(&self.budget) {
            return Err(Error::Invalid);
        }
        let sequence = transfer.header().seqnum;
        if sequence <= l.last_sequence {
            return Err(Error::Invalid);
        }
        let device = l
            .view
            .devices
            .iter()
            .filter_map(|key| self.inventory.get(&key.id))
            .find(|d| d.device.wire_id() == transfer.header().device_id)
            .ok_or(Error::Invalid)?;
        if !device.present
            || !self.allowed(
                Caller {
                    peer: l.view.peer,
                    administrator: false,
                },
                device.device.key,
            )
        {
            return Err(Error::Denied);
        }
        transfer.validate(&device.device, self.backend.manages_topology())?;
        // Retired replies also occupy a slot: slow consumers backpressure even
        // zero-byte operations after native request credits have been released.
        let queued = self.replies.len();
        if self.pending.len() + queued >= self.config.resources.requests {
            return Err(Error::Exhausted);
        }
        let token = TransferId {
            session: id,
            sequence,
        };
        let credit = self
            .budget
            .reserve(std::mem::size_of::<(TransferId, Pending)>(), 0)?;
        self.sessions.get_mut(&id).unwrap().last_sequence = sequence;
        transfer.bind(token)?;
        match self.backend.submit(token, transfer) {
            Ok(()) => {
                self.pending.insert(
                    token,
                    Pending {
                        // Transfer timeout is independent of the renewable lease.
                        // Current lease expiry is enforced separately by tick().
                        deadline,
                        retirement_deadline: None,
                        _credit: credit,
                        cancel: None,
                        unlink: None,
                    },
                );
                Ok(())
            }
            Err((error, _transfer)) => Err(error),
        }
    }
    /// Request USB/IP cancellation with independent cleanup capacity. Completion
    /// order is the order observed by the engine; a queued completion already won.
    pub fn unlink(
        &mut self,
        caller: Caller,
        id: SessionId,
        sequence: u32,
        target: u32,
    ) -> Result<UnlinkState> {
        self.owner(caller, id)?;
        let l = self.sessions.get_mut(&id).unwrap();
        if l.view.state != SessionState::Active {
            return Err(Error::State);
        }
        if sequence <= l.last_sequence || target >= sequence || target == 0 {
            return Err(Error::Invalid);
        }
        l.last_sequence = sequence;
        let token = TransferId {
            session: id,
            sequence: target,
        };
        let Some(p) = self.pending.get_mut(&token) else {
            return Ok(UnlinkState::TooLate);
        };
        if p.cancel.is_some() {
            return Err(Error::State);
        }
        p.cancel = Some(Error::Cancelled);
        p.unlink = Some(sequence);
        p.retirement_deadline = Some(self.clock.now().saturating_add(self.config.cleanup_ms));
        self.backend.cancel(token);
        Ok(UnlinkState::Pending)
    }
    /// Take one retired result; its owned byte charge persists until it is dropped.
    pub fn next_transfer_event(
        &mut self,
        caller: Caller,
        id: SessionId,
    ) -> Result<Option<TransferEvent>> {
        self.owner(caller, id)?;
        Ok(self
            .replies
            .pop(&mut self.sessions.get_mut(&id).unwrap().replies))
    }
    /// Advance deadlines, native completions and device ownership.
    /// This is on the transfer-completion path: the host must drive it promptly
    /// when native work completes and when deadlines become due. It does not wait
    /// for I/O. Work is bounded by configured session/request counts per tick.
    pub fn tick(&mut self) {
        let now = self.clock.now();
        if now < self.last_now {
            self.shutdown();
        }
        self.last_now = now;

        for _ in 0..self.config.devices {
            let Some(key) = self.backend.poll_removed() else {
                break;
            };
            let _ = self.remove(key);
        }

        // Preserve race resolution: deadlines before completions, then claims and releases.
        self.expire_deadlines(now);
        self.retire_completions();
        self.advance_sessions(now);
    }

    fn expire_deadlines(&mut self, now: u64) {
        let mut cursor = None;
        while let Some(id) = self.next_session(cursor) {
            cursor = Some(id);
            let lease = &self.sessions[&id];
            if matches!(
                lease.view.state,
                SessionState::Active | SessionState::Acquiring
            ) && now >= lease.expires
            {
                self.drain(id, Error::Deadline);
            }
        }
        for (id, pending) in &mut self.pending {
            if now >= pending.deadline && pending.cancel.is_none() {
                pending.cancel = Some(Error::Deadline);
                pending.retirement_deadline = Some(now.saturating_add(self.config.cleanup_ms));
                self.backend.cancel(*id);
            }
        }
    }

    fn retire_completions(&mut self) {
        for _ in 0..self.config.resources.requests {
            let Some((id, done)) = self.backend.poll_completion() else {
                break;
            };
            self.retire_completion(id, done);
        }
    }

    fn retire_completion(&mut self, id: TransferId, mut done: Completion) {
        // Unknown or mismatched backend tokens are contract failures. Never
        // retire a different pending request using a forged correlation ID.
        if done.transfer.token() != Some(id) || !done.transfer.budget_matches(&self.budget) {
            self.shutdown();
            return;
        }
        let Some(pending) = self.pending.remove(&id) else {
            self.shutdown();
            return;
        };
        done.transfer.retire();
        let valid = done.validate();
        let lease = self.sessions.get_mut(&id.session).expect("pending lease");
        if lease.view.state != SessionState::Active {
            return;
        }
        let event = if let Some(sequence) = pending.unlink {
            TransferEvent::Unlinked { sequence }
        } else if let Some(error) = pending.cancel {
            TransferEvent::Failed {
                sequence: id.sequence,
                error,
            }
        } else if let Err(error) = valid {
            TransferEvent::Failed {
                sequence: id.sequence,
                error,
            }
        } else {
            TransferEvent::Complete(done)
        };
        self.replies.push(&mut lease.replies, event);
    }

    fn advance_sessions(&mut self, now: u64) {
        let mut cursor = None;
        while let Some(id) = self.next_session(cursor) {
            cursor = Some(id);
            if self.sessions[&id].view.state == SessionState::Active
                && self
                    .pending
                    .range(Self::transfer_range(id))
                    .any(|(_, p)| p.retirement_deadline.is_some_and(|d| now >= d))
            {
                self.drain(id, Error::Cleanup);
                self.sessions.get_mut(&id).unwrap().cleanup = now;
            }
            if self.sessions[&id].view.state == SessionState::Acquiring {
                self.advance_claim(id, now);
            }
            // A claim failure may start draining in this same tick.
            if matches!(
                self.sessions[&id].view.state,
                SessionState::Draining | SessionState::Quarantined
            ) {
                self.advance_release(id, now);
            }
        }
    }

    fn advance_claim(&mut self, id: SessionId, now: u64) {
        let lease = &self.sessions[&id];
        let key = lease.view.devices[lease.claimed];
        let caller = Caller {
            peer: lease.view.peer,
            administrator: false,
        };
        if !self.allowed(caller, key) {
            self.drain(id, Error::Denied);
            return;
        }
        self.sessions.get_mut(&id).unwrap().attempted = lease.claimed + 1;
        match self.backend.claim(id, &self.inventory[&key.id].device) {
            Poll::Pending => {}
            Poll::Ready(Err(error)) => self.drain(id, error),
            Poll::Ready(Ok(())) => {
                let lease = self.sessions.get_mut(&id).unwrap();
                lease.claimed += 1;
                if lease.claimed == lease.view.devices.len() {
                    lease.view.state = SessionState::Active;
                    lease.expires = now.saturating_add(self.config.lease_ms);
                    self.emit_session(id);
                }
            }
        }
    }

    fn advance_release(&mut self, id: SessionId, now: u64) {
        if now >= self.sessions[&id].cleanup {
            self.quarantine(id);
        }
        if self
            .pending
            .range(Self::transfer_range(id))
            .next()
            .is_some()
        {
            return;
        }

        // One reverse-order release per tick, including a failed/pending claim.
        let lease = &self.sessions[&id];
        if lease.attempted > 0 {
            let key = lease.view.devices[lease.attempted - 1];
            match self.backend.release(id, key) {
                Poll::Pending => return,
                Poll::Ready(Err(_)) => {
                    self.quarantine(id);
                    return;
                }
                Poll::Ready(Ok(())) => self.sessions.get_mut(&id).unwrap().attempted -= 1,
            }
        }
        if self.sessions[&id].attempted == 0 {
            self.sessions.get_mut(&id).unwrap().view.state = SessionState::Closed;
            self.emit_session(id);
        }
    }

    fn quarantine(&mut self, id: SessionId) {
        let lease = self.sessions.get_mut(&id).unwrap();
        if lease.view.state == SessionState::Quarantined {
            return;
        }
        lease.view.state = SessionState::Quarantined;
        lease.view.error = Some(Error::Cleanup);
        // Notify once, whether timeout or backend release failure caused it.
        self.emit_session(id);
    }
    /// Remove all grants and drain all leases for a revoked authenticated peer.
    /// Only trusted host composition calls this after changing its trust store.
    pub fn revoke_peer(&mut self, peer: PeerId) {
        self.grants.retain(|(_, grantee), _| *grantee != peer);
        let mut cursor = None;
        while let Some(id) = self.next_session(cursor) {
            cursor = Some(id);
            if self.sessions[&id].view.peer == peer {
                self.drain(id, Error::Denied);
            }
        }
        // No device identifiers are disclosed by inventory notifications.
        let mut after = None;
        loop {
            use std::ops::Bound::{Excluded, Unbounded};
            let next = match after {
                None => self.inventory.keys().next().copied(),
                Some(id) => self
                    .inventory
                    .range((Excluded(id), Unbounded))
                    .next()
                    .map(|(id, _)| *id),
            };
            let Some(id) = next else {
                break;
            };
            self.emit_inventory(id, Some(peer));
            after = Some(id);
        }
    }
    /// Freeze ingress and begin idempotent drain. Continue ticking until stopped.
    pub fn shutdown(&mut self) {
        self.stopped = true;
        let mut cursor = None;
        while let Some(id) = self.next_session(cursor) {
            cursor = Some(id);
            self.drain(id, Error::Cancelled);
        }
    }
    /// True only after all native requests and claims have retired. Consumers may
    /// still own completion buffers, whose credits remain independently charged.
    pub fn shutdown_complete(&self) -> bool {
        self.stopped
            && self.pending.is_empty()
            && self
                .sessions
                .values()
                .all(|l| l.view.state == SessionState::Closed)
    }
    // Walk sessions without allocating temporary cleanup lists. Admission bounds
    // every traversal; removal/cancellation never needs another data credit.
    fn next_session(&self, after: Option<SessionId>) -> Option<SessionId> {
        match after {
            None => self.sessions.keys().next().copied(),
            Some(id) => self
                .sessions
                .range((std::ops::Bound::Excluded(id), std::ops::Bound::Unbounded))
                .next()
                .map(|(id, _)| *id),
        }
    }
    fn transfer_range(id: SessionId) -> std::ops::RangeInclusive<TransferId> {
        TransferId {
            session: id,
            sequence: 0,
        }..=TransferId {
            session: id,
            sequence: u32::MAX,
        }
    }
    fn reclaim_closed(&mut self) -> bool {
        let oldest = self
            .sessions
            .iter()
            .find(|(_, l)| l.view.state == SessionState::Closed)
            .map(|(id, _)| *id);
        if let Some(id) = oldest {
            self.sessions.remove(&id);
            true
        } else {
            false
        }
    }
    fn event_queue(budget: &Budget, count: usize) -> Result<(VecDeque<Event>, Credit)> {
        let bytes = count
            .checked_mul(std::mem::size_of::<Event>())
            .and_then(|n| n.checked_add(std::mem::size_of::<Subscription>()))
            .ok_or(Error::Exhausted)?;
        let mut credit = budget.reserve(bytes, 0)?;
        let mut events = VecDeque::new();
        events
            .try_reserve_exact(count)
            .map_err(|_| Error::Exhausted)?;
        credit.grow((events.capacity() - count) * std::mem::size_of::<Event>())?;
        Ok((events, credit))
    }
    fn enqueue_event(
        sub: &mut Subscription,
        capacity: usize,
        kind: EventKind,
        session: Option<SessionId>,
    ) {
        sub.sequence = sub.sequence.saturating_add(1);
        if sub.overflow {
            return;
        }
        // Exclusive engine access serializes this check and push. >= also fails
        // closed if a future change ever leaves the queue above its limit.
        if sub.events.len() >= capacity {
            sub.events.clear();
            sub.overflow = true;
        } else {
            sub.events.push_back(Event {
                sequence: sub.sequence,
                kind,
                session,
            });
        }
    }
    fn emit_session(&mut self, id: SessionId) {
        let peer = self.sessions[&id].view.peer;
        for sub in self.subscribers.values_mut() {
            if sub.caller.administrator || sub.caller.peer == peer {
                Self::enqueue_event(sub, self.config.events, EventKind::Session, Some(id));
            }
        }
    }
    fn emit_inventory(&mut self, device: DeviceId, grantee: Option<PeerId>) {
        for sub in self.subscribers.values_mut() {
            let visible = if let Some(peer) = grantee {
                sub.caller.peer == peer
            } else {
                self.grants.contains_key(&(device, sub.caller.peer))
            };
            if sub.caller.administrator || visible {
                Self::enqueue_event(sub, self.config.events, EventKind::Inventory, None);
            }
        }
    }
}
impl<B: ExporterBackend, C: Clock> Engine<B, C> {
    fn authorize(
        &mut self,
        caller: Caller,
        device: DeviceKey,
        peer: PeerId,
        allow: bool,
    ) -> Result<Reply> {
        if !caller.administrator {
            return Err(Error::Denied);
        }
        if !self.allowed(caller, device) {
            return Err(Error::NotFound);
        }
        if allow {
            if self.stopped {
                return Err(Error::State);
            }
            let existing = self.grants.contains_key(&(device.id, peer));
            let device_grants = self
                .grants
                .keys()
                .filter(|(id, _)| *id == device.id)
                .count();
            if !existing
                && (self.grants.len() >= self.config.grants || device_grants >= MAX_DEVICE_GRANTS)
            {
                return Err(Error::Exhausted);
            }
            if let std::collections::btree_map::Entry::Vacant(entry) =
                self.grants.entry((device.id, peer))
            {
                let credit = self
                    .budget
                    .reserve(std::mem::size_of::<((DeviceId, PeerId), Credit)>(), 0)?;
                entry.insert(credit);
            }
        } else {
            self.grants.remove(&(device.id, peer));
            let mut cursor = None;
            while let Some(id) = self.next_session(cursor) {
                cursor = Some(id);
                let view = &self.sessions[&id].view;
                if view.peer == peer && view.devices.contains(&device) {
                    self.drain(id, Error::Denied);
                }
            }
        }
        self.emit_inventory(device.id, Some(peer));
        Ok(Reply::Done)
    }
    fn subscribe(&mut self, caller: Caller) -> Result<Reply> {
        let reserved = caller.administrator
            && !self.admin_queues.is_empty()
            && !self
                .subscribers
                .values()
                .any(|s| s.reserved && s.caller.peer == caller.peer);
        if !reserved
            && self
                .subscribers
                .values()
                .filter(|s| !s.reserved && s.caller.peer == caller.peer)
                .count()
                >= self.config.subscriptions_per_peer
        {
            return Err(Error::Exhausted);
        }
        if !reserved
            && self.subscribers.values().filter(|s| !s.reserved).count()
                >= self.config.subscriptions - self.config.admin_subscriptions
        {
            return Err(Error::Exhausted);
        }
        let id = self.subscription.checked_add(1).ok_or(Error::Exhausted)?;
        let (events, credit) = if reserved {
            self.admin_queues.pop().unwrap()
        } else {
            Self::event_queue(&self.budget, self.config.events)?
        };
        self.subscription = id;
        self.subscribers.insert(
            id,
            Subscription {
                caller,
                events,
                overflow: false,
                sequence: 0,
                reserved,
                _credit: credit,
            },
        );
        Ok(Reply::Subscription(id))
    }
    fn permissions(&self, caller: Caller, key: DeviceKey) -> Result<Reply> {
        if !caller.administrator {
            return Err(Error::Denied);
        }
        if !self.allowed(caller, key) {
            return Err(Error::NotFound);
        }
        Ok(Reply::Peers(
            self.grants
                .keys()
                .filter(|(id, _)| *id == key.id)
                .map(|(_, peer)| *peer)
                .collect(),
        ))
    }
    fn poll_event(&mut self, caller: Caller, id: u64) -> Result<Reply> {
        let sub = self
            .subscribers
            .get_mut(&id)
            .filter(|s| s.caller == caller)
            .ok_or(Error::Denied)?;
        let event = if sub.overflow {
            sub.overflow = false;
            Some(Event {
                sequence: sub.sequence,
                kind: EventKind::Resync,
                session: None,
            })
        } else {
            sub.events.pop_front()
        };
        Ok(Reply::Event(event))
    }
    fn unsubscribe(&mut self, caller: Caller, id: u64) -> Result<Reply> {
        if !self
            .subscribers
            .get(&id)
            .is_some_and(|s| s.caller == caller)
        {
            return Err(Error::Denied);
        }
        let mut sub = self.subscribers.remove(&id).unwrap();
        if sub.reserved {
            sub.events.clear();
            self.admin_queues.push((sub.events, sub._credit));
        }
        Ok(Reply::Done)
    }
}

impl<B: ExporterBackend, C: Clock> ControlTarget for Engine<B, C> {
    fn now(&self) -> u64 {
        self.clock.now()
    }
    fn execute(&mut self, caller: Caller, command: Command, deadline: u64) -> Result<Reply> {
        if self.clock.now() >= deadline {
            return Err(Error::Deadline);
        }
        match command {
            Command::List => Ok(Reply::Devices(
                self.inventory
                    .values()
                    .filter(|d| self.allowed(caller, d.device.key))
                    .map(|d| DeviceInfo {
                        key: d.device.key,
                        busy: self.busy(d.device.key),
                    })
                    .collect(),
            )),
            Command::Authorize {
                device,
                peer,
                allow,
            } => self.authorize(caller, device, peer, allow),
            Command::Permissions(key) => self.permissions(caller, key),
            Command::Attach(keys) => self.attach(caller, keys, deadline),
            Command::Detach(id) => {
                self.owner(caller, id)?;
                self.drain(id, Error::Cancelled);
                Ok(Reply::Done)
            }
            Command::Status(id) => {
                self.owner(caller, id)?;
                Ok(Reply::Session(self.sessions[&id].view.clone()))
            }
            Command::Heartbeat(id) => {
                self.owner(caller, id)?;
                let l = self.sessions.get_mut(&id).unwrap();
                if l.view.state != SessionState::Active || self.clock.now() >= l.expires {
                    return Err(Error::Deadline);
                }
                l.expires = self.clock.now().saturating_add(self.config.lease_ms);
                Ok(Reply::Done)
            }
            Command::Subscribe => self.subscribe(caller),
            Command::Poll(id) => self.poll_event(caller, id),
            Command::Unsubscribe(id) => self.unsubscribe(caller, id),
            Command::Forget(id) => {
                self.owner(caller, id)?;
                if self.sessions[&id].view.state != SessionState::Closed {
                    return Err(Error::State);
                }
                self.sessions.remove(&id);
                Ok(Reply::Done)
            }
            Command::Shutdown => {
                if !caller.administrator {
                    return Err(Error::Denied);
                }
                self.shutdown();
                Ok(Reply::Done)
            }
        }
    }
}
