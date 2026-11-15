//! OS-authenticated local management. USB data never enters these endpoints.
use crate::wire::{LENGTH_PREFIX_BYTES, MAX_FRAME};
use interprocess::local_socket::{
    GenericFilePath, ListenerOptions,
    tokio::{Stream, prelude::*},
};
use knightbus_core::*;
use std::sync::Arc;
use std::{
    path::Path,
    time::{Duration, Instant},
};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    runtime::{Builder, Runtime},
};
const ACCEPT_TIMEOUT: Duration = Duration::from_millis(100);
const RATE_WINDOW: Duration = Duration::from_secs(1);
const REQUESTS_PER_WINDOW: u32 = 32;
const IO_WORKERS: usize = 1;
#[cfg(windows)]
const PIPE_PREFIX: &str = r"\\.\pipe\";
const IO_TIMEOUT: Duration = Duration::from_secs(5);
const IDLE_TIMEOUT: Duration = Duration::from_secs(30);

/// Shared cancellation for a host's local management connections.
#[derive(Clone)]
pub struct Cancellation(tokio::sync::watch::Sender<bool>);
impl Default for Cancellation {
    fn default() -> Self {
        Self(tokio::sync::watch::channel(false).0)
    }
}
impl Cancellation {
    /// Wake all pending management I/O and prevent further requests.
    pub fn cancel(&self) {
        self.0.send_replace(true);
    }
    /// Whether shutdown has been requested.
    pub fn is_cancelled(&self) -> bool {
        *self.0.borrow()
    }
    async fn cancelled(&self) {
        let mut receiver = self.0.subscribe();
        while !*receiver.borrow_and_update() {
            if receiver.changed().await.is_err() {
                break;
            }
        }
    }
}

/// Restricted local listener. Unix permits only its owner UID; Windows permits
/// SYSTEM and the owning account through an explicit protected pipe DACL.
pub struct Listener {
    listener: interprocess::local_socket::tokio::Listener,
    runtime: Arc<Runtime>,
    #[cfg(unix)]
    owner: u32,
}
impl Listener {
    /// Bind without replacing an existing endpoint. On Unix the parent must be
    /// a private directory owned by the current user. Windows requires a local pipe path.
    pub fn bind(path: &Path) -> Result<Self> {
        let name = path
            .to_fs_name::<GenericFilePath>()
            .map_err(|_| Error::Invalid)?;
        let options = ListenerOptions::new().name(name);
        #[cfg(unix)]
        let (options, owner) = {
            use std::os::unix::fs::MetadataExt;
            let parent = path.parent().ok_or(Error::Invalid)?;
            let metadata = std::fs::symlink_metadata(parent).map_err(|_| Error::Denied)?;
            let owner = rustix::process::geteuid().as_raw();
            if !path.is_absolute()
                || !metadata.is_dir()
                || metadata.uid() != owner
                || metadata.mode() & crate::private::NON_OWNER_ACCESS != 0
            {
                return Err(Error::Denied);
            }
            // macOS does not support pre-bind fchmod on sockets. Its private
            // parent and the connected peer UID are the access boundary.
            #[cfg(target_os = "linux")]
            let options = {
                use interprocess::os::unix::local_socket::ListenerOptionsExt;
                options.mode(crate::private::FILE_MODE)
            };
            (options, owner)
        };
        #[cfg(windows)]
        let options = {
            use interprocess::os::windows::{
                local_socket::ListenerOptionsExt, security_descriptor::SecurityDescriptor,
            };
            validate_pipe_path(path)?;
            let sid = crate::private::windows::user()?;
            let sddl =
                widestring::U16CString::from_str(format!("O:{sid}D:P(A;;GA;;;SY)(A;;GA;;;{sid})"))
                    .map_err(|_| Error::Internal)?;
            options.security_descriptor(
                SecurityDescriptor::deserialize(&sddl).map_err(|_| Error::Denied)?,
            )
        };
        let runtime = runtime()?;
        let listener = {
            let _guard = runtime.enter();
            options.create_tokio().map_err(|_| Error::Unavailable)?
        };
        Ok(Self {
            listener,
            runtime,
            #[cfg(unix)]
            owner,
        })
    }
    /// Accept only an OS-authorized connection. No request can supply its own authority.
    pub fn accept(&self) -> Result<Option<Connection>> {
        let stream = match self
            .runtime
            .block_on(async { tokio::time::timeout(ACCEPT_TIMEOUT, self.listener.accept()).await })
        {
            Ok(Ok(s)) => s,
            Err(_) => return Ok(None),
            Ok(Err(_)) => return Err(Error::Unavailable),
        };
        #[cfg(unix)]
        {
            let credentials = stream.peer_creds().map_err(|_| Error::Denied)?;
            if credentials.euid() != Some(self.owner) {
                return Err(Error::Denied);
            }
        }
        // The endpoint has one administrative role. Remote identities are never accepted here.
        let caller = Caller {
            peer: PeerId([0; 32]),
            administrator: true,
        };
        Ok(Some(Connection {
            stream,
            runtime: self.runtime.clone(),
            caller,
            last_id: 0,
            subscriptions: Default::default(),
        }))
    }
}
/// OS-authorized management connection with strict request ordering and scoped subscriptions.
pub struct Connection {
    stream: Stream,
    runtime: Arc<Runtime>,
    caller: Caller,
    last_id: u64,
    subscriptions: std::collections::BTreeSet<u64>,
}
impl Connection {
    /// Dispatch requests with server-local deadlines. Runs until disconnect, idle
    /// timeout or shutdown; callers bound the number of concurrent connections.
    pub fn serve(
        mut self,
        cancellation: &Cancellation,
        mut execute: impl FnMut(Caller, Command, u64) -> Result<Reply>,
    ) -> Result<()> {
        let result = self.serve_requests(cancellation, &mut execute);
        for id in self.subscriptions {
            let _ = execute(self.caller, Command::Unsubscribe(id), u64::MAX);
        }
        result
    }

