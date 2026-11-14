//! Opt-in Windows lab proof. Requires the separately test-signed original driver.
//! Defaults to an original synthetic CDC exporter. KNIGHTBUS_TEST_EXPORTER and
//! KNIGHTBUS_TEST_BUS_ID select a stock Linux CDC gadget with a ttyGS0 echo peer.
#![cfg(windows)]
use knightbus_core::{
    Budget, ResourceLimits, SessionId,
    usbip::{self, Context, Limits, Message, ReturnSubmit, WireString},
};
use knightbus_windows_importer::{
    backend::Backend,
    native::Controller,
    transport::{Forwarder, Wire},
};
use std::{
    io::{Read, Write},
    net::{TcpListener, TcpStream},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
    task::Poll,
    time::{Duration, Instant},
};
static CONTROLLER_TEST: Mutex<()> = Mutex::new(());
const DEVICE: [u8; 18] = [18, 1, 0, 2, 2, 2, 0, 64, 9, 0x12, 1, 0, 0, 1, 0, 0, 0, 1];
const CONFIG: [u8; 67] = [
    9, 2, 67, 0, 2, 1, 0, 0x80, 50, 9, 4, 0, 0, 1, 2, 2, 1, 0, 5, 0x24, 0, 0x20, 1, 5, 0x24, 1, 0,
    1, 4, 0x24, 2, 2, 5, 0x24, 6, 0, 1, 7, 5, 0x81, 3, 16, 0, 16, 9, 4, 1, 0, 2, 0x0a, 0, 0, 0, 7,
    5, 2, 2, 64, 0, 0, 7, 5, 0x82, 2, 64, 0, 0,
];
// A vendor fixture bound to inbox WinUSB by tests/winusb.inf. Same bulk
// endpoint numbers as CDC, so the existing exporter can exercise a real pipe reset.
const WINUSB_DEVICE: [u8; 18] = [18, 1, 0, 2, 0, 0, 0, 64, 9, 0x12, 4, 0, 0, 1, 0, 0, 0, 1];
const WINUSB_CONFIG: [u8; 32] = [
    9, 2, 32, 0, 1, 1, 0, 0x80, 50, 9, 4, 0, 0, 2, 0xff, 0, 0, 0, 7, 5, 2, 2, 64, 0, 0, 7, 5, 0x82,
    2, 64, 0, 0,
];
fn reply(s: &mut TcpStream, request: usbip::ReplyContext, status: i32, actual: u32, data: Vec<u8>) {
    usbip::write_message(
        s,
        &Message::ReturnSubmit(ReturnSubmit {
            request,
            status,
            actual_length: actual,
            start_frame: 0,
            number_of_packets: 0,
            error_count: 0,
            data,
            iso_packets: vec![],
        }),
        Limits::default(),
    )
    .unwrap();
}
fn exporter(
    listener: TcpListener,
    completed: Arc<AtomicUsize>,
    connected: Option<std::sync::mpsc::Sender<TcpStream>>,
) {
    export_fixture(listener, completed, connected, None);
}
fn export_fixture(
    listener: TcpListener,
    completed: Arc<AtomicUsize>,
    connected: Option<std::sync::mpsc::Sender<TcpStream>>,
    halt: Option<Arc<AtomicUsize>>,
) {
    let (mut stream, _) = listener.accept().unwrap();
    if let Some(connected) = connected {
        connected.send(stream.try_clone().unwrap()).unwrap();
    }
    stream
        .set_read_timeout(Some(Duration::from_secs(30)))
        .unwrap();
    let Some(Message::ImportRequest(bus_id)) =
        usbip::read_message(&mut stream, Context::Management, Limits::default()).unwrap()
    else {
        panic!("import")
    };
    let vendor = halt.is_some();
    let device = usbip::Device {
        path: WireString::new(b"synthetic-cdc").unwrap(),
        bus_id,
        bus_number: 1,
        device_number: 1,
        speed: 2,
        vendor_id: 0x1209,
        product_id: if vendor { 4 } else { 1 },
        device_bcd: 0x100,
        class: if vendor { 0 } else { 2 },
        subclass: if vendor { 0 } else { 2 },
        protocol: 0,
        configuration: 1,
        configurations: 1,
        interfaces: if vendor { 1 } else { 2 },
    };
    usbip::write_message(
        &mut stream,
        &Message::ImportReply {
            status: 0,
            device: Some(Box::new(device)),
        },
        Limits::default(),
    )
    .unwrap();
    let mut pending: Vec<(u32, usbip::ReplyContext)> = vec![];
    let mut data = vec![];
    while let Ok(Some(request)) =
        usbip::read_message(&mut stream, Context::Requests, Limits::default())
    {
        match request {
            Message::Submit(s) => {
                let ctx = (&s).into();
                match s.endpoint {
                    0 => {
                        if s.setup == [2, 1, 0, 0, 2, 0, 0, 0] {
                            if let Some(halt) = &halt {
                                // A reset only recovers the fixture if the real
                                // CLEAR_FEATURE reaches this USB/IP peer.
                                if halt
                                    .compare_exchange(1, 2, Ordering::AcqRel, Ordering::Acquire)
                                    .is_ok()
                                {
                                    // Reject the direct clear to verify native
                                    // control-error propagation before retry.
                                    reply(&mut stream, ctx, -5, 0, vec![]);
                                    continue;
                                }
                                let _ = halt.compare_exchange(
                                    2,
                                    3,
                                    Ordering::AcqRel,
                                    Ordering::Acquire,
                                );
                            }
                        }
                        let mut payload = match (s.setup[0], s.setup[1], s.setup[3]) {
                            (0x80, 6, 1) => {
                                if vendor {
                                    WINUSB_DEVICE.to_vec()
                                } else {
                                    DEVICE.to_vec()
                                }
                            }
                            (0x80, 6, 2) => {
                                if vendor {
                                    WINUSB_CONFIG.to_vec()
                                } else {
                                    CONFIG.to_vec()
                                }
                            }
                            (0x80, 8, _) => vec![1],
                            (0x81, 10, _) => vec![0],
                            (0x80..=0x82, 0, _) => vec![0, 0],
                            (0xa1, 0x21, _) => vec![0, 0xc2, 1, 0, 0, 0, 8],
                            _ => vec![],
                        };
                        payload.truncate(s.transfer_buffer_length as usize);
                        let actual = if s.direction == usbip::Direction::In {
                            payload.len() as u32
                        } else {
                            s.transfer_buffer_length
                        };
                        reply(&mut stream, ctx, 0, actual, payload);
                    }
                    2 if s.direction == usbip::Direction::Out => {
                        if let Some(halt) = &halt {
                            if halt.load(Ordering::Acquire) != 3 {
                                let _ = halt.compare_exchange(
                                    0,
                                    1,
                                    Ordering::AcqRel,
                                    Ordering::Acquire,
                                );
                                reply(&mut stream, ctx, -32, 0, vec![]);
                                continue;
                            }
                        }
                        data.extend(&s.data);
                        reply(&mut stream, ctx, 0, s.transfer_buffer_length, vec![]);
                        completed.fetch_add(1, Ordering::Release);
                    }
                    2 | 1 => pending.push((s.endpoint, ctx)),
                    _ => reply(&mut stream, ctx, -32, 0, vec![]),
                }
                let index = pending.iter().position(|(endpoint, p)| {
                    *endpoint == 2
                        && p.direction == usbip::Direction::In
                        && p.transfer_buffer_length >= data.len() as u32
                        && !data.is_empty()
                });
                if let Some(index) = index {
                    let (_, p) = pending.remove(index);
                    let payload = std::mem::take(&mut data);
                    reply(&mut stream, p, 0, payload.len() as u32, payload);
                }
            }
            Message::Unlink {
                seqnum,
                target_seqnum,
                ..
            } => {
                let index = pending.iter().position(|(_, p)| p.seqnum == target_seqnum);
                if let Some(i) = index {
                    pending.remove(i);
                }
                usbip::write_message(
                    &mut stream,
                    &Message::ReturnUnlink {
                        seqnum,
                        status: if index.is_some() { -104 } else { 0 },
                    },
                    Limits::default(),
                )
                .unwrap();
            }
            _ => panic!("unexpected request"),
        }
    }
}
#[test]
#[ignore = "requires administrator access and one idle KnightBus development controller"]
fn native_channel_rejects_malformed_requests_and_other_owners() {
    use knightbus_core::Error;
    use knightbus_windows_importer::backend::Driver;
    let _guard = CONTROLLER_TEST.lock().unwrap();
    let mut owner = Controller::open(None).unwrap();
    let before = owner.lifecycle().unwrap();
    assert_eq!(owner.attach(b"invalid"), Err(Error::Invalid));
    assert_eq!(owner.lifecycle().unwrap(), before);

    // Original endpoint-zero-only vendor fixture; no native function driver needed.
    let mut device = DEVICE;
    device[4..7].fill(0);
    device[10] = 0x42;
    let config = [9, 2, 18, 0, 1, 1, 0, 0x80, 50, 9, 4, 0, 0, 0, 0xff, 0, 0, 0];
    let mut envelope = Vec::from(*b"KBWD");
    for value in [1u32, 2, 18, config.len() as u32] {
        envelope.extend(value.to_le_bytes());
    }
    envelope.extend(device);
    envelope.extend(config);
    owner.attach(&envelope).unwrap();
    let mut other = Controller::open(None).unwrap();
    assert!(other.attach(&envelope).is_err());
    assert_eq!(other.lifecycle(), Err(Error::Denied));
    assert_eq!(other.detach(), Err(Error::Denied));
    assert_eq!(other.reply(&[0; 32]), Err(Error::Denied));
    assert_eq!(other.fetch(0x7fff_ffff, &mut [0; 32]), Err(Error::Denied));
    drop(other); // Closing a different file must not detach the owner's child.
    assert_eq!(owner.fetch(0x7fff_ffff, &mut [0; 32]), Err(Error::Invalid));
    assert_eq!(owner.reply(&[0; 32]), Err(Error::Invalid));
    owner.detach().unwrap();
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let after = owner.lifecycle().unwrap();
        // Compare created/destroyed pairs for children, endpoints and queues.
        // Index 4 is default-endpoint additions, not endpoint creations; those
        // totals diverge after the CDC test has used nondefault endpoints.
        if after[1] == after[3] && after[5] == after[7] && after[8] == after[10] && after[15] == 0 {
            assert_eq!(after[1], before[1] + 1);
            break;
        }
        assert!(Instant::now() < deadline, "owned objects did not retire");
        std::thread::sleep(Duration::from_millis(10));
    }
}
#[test]
#[ignore = "requires administrator access and the installed KnightBus development driver"]
fn native_cdc_bulk_roundtrip_and_detach() {
    let _guard = CONTROLLER_TEST.lock().unwrap();
    cdc_roundtrip(false);
}
#[test]
#[ignore = "requires administrator access and the staged KnightBus driver package"]
fn native_owned_controller_retirement() {
    let _guard = CONTROLLER_TEST.lock().unwrap();
    assert!(
        owned_controllers().is_empty(),
        "another attachment owner is running"
    );
    // Partial attachment failure must release the whole newly created controller.
    use knightbus_windows_importer::backend::Driver;
    let mut rejected = Controller::owned();
    assert_eq!(
        rejected.attach(b"invalid"),
        Err(knightbus_core::Error::Invalid)
    );
    rejected.detach().unwrap();
    rejected.detach().unwrap();
    assert!(owned_controllers().is_empty());
    for _ in 0..3 {
        cdc_roundtrip(true);
    }
    owner_faults(true, false);
}
#[test]
#[ignore = "requires administrator access, staged KnightBus and tests/winusb.inf packages"]
fn native_bulk_stall_reset_reaches_exporter() {
    let _guard = CONTROLLER_TEST.lock().unwrap();
    assert!(
        owned_controllers().is_empty() && winusb_paths().is_empty(),
        "another lab owner is running"
    );
    assert!(std::env::var_os("KNIGHTBUS_TEST_EXPORTER").is_none());
    let halt = Arc::new(AtomicUsize::new(0));
    roundtrip(true, Some(halt.clone()));
    assert_eq!(
        halt.load(Ordering::Acquire),
        3,
        "remote halt was never cleared"
    );
}
fn cdc_roundtrip(owned: bool) {
    roundtrip(owned, None);
}
fn roundtrip(owned: bool, halt: Option<Arc<AtomicUsize>>) {
    assert!(
        serial_ports().is_empty(),
        "another CDC fixture is already present"
    );
    let expected_ports: usize = if halt.is_some() {
        1
    } else {
        std::env::var("KNIGHTBUS_TEST_PORT_COUNT")
            .map(|n| n.parse().unwrap())
            .unwrap_or(1)
    };
    assert!((1..=2).contains(&expected_ports));
    let before = (!owned).then(|| Controller::open(None).unwrap().lifecycle().unwrap());
    let completed = Arc::new(AtomicUsize::new(0));
    let (address, bus_id, server) = if let Ok(address) = std::env::var("KNIGHTBUS_TEST_EXPORTER") {
        (
            address,
            std::env::var("KNIGHTBUS_TEST_BUS_ID").unwrap(),
            None,
        )
    } else {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap().to_string();
        let count = completed.clone();
        let halt = halt.clone();
        (
            address,
            "1-1".to_string(),
            Some(std::thread::spawn(move || {
                export_fixture(listener, count, None, halt)
            })),
        )
    };
    let stop = Arc::new(AtomicBool::new(false));
    let end = stop.clone();
    let budget = Budget::new(ResourceLimits {
        bytes: 16 * 1024 * 1024,
        requests: 64,
    });
    let b = budget.clone();
    let runner = std::thread::spawn(move || {
        let timeout = Duration::from_secs(15);
        let mut wire = Wire::new(TcpStream::connect(address).unwrap(), b.clone(), timeout).unwrap();
        let (device, descriptors) = wire
            .import(WireString::new(bus_id.as_bytes()).unwrap())
            .unwrap();
        let backend = Backend::new(
            move || {
                if owned {
                    Ok(Controller::owned())
                } else {
                    Controller::open(None)
                }
            },
            descriptors,
            b.clone(),
        )
        .unwrap();
        let mut forwarding = Forwarder::new(
            backend,
            wire,
            device,
            SessionId {
                epoch: 123,
                serial: 1,
            },
            b,
            timeout,
        )
        .unwrap();
        let deadline = Instant::now() + Duration::from_secs(25);
        loop {
            if end.load(Ordering::Acquire) || Instant::now() > deadline {
                forwarding.disconnect();
            }
            if let Poll::Ready(result) = forwarding.tick() {
                return result;
            }
            std::thread::sleep(Duration::from_millis(1));
        }
    });
    let outcome = std::panic::catch_unwind(|| {
        let deadline = Instant::now() + Duration::from_secs(12);
        let ports = loop {
            assert!(Instant::now() < deadline, "native CDC did not enumerate");
            let ports = if halt.is_some() {
                winusb_paths()
            } else {
                serial_ports()
            };
            if ports.len() == expected_ports {
                break ports;
            }
            std::thread::sleep(Duration::from_millis(100));
        };
        for port in ports {
            if let Some(halt) = &halt {
                winusb_reset_echo(&port, halt);
            } else {
                echo(&port);
            }
        }
    });
    stop.store(true, Ordering::Release);
    let result = runner.join().unwrap();
    if let Some(server) = server {
        server.join().unwrap();
        assert!(completed.load(Ordering::Acquire) >= 20);
    }
    outcome.unwrap();
    result.unwrap();
    if let Some(before) = before {
        let after = Controller::open(None).unwrap().lifecycle().unwrap();
        assert_eq!(after[1] - before[1], 1);
        assert_eq!(after[3] - before[3], 1);
        assert_eq!(after[5] - before[5], after[7] - before[7]);
        assert_eq!(after[8] - before[8], after[10] - before[10]);
        assert_eq!(after[15], 0);
    } else {
        assert!(
            owned_controllers().is_empty(),
            "owned controller outlived retirement"
        );
    }
    let deadline = Instant::now() + Duration::from_secs(2);
    while budget.usage().bytes != 0 {
        assert!(Instant::now() < deadline);
        std::thread::yield_now();
    }
}

