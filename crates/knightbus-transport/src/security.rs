//! Explicit certificate pairing and mutually authenticated TLS 1.3.
use knightbus_core::{Caller, Error, PeerId, Result};
use rustls::{
    ClientConfig, RootCertStore, ServerConfig,
    pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer, ServerName},
};
use std::{collections::BTreeMap, sync::Arc};

/// Maximum certificate or private-key encoding accepted from storage.
pub const MAX_CREDENTIAL_BYTES: usize = 16 * 1024;
/// Maximum explicitly paired peers per host.
pub const MAX_PEERS: usize = 64;
const FINGERPRINT_BYTES: usize = ring::digest::SHA256_OUTPUT_LEN;
const HEX_DIGITS_PER_BYTE: usize = 2;
const FINGERPRINT_HEX_LEN: usize = FINGERPRINT_BYTES * HEX_DIGITS_PER_BYTE;
const CERTIFICATE_LENGTH_BYTES: usize = size_of::<u32>();
const IDENTITY_RECORDS: usize = 2; // Private identity and public certificate.
const MAX_IDENTITY_BYTES: usize = CERTIFICATE_LENGTH_BYTES + MAX_CREDENTIAL_BYTES * 2;
const SERVER_NAME: &str = "knightbus.local";

/// Separate TLS application protocols; USB/IP itself is unchanged.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Channel {
    /// Standard USB/IP data stream.
    Usb,
    /// Versioned management, events and heartbeats.
    Control,
}
impl Channel {
    pub(crate) fn alpn(self) -> &'static [u8] {
        match self {
            Self::Usb => b"knightbus-usbip/1",
            Self::Control => b"knightbus-control/1",
        }
    }
}

/// Local certificate and private key. Debug output never includes key material.
pub struct Identity {
    certificate: CertificateDer<'static>,
    key: PrivateKeyDer<'static>,
}
impl Identity {
    /// Generate a local identity; exchanging its public certificate is a separate action.
    pub fn generate() -> Result<Self> {
        let generated = rcgen::generate_simple_self_signed(vec![SERVER_NAME.into()])
            .map_err(|_| Error::Internal)?;
        Self::from_der(
            generated.cert.der().to_vec(),
            generated.signing_key.serialize_der(),
        )
    }
    /// Load DER credentials from protected host storage.
    pub fn from_der(certificate: Vec<u8>, key: Vec<u8>) -> Result<Self> {
        if certificate.is_empty()
            || certificate.len() > MAX_CREDENTIAL_BYTES
            || key.is_empty()
            || key.len() > MAX_CREDENTIAL_BYTES
        {
            return Err(Error::Invalid);
        }
        let identity = Self {
            certificate: CertificateDer::from(certificate),
            key: PrivatePkcs8KeyDer::from(key).into(),
        };
        // Validate the certificate/key pair before accepting persisted state.
        identity.client_config(&identity.certificate, Channel::Control)?;
        Ok(identity)
    }
    /// Public certificate for explicit out-of-band enrollment.
    pub fn certificate(&self) -> &[u8] {
        self.certificate.as_ref()
    }
    /// Secret PKCS#8 encoding. Store only with OS-protected permissions.
    fn private_key(&self) -> &[u8] {
        self.key.secret_der()
    }
    /// SHA-256 certificate identity. Not an access grant.
    pub fn peer(&self) -> PeerId {
        fingerprint(self.certificate())
    }
    fn client_config(
        &self,
        remote: &CertificateDer<'static>,
        channel: Channel,
    ) -> Result<ClientConfig> {
        let mut roots = RootCertStore::empty();
        roots.add(remote.clone()).map_err(|_| Error::Invalid)?;
        let mut config = ClientConfig::builder_with_provider(provider())
            .with_protocol_versions(&[&rustls::version::TLS13])
            .map_err(|_| Error::Internal)?
            .with_root_certificates(roots)
            .with_client_auth_cert(vec![self.certificate.clone()], self.key.clone_key())
            .map_err(|_| Error::Invalid)?;
        config.alpn_protocols = vec![channel.alpn().to_vec()];
        config.enable_early_data = false;
        config.resumption = rustls::client::Resumption::disabled();
        Ok(config)
    }
}
fn provider() -> Arc<rustls::crypto::CryptoProvider> {
    Arc::new(rustls::crypto::ring::default_provider())
}
/// Hash a public certificate for confirmation over a trusted separate channel.
pub fn fingerprint(certificate: &[u8]) -> PeerId {
    let digest = ring::digest::digest(&ring::digest::SHA256, certificate);
    PeerId(digest.as_ref().try_into().expect("SHA-256 output size"))
}
/// Fresh nonzero host epoch; stale session IDs cannot cross a restart.
pub fn epoch() -> Result<u64> {
    use ring::rand::SecureRandom;
    loop {
        let mut bytes = [0; size_of::<u64>()];
        ring::rand::SystemRandom::new()
            .fill(&mut bytes)
            .map_err(|_| Error::Internal)?;
        let value = u64::from_be_bytes(bytes);
        if value != 0 {
            return Ok(value);
        }
    }
}

