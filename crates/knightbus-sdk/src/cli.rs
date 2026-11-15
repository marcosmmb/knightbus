//! Shared operator commands. Parsing and presentation only; all policy and native
//! lifecycle decisions remain in the control target.
use crate::{
    Client,
    config::{Configuration, number},
    policy::{ATTACH_TIMEOUT_MS, CONTROL_TIMEOUT_MS},
};
use knightbus_core::*;
use std::{path::Path, time::Duration};

const MAX_WATCH_SECONDS: u64 = 3_600;
// Ten requests/second leaves room under the IPC limit for negotiation and queries.
const CONTROL_POLL_INTERVAL: Duration = Duration::from_millis(100);

/// Command help for the unified KnightBus CLI.
pub const HELP: &str = "Usage: knightbus COMMAND [ARGUMENTS]\n\nlocal-list | local-inspect REGISTRY_ID\nidentity STATE | fingerprint STATE | pair STATE CERT.der CONFIRMED_FINGERPRINT | unpair STATE FINGERPRINT\nmanage ENDPOINT info|list|sessions|inspect ID GENERATION|permissions ID GENERATION|share ID GENERATION PEER|deny ID GENERATION PEER|unshare ID GENERATION|attach ID GENERATION|status EPOCH SERIAL|detach EPOCH SERIAL|forget EPOCH SERIAL|watch SECONDS|diagnostics OUTPUT|shutdown\ndiscover IMPORTER_CONFIG | browse SECONDS (mdns feature)\nIDs accept decimal or 0x-prefixed hex.";
/// Parse one management operation before opening an endpoint.
pub fn command(args: &[String]) -> Result<Command> {
    let key = |id: &str, generation: &str| -> Result<DeviceKey> {
        Ok(DeviceKey {
            id: DeviceId(number(id)?),
            generation: number(generation)?,
        })
    };
    let session = |epoch: &str, serial: &str| -> Result<SessionId> {
        Ok(SessionId {
            epoch: number(epoch)?,
            serial: number(serial)?,
        })
    };
    Ok(match args {
        [c] if c == "info" => Command::Info,
        [c] if c == "list" => Command::List,
        [c] if c == "sessions" => Command::Sessions,
        [c] if c == "shutdown" => Command::Shutdown,
        [c, id, generation] if c == "inspect" => Command::Inspect(key(id, generation)?),
        [c, id, generation] if c == "permissions" => Command::Permissions(key(id, generation)?),
        [c, id, generation] if c == "unshare" => Command::Unshare(key(id, generation)?),
        [c, id, generation] if c == "attach" => Command::Attach(vec![key(id, generation)?]),
        [c, id, generation, peer] if c == "share" || c == "grant" || c == "deny" => {
            Command::Authorize {
                device: key(id, generation)?,
                peer: knightbus_transport::security::parse_fingerprint(peer)?,
                allow: c != "deny",
            }
        }
        [c, epoch, serial] if c == "status" => Command::Status(session(epoch, serial)?),
        [c, epoch, serial] if c == "detach" => Command::Detach(session(epoch, serial)?),
        [c, epoch, serial] if c == "forget" => Command::Forget(session(epoch, serial)?),
        _ => return Err(Error::Invalid),
    })
}
/// Execute explicit pairing, discovery or management; never captures a local device.
pub fn run(args: &[String]) -> std::result::Result<(), Box<dyn std::error::Error>> {
    if args.is_empty() || matches!(args, [c] if c == "--help" || c == "help") {
        println!(
            "{HELP} Management timeout: {CONTROL_TIMEOUT_MS} ms; attach: {ATTACH_TIMEOUT_MS} ms."
        );
        return Ok(());
    }
    if let Some(result) = knightbus_transport::security::command(args) {
        return result;
    }
    match args {
        #[cfg(feature = "mdns")]
        [c, seconds] if c == "browse" => {
            let seconds = number(seconds)?;
            for hint in crate::discovery::browse(std::time::Duration::from_secs(seconds))? {
                println!("untrusted_hint={hint:?}");
            }
        }
        [c] if c == "local-list" => {
            for device in crate::physical::list()? {
                println!(
                    "registry=0x{:x} vid={:04x} pid={:04x} claimability=untested",
                    device.registry, device.vendor, device.product
                );
            }
        }
        [c, registry] if c == "local-inspect" => {
            println!(
                "{}claimability=untested",
                format_reply(&Reply::Inspection(Box::new(crate::physical::inspect(
                    number(registry)?
                )?)))
            );
        }
        [c, config] if c == "discover" => {
            for device in crate::importer::discover(&Configuration::read(Path::new(config))?)? {
                println!(
                    "bus={} vid={:04x} pid={:04x} speed={} interfaces={:?}",
                    String::from_utf8_lossy(device.device.bus_id.as_bytes()),
                    device.device.vendor_id,
                    device.device.product_id,
                    device.device.speed,
                    device.interfaces
                );
            }
        }
        [c, endpoint, commands @ ..] if c == "manage" => {
            let [operation, rest @ ..] = commands else {
                return Err(Error::Invalid.into());
            };
            if operation == "watch" {
                let [seconds] = rest else {
                    return Err(Error::Invalid.into());
                };
                let seconds = number(seconds)?;
                if !(1..=MAX_WATCH_SECONDS).contains(&seconds) {
                    return Err(Error::Invalid.into());
                }
                let mut client = Client::connect(Path::new(endpoint))?;
                let Reply::Subscription(id) =
                    client.request(Command::Subscribe, CONTROL_TIMEOUT_MS)?
                else {
                    return Err(Error::Internal.into());
                };
                let end = std::time::Instant::now() + std::time::Duration::from_secs(seconds);
                while std::time::Instant::now() < end {
                    if let Reply::Event(Some(event)) =
                        client.request(Command::Poll(id), CONTROL_TIMEOUT_MS)?
                    {
                        println!("{event:?}");
                    }
                    std::thread::sleep(CONTROL_POLL_INTERVAL);
                }
                client.request(Command::Unsubscribe(id), CONTROL_TIMEOUT_MS)?;
            } else if operation == "diagnostics" {
                let [output] = rest else {
                    return Err(Error::Invalid.into());
                };
                let mut client = Client::connect(Path::new(endpoint))?;
                let mut report = format!(
                    "KnightBus metadata diagnostics\nplatform={} arch={}\n",
                    std::env::consts::OS,
                    std::env::consts::ARCH
                );
                let mut failure = None;
                let mut collect = |command: Command| {
                    std::thread::sleep(CONTROL_POLL_INTERVAL);
                    let reply = client
                        .request(command.clone(), CONTROL_TIMEOUT_MS)
                        .and_then(|reply| match (&command, &reply) {
                            (Command::Info, Reply::Info { .. })
                            | (Command::Sessions, Reply::Sessions(_)) => Ok(reply),
                            (Command::Status(id), Reply::Session(session)) if *id == session.id => {
                                Ok(reply)
                            }
                            _ => Err(Error::Invalid),
                        });
                    if let Err(error) = reply {
                        failure.get_or_insert(error);
                    }
                    report.push_str(&format!("{command:?}={reply:?}\n"));
                    reply
                };
                let _ = collect(Command::Info);
                if let Ok(Reply::Sessions(ids)) = collect(Command::Sessions) {
                    for id in ids {
                        let _ = collect(Command::Status(id));
                    }
                }
                report.push_str(&format!("complete={}\n", failure.is_none()));
                use std::io::Write;
                let mut options = std::fs::OpenOptions::new();
                options.write(true).create_new(true);
                #[cfg(unix)]
                {
                    use std::os::unix::fs::OpenOptionsExt;
                    options.mode(crate::ipc::private::FILE_MODE);
                }
                options.open(output)?.write_all(report.as_bytes())?;
                if let Some(error) = failure {
                    eprintln!("diagnostics_written=true complete=false");
                    return Err(error.into());
                }
                println!("diagnostics_written=true complete=true");
            } else {
                let command = command(commands)?;
                let timeout = if matches!(command, Command::Attach(_)) {
                    ATTACH_TIMEOUT_MS
                } else {
                    CONTROL_TIMEOUT_MS
                };
                let mut client = Client::connect(Path::new(endpoint))?;
                println!("{}", format_reply(&client.request(command, timeout)?));
            }
        }
        _ => return Err(Error::Invalid.into()),
    }
    Ok(())
}
/// Consistent CLI exit behavior and sanitized, actionable errors.
pub fn main() {
    if let Err(error) = run(&std::env::args().skip(1).collect::<Vec<_>>()) {
        if let Some(error) = error.downcast_ref::<Error>() {
            eprintln!(
                "event=command_failed error={error:?}\n{}",
                crate::guidance(*error)
            );
        } else {
            eprintln!("event=command_failed error={error}");
        }
        std::process::exit(1);
    }
}

