//! Automated process-boundary contract tests. Physical hardware fixtures live in lab/.
#[cfg(test)]
mod tests {
    use knightbus_core::*;
    use knightbus_sdk::{
        Client, Embedded, Runtime,
        ipc::{self, local::Listener, private::Directory},
    };
    use knightbus_transport::{
        exporter::Mono,
        security::{Identity, Peers},
    };
    use std::{
        net::TcpListener,
        path::PathBuf,
        task::Poll,
        time::{Duration, Instant},
    };
    const KEY: DeviceKey = DeviceKey {
        id: DeviceId(1),
        generation: 2,
    };
    const PEER: PeerId = PeerId([0; 32]);
    struct Backend;
    impl ExporterBackend for Backend {
        fn set_waker(&mut self, _: Option<std::task::Waker>) {}
        fn manages_topology(&self) -> bool {
            false
        }
        fn poll_removed(&mut self) -> Option<DeviceKey> {
            None
        }
        fn claim(&mut self, _: SessionId, _: &Device) -> Poll<Result<()>> {
            Poll::Ready(Ok(()))
        }
        fn submit(
            &mut self,
            _: TransferId,
            t: Transfer,
        ) -> std::result::Result<(), (Error, Transfer)> {
            Err((Error::Unsupported, t))
        }
        fn cancel(&mut self, _: TransferId) {}
        fn poll_completion(&mut self) -> Option<(TransferId, Completion)> {
            None
        }
        fn release(&mut self, _: SessionId, _: DeviceKey) -> Poll<Result<()>> {
            Poll::Ready(Ok(()))
        }
    }
    fn device() -> Device {
        Device {
            key: KEY,
            wire: usbip::Device {
                path: usbip::WireString::new(b"/fixture").unwrap(),
                bus_id: KEY.id.bus_id(),
                bus_number: 1,
                device_number: 1,
                speed: 3,
                vendor_id: 0x1234,
                product_id: 0x5678,
                device_bcd: 0x100,
                class: 0,
                subclass: 0,
                protocol: 0,
                configuration: 1,
                configurations: 1,
                interfaces: 1,
            },
            configurations: vec![Configuration {
                value: 1,
                interfaces: vec![Interface {
                    number: 0,
                    alternate: 0,
                    class: usbip::Interface {
                        class: 3,
                        subclass: 0,
                        protocol: 0,
                    },
                    endpoints: vec![Endpoint {
                        address: 0x81,
                        kind: TransferKind::Interrupt,
                        max_packet_size: 64,
                        service_bytes: 64,
                        interval: 1,
                        allowed_flags: 0,
                    }],
                }],
            }],
        }
    }
    pub(crate) fn runtime() -> Runtime {
        let mut engine =
            Engine::new(Mono::default(), Config::default(), 42, |_| Ok(Backend)).unwrap();
        engine.register(device()).unwrap();
        Runtime::exporter(
            engine,
            Identity::generate().unwrap(),
            Peers::default(),
            TcpListener::bind("127.0.0.1:0").unwrap(),
            TcpListener::bind("127.0.0.1:0").unwrap(),
            None,
        )
        .unwrap()
    }
    pub(crate) fn location() -> (PathBuf, PathBuf) {
        let root = std::env::temp_dir().join(format!(
            "kb-parity-{}-{}",
            std::process::id(),
            knightbus_transport::security::epoch().unwrap()
        ));
        Directory::open(&root).unwrap();
        #[cfg(unix)]
        let endpoint = root.join("control");
        #[cfg(windows)]
        let endpoint = PathBuf::from(format!(
            r"\\.\pipe\kb-parity-{}",
            root.file_name().unwrap().to_str().unwrap()
        ));
        (root, endpoint)
    }
    fn wait(
        call: &mut impl FnMut(Command) -> Result<Reply>,
        id: SessionId,
        expected: SessionState,
    ) {
        let end = Instant::now() + Duration::from_secs(5);
        loop {
            let Reply::Session(s) = call(Command::Status(id)).unwrap() else {
                panic!()
            };
            if s.state == expected {
                return;
            }
            assert!(Instant::now() < end, "{s:?}");
            std::thread::sleep(Duration::from_millis(100));
        }
    }
    fn scenario(mut call: impl FnMut(Command) -> Result<Reply>) -> Vec<EventKind> {
        assert!(matches!(
            call(Command::Info),
            Ok(Reply::Info {
                version: CONTROL_VERSION,
                epoch: 42,
                ..
            })
        ));
        assert_eq!(
            call(Command::Inspect(KEY)),
            Ok(Reply::Inspection(Box::new(device())))
        );
        assert_eq!(call(Command::Permissions(KEY)), Ok(Reply::Peers(vec![])));
        let Reply::Subscription(subscription) = call(Command::Subscribe).unwrap() else {
            panic!()
        };
        assert_eq!(
            call(Command::Authorize {
                device: KEY,
                peer: PEER,
                allow: true
            }),
            Ok(Reply::Done)
        );
        assert_eq!(
            call(Command::Permissions(KEY)),
            Ok(Reply::Peers(vec![PEER]))
        );
        let Reply::Session(s) = call(Command::Attach(vec![KEY])).unwrap() else {
            panic!()
        };
        wait(&mut call, s.id, SessionState::Active);
        assert_eq!(call(Command::Sessions), Ok(Reply::Sessions(vec![s.id])));
        assert_eq!(call(Command::Unshare(KEY)), Ok(Reply::Done));
        assert_eq!(call(Command::Permissions(KEY)), Ok(Reply::Peers(vec![])));
        wait(&mut call, s.id, SessionState::Closed);
        assert_eq!(call(Command::Detach(s.id)), Ok(Reply::Done));
        let mut events = vec![];
        while let Reply::Event(Some(e)) = call(Command::Poll(subscription)).unwrap() {
            events.push(e.kind);
        }
        assert_eq!(call(Command::Unsubscribe(subscription)), Ok(Reply::Done));
        assert_eq!(call(Command::Poll(subscription)), Err(Error::Denied));
        assert_eq!(call(Command::Forget(s.id)), Ok(Reply::Done));
        assert_eq!(call(Command::Status(s.id)), Err(Error::Denied));
        events
    }
    #[test]
    fn service_child() {
        let Some(path) = std::env::var_os("KNIGHTBUS_PARITY_ENDPOINT") else {
            return;
        };
        let listener = Listener::bind(&PathBuf::from(path)).unwrap();
        let host = runtime();
        host.serve(&listener).unwrap();
    }
    #[test]
    fn actual_process_and_embedded_have_identical_control_events() {
        let host = runtime();
        let mut embedded = Embedded::connect(&host).unwrap();
        let expected = scenario(|c| embedded.request(c, 5000));
        host.shutdown();
        host.join().unwrap();
        let (root, endpoint) = location();
        let mut child = std::process::Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "tests::service_child", "--nocapture"])
            .env("KNIGHTBUS_PARITY_ENDPOINT", &endpoint)
            .spawn()
            .unwrap();
        let end = Instant::now() + Duration::from_secs(10);
        let mut client = loop {
            if let Ok(client) = Client::connect(&endpoint) {
                break client;
            }
            assert!(Instant::now() < end);
            std::thread::sleep(Duration::from_millis(100));
        };
        assert_eq!(scenario(|c| client.request(c, 5000)), expected);
        drop(client);
        // UI exit does not stop the host. A new connection can inspect and stop it.
        let mut client = Client::connect(&endpoint).unwrap();
        assert!(client.request(Command::Info, 5000).is_ok());
        client.request(Command::Shutdown, 5000).unwrap();
        assert!(child.wait().unwrap().success());
        std::fs::remove_dir_all(root).unwrap();
    }
    #[test]
    fn service_clients_can_exit_during_a_lease_and_cannot_steal_subscriptions() {
        let host = runtime();
        let (root, endpoint) = location();
        let listener = Listener::bind(&endpoint).unwrap();
        let runner = host.clone();
        let worker = std::thread::spawn(move || runner.serve(&listener));
        let mut client = Client::connect(&endpoint).unwrap();
        assert_eq!(client.request(Command::List, 0), Err(Error::Deadline));
        client
            .request(
                Command::Authorize {
                    device: KEY,
                    peer: PEER,
                    allow: true,
                },
                5000,
            )
            .unwrap();
        let Reply::Subscription(subscription) = client.request(Command::Subscribe, 5000).unwrap()
        else {
            panic!()
        };
        let Reply::Session(session) = client.request(Command::Attach(vec![KEY]), 5000).unwrap()
        else {
            panic!()
        };
        wait(
            &mut |c| client.request(c, 5000),
            session.id,
            SessionState::Active,
        );
        let mut other = Client::connect(&endpoint).unwrap();
        assert_eq!(
            other.request(Command::Poll(subscription), 5000),
            Err(Error::Denied)
        );
        drop(client);
        assert!(matches!(
            other.request(Command::Status(session.id), 5000),
            Ok(Reply::Session(Session {
                state: SessionState::Active,
                ..
            }))
        ));
        other.request(Command::Detach(session.id), 5000).unwrap();
        wait(
            &mut |c| other.request(c, 5000),
            session.id,
            SessionState::Closed,
        );
        other.request(Command::Shutdown, 5000).unwrap();
        worker.join().unwrap().unwrap();
        std::fs::remove_dir_all(root).unwrap();
    }
    #[test]
    fn descriptor_codec_and_new_commands_are_bounded_and_round_trip() {
        let response = Response {
            id: 9,
            result: Ok(Reply::Inspection(Box::new(device()))),
        };
        let encoded = ipc::encode_response(&response).unwrap();
        assert_eq!(ipc::decode_response(&encoded), Ok(response));
        for length in 0..encoded.len() {
            assert!(ipc::decode_response(&encoded[..length]).is_err());
        }
        for command in [
            Command::Info,
            Command::Inspect(KEY),
            Command::Sessions,
            Command::Unshare(KEY),
        ] {
            let request = Request {
                version: CONTROL_VERSION,
                id: 1,
                deadline: 200,
                command,
            };
            assert_eq!(
                ipc::decode_request(&ipc::encode_request(&request).unwrap()),
                Ok(request)
            );
        }
        let mut big = device();
        big.configurations[0].interfaces = vec![big.configurations[0].interfaces[0].clone(); 256];
        big.configurations = vec![big.configurations[0].clone(); 255];
        assert_eq!(
            ipc::encode_response(&Response {
                id: 1,
                result: Ok(Reply::Inspection(Box::new(big)))
            }),
            Err(Error::Exhausted)
        );
    }
    #[test]
    fn inspection_and_sessions_do_not_bypass_peer_authorization() {
        let mut engine =
            Engine::new(Mono::default(), Config::default(), 42, |_| Ok(Backend)).unwrap();
        engine.register(device()).unwrap();
        let caller = Caller {
            peer: PEER,
            administrator: false,
        };
        assert_eq!(
            engine.execute(caller, Command::Inspect(KEY), u64::MAX),
            Err(Error::Denied)
        );
        assert_eq!(
            engine.execute(caller, Command::Unshare(KEY), u64::MAX),
            Err(Error::Denied)
        );
        assert_eq!(
            engine.execute(caller, Command::Sessions, u64::MAX),
            Ok(Reply::Sessions(vec![]))
        );
        assert_eq!(
            engine.execute(caller, Command::Info, 0),
            Err(Error::Deadline)
        );
    }
}

#[cfg(test)]
#[path = "../../mode-benchmarks/control.rs"]
mod benchmarks;