/// Bounded, default-deny trust store. Enrollment never changes device permissions.
#[derive(Clone, Default)]
pub struct Peers(BTreeMap<PeerId, CertificateDer<'static>>);
impl Peers {
    /// Enroll a certificate only after confirming its fingerprint out of band.
    /// Repeating the same enrollment is harmless; a different key is a new peer.
    pub fn pair(&mut self, certificate: Vec<u8>, confirmed: PeerId) -> Result<()> {
        if certificate.is_empty()
            || certificate.len() > MAX_CREDENTIAL_BYTES
            || fingerprint(&certificate) != confirmed
        {
            return Err(Error::Denied);
        }
        let certificate = CertificateDer::from(certificate);
        let mut roots = RootCertStore::empty();
        roots.add(certificate.clone()).map_err(|_| Error::Invalid)?;
        if !self.0.contains_key(&confirmed) && self.0.len() >= MAX_PEERS {
            return Err(Error::Exhausted);
        }
        self.0.insert(confirmed, certificate);
        Ok(())
    }
    /// Revoke future authentication. Active connections must also be closed by the host.
    pub fn revoke(&mut self, peer: PeerId) {
        self.0.remove(&peer);
    }
    /// Enumerate enrolled identities without exposing certificate bytes.
    pub fn identities(&self) -> impl Iterator<Item = PeerId> + '_ {
        self.0.keys().copied()
    }
    /// Whether this identity remains explicitly enrolled.
    pub fn contains(&self, peer: PeerId) -> bool {
        self.0.contains_key(&peer)
    }
    pub(crate) fn client(
        &self,
        identity: &Identity,
        peer: PeerId,
        channel: Channel,
    ) -> Result<rustls::Connection> {
        let certificate = self.0.get(&peer).ok_or(Error::Denied)?;
        Ok(rustls::ClientConnection::new(
            Arc::new(identity.client_config(certificate, channel)?),
            ServerName::try_from(SERVER_NAME).map_err(|_| Error::Internal)?,
        )
        .map_err(|_| Error::Invalid)?
        .into())
    }
    pub(crate) fn server(
        &self,
        identity: &Identity,
        channel: Channel,
    ) -> Result<rustls::Connection> {
        let mut roots = RootCertStore::empty();
        for certificate in self.0.values() {
            roots.add(certificate.clone()).map_err(|_| Error::Invalid)?;
        }
        let verifier = rustls::server::WebPkiClientVerifier::builder_with_provider(
            Arc::new(roots),
            provider(),
        )
        .build()
        .map_err(|_| Error::Denied)?;
        let mut config = ServerConfig::builder_with_provider(provider())
            .with_protocol_versions(&[&rustls::version::TLS13])
            .map_err(|_| Error::Internal)?
            .with_client_cert_verifier(verifier)
            .with_single_cert(vec![identity.certificate.clone()], identity.key.clone_key())
            .map_err(|_| Error::Invalid)?;
        config.alpn_protocols = vec![channel.alpn().to_vec()];
        config.max_early_data_size = 0;
        config.send_tls13_tickets = 0;
        config.session_storage = Arc::new(rustls::server::NoServerSessionStorage {});
        Ok(rustls::ServerConnection::new(Arc::new(config))
            .map_err(|_| Error::Invalid)?
            .into())
    }
    pub(crate) fn verify(
        &self,
        connection: &rustls::Connection,
        channel: Channel,
    ) -> Result<Caller> {
        if connection.is_handshaking() || connection.alpn_protocol() != Some(channel.alpn()) {
            return Err(Error::Denied);
        }
        let certificate = connection
            .peer_certificates()
            .and_then(|chain| chain.first())
            .ok_or(Error::Denied)?;
        let peer = fingerprint(certificate.as_ref());
        // Check the exact leaf too: a paired certificate cannot delegate enrollment.
        if !self.contains(peer) {
            return Err(Error::Denied);
        }
        Ok(Caller {
            peer,
            administrator: false,
        })
    }
}

