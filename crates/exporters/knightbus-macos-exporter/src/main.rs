//! Explicit, bounded physical-device diagnostics. No network listener or capture
//! during enumeration. Hardware payloads and serial strings are never printed.
#[cfg(target_os = "macos")]
use knightbus_macos_exporter::constants::{lab, status, usb};
#[cfg(target_os = "macos")]
mod qualification;
#[cfg(target_os = "macos")]
fn run() -> Result<(), Box<dyn std::error::Error>> {
    use knightbus_core::{
        Budget, ResourceLimits, SessionId, Transfer, TransferId,
        usbip::{Direction, SubmitHeader},
    };
    use knightbus_macos_exporter::{Driver, native};
    use std::time::Instant;
    let mut args: Vec<_> = std::env::args().skip(1).collect();
    if let Some(index) = args.iter().position(|arg| arg == "--lab-journal") {
        let directory = args
            .get(index + 1)
            .ok_or("--lab-journal requires a prepared directory")?
            .clone();
        knightbus_macos_exporter::journal::initialize(std::path::Path::new(&directory))?;
        args.drain(index..index + 2);
    }
    if args == ["journal-check"] {
        if !knightbus_macos_exporter::journal::enabled() {
            return Err("prepared journal required".into());
        }
        knightbus_macos_exporter::journal::begin("recorder.selftest", "no USB APIs")?
            .finish("PASS");
        return Ok(());
    }
    if args.len() == 4 && args[0] == "capture-worker" && args[3] == "--capture" {
        let location = u32::from_str_radix(args[1].trim_start_matches("0x"), 16)?;
        qualification::capture_worker(location, args[2].parse()?)?;
        return Ok(());
    }
    if args.len() == 4 && args[0] == "access-then-qualify" && args[3] == "--capture" {
        let location = u32::from_str_radix(args[1].trim_start_matches("0x"), 16)?;
        let span = knightbus_macos_exporter::journal::begin(
            "prefix.ordinary-root-access",
            &format!("location={location:08x}"),
        )?;
        qualification::access(location)?;
        span.finish("ordinary access diagnostic returned; starting capture cycles");
        qualification::run(location, args[2].parse()?)?;
        return Ok(());
    }
    if args.len() >= 2
        && [
            "qualify",
            "reset",
            "faults",
            "unplug",
            "idle-unplug",
            "access",
            "hold",
        ]
        .contains(&args[0].as_str())
    {
        let location = u32::from_str_radix(args[1].trim_start_matches("0x"), 16)?;
        match args[0].as_str() {
            "qualify" if args.len()==4 && args[3]=="--capture"=>qualification::run(location,args[2].parse()?)?,
            "reset" if args.len()==3 && args[2]=="--capture"=>qualification::reset(location)?,
            "faults" if args.len()==3 && args[2]=="--capture"=>qualification::faults(location)?,
            "unplug" if args.len()==3 && args[2]=="--capture"=>qualification::unplug(location, false)?,
            "idle-unplug" if args.len()==3 && args[2]=="--capture"=>qualification::unplug(location, true)?,
            "access" if args.len()==2=>qualification::access(location)?,
            "hold" if args.len()==3 && args[2]=="--capture"=>qualification::hold(location)?,
            _=>return Err("usage: qualify LOCATION CYCLES --capture | reset LOCATION --capture | access LOCATION".into()),
        }
        return Ok(());
    }
    let devices = native::enumerate()?;
    if args.is_empty() || args == ["list"] {
        for d in devices {
            println!(
                "registry=0x{:x} location=0x{:08x} vid={:04x} pid={:04x} speed={}",
                d.registry_id, d.location, d.vendor, d.product, d.speed
            );
        }
        return Ok(());
    }
    if args.len() < 2 || !["inspect", "probe"].contains(&args[0].as_str()) {
        return Err("usage: list | inspect REGISTRY_ID | probe REGISTRY_ID --capture".into());
    }
    let registry = u64::from_str_radix(args[1].trim_start_matches("0x"), 16)?;
    let id = devices
        .into_iter()
        .find(|d| d.registry_id == registry)
        .ok_or("physical generation absent")?;
    let snapshot = native::inspect(id, native::lab_key(id))?;
    println!(
        "descriptors=PASS device_bytes={} config_bytes={:?} interfaces={}",
        snapshot.descriptor.len(),
        snapshot
            .configurations
            .iter()
            .map(Vec::len)
            .collect::<Vec<_>>(),
        snapshot.device.wire.interfaces
    );
    if args[0] == "inspect" {
        return Ok(());
    }
    if args.len() != 3
        || args[2] != "--capture"
        || id.vendor != lab::VENDOR
        || id.product != lab::PRODUCT
    {
        return Err("probe requires --capture and the explicitly selected Logitech 046d:c215; other devices require a device-specific safe test".into());
    }
    let endpoint = snapshot
        .device
        .configurations
        .iter()
        .filter(|c| c.value == snapshot.device.wire.configuration)
        .flat_map(|c| &c.interfaces)
        .filter(|i| i.alternate == 0)
        .flat_map(|i| &i.endpoints)
        .find(|e| {
            e.address & usb::DIRECTION_IN != 0 && e.kind == knightbus_core::TransferKind::Interrupt
        })
        .ok_or("no interrupt IN endpoint")?
        .clone();
    let budget = Budget::new(ResourceLimits {
        bytes: lab::BUDGET_BYTES,
        requests: knightbus_macos_exporter::SLOTS,
    });
    let wire_id = (snapshot.device.wire.bus_number << usb::WIRE_BUS_SHIFT)
        | snapshot.device.wire.device_number;
    let mut driver = native::Native::open(snapshot, true, budget.clone())?;
    println!("claim=PASS");
    for sequence in 1..=lab::PROBE_CANCEL_SEQUENCE {
        let token = TransferId {
            session: SessionId {
                epoch: 1,
                serial: 1,
            },
            sequence,
        };
        let transfer = Transfer::new(
            &budget,
            SubmitHeader {
                seqnum: sequence,
                device_id: wire_id,
                direction: Direction::In,
                endpoint: u32::from(endpoint.address & usb::ENDPOINT_NUMBER_MASK),
                transfer_flags: 0,
                transfer_buffer_length: u32::from(
                    endpoint.max_packet_size & usb::MAX_PACKET_SIZE_MASK,
                ),
                start_frame: 0,
                number_of_packets: 0,
                interval: i32::from(endpoint.interval),
                setup: [0; usb::SETUP_SIZE],
            },
            &[],
        )?;
        driver
            .submit(token, transfer)
            .map_err(|(e, _)| knightbus_core::Error::from(e))?;
        let start = Instant::now();
        if sequence == lab::PROBE_CANCEL_SEQUENCE {
            let _ = driver.cancel(token);
        }
        loop {
            if let Some((actual, done)) = driver.completion() {
                println!(
                    "sequence={} status={} bytes={} elapsed_us={} cancel={}",
                    actual.sequence,
                    done.status,
                    done.actual_length,
                    start.elapsed().as_micros(),
                    sequence == lab::PROBE_CANCEL_SEQUENCE
                );
                if sequence <= lab::PROBE_REPORTS && (done.status != 0 || done.actual_length == 0) {
                    return Err("interrupt transfer failed".into());
                }
                if sequence == lab::PROBE_CANCEL_SEQUENCE
                    && ![0, status::CANCELLED].contains(&done.status)
                {
                    return Err("cancel failed".into());
                }
                break;
            }
            if start.elapsed() > lab::CANCEL_AFTER {
                let _ = driver.cancel(token);
            }
            if start.elapsed() > lab::WAIT_TIMEOUT {
                eprintln!(
                    "cleanup=QUARANTINED: no terminal callback; retained storage until process exit"
                );
                std::process::exit(2);
            }
            std::thread::sleep(lab::POLL_INTERVAL);
        }
    }
    driver.release()?;
    drop(driver);
    println!(
        "release=PASS budget_bytes={} requests={}",
        budget.usage().bytes,
        budget.usage().requests
    );
    Ok(())
}
fn main() {
    #[cfg(target_os = "macos")]
    if let Err(error) = run() {
        eprintln!(
            "error={error} ioreturn={:?}",
            knightbus_macos_exporter::native::last_native_error().map(|v| format!("0x{v:08x}"))
        );
        std::process::exit(1);
    }
    #[cfg(not(target_os = "macos"))]
    {
        eprintln!("IOUSBHost diagnostics require macOS");
        std::process::exit(1);
    }
}
