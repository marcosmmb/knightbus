//! Single-device host with an explicit importer grant.
use knightbus_core::exporter::Backend;
use knightbus_core::*;
use knightbus_macos_exporter::native::{self, Native};
use knightbus_transport::exporter::{self as transport, Mono};
use std::{
    net::{Ipv4Addr, SocketAddr, TcpListener},
    path::Path,
    time::{Duration, Instant},
};

pub fn run(args: &[String]) -> std::result::Result<(), Box<dyn std::error::Error>> {
    let options = Options::parse(args)?;
    let secure = options.security.prepare()?;
    let registry = options.registry;
    let id = native::enumerate()?
        .into_iter()
        .find(|d| d.registry_id == registry)
        .ok_or(Error::NotFound)?;
    let snapshot = native::inspect(id, id.device_key())?;
    if snapshot
        .device
        .configurations
        .iter()
        .flat_map(|c| &c.interfaces)
        .any(|i| i.class.class == MASS_STORAGE_CLASS)
        && !options.unmounted
    {
        return Err(
            "storage capture requires clean unmount of every volume and --storage-unmounted".into(),
        );
    }
    let device = snapshot.device.clone();
    let key = device.key;
    let bus = String::from_utf8_lossy(device.wire.bus_id.as_bytes()).into_owned();
    let epoch = knightbus_transport::security::epoch()?;
    let mut engine = Engine::new(
        Mono::default(),
        Config {
            resources: RESOURCE_LIMITS,
            devices: 1,
            lease_ms: DEVICE_LEASE_MS,
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
    let caller = Caller {
        peer: secure.as_ref().map_or(LOOPBACK_PEER, |secure| secure.peer),
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
    let listener = TcpListener::bind(options.address)?;
    if let Some(secure) = secure {
        return secure.serve(engine, listener, &bus);
    }
    serve_insecure(engine, listener, caller, &bus, registry)
}

fn serve_insecure<B: ExporterBackend>(
    mut engine: Engine<B, Mono>,
    listener: TcpListener,
    caller: Caller,
    bus: &str,
    registry: u64,
) -> std::result::Result<(), Box<dyn std::error::Error>> {
    eprintln!("Raw USB/IP on loopback; use an authenticated tunnel to the selected importer.");
    println!(
        "listening={} bus_id={bus} registry=0x{registry:x}",
        listener.local_addr()?
    );
    let budget = engine.budget();
    // Discovery does not consume the native factory. A captured generation is
    // single-use: after disconnect, rediscover and explicitly authorize again.
    loop {
        let (stream, _) = listener.accept()?;
        let result = transport::serve_insecure(
            &mut engine,
            knightbus_transport::Stream::insecure_loopback(stream)?,
            caller,
        );
        if result.is_err() || engine.discover(caller).is_empty() {
            eprintln!("connection ended: {result:?}");
            break;
        }
    }
    engine.shutdown();
    let end = Instant::now() + CLEANUP_TIMEOUT;
    while !engine.shutdown_complete() {
        engine.tick();
        if Instant::now() >= end {
            return Err(Error::Cleanup.into());
        }
        std::thread::sleep(CLEANUP_INTERVAL);
    }
    drop(engine);
    println!("exporter shutdown; budget={:?}", budget.usage());
    Ok(())
}

const MASS_STORAGE_CLASS: u8 = 0x08;
const RESOURCE_LIMITS: ResourceLimits = ResourceLimits {
    bytes: 32 * 1024 * 1024,
    requests: 64,
};
const DEVICE_LEASE_MS: u64 = 60_000;
const CLEANUP_TIMEOUT: Duration = Duration::from_secs(30);
const CLEANUP_INTERVAL: Duration = Duration::from_millis(1);
const LOOPBACK_PEER: PeerId = PeerId([0x4d; 32]);

struct Options<'a> {
    registry: u64,
    address: SocketAddr,
    security: Security<'a>,
    unmounted: bool,
}
enum Security<'a> {
    Raw,
    Tls {
        control: SocketAddr,
        state: &'a Path,
        peer: PeerId,
        management: &'a Path,
    },
}
impl<'a> Options<'a> {
    fn parse(args: &'a [String]) -> std::result::Result<Self, Box<dyn std::error::Error>> {
        let (args, unmounted) = match args.split_last() {
            Some((flag, rest)) if flag == "--storage-unmounted" => (rest, true),
            _ => (args, false),
        };
        let (registry, address, security) = match args {
            [command, registry, port, capture, mode]
                if command == "serve"
                    && capture == "--capture"
                    && mode == "--insecure-loopback" =>
            {
                let port: u16 = port.parse()?;
                (
                    registry,
                    SocketAddr::from((Ipv4Addr::LOCALHOST, port)),
                    Security::Raw,
                )
            }
            [
                command,
                registry,
                address,
                control,
                state,
                peer,
                capture,
                management,
            ] if command == "serve" && capture == "--capture" => (
                registry,
                address.parse()?,
                Security::Tls {
                    control: control.parse()?,
                    state: Path::new(state),
                    peer: knightbus_transport::security::parse_fingerprint(peer)?,
                    management: Path::new(management),
                },
            ),
            _ => return Err(USAGE.into()),
        };
        Ok(Self {
            registry: u64::from_str_radix(registry.trim_start_matches("0x"), 16)?,
            address,
            security,
            unmounted,
        })
    }
}
impl Security<'_> {
    fn prepare(&self) -> Result<Option<SecureHost>> {
        let Self::Tls {
            control,
            state,
            peer,
            management,
        } = self
        else {
            return Ok(None);
        };
        validate_socket_location(state, management)?;
        let store = knightbus_transport::security::Store::open(state)?;
        let identity = store.identity()?;
        let peers = store.peers()?;
        if !peers.contains(*peer) {
            return Err(Error::Denied);
        }
        Ok(Some(SecureHost {
            identity,
            peers,
            peer: *peer,
            control: *control,
            management: knightbus_transport::local::Listener::bind(management)?,
            store,
        }))
    }
}
fn validate_socket_location(state: &Path, socket: &Path) -> Result<()> {
    let state = state.canonicalize().map_err(|_| Error::Invalid)?;
    let parent = socket
        .parent()
        .ok_or(Error::Invalid)?
        .canonicalize()
        .map_err(|_| Error::Invalid)?;
    if parent.starts_with(state) {
        return Err(Error::Invalid);
    }
    Ok(())
}