#[test]
#[ignore = "requires administrator access and the installed KnightBus development driver"]
fn native_owner_exit_and_network_loss_retire_owned_objects() {
    let _guard = CONTROLLER_TEST.lock().unwrap();
    owner_faults(false, false);
}
#[test]
#[ignore = "requires administrator access and the staged KnightBus driver package"]
fn native_owned_retirement_waits_for_open_client() {
    let _guard = CONTROLLER_TEST.lock().unwrap();
    assert!(owned_controllers().is_empty() && serial_ports().is_empty());
    owner_faults(true, true);
}
fn owner_faults(owned: bool, hold_client: bool) {
    for kill_owner in [false, true] {
        let before = (!owned).then(|| Controller::open(None).unwrap().lifecycle().unwrap());
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let (connected, connection) = std::sync::mpsc::channel();
        let server = std::thread::spawn(move || {
            exporter(listener, Arc::new(AtomicUsize::new(0)), Some(connected))
        });
        let mut command =
            std::process::Command::new(env!("CARGO_BIN_EXE_knightbus-windows-importer"));
        command.args(["--insecure-lab", &address.to_string(), "1-1", "15"]);
        if !owned {
            // The CLI default is owned; preserve the explicit diagnostic path
            // when checking counters on a preinstalled root controller.
            let output = std::process::Command::new("powershell.exe").args(["-NoProfile", "-Command",
                r"Get-CimInstance Win32_PnPEntity | Where-Object {$_.PNPDeviceID -like 'ROOT\*' -and $_.HardwareID -contains 'ROOT\KNIGHTBUS_UDE'} | ForEach-Object PNPDeviceID"]).output().unwrap();
            assert!(output.status.success());
            let ids = String::from_utf8_lossy(&output.stdout);
            let ids: Vec<_> = ids
                .lines()
                .map(str::trim)
                .filter(|v| !v.is_empty())
                .collect();
            assert_eq!(ids.len(), 1);
            command.arg(ids[0]);
        }
        let mut child = command.spawn().unwrap();
        let stream = connection.recv_timeout(Duration::from_secs(10)).unwrap();
        let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let deadline = Instant::now() + Duration::from_secs(10);
            let ports = loop {
                let ports = serial_ports();
                if ports.len() == 1 {
                    break ports;
                }
                assert!(
                    Instant::now() < deadline,
                    "native attachment did not publish endpoints"
                );
                assert!(
                    child.try_wait().unwrap().is_none(),
                    "importer exited before attachment"
                );
                std::thread::sleep(Duration::from_millis(10));
            };
            let held = hold_client.then(|| {
                std::fs::OpenOptions::new()
                    .read(true)
                    .write(true)
                    .open(format!("\\\\.\\{}", ports[0]))
                    .unwrap()
            });
            let instance = hold_client.then(|| {
                let ids = owned_controllers();
                assert_eq!(ids.len(), 1);
                ids[0].clone()
            });
            // Exercise removal while the native stack still owns active pipes.
            if kill_owner {
                child.kill().unwrap();
            } else {
                stream.shutdown(std::net::Shutdown::Both).unwrap();
            }
            let deadline = Instant::now() + Duration::from_secs(if hold_client { 20 } else { 10 });
            while child.try_wait().unwrap().is_none() {
                assert!(
                    Instant::now() < deadline,
                    "importer failed to exit after disconnect"
                );
                std::thread::sleep(Duration::from_millis(10));
            }
            if let Some(instance) = instance {
                assert_retirement_waits(&instance, held.unwrap());
            }
            loop {
                if owned {
                    if owned_controllers().is_empty() && serial_ports().is_empty() {
                        break;
                    }
                    assert!(
                        Instant::now() < deadline,
                        "controller survived owner/connection loss"
                    );
                    std::thread::sleep(Duration::from_millis(20));
                    continue;
                }
                let before = before.unwrap();
                let after = Controller::open(None).unwrap().lifecycle();
                if let Ok(after) = after {
                    if after[3] - before[3] == 1 && after[15] == 0 {
                        assert_eq!(after[5] - before[5], after[7] - before[7]);
                        assert_eq!(after[8] - before[8], after[10] - before[10]);
                        break;
                    }
                }
                assert!(Instant::now() < deadline, "native objects did not retire");
                std::thread::sleep(Duration::from_millis(10));
            }
        }));
        let _ = child.kill();
        let _ = child.wait();
        let _ = stream.shutdown(std::net::Shutdown::Both);
        // Closing the socket may race a synthetic response write; only owned
        // native retirement, above, is the assertion in this fault test.
        let _ = server.join();
        outcome.unwrap();
    }
}

