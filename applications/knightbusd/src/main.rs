//! Native companion process; its management endpoint never carries USB transfers.
use knightbus_sdk::{Runtime, config::Configuration, core::Error, ipc::local::Listener};
use std::path::Path;
fn run() -> Result<(), Error> {
    let args = std::env::args().skip(1).collect::<Vec<_>>();
    if args.is_empty() || matches!(args.as_slice(), [help] if help == "--help") {
        println!(
            "usage: knightbusd CONFIG ENDPOINT\nCreate a private endpoint parent first. Stop with knightbus manage ENDPOINT shutdown."
        );
        return Ok(());
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
