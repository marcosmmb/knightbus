//! Explicit development runner; production authentication/CLI hosting is later.
#[cfg(windows)]
fn main() -> Result<(), Box<dyn std::error::Error>> {
    use knightbus_core::{Budget, ResourceLimits, SessionId, usbip::WireString};
    use knightbus_windows_importer::{
        backend::Backend,
        native::Controller,
        transport::{Forwarder, Wire},
    };
    use std::{
        net::{SocketAddr, TcpStream},
        task::Poll,
        time::{Duration, Instant},
    };
    const MAX_LAB_SECONDS: u64 = 300;
    const LAB_LIMITS: ResourceLimits = ResourceLimits {
        bytes: 96 * 1024 * 1024,
        requests: 64,
    };
    const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);
    const CONNECT_TIMEOUT: Duration = Duration::from_secs(5);
    const TICK_INTERVAL: Duration = Duration::from_millis(1);
    const MODE_ARG: usize = 0;
    const ADDRESS_ARG: usize = 1;
    const BUS_ARG: usize = 2;
    const SECONDS_ARG: usize = 3;
    const REQUIRED_ARGS: usize = 4;
    const CONTROLLER_ARG: usize = REQUIRED_ARGS;
    let args: Vec<String> = std::env::args().skip(1).collect();
    if !(REQUIRED_ARGS..=REQUIRED_ARGS + 1).contains(&args.len())
        || args[MODE_ARG] != "--insecure-lab"
    {
        return Err("usage: knightbus-windows-importer --insecure-lab ADDRESS:PORT BUS_ID SECONDS|until-stop=ABSOLUTE_PATH [CONTROLLER_INSTANCE]; raw USB/IP has no peer authentication".into());
    }
    let address: SocketAddr = args[ADDRESS_ARG].parse()?;
    let bus = WireString::new(args[BUS_ARG].as_bytes())?;
    let stop_file = args[SECONDS_ARG]
        .strip_prefix("until-stop=")
        .map(std::path::PathBuf::from);
    if stop_file
        .as_ref()
        .is_some_and(|p| !p.is_absolute() || p.exists())
    {
        return Err("stop file must be an absolute, initially absent path".into());
    }
    let seconds: u64 = if stop_file.is_some() {
        u64::MAX
    } else {
        args[SECONDS_ARG].parse()?
    };
    if stop_file.is_none() && !(1..=MAX_LAB_SECONDS).contains(&seconds) {
        return Err(format!("lab duration must be 1..{MAX_LAB_SECONDS} seconds").into());
    }
    eprintln!(
        "Development-only raw USB/IP: unauthenticated peer; use only the isolated test lab or an authenticated tunnel."
    );
    let budget = Budget::new(LAB_LIMITS);
    let timeout = REQUEST_TIMEOUT;
    let mut wire = Wire::new(
        TcpStream::connect_timeout(&address, CONNECT_TIMEOUT)?,
        budget.clone(),
        timeout,
    )?;
    let (device, descriptors) = wire.import(bus)?;
    eprintln!(
        "Imported descriptors {:04x}:{:04x}, speed {}, {} interfaces",
        device.device().wire.vendor_id,
        device.device().wire.product_id,
        device.device().wire.speed,
        descriptors.topology.interfaces
    );
    let instance = args.get(CONTROLLER_ARG).cloned();
    let backend = Backend::new(
        move || match instance.as_deref() {
            Some(id) => Controller::open(Some(id)),
            None => Ok(Controller::owned()),
        },
        descriptors,
        budget.clone(),
    )?;
    let session = SessionId {
        epoch: std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)?
            .as_nanos() as u64,
        serial: 1,
    };
    let mut forwarding = Forwarder::new(backend, wire, device, session, budget.clone(), timeout)?;
    let start = Instant::now();
    loop {
        if start.elapsed() >= Duration::from_secs(seconds)
            || stop_file.as_ref().is_some_and(|p| p.exists())
        {
            forwarding.disconnect();
        }
        if let Poll::Ready(result) = forwarding.tick() {
            result?;
            break;
        }
        std::thread::sleep(TICK_INTERVAL);
    }
    drop(forwarding);
    // The worker publishes retirement after its native owners have been dropped.
    eprintln!(
        "Native detach confirmed; remaining budget: {:?}",
        budget.usage()
    );
    Ok(())
}
#[cfg(not(windows))]
fn main() {
    eprintln!(
        "Native Windows runner is available only on Windows; portable tests run on all hosts."
    );
    std::process::exit(1);
}
