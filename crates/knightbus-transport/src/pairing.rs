//! Short-lived PIN authentication using SPAKE2 and directional AEAD channels.
//! Discovery supplies addresses only. Certificates are enrolled after PIN proof.
use crate::{
    operation::Operation,
    security::{self, Identity, Peers, Store},
};
use knightbus_core::{
    Error, Result,
    host::{MAX_COMPUTER_NAME_BYTES, NearbyComputer, PAIRING_PIN_DIGITS},
};
use ring::{
    aead, hkdf,
    rand::{SecureRandom, SystemRandom},
};
use spake2::{Ed25519Group, Identity as PakeIdentity, Password, Spake2};
use std::{
    io::{Read, Write},
    net::{SocketAddr, TcpStream},
};
use zeroize::Zeroizing;

const PROTOCOL: &[u8] = b"knightbus-pair/1";
const CLIENT_ID: &[u8] = b"knightbus-pair/1/client";
const SERVER_ID: &[u8] = b"knightbus-pair/1/server";
// SPAKE2 Ed25519 messages contain a role byte and a compressed group element.
const COMPRESSED_ED25519_BYTES: usize = 32;
const PAKE_MESSAGE_BYTES: usize = size_of::<u8>() + COMPRESSED_ED25519_BYTES;
const LENGTH_BYTES: usize = size_of::<u32>();
const MAX_CONTACT_BYTES: usize = 1024;
const MAX_RECORD_BYTES: usize =
    LENGTH_BYTES + security::MAX_CREDENTIAL_BYTES + MAX_CONTACT_BYTES + aead::MAX_TAG_LEN;
const NONCE_COUNTER_BYTES: usize = size_of::<u64>();
const PIN_VALUES: u32 = 10_u32.pow(PAIRING_PIN_DIGITS as u32);
const READY: &[u8] = b"ready";
const STORED: &[u8] = b"stored";

/// Generate a uniformly distributed PIN from the operating system CSPRNG.
pub fn generate_pin() -> Result<Zeroizing<String>> {
    let limit = u32::MAX - u32::MAX % PIN_VALUES;
    loop {
        let mut random = [0; size_of::<u32>()];
        SystemRandom::new()
            .fill(&mut random)
            .map_err(|_| Error::Internal)?;
        let value = u32::from_be_bytes(random);
        if value < limit {
            return Ok(Zeroizing::new(format!(
                "{:0width$}",
                value % PIN_VALUES,
                width = PAIRING_PIN_DIGITS
            )));
        }
    }
}
/// Accept only the exact PIN format; never normalize a different secret silently.
pub fn validate_pin(pin: &str) -> Result<()> {
    if pin.len() != PAIRING_PIN_DIGITS || !pin.bytes().all(|b| b.is_ascii_digit()) {
        return Err(Error::Invalid);
    }
    Ok(())
}
/// Check bounded computer metadata before publishing or trusting it.
pub fn validate_contact(contact: &NearbyComputer) -> Result<()> {
    if contact.name.trim().is_empty()
        || contact.name.len() > MAX_COMPUTER_NAME_BYTES
        || contact.name.chars().any(char::is_control)
        || contact.pairing_port.is_some()
    {
        return Err(Error::Invalid);
    }
    security::parse_fingerprint(&contact.fingerprint)?;
    let usb: SocketAddr = contact.usb.parse().map_err(|_| Error::Invalid)?;
    let control: SocketAddr = contact.control.parse().map_err(|_| Error::Invalid)?;
    if usb.ip() != control.ip()
        || usb.port() == 0
        || control.port() == 0
        || usb.ip().is_unspecified()
        || usb.ip().is_multicast()
        || matches!(usb.ip(), std::net::IpAddr::V4(ip) if ip.is_broadcast())
    {
        return Err(Error::Invalid);
    }
    Ok(())
}

