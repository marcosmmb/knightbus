//! Remote descriptor discovery and configuration selection.
use knightbus_core::{
    Budget, Error, ResourceLimits,
    usbip::{self, Context, Limits, Message},
};
use knightbus_transport::importer::Wire;
use std::{
    net::{TcpListener, TcpStream},
    time::Duration,
};
fn pair(timeout: Duration) -> (Wire, TcpStream, Budget) {
    pair_with_transfer_limit(timeout, 1024 * 1024)
}
fn pair_with_transfer_limit(timeout: Duration, transfer_bytes: usize) -> (Wire, TcpStream, Budget) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let sender = TcpStream::connect(listener.local_addr().unwrap()).unwrap();
    let (receiver, _) = listener.accept().unwrap();
    let b = Budget::new(ResourceLimits {
        // Include the decoder's conservative charges for reallocations.
        bytes: 8 * 1024 * 1024,
        requests: 32,
    });
    (
        Wire::new(
            knightbus_transport::Stream::insecure_loopback(receiver).unwrap(),
            b.clone(),
            timeout,
            knightbus_core::Capabilities {
                transfer_bytes,
                requests: 31,
                iso_packets: 0,
                speeds: 0b1101110,
                descriptor_bytes: 4096,
                configurations: 1,
                remote_wakeup: false,
            },
        )
        .unwrap(),
        sender,
        b,
    )
}
#[test]
fn unconfigured_linux_gadget_requires_verified_configuration_and_charged_topology() {
    check_configuration(0);
    check_configuration(1);
}
fn check_configuration(reported_configuration: u8) {
    use usbip::WireString;
    let (mut wire, peer, budget) = pair(Duration::from_secs(2));
    let server = std::thread::spawn(move || {
        serve_gadget(peer, reported_configuration, |_| {});
    });
    let result = wire.import(WireString::new(b"usbip-vudc.0").unwrap());
    if reported_configuration == 0 {
        assert!(matches!(result, Err(Error::Unavailable)));

        server.join().unwrap();
        drop(wire);
        assert_eq!(budget.usage().bytes, 0);
        return;
    }
    let (device, descriptors) = result.unwrap();
    assert_eq!(device.device().wire.configuration, 1);
    assert_eq!(
        device.device().configurations[0].interfaces[0].endpoints[0].address,
        0x81
    );
    assert_eq!(descriptors.topology().interfaces, 1);
    server.join().unwrap();
    drop(wire);
    drop(descriptors);
    assert!(
        budget.usage().bytes > 0,
        "inventory must retain its storage charge"
    );
    drop(device);
    assert_eq!(budget.usage().bytes, 0);
}

fn serve_gadget(
    mut peer: TcpStream,
    reported_configuration: u8,
    mut before_reply: impl FnMut(usize),
) {
    use usbip::{ReturnSubmit, WireString};
    let Ok(Some(Message::ImportRequest(bus_id))) = usbip::read_message(
        &mut peer,
        Context::Management,
        Limits::default(),
        &mut |_| Ok(()),
    ) else {
        return;
    };
    before_reply(0);
    usbip::write_message(
        &mut peer,
        &Message::ImportReply {
            status: 0,
            device: Some(Box::new(usbip::Device {
                path: WireString::new(b"original-test-gadget").unwrap(),
                bus_id,
                bus_number: 0,
                device_number: 0,
                speed: 2,
                vendor_id: 0x1209,
                product_id: 1,
                device_bcd: 0x100,
                class: 0,
                subclass: 0,
                protocol: 0,
                // Stock usbip-vudc advertises current, unconfigured state.
                configuration: 0,
                configurations: 1,
                interfaces: 0,
            })),
        },
        Limits::default(),
        &mut |_| Ok(()),
    )
    .ok();
    let device = [18, 1, 0, 2, 0, 0, 0, 64, 9, 0x12, 1, 0, 0, 1, 0, 0, 0, 1];
    let config = [
        9, 2, 25, 0, 1, 1, 0, 0x80, 50, 9, 4, 0, 0, 1, 0xff, 0, 0, 0, 7, 5, 0x81, 3, 8, 0, 10,
    ];
    for (index, payload) in [
        &device[..],
        &config[..9],
        &config[..],
        &[][..],
        &[reported_configuration][..],
    ]
    .into_iter()
    .enumerate()
    {
        let Ok(Some(Message::Submit(request))) =
            usbip::read_message(&mut peer, Context::Requests, Limits::default(), &mut |_| {
                Ok(())
            })
        else {
            return;
        };
        assert_eq!(request.transfer_buffer_length as usize, payload.len());
        if index == 3 {
            assert_eq!(request.setup, [0, 9, 1, 0, 0, 0, 0, 0]);
        }
        if index == 4 {
            assert_eq!(request.setup, [0x80, 8, 0, 0, 0, 0, 1, 0]);
        }
        before_reply(index + 1);
        usbip::write_message(
            &mut peer,
            &Message::ReturnSubmit(ReturnSubmit {
                request: (&request).into(),
                status: 0,
                actual_length: payload.len() as u32,
                start_frame: 0,
                number_of_packets: 0,
                error_count: 0,
                data: payload.to_vec(),
                iso_packets: vec![],
            }),
            Limits::default(),
            &mut |_| Ok(()),
        )
        .ok();
    }
}

