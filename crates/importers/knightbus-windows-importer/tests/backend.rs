//! Native ownership, bounded admission and independent descriptor validation.
use knightbus_core::{
    Budget, Completion, DescriptorSet as Descriptors, Device, DeviceId, DeviceKey, Error,
    ImporterBackend, ResourceLimits, SessionId, usbip,
};
use knightbus_windows_importer::{
    backend::{self, Driver},
    channel::{MAX_TRANSFER, SLOTS},
    descriptors::{Speed, validate},
};
use std::{
    collections::VecDeque,
    sync::{
        Arc, Condvar, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    task::Poll,
    time::{Duration, Instant},
};
struct HostWake(std::sync::atomic::AtomicUsize);
impl std::task::Wake for HostWake {
    fn wake(self: Arc<Self>) {
        self.0.fetch_add(1, Ordering::Relaxed);
    }
    fn wake_by_ref(self: &Arc<Self>) {
        self.0.fetch_add(1, Ordering::Relaxed);
    }
}
const DEVICE: [u8; 18] = [18, 1, 0, 2, 0, 0, 0, 64, 0x09, 0x12, 1, 0, 0, 1, 0, 0, 0, 1];
const CONFIG: [u8; 32] = [
    9, 2, 32, 0, 1, 1, 0, 0x80, 50, 9, 4, 0, 0, 2, 0xff, 0, 0, 0, 7, 5, 2, 2, 64, 0, 0, 7, 5, 0x82,
    2, 64, 0, 0,
];
const SESSION: SessionId = SessionId {
    epoch: 9,
    serial: 1,
};
fn budget() -> Budget {
    Budget::new(ResourceLimits {
        bytes: 64 * 1024 * 1024,
        requests: 64,
    })
}
fn device() -> Device {
    Device {
        key: DeviceKey {
            id: DeviceId(1),
            generation: 1,
        },
        wire: usbip::Device {
            path: usbip::WireString::new(b"test").unwrap(),
            bus_id: DeviceId(1).bus_id(),
            bus_number: 1,
            device_number: 2,
            speed: 2,
            vendor_id: 0x1209,
            product_id: 1,
            device_bcd: 0x100,
            class: 0,
            subclass: 0,
            protocol: 0,
            configuration: 1,
            configurations: 1,
            interfaces: 1,
        },
        configurations: vec![],
    }
}
fn wait(mut condition: impl FnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(3);
    while !condition() {
        assert!(Instant::now() < deadline, "worker deadline");
        std::thread::sleep(Duration::from_millis(1));
    }
}
#[derive(Default)]
struct Lab {
    wake: Option<Arc<knightbus_windows_importer::DriverWake>>,
    requests: VecDeque<Vec<u8>>,
    active: [u64; SLOTS],
    replies: Vec<Vec<u8>>,
    attached: bool,
    detached: bool,
    cleanup_error: Option<Error>,
    snapshots: usize,
    fetches: Vec<(usize, u8)>,
    repeat: bool,
}
impl Lab {
    fn notify(&self) {
        if let Some(wake) = &self.wake {
            wake.notify();
        }
    }
    fn push(&mut self, request: Vec<u8>) {
        self.requests.push_back(request);
        self.notify();
    }
}
struct Mock {
    lab: Arc<Mutex<Lab>>,
    release: Arc<(Mutex<bool>, Condvar)>,
    entered: Arc<AtomicBool>,
}
fn mock_slot(endpoint: u8) -> usize {
    match endpoint {
        0 => 0,
        2 => 1,
        0x82 => 2,
        _ => panic!("unexpected mock endpoint"),
    }
}
impl Driver for Mock {
    fn notifications(
        &mut self,
        wake: Arc<knightbus_windows_importer::DriverWake>,
    ) -> knightbus_core::Result<()> {
        wake.notify();
        self.lab.lock().unwrap().wake = Some(wake);
        Ok(())
    }
    fn attach(&mut self, b: &[u8]) -> knightbus_core::Result<()> {
        assert_eq!(&b[..4], b"KBWD");
        self.lab.lock().unwrap().attached = true;
        Ok(())
    }
    fn fetch(&mut self, mask: u32, out: &mut [u8]) -> knightbus_core::Result<usize> {
        assert_eq!(
            out.len(),
            32 + MAX_TRANSFER,
            "non-ISO keeps its original IOCTL size"
        );
        let mut l = self.lab.lock().unwrap();
        let Some(position) = l
            .requests
            .iter()
            .position(|request| mask & (1 << mock_slot(request[4])) != 0)
        else {
            return Ok(0);
        };
        let b = l.requests.remove(position).unwrap();
        let turn = l.snapshots;
        l.fetches.push((turn, b[4]));
        let i = mock_slot(b[4]);
        let n = b.len();
        out[..n].copy_from_slice(&b);
        l.active[i] = u64::from_le_bytes(b[8..16].try_into().unwrap());
        Ok(n)
    }
    fn snapshot(&mut self, b: &mut [u8]) -> knightbus_core::Result<()> {
        b.fill(0);
        b[..8].copy_from_slice(&1u64.to_le_bytes());
        let mut l = self.lab.lock().unwrap();
        l.snapshots += 1;
        let Lab {
            requests, active, ..
        } = &mut *l;
        for request in requests {
            let index = mock_slot(request[4]);
            active[index] = u64::from_le_bytes(request[8..16].try_into().unwrap());
        }
        let mask = l
            .active
            .iter()
            .enumerate()
            .fold(0u64, |m, (i, t)| m | if *t != 0 { 1 << i } else { 0 });
        b[56..64].copy_from_slice(&mask.to_le_bytes());
        for (i, t) in l.active.iter().enumerate() {
            b[64 + i * 8..72 + i * 8].copy_from_slice(&t.to_le_bytes());
        }
        Ok(())
    }
    fn reply(&mut self, b: &[u8]) -> knightbus_core::Result<()> {
        let mut l = self.lab.lock().unwrap();
        l.active[mock_slot(b[4])] = 0;
        if l.repeat {
            let token = u64::from_le_bytes(b[8..16].try_into().unwrap()) + 2;
            l.requests.push_back(request(b[4], token));
        }
        l.replies.push(b.to_vec());
        Ok(())
    }
    fn detach(&mut self) -> knightbus_core::Result<()> {
        self.entered.store(true, Ordering::Release);
        let (lock, cv) = &*self.release;
        let mut released = lock.lock().unwrap();
        while !*released {
            released = cv.wait(released).unwrap();
        }
        let mut lab = self.lab.lock().unwrap();
        lab.wake = None;
        if let Some(error) = lab.cleanup_error {
            return Err(error);
        }
        lab.detached = true;
        Ok(())
    }
}
#[test]
fn continuously_ready_pipes_are_batched_and_delivered_fairly() {
    let b = budget();
    let lab = Arc::new(Mutex::new(Lab {
        requests: [request(2, 1), request(0x82, 2)].into(),
        repeat: true,
        ..Lab::default()
    }));
    let worker = lab.clone();
    let descriptors = Descriptors::new(&b, &DEVICE, &CONFIG, &[], Speed::Full).unwrap();
    let mut backend = backend::new(
        move || {
            Ok(Mock {
                lab: worker,
                release: Arc::new((Mutex::new(true), Condvar::new())),
                entered: Arc::new(AtomicBool::new(false)),
            })
        },
        descriptors,
        b.clone(),
    )
    .unwrap();
    wait(|| matches!(backend.attach(SESSION, &device()), Poll::Ready(Ok(()))));
    let mut counts = [0; 2];
    for iteration in 0..100 {
        // Let both pipes become available before each single host dequeue.
        // Completing a low pipe immediately replenishes it, reproducing the
        // starvation of the former lowest-index mailbox selection.
        wait(|| {
            let lab = lab.lock().unwrap();
            lab.fetches.len() >= iteration + 2
                && lab.requests.is_empty()
                && lab.snapshots > lab.fetches.last().unwrap().0
        });
        let transfer = backend.next_transfer(SESSION, &b).unwrap().unwrap();
        let index = usize::from(transfer.header().direction == usbip::Direction::In);
        counts[index] += 1;
        backend
            .complete(
                SESSION,
                Completion {
                    transfer,
                    status: 0,
                    actual_length: 4,
                    start_frame: 0,
                },
            )
            .unwrap();
    }
    assert_eq!(
        counts,
        [50, 50],
        "a continuously ready pipe must not starve"
    );
    {
        let lab = lab.lock().unwrap();
        assert_eq!(
            lab.fetches[0].0, lab.fetches[1].0,
            "both ready endpoints must be fetched before the next polling turn"
        );
        assert_eq!([lab.fetches[0].1, lab.fetches[1].1], [2, 0x82]);
    }
    wait(|| matches!(backend.detach(SESSION), Poll::Ready(Ok(()))));
    drop(backend);
    wait(|| b.usage().bytes == 0);
    assert_eq!(b.usage().requests, 0);
}
#[test]
fn busy_pipe_completions_progress_and_idle_worker_backs_off() {
    let b = budget();
    let lab = Arc::new(Mutex::new(Lab {
        requests: [request(2, 1)].into(),
        repeat: true,
        ..Lab::default()
    }));
    let worker = lab.clone();
    let descriptors = Descriptors::new(&b, &DEVICE, &CONFIG, &[], Speed::Full).unwrap();
    let mut backend = backend::new(
        move || {
            Ok(Mock {
                lab: worker,
                release: Arc::new((Mutex::new(true), Condvar::new())),
                entered: Arc::new(AtomicBool::new(false)),
            })
        },
        descriptors,
        b.clone(),
    )
    .unwrap();
    let host = Arc::new(HostWake(std::sync::atomic::AtomicUsize::new(0)));
    backend.set_waker(Some(std::task::Waker::from(host.clone())));
    wait(|| matches!(backend.attach(SESSION, &device()), Poll::Ready(Ok(()))));
    let start = Instant::now();
    for sequence in 1..=256 {
        let transfer = loop {
            if let Some(transfer) = backend.next_transfer(SESSION, &b) {
                break transfer.unwrap();
            }
            assert!(start.elapsed() < Duration::from_secs(5));
            std::thread::yield_now();
        };
        assert_eq!(transfer.header().seqnum, sequence);
        assert_eq!(transfer.buffer(), [42; 4]);
        if sequence == 256 {
            lab.lock().unwrap().repeat = false;
        }
        backend
            .complete(
                SESSION,
                Completion {
                    transfer,
                    status: 0,
                    actual_length: 4,
                    start_frame: 0,
                },
            )
            .unwrap();
    }
    wait(|| lab.lock().unwrap().replies.len() == 256);
    assert!(
        host.0.load(Ordering::Relaxed) > 1,
        "native requests must wake the host"
    );
    backend.set_waker(None);
    // Report timing for local before/after comparison, not a flaky CI threshold.
    eprintln!("256 immediate pipe completions: {:?}", start.elapsed());
    let snapshots = lab.lock().unwrap().snapshots;
    std::thread::sleep(Duration::from_millis(20));
    let turns = lab.lock().unwrap().snapshots - snapshots;
    assert!(
        turns <= 2,
        "no pending work: worker must park, not busy-spin"
    );
    drop(backend); // Shutdown must also wake an idle worker.
    wait(|| lab.lock().unwrap().detached);
    wait(|| b.usage().bytes == 0);
    assert_eq!(b.usage().requests, 0);
}
fn request(endpoint: u8, token: u64) -> Vec<u8> {
    let payload = if endpoint & 0x80 == 0 { 4 } else { 0 };
    let mut b = vec![0; 32 + payload];
    for (o, v) in [
        (0, 1u32),
        (4, endpoint as u32),
        (16, 4),
        (20, payload as u32),
    ] {
        b[o..o + 4].copy_from_slice(&v.to_le_bytes());
    }
    b[8..16].copy_from_slice(&token.to_le_bytes());
    b[32..].fill(42);
    b
}
#[test]
fn cleanup_failure_is_not_hidden_by_an_earlier_native_response_error() {
    let b = budget();
    let lab = Arc::new(Mutex::new(Lab {
        cleanup_error: Some(Error::Unavailable),
        ..Lab::default()
    }));
    let worker = lab.clone();
    let descriptors = Descriptors::new(&b, &DEVICE, &CONFIG, &[], Speed::Full).unwrap();
    let mut backend = backend::new(
        move || {
            Ok(Mock {
                lab: worker,
                release: Arc::new((Mutex::new(true), Condvar::new())),
                entered: Arc::new(AtomicBool::new(false)),
            })
        },
        descriptors,
        b.clone(),
    )
    .unwrap();
    wait(|| matches!(backend.attach(SESSION, &device()), Poll::Ready(Ok(()))));
    let mut invalid = request(2, 1);
    invalid[0] = 0; // Invalid driver ABI version after native publication.
    lab.lock().unwrap().push(invalid);
    wait(|| {
        matches!(
            backend.next_transfer(SESSION, &b),
            Some(Err(Error::Invalid))
        )
    });
    assert_eq!(backend.detach(SESSION), Poll::Ready(Err(Error::Cleanup)));
    drop(backend);
    assert!(
        b.usage().bytes >= SLOTS * MAX_TRANSFER,
        "unretired native storage must stay charged after worker exit"
    );
    assert_eq!(b.usage().requests, 0);
}
#[test]
fn concurrent_pipes_frozen_identity_duplicate_completion_and_cancellation() {
    let b = budget();
    let lab = Arc::new(Mutex::new(Lab::default()));
    let worker = lab.clone();
    let release = Arc::new((Mutex::new(true), Condvar::new()));
    let entered = Arc::new(AtomicBool::new(false));
    let descriptors = Descriptors::new(&b, &DEVICE, &CONFIG, &[], Speed::Full).unwrap();
    let mut backend = backend::new(
        move || {
            Ok(Mock {
                lab: worker,
                release,
                entered,
            })
        },
        descriptors,
        b.clone(),
    )
    .unwrap();
    wait(|| matches!(backend.attach(SESSION, &device()), Poll::Ready(Ok(()))));
    lab.lock()
        .unwrap()
        .requests
        .extend([request(2, 1), request(0x82, 2)]);
    lab.lock().unwrap().notify();
    wait(|| {
        let lab = lab.lock().unwrap();
        lab.active[1] == 1 && lab.requests.is_empty()
    });
    let snapshots = {
        let mut lab = lab.lock().unwrap();
        lab.active[1] = 0;
        lab.notify();
        lab.snapshots
    };
    wait(|| lab.lock().unwrap().snapshots >= snapshots + 2);
    assert_eq!(
        backend.next_cancellation(SESSION),
        None,
        "cancellation cannot precede delivery of its transfer owner"
    );
    let mut transfers = vec![];
    wait(|| {
        while let Some(t) = backend.next_transfer(SESSION, &b) {
            transfers.push(t.unwrap());
        }
        transfers.len() == 2
    });
    assert_eq!(b.usage().requests, 2);
    let mut input = transfers.pop().unwrap();
    let output = transfers.pop().unwrap();
    assert_eq!(output.buffer(), [42; 4]);
    input.buffer_mut().copy_from_slice(&[1, 2, 3, 4]);
    let header = *input.header();
    assert!(
        backend
            .complete(
                SessionId {
                    epoch: 10,
                    ..SESSION
                },
                Completion {
                    transfer: input,
                    status: 0,
                    actual_length: 4,
                    start_frame: 0
                }
            )
            .is_err()
    );
    // The rejected owner is dropped by the caller; this unrelated forged header
    // must never reach native memory even if its byte count looks plausible.
    let mut wrong = header;
    wrong.device_id += 1;
    let forged = knightbus_core::Transfer::new(&b, wrong, &[]).unwrap();
    assert!(
        backend
            .complete(
                SESSION,
                Completion {
                    transfer: forged,
                    status: 0,
                    actual_length: 4,
                    start_frame: 0
                }
            )
            .is_err()
    );
    let input = knightbus_core::Transfer::new(&b, header, &[]).unwrap();
    backend
        .complete(
            SESSION,
            Completion {
                transfer: input,
                status: 0,
                actual_length: 4,
                start_frame: 0,
            },
        )
        .unwrap();
    let duplicate = knightbus_core::Transfer::new(&b, header, &[]).unwrap();
    assert!(
        backend
            .complete(
                SESSION,
                Completion {
                    transfer: duplicate,
                    status: 0,
                    actual_length: 4,
                    start_frame: 0
                }
            )
            .is_err()
    );
    lab.lock().unwrap().active[1] = 0;
    lab.lock().unwrap().notify();
    let mut cancellation = None;
    wait(|| {
        cancellation = backend.next_cancellation(SESSION);
        cancellation.is_some()
    });
    assert_eq!(cancellation, Some(output.header().seqnum));
    assert_eq!(backend.next_cancellation(SESSION), None);
    backend
        .complete(
            SESSION,
            Completion {
                transfer: output,
                status: -104,
                actual_length: 0,
                start_frame: 0,
            },
        )
        .unwrap();
    wait(|| lab.lock().unwrap().replies.len() == 2);
    wait(|| matches!(backend.detach(SESSION), Poll::Ready(Ok(()))));
    drop(backend);
    wait(|| b.usage().bytes == 0);
    assert_eq!(b.usage().requests, 0);
}
#[test]
fn alternate_settings_follow_successful_controls_and_configuration_resets() {
    // The bulk endpoints exist only at alternate 1, like a CDC NCM data interface.
    let config = [
        9, 2, 41, 0, 1, 1, 0, 0x80, 50, 9, 4, 0, 0, 0, 0xff, 0, 0, 0, 9, 4, 0, 1, 2, 0xff, 0, 0, 0,
        7, 5, 2, 2, 64, 0, 0, 7, 5, 0x82, 2, 64, 0, 0,
    ];
    for (status, reset_configuration) in [(0, false), (-32, false), (0, true)] {
        let b = budget();
        let lab = Arc::new(Mutex::new(Lab::default()));
        let worker = lab.clone();
        let descriptors = Descriptors::new(&b, &DEVICE, &config, &[], Speed::Full).unwrap();
        let mut backend = backend::new(
            move || {
                Ok(Mock {
                    lab: worker,
                    release: Arc::new((Mutex::new(true), Condvar::new())),
                    entered: Arc::new(AtomicBool::new(false)),
                })
            },
            descriptors,
            b.clone(),
        )
        .unwrap();
        wait(|| matches!(backend.attach(SESSION, &device()), Poll::Ready(Ok(()))));
        if !reset_configuration {
            // UDE can queue new-pipe work before delivering SET_INTERFACE.
            lab.lock().unwrap().push(request(0x82, 3));
        }
        let controls = if reset_configuration { 2 } else { 1 };
        for step in 0..controls {
            let setup = if step == 0 {
                [1, 11, 1, 0, 0, 0, 0, 0]
            } else {
                [0, 9, 1, 0, 0, 0, 0, 0]
            };
            let mut control = vec![0; 32];
            control[..4].copy_from_slice(&1u32.to_le_bytes());
            control[8..16].copy_from_slice(&(step as u64 + 1).to_le_bytes());
            control[24..32].copy_from_slice(&setup);
            lab.lock().unwrap().push(control);
            let mut transfer = None;
            wait(|| {
                transfer = backend.next_transfer(SESSION, &b);
                transfer.is_some()
            });
            let transfer = transfer.unwrap().unwrap();
            assert_eq!(transfer.header().setup, setup);
            assert!(backend.next_transfer(SESSION, &b).is_none());
            backend
                .complete(
                    SESSION,
                    Completion {
                        transfer,
                        status,
                        actual_length: 0,
                        start_frame: 0,
                    },
                )
                .unwrap();
            wait(|| lab.lock().unwrap().replies.len() == step + 1);
        }
        if status == 0 && !reset_configuration {
            let mut transfer = None;
            wait(|| {
                transfer = backend.next_transfer(SESSION, &b);
                transfer.is_some()
            });
            let transfer = transfer.unwrap().unwrap();
            assert_eq!(transfer.header().endpoint, 2);
            assert_eq!(transfer.header().interval, 0);
            backend
                .complete(
                    SESSION,
                    Completion {
                        transfer,
                        status: 0,
                        actual_length: 0,
                        start_frame: 0,
                    },
                )
                .unwrap();
            wait(|| lab.lock().unwrap().replies.len() == 2);
        } else {
            if reset_configuration {
                lab.lock().unwrap().push(request(0x82, 3));
            }
            let snapshots = {
                let lab = lab.lock().unwrap();
                lab.notify();
                lab.snapshots
            };
            wait(|| lab.lock().unwrap().snapshots > snapshots);
            assert!(
                backend.next_transfer(SESSION, &b).is_none(),
                "an inactive endpoint must not reach the remote device"
            );
            assert_eq!(b.usage().requests, 0);
        }
        wait(|| matches!(backend.detach(SESSION), Poll::Ready(Ok(()))));
        drop(backend);
        wait(|| b.usage().bytes == 0);
        assert_eq!(b.usage().requests, 0);
    }
}
#[test]
fn drop_retains_credits_until_blocked_native_cleanup_retires() {
    let b = budget();
    let lab = Arc::new(Mutex::new(Lab::default()));
    let release = Arc::new((Mutex::new(false), Condvar::new()));
    let unlock = release.clone();
    let entered = Arc::new(AtomicBool::new(false));
    let observed = entered.clone();
    let d = Descriptors::new(&b, &DEVICE, &CONFIG, &[], Speed::Full).unwrap();
    let mut backend = backend::new(
        move || {
            Ok(Mock {
                lab,
                release,
                entered,
            })
        },
        d,
        b.clone(),
    )
    .unwrap();
    wait(|| matches!(backend.attach(SESSION, &device()), Poll::Ready(Ok(()))));
    let start = Instant::now();
    drop(backend);
    assert!(start.elapsed() < Duration::from_millis(200));
    wait(|| observed.load(Ordering::Acquire));
    assert!(b.usage().bytes > SLOTS * MAX_TRANSFER);
    *unlock.0.lock().unwrap() = true;
    unlock.1.notify_all();
    wait(|| b.usage().bytes == 0);
}
#[test]
fn native_admission_fails_before_opening_driver() {
    let b = Budget::new(ResourceLimits {
        bytes: 100000,
        requests: 1,
    });
    let opened = Arc::new(AtomicBool::new(false));
    let check = opened.clone();
    let d = Descriptors::new(&b, &DEVICE, &CONFIG, &[], Speed::Full).unwrap();
    let mut backend = backend::new::<Mock>(
        move || {
            opened.store(true, Ordering::Release);
            Err(Error::Unavailable)
        },
        d,
        b.clone(),
    )
    .unwrap();
    wait(|| {
        matches!(
            backend.attach(SESSION, &device()),
            Poll::Ready(Err(Error::Exhausted))
        )
    });
    assert!(!check.load(Ordering::Acquire));
    drop(backend);
    assert_eq!(b.usage().bytes, 0);
}
#[test]
fn descriptors_reject_truncation_collisions_and_accept_composite_alternates() {
    for n in 0..DEVICE.len() {
        assert!(validate(&DEVICE[..n], &CONFIG, Speed::Full).is_err());
    }
    for n in 0..CONFIG.len() {
        assert!(validate(&DEVICE, &CONFIG[..n], Speed::Full).is_err());
    }
    let mut c = CONFIG.to_vec();
    c.extend([9, 4, 0, 1, 1, 0xff, 0, 0, 0, 7, 5, 2, 2, 32, 0, 0]);
    c.extend([9, 4, 1, 0, 0, 0xff, 0, 0, 0]);
    c[4] = 2;
    let length = c.len() as u16;
    c[2..4].copy_from_slice(&length.to_le_bytes());
    assert_eq!(
        validate(&DEVICE, &c, Speed::Full)
            .unwrap()
            .settings
            .iter()
            .flatten()
            .count(),
        3
    );
    let mut duplicate = CONFIG;
    duplicate[27] = 2;
    assert!(validate(&DEVICE, &duplicate, Speed::Full).is_err());
    let mut iso = CONFIG;
    iso[21] = 1;
    iso[24] = 1;
    assert!(validate(&DEVICE, &iso, Speed::Full).is_ok());
    let mut wake = CONFIG;
    wake[7] |= 0x20;
    assert!(validate(&DEVICE, &wake, Speed::Full).is_err());
    assert!(validate(&DEVICE, &CONFIG, Speed::Low).is_err());
    assert!(validate(&DEVICE, &CONFIG, Speed::High).is_err());
}

#[test]
fn iso_in_active_or_inactive_alternate_reaches_native_open() {
    for address in [2, 0x82] {
        for alternate in [0, 1] {
            let b = budget();
            let mut config = vec![9, 2, 0, 0, 1, 1, 0, 0x80, 50];
            if alternate != 0 {
                config.extend([9, 4, 0, 0, 1, 0xff, 0, 0, 0, 7, 5, address, 2, 64, 0, 0]);
            }
            config.extend([
                9, 4, 0, alternate, 1, 0xff, 0, 0, 0, 7, 5, address, 1, 64, 0, 1,
            ]);
            config[2] = config.len() as u8;
            let d = Descriptors::new(&b, &DEVICE, &config, &[], Speed::Full).unwrap();
            let opened = Arc::new(AtomicBool::new(false));
            let check = opened.clone();
            let mut backend = backend::new::<Mock>(
                move || {
                    opened.store(true, Ordering::Release);
                    Err(Error::Unavailable)
                },
                d,
                b.clone(),
            )
            .unwrap();
            wait(|| {
                matches!(
                    backend.attach(SESSION, &device()),
                    Poll::Ready(Err(Error::Unavailable))
                )
            });
            assert!(check.load(Ordering::Acquire));
            drop(backend);
            wait(|| b.usage().bytes == 0);
            assert_eq!(b.usage().requests, 0);
        }
    }
}
