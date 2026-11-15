//! Fault-injected ownership, admission, ordering and retirement regressions.
use knightbus_core::{
    descriptors,
    exporter::{Backend, Driver, SubmitError},
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
}
struct Fake(Arc<Mutex<State>>);
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
    fn endpoint(&self, address: u8) -> Option<knightbus_core::Endpoint> {
        if !matches!(address, 0x81 | 0x82) {
            return None;
        }
        Some(knightbus_core::Endpoint {
            address,
            kind: knightbus_core::TransferKind::Bulk,
            max_packet_size: 64,
            interval: 0,
            allowed_flags: 0,
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
        if let Some(bytes) = s.scratch_bytes.get(&id) {
            if let Err(error) = t.reserve_native_memory(*bytes) {
                return Err((SubmitError::Failed(error), t));
            }
        }
        if s.pending
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
    let state = Arc::new(Mutex::new(State::default()));
    let copied = state.clone();
    let b = budget();
    let device = model();
    let factory: Factory = Box::new(move || Ok(Fake(copied)));
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
    wait(|| state.lock().unwrap().submitted.len() >= 2);
    std::thread::sleep(Duration::from_millis(10));
    // The barrier itself must retire before later traffic may be offered.
    assert_eq!(state.lock().unwrap().submitted, [1, 2]);
    {
        let mut s = state.lock().unwrap();
        s.ready.insert(token(2));
        s.waker.as_ref().unwrap().wake_by_ref();
    }
    wait(|| state.lock().unwrap().submitted == [1, 2, 3]);
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
    wait(|| state.lock().unwrap().submitted == [1, 2]);
    complete(&state, &[2]);
    wait(|| state.lock().unwrap().submitted == [1, 2, 3]);
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