fn assert_retirement_waits(instance: &str, client: std::fs::File) {
    use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle};
    use windows_sys::Win32::{Foundation::*, System::Threading::*};
    let suffix = instance.rsplit('\\').next().unwrap().to_ascii_lowercase();
    let slot: u8 = suffix.strip_prefix("slot").unwrap().parse().unwrap();
    assert!(slot < 64);
    let mut name: Vec<u16> = format!("\\Device\\KnightBus.Controller.{slot}")
        .encode_utf16()
        .chain([0])
        .collect();
    // SYNCHRONIZE only; opening must not create or signal a reservation. The
    // importer has exited, so a live name now proves the kernel retained it.
    use windows_sys::Wdk::{Foundation::OBJECT_ATTRIBUTES, System::Threading::NtOpenEvent};
    let mut name = UNICODE_STRING {
        Length: ((name.len() - 1) * 2) as u16,
        MaximumLength: (name.len() * 2) as u16,
        Buffer: name.as_mut_ptr(),
    };
    let attributes = OBJECT_ATTRIBUTES {
        Length: std::mem::size_of::<OBJECT_ATTRIBUTES>() as u32,
        ObjectName: &mut name,
        Attributes: OBJ_CASE_INSENSITIVE,
        ..Default::default()
    };
    let mut event = std::ptr::null_mut();
    assert_eq!(
        unsafe { NtOpenEvent(&mut event, SYNCHRONIZATION_SYNCHRONIZE, &attributes) },
        STATUS_SUCCESS,
        "slot was released before native destruction"
    );
    // SAFETY: Unique valid event handle, owned solely by this RAII guard.
    let event = unsafe { OwnedHandle::from_raw_handle(event) };
    assert_eq!(
        unsafe { WaitForSingleObject(event.as_raw_handle(), 0) },
        WAIT_TIMEOUT,
        "retirement acknowledged while the USB client still held the controller"
    );
    drop(client);
    assert_eq!(
        unsafe { WaitForSingleObject(event.as_raw_handle(), 10000) },
        WAIT_OBJECT_0,
        "controller did not retire after its last client closed"
    );
}