/// Protected persistent identity and explicitly enrolled public certificates.
/// Incomplete writes fail closed; opening state never silently generates a new identity.
pub struct Store {
    directory: knightbus_ipc::private::Directory,
}
impl Store {
    const IDENTITY: &'static str = "identity.bin";
    /// Initialize once. Refuses to replace an existing private key.
    pub fn initialize(path: &std::path::Path) -> Result<PeerId> {
        let directory = knightbus_ipc::private::Directory::open(path)?;
        let identity = Identity::generate()?;
        let mut bytes = zeroize::Zeroizing::new(Vec::new());
        bytes.extend_from_slice(&(identity.certificate().len() as u32).to_be_bytes());
        bytes.extend_from_slice(identity.certificate());
        bytes.extend_from_slice(identity.private_key());
        directory.create(Self::IDENTITY, &bytes)?;
        directory.create("certificate.der", identity.certificate())?;
        Ok(identity.peer())
    }
    /// Open existing protected state and verify its key before use.
    pub fn open(path: &std::path::Path) -> Result<Self> {
        let store = Self {
            directory: knightbus_ipc::private::Directory::open(path)?,
        };
        store.identity()?;
        Ok(store)
    }
    /// Load the local identity. Never include returned key bytes in logs.
    pub fn identity(&self) -> Result<Identity> {
        let bytes =
            zeroize::Zeroizing::new(self.directory.read(Self::IDENTITY, MAX_IDENTITY_BYTES)?);
        let length = u32::from_be_bytes(
            bytes
                .get(..CERTIFICATE_LENGTH_BYTES)
                .ok_or(Error::Invalid)?
                .try_into()
                .unwrap(),
        ) as usize;
        if length > MAX_CREDENTIAL_BYTES || bytes.len() <= CERTIFICATE_LENGTH_BYTES + length {
            return Err(Error::Invalid);
        }
        Identity::from_der(
            bytes[CERTIFICATE_LENGTH_BYTES..CERTIFICATE_LENGTH_BYTES + length].to_vec(),
            bytes[CERTIFICATE_LENGTH_BYTES + length..].to_vec(),
        )
    }
    /// Load only bounded, fingerprint-checked enrollment records.
    pub fn peers(&self) -> Result<Peers> {
        self.read_peers(self.directory.names(MAX_PEERS + IDENTITY_RECORDS)?)
    }
    fn read_peers(&self, names: Vec<String>) -> Result<Peers> {
        let mut peers = Peers::default();
        for name in names {
            if let Some(hex) = name.strip_prefix("peer-") {
                let peer = parse_fingerprint(hex)?;
                let certificate = match self.directory.read(&name, MAX_CREDENTIAL_BYTES) {
                    Ok(bytes) => bytes,
                    // Concurrent unpairing removes only this grant's trust record.
                    Err(Error::NotFound) => continue,
                    Err(error) => return Err(error),
                };
                peers.pair(certificate, peer)?;
            } else if name != Self::IDENTITY && name != "certificate.der" {
                return Err(Error::Invalid);
            }
        }
        Ok(peers)
    }
    /// Persist explicit enrollment; repeated confirmation of the same identity is idempotent.
    pub fn pair(&self, certificate: Vec<u8>, confirmed: PeerId) -> Result<()> {
        let mut peers = self.peers()?;
        let exists = peers.contains(confirmed);
        peers.pair(certificate.clone(), confirmed)?;
        if !exists {
            self.directory.create(
                &format!("peer-{}", display_fingerprint(confirmed)),
                &certificate,
            )?;
        }
        Ok(())
    }
    /// Remove persistent trust. A running Host must also revoke its live peer.
    pub fn revoke(&self, peer: PeerId) -> Result<()> {
        if self.peers()?.contains(peer) {
            self.directory
                .remove(&format!("peer-{}", display_fingerprint(peer)))?;
        }
        Ok(())
    }
}
/// Canonical public fingerprint, suitable for confirmation and peer selection.
pub fn display_fingerprint(peer: PeerId) -> String {
    use std::fmt::Write;
    let mut result = String::with_capacity(FINGERPRINT_HEX_LEN);
    for byte in peer.0 {
        write!(&mut result, "{byte:02x}").expect("String write");
    }
    result
}
/// Parse exactly one SHA-256 fingerprint; partial matching is never accepted.
pub fn parse_fingerprint(text: &str) -> Result<PeerId> {
    if text.len() != FINGERPRINT_HEX_LEN || !text.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Err(Error::Invalid);
    }
    let mut bytes = [0; FINGERPRINT_BYTES];
    for (i, byte) in bytes.iter_mut().enumerate() {
        *byte = u8::from_str_radix(
            &text[HEX_DIGITS_PER_BYTE * i..HEX_DIGITS_PER_BYTE * (i + 1)],
            16,
        )
        .map_err(|_| Error::Invalid)?;
    }
    Ok(PeerId(bytes))
}
/// Shared identity commands for thin OS runners. Returns None for other commands.
pub fn command(args: &[String]) -> Option<std::result::Result<(), Box<dyn std::error::Error>>> {
    if !args
        .first()
        .is_some_and(|s| matches!(s.as_str(), "identity" | "pair" | "unpair" | "fingerprint"))
    {
        return None;
    }
    Some(identity_command(args))
}

