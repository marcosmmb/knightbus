//! Route independent native workers through one bounded engine and listener pair.
use crate::*;
use std::{
    collections::BTreeMap,
    task::{Poll, Waker},
};

struct Entry {
    backend: Box<dyn ExporterBackend + Send>,
    wire_id: u32,
    session: Option<SessionId>,
    _credit: MemoryReservation,
}
/// A set of topology-managing exporters, each owning one physical generation.
#[derive(Default)]
pub struct Group {
    entries: BTreeMap<DeviceKey, Entry>,
    waker: Option<Waker>,
    completion_cursor: Option<DeviceKey>,
}
impl Group {
    /// Install only inside `Engine::register_backend`, whose inventory limit bounds
    /// this map. Failed installation leaves existing workers untouched.
    pub fn insert(
        &mut self,
        device: &Device,
        mut backend: Box<dyn ExporterBackend + Send>,
        budget: &Budget,
    ) -> Result<()> {
        if self.entries.contains_key(&device.key)
            || self
                .entries
                .iter()
                .any(|(key, entry)| key.id == device.key.id && entry.session.is_some())
            || !backend.manages_topology()
        {
            return Err(Error::State);
        }
        let credit = budget.reserve_memory(size_of::<(DeviceKey, Entry)>())?;
        backend.set_waker(self.waker.clone());
        self.entries.retain(|key, _| key.id != device.key.id);
        self.entries.insert(
            device.key,
            Entry {
                backend,
                wire_id: device.wire_id(),
                session: None,
                _credit: credit,
            },
        );
        Ok(())
    }
}
impl ExporterBackend for Group {
    fn set_waker(&mut self, waker: Option<Waker>) {
        self.waker = waker.clone();
        for entry in self.entries.values_mut() {
            entry.backend.set_waker(waker.clone());
        }
    }
    fn manages_topology(&self) -> bool {
        true
    }
    fn poll_removed(&mut self) -> Option<DeviceKey> {
        self.entries
            .values_mut()
            .find_map(|e| e.backend.poll_removed())
    }
    fn claim(&mut self, session: SessionId, device: &Device) -> Poll<Result<()>> {
        let Some(entry) = self.entries.get_mut(&device.key) else {
            return Poll::Ready(Err(Error::NotFound));
        };
        if entry.session.is_some_and(|owner| owner != session) {
            return Poll::Ready(Err(Error::State));
        }
        entry.session = Some(session);
        entry.backend.claim(session, device)
    }
    fn submit(
        &mut self,
        id: TransferId,
        transfer: Transfer,
    ) -> std::result::Result<(), (Error, Transfer)> {
        let entry = self
            .entries
            .values_mut()
            .find(|e| e.session == Some(id.session) && e.wire_id == transfer.header().device_id);
        match entry {
            Some(e) => e.backend.submit(id, transfer),
            None => Err((Error::NotFound, transfer)),
        }
    }
    fn cancel(&mut self, id: TransferId) {
        for entry in self
            .entries
            .values_mut()
            .filter(|e| e.session == Some(id.session))
        {
            entry.backend.cancel(id);
        }
    }
    fn poll_completion(&mut self) -> Option<(TransferId, Completion)> {
        // Rotate workers so a busy camera cannot starve another device.
        use std::ops::Bound::{Excluded, Unbounded};
        let after = self.completion_cursor.map_or(Unbounded, Excluded);
        let found = self
            .entries
            .range_mut((after, Unbounded))
            .find_map(|(key, e)| e.backend.poll_completion().map(|done| (*key, done)))
            .or_else(|| {
                self.entries
                    .range_mut((
                        Unbounded,
                        self.completion_cursor
                            .map_or(Unbounded, std::ops::Bound::Included),
                    ))
                    .find_map(|(key, e)| e.backend.poll_completion().map(|done| (*key, done)))
            });
        found.map(|(key, done)| {
            self.completion_cursor = Some(key);
            done
        })
    }
    fn release(&mut self, session: SessionId, device: DeviceKey) -> Poll<Result<()>> {
        let Some(entry) = self.entries.get_mut(&device) else {
            return Poll::Ready(Err(Error::NotFound));
        };
        if entry.session.is_some_and(|owner| owner != session) {
            return Poll::Ready(Err(Error::State));
        }
        let result = entry.backend.release(session, device);
        if matches!(result, Poll::Ready(Ok(()))) {
            entry.session = None;
        }
        result
    }
}
