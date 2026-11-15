//! Native adapter contract exercised without an OS driver or private wire ABI.
use knightbus_core::{
    importer::{self, Driver, Signal},
    topology::Speed,
    *,
};
use std::{
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    task::Poll,
    time::{Duration, Instant},
};
const DEVICE: [u8; 18] = [18, 1, 0, 2, 0, 0, 0, 64, 9, 0x12, 1, 0, 0, 1, 0, 0, 0, 1];
const CONFIG: [u8; 25] = [
    9, 2, 25, 0, 1, 1, 0, 0x80, 50, 9, 4, 0, 0, 1, 0xff, 0, 0, 0, 7, 5, 0x81, 3, 8, 0, 1,
];
const SESSION: SessionId = SessionId {
    epoch: 1,
    serial: 1,
};
#[derive(Default)]
struct Lab {
    wake: Option<Arc<ThreadSignal>>,
    token: u64,
    fetched: bool,
    invalid: bool,
    snapshots: usize,
    detached: bool,
    iso_address: u8,
    completed: Option<Observed>,
}
struct Observed {
    status: i32,
    actual: u32,
    bytes: Vec<u8>,
    packets: Vec<usbip::IsoPacket>,
}
const PACKETS: [usbip::IsoPacket; 2] = [
    usbip::IsoPacket {
        offset: 0,
        length: 4,
        actual_length: 0,
        status: 0,
    },
    usbip::IsoPacket {
        offset: 12,
        length: 4,
        actual_length: 0,
        status: 0,
    },
];
struct Native(Arc<Mutex<Lab>>);
impl Driver for Native {
    type Signal = ThreadSignal;
    const CAPABILITIES: Capabilities = Capabilities {
        transfer_bytes: 256,
        requests: 2,
        iso_packets: 2,
        speeds: 1 << 2,
        descriptor_bytes: 4096,
        configurations: 1,
        remote_wakeup: false,
    };
    const NATIVE_BYTES: usize = 128;
    const SCRATCH_BYTES: usize = 256;
    fn attach(&mut self, _: &DescriptorSet, _: &Budget) -> Result<()> {
        Ok(())
    }
    fn fetch<'a>(
        &'a mut self,
        mask: u32,
        _: &topology::Topology,
        scratch: &'a mut [u8],
    ) -> Result<Option<importer::Request<'a>>> {
        let mut lab = self.0.lock().unwrap();
        if lab.token == 0 || lab.fetched || mask & 2 == 0 {
            return Ok(None);
        }
        lab.fetched = true;
        let iso = lab.iso_address != 0;
        scratch[..16].fill(0x42);
        Ok(Some(importer::Request {
            slot: 1,
            packets: if iso { &PACKETS } else { &[] },
            flags: if iso { usbip::ISO_ASAP } else { 0 },
            start_frame: 0,
            token: if lab.invalid { 0 } else { lab.token },
            endpoint: if iso { lab.iso_address } else { 0x81 },
            length: if iso { 16 } else { 8 },
            setup: [0; 8],
            payload: if iso && lab.iso_address & 0x80 == 0 {
                &scratch[..16]
            } else {
                &[]
            },
        }))
    }
    fn complete(&mut self, token: u64, done: &Completion, _: &mut [u8]) -> Result<()> {
        let mut lab = self.0.lock().unwrap();
        assert!(lab.token == token || lab.token == 0);
        lab.token = 0;
        lab.completed = Some(Observed {
            status: done.status,
            actual: done.actual_length,
            bytes: done.transfer.buffer().to_vec(),
            packets: done.transfer.packets().to_vec(),
        });
        Ok(())
    }
    fn snapshot(&mut self) -> Result<importer::Snapshot> {
        let mut lab = self.0.lock().unwrap();
        lab.snapshots += 1;
        let mut tokens = [0; 31];
        tokens[1] = lab.token;
        Ok(importer::Snapshot {
            live: if lab.token == 0 { 0 } else { 2 },
            tokens,
        })
    }
    fn notifications(&mut self, wake: Arc<ThreadSignal>) -> Result<()> {
        let mut lab = self.0.lock().unwrap();
        lab.wake = Some(wake.clone());
        wake.notify();
        Ok(())
    }
    fn detach(&mut self) -> Result<()> {
        let mut lab = self.0.lock().unwrap();
        lab.wake = None;
        lab.detached = true;
        Ok(())
    }
}
fn wait(mut ready: impl FnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(3);
    while !ready() {
        assert!(Instant::now() < deadline, "native contract timeout");
        std::thread::yield_now();
    }
}
fn budget() -> Budget {
    Budget::new(ResourceLimits {
        bytes: 1024 * 1024,
        requests: 8,
    })
}
fn device() -> Device {
    descriptors::device(
        DeviceKey {
            id: DeviceId(1),
            generation: 1,
        },
        descriptors::Address {
            path: b"test",
            bus: 1,
            device: 1,
        },
        2,
        &DEVICE,
        &[CONFIG.to_vec()],
        1,
    )
    .unwrap()
}