/// Human-readable descriptor and lifecycle output. No payloads, credentials or
/// unbounded native error strings enter this representation.
pub fn format_reply(reply: &Reply) -> String {
    use std::fmt::Write;
    let mut out = String::new();
    match reply {
        Reply::Info {
            version,
            epoch,
            now,
        } => {
            let _ = writeln!(out, "version={version} epoch={epoch} monotonic_ms={now}");
        }
        Reply::Done => out.push_str("accepted=true\n"),
        Reply::Devices(devices) => {
            for d in devices {
                let _ = writeln!(
                    out,
                    "id=0x{:x} generation={} busy={}",
                    d.key.id.0, d.key.generation, d.busy
                );
            }
        }
        Reply::Peers(peers) => {
            for peer in peers {
                let _ = writeln!(
                    out,
                    "peer={}",
                    knightbus_transport::security::display_fingerprint(*peer)
                );
            }
        }
        Reply::Sessions(ids) => {
            for id in ids {
                let _ = writeln!(out, "epoch={} serial={}", id.epoch, id.serial);
            }
        }
        Reply::Session(s) => {
            let _ = writeln!(
                out,
                "epoch={} serial={} state={:?} error={:?}",
                s.id.epoch, s.id.serial, s.state, s.error
            );
            for d in &s.devices {
                let _ = writeln!(out, "id=0x{:x} generation={}", d.id.0, d.generation);
            }
            if let Some(error) = s.error {
                let _ = writeln!(out, "{}", crate::guidance(error));
            }
        }
        Reply::Inspection(d) => {
            let _ = writeln!(
                out,
                "id=0x{:x} generation={} bus={:?} vid={:04x} pid={:04x} speed={} configuration={}",
                d.key.id.0,
                d.key.generation,
                String::from_utf8_lossy(d.wire.bus_id.as_bytes()),
                d.wire.vendor_id,
                d.wire.product_id,
                d.wire.speed,
                d.wire.configuration
            );
            let _ = writeln!(
                out,
                "device_revision=0x{:04x} class={:02x} subclass={:02x} protocol={:02x} configurations={}",
                d.wire.device_bcd,
                d.wire.class,
                d.wire.subclass,
                d.wire.protocol,
                d.wire.configurations
            );
            for c in &d.configurations {
                for i in &c.interfaces {
                    let _ = writeln!(
                        out,
                        "configuration={} interface={} alternate={} class={:02x} subclass={:02x} protocol={:02x}",
                        c.value,
                        i.number,
                        i.alternate,
                        i.class.class,
                        i.class.subclass,
                        i.class.protocol
                    );
                    for e in &i.endpoints {
                        let _ = writeln!(
                            out,
                            "  endpoint=0x{:02x} kind={:?} max_packet={} service_bytes={} interval={}",
                            e.address, e.kind, e.max_packet_size, e.service_bytes, e.interval
                        );
                    }
                }
            }
        }
        Reply::Subscription(id) => {
            let _ = writeln!(out, "subscription={id}");
        }
        Reply::Event(event) => {
            let _ = writeln!(out, "event={event:?}");
        }
    }
    out
}