fn read_exact(stream: &mut TcpStream, mut bytes: &mut [u8], op: &Operation<'_>) -> Result<()> {
    while !bytes.is_empty() {
        op.check()?;
        match stream.read(bytes) {
            Ok(0) => return Err(Error::Unavailable),
            Ok(n) => {
                bytes = &mut bytes[n..];
            }
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => std::thread::sleep(op.wait()?),
            Err(_) => return Err(Error::Unavailable),
        }
    }
    op.check()
}
fn write_all(stream: &mut TcpStream, mut bytes: &[u8], op: &Operation<'_>) -> Result<()> {
    while !bytes.is_empty() {
        op.check()?;
        match stream.write(bytes) {
            Ok(0) => return Err(Error::Unavailable),
            Ok(n) => {
                bytes = &bytes[n..];
            }
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => std::thread::sleep(op.wait()?),
            Err(_) => return Err(Error::Unavailable),
        }
    }
    op.check()
}
fn send(stream: &mut TcpStream, bytes: &[u8], op: &Operation<'_>) -> Result<()> {
    if bytes.is_empty() || bytes.len() > MAX_RECORD_BYTES {
        return Err(Error::Invalid);
    }
    write_all(stream, &(bytes.len() as u32).to_be_bytes(), op)?;
    write_all(stream, bytes, op)
}
fn receive(stream: &mut TcpStream, limit: usize, op: &Operation<'_>) -> Result<Vec<u8>> {
    let mut length = [0; LENGTH_BYTES];
    read_exact(stream, &mut length, op)?;
    let length = u32::from_be_bytes(length) as usize;
    if length == 0 || length > limit {
        return Err(Error::Invalid);
    }
    let mut bytes = vec![0; length];
    read_exact(stream, &mut bytes, op)?;
    Ok(bytes)
}

struct Channel {
    send: aead::LessSafeKey,
    receive: aead::LessSafeKey,
    sent: u64,
    received: u64,
}
impl Channel {
    fn new(secret: &[u8], client: bool) -> Result<Self> {
        let prk = hkdf::Salt::new(hkdf::HKDF_SHA256, PROTOCOL).extract(secret);
        let derive = |direction: &[u8]| -> Result<aead::LessSafeKey> {
            let info = [direction];
            let okm = prk
                .expand(&info, &aead::CHACHA20_POLY1305)
                .map_err(|_| Error::Internal)?;
            let key = aead::UnboundKey::from(okm);
            Ok(aead::LessSafeKey::new(key))
        };
        let (send, receive) = if client {
            (CLIENT_ID, SERVER_ID)
        } else {
            (SERVER_ID, CLIENT_ID)
        };
        Ok(Self {
            send: derive(send)?,
            receive: derive(receive)?,
            sent: 0,
            received: 0,
        })
    }
    fn nonce(counter: u64) -> aead::Nonce {
        let mut bytes = [0; aead::NONCE_LEN];
        bytes[aead::NONCE_LEN - NONCE_COUNTER_BYTES..].copy_from_slice(&counter.to_be_bytes());
        aead::Nonce::assume_unique_for_key(bytes)
    }
    fn seal(&mut self, bytes: &[u8]) -> Result<Vec<u8>> {
        let mut encrypted = bytes.to_vec();
        let nonce = Self::nonce(self.sent);
        self.sent = self.sent.checked_add(1).ok_or(Error::Exhausted)?;
        self.send
            .seal_in_place_append_tag(nonce, aead::Aad::from(PROTOCOL), &mut encrypted)
            .map_err(|_| Error::Internal)?;
        Ok(encrypted)
    }
    fn open(&mut self, bytes: &mut [u8]) -> Result<Vec<u8>> {
        let plain = self
            .receive
            .open_in_place(Self::nonce(self.received), aead::Aad::from(PROTOCOL), bytes)
            .map_err(|_| Error::Denied)?
            .to_vec();
        self.received = self.received.checked_add(1).ok_or(Error::Exhausted)?;
        Ok(plain)
    }
    fn send(&mut self, stream: &mut TcpStream, bytes: &[u8], op: &Operation<'_>) -> Result<()> {
        send(stream, &self.seal(bytes)?, op)
    }
    fn receive(&mut self, stream: &mut TcpStream, op: &Operation<'_>) -> Result<Vec<u8>> {
        self.open(&mut receive(stream, MAX_RECORD_BYTES, op)?)
    }
    fn expect(
        &mut self,
        stream: &mut TcpStream,
        expected: &[u8],
        op: &Operation<'_>,
    ) -> Result<()> {
        if self.receive(stream, op)? != expected {
            return Err(Error::Denied);
        }
        Ok(())
    }
}
fn encode_identity(identity: &Identity, contact: &NearbyComputer) -> Result<Vec<u8>> {
    validate_contact(contact)?;
    if contact.fingerprint != security::display_fingerprint(identity.peer()) {
        return Err(Error::Invalid);
    }
    let metadata = serde_json::to_vec(contact).map_err(|_| Error::Invalid)?;
    if metadata.len() > MAX_CONTACT_BYTES {
        return Err(Error::Invalid);
    }
    let mut bytes = Vec::new();
    bytes.extend_from_slice(&(identity.certificate().len() as u32).to_be_bytes());
    bytes.extend_from_slice(identity.certificate());
    bytes.extend_from_slice(&metadata);
    Ok(bytes)
}
struct Remote {
    certificate: Vec<u8>,
    contact: NearbyComputer,
}
fn decode_identity(bytes: &[u8], peers: &mut Peers) -> Result<Remote> {
    let prefix: [u8; LENGTH_BYTES] = bytes
        .get(..LENGTH_BYTES)
        .ok_or(Error::Invalid)?
        .try_into()
        .map_err(|_| Error::Invalid)?;
    let length = u32::from_be_bytes(prefix) as usize;
    if length == 0 || length > security::MAX_CREDENTIAL_BYTES {
        return Err(Error::Invalid);
    }
    let certificate = bytes
        .get(LENGTH_BYTES..LENGTH_BYTES + length)
        .ok_or(Error::Invalid)?
        .to_vec();
    let metadata = bytes.get(LENGTH_BYTES + length..).ok_or(Error::Invalid)?;
    if metadata.len() > MAX_CONTACT_BYTES {
        return Err(Error::Invalid);
    }
    let contact: NearbyComputer = serde_json::from_slice(metadata).map_err(|_| Error::Invalid)?;
    validate_contact(&contact)?;
    peers.pair(
        certificate.clone(),
        security::parse_fingerprint(&contact.fingerprint)?,
    )?;
    Ok(Remote {
        certificate,
        contact,
    })
}
fn enroll(store: &Store, remote: &Remote, op: &Operation<'_>) -> Result<()> {
    op.check()?;
    store.pair(
        remote.certificate.clone(),
        security::parse_fingerprint(&remote.contact.fingerprint)?,
    )
}

