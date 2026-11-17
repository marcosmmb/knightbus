//! Idle-capable service host. Owns setup and native runtimes independently of UI
//! connections. Runtime changes never replay grants, device claims or attachments.
use crate::{
    Runtime,
    config::{Configuration, Role},
    core::*,
    ipc::local::{Cancellation, Listener},
};
use knightbus_transport::security::{self, Store};
use std::{
    path::{Path, PathBuf},
    sync::Mutex,
    time::{Duration, Instant},
};

const MAX_CONNECTIONS: usize = 16;
const CAPACITY_POLL: Duration = Duration::from_millis(20);
const BROWSE_DURATION: Duration = Duration::from_secs(2);

const HEX_DIGITS_PER_BYTE: usize = 2;

struct Selection {
    runtime: Runtime,
    config: Configuration,
}
#[cfg(feature = "mdns")]
struct Presence {
    announcement: crate::discovery::Announcement,
    local: host::NearbyComputer,
    port: Option<u16>,
}
/// Account-scoped administrator host. No device is selected at startup.
pub struct Service {
    state: PathBuf,
    app_context: bool,
    clock: Instant,
    driver: Option<host::DriverStatus>,
    epoch: u64,
    selected: Mutex<Option<Selection>>,
    stop: Cancellation,
    #[cfg(feature = "mdns")]
    presence: Mutex<Option<Presence>>,
    pairing: Mutex<Option<crate::pairing::Job>>,
}
impl Service {
    /// Open an existing identity, or initialize a new absent directory once.
    pub fn open(state: &Path) -> Result<Self> {
        if !state.exists() {
            Store::initialize(state)?;
        }
        Store::open(state)?;
        Ok(Self {
            state: state.into(),
            app_context: false,
            clock: Instant::now(),
            driver: driver_status(),
            epoch: security::epoch()?,
            selected: Mutex::new(None),
            stop: Cancellation::default(),
            #[cfg(feature = "mdns")]
            presence: Mutex::new(None),
            pairing: Mutex::new(None),
        })
    }
    /// Open the privileged desktop helper after the app authorizes its startup.
    #[cfg(target_os = "macos")]
    pub fn open_desktop(state: &Path) -> Result<Self> {
        if !capture_ready() {
            return Err(Error::Denied);
        }
        let mut service = Self::open(state)?;
        service.app_context = true;
        Ok(service)
    }
    /// Cancel admission. Native retirement is performed by the serving owner.
    pub fn shutdown(&self) {
        self.stop.cancel();
        if let Ok(mut pairing) = self.pairing.lock() {
            pairing.take();
        }
    }
    /// Execute the shared control contract with trusted local authority.
    pub fn execute(&self, caller: Caller, command: Command, deadline: u64) -> Result<Reply> {
        if !caller.administrator {
            return Err(Error::Denied);
        }
        let now = self.clock.elapsed().as_millis() as u64;
        if now >= deadline {
            return Err(Error::Deadline);
        }
        if self.stop.is_cancelled() {
            return Err(Error::State);
        }
        if matches!(command, Command::Info) {
            return Ok(Reply::Info {
                version: CONTROL_VERSION,
                epoch: self.epoch,
                now,
            });
        }
        if matches!(command, Command::Shutdown) {
            self.shutdown();
            return Ok(Reply::Done);
        }
        let mut selected = self.selected.lock().map_err(|_| Error::Internal)?;
        // Account for time waiting behind another administrator before any mutation.
        if self.clock.elapsed().as_millis() as u64 >= deadline {
            return Err(Error::Deadline);
        }
        if let Command::Host(request) = command {
            return self.setup(request, &mut selected);
        }
        if let Command::Authorize {
            peer, allow: true, ..
        } = &command
            && !Store::open(&self.state)?.peers()?.contains(*peer)
        {
            return Err(Error::Denied);
        }
        let Some(selection) = selected.as_ref() else {
            return match command {
                Command::List => Ok(Reply::Devices(vec![])),
                Command::Sessions => Ok(Reply::Sessions(vec![])),
                _ => Err(Error::State),
            };
        };
        let mut endpoint = selection.runtime.endpoint();
        let remaining = deadline.saturating_sub(self.clock.elapsed().as_millis() as u64);
        let native_deadline = endpoint.now().saturating_add(remaining);
        endpoint.execute(caller, command, native_deadline)
    }
    fn setup(&self, request: host::Request, selected: &mut Option<Selection>) -> Result<Reply> {
        let store = Store::open(&self.state)?;
        match request {
            host::Request::Status => {
                self.refresh_presence()?;
                let identity = store.identity()?;
                let (role, selection) = selected.as_ref().map_or(("idle", String::new()), |s| {
                    let role = if matches!(s.config.role, Role::Exporter { .. }) {
                        "exporter"
                    } else {
                        "importer"
                    };
                    (
                        role,
                        format!(
                            "{:?}; USB {}; control {}; host {}",
                            s.config.role,
                            s.config.usb,
                            s.config.control,
                            if s.runtime.stopped() {
                                "stopped"
                            } else {
                                "running"
                            }
                        ),
                    )
                });
                Ok(Reply::Host(host::Reply::Status(host::Status {
                    fingerprint: security::display_fingerprint(identity.peer()), certificate: hex(identity.certificate()),
                    peers: store.peers()?.identities().map(security::display_fingerprint).collect(),
                    exporter: cfg!(target_os = "macos"), importer: cfg!(windows), discovery: cfg!(feature = "mdns"), pin_pairing: true, process_id: std::process::id(), capture_ready: capture_ready(), storage_consent_ready: capture_ready() && self.app_context, driver: self.driver, role: role.into(), selection,
                    guidance: if cfg!(target_os = "macos") {
                        if capture_ready() { "Administrator capture is available. Only an authorized device attachment takes control from OS drivers; replug or release requires a fresh selection." } else { "Approve administrator setup when sharing a device to enable USB capture. Pairing and inspection do not need capture access." }
                    } else if cfg!(windows) {
                        match self.driver { Some(host::DriverStatus::Missing) => "The KnightBus USB driver is missing. Install a Windows-trusted driver package, then restart the service.", Some(host::DriverStatus::Staged) => "A compatible driver package is staged. Driver ABI, load policy and controller access are checked at attachment.", _ => "The Windows driver store could not be checked. Repair the installed service and retry." }
                    } else { "No native exporter or importer is available in this build." }.into(),
                })))
            }
            host::Request::Physical => {
                let devices = crate::physical::shareable()?;
                if devices.len() > host::MAX_PHYSICAL_DEVICES {
                    return Err(Error::Exhausted);
                }
                Ok(Reply::Host(host::Reply::Physical(
                    devices
                        .into_iter()
                        .map(|d| host::Physical {
                            id: d.id.0.to_string(),
                            registry: d.registry.to_string(),
                            vendor: d.vendor,
                            product: d.product,
                            name: d.name,
                        })
                        .collect(),
                )))
            }
            host::Request::Inspect { registry } => Ok(Reply::Inspection(Box::new(
                crate::physical::inspect(crate::config::number(&registry)?)?,
            ))),
            host::Request::Pair {
                certificate,
                fingerprint,
            } => {
                store.pair(
                    unhex(&certificate)?,
                    security::parse_fingerprint(&fingerprint)?,
                )?;
                Ok(Reply::Done)
            }
            host::Request::Revoke { fingerprint } => {
                let peer = security::parse_fingerprint(&fingerprint)?;
                self.pairing.lock().map_err(|_| Error::Internal)?.take();
                store.revoke(peer)?;
                // Remove live grants immediately as well as transport trust on its next tick.
                if let Some(s) = selected.as_ref()
                    && matches!(s.config.role, Role::Exporter { .. })
                {
                    let mut target = s.runtime.endpoint();
                    if let Reply::Devices(devices) =
                        target.execute(crate::runtime::ADMIN, Command::List, u64::MAX)?
                    {
                        for d in devices {
                            target.execute(
                                crate::runtime::ADMIN,
                                Command::Authorize {
                                    device: d.key,
                                    peer,
                                    allow: false,
                                },
                                u64::MAX,
                            )?;
                        }
                    }
                }
                Ok(Reply::Done)
            }
            host::Request::Export {
                registry,
                usb,
                control,
                capture,
                storage_unmounted,
                announce,
            } => {
                let config = Configuration {
                    state: self.state.clone(),
                    usb: address(&usb)?,
                    control: address(&control)?,
                    mdns_name: if announce {
                        Some(format!("knightbus-{:x}", self.epoch))
                    } else {
                        None
                    },
                    role: Role::Exporter {
                        registry: crate::config::number(&registry)?,
                        capture,
                        storage_unmounted,
                        grants: vec![],
                    },
                };
                if let Some(selection) = selected.as_ref() {
                    // Keep the listener pair, permissions and active native workers.
                    if !matches!(selection.config.role, Role::Exporter { .. })
                        || selection.config.usb != config.usb
                        || selection.config.control != config.control
                    {
                        return Err(Error::State);
                    }
                    selection.runtime.add_exporter(&config)?;
                    Ok(Reply::Done)
                } else {
                    self.select(selected, config)
                }
            }
            host::Request::Import {
                fingerprint,
                usb,
                control,
            } => {
                let peer = security::parse_fingerprint(&fingerprint)?;
                if !store.peers()?.contains(peer) {
                    return Err(Error::Denied);
                }
                let config = Configuration {
                    state: self.state.clone(),
                    usb: address(&usb)?,
                    control: address(&control)?,
                    mdns_name: None,
                    role: Role::Importer { peer },
                };
                self.select(selected, config)
            }
            host::Request::Clear => {
                Self::clear(selected)?;
                Ok(Reply::Done)
            }
            host::Request::PairingStart { name, usb, control } => {
                let local = crate::pairing::contact(&store, name, usb, control)?;
                let mut pairing = self.pairing.lock().map_err(|_| Error::Internal)?;
                pairing.take();
                let (job, offer) = crate::pairing::Job::start(self.state.clone(), local)?;
                *pairing = Some(job);
                drop(pairing);
                self.refresh_presence()?;
                Ok(Reply::Host(host::Reply::PairingOffer(offer)))
            }
            host::Request::PairingJoin {
                address: remote,
                pin,
                name,
                usb,
                control,
            } => {
                let local = crate::pairing::contact(&store, name, usb, control)?;
                let remote = address(&remote)?;
                knightbus_transport::pairing::validate_pin(&pin.0)?;
                let mut pairing = self.pairing.lock().map_err(|_| Error::Internal)?;
                pairing.take();
                *pairing = Some(crate::pairing::Job::join(
                    self.state.clone(),
                    local,
                    remote,
                    pin,
                )?);
                drop(pairing);
                self.refresh_presence()?;
                Ok(Reply::Done)
            }
            host::Request::PairingStatus => {
                let progress = self
                    .pairing
                    .lock()
                    .map_err(|_| Error::Internal)?
                    .as_ref()
                    .map_or(
                        Ok(host::PairingProgress::Idle),
                        crate::pairing::Job::progress,
                    )?;
                self.refresh_presence()?;
                Ok(Reply::Host(host::Reply::PairingProgress(progress)))
            }
            host::Request::PairingCancel => {
                self.pairing.lock().map_err(|_| Error::Internal)?.take();
                self.refresh_presence()?;
                Ok(Reply::Done)
            }
            host::Request::Advertise {
                enabled,
                name,
                usb,
                control,
            } => {
                #[cfg(feature = "mdns")]
                {
                    let port = self.pairing_port()?;
                    let mut presence = self.presence.lock().map_err(|_| Error::Internal)?;
                    if !enabled {
                        *presence = None;
                    } else {
                        let local = crate::pairing::contact(&store, name, usb, control)?;
                        let usb = address(&local.usb)?;
                        let control = address(&local.control)?;
                        if let Some(p) = presence.as_mut() {
                            p.announcement.update_computer(
                                &local.name,
                                &local.fingerprint,
                                usb,
                                control,
                                port,
                            )?;
                            p.local = local;
                            p.port = port;
                        } else {
                            let announcement = crate::discovery::Announcement::computer(
                                &local.name,
                                &local.fingerprint,
                                usb,
                                control,
                                port,
                            )?;
                            *presence = Some(Presence {
                                announcement,
                                local,
                                port,
                            });
                        }
                    }
                    Ok(Reply::Done)
                }
                #[cfg(not(feature = "mdns"))]
                {
                    let _ = (enabled, name, usb, control);
                    Err(Error::Unsupported)
                }
            }
            host::Request::Discover => {
                #[cfg(feature = "mdns")]
                {
                    let own = security::display_fingerprint(store.identity()?.peer());
                    let computers = crate::discovery::computers(BROWSE_DURATION)?
                        .into_iter()
                        .filter(|c| c.fingerprint != own)
                        .collect();
                    Ok(Reply::Host(host::Reply::Nearby(computers)))
                }
                #[cfg(not(feature = "mdns"))]
                {
                    Err(Error::Unsupported)
                }
            }
            host::Request::Browse => {
                #[cfg(feature = "mdns")]
                {
                    Ok(Reply::Host(host::Reply::Hints(
                        crate::discovery::browse(BROWSE_DURATION)?
                            .into_iter()
                            .map(|h| (h.usb.to_string(), h.control.to_string()))
                            .collect(),
                    )))
                }
                #[cfg(not(feature = "mdns"))]
                {
                    let _ = BROWSE_DURATION;
                    Err(Error::Unsupported)
                }
            }
        }
    }
    #[cfg(feature = "mdns")]
    fn pairing_port(&self) -> Result<Option<u16>> {
        Ok(self
            .pairing
            .lock()
            .map_err(|_| Error::Internal)?
            .as_ref()
            .and_then(crate::pairing::Job::port))
    }
    fn refresh_presence(&self) -> Result<()> {
        #[cfg(feature = "mdns")]
        {
            let port = self.pairing_port()?;
            if let Some(p) = self.presence.lock().map_err(|_| Error::Internal)?.as_mut()
                && p.port != port
            {
                p.announcement.update_computer(
                    &p.local.name,
                    &p.local.fingerprint,
                    address(&p.local.usb)?,
                    address(&p.local.control)?,
                    port,
                )?;
                p.port = port;
            }
        }
        Ok(())
    }
    fn select(&self, selected: &mut Option<Selection>, config: Configuration) -> Result<Reply> {
        // Require explicit Clear so a failed new selection cannot destroy a working one.
        if selected.is_some() {
            return Err(Error::State);
        }
        *selected = Some(Selection {
            runtime: Runtime::start(&config)?,
            config,
        });
        Ok(Reply::Done)
    }
    fn clear(selected: &mut Option<Selection>) -> Result<()> {
        if let Some(s) = selected.as_ref() {
            let mut endpoint = s.runtime.endpoint();
            if let Reply::Sessions(ids) =
                endpoint.execute(crate::runtime::ADMIN, Command::Sessions, u64::MAX)?
            {
                for id in ids {
                    if !matches!(
                        endpoint.execute(crate::runtime::ADMIN, Command::Status(id), u64::MAX)?,
                        Reply::Session(Session {
                            state: SessionState::Closed,
                            ..
                        })
                    ) {
                        return Err(Error::State);
                    }
                }
            } else {
                return Err(Error::Internal);
            }
            s.runtime.shutdown();
            s.runtime.join()?;
        }
        *selected = None;
        Ok(())
    }
    /// Serve bounded authenticated connections; clients never own this host's lifetime.
    pub fn serve(&self, listener: &Listener) -> Result<()> {
        let result = std::thread::scope(|scope| {
            let mut workers = Vec::new();
            while !self.stop.is_cancelled() {
                let _ = self.refresh_presence();
                if let Ok(selected) = self.selected.try_lock()
                    && let Some(selection) = selected.as_ref()
                {
                    while let Some(audit) = selection.runtime.audit() {
                        eprintln!("event=audit kind={:?} error={:?}", audit.kind, audit.error);
                    }
                }
                workers.retain(|w: &std::thread::ScopedJoinHandle<'_, ()>| !w.is_finished());
                if workers.len() >= MAX_CONNECTIONS {
                    std::thread::sleep(CAPACITY_POLL);
                    continue;
                }
                match listener.accept() {
                    Ok(Some(connection)) => {
                        workers.push(scope.spawn(|| {
                            let _ = connection.serve(&self.stop, |caller, command, deadline| {
                                self.execute(caller, command, deadline)
                            });
                        }));
                    }
                    Ok(None) | Err(Error::Denied) => {}
                    Err(error) => {
                        self.stop.cancel();
                        return Err(error);
                    }
                }
            }
            Ok(())
        });
        self.pairing.lock().map_err(|_| Error::Internal)?.take();
        if let Some(s) = self.selected.lock().map_err(|_| Error::Internal)?.take() {
            s.runtime.shutdown();
            s.runtime.join()?;
        }
        result
    }
}
fn address(value: &str) -> Result<std::net::SocketAddr> {
    let address: std::net::SocketAddr = value.parse().map_err(|_| Error::Invalid)?;
    if address.port() == 0 || address.ip().is_multicast() {
        return Err(Error::Invalid);
    }
    Ok(address)
}
/// Encode a public certificate for text exchange.
pub fn hex(bytes: &[u8]) -> String {
    use std::fmt::Write;
    let mut out = String::new();
    for b in bytes {
        let _ = write!(out, "{b:02x}");
    }
    out
}
fn unhex(value: &str) -> Result<Vec<u8>> {
    if value.len() > security::MAX_CREDENTIAL_BYTES * HEX_DIGITS_PER_BYTE
        || !value.len().is_multiple_of(HEX_DIGITS_PER_BYTE)
        || !value.bytes().all(|b| b.is_ascii_hexdigit())
    {
        return Err(Error::Invalid);
    }
    value
        .as_bytes()
        .as_chunks::<HEX_DIGITS_PER_BYTE>()
        .0
        .iter()
        .map(|pair| {
            u8::from_str_radix(std::str::from_utf8(pair).map_err(|_| Error::Invalid)?, 16)
                .map_err(|_| Error::Invalid)
        })
        .collect()
}

fn capture_ready() -> bool {
    #[cfg(target_os = "macos")]
    {
        rustix::process::geteuid().is_root()
    }
    #[cfg(not(target_os = "macos"))]
    {
        false
    }
}

fn driver_status() -> Option<host::DriverStatus> {
    #[cfg(windows)]
    {
        Some(match knightbus_windows_importer::native::driver_staged() {
            Ok(true) => host::DriverStatus::Staged,
            Ok(false) => host::DriverStatus::Missing,
            Err(_) => host::DriverStatus::Unavailable,
        })
    }
    #[cfg(not(windows))]
    {
        None
    }
}