fn echo(port: &str) {
    let mut serial = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(format!("\\\\.\\{port}"))
        .unwrap();
    use std::os::windows::io::AsRawHandle;
    use windows_sys::Win32::Devices::Communication::{COMMTIMEOUTS, SetCommTimeouts};
    let timeout = COMMTIMEOUTS {
        ReadIntervalTimeout: 50,
        ReadTotalTimeoutMultiplier: 0,
        ReadTotalTimeoutConstant: 2000,
        WriteTotalTimeoutMultiplier: 0,
        WriteTotalTimeoutConstant: 2000,
    };
    // SAFETY: Borrowed live synchronous serial handle; no retained pointer.
    assert_ne!(
        unsafe { SetCommTimeouts(serial.as_raw_handle(), &timeout) },
        0
    );
    for n in 0..20u8 {
        let payload = [n; 64];
        serial.write_all(&payload).unwrap();
        let mut received = [0; 64];
        serial.read_exact(&mut received).unwrap();
        assert_eq!(received, payload);
    }
}

fn winusb_paths() -> Vec<String> {
    use windows_sys::{Win32::Devices::DeviceAndDriverInstallation::*, core::GUID};
    let interface = GUID::from_u128(0x655b099e_fc72_45c8_9264_853b7b9a54bd);
    let mut size = 0;
    // SAFETY: Local outputs, a fixed test-interface GUID, no retained pointers.
    if unsafe { CM_Get_Device_Interface_List_SizeW(&mut size, &interface, std::ptr::null(), 0) }
        != CR_SUCCESS
        || !(2..=4096).contains(&size)
    {
        return vec![];
    }
    let mut paths = vec![0u16; size as usize];
    // SAFETY: OS size is bounded above and matches the allocated buffer.
    if unsafe {
        CM_Get_Device_Interface_ListW(&interface, std::ptr::null(), paths.as_mut_ptr(), size, 0)
    } != CR_SUCCESS
    {
        return vec![];
    }
    paths
        .split(|v| *v == 0)
        .filter(|p| !p.is_empty())
        .map(|p| String::from_utf16(p).unwrap())
        .filter(|p| p.to_ascii_lowercase().contains("vid_1209&pid_0004"))
        .collect()
}
fn winusb_reset_echo(path: &str, halt: &AtomicUsize) {
    use std::ptr;
    use windows_sys::Win32::{Devices::Usb::*, Foundation::*, Storage::FileSystem::*};
    struct Handles {
        file: HANDLE,
        usb: WINUSB_INTERFACE_HANDLE,
    }
    impl Drop for Handles {
        fn drop(&mut self) {
            // SAFETY: Exclusive handles; synchronous calls have already returned.
            unsafe {
                if !self.usb.is_null() {
                    WinUsb_Free(self.usb);
                }
                CloseHandle(self.file);
            }
        }
    }
    let path: Vec<u16> = path.encode_utf16().chain([0]).collect();
    // SAFETY: Terminated OS interface path, noninheritable handle. WinUSB requires
    // an overlapped-capable file even when its transfer calls are synchronous.
    let file = unsafe {
        CreateFileW(
            path.as_ptr(),
            GENERIC_READ | GENERIC_WRITE,
            FILE_SHARE_READ | FILE_SHARE_WRITE,
            ptr::null(),
            OPEN_EXISTING,
            FILE_FLAG_OVERLAPPED,
            ptr::null_mut(),
        )
    };
    assert_ne!(file, INVALID_HANDLE_VALUE);
    let mut handles = Handles {
        file,
        usb: ptr::null_mut(),
    };
    // SAFETY: Live file and local output; Handles closes both, including on panic.
    assert_ne!(unsafe { WinUsb_Initialize(file, &mut handles.usb) }, 0);
    let timeout = 2000u32;
    for endpoint in [2, 0x82] {
        // SAFETY: A four-byte policy value, copied by the synchronous API.
        assert_ne!(
            unsafe {
                WinUsb_SetPipePolicy(
                    handles.usb,
                    endpoint,
                    PIPE_TRANSFER_TIMEOUT,
                    4,
                    ptr::from_ref(&timeout).cast(),
                )
            },
            0
        );
    }
    let mut count = 0;
    let mut sent = [0u8; 64];
    // SAFETY: Synchronous transfer, initialized buffer and live interface handle.
    assert_eq!(
        unsafe {
            WinUsb_WritePipe(
                handles.usb,
                2,
                sent.as_ptr(),
                64,
                &mut count,
                ptr::null_mut(),
            )
        },
        0,
        "injected STALL was hidden"
    );
    assert_eq!(unsafe { GetLastError() }, ERROR_GEN_FAILURE);
    // This must clear the *remote* halt before success; the exporter continues
    // returning EPIPE until the correct CLEAR_FEATURE setup packet arrives.
    let clear = WINUSB_SETUP_PACKET {
        RequestType: 2,
        Request: 1,
        Value: 0,
        Index: 2,
        Length: 0,
    };
    // Check the control URB's error directly. WinUSB's higher-level ResetPipe
    // can report success despite a failed clear, so its BOOL is not that proof.
    assert_eq!(
        unsafe {
            WinUsb_ControlTransfer(
                handles.usb,
                clear,
                ptr::null_mut(),
                0,
                &mut count,
                ptr::null_mut(),
            )
        },
        0,
        "remote clear failure was hidden"
    );
    assert_eq!(unsafe { GetLastError() }, ERROR_IO_DEVICE);
    assert_eq!(halt.load(Ordering::Acquire), 2);
    assert_ne!(unsafe { WinUsb_ResetPipe(handles.usb, 2) }, 0);
    for n in 0..20u8 {
        sent.fill(n);
        let mut got = [0u8; 64];
        // SAFETY: Both calls are synchronous and bounded by the pipe timeout;
        // their buffers and count output remain alive until completion.
        unsafe {
            assert_ne!(
                WinUsb_WritePipe(
                    handles.usb,
                    2,
                    sent.as_ptr(),
                    64,
                    &mut count,
                    ptr::null_mut()
                ),
                0
            );
            assert_eq!(count, 64);
            assert_ne!(
                WinUsb_ReadPipe(
                    handles.usb,
                    0x82,
                    got.as_mut_ptr(),
                    64,
                    &mut count,
                    ptr::null_mut()
                ),
                0
            );
        }
        assert_eq!(count, 64);
        assert_eq!(got, sent);
    }
}

