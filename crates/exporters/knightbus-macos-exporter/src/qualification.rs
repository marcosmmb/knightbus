//! Bounded, opt-in tests on the explicitly selected Logitech joystick.
use knightbus_core::{
    usbip::{Direction, SubmitHeader},
    *,
};
use knightbus_macos_exporter::constants::{lab, status, usb};
use knightbus_macos_exporter::{
    Backend, Driver,
    native::{self, Native},
};
use std::time::Instant;

struct Mono(Instant);
impl Clock for Mono {
    fn now(&self) -> u64 {
        self.0.elapsed().as_millis() as u64
    }
}
type Factory = Box<dyn FnOnce() -> Result<Native> + Send>;
type Host = Engine<Backend<Factory>, Mono>;
pub fn capture_worker(location: u32, cycles: u32) -> Result<()> {
    use knightbus_macos_exporter::journal;
    if cycles == 0 || cycles > lab::MAX_CYCLES {
        return Err(Error::Invalid);
    }
    for cycle in 1..=cycles {
        let cycle_span = journal::begin(
            "cycle.capture-worker",
            &format!("cycle={cycle} location={location:08x}"),
        )?;
        let id = selected(location)?;
        let snapshot = native::inspect(id, native::lab_key(id))?;
        let device = snapshot.device.clone();
        let budget = Budget::new(ResourceLimits {
            bytes: lab::BUDGET_BYTES,
            requests: knightbus_macos_exporter::SLOTS,
        });
        let b = budget.clone();
        let factory: Factory = Box::new(move || Native::open(snapshot, true, b));
        let mut backend = Backend::new(device.clone(), &budget, factory)?;
        let session = SessionId {
            epoch: 1,
            serial: u64::from(cycle),
        };
        until(|| match backend.claim(session, &device) {
            std::task::Poll::Pending => Ok(false),
            std::task::Poll::Ready(r) => r.map(|_| true),
        })?;
        journal::begin(
            "worker.claimed",
            &format!("cycle={cycle} registry={:x}", id.registry_id),
        )?
        .finish("no endpoint transfers or topology changes");
        until(|| match backend.release(session, device.key) {
            std::task::Poll::Pending => Ok(false),
            std::task::Poll::Ready(r) => r.map(|_| true),
        })?;
        drop(backend);
        until(|| Ok(budget.usage().bytes == 0))?;
        until(|| Ok(selected(location).is_ok_and(|new| new.registry_id != id.registry_id)))?;
        cycle_span.finish("released; new generation observed; credits=0");
    }
    Ok(())
}
fn until(mut condition: impl FnMut() -> Result<bool>) -> Result<()> {
    let deadline = Instant::now() + lab::WAIT_TIMEOUT;
    while !condition()? {
        if Instant::now() >= deadline {
            return Err(Error::Deadline);
        }
        std::thread::sleep(lab::POLL_INTERVAL);
    }
    Ok(())
}
fn exchange(
    engine: &mut Host,
    caller: Caller,
    session: SessionId,
    header: SubmitHeader,
    cancel: bool,
) -> Result<()> {
    let length = header.transfer_buffer_length;
    let sequence = header.seqnum;
    let transfer = Transfer::new(&engine.budget(), header, &[])?;
    engine.submit(
        caller,
        session,
        transfer,
        engine.now() + lab::TRANSFER_DEADLINE_MS,
    )?;
    if cancel {
        engine.unlink(caller, session, sequence + 1, sequence)?;
    }
    until(|| {
        engine.tick();
        match engine.next_transfer_event(caller, session)? {
            Some(TransferEvent::Complete(done)) => {
                println!(
                    "engine sequence={sequence} status={} bytes={}",
                    done.status, done.actual_length
                );
                if done.status != 0 || done.actual_length != length {
                    return Err(Error::Internal);
                }
                Ok(true)
            }
            Some(TransferEvent::Unlinked { .. }) if cancel => {
                println!("engine sequence={sequence} unlink=PASS");
                Ok(true)
            }
            Some(_) => Err(Error::Internal),
            None => Ok(false),
        }
    })
}
fn attach_engine(snapshot: native::Snapshot, epoch: u64) -> Result<(Host, Caller, Session)> {
    let device = snapshot.device.clone();
    let key = device.key;
    let mut engine = Engine::with_backend_factory(
        Mono(Instant::now()),
        Config {
            lease_ms: lab::LEASE_MS,
            ..Config::default()
        },
        epoch,
        |budget| {
            let b = budget.clone();
            let factory: Factory = Box::new(move || Native::open(snapshot, true, b));
            Backend::new(device.clone(), budget, factory)
        },
    )?;
    engine.register(device)?;
    let caller = Caller {
        peer: PeerId(lab::PEER_FINGERPRINT),
        administrator: false,
    };
    let admin = Caller {
        administrator: true,
        ..caller
    };
    assert!(engine.discover(caller).is_empty());
    let mut control =
        knightbus_embedded::InProcessControl::new(&mut engine, admin, CONTROL_VERSION)?;
    control.call(
        Command::Authorize {
            device: key,
            peer: caller.peer,
            allow: true,
        },
        u64::MAX,
    )?;
    drop(control);
    let mut control =
        knightbus_embedded::InProcessControl::new(&mut engine, caller, CONTROL_VERSION)?;
    let Reply::Session(session) = control.call(Command::Attach(vec![key]), u64::MAX)? else {
        return Err(Error::Internal);
    };
    drop(control);
    until(|| {
        engine.tick();
        let Reply::Session(s) = engine.execute(caller, Command::Status(session.id), u64::MAX)?
        else {
            return Err(Error::Internal);
        };
        if let Some(e) = s.error {
            return Err(e);
        }
        Ok(s.state == SessionState::Active)
    })?;
    Ok((engine, caller, session))
}
pub fn run(location: u32, cycles: u32) -> Result<()> {
    if cycles == 0 || cycles > lab::MAX_CYCLES {
        return Err(Error::Invalid);
    }
    for cycle in 0..cycles {
        let cycle_span = knightbus_macos_exporter::journal::begin(
            "cycle.engine",
            &format!("cycle={} location={location:08x}", cycle + 1),
        )?;
        let id = selected(location)?;
        let snapshot = native::inspect(id, native::lab_key(id))?;
        let device = snapshot.device.clone();
        let wire_id = (device.wire.bus_number << usb::WIRE_BUS_SHIFT) | device.wire.device_number;
        let ep = device
            .configurations
            .iter()
            .filter(|c| c.value == device.wire.configuration)
            .flat_map(|c| &c.interfaces)
            .filter(|i| i.alternate == 0)
            .flat_map(|i| &i.endpoints)
            .find(|e| e.kind == TransferKind::Interrupt && e.address & usb::DIRECTION_IN != 0)
            .ok_or(Error::Unsupported)?
            .clone();
        let (mut engine, caller, session) = attach_engine(snapshot, u64::from(cycle) + 1)?;
        println!(
            "cycle={} engine_claim=PASS registry=0x{:x}",
            cycle + 1,
            id.registry_id
        );
        let mut sequence = 1;
        // Generic controls: real device/report descriptors, current state, then
        // explicit SET_INTERFACE and SET_CONFIGURATION using declared values.
        for setup in [
            usb::READ_DEVICE_DESCRIPTOR,
            usb::setup(
                usb::DIRECTION_IN | usb::RECIPIENT_INTERFACE,
                usb::GET_DESCRIPTOR,
                u16::from(usb::DESCRIPTOR_HID_REPORT) << u8::BITS,
                0,
                lab::REPORT_DESCRIPTOR_LENGTH,
            ),
            usb::READ_CONFIGURATION,
            usb::setup(usb::RECIPIENT_INTERFACE, usb::SET_INTERFACE, 0, 0, 0),
            usb::setup(
                usb::RECIPIENT_DEVICE,
                usb::SET_CONFIGURATION,
                lab::CONFIGURATION,
                0,
                0,
            ),
        ] {
            exchange(
                &mut engine,
                caller,
                session.id,
                SubmitHeader {
                    seqnum: sequence,
                    device_id: wire_id,
                    direction: if setup[usb::SETUP_TYPE] & usb::DIRECTION_IN != 0 {
                        Direction::In
                    } else {
                        Direction::Out
                    },
                    endpoint: 0,
                    transfer_flags: 0,
                    transfer_buffer_length: u32::from(u16::from_le_bytes([
                        setup[usb::SETUP_LENGTH],
                        setup[usb::SETUP_LENGTH + 1],
                    ])),
                    start_frame: 0,
                    number_of_packets: 0,
                    interval: 0,
                    setup,
                },
                false,
            )?;
            sequence += 1;
        }
        for cancel in [false, true] {
            exchange(
                &mut engine,
                caller,
                session.id,
                SubmitHeader {
                    seqnum: sequence,
                    device_id: wire_id,
                    direction: Direction::In,
                    endpoint: u32::from(ep.address & usb::ENDPOINT_NUMBER_MASK),
                    transfer_flags: 0,
                    transfer_buffer_length: u32::from(
                        ep.max_packet_size & usb::MAX_PACKET_SIZE_MASK,
                    ),
                    start_frame: 0,
                    number_of_packets: 0,
                    interval: i32::from(ep.interval),
                    setup: [0; usb::SETUP_SIZE],
                },
                cancel,
            )?;
            sequence += 2;
        }
        engine.shutdown();
        until(|| {
            engine.tick();
            Ok(engine.shutdown_complete())
        })?;
        let budget = engine.budget();
        drop(engine);
        until(|| Ok(budget.usage().bytes == 0 && budget.usage().requests == 0))?;
        println!("cycle={} shutdown=PASS credits=0", cycle + 1);
        // Device capture release intentionally resets/rematches. Re-enumerate;
        // never reuse its old registry handle or assume the old generation lives.
        until(|| Ok(selected(location).is_ok_and(|new| new.registry_id != id.registry_id)))?;
        cycle_span.finish("shutdown complete; credits=0");
    }
    Ok(())
}
pub fn unplug(location: u32, idle: bool) -> Result<()> {
    let span = knightbus_macos_exporter::journal::begin(
        "cycle.hot-unplug",
        &format!("location={location:08x}"),
    )?;
    let id = selected(location)?;
    let snapshot = native::inspect(id, native::lab_key(id))?;
    let wire = (snapshot.device.wire.bus_number << usb::WIRE_BUS_SHIFT)
        | snapshot.device.wire.device_number;
    let (mut engine, caller, session) = attach_engine(snapshot, 1)?;
    let header = |seqnum| SubmitHeader {
        seqnum,
        device_id: wire,
        direction: Direction::In,
        endpoint: lab::INPUT_ENDPOINT,
        transfer_flags: 0,
        transfer_buffer_length: lab::REPORT_LENGTH,
        start_frame: 0,
        number_of_packets: 0,
        interval: lab::INPUT_INTERVAL,
        setup: [0; usb::SETUP_SIZE],
    };
    exchange(&mut engine, caller, session.id, header(1), false)?;
    knightbus_macos_exporter::journal::begin(
        "hot-unplug.ready",
        if idle {
            "waiting for physical removal; no pending requests or new transfers"
        } else {
            "waiting for physical removal; interrupt traffic active"
        },
    )?
    .finish("ready");
    println!(
        "unplug=READY Disconnect only the Logitech joystick now; leave it disconnected until the trial finishes."
    );
    use std::io::Write;
    std::io::stdout().flush().map_err(|_| Error::Internal)?;
    let deadline = Instant::now() + lab::UNPLUG_TIMEOUT;
    let mut pending = false;
    let mut sequence = 2;
    let mut completed = 0;
    loop {
        engine.tick();
        while let Some(event) = engine.next_transfer_event(caller, session.id)? {
            match event {
                // A disconnected transaction can complete with EPROTO before
                // the independent IOService termination notification arrives.
                TransferEvent::Complete(done)
                    if [0, status::NO_DEVICE, status::PROTOCOL, status::CANCELLED]
                        .contains(&done.status) =>
                {
                    if done.status != 0 {
                        println!(
                            "unplug_transfer_status={} bytes={}",
                            done.status, done.actual_length
                        );
                    }
                    pending = false;
                    completed += 1;
                }
                _ => return Err(Error::Internal),
            }
        }
        let Reply::Session(state) =
            engine.execute(caller, Command::Status(session.id), u64::MAX)?
        else {
            return Err(Error::Internal);
        };
        if state.state == SessionState::Closed {
            println!(
                "unplug_session=Closed cause={:?} visible_devices={}",
                state.error,
                engine.discover(caller).len()
            );
            if state.error != Some(Error::Unavailable) || !engine.discover(caller).is_empty() {
                return Err(Error::Internal);
            }
            break;
        }
        if state.state == SessionState::Quarantined {
            return Err(Error::Cleanup);
        }
        if Instant::now() >= deadline {
            return Err(Error::Deadline);
        }
        if !idle && state.state == SessionState::Active && !pending {
            let transfer = Transfer::new(&engine.budget(), header(sequence), &[])?;
            match engine.submit(
                caller,
                session.id,
                transfer,
                engine.now() + lab::TRANSFER_DEADLINE_MS,
            ) {
                Ok(()) => pending = true,
                // Physical removal may race the engine's last status read.
                // Wait for its own removal notification; do not synthesize one.
                Err(Error::Unavailable) => {}
                Err(error) => return Err(error),
            }
            sequence += 1;
        }
        std::thread::sleep(lab::POLL_INTERVAL);
    }
    if selected(location).is_ok() {
        return Err(Error::Internal);
    }
    engine.shutdown();
    until(|| {
        engine.tick();
        Ok(engine.shutdown_complete())
    })?;
    let budget = engine.budget();
    drop(engine);
    until(|| Ok(budget.usage().bytes == 0 && budget.usage().requests == 0))?;
    println!(
        "hot_unplug=PASS idle={idle} engine_session=Closed cause=Unavailable inventory_removed=PASS retired_completions={completed} credits=0"
    );
    span.finish("physical removal reached engine; session closed; inventory removed; credits=0");
    Ok(())
}
pub fn reset(location: u32) -> Result<()> {
    let id = selected(location)?;
    let snapshot = native::inspect(id, native::lab_key(id))?;
    let budget = Budget::new(ResourceLimits {
        bytes: lab::BUDGET_BYTES,
        requests: knightbus_macos_exporter::SLOTS,
    });
    let mut driver = Native::open(snapshot, true, budget.clone())?;
    driver.reset()?;
    assert!(driver.removed());
    driver.release()?;
    drop(driver);
    until(|| Ok(selected(location).is_ok_and(|new| new.registry_id != id.registry_id)))?;
    if budget.usage().bytes != 0 {
        return Err(Error::Cleanup);
    }
    println!("physical_reset=PASS generation_retired=PASS credits=0");
    Ok(())
}
/// Selected-joystick error/recovery fixture. All operations still use the
/// generic native backend; no device behavior is emulated.
pub fn faults(location: u32) -> Result<()> {
    let span = knightbus_macos_exporter::journal::begin(
        "cycle.native-faults",
        &format!("location={location:08x}"),
    )?;
    let id = selected(location)?;
    let snapshot = native::inspect(id, native::lab_key(id))?;
    let wire = (snapshot.device.wire.bus_number << usb::WIRE_BUS_SHIFT)
        | snapshot.device.wire.device_number;
    let budget = Budget::new(ResourceLimits {
        bytes: lab::BUDGET_BYTES,
        requests: knightbus_macos_exporter::SLOTS,
    });
    let mut driver = Native::open(snapshot, true, budget.clone())?;
    // Reserved descriptor type must STALL; a subsequent valid request checks
    // endpoint-zero recovery. Clear-halt is tested on a healthy interrupt pipe,
    // so it does not qualify recovery from a physically stalled data endpoint.
    let cases = [
        (
            "control-stall",
            usb::setup(
                usb::DIRECTION_IN,
                usb::GET_DESCRIPTOR,
                u16::from(lab::RESERVED_DESCRIPTOR_TYPE) << u8::BITS,
                0,
                usb::DEVICE_DESCRIPTOR_LENGTH as u16,
            ),
            0,
            usb::DEVICE_DESCRIPTOR_LENGTH as u32,
            false,
            status::PIPE,
            0,
        ),
        (
            "control-recovery",
            usb::READ_DEVICE_DESCRIPTOR,
            0,
            usb::DEVICE_DESCRIPTOR_LENGTH as u32,
            false,
            0,
            usb::DEVICE_DESCRIPTOR_LENGTH as u32,
        ),
        (
            "clear-halt-healthy",
            usb::setup(
                usb::RECIPIENT_ENDPOINT,
                usb::CLEAR_FEATURE,
                usb::ENDPOINT_HALT,
                u16::from(lab::INPUT_ADDRESS),
                0,
            ),
            0,
            0,
            false,
            0,
            0,
        ),
        (
            "interrupt-after-clear",
            [0; usb::SETUP_SIZE],
            lab::INPUT_ENDPOINT,
            lab::REPORT_LENGTH,
            false,
            0,
            lab::REPORT_LENGTH,
        ),
        (
            "native-abort",
            [0; usb::SETUP_SIZE],
            lab::INPUT_ENDPOINT,
            lab::REPORT_LENGTH,
            true,
            status::CANCELLED,
            0,
        ),
    ];
    let abort_case = *cases.last().unwrap();
    let mut aborted = false;
    for (i, (name, setup, endpoint, length, cancel, status, bytes)) in cases
        .into_iter()
        .chain(std::iter::repeat_n(abort_case, lab::ABORT_ATTEMPTS - 1))
        .enumerate()
    {
        if cancel && aborted {
            break;
        }
        let token = TransferId {
            session: SessionId {
                epoch: 1,
                serial: 1,
            },
            sequence: i as u32 + 1,
        };
        let transfer = Transfer::new(
            &budget,
            SubmitHeader {
                seqnum: token.sequence,
                device_id: wire,
                direction: if endpoint != 0 || setup[usb::SETUP_TYPE] & usb::DIRECTION_IN != 0 {
                    Direction::In
                } else {
                    Direction::Out
                },
                endpoint,
                transfer_flags: 0,
                transfer_buffer_length: length,
                start_frame: 0,
                number_of_packets: 0,
                interval: if endpoint == 0 {
                    0
                } else {
                    lab::INPUT_INTERVAL
                },
                setup,
            },
            &[],
        )?;
        driver
            .submit(token, transfer)
            .map_err(|(e, _)| knightbus_core::Error::from(e))?;
        if cancel {
            let _ = driver.cancel(token);
        }
        until(|| {
            let Some((actual, done)) = driver.completion() else {
                return Ok(false);
            };
            println!(
                "fault={name} status={} bytes={}",
                done.status, done.actual_length
            );
            let completion_won = cancel && done.status == 0 && done.actual_length == length;
            if actual != token
                || (!completion_won && (done.status != status || done.actual_length != bytes))
            {
                return Err(Error::Internal);
            }
            aborted |= cancel && done.status == status::CANCELLED;
            Ok(true)
        })?;
    }
    println!(
        "native_abort={}",
        if aborted {
            "PASS"
        } else {
            "UNPROVEN completion won all eight attempts"
        }
    );
    driver.reset()?;
    if !driver.removed() {
        return Err(Error::Internal);
    }
    driver.release()?;
    drop(driver);
    until(|| Ok(selected(location).is_ok_and(|new| new.registry_id != id.registry_id)))?;
    if budget.usage().bytes != 0 || budget.usage().requests != 0 {
        return Err(Error::Cleanup);
    }
    println!("physical_reset=PASS generation_retired=PASS credits=0");
    span.finish(&format!(
        "error recovery and explicit reset passed; native_aborted={aborted}; credits=0"
    ));
    Ok(())
}
fn selected(location: u32) -> Result<native::Identity> {
    native::enumerate()?
        .into_iter()
        .find(|d| d.location == location && d.vendor == lab::VENDOR && d.product == lab::PRODUCT)
        .ok_or(Error::NotFound)
}
pub fn hold(location: u32) -> Result<()> {
    let id = selected(location)?;
    let snapshot = native::inspect(id, native::lab_key(id))?;
    let budget = Budget::new(ResourceLimits {
        bytes: lab::BUDGET_BYTES,
        requests: knightbus_macos_exporter::SLOTS,
    });
    let wire = (snapshot.device.wire.bus_number << usb::WIRE_BUS_SHIFT)
        | snapshot.device.wire.device_number;
    let mut driver = Native::open(snapshot, true, budget.clone())?;
    let id = TransferId {
        session: SessionId {
            epoch: 1,
            serial: 1,
        },
        sequence: 1,
    };
    let transfer = Transfer::new(
        &budget,
        SubmitHeader {
            seqnum: 1,
            device_id: wire,
            direction: Direction::In,
            endpoint: lab::INPUT_ENDPOINT,
            transfer_flags: 0,
            transfer_buffer_length: lab::REPORT_LENGTH,
            start_frame: 0,
            number_of_packets: 0,
            interval: 1,
            setup: [0; usb::SETUP_SIZE],
        },
        &[],
    )?;
    driver
        .submit(id, transfer)
        .map_err(|(e, _)| knightbus_core::Error::from(e))?;
    println!("crash_probe=READY");
    use std::io::Write;
    std::io::stdout().flush().map_err(|_| Error::Internal)?;
    std::thread::sleep(lab::HOLD_DURATION);
    let _ = driver.cancel(id);
    until(|| Ok(driver.completion().is_some()))?;
    driver.release()
}
pub fn access(location: u32) -> Result<()> {
    let id = selected(location)?;
    let snapshot = native::inspect(id, native::lab_key(id))?;
    let budget = Budget::new(ResourceLimits {
        bytes: lab::BUDGET_BYTES,
        requests: knightbus_macos_exporter::SLOTS,
    });
    match Native::open(snapshot, false, budget) {
        Ok(mut d) => {
            d.release()?;
            println!("ordinary_claim=PASS");
        }
        Err(e) => println!(
            "ordinary_claim={e} ioreturn={:?}",
            native::last_native_error().map(|v| format!("0x{v:08x}"))
        ),
    }
    Ok(())
}