/// Join one PIN window. Both peers authenticate and confirm enrollment before success.
/// An interrupted commit can leave one-sided trust; retrying enrollment is idempotent.
pub fn join(
    address: SocketAddr,
    pin: &str,
    store: &Store,
    local: &NearbyComputer,
    op: &Operation<'_>,
) -> Result<NearbyComputer> {
    validate_pin(pin)?;
    let local = encode_identity(&store.identity()?, local)?;
    let mut stream = op.connect(address)?;
    stream
        .set_nonblocking(true)
        .map_err(|_| Error::Unavailable)?;
    stream.set_nodelay(true).map_err(|_| Error::Unavailable)?;
    write_all(&mut stream, PROTOCOL, op)?;
    let (pake, message) = Spake2::<Ed25519Group>::start_a(
        &Password::new(pin.as_bytes()),
        &PakeIdentity::new(CLIENT_ID),
        &PakeIdentity::new(SERVER_ID),
    );
    send(&mut stream, &message, op)?;
    let secret = Zeroizing::new(
        pake.finish(&receive(&mut stream, PAKE_MESSAGE_BYTES, op)?)
            .map_err(|_| Error::Denied)?,
    );
    let mut channel = Channel::new(&secret, true)?;
    channel.send(&mut stream, &local, op)?;
    let remote = decode_identity(&channel.receive(&mut stream, op)?, &mut store.peers()?)?;
    channel.send(&mut stream, READY, op)?;
    channel.expect(&mut stream, STORED, op)?;
    enroll(store, &remote, op)?;
    channel.send(&mut stream, STORED, op)?;
    Ok(remote.contact)
}
/// Accept one bounded attempt. The caller owns PIN expiry and the global attempt limit.
pub fn accept(
    mut stream: TcpStream,
    pin: &str,
    store: &Store,
    local: &NearbyComputer,
    op: &Operation<'_>,
    authenticated: &mut bool,
) -> Result<NearbyComputer> {
    validate_pin(pin)?;
    let local = encode_identity(&store.identity()?, local)?;
    stream
        .set_nonblocking(true)
        .map_err(|_| Error::Unavailable)?;
    stream.set_nodelay(true).map_err(|_| Error::Unavailable)?;
    let mut protocol = vec![0; PROTOCOL.len()];
    read_exact(&mut stream, &mut protocol, op)?;
    if protocol != PROTOCOL {
        return Err(Error::Version);
    }
    let message = receive(&mut stream, PAKE_MESSAGE_BYTES, op)?;
    let (pake, reply) = Spake2::<Ed25519Group>::start_b(
        &Password::new(pin.as_bytes()),
        &PakeIdentity::new(CLIENT_ID),
        &PakeIdentity::new(SERVER_ID),
    );
    send(&mut stream, &reply, op)?;
    let secret = Zeroizing::new(pake.finish(&message).map_err(|_| Error::Denied)?);
    let mut channel = Channel::new(&secret, false)?;
    let remote = decode_identity(&channel.receive(&mut stream, op)?, &mut store.peers()?)?;
    *authenticated = true; // Consume the PIN even if the later commit is interrupted.
    channel.send(&mut stream, &local, op)?;
    channel.expect(&mut stream, READY, op)?;
    enroll(store, &remote, op)?;
    channel.send(&mut stream, STORED, op)?;
    channel.expect(&mut stream, STORED, op)?;
    Ok(remote.contact)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        net::TcpListener,
        path::PathBuf,
        time::{Duration, Instant},
    };
    const TEST_TIMEOUT: Duration = Duration::from_secs(3);
    struct Fixture(PathBuf);
    impl Fixture {
        fn new() -> Self {
            let path = std::env::temp_dir().join(format!(
                "kb-pin-{}-{}",
                std::process::id(),
                security::epoch().unwrap()
            ));
            Store::initialize(&path).unwrap();
            Self(path)
        }
        fn store(&self) -> Store {
            Store::open(&self.0).unwrap()
        }
        fn contact(&self, name: &str) -> NearbyComputer {
            NearbyComputer {
                name: name.into(),
                fingerprint: security::display_fingerprint(self.store().identity().unwrap().peer()),
                usb: "127.0.0.1:3337".into(),
                control: "127.0.0.1:3338".into(),
                pairing_port: None,
            }
        }
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }
    fn exchange(
        client_pin: &str,
    ) -> (
        Result<NearbyComputer>,
        Result<NearbyComputer>,
        bool,
        Fixture,
        Fixture,
    ) {
        let host = Fixture::new();
        let client = Fixture::new();
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let host_store = host.store();
        let host_contact = host.contact("Host");
        let worker = std::thread::spawn(move || {
            let (stream, _) = listener.accept().unwrap();
            let op = Operation::new(Instant::now() + TEST_TIMEOUT, &|| false);
            let mut authenticated = false;
            let result = accept(
                stream,
                "123456",
                &host_store,
                &host_contact,
                &op,
                &mut authenticated,
            );
            (result, authenticated)
        });
        let op = Operation::new(Instant::now() + TEST_TIMEOUT, &|| false);
        let joined = join(
            address,
            client_pin,
            &client.store(),
            &client.contact("Client"),
            &op,
        );
        let (accepted, authenticated) = worker.join().unwrap();
        (joined, accepted, authenticated, host, client)
    }
    #[test]
    fn one_pin_enrolls_both_certificates_with_authenticated_metadata() {
        let (joined, accepted, authenticated, host, client) = exchange("123456");
        assert_eq!(joined.unwrap(), host.contact("Host"));
        assert_eq!(accepted.unwrap(), client.contact("Client"));
        assert!(authenticated);
        assert!(
            host.store()
                .peers()
                .unwrap()
                .contains(client.store().identity().unwrap().peer())
        );
        assert!(
            client
                .store()
                .peers()
                .unwrap()
                .contains(host.store().identity().unwrap().peer())
        );
    }
    #[test]
    fn wrong_pin_never_enrolls_either_peer() {
        let (joined, accepted, authenticated, host, client) = exchange("654321");
        assert!(joined.is_err());
        assert_eq!(accepted, Err(Error::Denied));
        assert!(!authenticated);
        assert_eq!(host.store().peers().unwrap().identities().count(), 0);
        assert_eq!(client.store().peers().unwrap().identities().count(), 0);
    }
    #[test]
    fn aead_rejects_tampering_replay_and_direction_reflection() {
        let mut client = Channel::new(b"test session secret", true).unwrap();
        let mut server = Channel::new(b"test session secret", false).unwrap();
        let original = client.seal(b"certificate metadata").unwrap();
        let mut tampered = original.clone();
        tampered[0] ^= 1;
        assert_eq!(server.open(&mut tampered), Err(Error::Denied));
        assert_eq!(client.open(&mut original.clone()), Err(Error::Denied));
        assert_eq!(
            server.open(&mut original.clone()).unwrap(),
            b"certificate metadata"
        );
        assert_eq!(server.open(&mut original.clone()), Err(Error::Denied));
    }
    #[test]
    fn public_certificate_must_match_the_authenticated_fingerprint() {
        let fixture = Fixture::new();
        let identity = fixture.store().identity().unwrap();
        let mut contact = fixture.contact("Host");
        contact.fingerprint = "a".repeat(knightbus_core::PEER_ID_BYTES * 2);
        let mut bytes = (identity.certificate().len() as u32).to_be_bytes().to_vec();
        bytes.extend_from_slice(identity.certificate());
        bytes.extend_from_slice(&serde_json::to_vec(&contact).unwrap());
        assert!(matches!(
            decode_identity(&bytes, &mut Peers::default()),
            Err(Error::Denied)
        ));
    }
    #[test]
    fn dropping_before_confirmation_consumes_the_pin_but_does_not_enroll() {
        let host = Fixture::new();
        let client = Fixture::new();
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let store = host.store();
        let contact = host.contact("Host");
        let worker = std::thread::spawn(move || {
            let (stream, _) = listener.accept().unwrap();
            let mut authenticated = false;
            let op = Operation::new(Instant::now() + TEST_TIMEOUT, &|| false);
            let result = accept(stream, "123456", &store, &contact, &op, &mut authenticated);
            (result, authenticated)
        });
        let op = Operation::new(Instant::now() + TEST_TIMEOUT, &|| false);
        let mut stream = op.connect(address).unwrap();
        write_all(&mut stream, PROTOCOL, &op).unwrap();
        let (pake, message) = Spake2::<Ed25519Group>::start_a(
            &Password::new(b"123456"),
            &PakeIdentity::new(CLIENT_ID),
            &PakeIdentity::new(SERVER_ID),
        );
        send(&mut stream, &message, &op).unwrap();
        let secret = pake
            .finish(&receive(&mut stream, PAKE_MESSAGE_BYTES, &op).unwrap())
            .unwrap();
        let mut channel = Channel::new(&secret, true).unwrap();
        channel
            .send(
                &mut stream,
                &encode_identity(
                    &client.store().identity().unwrap(),
                    &client.contact("Client"),
                )
                .unwrap(),
                &op,
            )
            .unwrap();
        let _ = channel.receive(&mut stream, &op).unwrap();
        drop(stream);
        let (result, authenticated) = worker.join().unwrap();
        assert!(result.is_err());
        assert!(authenticated);
        assert_eq!(host.store().peers().unwrap().identities().count(), 0);
    }
    #[test]
    fn cancellation_and_deadlines_bound_a_stalled_peer() {
        use std::sync::{
            Arc,
            atomic::{AtomicBool, Ordering},
        };
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let mut client = TcpStream::connect(listener.local_addr().unwrap()).unwrap();
        let (mut server, _) = listener.accept().unwrap();
        server.set_nonblocking(true).unwrap();
        client.write_all(&[0]).unwrap();
        let signal = Arc::new(AtomicBool::new(false));
        let cancel = signal.clone();
        let worker = std::thread::spawn(move || {
            let stopped = || cancel.load(Ordering::Acquire);
            let op = Operation::new(Instant::now() + TEST_TIMEOUT, &stopped);
            receive(&mut server, MAX_RECORD_BYTES, &op)
        });
        signal.store(true, Ordering::Release);
        assert_eq!(worker.join().unwrap(), Err(Error::Cancelled));
        let mut server = TcpStream::connect(listener.local_addr().unwrap()).unwrap();
        server.set_nonblocking(true).unwrap();
        let _peer = listener.accept().unwrap();
        let started = Instant::now();
        let op = Operation::new(started + Duration::from_millis(60), &|| false);
        assert_eq!(
            receive(&mut server, MAX_RECORD_BYTES, &op),
            Err(Error::Deadline)
        );
        assert!(started.elapsed() < TEST_TIMEOUT);
    }
    #[test]
    fn oversized_network_frames_are_rejected_before_allocation() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let mut client = TcpStream::connect(listener.local_addr().unwrap()).unwrap();
        let (mut server, _) = listener.accept().unwrap();
        server.set_nonblocking(true).unwrap();
        client.write_all(&u32::MAX.to_be_bytes()).unwrap();
        let op = Operation::new(Instant::now() + TEST_TIMEOUT, &|| false);
        assert_eq!(
            receive(&mut server, MAX_RECORD_BYTES, &op),
            Err(Error::Invalid)
        );
    }
}