struct SecureHost {
    identity: knightbus_transport::security::Identity,
    peers: knightbus_transport::security::Peers,
    peer: PeerId,
    control: SocketAddr,
    management: knightbus_transport::local::Listener,
    store: knightbus_transport::security::Store,
}
impl SecureHost {
    fn serve<B: ExporterBackend + Send>(
        self,
        engine: Engine<B, Mono>,
        listener: TcpListener,
        bus: &str,
    ) -> std::result::Result<(), Box<dyn std::error::Error>> {
        let control = TcpListener::bind(self.control)?;
        println!(
            "usb={} control={} bus_id={bus}",
            listener.local_addr()?,
            control.local_addr()?
        );
        let host = knightbus_transport::host::Host::new(engine, self.identity, self.peers);
        host.run(listener, control, Some(&self.management), Some(&self.store))?;
        Ok(())
    }
}

const USAGE: &str = "usage: serve REGISTRY_ID USB_ADDRESS:PORT CONTROL_ADDRESS:PORT STATE_DIR PEER_FINGERPRINT --capture MANAGEMENT_SOCKET [--storage-unmounted]; raw compatibility: serve REGISTRY_ID PORT --capture --insecure-loopback [--storage-unmounted]";

#[cfg(test)]
mod tests {
    use super::*;
    fn args(values: &[&str]) -> Vec<String> {
        values.iter().map(|value| (*value).to_owned()).collect()
    }

    #[test]
    fn management_socket_cannot_enter_the_credential_directory() {
        let root = std::env::temp_dir().join(format!("kb-socket-location-{}", std::process::id()));
        let state = root.join("state");
        let control = root.join("control");
        std::fs::create_dir_all(&state).unwrap();
        std::fs::create_dir(&control).unwrap();
        assert_eq!(
            validate_socket_location(&state, &state.join("socket")),
            Err(Error::Invalid)
        );
        assert_eq!(
            validate_socket_location(&state, &state.join("../state/socket")),
            Err(Error::Invalid)
        );
        std::os::unix::fs::symlink(&state, root.join("alias")).unwrap();
        assert_eq!(
            validate_socket_location(&state, &root.join("alias/socket")),
            Err(Error::Invalid)
        );
        assert_eq!(
            validate_socket_location(&state, &control.join("socket")),
            Ok(())
        );
        assert!(!state.join("socket").exists());
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn secure_options_require_an_explicit_peer_and_capture_flag() {
        let fingerprint = "01".repeat(32);
        let mut values = args(&[
            "serve",
            "0xab",
            "127.0.0.1:3240",
            "127.0.0.1:3241",
            "/state",
            &fingerprint,
            "--capture",
            "/state/socket",
        ]);
        let options = Options::parse(&values).unwrap();
        assert_eq!(options.registry, 0xab);
        assert_eq!(options.address.port(), 3240);
        assert!(!options.unmounted);
        assert!(matches!(
            options.security,
            Security::Tls {
                peer: PeerId([1, ..]),
                ..
            }
        ));
        values.push("--storage-unmounted".into());
        assert!(Options::parse(&values).unwrap().unmounted);
        values[6] = "--unknown".into();
        assert!(Options::parse(&values).is_err());
        values[6] = "--capture".into();
        values[5] = "01".into();
        assert!(Options::parse(&values).is_err());
    }

    #[test]
    fn raw_options_are_explicit_loopback_only_and_reject_extra_arguments() {
        let mut values = args(&["serve", "ab", "3240", "--capture", "--insecure-loopback"]);
        let options = Options::parse(&values).unwrap();
        assert!(options.address.ip().is_loopback());
        assert!(matches!(options.security, Security::Raw));
        for length in 0..values.len() {
            assert!(Options::parse(&values[..length]).is_err());
        }
        values.push("--storage-unmounted".into());
        assert!(Options::parse(&values).unwrap().unmounted);
        values.push("extra".into());
        assert!(Options::parse(&values).is_err());
    }
}
