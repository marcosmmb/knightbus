//! One explicit, bounded pairing job per service. Never owns USB resources.
use knightbus_core::{
    Error, Result,
    host::{NearbyComputer, PairingOffer, PairingPin, PairingProgress},
};
use knightbus_transport::{
    operation::Operation,
    pairing,
    security::{self, Store},
};
use std::{
    net::{SocketAddr, TcpListener},
    path::PathBuf,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    thread::JoinHandle,
    time::{Duration, Instant},
};

const OFFER_LIFETIME: Duration = Duration::from_secs(120);
const ATTEMPT_TIMEOUT: Duration = Duration::from_secs(5);
const JOIN_TIMEOUT: Duration = Duration::from_secs(10);
const ACCEPT_POLL: Duration = Duration::from_millis(20);
const MAX_ATTEMPTS: usize = 5;

pub(crate) struct Job {
    cancelled: Arc<AtomicBool>,
    progress: Arc<Mutex<PairingProgress>>,
    worker: Option<JoinHandle<()>>,
    #[cfg(feature = "mdns")]
    address: Option<SocketAddr>,
    expires: Instant,
}
impl Drop for Job {
    fn drop(&mut self) {
        self.cancelled.store(true, Ordering::Release);
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}
impl Job {
    pub fn progress(&self) -> Result<PairingProgress> {
        let mut value = self.progress.lock().map_err(|_| Error::Internal)?.clone();
        if let PairingProgress::Waiting { expires_in, .. } = &mut value {
            *expires_in = self
                .expires
                .saturating_duration_since(Instant::now())
                .as_secs();
            if *expires_in == 0 {
                return Ok(failed(Error::Deadline));
            }
        }
        Ok(value)
    }
    #[cfg(feature = "mdns")]
    pub fn port(&self) -> Option<u16> {
        if matches!(self.progress(), Ok(PairingProgress::Waiting { .. })) {
            self.address.map(|a| a.port())
        } else {
            None
        }
    }
    pub fn start(state: PathBuf, local: NearbyComputer) -> Result<(Self, PairingOffer)> {
        pairing::validate_contact(&local)?;
        let ip = local
            .control
            .parse::<SocketAddr>()
            .map_err(|_| Error::Invalid)?
            .ip();
        let listener = TcpListener::bind((ip, 0)).map_err(|_| Error::Unavailable)?;
        listener
            .set_nonblocking(true)
            .map_err(|_| Error::Unavailable)?;
        let address = listener.local_addr().map_err(|_| Error::Unavailable)?;
        let pin = pairing::generate_pin()?;
        let offer = PairingOffer {
            pin: PairingPin(pin.to_string()),
            address: address.to_string(),
            expires_in: OFFER_LIFETIME.as_secs(),
        };
        let cancelled = Arc::new(AtomicBool::new(false));
        let progress = Arc::new(Mutex::new(PairingProgress::Waiting {
            expires_in: OFFER_LIFETIME.as_secs(),
            attempts_left: MAX_ATTEMPTS,
        }));
        let expires = Instant::now() + OFFER_LIFETIME;
        let signal = cancelled.clone();
        let result = progress.clone();
        let worker = std::thread::spawn(move || {
            let stopped = || signal.load(Ordering::Acquire);
            let window = Operation::new(expires, &stopped);
            let run = || -> Result<NearbyComputer> {
                for attempt in 0..MAX_ATTEMPTS {
                    let stream = loop {
                        window.check()?;
                        match listener.accept() {
                            Ok((stream, _)) => break stream,
                            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                                std::thread::sleep(ACCEPT_POLL)
                            }
                            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
                            Err(_) => return Err(Error::Unavailable),
                        }
                    };
                    if let Ok(mut value) = result.lock() {
                        *value = PairingProgress::Waiting {
                            expires_in: expires.saturating_duration_since(Instant::now()).as_secs(),
                            attempts_left: MAX_ATTEMPTS - attempt - 1,
                        };
                    }
                    let op =
                        Operation::new(expires.min(Instant::now() + ATTEMPT_TIMEOUT), &stopped);
                    let store = Store::open(&state)?;
                    let mut authenticated = false;
                    match pairing::accept(stream, &pin, &store, &local, &op, &mut authenticated) {
                        Ok(computer) => return Ok(computer),
                        Err(error) if authenticated => return Err(error),
                        Err(error @ (Error::Exhausted | Error::Internal)) => return Err(error),
                        Err(Error::Cancelled) => return Err(Error::Cancelled),
                        // Every accepted connection consumes an attempt, including malformed traffic.
                        Err(_) => continue,
                    }
                }
                Err(Error::Denied)
            };
            let outcome = run();
            drop(listener); // Close admission before exposing a final state to the UI.
            if let Ok(mut value) = result.lock() {
                *value = completed(outcome);
            }
        });
        Ok((
            Self {
                cancelled,
                progress,
                worker: Some(worker),
                #[cfg(feature = "mdns")]
                address: Some(address),
                expires,
            },
            offer,
        ))
    }
    pub fn join(
        state: PathBuf,
        local: NearbyComputer,
        address: SocketAddr,
        pin: PairingPin,
    ) -> Result<Self> {
        pairing::validate_contact(&local)?;
        pairing::validate_pin(&pin.0)?;
        if address.port() == 0 || address.ip().is_unspecified() || address.ip().is_multicast() {
            return Err(Error::Invalid);
        }
        let cancelled = Arc::new(AtomicBool::new(false));
        let progress = Arc::new(Mutex::new(PairingProgress::Connecting));
        let signal = cancelled.clone();
        let result = progress.clone();
        let expires = Instant::now() + JOIN_TIMEOUT;
        let worker = std::thread::spawn(move || {
            let stopped = || signal.load(Ordering::Acquire);
            let op = Operation::new(expires, &stopped);
            let secret = zeroize::Zeroizing::new(pin.0);
            let outcome = Store::open(&state)
                .and_then(|store| pairing::join(address, &secret, &store, &local, &op));
            if let Ok(mut value) = result.lock() {
                *value = completed(outcome);
            }
        });
        Ok(Self {
            cancelled,
            progress,
            worker: Some(worker),
            #[cfg(feature = "mdns")]
            address: None,
            expires,
        })
    }
}
fn failed(error: Error) -> PairingProgress {
    PairingProgress::Failed {
        error: format!("{error:?}"),
    }
}
fn completed(result: Result<NearbyComputer>) -> PairingProgress {
    match result {
        Ok(computer) => PairingProgress::Paired { computer },
        Err(error) => failed(error),
    }
}
pub(crate) fn contact(
    store: &Store,
    name: String,
    usb: String,
    control: String,
) -> Result<NearbyComputer> {
    let computer = NearbyComputer {
        name,
        usb,
        control,
        fingerprint: security::display_fingerprint(store.identity()?.peer()),
        pairing_port: None,
    };
    pairing::validate_contact(&computer)?;
    Ok(computer)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::TcpStream;
    struct Fixture(PathBuf);
    impl Fixture {
        fn new() -> Self {
            let path = std::env::temp_dir().join(format!(
                "kb-pin-job-{}-{}",
                std::process::id(),
                security::epoch().unwrap()
            ));
            Store::initialize(&path).unwrap();
            Self(path)
        }
        fn contact(&self, name: &str) -> NearbyComputer {
            contact(
                &Store::open(&self.0).unwrap(),
                name.into(),
                "127.0.0.1:3337".into(),
                "127.0.0.1:3338".into(),
            )
            .unwrap()
        }
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }
    fn wait(job: &Job) -> PairingProgress {
        let deadline = Instant::now() + Duration::from_secs(3);
        loop {
            let progress = job.progress().unwrap();
            if !matches!(
                progress,
                PairingProgress::Waiting { .. } | PairingProgress::Connecting
            ) {
                return progress;
            }
            assert!(Instant::now() < deadline);
            std::thread::sleep(ACCEPT_POLL);
        }
    }
    #[test]
    fn successful_job_closes_the_single_use_listener_and_pairs_both_sides() {
        let server = Fixture::new();
        let client = Fixture::new();
        let (host, offer) = Job::start(server.0.clone(), server.contact("Server")).unwrap();
        let join = Job::join(
            client.0.clone(),
            client.contact("Client"),
            offer.address.parse().unwrap(),
            offer.pin,
        )
        .unwrap();
        assert!(matches!(wait(&join), PairingProgress::Paired { .. }));
        assert!(matches!(wait(&host), PairingProgress::Paired { .. }));
        assert!(TcpStream::connect(&offer.address).is_err());
        assert_eq!(
            Store::open(&server.0)
                .unwrap()
                .peers()
                .unwrap()
                .identities()
                .count(),
            1
        );
        assert_eq!(
            Store::open(&client.0)
                .unwrap()
                .peers()
                .unwrap()
                .identities()
                .count(),
            1
        );
    }
    #[test]
    fn malformed_attempts_exhaust_one_global_limit_and_close_the_listener() {
        let server = Fixture::new();
        let (job, offer) = Job::start(server.0.clone(), server.contact("Server")).unwrap();
        for _ in 0..MAX_ATTEMPTS {
            drop(TcpStream::connect(&offer.address).unwrap());
        }
        assert_eq!(
            wait(&job),
            PairingProgress::Failed {
                error: "Denied".into()
            }
        );
        assert!(TcpStream::connect(&offer.address).is_err());
        assert_eq!(
            Store::open(&server.0)
                .unwrap()
                .peers()
                .unwrap()
                .identities()
                .count(),
            0
        );
    }
    #[test]
    fn cancel_closes_a_window_promptly_even_with_a_slow_connection() {
        let server = Fixture::new();
        let (job, offer) = Job::start(server.0.clone(), server.contact("Server")).unwrap();
        let _slow = TcpStream::connect(&offer.address).unwrap();
        std::thread::sleep(ACCEPT_POLL);
        let started = Instant::now();
        drop(job);
        assert!(started.elapsed() < Duration::from_secs(1));
        assert!(TcpStream::connect(&offer.address).is_err());
        assert_eq!(
            Store::open(&server.0)
                .unwrap()
                .peers()
                .unwrap()
                .identities()
                .count(),
            0
        );
    }
}