    fn serve_requests(
        &mut self,
        cancellation: &Cancellation,
        execute: &mut impl FnMut(Caller, Command, u64) -> Result<Reply>,
    ) -> Result<()> {
        let mut rate = RequestRate::new();
        loop {
            let frame = self.runtime.block_on(async {
                tokio::select! {
                    biased;
                    _ = cancellation.cancelled() => Err(Error::Cancelled),
                    frame = read_frame(&mut self.stream, IDLE_TIMEOUT) => frame,
                }
            })?;
            rate.admit()?;
            let request = crate::decode_request(&frame)?;
            if request.id <= self.last_id {
                return Err(Error::Invalid);
            }
            self.last_id = request.id;
            let incompatible = request.version != CONTROL_VERSION;
            let shutdown = matches!(request.command, Command::Shutdown);
            let result = if incompatible {
                Err(Error::Version)
            } else {
                self.dispatch(request.command, request.deadline, execute)
            };
            let shutdown = shutdown && result.is_ok();
            let response = Response {
                id: request.id,
                result,
            };
            let bytes = crate::encode_response(&response).or_else(|error| {
                crate::encode_response(&Response {
                    id: request.id,
                    result: Err(error),
                })
            })?;
            self.runtime.block_on(async {
                if shutdown {
                    // The initiator receives its acknowledgement before closing.
                    write_frame(&mut self.stream, &bytes).await
                } else {
                    tokio::select! {
                        biased;
                        _ = cancellation.cancelled() => Err(Error::Cancelled),
                        result = write_frame(&mut self.stream, &bytes) => result,
                    }
                }
            })?;
            if shutdown {
                return Ok(());
            }
            if incompatible {
                return Err(Error::Version);
            }
        }
    }

    fn dispatch(
        &mut self,
        command: Command,
        deadline: u64,
        execute: &mut impl FnMut(Caller, Command, u64) -> Result<Reply>,
    ) -> Result<Reply> {
        if let Command::Poll(id) | Command::Unsubscribe(id) = command
            && !self.subscriptions.contains(&id)
        {
            return Err(Error::Denied);
        }
        let release = match command {
            Command::Unsubscribe(id) => Some(id),
            _ => None,
        };
        // Local clients share the host clock; preserve their deadlines.
        let reply = execute(self.caller, command, deadline)?;
        if let Reply::Subscription(id) = &reply {
            self.subscriptions.insert(*id);
        }
        if let Some(id) = release {
            self.subscriptions.remove(&id);
        }
        Ok(reply)
    }
}
/// Local IPC client using the same versioned commands and stable errors as direct calls.
pub struct Client {
    failed: bool,
    stream: Stream,
    next: u64,
    runtime: Arc<Runtime>,
}
impl Client {
    /// Connect to an explicitly selected local management endpoint.
    pub fn connect(path: &Path) -> Result<Self> {
        #[cfg(windows)]
        {
            validate_pipe_path(path)?;
        }

        let name = path
            .to_fs_name::<GenericFilePath>()
            .map_err(|_| Error::Invalid)?;
        let runtime = runtime()?;
        let stream = runtime
            .block_on(async { tokio::time::timeout(IO_TIMEOUT, Stream::connect(name)).await })
            .map_err(|_| Error::Deadline)?
            .map_err(|_| Error::Unavailable)?;
        #[cfg(unix)]
        if stream.peer_creds().map_err(|_| Error::Denied)?.euid()
            != Some(rustix::process::geteuid().as_raw())
        {
            return Err(Error::Denied);
        }
        #[cfg(windows)]
        {
            use std::os::windows::io::{AsHandle, AsRawHandle};
            let Stream::NamedPipe(pipe) = &stream;
            crate::private::windows::verify_pipe_owner(pipe.as_handle().as_raw_handle())?;
        }
        Ok(Self {
            stream,
            next: 0,
            failed: false,
            runtime,
        })
    }
}
impl KnightBusControl for Client {
    fn call(&mut self, command: Command, deadline: u64) -> Result<Reply> {
        if self.failed {
            return Err(Error::State);
        }
        self.next = self.next.checked_add(1).ok_or(Error::Exhausted)?;
        let bytes = crate::encode_request(&Request {
            version: CONTROL_VERSION,
            id: self.next,
            deadline,
            command,
        })?;
        // After ambiguous I/O failure, never append another mutation to this stream.
        // The caller must reconnect and inspect state before deciding to retry.
        let response = (|| {
            self.runtime
                .block_on(write_frame(&mut self.stream, &bytes))?;
            let response = crate::decode_response(
                &self
                    .runtime
                    .block_on(read_frame(&mut self.stream, IO_TIMEOUT))?,
            )?;
            if response.id != self.next {
                return Err(Error::Invalid);
            }
            Ok(response)
        })();
        match response {
            Ok(response) => response.result,
            Err(error) => {
                self.failed = true;
                Err(error)
            }
        }
    }
}
#[cfg(windows)]
fn validate_pipe_path(path: &Path) -> Result<()> {
    let name = path
        .to_str()
        .and_then(|text| text.strip_prefix(PIPE_PREFIX))
        .ok_or(Error::Invalid)?;
    if name.contains(['\\', '/']) {
        return Err(Error::Invalid);
    }
    Ok(())
}