#[test]
fn cancellation_before_delivery_keeps_the_owner() {
    let budget = budget();
    let lab = Arc::new(Mutex::new(Lab {
        token: 1,
        ..Default::default()
    }));
    let state = lab.clone();
    let descriptors = DescriptorSet::new(&budget, &DEVICE, &CONFIG, &[], Speed::Full).unwrap();
    let mut backend =
        importer::Backend::new(move || Ok(Native(state)), descriptors, budget.clone()).unwrap();
    wait(|| matches!(backend.attach(SESSION, &device()), Poll::Ready(Ok(()))));
    wait(|| lab.lock().unwrap().fetched);
    let snapshots = {
        let mut lab = lab.lock().unwrap();
        lab.token = 0;
        lab.wake.as_ref().unwrap().notify();
        lab.snapshots
    };
    wait(|| lab.lock().unwrap().snapshots >= snapshots + 2);
    assert_eq!(backend.next_cancellation(SESSION), None);
    let transfer = backend.next_transfer(SESSION, &budget).unwrap().unwrap();
    assert_eq!(
        backend.next_cancellation(SESSION),
        Some(transfer.header().seqnum)
    );
    assert_eq!(backend.next_cancellation(SESSION), None);
    backend
        .complete(
            SESSION,
            Completion {
                transfer,
                status: -104,
                actual_length: 0,
                start_frame: 0,
            },
        )
        .unwrap();
    drop(backend);
    wait(|| lab.lock().unwrap().detached);
    wait(|| budget.usage().bytes == 0);
}
#[test]
fn malformed_native_request_fails_closed_and_retires() {
    let budget = budget();
    let lab = Arc::new(Mutex::new(Lab {
        token: 1,
        invalid: true,
        ..Default::default()
    }));
    let state = lab.clone();
    let descriptors = DescriptorSet::new(&budget, &DEVICE, &CONFIG, &[], Speed::Full).unwrap();
    let mut backend =
        importer::Backend::new(move || Ok(Native(state)), descriptors, budget.clone()).unwrap();
    let _ = backend.attach(SESSION, &device());
    wait(|| {
        matches!(
            backend.next_transfer(SESSION, &budget),
            Some(Err(Error::Invalid))
        )
    });
    wait(|| matches!(backend.detach(SESSION), Poll::Ready(Ok(()))));
    drop(backend);
    wait(|| budget.usage().bytes == 0);
}
#[test]
fn unsupported_device_is_rejected_before_native_factory_runs() {
    let budget = budget();
    let mut config = CONFIG;
    config[7] |= 0x20;
    let descriptors = DescriptorSet::new(&budget, &DEVICE, &config, &[], Speed::Full).unwrap();
    let invoked = Arc::new(AtomicBool::new(false));
    let observed = invoked.clone();
    assert!(matches!(
        importer::Backend::new(
            move || {
                observed.store(true, Ordering::SeqCst);
                Ok(Native(Arc::new(Mutex::new(Lab::default()))))
            },
            descriptors,
            budget.clone()
        ),
        Err(Error::Unsupported)
    ));
    assert!(!invoked.load(Ordering::SeqCst));
    assert_eq!(budget.usage().bytes, 0);
}

