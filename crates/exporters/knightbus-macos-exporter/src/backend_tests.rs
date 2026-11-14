//! Scheduling checks with direct visibility of waits, not timing thresholds.
use super::*;
use knightbus_core::{
    DeviceId, ResourceLimits,
    usbip::{Direction, SubmitHeader},
};
use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

struct Immediate {
    pending: Option<(TransferId, Transfer)>,
    wake: Option<Waker>,
}
impl Driver for Immediate {
    fn set_waker(&mut self, waker: Waker) {
        self.wake = Some(waker);
    }
    fn submit(
        &mut self,
        id: TransferId,
        transfer: Transfer,
    ) -> std::result::Result<(), (SubmitError, Transfer)> {
        assert!(self.pending.is_none(), "one active request per pipe");
        self.pending = Some((id, transfer));
        // Models completion inside enqueue, before the worker reaches wait.
        self.wake.as_ref().unwrap().wake_by_ref();
        Ok(())
    }
    fn cancel(&mut self, _: TransferId) -> Result<()> {
        Ok(())
    }
    fn completion(&mut self) -> Option<(TransferId, Completion)> {
        let (id, transfer) = self.pending.take()?;
        Some((
            id,
            Completion {
                transfer,
                status: 0,
                actual_length: 0,
                start_frame: 0,
            },
        ))
    }
    fn removed(&self) -> bool {
        false
    }
    fn release(&mut self) -> Result<()> {
        assert!(self.pending.is_none());
        Ok(())
    }
}

#[test]
fn sixty_four_immediate_completions_drain_without_inter_request_waits() {
    let budget = Budget::new(ResourceLimits {
        bytes: 4 * 1024 * 1024,
        requests: 64,
    });
    let descriptor = [18, 1, 0, 2, 0, 0, 0, 64, 9, 0x12, 1, 0, 0, 1, 0, 0, 0, 1];
    let config = vec![
        9, 2, 25, 0, 1, 1, 0, 0x80, 50, 9, 4, 0, 0, 1, 0xff, 0, 0, 0, 7, 5, 0x81, 2, 64, 0, 0,
    ];
    let device = crate::descriptors::device(
        DeviceKey {
            id: DeviceId(1),
            generation: 1,
        },
        0x01100000,
        2,
        &descriptor,
        &[config],
        1,
    )
    .unwrap();
    let factory = || {
        Ok(Immediate {
            pending: None,
            wake: None,
        })
    };
    let mut backend = Backend::new(device, &budget, factory).unwrap();
    let session = SessionId {
        epoch: 1,
        serial: 1,
    };
    // Preload a complete burst before starting the worker, eliminating host
    // scheduling races from the assertion about inter-request waits.
    backend.session = Some(session);
    backend.lock().state = State::Active;
    for sequence in 1..=SLOTS as u32 {
        let transfer = Transfer::new(
            &budget,
            SubmitHeader {
                seqnum: sequence,
                device_id: 0x00011000,
                direction: Direction::In,
                endpoint: 1,
                transfer_flags: 0,
                transfer_buffer_length: 8,
                start_frame: 0,
                number_of_packets: 0,
                interval: 0,
                setup: [0; 8],
            },
            &[],
        )
        .unwrap();
        backend
            .submit(TransferId { session, sequence }, transfer)
            .unwrap();
    }
    let shared = backend.shared.clone();
    let worker_shared = shared.clone();
    let factory = backend.factory.take().unwrap();
    let start = Instant::now();
    let thread = std::thread::spawn(move || worker(worker_shared, factory));
    while backend
        .lock()
        .slots
        .iter()
        .filter(|s| matches!(s, Slot::Done(..)))
        .count()
        != SLOTS
    {
        assert!(start.elapsed() < Duration::from_secs(3));
        std::thread::yield_now();
    }
    // Zero waits while dispatching the burst; one idle wait is allowed after it.
    assert!(shared.wake.waits.load(Ordering::Relaxed) <= 1);
    eprintln!(
        "64 immediate requests retired in {:?}; no inter-request waits",
        start.elapsed()
    );
    drop(backend);
    thread.join().unwrap();
    drop(shared);
    assert_eq!(budget.usage().bytes, 0);
}

#[test]
fn notification_before_wait_is_latched() {
    let wake = Arc::new(Notification::default());
    let cloned = wake.clone();
    let (tx, rx) = std::sync::mpsc::channel();
    let thread = std::thread::spawn(move || {
        cloned.clear();
        Waker::from(cloned.clone()).wake();
        cloned.wait(false);
        tx.send(()).unwrap();
    });
    rx.recv_timeout(Duration::from_secs(3)).unwrap();
    thread.join().unwrap();
    assert_eq!(wake.waits.load(Ordering::Relaxed), 0);
}
