//! Fault-injected ownership, admission, ordering and retirement regressions.
use knightbus_core::{
    descriptors,
    exporter::{Backend, ControlError, Driver, InterfaceDriver, SubmitError},
};
use knightbus_core::{
    usbip::{Direction, SubmitHeader},
    *,
};
const SLOTS: usize = Fake::CAPABILITIES.requests;
use std::{
    collections::{BTreeMap, BTreeSet},
    sync::{Arc, Mutex},
    task::{Poll, Waker},
    time::{Duration, Instant},
};

const SESSION: SessionId = SessionId {
    epoch: 1,
    serial: 1,
};
fn model() -> Device {
    let descriptor = [18, 1, 0, 2, 0, 0, 0, 64, 9, 0x12, 1, 0, 0, 1, 0, 0, 0, 1];
    let config = vec![
        9, 2, 32, 0, 1, 1, 0, 0x80, 50, 9, 4, 0, 0, 2, 0xff, 0, 0, 0, 7, 5, 0x81, 2, 64, 0, 0, 7,
        5, 2, 2, 64, 0, 0,
    ];
    descriptors::device(
        DeviceKey {
            id: DeviceId(1),
            generation: 1,
        },
        descriptors::Address {
            path: b"test",
            bus: 1,
            device: 0x1000,
        },
        2,
        &descriptor,
        &[config],
        1,
    )
    .unwrap()
}
fn budget() -> Budget {
    Budget::new(ResourceLimits {
        bytes: 4 * 1024 * 1024,
        requests: 256,
    })
}
fn transfer(b: &Budget, sequence: u32, endpoint: u32) -> Transfer {
    Transfer::new(
        b,
        SubmitHeader {
            seqnum: sequence,
            device_id: 0x00011000,
            direction: Direction::In,
            endpoint,
            transfer_flags: 0,
            transfer_buffer_length: 8,
            start_frame: 0,
            number_of_packets: 0,
            interval: 0,
            setup: [0; 8],
        },
        &[],
    )
    .unwrap()
}
fn token(sequence: u32) -> TransferId {
    TransferId {
        session: SESSION,
        sequence,
    }
}
fn wait(mut condition: impl FnMut() -> bool) {
    let end = Instant::now() + Duration::from_secs(3);
    while !condition() {
        assert!(Instant::now() < end, "worker timeout");
        std::thread::sleep(Duration::from_millis(1));
    }
}
#[derive(Default)]
struct State {
    pending: BTreeMap<TransferId, Transfer>,
    ready: BTreeSet<TransferId>,
    cancelled: BTreeSet<TransferId>,
    submitted: Vec<u32>,
    attempted: Vec<u32>,
    busy: BTreeSet<TransferId>,
    iso: bool,
    scratch_bytes: BTreeMap<TransferId, usize>,
    polls: usize,
    removed: bool,
    released: bool,
    drop_blocked: bool,
    dropped: bool,
    fail_release: bool,
    fail_cancel_count: usize,
    complete_on_cancel: bool,
    waker: Option<Waker>,
    configuration: u8,
    changes: Vec<(NativeChange, Vec<u32>)>,
    change_error: Option<Error>,
    control_status: Option<i32>,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum NativeChange {
    Configure(u8),
    Alternate(u8, u8),
    ClearHalt(u8),
}
fn record_change(
    state: &Arc<Mutex<State>>,
    change: NativeChange,
) -> std::result::Result<(), ControlError> {
    let mut s = state.lock().unwrap();
    let pending = s.pending.keys().map(|id| id.sequence).collect();
    s.changes.push((change, pending));
    if let Some(status) = s.control_status {
        return Err(status.into());
    }
    s.change_error.map_or(Ok(()), |error| Err(error.into()))
}
struct FakeInterface {
    number: u8,
    state: Arc<Mutex<State>>,
}
impl InterfaceDriver for FakeInterface {
    fn select_alternate(
        &mut self,
        setting: &knightbus_core::Interface,
    ) -> std::result::Result<(), ControlError> {
        assert_eq!(self.number, setting.number);
        record_change(
            &self.state,
            NativeChange::Alternate(self.number, setting.alternate),
        )
    }
    fn clear_halt(&mut self, address: u8) -> std::result::Result<(), ControlError> {
        record_change(&self.state, NativeChange::ClearHalt(address))
    }
}
struct Fake(Arc<Mutex<State>>, Vec<FakeInterface>);
impl Drop for Fake {
    fn drop(&mut self) {
        loop {
            let mut state = self.0.lock().unwrap();
            if !state.drop_blocked {
                state.dropped = true;
                break;
            }
            drop(state);
            std::thread::sleep(Duration::from_millis(1));
        }
    }
}
impl Driver for Fake {
    type Interface = FakeInterface;
    fn configuration(&self) -> u8 {
        self.0.lock().unwrap().configuration
    }
    fn interface(&mut self, number: u8) -> Result<&mut FakeInterface> {
        self.1
            .iter_mut()
            .find(|i| i.number == number)
            .ok_or(Error::Invalid)
    }
    fn configure(
        &mut self,
        configuration: Option<&Configuration>,
    ) -> std::result::Result<(), ControlError> {
        let value = configuration.map_or(0, |c| c.value);
        record_change(&self.0, NativeChange::Configure(value))?;
        self.0.lock().unwrap().configuration = value;
        Ok(())
    }
    fn endpoint(&self, address: u8) -> Option<knightbus_core::Endpoint> {
        if !matches!(address, 0x81 | 0x82) {
            return None;
        }
        let iso = self.0.lock().unwrap().iso && address == 0x81;
        Some(knightbus_core::Endpoint {
            address,
            kind: if iso {
                TransferKind::Isochronous
            } else {
                TransferKind::Bulk
            },
            max_packet_size: 64,
            service_bytes: 64,
            interval: if iso { 1 } else { 0 },
            allowed_flags: if iso { usbip::ISO_ASAP } else { 0 },
        })
    }
    const CAPABILITIES: knightbus_core::Capabilities = knightbus_core::Capabilities {
        transfer_bytes: 1024 * 1024,
        requests: 64,
        iso_packets: 1024,
        speeds: 0b1101110,
        descriptor_bytes: 65535,
        configurations: 16,
        remote_wakeup: true,
    };
    fn set_waker(&mut self, waker: Waker) {
        self.0.lock().unwrap().waker = Some(waker);
    }
    fn submit(
        &mut self,
        id: TransferId,
        t: Transfer,
    ) -> std::result::Result<(), (SubmitError, Transfer)> {
        let mut s = self.0.lock().unwrap();
        s.attempted.push(id.sequence);
        if s.busy.contains(&id) {
            return Err((SubmitError::Busy, t));
        }
        if let Some(bytes) = s.scratch_bytes.get(&id)
            && let Err(error) = t.reserve_native_memory(*bytes)
        {
            return Err((SubmitError::Failed(error), t));
        }
        if t.layout().is_empty()
            && s.pending
                .values()
                .any(|v| v.header().endpoint == t.header().endpoint)
        {
            return Err((SubmitError::Busy, t));
        }
        s.submitted.push(id.sequence);
        s.pending.insert(id, t);
        Ok(())
    }
    fn cancel(&mut self, id: TransferId) -> Result<()> {
        let mut s = self.0.lock().unwrap();
        if s.fail_cancel_count > 0 {
            s.fail_cancel_count -= 1;
            return Err(Error::Cleanup);
        }
        s.cancelled.insert(id);
        if s.complete_on_cancel {
            s.ready.insert(id);
            s.waker.as_ref().unwrap().wake_by_ref();
        }
        Ok(())
    }
    fn completion(&mut self) -> Option<(TransferId, Completion)> {
        let mut s = self.0.lock().unwrap();
        let id = s.ready.pop_first()?;
        let transfer = s.pending.remove(&id).unwrap();
        let status = if s.cancelled.contains(&id) { -125 } else { 0 };
        Some((
            id,
            Completion {
                transfer,
                status,
                actual_length: 0,
                start_frame: 0,
            },
        ))
    }
    fn removed(&self) -> bool {
        let mut s = self.0.lock().unwrap();
        s.polls += 1;
        s.removed
    }
    fn release(&mut self) -> Result<()> {
        let mut s = self.0.lock().unwrap();
        assert!(s.pending.is_empty());
        if s.fail_release {
            return Err(Error::Cleanup);
        }
        s.released = true;
        Ok(())
    }
}
type Factory = Box<dyn FnOnce() -> Result<Fake> + Send>;
type Fixture = (Backend<Factory>, Arc<Mutex<State>>, Budget, Device);
fn fixture() -> Fixture {
    fixture_for(model())
}
fn fixture_for(device: Device) -> Fixture {
    let state = Arc::new(Mutex::new(State {
        configuration: 1,
        ..State::default()
    }));
    let copied = state.clone();
    let b = budget();
    let interfaces = device
        .configurations
        .iter()
        .flat_map(|c| &c.interfaces)
        .map(|i| i.number)
        .collect::<BTreeSet<_>>()
        .into_iter()
        .map(|number| FakeInterface {
            number,
            state: state.clone(),
        })
        .collect();
    let factory: Factory = Box::new(move || Ok(Fake(copied, interfaces)));
    let mut backend = Backend::new(device.clone(), &b, factory).unwrap();
    wait(|| matches!(backend.claim(SESSION, &device), Poll::Ready(Ok(()))));
    (backend, state, b, device)
}
#[test]
fn retirement_waits_for_native_destructor() {
    let (mut backend, state, _budget, device) = fixture();
    state.lock().unwrap().drop_blocked = true;
    assert!(backend.release(SESSION, device.key).is_pending());
    wait(|| state.lock().unwrap().released);
    let still_pending = backend.release(SESSION, device.key).is_pending();
    state.lock().unwrap().drop_blocked = false;
    assert!(still_pending, "retirement preceded the native destructor");
    wait(|| matches!(backend.release(SESSION, device.key), Poll::Ready(Ok(()))));
    assert!(state.lock().unwrap().dropped);
}
#[test]
fn cancellation_keeps_native_owner_and_credits_until_callback() {
    let (mut backend, state, b, device) = fixture();
    let baseline = b.usage();
    backend.submit(token(1), transfer(&b, 1, 1)).unwrap();
    wait(|| state.lock().unwrap().pending.len() == 1);
    backend.cancel(token(1));
    wait(|| state.lock().unwrap().cancelled.contains(&token(1)));
    assert!(backend.poll_completion().is_none());
    assert_eq!(b.usage().requests, 1);
    assert!(matches!(
        backend.release(SESSION, device.key),
        Poll::Pending
    ));
    {
        let mut s = state.lock().unwrap();
        s.ready.insert(token(1));
        s.waker.as_ref().unwrap().wake_by_ref();
    }
    let mut done = None;
    wait(|| {
        done = backend.poll_completion();
        done.is_some()
    });
    assert_eq!(done.as_ref().unwrap().1.status, -125);
    drop(done);
    wait(|| matches!(backend.release(SESSION, device.key), Poll::Ready(Ok(()))));
    assert_eq!(b.usage(), baseline);
}
#[test]
fn busy_pipe_order_and_independent_pipe_progress() {
    let (mut backend, state, b, device) = fixture();
    for (seq, ep) in [(1, 1), (2, 1), (3, 2)] {
        backend.submit(token(seq), transfer(&b, seq, ep)).unwrap();
    }
    wait(|| state.lock().unwrap().submitted.len() == 2);
    assert_eq!(state.lock().unwrap().submitted, [1, 3]);
    {
        let mut s = state.lock().unwrap();
        s.ready.insert(token(1));
        s.waker.as_ref().unwrap().wake_by_ref();
    }
    wait(|| state.lock().unwrap().submitted.len() == 3);
    assert_eq!(state.lock().unwrap().submitted, [1, 3, 2]);
    {
        let mut s = state.lock().unwrap();
        s.ready.extend([token(2), token(3)]);
        s.waker.as_ref().unwrap().wake_by_ref();
    }
    let mut count = 0;
    wait(|| {
        while backend.poll_completion().is_some() {
            count += 1;
        }
        count == 3
    });
    wait(|| matches!(backend.release(SESSION, device.key), Poll::Ready(Ok(()))));
}
#[test]
fn drop_does_not_release_device_or_storage_while_callback_is_stalled() {
    let (mut backend, state, b, _) = fixture();
    backend.submit(token(1), transfer(&b, 1, 1)).unwrap();
    wait(|| state.lock().unwrap().pending.len() == 1);
    drop(backend);
    wait(|| state.lock().unwrap().cancelled.contains(&token(1)));
    assert!(!state.lock().unwrap().released);
    assert_eq!(b.usage().requests, 1);
    {
        let mut s = state.lock().unwrap();
        s.ready.insert(token(1));
        s.waker.as_ref().unwrap().wake_by_ref();
    }
    wait(|| state.lock().unwrap().released);
    wait(|| b.usage().bytes == 0);
}
#[test]
fn idle_removal_is_reported_once_and_release_failure_is_retryable() {
    let (mut backend, state, _b, device) = fixture();
    {
        let mut s = state.lock().unwrap();
        s.fail_release = true;
        s.removed = true;
        s.waker.as_ref().unwrap().wake_by_ref();
    }
    wait(|| backend.poll_removed() == Some(device.key));
    assert_eq!(backend.poll_removed(), None);
    assert!(matches!(
        backend.release(SESSION, device.key),
        Poll::Pending
    ));
    state.lock().unwrap().fail_release = false;
    wait(|| matches!(backend.release(SESSION, device.key), Poll::Ready(Ok(()))));
}
#[test]
fn saturation_and_wrong_generation_fail_before_native_admission() {
    let (mut backend, state, b, device) = fixture();
    let mut other = device.clone();
    other.key.generation += 1;
    assert_eq!(
        backend.claim(SESSION, &other),
        Poll::Ready(Err(Error::Invalid))
    );
    for sequence in 1..=SLOTS as u32 {
        backend
            .submit(token(sequence), transfer(&b, sequence, 1))
            .unwrap();
    }
    let (error, owner) = backend
        .submit(token(100), transfer(&b, 100, 1))
        .unwrap_err();
    assert_eq!(error, Error::Exhausted);
    drop(owner);
    let mut foreign = token(101);
    foreign.session.serial = 2;
    assert_eq!(
        backend.submit(foreign, transfer(&b, 101, 1)).unwrap_err().0,
        Error::Unavailable
    );
    // Exercise shutdown with one native owner and the rest still queued.
    // Without this handshake, shutdown may cancel every request before native
    // submission, in which case waiting for Fake::cancel would never finish.
    wait(|| state.lock().unwrap().pending.contains_key(&token(1)));
    let _ = backend.release(SESSION, device.key);
    wait(|| !state.lock().unwrap().cancelled.is_empty());
    {
        let mut s = state.lock().unwrap();
        let ids: Vec<_> = s.pending.keys().copied().collect();
        s.ready.extend(ids);
        s.waker.as_ref().unwrap().wake_by_ref();
    }
    wait(|| matches!(backend.release(SESSION, device.key), Poll::Ready(Ok(()))));
}
#[test]
fn topology_change_waits_for_prior_pipes_and_blocks_later_submissions() {
    let (mut backend, state, b, device) = fixture();
    backend.submit(token(1), transfer(&b, 1, 1)).unwrap();
    let change = Transfer::new(
        &b,
        SubmitHeader {
            seqnum: 2,
            device_id: 0x00011000,
            direction: Direction::Out,
            endpoint: 0,
            transfer_flags: 0,
            transfer_buffer_length: 0,
            start_frame: 0,
            number_of_packets: 0,
            interval: 0,
            setup: [0, 9, 1, 0, 0, 0, 0, 0],
        },
        &[],
    )
    .unwrap();
    backend.submit(token(2), change).unwrap();
    backend.submit(token(3), transfer(&b, 3, 2)).unwrap();
    wait(|| state.lock().unwrap().submitted == [1]);
    std::thread::sleep(Duration::from_millis(10));
    assert_eq!(state.lock().unwrap().submitted, [1]);
    {
        let mut s = state.lock().unwrap();
        s.ready.insert(token(1));
        s.waker.as_ref().unwrap().wake_by_ref();
    }
    wait(|| state.lock().unwrap().submitted == [1, 3]);
    assert_eq!(
        state.lock().unwrap().changes,
        [(NativeChange::Configure(1), vec![])]
    );
    {
        let mut s = state.lock().unwrap();
        s.ready.insert(token(3));
        s.waker.as_ref().unwrap().wake_by_ref();
    }
    let mut n = 0;
    wait(|| {
        while backend.poll_completion().is_some() {
            n += 1;
        }
        n == 3
    });
    wait(|| matches!(backend.release(SESSION, device.key), Poll::Ready(Ok(()))));
}

#[test]
fn descriptors_reject_every_truncated_prefix_and_ambiguous_interfaces() {
    let c = [
        9, 2, 25, 0, 1, 1, 0, 0x80, 50, 9, 4, 0, 0, 1, 0xff, 0, 0, 0, 7, 5, 0x81, 1, 64, 0, 1,
    ];
    for length in 0..c.len() {
        assert!(descriptors::configuration(&c[..length]).is_err());
    }
    assert_eq!(
        descriptors::configuration(&c).unwrap().interfaces[0].endpoints[0].kind,
        TransferKind::Isochronous
    );
    let mut malformed = c;
    malformed[18] = 0;
    assert!(descriptors::configuration(&malformed).is_err());
    let mut duplicate = c.to_vec();
    duplicate.extend_from_slice(&c[9..]);
    duplicate[2] = duplicate.len() as u8;
    duplicate[4] = 2;
    assert!(descriptors::configuration(&duplicate).is_err());
    duplicate[27] = 1; // Independent interface using the same endpoint.
    assert!(descriptors::configuration(&duplicate).is_err());
}

fn complete(state: &Arc<Mutex<State>>, sequences: &[u32]) {
    let mut s = state.lock().unwrap();
    s.ready.extend(sequences.iter().copied().map(token));
    s.waker.as_ref().unwrap().wake_by_ref();
}
fn finish(backend: &mut Backend<Factory>, state: &Arc<Mutex<State>>, device: &Device) {
    let sequences = state
        .lock()
        .unwrap()
        .pending
        .keys()
        .map(|id| id.sequence)
        .collect::<Vec<_>>();
    complete(state, &sequences);
    wait(|| matches!(backend.release(SESSION, device.key), Poll::Ready(Ok(()))));
    while backend.poll_completion().is_some() {}
}

#[test]
fn scratch_allocation_failure_completes_once_before_later_same_pipe_work() {
    let (mut backend, state, b, device) = fixture();
    // This reservation really exceeds the shared memory budget. It must retire
    // as ENOMEM rather than enter the endpoint-busy retry path.
    state
        .lock()
        .unwrap()
        .scratch_bytes
        .insert(token(2), 4 * 1024 * 1024);
    for (seq, ep) in [(1, 1), (2, 1), (3, 1), (4, 2)] {
        backend.submit(token(seq), transfer(&b, seq, ep)).unwrap();
    }
    wait(|| state.lock().unwrap().submitted == [1, 4]);
    complete(&state, &[1]);
    wait(|| state.lock().unwrap().submitted == [1, 4, 3]);
    let mut failed = None;
    wait(|| {
        while let Some((id, done)) = backend.poll_completion() {
            if id == token(2) {
                failed = Some(done.status);
            }
        }
        failed.is_some()
    });
    assert_eq!(failed, Some(-12));
    assert_eq!(state.lock().unwrap().attempted, [1, 4, 2, 3]);
    finish(&mut backend, &state, &device);
}

#[test]
fn busy_head_blocks_its_pipe_but_not_other_pipes_and_can_be_cancelled() {
    let (mut backend, state, b, device) = fixture();
    state.lock().unwrap().busy.insert(token(1));
    for (seq, ep) in [(1, 1), (2, 1), (3, 2)] {
        backend.submit(token(seq), transfer(&b, seq, ep)).unwrap();
    }
    wait(|| state.lock().unwrap().submitted == [3]);
    assert!(!state.lock().unwrap().attempted.contains(&2));
    backend.cancel(token(1));
    wait(|| state.lock().unwrap().submitted == [3, 2]);
    finish(&mut backend, &state, &device);
}

#[test]
fn endpoint_order_uses_admission_order_not_wrapping_sequence_numbers() {
    let (mut backend, state, b, device) = fixture();
    backend.submit(token(10), transfer(&b, 10, 1)).unwrap();
    wait(|| state.lock().unwrap().submitted == [10]);
    for seq in [u32::MAX, 1] {
        backend.submit(token(seq), transfer(&b, seq, 1)).unwrap();
    }
    complete(&state, &[10]);
    wait(|| state.lock().unwrap().submitted.len() == 2);
    assert_eq!(state.lock().unwrap().submitted, [10, u32::MAX]);
    complete(&state, &[u32::MAX]);
    wait(|| state.lock().unwrap().submitted == [10, u32::MAX, 1]);
    finish(&mut backend, &state, &device);
}

#[test]
fn idle_worker_does_not_poll_and_completion_notification_restarts_it() {
    let (mut backend, state, b, device) = fixture();
    wait(|| state.lock().unwrap().polls > 0);
    let idle_polls = state.lock().unwrap().polls;
    std::thread::sleep(Duration::from_millis(25));
    assert_eq!(state.lock().unwrap().polls, idle_polls);
    backend.submit(token(1), transfer(&b, 1, 1)).unwrap();
    wait(|| state.lock().unwrap().pending.len() == 1);
    complete(&state, &[1]);
    wait(|| backend.poll_completion().is_some());
    finish(&mut backend, &state, &device);
}

#[test]
fn failed_abort_retries_without_requiring_an_unavailable_completion_wakeup() {
    let (mut backend, state, b, device) = fixture();
    {
        let mut s = state.lock().unwrap();
        s.fail_cancel_count = 1;
        s.complete_on_cancel = true;
    }
    backend.submit(token(1), transfer(&b, 1, 1)).unwrap();
    wait(|| state.lock().unwrap().pending.len() == 1);
    backend.cancel(token(1));
    let mut completion = None;
    wait(|| {
        completion = backend.poll_completion();
        completion.is_some()
    });
    assert_eq!(completion.unwrap().1.status, -125);
    assert_eq!(state.lock().unwrap().fail_cancel_count, 0);
    finish(&mut backend, &state, &device);
}

#[test]
fn topology_barrier_cannot_overtake_an_earlier_busy_queued_request() {
    let (mut backend, state, b, device) = fixture();
    state.lock().unwrap().busy.insert(token(1));
    backend.submit(token(1), transfer(&b, 1, 1)).unwrap();
    let change = Transfer::new(
        &b,
        SubmitHeader {
            seqnum: 2,
            device_id: 0x00011000,
            direction: Direction::Out,
            endpoint: 0,
            transfer_flags: 0,
            transfer_buffer_length: 0,
            start_frame: 0,
            number_of_packets: 0,
            interval: 0,
            setup: [0, 9, 1, 0, 0, 0, 0, 0],
        },
        &[],
    )
    .unwrap();
    backend.submit(token(2), change).unwrap();
    backend.submit(token(3), transfer(&b, 3, 2)).unwrap();
    wait(|| !state.lock().unwrap().attempted.is_empty());
    assert!(state.lock().unwrap().submitted.is_empty());
    {
        let mut s = state.lock().unwrap();
        s.busy.remove(&token(1));
        s.waker.as_ref().unwrap().wake_by_ref();
    }
    wait(|| state.lock().unwrap().submitted == [1]);
    complete(&state, &[1]);
    wait(|| state.lock().unwrap().submitted == [1, 3]);
    assert_eq!(
        state.lock().unwrap().changes,
        [(NativeChange::Configure(1), vec![])]
    );
    finish(&mut backend, &state, &device);
}

#[test]
fn shared_worker_rejects_invalid_requests_before_native_submission() {
    let (mut backend, state, b, device) = fixture();
    for sequence in 1..=3 {
        let valid = transfer(&b, sequence, 1);
        let mut header = *valid.header();
        drop(valid);
        match sequence {
            1 => header.endpoint = 3, // Not selected by the native driver.
            2 => header.device_id += 1,
            3 => header.transfer_flags = u32::MAX,
            _ => unreachable!(),
        }
        backend
            .submit(token(sequence), Transfer::new(&b, header, &[]).unwrap())
            .unwrap();
        let mut done = None;
        wait(|| {
            done = backend.poll_completion();
            done.is_some()
        });
        let (id, done) = done.unwrap();
        assert_eq!(id, token(sequence));
        assert_ne!(done.status, 0);
        assert_eq!(done.actual_length, 0);
        assert!(state.lock().unwrap().attempted.is_empty());
    }
    finish(&mut backend, &state, &device);
}

#[test]
fn cancellation_behind_a_waiting_topology_change_does_not_wait_for_usb_data() {
    let (mut backend, state, b, device) = fixture();
    backend.submit(token(1), transfer(&b, 1, 1)).unwrap();
    wait(|| state.lock().unwrap().submitted == [1]);
    let mut header = *transfer(&b, 2, 0).header();
    header.direction = Direction::Out;
    header.transfer_buffer_length = 0;
    header.setup = [0, 9, 1, 0, 0, 0, 0, 0];
    backend
        .submit(token(2), Transfer::new(&b, header, &[]).unwrap())
        .unwrap();
    for sequence in 3..=SLOTS as u32 {
        backend
            .submit(token(sequence), transfer(&b, sequence, 2))
            .unwrap();
    }
    let peak = b.usage().bytes;
    for sequence in 3..=SLOTS as u32 {
        backend.cancel(token(sequence));
    }
    let mut retired = BTreeSet::new();
    wait(|| {
        assert!(b.usage().bytes <= peak);
        while let Some((id, done)) = backend.poll_completion() {
            assert!((3..=SLOTS as u32).contains(&id.sequence));
            assert!(retired.insert(id.sequence), "duplicate retirement");
            assert_eq!(done.status, usbip::status::CANCELLED);
        }
        retired.len() == SLOTS - 2
    });
    assert_eq!(state.lock().unwrap().submitted, [1]);
    complete(&state, &[1]);
    wait(|| state.lock().unwrap().changes.len() == 1);
    assert_eq!(
        state.lock().unwrap().changes,
        [(NativeChange::Configure(1), vec![])]
    );
    finish(&mut backend, &state, &device);
}

#[test]
fn native_admission_accepts_direction_hint_but_rejects_mismatches_and_unknown_flags() {
    for endpoint in [0, 1] {
        for flags in [0, usbip::DIR_IN] {
            let b = budget();
            let original = transfer(&b, 1, endpoint);
            let mut h = *original.header();
            h.transfer_flags = flags;
            if endpoint == 0 {
                h.setup = [0xc0, 0x50, 0, 0, 0, 0, 8, 0];
            }
            let t = Transfer::new(&b, h, &[]).unwrap();
            let e = Endpoint {
                address: 0x81,
                kind: TransferKind::Bulk,
                max_packet_size: 64,
                service_bytes: 64,
                interval: 0,
                allowed_flags: 0,
            };
            assert_eq!(
                t.validate_native(h.device_id, Some(&e), Fake::CAPABILITIES),
                Ok(if endpoint == 0 { 0 } else { 0x81 })
            );
            h.transfer_flags |= 0x8000_0000;
            let bad = Transfer::new(&b, h, &[]).unwrap();
            assert!(
                bad.validate_native(h.device_id, Some(&e), Fake::CAPABILITIES)
                    .is_err()
            );
            h.transfer_flags = usbip::DIR_IN;
            h.direction = Direction::Out;
            assert!(matches!(Transfer::new(&b, h, &[]), Err(Error::Invalid)));
        }
    }
}

#[test]
fn iso_in_an_inactive_alternate_cannot_bypass_importer_publication_checks() {
    use knightbus_core::topology::Speed;
    let device = [18, 1, 0, 2, 0, 0, 0, 64, 9, 0x12, 1, 0, 0, 1, 0, 0, 0, 1];
    let configuration = [
        9, 2, 34, 0, 1, 1, 0, 0x80, 50, 9, 4, 0, 0, 0, 0xff, 0, 0, 0, 9, 4, 0, 1, 1, 0xff, 0, 0, 0,
        7, 5, 0x81, 5, 64, 0, 1,
    ];
    let topology = descriptors::configuration(&configuration).unwrap();
    assert!(topology.interfaces[0].endpoints.is_empty());
    assert_eq!(
        topology.interfaces[1].endpoints[0].kind,
        TransferKind::Isochronous
    );
    let b = budget();
    let descriptors = DescriptorSet::new(&b, &device, &configuration, &[], Speed::High).unwrap();
    let mut capabilities = Fake::CAPABILITIES;
    capabilities.iso_packets = 0;
    assert_eq!(
        capabilities.descriptors(&descriptors),
        Err(Error::Unsupported)
    );
    capabilities.iso_packets = 1024;
    assert_eq!(capabilities.descriptors(&descriptors), Ok(()));
    drop(descriptors);
    assert_eq!(b.usage().bytes, 0);
}

#[test]
fn iso_pipeline_is_bounded_ordered_and_does_not_block_bulk() {
    let (mut backend, state, b, device) = fixture();
    state.lock().unwrap().iso = true;
    for sequence in 1..=10 {
        let mut header = *transfer(&b, sequence, 1).header();
        header.number_of_packets = 1;
        header.transfer_flags = usbip::ISO_ASAP;
        header.interval = 1;
        let t = Transfer::new(
            &b,
            header,
            &[usbip::IsoPacket {
                offset: 0,
                length: 8,
                actual_length: 0,
                status: 0,
            }],
        )
        .unwrap();
        backend.submit(token(sequence), t).unwrap();
    }
    backend.submit(token(11), transfer(&b, 11, 2)).unwrap();
    wait(|| state.lock().unwrap().pending.len() == 9);
    {
        let mut s = state.lock().unwrap();
        assert_eq!(s.submitted, [1, 2, 3, 4, 5, 6, 7, 8, 11]);
        s.busy.insert(token(9));
        s.ready.insert(token(2));
        s.waker.as_ref().unwrap().wake_by_ref();
    }
    wait(|| state.lock().unwrap().attempted.contains(&9));
    assert!(!state.lock().unwrap().attempted.contains(&10));
    {
        let mut s = state.lock().unwrap();
        s.busy.remove(&token(9));
        s.waker.as_ref().unwrap().wake_by_ref();
    }
    wait(|| state.lock().unwrap().submitted.contains(&9));
    {
        let mut s = state.lock().unwrap();
        assert!(!s.submitted.contains(&10));
        s.complete_on_cancel = true;
    }
    wait(|| matches!(backend.release(SESSION, device.key), Poll::Ready(Ok(()))));
    drop(backend);
    wait(|| b.usage().bytes == 0);
}

#[test]
fn cancel_before_iso_admission_marks_every_packet_failed() {
    let (mut backend, state, b, device) = fixture();
    {
        let mut s = state.lock().unwrap();
        s.iso = true;
        s.busy.insert(token(1));
    }
    let mut header = *transfer(&b, 1, 1).header();
    header.number_of_packets = 2;
    header.transfer_flags = usbip::ISO_ASAP;
    header.interval = 1;
    let packets = [
        usbip::IsoPacket {
            offset: 0,
            length: 4,
            actual_length: 0,
            status: 0,
        },
        usbip::IsoPacket {
            offset: 4,
            length: 4,
            actual_length: 0,
            status: 0,
        },
    ];
    backend
        .submit(token(1), Transfer::new(&b, header, &packets).unwrap())
        .unwrap();
    wait(|| state.lock().unwrap().attempted.contains(&1));
    backend.cancel(token(1));
    let mut result = None;
    wait(|| {
        result = backend.poll_completion();
        result.is_some()
    });
    let (_, done) = result.unwrap();
    assert_eq!(done.status, usbip::status::CANCELLED);
    assert_eq!(done.actual_length, 0);
    assert!(
        done.transfer
            .packets()
            .iter()
            .all(|p| p.status == usbip::status::CANCELLED && p.actual_length == 0)
    );
    drop(done);
    wait(|| matches!(backend.release(SESSION, device.key), Poll::Ready(Ok(()))));
    drop(backend);
    wait(|| b.usage().bytes == 0);
}

fn composite() -> Device {
    let mut device = model();
    let interface = &mut device.configurations[0].interfaces[0];
    let mut other = interface.clone();
    other.number = 1;
    other.endpoints = vec![interface.endpoints.pop().unwrap()];
    other.endpoints[0].address = 0x82;
    device.configurations[0].interfaces.push(other.clone());
    other.alternate = 1;
    device.configurations[0].interfaces.push(other);
    device.wire.interfaces = 2;
    device
}
fn control(b: &Budget, sequence: u32, setup: [u8; 8]) -> Transfer {
    let mut header = *transfer(b, sequence, 0).header();
    header.direction = Direction::Out;
    header.transfer_buffer_length = 0;
    header.setup = setup;
    Transfer::new(b, header, &[]).unwrap()
}
#[test]
fn interface_change_and_clear_halt_do_not_wait_for_another_interfaces_idle_read() {
    for (setup, expected) in [
        ([1, 11, 1, 0, 1, 0, 0, 0], NativeChange::Alternate(1, 1)),
        ([2, 1, 0, 0, 0x82, 0, 0, 0], NativeChange::ClearHalt(0x82)),
    ] {
        let (mut backend, state, b, device) = fixture_for(composite());
        backend.submit(token(1), transfer(&b, 1, 1)).unwrap();
        wait(|| state.lock().unwrap().submitted == [1]);
        backend.submit(token(2), control(&b, 2, setup)).unwrap();
        wait(|| !state.lock().unwrap().changes.is_empty());
        assert_eq!(state.lock().unwrap().changes, [(expected, vec![1])]);
        // New traffic on the changed interface proceeds while the unrelated read lives.
        backend.submit(token(3), transfer(&b, 3, 2)).unwrap();
        wait(|| state.lock().unwrap().submitted == [1, 3]);
        finish(&mut backend, &state, &device);
    }
}
#[test]
fn interface_change_drains_its_own_busy_requests_but_keeps_other_interfaces_running() {
    let (mut backend, state, b, device) = fixture_for(composite());
    state.lock().unwrap().busy.insert(token(1));
    backend.submit(token(1), transfer(&b, 1, 2)).unwrap();
    backend
        .submit(token(2), control(&b, 2, [1, 11, 1, 0, 1, 0, 0, 0]))
        .unwrap();
    backend.submit(token(3), transfer(&b, 3, 1)).unwrap();
    backend.submit(token(4), transfer(&b, 4, 2)).unwrap();
    wait(|| state.lock().unwrap().submitted == [3]);
    assert!(state.lock().unwrap().changes.is_empty());
    {
        let mut s = state.lock().unwrap();
        s.busy.remove(&token(1));
        s.waker.as_ref().unwrap().wake_by_ref();
    }
    wait(|| state.lock().unwrap().submitted == [3, 1]);
    assert!(state.lock().unwrap().changes.is_empty());
    complete(&state, &[1]);
    wait(|| state.lock().unwrap().submitted == [3, 1, 4]);
    assert_eq!(
        state.lock().unwrap().changes,
        [(NativeChange::Alternate(1, 1), vec![3])]
    );
    finish(&mut backend, &state, &device);
}

#[test]
fn invalid_management_requests_never_reach_native_operations() {
    let (mut backend, state, b, device) = fixture_for(composite());
    for (i, setup) in [
        [0, 9, 2, 0, 0, 0, 0, 0],    // Unknown configuration.
        [0, 9, 1, 1, 0, 0, 0, 0],    // Configuration wider than u8.
        [0, 9, 1, 0, 1, 0, 0, 0],    // Reserved index.
        [1, 11, 2, 0, 1, 0, 0, 0],   // Unknown alternate.
        [1, 11, 0, 0, 9, 0, 0, 0],   // Unknown interface.
        [2, 11, 0, 0, 1, 0, 0, 0],   // Wrong recipient.
        [2, 1, 0, 0, 0x83, 0, 0, 0], // Unknown endpoint.
        [2, 1, 0, 0, 0x82, 1, 0, 0], // Endpoint wider than u8.
        [2, 1, 0, 0, 0x92, 0, 0, 0], // Reserved endpoint bits.
        [2, 1, 0, 0, 0, 0, 0, 0],    // Control pipe cannot halt.
        [2, 1, 1, 0, 0x82, 0, 0, 0], // Unknown endpoint feature.
    ]
    .into_iter()
    .enumerate()
    {
        let sequence = i as u32 + 1;
        backend
            .submit(token(sequence), control(&b, sequence, setup))
            .unwrap();
        let mut result = None;
        wait(|| {
            result = backend.poll_completion();
            result.is_some()
        });
        assert_eq!(result.unwrap().1.status, usbip::status::INVALID);
        assert!(state.lock().unwrap().changes.is_empty());
        assert!(state.lock().unwrap().attempted.is_empty());
    }
    finish(&mut backend, &state, &device);
}
#[test]
fn class_and_vendor_requests_with_management_opcodes_stay_ordinary_control() {
    let (mut backend, state, b, device) = fixture();
    for (i, kind) in [0x21, 0x41].into_iter().enumerate() {
        let sequence = i as u32 + 1;
        backend
            .submit(
                token(sequence),
                control(&b, sequence, [kind, 11, 1, 0, 0, 0, 0, 0]),
            )
            .unwrap();
        wait(|| state.lock().unwrap().pending.contains_key(&token(sequence)));
        assert!(state.lock().unwrap().changes.is_empty());
        complete(&state, &[sequence]);
        wait(|| backend.poll_completion().is_some());
    }
    finish(&mut backend, &state, &device);
}
#[test]
fn failed_native_topology_changes_retire_unrelated_io_before_releasing_handles() {
    for setup in [[1, 11, 1, 0, 1, 0, 0, 0], [0, 9, 1, 0, 0, 0, 0, 0]] {
        let (mut backend, state, b, device) = fixture_for(composite());
        {
            let mut s = state.lock().unwrap();
            s.change_error = Some(Error::Unavailable);
            s.complete_on_cancel = true;
        }
        if setup[1] == 11 {
            backend.submit(token(1), transfer(&b, 1, 1)).unwrap();
            wait(|| state.lock().unwrap().pending.contains_key(&token(1)));
        }
        backend.submit(token(2), control(&b, 2, setup)).unwrap();
        wait(|| state.lock().unwrap().released);
        assert_eq!(backend.poll_removed(), Some(device.key));
        if setup[1] == 11 {
            assert!(state.lock().unwrap().cancelled.contains(&token(1)));
        }
        while backend.poll_completion().is_some() {}
        drop(backend);
        wait(|| b.usage().bytes == 0);
    }
}
#[test]
fn failed_clear_halt_preserves_the_device_and_unrelated_transfers() {
    let (mut backend, state, b, device) = fixture_for(composite());
    state.lock().unwrap().change_error = Some(Error::Internal);
    backend.submit(token(1), transfer(&b, 1, 1)).unwrap();
    wait(|| state.lock().unwrap().submitted == [1]);
    backend
        .submit(token(2), control(&b, 2, [2, 1, 0, 0, 0x82, 0, 0, 0]))
        .unwrap();
    let mut result = None;
    wait(|| {
        result = backend.poll_completion();
        result.is_some()
    });
    assert_eq!(result.unwrap().1.status, usbip::status::IO);
    assert_eq!(backend.poll_removed(), None);
    assert!(state.lock().unwrap().pending.contains_key(&token(1)));
    assert!(!state.lock().unwrap().released);
    finish(&mut backend, &state, &device);
}
#[test]
fn clear_halt_reporting_removal_retires_other_interfaces() {
    let (mut backend, state, b, device) = fixture_for(composite());
    {
        let mut s = state.lock().unwrap();
        s.control_status = Some(usbip::status::NO_DEVICE);
        s.complete_on_cancel = true;
    }
    backend.submit(token(1), transfer(&b, 1, 1)).unwrap();
    wait(|| state.lock().unwrap().submitted == [1]);
    backend
        .submit(token(2), control(&b, 2, [2, 1, 0, 0, 0x82, 0, 0, 0]))
        .unwrap();
    wait(|| state.lock().unwrap().released);
    assert_eq!(backend.poll_removed(), Some(device.key));
    assert!(state.lock().unwrap().cancelled.contains(&token(1)));
    let mut results = BTreeMap::new();
    while let Some((id, done)) = backend.poll_completion() {
        results.insert(id.sequence, done.status);
    }
    assert_eq!(results[&2], usbip::status::NO_DEVICE);
    drop(backend);
    wait(|| b.usage().bytes == 0);
}
#[test]
fn cancellation_of_a_queued_change_does_not_touch_native_interface_state() {
    let (mut backend, state, b, device) = fixture_for(composite());
    backend.submit(token(1), transfer(&b, 1, 2)).unwrap();
    wait(|| state.lock().unwrap().submitted == [1]);
    backend
        .submit(token(2), control(&b, 2, [1, 11, 1, 0, 1, 0, 0, 0]))
        .unwrap();
    backend.cancel(token(2));
    let mut result = None;
    wait(|| {
        result = backend.poll_completion();
        result.is_some()
    });
    assert_eq!(result.unwrap().1.status, usbip::status::CANCELLED);
    assert!(state.lock().unwrap().changes.is_empty());
    finish(&mut backend, &state, &device);
}
#[test]
fn unconfigure_is_explicit_and_inactive_configurations_do_not_expand_interface_scope() {
    let mut device = composite();
    let mut unused = device.configurations[0].clone();
    unused.value = 2;
    // This address belongs to interface 0 in configuration 1, but 1 in configuration 2.
    unused.interfaces[1].endpoints[0].address = 0x81;
    device.configurations.push(unused);
    let (mut backend, state, b, device) = fixture_for(device);
    backend.submit(token(1), transfer(&b, 1, 1)).unwrap();
    wait(|| state.lock().unwrap().submitted == [1]);
    backend
        .submit(token(2), control(&b, 2, [1, 11, 1, 0, 1, 0, 0, 0]))
        .unwrap();
    wait(|| state.lock().unwrap().changes.len() == 1);
    complete(&state, &[1]);
    wait(|| !state.lock().unwrap().pending.contains_key(&token(1)));
    backend
        .submit(token(3), control(&b, 3, [0, 9, 0, 0, 0, 0, 0, 0]))
        .unwrap();
    wait(|| state.lock().unwrap().configuration == 0);
    backend
        .submit(token(4), control(&b, 4, [1, 11, 1, 0, 1, 0, 0, 0]))
        .unwrap();
    let mut results = BTreeMap::new();
    wait(|| {
        while let Some((id, done)) = backend.poll_completion() {
            results.insert(id.sequence, done.status);
        }
        results.len() == 4
    });
    assert_eq!(results[&4], usbip::status::INVALID);
    assert_eq!(state.lock().unwrap().changes.len(), 2);
    finish(&mut backend, &state, &device);
}

#[test]
fn native_clear_halt_status_is_preserved_and_invalid_statuses_fail_closed() {
    let (mut backend, state, b, device) = fixture_for(composite());
    for (i, (native, expected)) in [(-32, -32), (-75, -75), (0, -5), (42, -5)]
        .into_iter()
        .enumerate()
    {
        state.lock().unwrap().control_status = Some(native);
        let sequence = i as u32 + 1;
        backend
            .submit(
                token(sequence),
                control(&b, sequence, [2, 1, 0, 0, 0x82, 0, 0, 0]),
            )
            .unwrap();
        let mut result = None;
        wait(|| {
            result = backend.poll_completion();
            result.is_some()
        });
        assert_eq!(result.unwrap().1.status, expected);
        assert_eq!(backend.poll_removed(), None);
    }
    finish(&mut backend, &state, &device);
}
#[test]
fn alternate_change_drains_pipes_from_the_old_setting() {
    let mut device = composite();
    device.configurations[0].interfaces[2].endpoints.clear();
    let (mut backend, state, b, device) = fixture_for(device);
    backend.submit(token(1), transfer(&b, 1, 2)).unwrap();
    wait(|| state.lock().unwrap().submitted == [1]);
    backend
        .submit(token(2), control(&b, 2, [1, 11, 1, 0, 1, 0, 0, 0]))
        .unwrap();
    // A request on the old setting must retire even though the new setting has no pipes.
    backend.submit(token(3), transfer(&b, 3, 1)).unwrap();
    wait(|| state.lock().unwrap().submitted == [1, 3]);
    assert!(state.lock().unwrap().changes.is_empty());
    complete(&state, &[1]);
    wait(|| state.lock().unwrap().changes.len() == 1);
    assert_eq!(state.lock().unwrap().changes[0].1, [3]);
    finish(&mut backend, &state, &device);
}