#[test]
fn capabilities_bound_native_publication_before_allocation() {
    let caps = Native::CAPABILITIES;
    for invalid in [
        Capabilities {
            transfer_bytes: 0,
            ..caps
        },
        Capabilities {
            transfer_bytes: i32::MAX as usize + 1,
            ..caps
        },
        Capabilities {
            requests: 0,
            ..caps
        },
        Capabilities {
            requests: 65,
            ..caps
        },
        Capabilities {
            iso_packets: 1025,
            ..caps
        },
        Capabilities {
            descriptor_bytes: 8,
            ..caps
        },
        Capabilities {
            descriptor_bytes: 65536,
            ..caps
        },
        Capabilities {
            configurations: 0,
            ..caps
        },
        Capabilities {
            configurations: 17,
            ..caps
        },
        Capabilities { speeds: 0, ..caps },
        Capabilities {
            speeds: 1 << 31,
            ..caps
        },
    ] {
        assert_eq!(invalid.validate(), Err(Error::Invalid));
    }
    for speed in [1, 2, 3, 5, 6] {
        assert!(
            Capabilities {
                speeds: 1 << speed,
                ..caps
            }
            .validate()
            .is_ok()
        );
    }
    let b = budget();
    let descriptors = DescriptorSet::new(&b, &DEVICE, &CONFIG, &[], Speed::Full).unwrap();
    assert_eq!(caps.descriptors(&descriptors), Ok(()));
    assert_eq!(
        Capabilities {
            descriptor_bytes: 24,
            ..caps
        }
        .descriptors(&descriptors),
        Err(Error::Unsupported)
    );
    assert_eq!(
        Capabilities {
            speeds: 1 << 3,
            ..caps
        }
        .descriptors(&descriptors),
        Err(Error::Unsupported)
    );
    let mut inventory = device();
    inventory.wire.speed = 32;
    assert_eq!(caps.device(&inventory), Err(Error::Unsupported));
    inventory.wire.speed = 2;
    inventory.wire.configurations = 2;
    assert_eq!(caps.device(&inventory), Err(Error::Unsupported));
    drop(descriptors);
    assert_eq!(b.usage(), Usage::default());
}

#[test]
fn worker_rejects_foreign_budget_without_consuming_the_original_request() {
    let budget = budget();
    let foreign = Budget::new(ResourceLimits {
        bytes: 1024 * 1024,
        requests: 8,
    });
    let lab = Arc::new(Mutex::new(Lab {
        token: 1,
        ..Default::default()
    }));
    let state = lab.clone();
    let descriptors = DescriptorSet::new(&budget, &DEVICE, &CONFIG, &[], Speed::Full).unwrap();
    let mut backend =
        importer::Backend::new(move || Ok(Native(state)), descriptors, budget.clone()).unwrap();
    wait(|| matches!(backend.attach(SESSION, &device()), Poll::Ready(Ok(()))));
    wait(|| lab.lock().unwrap().fetched);
    assert!(matches!(
        backend.next_transfer(SESSION, &foreign),
        Some(Err(Error::State))
    ));
    let mut transfer = None;
    wait(|| {
        transfer = backend.next_transfer(SESSION, &budget).map(|r| r.unwrap());
        transfer.is_some()
    });
    let transfer = transfer.unwrap();
    let fake = Completion {
        transfer: Transfer::new(&foreign, *transfer.header(), &[]).unwrap(),
        status: 0,
        actual_length: 8,
        start_frame: 0,
    };
    drop(backend.complete(SESSION, fake).unwrap_err());
    assert_eq!(foreign.usage(), Usage::default());
    backend
        .complete(
            SESSION,
            Completion {
                transfer,
                status: 0,
                actual_length: 8,
                start_frame: 0,
            },
        )
        .unwrap();
    wait(|| lab.lock().unwrap().token == 0);
    drop(backend);
    wait(|| budget.usage().bytes == 0);
}

#[test]
fn idle_native_worker_parks_after_registration() {
    let budget = budget();
    let lab = Arc::new(Mutex::new(Lab::default()));
    let state = lab.clone();
    let descriptors = DescriptorSet::new(&budget, &DEVICE, &CONFIG, &[], Speed::Full).unwrap();
    let mut backend =
        importer::Backend::new(move || Ok(Native(state)), descriptors, budget.clone()).unwrap();
    wait(|| matches!(backend.attach(SESSION, &device()), Poll::Ready(Ok(()))));
    wait(|| lab.lock().unwrap().snapshots > 0);
    let before = lab.lock().unwrap().snapshots;
    std::thread::sleep(Duration::from_millis(20));
    // Registration's latched signal can cause one extra observation before parking.
    assert!(
        lab.lock().unwrap().snapshots <= before + 1,
        "idle worker must park"
    );
    drop(backend);
    wait(|| budget.usage().bytes == 0);
}