fn identity_command(args: &[String]) -> std::result::Result<(), Box<dyn std::error::Error>> {
    use std::{io::Read, path::Path};
    match args {
        [cmd, path] if cmd == "identity" => {
            let peer = Store::initialize(Path::new(path))?;
            println!("{}", display_fingerprint(peer));
        }
        [cmd, path] if cmd == "fingerprint" => {
            let identity = Store::open(Path::new(path))?.identity()?;
            println!("{}", display_fingerprint(identity.peer()));
        }
        [cmd, path, certificate, confirmed] if cmd == "pair" => {
            let mut bytes = Vec::new();
            std::fs::File::open(certificate)?
                .take((MAX_CREDENTIAL_BYTES + 1) as u64)
                .read_to_end(&mut bytes)?;
            Store::open(Path::new(path))?.pair(bytes, parse_fingerprint(confirmed)?)?;
        }
        [cmd, path, peer] if cmd == "unpair" => {
            Store::open(Path::new(path))?.revoke(parse_fingerprint(peer)?)?;
        }
        _ => return Err("usage: identity STATE_DIR | fingerprint STATE_DIR | pair STATE_DIR CERTIFICATE.der CONFIRMED_FINGERPRINT | unpair STATE_DIR FINGERPRINT".into()),
    }
    Ok(())
}

#[cfg(test)]
mod storage_tests {
    use super::*;
    #[test]
    fn unpair_during_enumeration_skips_only_the_removed_record() {
        let root = std::env::temp_dir().join(format!(
            "kb-unpair-{}-{}",
            std::process::id(),
            epoch().unwrap()
        ));
        Store::initialize(&root).unwrap();
        let store = Store::open(&root).unwrap();
        let first = Identity::generate().unwrap();
        let second = Identity::generate().unwrap();
        for peer in [&first, &second] {
            store
                .pair(peer.certificate().to_vec(), peer.peer())
                .unwrap();
        }
        let names = store.directory.names(MAX_PEERS + IDENTITY_RECORDS).unwrap();
        store.revoke(first.peer()).unwrap();
        let peers = store.read_peers(names).unwrap();
        assert!(!peers.contains(first.peer()));
        assert!(peers.contains(second.peer()));
        // Corruption still fails closed, rather than masquerading as unpairing.
        std::fs::write(
            root.join(format!("peer-{}", display_fingerprint(second.peer()))),
            b"corrupt",
        )
        .unwrap();
        assert!(store.peers().is_err());
        drop(store);
        std::fs::remove_dir_all(root).unwrap();
    }
}

