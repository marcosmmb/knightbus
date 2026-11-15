use crate::config::{Configuration, Role};
use knightbus_core::*;
use knightbus_transport::{
    exporter::Mono,
    host::{Audit, Host},
    security::{Identity, Peers, Store},
};
use std::{
    net::TcpListener,
    path::Path,
    sync::{Arc, Mutex},
    thread::JoinHandle,
};

const MAX_CONTROL_CONNECTIONS: usize = 16;
const CONNECTION_CAPACITY_POLL: std::time::Duration = std::time::Duration::from_millis(20);
#[cfg(target_os = "macos")]
const EXPORTER_RESOURCES: ResourceLimits = ResourceLimits {
    bytes: 32 * 1024 * 1024,
    requests: 64,
};
#[cfg(target_os = "macos")]
const EXPORTER_LEASE_MS: u64 = 60_000;

pub(crate) const ADMIN: Caller = Caller {
    peer: PeerId([0; PEER_ID_BYTES]),
    administrator: true,
};
pub(crate) trait Managed: Send + Sync {
    fn execute(&self, caller: Caller, command: Command, deadline: u64) -> Result<Reply>;
    fn shutdown(&self);
    fn audit(&self) -> Option<Audit> {
        None
    }
}
impl<B: ExporterBackend + Send, C: Clock + Send> Managed for Host<B, C> {
    fn execute(&self, caller: Caller, command: Command, deadline: u64) -> Result<Reply> {
        self.execute(caller, command, deadline)
    }
    fn shutdown(&self) {
        self.shutdown();
    }
    fn audit(&self) -> Option<Audit> {
        self.audit()
    }
}
struct Inner {
    #[cfg(feature = "mdns")]
    announcement: Mutex<Option<crate::discovery::Announcement>>,
    host: Arc<dyn Managed>,
    worker: Mutex<Option<JoinHandle<Result<()>>>>,
    stop: knightbus_ipc::local::Cancellation,
}
impl Drop for Inner {
    fn drop(&mut self) {
        self.stop.cancel();
        self.host.shutdown();
        if let Ok(worker) = self.worker.get_mut()
            && let Some(worker) = worker.take()
        {
            let _ = worker.join();
        }
    }
}
/// One background native host. The last owner drains it before destruction.
/// A control client disconnect never destroys this owner in Service mode.
#[derive(Clone)]
pub struct Runtime(Arc<Inner>);
/// Owned direct control target, suitable for a scoped ControlConnection.
#[derive(Clone)]
pub struct Endpoint(Runtime);
impl ControlTarget for Endpoint {
    fn now(&self) -> u64 {
        match self.0.0.host.execute(ADMIN, Command::Info, u64::MAX) {
            Ok(Reply::Info { now, .. }) => now,
            _ => u64::MAX,
        }
    }
    fn execute(&mut self, caller: Caller, command: Command, deadline: u64) -> Result<Reply> {
        let shutdown = matches!(command, Command::Shutdown);
        let result = self.0.0.host.execute(caller, command, deadline);
        if shutdown && result.is_ok() {
            self.0.0.stop.cancel();
        }
        result
    }
}
impl Runtime {
    /// Start the configured native role. No service endpoint or elevated process is created.
    pub fn start(config: &Configuration) -> Result<Self> {
        #[cfg(not(feature = "mdns"))]
        if config.mdns_name.is_some() {
            return Err(Error::Unsupported);
        }
        let runtime = match &config.role {
            Role::Exporter { .. } => exporter(config),
            Role::Importer { .. } => crate::importer::start(config),
        }?;
        #[cfg(feature = "mdns")]
        if let Some(name) = &config.mdns_name {
            *runtime.0.announcement.lock().map_err(|_| Error::Internal)? = Some(
                crate::discovery::Announcement::new(name, config.usb, config.control)?,
            );
        }
        Ok(runtime)
    }
    pub(crate) fn spawn(
        host: Arc<dyn Managed>,
        run: impl FnOnce() -> Result<()> + Send + 'static,
    ) -> Result<Self> {
        let stop = knightbus_ipc::local::Cancellation::default();
        let signal = stop.clone();
        let worker = std::thread::Builder::new()
            .name("knightbus-host".into())
            .spawn(move || {
                // A panic marks the host stopped; no poisoned engine is reused.
                let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(run))
                    .unwrap_or(Err(Error::Internal));
                signal.cancel();
                result
            })
            .map_err(|_| Error::Unavailable)?;
        Ok(Self(Arc::new(Inner {
            #[cfg(feature = "mdns")]
            announcement: Mutex::new(None),
            host,
            worker: Mutex::new(Some(worker)),
            stop,
        })))
    }
    /// Host an existing engine with authenticated transport. Native and test backends
    /// use this same composition; no alternate forwarding engine is installed.
    pub fn exporter<B: ExporterBackend + Send + 'static>(
        engine: Engine<B, Mono>,
        identity: Identity,
        peers: Peers,
        usb: TcpListener,
        control: TcpListener,
        store: Option<Store>,
    ) -> Result<Self> {
        let host = Arc::new(Host::new(engine, identity, peers));
        let runner = host.clone();
        Self::spawn(host, move || runner.run(usb, control, None, store.as_ref()))
    }
    /// Connect to this host with a connection-scoped control adapter.
    pub fn endpoint(&self) -> Endpoint {
        Endpoint(self.clone())
    }
    /// Stop accepting work and drain native resources.
    pub fn shutdown(&self) {
        #[cfg(feature = "mdns")]
        if let Ok(mut announcement) = self.0.announcement.lock() {
            announcement.take();
        }
        self.0.host.shutdown();
        self.0.stop.cancel();
    }
    /// Host cancellation or background termination, including unexpected errors.
    pub fn stopped(&self) -> bool {
        self.0.stop.is_cancelled()
    }
    /// Obtain the bounded metadata audit, with explicit overflow records.
    pub fn audit(&self) -> Option<Audit> {
        self.0.host.audit()
    }
    /// Wait for native cleanup and propagate the background result exactly once.
    pub fn join(&self) -> Result<()> {
        self.0
            .worker
            .lock()
            .map_err(|_| Error::Internal)?
            .take()
            .map_or(Ok(()), |w| w.join().unwrap_or(Err(Error::Internal)))
    }
    /// Serve a bounded set of owner-authenticated local connections until shutdown.
    /// Bind is performed by the caller before starting native work, so setup failures
    /// cannot silently leave a forwarding host running.
    pub fn serve(&self, listener: &knightbus_ipc::local::Listener) -> Result<()> {
        let result = std::thread::scope(|scope| -> Result<()> {
            let mut workers = Vec::new();
            let result = (|| {
                while !self.stopped() {
                    #[cfg(feature = "mdns")]
                    if let Ok(announcement) = self.0.announcement.lock()
                        && let Some(announcement) = &*announcement
                        && let Err(error) = announcement.check()
                    {
                        eprintln!("event=discovery_failed error={error:?}");
                    }
                    workers.retain(|w: &std::thread::ScopedJoinHandle<'_, ()>| !w.is_finished());
                    while let Some(audit) = self.audit() {
                        eprintln!("event=audit kind={:?} error={:?}", audit.kind, audit.error);
                    }
                    if workers.len() >= MAX_CONTROL_CONNECTIONS {
                        std::thread::sleep(CONNECTION_CAPACITY_POLL);
                        continue;
                    }
                    match listener.accept() {
                        Ok(Some(connection)) => {
                            workers.push(scope.spawn(|| {
                                let mut endpoint = self.endpoint();
                                let result =
                                    connection.serve(&self.0.stop, |caller, command, deadline| {
                                        endpoint.execute(caller, command, deadline)
                                    });
                                if let Err(error) = result {
                                    eprintln!("event=control_disconnect error={error:?}");
                                }
                            }));
                        }
                        Ok(None) | Err(Error::Denied) => {}
                        Err(error) => return Err(error),
                    }
                }
                Ok(())
            })();
            self.shutdown();
            result
        });
        let cleanup = self.join();
        result.and(cleanup)
    }
    /// Reject local sockets inside the credential store (including symlink aliases).
    pub fn validate_endpoint(config: &Configuration, path: &Path) -> Result<()> {
        #[cfg(unix)]
        {
            let state = config.state.canonicalize().map_err(|_| Error::NotFound)?;
            let parent = path
                .parent()
                .ok_or(Error::Invalid)?
                .canonicalize()
                .map_err(|_| Error::NotFound)?;
            if parent.starts_with(state) {
                return Err(Error::Denied);
            }
        }
        #[cfg(not(unix))]
        let _ = (config, path);
        Ok(())
    }
}
#[cfg(target_os = "macos")]
fn exporter(config: &Configuration) -> Result<Runtime> {
    use knightbus_macos_exporter::native::{self, Native};
    let Role::Exporter {
        registry,
        capture,
        storage_unmounted,
        grants,
    } = &config.role
    else {
        return Err(Error::Invalid);
    };
    if !capture {
        return Err(Error::Denied);
    }
    let store = Store::open(&config.state)?;
    let identity = store.identity()?;
    let peers = store.peers()?;
    if grants.iter().any(|peer| !peers.contains(*peer)) {
        return Err(Error::Denied);
    }
    let id = native::enumerate()?
        .into_iter()
        .find(|d| d.registry_id == *registry)
        .ok_or(Error::NotFound)?;
    let snapshot = native::inspect(id, id.device_key())?;
    if !storage_unmounted
        && snapshot
            .device
            .configurations
            .iter()
            .flat_map(|c| &c.interfaces)
            .any(|i| i.class.class == topology::CLASS_MASS_STORAGE)
    {
        return Err(Error::Denied);
    }
    let device = snapshot.device.clone();
    let key = device.key;
    // Bind both listeners before creating the backend worker. No device is claimed
    // until the engine authorizes a real USB/IP import.
    let usb = TcpListener::bind(config.usb).map_err(|_| Error::Unavailable)?;
    let control = TcpListener::bind(config.control).map_err(|_| Error::Unavailable)?;
    let mut engine = Engine::new(
        Mono::default(),
        Config {
            resources: EXPORTER_RESOURCES,
            devices: 1,
            lease_ms: EXPORTER_LEASE_MS,
            ..Config::default()
        },
        knightbus_transport::security::epoch()?,
        |budget| {
            let b = budget.clone();
            knightbus_core::exporter::Backend::new(device.clone(), budget, move || {
                Native::open(snapshot, true, b)
            })
        },
    )?;
    engine.register(device)?;
    for peer in grants {
        engine.execute(
            ADMIN,
            Command::Authorize {
                device: key,
                peer: *peer,
                allow: true,
            },
            u64::MAX,
        )?;
    }
    Runtime::exporter(engine, identity, peers, usb, control, Some(store))
}
#[cfg(not(target_os = "macos"))]
fn exporter(_: &Configuration) -> Result<Runtime> {
    Err(Error::Unsupported)
}