#[test]
fn iso_worker_preserves_packet_results_and_retires_cancel_and_disconnect() {
    for address in [1, 0x81] {
        // Completion, cancellation after delivery, disconnect with a held owner.
        for outcome in 0..3 {
            let budget = budget();
            let mut config = CONFIG;
            config[20] = address;
            config[21] = 1;
            let d = DescriptorSet::new(&budget, &DEVICE, &config, &[], Speed::Full).unwrap();
            let mut inventory = device();
            inventory.configurations = vec![descriptors::configuration(&config).unwrap()];
            let lab = Arc::new(Mutex::new(Lab {
                token: 1,
                iso_address: address,
                ..Default::default()
            }));
            let state = lab.clone();
            let mut backend =
                importer::Backend::new(move || Ok(Native(state)), d, budget.clone()).unwrap();
            wait(|| matches!(backend.attach(SESSION, &inventory), Poll::Ready(Ok(()))));
            let mut transfer = None;
            wait(|| {
                transfer = backend.next_transfer(SESSION, &budget).map(|r| r.unwrap());
                transfer.is_some()
            });
            let mut transfer = transfer.unwrap();
            assert_eq!(transfer.layout().len(), 2);
            assert_eq!(transfer.header().transfer_flags, usbip::ISO_ASAP);
            assert_eq!(
                transfer.buffer(),
                if address == 1 { &[0x42; 16] } else { &[0; 16] }
            );
            if outcome == 2 {
                wait(|| matches!(backend.detach(SESSION), Poll::Ready(Ok(()))));
                // Native retirement does not free storage still owned by transport.
                assert!(budget.usage().bytes >= 16);
                drop(transfer);
            } else {
                if outcome == 1 {
                    let mut state = lab.lock().unwrap();
                    state.token = 0;
                    state.wake.as_ref().unwrap().notify();
                    drop(state);
                    wait(|| backend.next_cancellation(SESSION) == Some(transfer.header().seqnum));
                    assert!(budget.usage().requests > 0);
                    for p in transfer.packets_mut() {
                        p.status = -104;
                    }
                } else {
                    transfer.packets_mut()[0].actual_length = 3;
                    transfer.packets_mut()[1].status = -71;
                    if address == 0x81 {
                        transfer.buffer_mut()[..3].copy_from_slice(&[7, 8, 9]);
                    }
                }
                backend
                    .complete(
                        SESSION,
                        Completion {
                            transfer,
                            status: if outcome == 1 { -104 } else { 0 },
                            actual_length: if outcome == 1 { 0 } else { 3 },
                            start_frame: 0,
                        },
                    )
                    .unwrap();
                wait(|| lab.lock().unwrap().completed.is_some());
                let state = lab.lock().unwrap();
                let done = state.completed.as_ref().unwrap();
                assert_eq!(done.status, if outcome == 1 { -104 } else { 0 });
                assert_eq!(done.actual, if outcome == 1 { 0 } else { 3 });
                assert_eq!(done.packets[1].offset, 12);
                assert_eq!(
                    done.packets[1].status,
                    if outcome == 1 { -104 } else { -71 }
                );
                if outcome == 0 && address == 0x81 {
                    assert_eq!(&done.bytes[..3], &[7, 8, 9]);
                }
                drop(state);
                wait(|| matches!(backend.detach(SESSION), Poll::Ready(Ok(()))));
            }
            drop(backend);
            wait(|| budget.usage() == Usage::default());
            assert!(lab.lock().unwrap().detached);
        }
    }
}

