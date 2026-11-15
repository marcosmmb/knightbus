//! Standalone importer with authenticated transport or explicit loopback compatibility.
#[cfg(any(windows, test))]
mod options;

#[cfg(windows)]
fn main() -> Result<(), Box<dyn std::error::Error>> {
    use knightbus_core::{Budget, ResourceLimits, SessionId};
    use knightbus_windows_importer::{backend, native::Controller};
    use std::{
        task::Poll,
        time::{Duration, Instant},
    };
    const RESOURCE_LIMITS: ResourceLimits = ResourceLimits {
        bytes: 96 * 1024 * 1024,
        requests: 64,
    };
    const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);
    let args = std::env::args().skip(1).collect::<Vec<_>>();
    if let Some(result) = knightbus_transport::host::command(&args) {
        return result;
    }
    if let Some(result) = knightbus_transport::security::command(&args) {
        return result;
    }
    let options = options::Options::parse(&args)?;
    let (stream, remote) = connect(&options)?;
    let budget = Budget::new(RESOURCE_LIMITS);
    let timeout = REQUEST_TIMEOUT;
    let session = SessionId {
        epoch: knightbus_transport::security::epoch()?,
        serial: 1,
    };
    let mut forwarding = backend::connect(
        stream,
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
        if options.stop.requested(start) || remote.as_ref().is_some_and(|r| r.check().is_err()) {
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

#[cfg(windows)]
fn connect(
    options: &options::Options,
) -> Result<
    (
        knightbus_transport::Stream,
        Option<knightbus_transport::host::Remote>,
    ),
    Box<dyn std::error::Error>,
> {
    use std::{net::TcpStream, time::Duration};
    const CONNECT_TIMEOUT: Duration = Duration::from_secs(5);
    Ok(match &options.security {
        options::Security::Raw => {
            eprintln!("Raw USB/IP on loopback requires an authenticated tunnel.");
            (
                knightbus_transport::Stream::insecure_loopback(TcpStream::connect_timeout(
                    &options.address,
                    CONNECT_TIMEOUT,
                )?)?,
                None,
            )
        }
        options::Security::Tls {
            control,
            state,
            peer,
        } => {
            let store = knightbus_transport::security::Store::open(state)?;
            let identity = store.identity()?;
            let peers = store.peers()?;
            let mut remote = knightbus_transport::host::Remote::connect(
                TcpStream::connect_timeout(control, CONNECT_TIMEOUT)?,
                &identity,
                &peers,
                *peer,
            )?;
            let stream = remote.usb(
                TcpStream::connect_timeout(&options.address, CONNECT_TIMEOUT)?,
                &identity,
                &peers,
            )?;
            (stream, Some(remote))
        }
    })
}
