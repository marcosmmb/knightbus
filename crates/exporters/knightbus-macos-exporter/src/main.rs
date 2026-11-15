//! USB discovery and an explicitly authorized single-device exporter.
#[cfg(target_os = "macos")]
mod standalone;

#[cfg(target_os = "macos")]
fn run() -> Result<(), Box<dyn std::error::Error>> {
    use knightbus_macos_exporter::native;
    let args: Vec<_> = std::env::args().skip(1).collect();
    if let Some(result) = knightbus_transport::host::command(&args) {
        return result;
    }
    if let Some(result) = knightbus_transport::security::command(&args) {
        return result;
    }
    match args.as_slice() {
        [] => list(),
        [command] if command == "list" => list(),
        [command, registry] if command == "inspect" => {
            let registry = u64::from_str_radix(registry.trim_start_matches("0x"), 16)?;
            let id = native::enumerate()?.into_iter().find(|d| d.registry_id == registry)
                .ok_or("physical generation absent")?;
            let snapshot = native::inspect(id, id.device_key())?;
            println!("device_descriptor={:02x?}", snapshot.descriptor);
            for configuration in snapshot.configurations {
                println!("configuration_descriptor={configuration:02x?}");
            }
            Ok(())
        }
        [command, ..] if command == "serve" => standalone::run(&args),
        _ => Err("usage: list | inspect REGISTRY_ID | serve REGISTRY_ID PORT --capture --insecure-loopback [--storage-unmounted]".into()),
    }
}

#[cfg(target_os = "macos")]
fn list() -> Result<(), Box<dyn std::error::Error>> {
    for d in knightbus_macos_exporter::native::enumerate()? {
        println!(
            "registry=0x{:x} location=0x{:08x} vid={:04x} pid={:04x} speed={}",
            d.registry_id, d.location, d.vendor, d.product, d.speed
        );
    }
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
        eprintln!("Physical USB access requires macOS");
        std::process::exit(1);
    }
}
