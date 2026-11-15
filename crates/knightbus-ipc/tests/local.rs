//! Native local endpoint authentication and protected state tests.
use knightbus_core::*;
use knightbus_ipc::{
    local::{Client, Listener},
    private::Directory,
};
use std::{
    path::PathBuf,
    time::{Duration, Instant},
};
fn location(label: &str) -> PathBuf {
    let root = if cfg!(unix) {
        PathBuf::from("/tmp")
    } else {
        std::env::temp_dir()
    };
    root.join(format!(
        "knightbus-{label}-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ))
}
#[test]
fn private_storage_is_bounded_exclusive_and_refuses_paths() {
    let path = location("state");
    let directory = Directory::open(&path).unwrap();
    directory.create("identity", b"secret").unwrap();
    assert!(directory.create("identity", b"replacement").is_err());
    assert_eq!(directory.read("identity", 6).unwrap(), b"secret");
    assert_eq!(directory.read("identity", 5), Err(Error::Invalid));
    for name in ["../escape", "/absolute", "..", "a\\b", ""] {
        assert!(directory.create(name, b"x").is_err());
    }
    assert_eq!(directory.names(0), Err(Error::Exhausted));
    drop(directory);
    std::fs::remove_dir_all(path).unwrap();
}
#[test]
fn local_endpoint_authenticates_and_preserves_control_results() {
    let directory = location("ipc");
    let state = Directory::open(&directory).unwrap();
    #[cfg(unix)]
    let path = directory.join("management.sock");
    #[cfg(windows)]
    let path = PathBuf::from(format!(
        r"\\.\pipe\knightbus-test-{}",
        directory.file_name().unwrap().to_str().unwrap()
    ));
    let listener = Listener::bind(&path).unwrap();
    assert!(
        Listener::bind(&path).is_err(),
        "cannot replace an active endpoint"
    );
    let worker = std::thread::spawn(move || {
        let end = Instant::now() + Duration::from_secs(5);
        let connection = loop {
            if let Some(c) = listener.accept().unwrap() {
                break c;
            }
            assert!(Instant::now() < end);
        };
        let mut subscribed = false;
        let result = connection.serve(
            &knightbus_ipc::local::Cancellation::default(),
            |caller, command, _| {
                assert!(caller.administrator);
                match command {
                    Command::List => Ok(Reply::Devices(vec![])),
                    Command::Subscribe => {
                        subscribed = true;
                        Ok(Reply::Subscription(4))
                    }
                    Command::Unsubscribe(4) => {
                        subscribed = false;
                        Ok(Reply::Done)
                    }
                    _ => Err(Error::Denied),
                }
            },
        );
        assert_eq!(result, Err(Error::Unavailable));
        assert!(!subscribed, "disconnect retires subscriptions");
    });
    let mut client = Client::connect(&path).unwrap();
    assert_eq!(
        client.call(Command::List, u64::MAX),
        Ok(Reply::Devices(vec![]))
    );
    assert_eq!(client.call(Command::Poll(99), u64::MAX), Err(Error::Denied));
    assert_eq!(
        client.call(Command::Subscribe, u64::MAX),
        Ok(Reply::Subscription(4))
    );
    drop(client);
    worker.join().unwrap();
    drop(state);
    std::fs::remove_dir_all(directory).unwrap();
}
#[cfg(unix)]
#[test]
fn unsafe_directory_permissions_and_symlinks_are_rejected() {
    use std::os::unix::fs::{PermissionsExt, symlink};
    let directory = location("permissions");
    std::fs::create_dir(&directory).unwrap();
    std::fs::set_permissions(&directory, std::fs::Permissions::from_mode(0o755)).unwrap();
    assert!(Directory::open(&directory).is_err());
    assert!(Listener::bind(&directory.join("sock")).is_err());
    std::fs::set_permissions(&directory, std::fs::Permissions::from_mode(0o700)).unwrap();
    let state = Directory::open(&directory).unwrap();
    symlink("/etc/passwd", directory.join("identity")).unwrap();
    assert!(state.read("identity", 8192).is_err());
    drop(state);
    std::fs::remove_dir_all(directory).unwrap();
}

#[test]
fn incompatible_control_version_is_reported_without_dispatch() {
    use interprocess::local_socket::{GenericFilePath, Stream, prelude::*};
    use std::io::{Read, Write};
    let directory = location("version");
    let state = Directory::open(&directory).unwrap();
    #[cfg(unix)]
    let path = directory.join("socket");
    #[cfg(windows)]
    let path = PathBuf::from(format!(
        r"\\.\pipe\{}",
        directory.file_name().unwrap().to_str().unwrap()
    ));
    let listener = Listener::bind(&path).unwrap();
    let task = std::thread::spawn(move || {
        let end = Instant::now() + Duration::from_secs(5);
        let connection = loop {
            if let Some(c) = listener.accept().unwrap() {
                break c;
            }
            assert!(Instant::now() < end);
        };
        assert_eq!(
            connection.serve(
                &knightbus_ipc::local::Cancellation::default(),
                |_, _, _| panic!("version rejection must precede dispatch")
            ),
            Err(Error::Version)
        );
    });
    let mut stream = Stream::connect(path.to_fs_name::<GenericFilePath>().unwrap()).unwrap();
    let request = Request {
        id: 1,
        version: CONTROL_VERSION + 1,
        deadline: u64::MAX,
        command: Command::Shutdown,
    };
    stream
        .write_all(&knightbus_ipc::encode_request(&request).unwrap())
        .unwrap();
    let mut prefix = [0; 4];
    stream.read_exact(&mut prefix).unwrap();
    let length = u32::from_le_bytes(prefix) as usize;
    assert!(length < 128);
    let mut frame = prefix.to_vec();
    frame.resize(4 + length, 0);
    stream.read_exact(&mut frame[4..]).unwrap();
    assert_eq!(
        knightbus_ipc::decode_response(&frame).unwrap(),
        Response {
            id: 1,
            result: Err(Error::Version)
        }
    );
    drop(stream);
    task.join().unwrap();
    drop(state);
    std::fs::remove_dir_all(directory).unwrap();
}

fn endpoint(label: &str) -> (PathBuf, Directory, PathBuf, Listener) {
    let directory = location(label);
    let state = Directory::open(&directory).unwrap();
    #[cfg(unix)]
    let path = directory.join("socket");
    #[cfg(windows)]
    let path = PathBuf::from(format!(
        r"\\.\pipe\{}",
        directory.file_name().unwrap().to_str().unwrap()
    ));
    let listener = Listener::bind(&path).unwrap();
    (directory, state, path, listener)
}
fn accept(listener: &Listener) -> knightbus_ipc::local::Connection {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        if let Some(connection) = listener.accept().unwrap() {
            return connection;
        }
        assert!(Instant::now() < deadline);
    }
}

#[test]
fn cancellation_wakes_an_idle_client_and_retires_its_subscriptions() {
    use knightbus_ipc::local::Cancellation;
    let (directory, state, path, listener) = endpoint("cancel");
    let cancellation = Cancellation::default();
    let stop = cancellation.clone();
    let (done, finished) = std::sync::mpsc::channel();
    let worker = std::thread::spawn(move || {
        let mut retired = false;
        let result = accept(&listener).serve(&stop, |_, command, _| match command {
            Command::Subscribe => Ok(Reply::Subscription(1)),
            Command::Unsubscribe(1) => {
                retired = true;
                Ok(Reply::Done)
            }
            _ => Err(Error::Invalid),
        });
        done.send((result, retired)).unwrap();
    });
    let mut client = Client::connect(&path).unwrap();
    assert_eq!(
        client.call(Command::Subscribe, u64::MAX),
        Ok(Reply::Subscription(1))
    );
    cancellation.cancel();
    assert_eq!(
        finished.recv_timeout(Duration::from_secs(2)).unwrap(),
        (Err(Error::Cancelled), true)
    );
    drop(client);
    worker.join().unwrap();
    drop(state);
    std::fs::remove_dir_all(directory).unwrap();
}

#[test]
fn shutdown_acknowledges_then_closes_and_encoding_errors_keep_correlation() {
    use knightbus_ipc::local::Cancellation;
    let (directory, state, path, listener) = endpoint("shutdown");
    let worker = std::thread::spawn(move || {
        let stop = Cancellation::default();
        let result = accept(&listener).serve(&stop, |_, command, _| match command {
            Command::Permissions(_) => {
                Ok(Reply::Peers(vec![PeerId([1; 32]); MAX_DEVICE_GRANTS + 1]))
            }
            Command::List => Ok(Reply::Devices(vec![])),
            Command::Shutdown => {
                stop.cancel();
                Ok(Reply::Done)
            }
            _ => Err(Error::Invalid),
        });
        assert_eq!(result, Ok(()));
    });
    let mut client = Client::connect(&path).unwrap();
    assert_eq!(
        client.call(
            Command::Permissions(DeviceKey {
                id: DeviceId(1),
                generation: 1
            }),
            u64::MAX
        ),
        Err(Error::Exhausted)
    );
    assert_eq!(
        client.call(Command::List, u64::MAX),
        Ok(Reply::Devices(vec![]))
    );
    assert_eq!(client.call(Command::Shutdown, u64::MAX), Ok(Reply::Done));
    assert!(client.call(Command::List, u64::MAX).is_err());
    assert_eq!(client.call(Command::List, u64::MAX), Err(Error::State));
    drop(client);
    worker.join().unwrap();
    drop(state);
    std::fs::remove_dir_all(directory).unwrap();
}