#[cfg(test)]
mod tests {
    //! Real TLS authentication and unchanged USB/IP framing.
    use crate::{
        Connection, Stream,
        security::{Channel, Identity, Peers, fingerprint},
    };
    use knightbus_core::{
        Budget, Error, ResourceLimits,
        usbip::{self, Context, Message},
    };
    use std::{
        net::{TcpListener, TcpStream},
        time::{Duration, Instant},
    };
    const TIMEOUT: Duration = Duration::from_secs(3);
    fn paired() -> (Identity, Identity, Peers) {
        let a = Identity::generate().unwrap();
        let b = Identity::generate().unwrap();
        let mut peers = Peers::default();
        for id in [&a, &b] {
            peers.pair(id.certificate().to_vec(), id.peer()).unwrap();
        }
        (a, b, peers)
    }
    #[test]
    fn tls_carries_unmodified_usbip_and_recovers_credits() {
        let (a, b, peers) = paired();
        let peer = a.peer();
        let expected = b.peer();
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let trust = peers.clone();
        let thread = std::thread::spawn(move || {
            let stream = Stream::accept(
                listener.accept().unwrap().0,
                &a,
                &trust,
                Channel::Usb,
                TIMEOUT,
            )
            .unwrap();
            assert_eq!(stream.caller().unwrap().peer, expected);
            assert!(!stream.caller().unwrap().administrator);
            let budget = Budget::new(ResourceLimits {
                bytes: 2 * 1024 * 1024,
                requests: 4,
            });
            let mut wire =
                Connection::new(stream, budget.clone(), usbip::Limits::default(), TIMEOUT).unwrap();
            let deadline = Instant::now() + TIMEOUT;
            loop {
                assert!(Instant::now() < deadline);
                if let Some(msg) = wire.receive(|_, _| Ok(Context::Management)).unwrap() {
                    assert_eq!(msg.message(), &Message::DevListRequest);
                    break;
                }
                wire.wait(Duration::from_millis(10)).unwrap();
            }
            wire.send(&Message::DevListReply {
                status: 0,
                devices: vec![],
            })
            .unwrap();
            while !wire.writable() {
                wire.flush().unwrap();
                wire.wait(Duration::from_millis(1)).unwrap();
            }
            drop(wire);
            assert_eq!(budget.usage().bytes, 0);
        });
        let mut stream = Stream::connect(
            TcpStream::connect(addr).unwrap(),
            &b,
            &peers,
            peer,
            Channel::Usb,
            TIMEOUT,
        )
        .unwrap();
        // The plaintext codec sees exactly the standard USB/IP stream.
        use std::io::{Read, Write};
        let request = usbip::encode(
            &Message::DevListRequest,
            usbip::Limits::default(),
            &mut |_| Ok(()),
        )
        .unwrap();
        stream.write_all(&request).unwrap();
        let deadline = Instant::now() + TIMEOUT;
        loop {
            match stream.flush() {
                Ok(()) => break,
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {}
                Err(e) => panic!("{e}"),
            }
            assert!(Instant::now() < deadline);
            std::thread::yield_now();
        }
        let mut response = vec![];
        let mut buf = [0; 64];
        while response.len() < 12 {
            assert!(Instant::now() < deadline);
            match stream.read(&mut buf) {
                Ok(n) if n > 0 => response.extend_from_slice(&buf[..n]),
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => std::thread::yield_now(),
                r => panic!("{r:?}"),
            }
        }
        assert_eq!(
            usbip::decode(
                &response,
                Context::Management,
                usbip::Limits::default(),
                &mut |_| Ok(())
            )
            .unwrap()
            .0,
            Message::DevListReply {
                status: 0,
                devices: vec![]
            }
        );
        thread.join().unwrap();
    }
    #[test]
    fn wrong_peer_wrong_channel_and_unpaired_client_are_denied() {
        for mismatch in 0..3 {
            let (a, b, mut peers) = paired();
            let peer = a.peer();
            let listener = TcpListener::bind("127.0.0.1:0").unwrap();
            let addr = listener.local_addr().unwrap();
            let mut trust = peers.clone();
            if mismatch == 0 {
                trust.revoke(b.peer());
            }
            if mismatch == 2 {
                let impostor = Identity::generate().unwrap();
                peers.revoke(peer);
                peers
                    .pair(impostor.certificate().to_vec(), impostor.peer())
                    .unwrap();
            }
            let thread = std::thread::spawn(move || {
                Stream::accept(
                    listener.accept().unwrap().0,
                    &a,
                    &trust,
                    Channel::Usb,
                    TIMEOUT,
                )
                .is_err()
            });
            let result = Stream::connect(
                TcpStream::connect(addr).unwrap(),
                &b,
                &peers,
                peer,
                if mismatch == 1 {
                    Channel::Control
                } else {
                    Channel::Usb
                },
                TIMEOUT,
            );
            // TLS 1.3 client may finish before receiving the server's client-cert rejection.
            if mismatch != 0 {
                assert!(result.is_err());
            }
            drop(result);
            assert!(thread.join().unwrap());
        }
    }
    #[test]
    fn pairing_requires_confirmation_and_revocation_removes_trust() {
        let a = Identity::generate().unwrap();
        let mut peers = Peers::default();
        assert_eq!(
            peers.pair(a.certificate().to_vec(), fingerprint(b"wrong")),
            Err(Error::Denied)
        );
        assert!(!peers.contains(a.peer()));
        peers.pair(a.certificate().to_vec(), a.peer()).unwrap();
        peers.pair(a.certificate().to_vec(), a.peer()).unwrap();
        peers.revoke(a.peer());
        assert!(!peers.contains(a.peer()));
    }

