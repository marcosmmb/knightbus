//! Explicit single-device development host; loopback only, no implicit grants.
use knightbus_core::*;
use knightbus_macos_exporter::{
    Backend,
    native::{self, Native},
    transport::{self, Mono},
};
use std::{
    net::{Ipv4Addr, TcpListener},
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

pub fn run(args: &[String]) -> std::result::Result<(), Box<dyn std::error::Error>> {
    if !(5..=6).contains(&args.len())
        || args[3] != "--capture"
        || args[4] != "--insecure-loopback"
        || (args.len() == 6 && args[5] != "--storage-unmounted")
    {
        return Err("usage: serve REGISTRY_ID PORT --capture --insecure-loopback [--storage-unmounted]; storage requires all volumes cleanly unmounted first".into());
    }
    let registry = u64::from_str_radix(args[1].trim_start_matches("0x"), 16)?;
    let port: u16 = args[2].parse()?;
    let id = native::enumerate()?
        .into_iter()
        .find(|d| d.registry_id == registry)
        .ok_or(Error::NotFound)?;
    let snapshot = native::inspect(id, native::lab_key(id))?;
    if snapshot
        .device
        .configurations
        .iter()
        .flat_map(|c| &c.interfaces)
        .any(|i| i.class.class == 8)
        && args.len() != 6
    {
        return Err(
            "storage capture requires clean unmount of every volume and --storage-unmounted".into(),
        );
    }
    let device = snapshot.device.clone();
    let key = device.key;
    let bus = String::from_utf8_lossy(device.wire.bus_id.as_bytes()).into_owned();
    let epoch = SystemTime::now().duration_since(UNIX_EPOCH)?.as_nanos() as u64;
    let mut engine = Engine::with_backend_factory(
        Mono::default(),
        Config {
            resources: ResourceLimits {
                bytes: 32 * 1024 * 1024,
                requests: 64,
            },
            devices: 1,
            lease_ms: 60_000,
            ..Config::default()
        },
        epoch,
        |budget| {
            let b = budget.clone();
            Backend::new(device.clone(), budget, move || {
                Native::open(snapshot, true, b)
            })
        },
    )?;
    engine.register(device)?;
    // This is explicitly an insecure local lab principal, not a certificate or
    // proof of remote identity. The operator establishes the external SSH tunnel.
    let caller = Caller {
        peer: PeerId([0x4d; 32]),
        administrator: false,
    };
    engine.execute(
        Caller {
            administrator: true,
            ..caller
        },
        Command::Authorize {
            device: key,
            peer: caller.peer,
            allow: true,
        },
        u64::MAX,
    )?;
    let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, port))?;
    eprintln!(
        "Development-only raw USB/IP on loopback; use an authenticated tunnel to the selected importer."
    );
    println!(
        "listening={} bus_id={bus} registry=0x{registry:x}",
        listener.local_addr()?
    );
    let budget = engine.budget();
    // Discovery does not consume the native factory. A captured generation is
    // single-use: after disconnect, rediscover and explicitly authorize again.
    loop {
        let (stream, _) = listener.accept()?;
        let result = transport::serve(&mut engine, stream, caller);
        if result.is_err() || engine.discover(caller).is_empty() {
            eprintln!("connection ended: {result:?}");
            break;
        }
    }
    engine.shutdown();
    let end = Instant::now() + Duration::from_secs(30);
    while !engine.shutdown_complete() {
        engine.tick();
        if Instant::now() >= end {
            return Err(Error::Cleanup.into());
        }
        std::thread::sleep(Duration::from_millis(1));
    }
    drop(engine);
    println!("exporter shutdown; budget={:?}", budget.usage());
    Ok(())
}