fn serial_ports() -> Vec<String> {
    let result = std::process::Command::new("powershell.exe")
        .args(["-NoProfile", "-Command", "Get-CimInstance Win32_SerialPort | Where-Object {$_.PNPDeviceID -like 'USB\\VID_1209&PID_0001*'} | ForEach-Object DeviceID"])
        .output().unwrap();
    assert!(result.status.success());
    String::from_utf8_lossy(&result.stdout)
        .lines()
        .map(str::trim)
        .filter(|s| s.starts_with("COM") && s[3..].bytes().all(|b| b.is_ascii_digit()))
        .map(str::to_string)
        .collect()
}

#[test]
#[ignore = "requires two separately installed controllers in KNIGHTBUS_TEST_CONTROLLERS"]
fn native_two_controllers_isolate_owner_exit() {
    let _guard = CONTROLLER_TEST.lock().unwrap();
    let instances = std::env::var("KNIGHTBUS_TEST_CONTROLLERS").unwrap();
    let ids: Vec<_> = instances.split(';').map(str::to_owned).collect();
    assert_eq!(ids.len(), 2);
    controllers_isolate(Some(ids));
}
#[test]
#[ignore = "requires administrator access and the staged KnightBus driver package"]
fn native_owned_controllers_isolate_owner_exit() {
    let _guard = CONTROLLER_TEST.lock().unwrap();
    assert!(owned_controllers().is_empty());
    controllers_isolate(None);
}
fn controllers_isolate(ids: Option<Vec<String>>) {
    assert!(serial_ports().is_empty(), "other CDC fixture present");
    let before = ids.as_ref().map(|ids| {
        Controller::open(Some(&ids[0]))
            .unwrap()
            .lifecycle()
            .unwrap()
    });
    let mut children = vec![];
    let mut peers = vec![];
    let mut servers = vec![];
    let mut counts = vec![];
    for index in 0..2 {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let (connected, connection) = std::sync::mpsc::channel();
        let count = Arc::new(AtomicUsize::new(0));
        counts.push(count.clone());
        servers.push(std::thread::spawn(move || {
            exporter(listener, count, Some(connected))
        }));
        let mut command =
            std::process::Command::new(env!("CARGO_BIN_EXE_knightbus-windows-importer"));
        command.args(["--insecure-lab", &address.to_string(), "1-1", "30"]);
        if let Some(ids) = &ids {
            command.arg(&ids[index]);
        }
        children.push(command.spawn().unwrap());
        peers.push(connection.recv_timeout(Duration::from_secs(5)).unwrap());
    }
    let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let deadline = Instant::now() + Duration::from_secs(10);
        let ports = loop {
            let ports = serial_ports();
            if ports.len() == 2 {
                break ports;
            }
            assert!(
                Instant::now() < deadline,
                "two CDC devices did not enumerate"
            );
            std::thread::sleep(Duration::from_millis(20));
        };
        for port in ports {
            echo(&port);
        }
        assert!(counts.iter().all(|c| c.load(Ordering::Acquire) >= 20));
        children[0].kill().unwrap();
        children[0].wait().unwrap();
        let deadline = Instant::now() + Duration::from_secs(10);
        let survivor = loop {
            let ports = serial_ports();
            if ports.len() == 1 {
                break ports[0].clone();
            }
            assert!(Instant::now() < deadline, "first device did not retire");
            std::thread::sleep(Duration::from_millis(20));
        };
        assert!(children[1].try_wait().unwrap().is_none());
        echo(&survivor);
        assert!(
            counts[1].load(Ordering::Acquire) >= 40,
            "surviving controller must still route data"
        );
    }));
    for child in &mut children {
        let _ = child.kill();
        let _ = child.wait();
    }
    for peer in peers {
        let _ = peer.shutdown(std::net::Shutdown::Both);
    }
    for server in servers {
        let _ = server.join();
    }
    outcome.unwrap();
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        if ids.is_none() {
            if owned_controllers().is_empty() && serial_ports().is_empty() {
                break;
            }
            assert!(
                Instant::now() < deadline,
                "owned controllers survived process exit"
            );
            std::thread::sleep(Duration::from_millis(20));
            continue;
        }
        let ids = ids.as_ref().unwrap();
        let before = before.unwrap();
        if let Ok(after) = Controller::open(Some(&ids[0])).unwrap().lifecycle() {
            if after[3] - before[3] == 2 {
                assert_eq!(after[1] - before[1], 2);
                assert_eq!(after[5] - before[5], after[7] - before[7]);
                assert_eq!(after[8] - before[8], after[10] - before[10]);
                break;
            }
        }
        assert!(
            Instant::now() < deadline,
            "both native children must retire"
        );
        std::thread::sleep(Duration::from_millis(10));
    }
}

fn owned_controllers() -> Vec<String> {
    let result = std::process::Command::new("powershell.exe")
        .args(["-NoProfile", "-Command", "Get-PnpDevice -PresentOnly | Where-Object {$_.InstanceId -like 'SWD\\KnightBus\\*'} | ForEach-Object InstanceId"])
        .output().unwrap();
    assert!(result.status.success());
    String::from_utf8_lossy(&result.stdout)
        .lines()
        .filter(|s| !s.trim().is_empty())
        .map(|s| s.trim().to_owned())
        .collect()
}