    #[test]
    fn persistent_pairing_survives_restart_and_never_replaces_an_identity() {
        use crate::security::{Store, display_fingerprint, parse_fingerprint};
        let path = std::env::temp_dir().join(format!(
            "kb-trust-{}-{}",
            std::process::id(),
            crate::security::epoch().unwrap()
        ));
        let peer = Store::initialize(&path).unwrap();
        assert!(Store::initialize(&path).is_err());
        let store = Store::open(&path).unwrap();
        assert_eq!(store.identity().unwrap().peer(), peer);
        let other = Identity::generate().unwrap();
        store
            .pair(other.certificate().to_vec(), other.peer())
            .unwrap();
        store
            .pair(other.certificate().to_vec(), other.peer())
            .unwrap();
        drop(store);
        let store = Store::open(&path).unwrap();
        assert!(store.peers().unwrap().contains(other.peer()));
        store.revoke(other.peer()).unwrap();
        drop(store);
        assert!(
            !Store::open(&path)
                .unwrap()
                .peers()
                .unwrap()
                .contains(other.peer())
        );
        assert_eq!(parse_fingerprint(&display_fingerprint(peer)).unwrap(), peer);
        assert!(parse_fingerprint("01").is_err());
        assert!(parse_fingerprint(&"ø".repeat(32)).is_err());
        std::fs::remove_dir_all(path).unwrap();
    }
    #[test]
    fn silent_handshake_is_bounded_without_a_plaintext_fallback() {
        let (identity, _, peers) = paired();
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let _silent = TcpStream::connect(listener.local_addr().unwrap()).unwrap();
        let start = Instant::now();
        assert!(matches!(
            Stream::accept(
                listener.accept().unwrap().0,
                &identity,
                &peers,
                Channel::Usb,
                Duration::from_millis(30)
            ),
            Err(Error::Deadline)
        ));
        assert!(start.elapsed() < Duration::from_secs(1));
    }

