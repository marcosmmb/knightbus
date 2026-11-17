//! Host setup consent, authority, wire bounds and independent service lifetime.
use knightbus_sdk::core::host::{Reply as SetupReply, Request as Setup};
use knightbus_sdk::{Client, core::*, ipc, service::Service};
use std::{path::PathBuf, sync::Arc};
const ADMIN: Caller = Caller {
    peer: PeerId([0; PEER_ID_BYTES]),
    administrator: true,
};
const REMOTE: Caller = Caller {
    administrator: false,
    ..ADMIN
};
const TIMEOUT_MS: u64 = 5000;
struct Fixture(PathBuf);
impl Fixture {
    fn new() -> Self {
        let p = std::env::temp_dir().join(format!(
            "kb-service-{}-{}",
            std::process::id(),
            knightbus_transport::security::epoch().unwrap()
        ));
        ipc::private::Directory::open(&p).unwrap();
        Self(p)
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}
#[test]
fn setup_is_local_only_pairing_is_explicit_and_deadlines_precede_mutation() {
    let root = Fixture::new();
    let service = Service::open(&root.0.join("identity")).unwrap();
    let status = Command::Host(Setup::Status);
    assert_eq!(
        service.execute(REMOTE, status.clone(), u64::MAX),
        Err(Error::Denied)
    );
    let peer = knightbus_transport::security::Identity::generate().unwrap();
    let pair = Command::Host(Setup::Pair {
        certificate: knightbus_sdk::service::hex(peer.certificate()),
        fingerprint: knightbus_transport::security::display_fingerprint(peer.peer()),
    });
    assert_eq!(
        service.execute(ADMIN, pair.clone(), 0),
        Err(Error::Deadline)
    );
    assert_eq!(
        service.execute(REMOTE, pair.clone(), u64::MAX),
        Err(Error::Denied)
    );
    assert_eq!(service.execute(ADMIN, pair, u64::MAX), Ok(Reply::Done));
    let Reply::Host(SetupReply::Status(s)) =
        service.execute(ADMIN, status.clone(), u64::MAX).unwrap()
    else {
        panic!()
    };
    assert_eq!(s.peers.len(), 1);
    assert_eq!(s.role, "idle");
    assert_eq!(s.discovery, cfg!(feature = "mdns"));
    assert_eq!(
        service.execute(ADMIN, Command::List, u64::MAX),
        Ok(Reply::Devices(vec![]))
    );
    service
        .execute(
            ADMIN,
            Command::Host(Setup::Revoke {
                fingerprint: s.peers[0].clone(),
            }),
            u64::MAX,
        )
        .unwrap();
    let Reply::Host(SetupReply::Status(s)) = service.execute(ADMIN, status, u64::MAX).unwrap()
    else {
        panic!()
    };
    assert!(s.peers.is_empty());
}
#[test]
fn identity_and_pairing_survive_ui_and_service_restart_without_auto_attach() {
    let root = Fixture::new();
    let identity = root.0.join("identity");
    #[cfg(unix)]
    let endpoint = root.0.join("socket");
    #[cfg(windows)]
    let endpoint = PathBuf::from(format!(
        r"\\.\pipe\kb-service-test-{}",
        knightbus_transport::security::epoch().unwrap()
    ));
    let mut fingerprint = None;
    for _ in 0..2 {
        let listener = ipc::local::Listener::bind(&endpoint).unwrap();
        let host = Arc::new(Service::open(&identity).unwrap());
        let runner = host.clone();
        let worker = std::thread::spawn(move || runner.serve(&listener));
        for _ in 0..2 {
            let mut client = Client::connect(&endpoint).unwrap();
            let Reply::Host(SetupReply::Status(s)) = client
                .request(Command::Host(Setup::Status), TIMEOUT_MS)
                .unwrap()
            else {
                panic!()
            };
            if let Some(expected) = &fingerprint {
                assert_eq!(&s.fingerprint, expected);
            } else {
                fingerprint = Some(s.fingerprint);
            }
            assert_eq!(s.role, "idle");
            assert_eq!(s.discovery, cfg!(feature = "mdns"));
            assert_eq!(
                client.request(Command::Sessions, TIMEOUT_MS),
                Ok(Reply::Sessions(vec![]))
            );
        }
        host.shutdown();
        worker.join().unwrap().unwrap();
    }
}
#[test]
fn host_schema_roundtrips_and_rejects_oversize_and_unknown_fields() {
    let request = Request {
        version: CONTROL_VERSION,
        id: 1,
        deadline: u64::MAX,
        command: Command::Host(Setup::Inspect {
            registry: u64::MAX.to_string(),
        }),
    };
    let encoded = ipc::encode_request(&request).unwrap();
    assert_eq!(ipc::decode_request(&encoded).unwrap(), request);
    for cut in 0..encoded.len() {
        assert!(ipc::decode_request(&encoded[..cut]).is_err());
    }
    let oversized = Request {
        command: Command::Host(Setup::Pair {
            certificate: "f".repeat(ipc::MAX_FRAME),
            fingerprint: String::new(),
        }),
        ..request
    };
    assert_eq!(ipc::encode_request(&oversized), Err(Error::Exhausted));
    let mut unknown = encoded[..encoded.len() - 1].to_vec();
    unknown.extend_from_slice(b",\"administrator\":true}");
    const LENGTH_BYTES: usize = size_of::<u32>();
    let length = (unknown.len() - LENGTH_BYTES) as u32;
    unknown[..LENGTH_BYTES].copy_from_slice(&length.to_le_bytes());
    assert_eq!(ipc::decode_request(&unknown), Err(Error::Invalid));
    let mut corrupted = encoded;
    *corrupted.last_mut().unwrap() = b'x';
    assert_eq!(ipc::decode_request(&corrupted), Err(Error::Invalid));
}

#[cfg(target_os = "macos")]
#[test]
fn a_same_account_daemon_cannot_impersonate_the_root_service() {
    let root = Fixture::new();
    let endpoint = root.0.join("socket");
    // Normal tests run unprivileged. Root-only integration is kept in the lab.
    match ipc::local::Listener::bind_for_user(&endpoint, 0) {
        Err(Error::Denied) => {}
        Ok(_) => return, // This account already is root; it cannot impersonate itself.
        Err(error) => panic!("Unexpected endpoint failure: {error}"),
    }
    let listener = ipc::local::Listener::bind(&endpoint).unwrap();
    let worker = std::thread::spawn(move || {
        loop {
            if listener.accept().unwrap().is_some() {
                break;
            }
        }
    });
    assert!(matches!(
        Client::connect_system(&endpoint),
        Err(Error::Denied)
    ));
    worker.join().unwrap();
}

#[test]
fn host_frames_fit_the_existing_credential_and_pairing_limits() {
    use knightbus_transport::security::{MAX_CREDENTIAL_BYTES, MAX_PEERS};
    const HEX_DIGITS_PER_BYTE: usize = 2;
    let certificate = "f".repeat(MAX_CREDENTIAL_BYTES * HEX_DIGITS_PER_BYTE);
    let fingerprint = "a".repeat(PEER_ID_BYTES * HEX_DIGITS_PER_BYTE);
    let request = Request {
        version: CONTROL_VERSION,
        id: 1,
        deadline: u64::MAX,
        command: Command::Host(Setup::Pair {
            certificate: certificate.clone(),
            fingerprint: fingerprint.clone(),
        }),
    };
    assert_eq!(
        ipc::decode_request(&ipc::encode_request(&request).unwrap()).unwrap(),
        request
    );
    let response = Response {
        id: 1,
        result: Ok(Reply::Host(SetupReply::Status(host::Status {
            fingerprint: fingerprint.clone(),
            certificate,
            peers: vec![fingerprint; MAX_PEERS],
            exporter: false,
            importer: false,
            discovery: false,
            pin_pairing: true,
            process_id: 1,
            capture_ready: false,
            storage_consent_ready: false,
            driver: None,
            role: "idle".into(),
            selection: String::new(),
            guidance: String::new(),
        }))),
    };
    assert_eq!(
        ipc::decode_response(&ipc::encode_response(&response).unwrap()).unwrap(),
        response
    );
}

#[test]
fn physical_names_fit_the_host_frame_at_inventory_capacity() {
    let device = host::Physical {
        id: u64::MAX.to_string(),
        registry: u64::MAX.to_string(),
        vendor: u16::MAX,
        product: u16::MAX,
        name: Some("\\".repeat(host::MAX_DEVICE_NAME_BYTES)),
    };
    let response = Response {
        id: 1,
        result: Ok(Reply::Host(SetupReply::Physical(
            vec![device; host::MAX_PHYSICAL_DEVICES],
        ))),
    };
    let encoded = ipc::encode_response(&response).unwrap();
    assert_eq!(ipc::decode_response(&encoded).unwrap(), response);
}

#[test]
fn discovery_administration_never_pairs_or_selects_a_backend() {
    let root = Fixture::new();
    let service = Service::open(&root.0.join("identity")).unwrap();
    let disable = Command::Host(Setup::Advertise {
        enabled: false,
        name: String::new(),
        usb: String::new(),
        control: String::new(),
    });
    assert_eq!(
        service.execute(REMOTE, disable.clone(), u64::MAX),
        Err(Error::Denied)
    );
    assert_eq!(
        service.execute(ADMIN, disable.clone(), 0),
        Err(Error::Deadline)
    );
    let expected = if cfg!(feature = "mdns") {
        Ok(Reply::Done)
    } else {
        Err(Error::Unsupported)
    };
    assert_eq!(service.execute(ADMIN, disable, u64::MAX), expected);
    let invalid = Command::Host(Setup::Advertise {
        enabled: true,
        name: "Hagrid".into(),
        usb: "0.0.0.0:3337".into(),
        control: "0.0.0.0:3338".into(),
    });
    let expected = if cfg!(feature = "mdns") {
        Error::Invalid
    } else {
        Error::Unsupported
    };
    assert_eq!(service.execute(ADMIN, invalid, u64::MAX), Err(expected));
    let Reply::Host(SetupReply::Status(status)) = service
        .execute(ADMIN, Command::Host(Setup::Status), u64::MAX)
        .unwrap()
    else {
        panic!()
    };
    assert!(status.peers.is_empty());
    assert_eq!(status.role, "idle");
}
#[test]
fn nearby_computers_fit_the_control_frame_at_capacity() {
    let computer = host::NearbyComputer {
        name: "\\".repeat(host::MAX_COMPUTER_NAME_BYTES),
        fingerprint: "a".repeat(PEER_ID_BYTES * 2),
        usb: "192.168.1.20:3337".into(),
        control: "192.168.1.20:3338".into(),
        pairing_port: None,
    };
    let response = Response {
        id: 1,
        result: Ok(Reply::Host(SetupReply::Nearby(
            vec![computer; host::MAX_NEARBY_COMPUTERS],
        ))),
    };
    let encoded = ipc::encode_response(&response).unwrap();
    assert_eq!(ipc::decode_response(&encoded).unwrap(), response);
}

#[test]
fn pin_pairing_is_local_only_cancelable_and_never_selects_a_usb_role() {
    let root = Fixture::new();
    let service = Service::open(&root.0.join("identity")).unwrap();
    let request = Setup::PairingStart {
        name: "Host".into(),
        usb: "127.0.0.1:3337".into(),
        control: "127.0.0.1:3338".into(),
    };
    for request in [request.clone(), Setup::PairingStatus, Setup::PairingCancel] {
        assert_eq!(
            service.execute(REMOTE, Command::Host(request), u64::MAX),
            Err(Error::Denied)
        );
    }
    assert_eq!(
        service.execute(ADMIN, Command::Host(request.clone()), 0),
        Err(Error::Deadline)
    );
    let Reply::Host(SetupReply::PairingOffer(offer)) = service
        .execute(ADMIN, Command::Host(request), u64::MAX)
        .unwrap()
    else {
        panic!()
    };
    assert_eq!(offer.pin.0.len(), host::PAIRING_PIN_DIGITS);
    assert!(!format!("{offer:?}").contains(&offer.pin.0));
    service
        .execute(ADMIN, Command::Host(Setup::PairingCancel), u64::MAX)
        .unwrap();
    assert!(std::net::TcpStream::connect(&offer.address).is_err());
    let Reply::Host(SetupReply::Status(status)) = service
        .execute(ADMIN, Command::Host(Setup::Status), u64::MAX)
        .unwrap()
    else {
        panic!()
    };
    assert!(status.peers.is_empty());
    assert_eq!(status.role, "idle");
    assert!(status.pin_pairing);
}