#[test]
fn bootstrap_uses_one_deadline_across_all_exchanges() {
    let (mut wire, peer, budget) = pair(Duration::from_millis(200));
    let server = std::thread::spawn(move || {
        serve_gadget(peer, 1, |_| {
            std::thread::sleep(Duration::from_millis(60));
        })
    });
    let started = std::time::Instant::now();
    assert!(matches!(
        wire.import(usbip::WireString::new(b"gadget").unwrap()),
        Err(Error::Deadline)
    ));
    assert!(started.elapsed() < Duration::from_millis(350));
    drop(wire);
    server.join().unwrap();
    assert_eq!(budget.usage().bytes, 0);
}

#[test]
fn cancellation_interrupts_every_bootstrap_exchange() {
    use knightbus_transport::operation::Operation;
    use std::sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    };
    for stage in 0..6 {
        let (mut wire, peer, budget) = pair(Duration::from_secs(5));
        let cancelled = Arc::new(AtomicBool::new(false));
        let signal = cancelled.clone();
        let server = std::thread::spawn(move || {
            serve_gadget(peer, 1, |index| {
                if index == stage {
                    signal.store(true, Ordering::SeqCst);
                    std::thread::sleep(Duration::from_millis(300));
                }
            })
        });
        let started = std::time::Instant::now();
        let check = || cancelled.load(Ordering::SeqCst);
        let operation = Operation::new(started + Duration::from_secs(5), &check);
        assert!(
            matches!(
                wire.import_until(usbip::WireString::new(b"gadget").unwrap(), &operation),
                Err(Error::Cancelled)
            ),
            "stage {stage}"
        );
        assert!(
            started.elapsed() < Duration::from_millis(250),
            "stage {stage}"
        );
        drop(wire);
        server.join().unwrap();
        assert_eq!(budget.usage().bytes, 0);
    }
}

#[test]
fn discovery_frame_capacity_is_independent_of_transfer_size() {
    for count in [4, 128, 129] {
        let (mut wire, mut peer, budget) = pair_with_transfer_limit(Duration::from_secs(2), 1024);
        let server = std::thread::spawn(move || {
            assert!(matches!(
                usbip::read_message(
                    &mut peer,
                    Context::Management,
                    Limits::default(),
                    &mut |_| Ok(())
                )
                .unwrap(),
                Some(Message::DevListRequest)
            ));
            let devices = (0..count)
                .map(|id| usbip::ListedDevice {
                    device: usbip::Device {
                        path: usbip::WireString::new(b"fixture").unwrap(),
                        bus_id: usbip::WireString::new(format!("kb-{id:x}").as_bytes()).unwrap(),
                        bus_number: 1,
                        device_number: id,
                        speed: 2,
                        vendor_id: 1,
                        product_id: 1,
                        device_bcd: 0x100,
                        class: 0,
                        subclass: 0,
                        protocol: 0,
                        configuration: 1,
                        configurations: 1,
                        interfaces: 1,
                    },
                    interfaces: vec![usbip::Interface {
                        class: 3,
                        subclass: 0,
                        protocol: 0,
                    }],
                })
                .collect();
            usbip::write_message(
                &mut peer,
                &Message::DevListReply { status: 0, devices },
                Limits {
                    devices: 129,
                    ..Limits::default()
                },
                &mut |_| Ok(()),
            )
            .ok();
        });
        let result = wire.list();
        if count <= 128 {
            assert_eq!(
                result
                    .unwrap_or_else(|error| panic!("count={count}: {error:?}"))
                    .len(),
                count as usize
            );
        } else {
            assert!(result.is_err());
        }
        drop(wire);
        server.join().unwrap();
        assert_eq!(budget.usage().bytes, 0);
    }
}