    #[test]
    fn tls_large_transfers_preserve_bytes_and_bound_backpressure() {
        let (a, b, peers) = paired();
        let peer = a.peer();
        let trust = peers.clone();
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let limits = usbip::Limits {
            frame_bytes: 1024 * 1024 + 48,
            transfer_bytes: 1024 * 1024,
            devices: 1,
            iso_packets: 0,
        };
        let budget = || {
            Budget::new(ResourceLimits {
                bytes: 4 * 1024 * 1024,
                requests: 4,
            })
        };
        let task = std::thread::spawn(move || {
            let stream = Stream::accept(
                listener.accept().unwrap().0,
                &a,
                &trust,
                Channel::Usb,
                TIMEOUT,
            )
            .unwrap();
            let credits = budget();
            let mut wire = Connection::new(stream, credits.clone(), limits, TIMEOUT).unwrap();
            for _ in 0..8 {
                let deadline = Instant::now() + TIMEOUT;
                let message = loop {
                    assert!(Instant::now() < deadline);
                    wire.flush().unwrap();
                    if let Some(m) = wire.receive(|_, _| Ok(Context::Requests)).unwrap() {
                        break m;
                    }
                    if !wire.take_progress() {
                        wire.wait(Duration::from_millis(10)).unwrap();
                    }
                };
                wire.send(message.message()).unwrap();
                drop(message);
                while !wire.writable() {
                    wire.flush().unwrap();
                    assert!(Instant::now() < deadline);
                    if !wire.take_progress() {
                        wire.wait(Duration::from_millis(10)).unwrap();
                    }
                }
            }
            drop(wire);
            assert_eq!(credits.usage().bytes, 0);
        });
        let stream = Stream::connect(
            TcpStream::connect(address).unwrap(),
            &b,
            &peers,
            peer,
            Channel::Usb,
            TIMEOUT,
        )
        .unwrap();
        let credits = budget();
        let mut wire = Connection::new(stream, credits.clone(), limits, TIMEOUT).unwrap();
        for sequence in 1..=8 {
            let header = usbip::SubmitHeader {
                seqnum: sequence,
                device_id: 0x10001,
                direction: usbip::Direction::Out,
                endpoint: 1,
                transfer_flags: 0,
                transfer_buffer_length: 1024 * 1024,
                start_frame: 0,
                number_of_packets: 0,
                interval: 0,
                setup: [0; 8],
            };
            let message =
                Message::Submit(header.with_payload(vec![sequence as u8; 1024 * 1024], vec![]));
            wire.send(&message).unwrap();
            let deadline = Instant::now() + TIMEOUT;
            loop {
                assert!(Instant::now() < deadline);
                wire.flush().unwrap();
                if let Some(reply) = wire.receive(|_, _| Ok(Context::Requests)).unwrap() {
                    assert_eq!(reply.message(), &message);
                    break;
                }
                if !wire.take_progress() {
                    wire.wait(Duration::from_millis(10)).unwrap();
                }
            }
        }
        task.join().unwrap();
        drop(wire);
        assert_eq!(credits.usage().bytes, 0);
    }