struct RequestRate {
    count: u32,
    window: Instant,
}
impl RequestRate {
    fn new() -> Self {
        Self {
            count: 0,
            window: Instant::now(),
        }
    }
    fn admit(&mut self) -> Result<()> {
        if self.window.elapsed() >= RATE_WINDOW {
            self.window = Instant::now();
            self.count = 0;
        }
        if self.count >= REQUESTS_PER_WINDOW {
            return Err(Error::Exhausted);
        }
        self.count += 1;
        Ok(())
    }
}

fn runtime() -> Result<Arc<Runtime>> {
    Builder::new_multi_thread()
        .worker_threads(IO_WORKERS)
        .enable_all()
        .build()
        .map(Arc::new)
        .map_err(|_| Error::Unavailable)
}
async fn read_frame(stream: &mut Stream, idle: Duration) -> Result<Vec<u8>> {
    let mut buffer = [0; MAX_FRAME + LENGTH_PREFIX_BYTES];
    let mut used = 0;
    let mut expected = LENGTH_PREFIX_BYTES;
    let mut deadline = tokio::time::Instant::now() + idle;
    loop {
        let n = tokio::time::timeout_at(deadline, stream.read(&mut buffer[used..expected]))
            .await
            .map_err(|_| Error::Deadline)?
            .map_err(|_| Error::Unavailable)?;
        if n == 0 {
            return Err(Error::Unavailable);
        }
        if used == 0 {
            deadline = tokio::time::Instant::now() + IO_TIMEOUT;
        }
        used += n;
        if used == LENGTH_PREFIX_BYTES && expected == LENGTH_PREFIX_BYTES {
            let size =
                u32::from_le_bytes(buffer[..LENGTH_PREFIX_BYTES].try_into().unwrap()) as usize;
            if size == 0 || size > MAX_FRAME {
                return Err(Error::Invalid);
            }
            expected = LENGTH_PREFIX_BYTES + size;
        } else if used == expected {
            return Ok(buffer[..used].to_vec());
        }
    }
}
async fn write_frame(stream: &mut Stream, bytes: &[u8]) -> Result<()> {
    if bytes.len() > MAX_FRAME + LENGTH_PREFIX_BYTES {
        return Err(Error::Invalid);
    }
    tokio::time::timeout(IO_TIMEOUT, stream.write_all(bytes))
        .await
        .map_err(|_| Error::Deadline)?
        .map_err(|_| Error::Unavailable)
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    #[test]
    fn accepted_socket_credentials_must_match_the_configured_owner() {
        let path = std::path::PathBuf::from(format!("/tmp/kb-ipc-uid-{}", std::process::id()));
        let directory = crate::private::Directory::open(&path).unwrap();
        let socket = path.join("socket");
        let mut listener = Listener::bind(&socket).unwrap();
        listener.owner = listener.owner.wrapping_add(1);
        let client = Client::connect(&socket).unwrap();
        let end = Instant::now() + Duration::from_secs(2);
        loop {
            match listener.accept() {
                Err(Error::Denied) => break,
                Ok(None) => assert!(Instant::now() < end),
                _ => panic!("unverified UID reached management"),
            }
        }
        drop(client);
        drop(listener);
        drop(directory);
        std::fs::remove_dir_all(path).unwrap();
    }
}

#[cfg(test)]
mod rate_tests {
    use super::*;

    #[test]
    fn request_limit_resets_only_when_the_window_expires() {
        let mut rate = RequestRate::new();
        for _ in 0..REQUESTS_PER_WINDOW {
            rate.admit().unwrap();
        }
        assert_eq!(rate.admit(), Err(Error::Exhausted));
        assert_eq!(rate.admit(), Err(Error::Exhausted));
        rate.window = Instant::now() - RATE_WINDOW;
        rate.admit().unwrap();
        assert_eq!(rate.count, 1);
    }
}
