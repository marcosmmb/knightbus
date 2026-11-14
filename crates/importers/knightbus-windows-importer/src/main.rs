//! Standalone importer over an explicitly authorized raw USB/IP connection.
#[cfg(any(windows, test))]
mod options;

#[cfg(windows)]
fn main() -> Result<(), Box<dyn std::error::Error>> {
    use knightbus_core::{Budget, ResourceLimits, SessionId};
    use knightbus_windows_importer::{backend, native::Controller};
    use std::{
        net::TcpStream,
        task::Poll,
        time::{Duration, Instant},
    };
    const RESOURCE_LIMITS: ResourceLimits = ResourceLimits {
        bytes: 96 * 1024 * 1024,
        requests: 64,
    };
    const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);
    const CONNECT_TIMEOUT: Duration = Duration::from_secs(5);
    let options = options::Options::parse(&std::env::args().skip(1).collect::<Vec<_>>())?;
    eprintln!("Raw USB/IP does not authenticate peers; use an authenticated tunnel.");
    let budget = Budget::new(RESOURCE_LIMITS);
    let timeout = REQUEST_TIMEOUT;
    let session = SessionId {
        epoch: std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)?
            .as_nanos() as u64,
        serial: 1,
    };
    let mut forwarding = backend::connect(
        TcpStream::connect_timeout(&options.address, CONNECT_TIMEOUT)?,
        options.bus,
        session,
        budget.clone(),
        timeout,
        || Ok(Controller::owned()),
    )?;
    let device = forwarding.device().ok_or(knightbus_core::Error::State)?;
    eprintln!(
        "Imported descriptors {:04x}:{:04x}, speed {}, {} interfaces",
        device.wire.vendor_id, device.wire.product_id, device.wire.speed, device.wire.interfaces
    );
    let start = Instant::now();
    loop {
        if options.stop.requested(start) {
            forwarding.disconnect();
        }
        if let Poll::Ready(result) = forwarding.tick() {
            result?;
            break;
        }
        forwarding.wait();
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