#[test]
fn iso_slots_preserve_endpoint_order_and_correlate_out_of_order_completions() {
    #[derive(Default)]
    struct Stream {
        fetched: usize,
        published: bool,
        live: u64,
        completed: Vec<u64>,
        wake: Option<Arc<ThreadSignal>>,
    }
    struct Queued(Arc<Mutex<Stream>>);
    impl Driver for Queued {
        type Signal = ThreadSignal;
        const CAPABILITIES: Capabilities = Native::CAPABILITIES;
        const NATIVE_BYTES: usize = 128;
        const SCRATCH_BYTES: usize = 256;
        fn attach(&mut self, _: &DescriptorSet, _: &Budget) -> Result<()> {
            Ok(())
        }
        fn notifications(&mut self, wake: Arc<ThreadSignal>) -> Result<()> {
            self.0.lock().unwrap().wake = Some(wake);
            Ok(())
        }
        fn fetch<'a>(
            &'a mut self,
            mask: u32,
            _: &topology::Topology,
            _: &'a mut [u8],
        ) -> Result<Option<importer::Request<'a>>> {
            let mut s = self.0.lock().unwrap();
            let Some(&slot) = [30, 1].get(s.fetched) else {
                return Ok(None);
            };
            assert_ne!(mask & (1 << slot), 0);
            s.fetched += 1;
            Ok(Some(importer::Request {
                slot,
                token: slot as u64 + 10,
                endpoint: 0x81,
                length: 16,
                setup: [0; 8],
                payload: &[],
                packets: &PACKETS,
                flags: usbip::ISO_ASAP,
                start_frame: 0,
            }))
        }
        fn snapshot(&mut self) -> Result<importer::Snapshot> {
            let mut s = self.0.lock().unwrap();
            s.published = s.fetched == 2;
            let mut tokens = [0; 31];
            tokens[30] = 40;
            tokens[1] = 11;
            Ok(importer::Snapshot {
                live: s.live,
                tokens,
            })
        }
        fn complete(&mut self, token: u64, _: &Completion, _: &mut [u8]) -> Result<()> {
            let mut s = self.0.lock().unwrap();
            s.live &= !(1 << (token - 10));
            s.completed.push(token);
            Ok(())
        }
        fn detach(&mut self) -> Result<()> {
            self.0.lock().unwrap().wake = None;
            Ok(())
        }
    }
    let budget = budget();
    let mut config = CONFIG;
    config[21] = 1;
    let d = DescriptorSet::new(&budget, &DEVICE, &config, &[], Speed::Full).unwrap();
    let mut inventory = device();
    inventory.configurations = vec![descriptors::configuration(&config).unwrap()];
    let stream = Arc::new(Mutex::new(Stream {
        live: (1 << 30) | 2,
        ..Default::default()
    }));
    let native = stream.clone();
    let mut backend =
        importer::Backend::new(move || Ok(Queued(native)), d, budget.clone()).unwrap();
    wait(|| matches!(backend.attach(SESSION, &inventory), Poll::Ready(Ok(()))));
    wait(|| stream.lock().unwrap().published);
    let first = backend.next_transfer(SESSION, &budget).unwrap().unwrap();
    let second = backend.next_transfer(SESSION, &budget).unwrap().unwrap();
    assert!(first.header().seqnum < second.header().seqnum);
    assert!(backend.next_transfer(SESSION, &budget).is_none());
    for (transfer, token) in [(second, 11), (first, 40)] {
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
        wait(|| stream.lock().unwrap().completed.contains(&token));
    }
    assert_eq!(stream.lock().unwrap().completed, [11, 40]);
    wait(|| matches!(backend.detach(SESSION), Poll::Ready(Ok(()))));
    drop(backend);
    wait(|| budget.usage().bytes == 0);
}

#[test]
fn iso_spare_slots_do_not_admit_inactive_bulk_endpoints() {
    let b = budget();
    let mut config = vec![
        9, 2, 0, 0, 2, 1, 0, 0x80, 50, 9, 4, 0, 0, 1, 0xff, 0, 0, 0, 7, 5, 0x81, 1, 8, 0, 1, 9, 4,
        1, 0, 0, 0xff, 0, 0, 0, 9, 4, 1, 1, 1, 0xff, 0, 0, 0, 7, 5, 0x82, 2, 64, 0, 0,
    ];
    config[2] = config.len() as u8;
    let d = DescriptorSet::new(&b, &DEVICE, &config, &[], Speed::Full).unwrap();
    let mut alternates = [0; topology::MAX_SETTINGS];
    assert_eq!(d.active_slots(&alternates), ((1u32 << 31) - 1) & !4);
    alternates[1] = 1;
    assert_eq!(d.active_slots(&alternates), (1u32 << 31) - 1);
}
