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