    #[test]
    fn tls_peer_that_stops_reading_cannot_hold_output_past_its_deadline() {
        let (server_identity, client_identity, peers) = paired();
        let server_peer = server_identity.peer();
        let trust = peers.clone();
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let (ready, authenticated) = std::sync::mpsc::channel();
        let (done, hold) = std::sync::mpsc::channel();
        let worker = std::thread::spawn(move || {
            let _stream = Stream::accept(
                listener.accept().unwrap().0,
                &server_identity,
                &trust,
                Channel::Usb,
                TIMEOUT,
            )
            .unwrap();
            ready.send(()).unwrap();
            let _ = hold.recv_timeout(Duration::from_secs(15));
        });
        let stream = Stream::connect(
            TcpStream::connect(address).unwrap(),
            &client_identity,
            &peers,
            server_peer,
            Channel::Usb,
            TIMEOUT,
        )
        .unwrap();
        authenticated.recv_timeout(TIMEOUT).unwrap();
        let credits = Budget::new(ResourceLimits {
            bytes: 2 * 1024 * 1024,
            requests: 4,
        });
        let limits = usbip::Limits::default();
        let mut wire =
            Connection::new(stream, credits.clone(), limits, Duration::from_millis(250)).unwrap();
        let header = usbip::SubmitHeader {
            seqnum: 1,
            device_id: 0x10001,
            direction: usbip::Direction::Out,
            endpoint: 1,
            transfer_flags: 0,
            transfer_buffer_length: 16 * 1024,
            start_frame: 0,
            number_of_packets: 0,
            interval: 0,
            setup: [0; 8],
        };
        let message = Message::Submit(header.with_payload(vec![42; 16 * 1024], vec![]));
        let mut expired = false;
        let deadline = Instant::now() + Duration::from_secs(10);
        // Bounded traffic fills the kernel queue; the peer keeps its socket open.
        'frames: for _ in 0..4096 {
            wire.send(&message).unwrap();
            while !wire.writable() {
                assert!(
                    Instant::now() < deadline,
                    "TLS backpressure lost its deadline"
                );
                match wire.flush() {
                    Err(Error::Deadline) => {
                        expired = true;
                        break 'frames;
                    }
                    Err(error) => panic!("unexpected TLS failure: {error:?}"),
                    Ok(()) => {}
                }
                if !wire.take_progress() {
                    wire.wait(Duration::from_millis(10)).unwrap();
                }
            }
        }
        assert!(
            expired,
            "the nonreading peer must eventually exhaust socket capacity"
        );
        assert_eq!(wire.flush(), Err(Error::State));
        drop(wire);
        assert_eq!(credits.usage().bytes, 0);
        done.send(()).unwrap();
        worker.join().unwrap();
    }

    #[test]
    fn connection_setup_observes_deadline_and_cancellation() {
        use crate::{host::Remote, operation::Operation};
        use std::sync::{
            Arc,
            atomic::{AtomicBool, Ordering},
            mpsc,
        };
        // Cover a silent TLS handshake and an authenticated peer withholding Hello.
        for authenticate in [false, true] {
            for cancel in [false, true] {
                let (server_identity, client_identity, peers) = paired();
                let peer = server_identity.peer();
                let listener = TcpListener::bind("127.0.0.1:0").unwrap();
                let address = listener.local_addr().unwrap();
                let trust = peers.clone();
                let cancelled = Arc::new(AtomicBool::new(false));
                let signal = cancelled.clone();
                let (done, release) = mpsc::channel();
                let (connected, wait_connected) = mpsc::channel();
                let server = std::thread::spawn(move || {
                    let socket = listener.accept().unwrap().0;
                    wait_connected.recv_timeout(TIMEOUT).unwrap();
                    let _stream = if authenticate {
                        Stream::accept(socket, &server_identity, &trust, Channel::Control, TIMEOUT)
                            .unwrap()
                    } else {
                        Stream::insecure_loopback(socket).unwrap()
                    };
                    if cancel {
                        signal.store(true, Ordering::SeqCst);
                    }
                    release.recv_timeout(TIMEOUT).unwrap();
                });
                let started = Instant::now();
                let check = || cancelled.load(Ordering::SeqCst);
                let operation = Operation::new(
                    started
                        + if cancel {
                            TIMEOUT
                        } else {
                            Duration::from_millis(200)
                        },
                    &check,
                );
                let socket = operation.connect(address).unwrap();
                connected.send(()).unwrap();
                let result =
                    Remote::connect_until(socket, &client_identity, &peers, peer, &operation);
                assert!(
                    matches!(result, Err(e) if e == if cancel { Error::Cancelled } else { Error::Deadline })
                );
                assert!(started.elapsed() < Duration::from_millis(500));
                done.send(()).unwrap();
                server.join().unwrap();
            }
        }
    }

    #[test]
    fn expired_or_cancelled_operations_never_open_tcp_connections() {
        use crate::operation::Operation;
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let address = listener.local_addr().unwrap();
        assert!(matches!(
            Operation::new(Instant::now(), &|| false).connect(address),
            Err(Error::Deadline)
        ));
        assert!(matches!(
            Operation::new(Instant::now() + TIMEOUT, &|| true).connect(address),
            Err(Error::Cancelled)
        ));
        assert!(matches!(listener.accept(), Err(e) if e.kind() == std::io::ErrorKind::WouldBlock));
    }
}
