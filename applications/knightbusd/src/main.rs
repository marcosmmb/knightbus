//! Native companion process; its management endpoint never carries USB transfers.
#![cfg_attr(windows, windows_subsystem = "windows")]
use knightbus_sdk::{Runtime, config::Configuration, core::Error, ipc::local::Listener};
use std::path::Path;
#[cfg(target_os = "macos")]
mod install;
fn run() -> Result<(), Error> {
    let args = std::env::args().skip(1).collect::<Vec<_>>();
    #[cfg(target_os = "macos")]
    if let [mode, uid, source, fingerprint] = args.as_slice()
        && mode == "--install-desktop-service"
    {
        return install::install(
            uid.parse().map_err(|_| Error::Invalid)?,
            Path::new(source),
            fingerprint,
        );
    }
    #[cfg(target_os = "macos")]
    if let [mode, uid] = args.as_slice()
        && mode == "--start-desktop-service"
    {
        return install::restart(uid.parse().map_err(|_| Error::Invalid)?);
    }
    if args.is_empty() || matches!(args.as_slice(), [help] if help == "--help") {
        println!(
            "usage: knightbusd CONFIG ENDPOINT | --desktop STATE ENDPOINT\nmacOS root service: --desktop-user STATE ENDPOINT UID\nCreate a private endpoint parent first. Stop with knightbus manage ENDPOINT shutdown."
        );
        return Ok(());
    }
    #[cfg(target_os = "macos")]
    if let [mode, state, endpoint, uid] = args.as_slice()
        && (mode == "--desktop-user" || mode == "--desktop-app-user")
    {
        if endpoint == knightbus_sdk::SYSTEM_ENDPOINT {
            install::prepare_endpoint()?;
        }
        let listener = Listener::bind_for_user(
            Path::new(endpoint),
            uid.parse().map_err(|_| Error::Invalid)?,
        )?;
        let app_context = mode == "--desktop-app-user";
        if app_context
            && knightbus_sdk::storage_access::session_user()
                != Some(uid.parse().map_err(|_| Error::Invalid)?)
        {
            return Err(Error::Denied);
        }
        let service = std::sync::Arc::new(if app_context {
            knightbus_sdk::service::Service::open_desktop(Path::new(state))?
        } else {
            knightbus_sdk::service::Service::open(Path::new(state))?
        });
        let signal = service.clone();
        ctrlc::set_handler(move || signal.shutdown()).map_err(|_| Error::Unavailable)?;
        return service.serve(&listener);
    }
    if let [mode, state, endpoint] = args.as_slice()
        && mode == "--desktop"
    {
        let listener = Listener::bind(Path::new(endpoint))?;
        let service = std::sync::Arc::new(knightbus_sdk::service::Service::open(Path::new(state))?);
        let signal = service.clone();
        ctrlc::set_handler(move || signal.shutdown()).map_err(|_| Error::Unavailable)?;
        return service.serve(&listener);
    }
    let [config, endpoint] = args.as_slice() else {
        return Err(Error::Invalid);
    };
    let config = Configuration::read(Path::new(config))?;
    let endpoint = Path::new(endpoint);
    Runtime::validate_endpoint(&config, endpoint)?;
    let listener = Listener::bind(endpoint)?;
    let runtime = Runtime::start(&config)?;
    let signal_host = runtime.clone();
    ctrlc::set_handler(move || signal_host.shutdown()).map_err(|_| Error::Unavailable)?;
    eprintln!(
        "event=host_started control_version={}",
        knightbus_sdk::core::CONTROL_VERSION
    );
    let result = runtime.serve(&listener);
    eprintln!("event=host_stopped error={:?}", result.as_ref().err());
    result
}
fn main() {
    if let Err(error) = run() {
        eprintln!(
            "event=host_failed error={error:?}\n{}",
            knightbus_sdk::guidance(error)
        );
        std::process::exit(1);
    }
}
