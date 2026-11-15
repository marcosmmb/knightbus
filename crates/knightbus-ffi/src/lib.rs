//! Panic-contained C ABI over the shared SDK. All allocated objects have an
//! explicit release function. See include/knightbus.h for ownership and threading.
use knightbus_sdk::{
    Client, Embedded, Runtime,
    config::{Configuration, MAX_CONFIG_BYTES},
    core::*,
    ipc,
};
use std::{
    panic::{AssertUnwindSafe, catch_unwind},
    path::Path,
};

/// C ABI version, independent of the USB/IP version.
pub const ABI_VERSION: u32 = 1;
// Bound foreign path inputs before constructing an owned OS path.
const MAX_ENDPOINT_PATH_BYTES: usize = 4096;
/// Opaque, single-thread-at-a-time control connection.
pub struct KbHandle {
    mode: Mode,
    next: u64,
    poisoned: bool,
}
enum Mode {
    Embedded { control: Embedded, runtime: Runtime },
    Service(Client),
}
/// Opaque immutable response allocation; valid until kb_buffer_free.
pub struct KbBuffer(Vec<u8>);
fn guard(run: impl FnOnce() -> Result<()>) -> u16 {
    catch_unwind(AssertUnwindSafe(run))
        .unwrap_or(Err(Error::Internal))
        .map_or_else(|e| e as u16, |()| 0)
}
unsafe fn bytes<'a>(data: *const u8, len: usize, max: usize) -> Result<&'a [u8]> {
    if data.is_null() || len == 0 || len > max {
        return Err(Error::Invalid);
    }
    // SAFETY: the caller supplies a readable allocation of len bytes.
    Ok(unsafe { std::slice::from_raw_parts(data, len) })
}
/// Return the ABI version without creating any host or connection.
#[unsafe(no_mangle)]
pub extern "C" fn kb_abi_version() -> u32 {
    ABI_VERSION
}
/// Return the management schema version used by request/response frames.
#[unsafe(no_mangle)]
pub extern "C" fn kb_control_version() -> u16 {
    CONTROL_VERSION
}
/// Start an embedded native host using bounded UTF-8 configuration bytes.
/// # Safety
/// `config` must be readable for `len` bytes; `out` must be writable and aligned.
/// Serialize calls on the resulting handle and release it exactly once.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn kb_embedded_open(
    version: u32,
    config: *const u8,
    len: usize,
    out: *mut *mut KbHandle,
) -> u16 {
    guard(|| {
        if out.is_null() {
            return Err(Error::Invalid);
        }
        unsafe {
            *out = std::ptr::null_mut();
        }
        if version != ABI_VERSION {
            return Err(Error::Version);
        }
        let text = std::str::from_utf8(unsafe { bytes(config, len, MAX_CONFIG_BYTES)? })
            .map_err(|_| Error::Invalid)?;
        let runtime = Runtime::start(&Configuration::parse(text)?)?;
        let control = Embedded::connect(&runtime)?;
        let handle = KbHandle {
            mode: Mode::Embedded { control, runtime },
            next: 0,
            poisoned: false,
        };
        unsafe {
            *out = Box::into_raw(Box::new(handle));
        }
        Ok(())
    })
}
/// Open an OS-authenticated Service connection without owning the daemon.
/// # Safety
/// `path` must be readable for `len` UTF-8 bytes and `out` writable and aligned.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn kb_service_open(
    version: u32,
    path: *const u8,
    len: usize,
    out: *mut *mut KbHandle,
) -> u16 {
    guard(|| {
        if out.is_null() {
            return Err(Error::Invalid);
        }
        unsafe {
            *out = std::ptr::null_mut();
        }
        if version != ABI_VERSION {
            return Err(Error::Version);
        }
        let path = std::str::from_utf8(unsafe { bytes(path, len, MAX_ENDPOINT_PATH_BYTES)? })
            .map_err(|_| Error::Invalid)?;
        let handle = KbHandle {
            mode: Mode::Service(Client::connect(Path::new(path))?),
            next: 0,
            poisoned: false,
        };
        unsafe {
            *out = Box::into_raw(Box::new(handle));
        }
        Ok(())
    })
}
/// Execute one bounded management frame. IDs strictly increase on each handle.
/// `timeout_ms` replaces the frame's deadline with a negotiated host-clock deadline.
/// A zero return means a response exists; operation errors are encoded in that response.
/// # Safety
/// Handle must be live and exclusively borrowed; frame readable for len bytes;
/// out writable. Response must be released once with kb_buffer_free.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn kb_call(
    handle: *mut KbHandle,
    frame: *const u8,
    len: usize,
    timeout_ms: u64,
    out: *mut *mut KbBuffer,
) -> u16 {
    if out.is_null() {
        return Error::Invalid as u16;
    }
    unsafe {
        *out = std::ptr::null_mut();
    }
    if handle.is_null() {
        return Error::Invalid as u16;
    }
    let handle = unsafe { &mut *handle };
    let result = catch_unwind(AssertUnwindSafe(|| -> Result<()> {
        if handle.poisoned {
            return Err(Error::State);
        }
        let request = ipc::decode_request(unsafe { bytes(frame, len, ipc::MAX_FRAME)? })?;
        if request.version != CONTROL_VERSION {
            return Err(Error::Version);
        }
        if request.id <= handle.next {
            return Err(Error::Invalid);
        }
        handle.next = request.id;
        let result = match &mut handle.mode {
            Mode::Embedded { control, .. } => control.request(request.command, timeout_ms),
            Mode::Service(control) => control.request(request.command, timeout_ms),
        };
        let response = Response {
            id: request.id,
            result,
        };
        let bytes = ipc::encode_response(&response).or_else(|e| {
            ipc::encode_response(&Response {
                id: request.id,
                result: Err(e),
            })
        })?;
        unsafe {
            *out = Box::into_raw(Box::new(KbBuffer(bytes)));
        }
        Ok(())
    }));
    match result {
        Ok(Ok(())) => 0,
        Ok(Err(e)) => e as u16,
        Err(_) => {
            handle.poisoned = true;
            Error::Internal as u16
        }
    }
}
/// Borrow immutable response bytes; NULL is returned for a NULL buffer.
/// # Safety
/// The buffer must be live. Returned bytes expire at kb_buffer_free.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn kb_buffer_data(buffer: *const KbBuffer) -> *const u8 {
    if buffer.is_null() {
        std::ptr::null()
    } else {
        unsafe { (*buffer).0.as_ptr() }
    }
}
/// Return the response length, or zero for NULL.
/// # Safety
/// A non-NULL buffer must be live.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn kb_buffer_len(buffer: *const KbBuffer) -> usize {
    if buffer.is_null() {
        0
    } else {
        unsafe { (*buffer).0.len() }
    }
}
/// Free a response independently of its originating handle. NULL is a no-op.
/// # Safety
/// A non-NULL buffer must originate here and be freed exactly once.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn kb_buffer_free(buffer: *mut KbBuffer) {
    if !buffer.is_null() {
        drop(unsafe { Box::from_raw(buffer) });
    }
}
/// Close the connection. Embedded close requests shutdown and waits for native
/// drain; Service close retires subscriptions and leaves forwarding running.
/// # Safety
/// A non-NULL handle must originate here, be exclusively owned and closed once.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn kb_close(handle: *mut KbHandle) -> u16 {
    guard(|| {
        if handle.is_null() {
            return Ok(());
        }
        let handle = unsafe { Box::from_raw(handle) };
        if let Mode::Embedded { runtime, .. } = &handle.mode {
            runtime.shutdown();
            runtime.join()?;
        }
        drop(handle);
        Ok(())
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{Duration, Instant};
    #[test]
    fn version_null_and_panic_boundaries_are_explicit() {
        let mut out = std::ptr::dangling_mut();
        assert_eq!(
            unsafe { kb_embedded_open(99, std::ptr::null(), 0, &mut out) },
            Error::Version as u16
        );
        assert!(out.is_null());
        assert_eq!(
            unsafe { kb_service_open(ABI_VERSION, std::ptr::null(), 10, &mut out) },
            Error::Invalid as u16
        );
        assert_eq!(
            guard(|| panic!("contained test panic")),
            Error::Internal as u16
        );
        assert_eq!(unsafe { kb_close(std::ptr::null_mut()) }, 0);
        assert_eq!(unsafe { kb_buffer_len(std::ptr::null()) }, 0);
    }
    #[test]
    fn ffi_service_negotiates_owns_results_and_rejects_replayed_ids() {
        let root = std::env::temp_dir().join(format!("kb-ffi-{}", std::process::id()));
        let directory = ipc::private::Directory::open(&root).unwrap();
        #[cfg(unix)]
        let endpoint = root.join("socket");
        #[cfg(windows)]
        let endpoint = std::path::PathBuf::from(format!(r"\\.\pipe\kb-ffi-{}", std::process::id()));
        let listener = ipc::local::Listener::bind(&endpoint).unwrap();
        let worker =
            std::thread::spawn(move || {
                let end = Instant::now() + Duration::from_secs(5);
                let connection = loop {
                    if let Some(connection) = listener.accept().unwrap() {
                        break connection;
                    }
                    assert!(Instant::now() < end);
                };
                let _ = connection.serve(&ipc::local::Cancellation::default(), |_, command, _| {
                    match command {
                        Command::Info => Ok(Reply::Info {
                            version: CONTROL_VERSION,
                            epoch: 42,
                            now: 100,
                        }),
                        Command::List => Ok(Reply::Devices(vec![])),
                        _ => Err(Error::Denied),
                    }
                });
            });
        let endpoint = endpoint.to_str().unwrap().as_bytes();
        let mut handle = std::ptr::null_mut();
        assert_eq!(
            unsafe { kb_service_open(ABI_VERSION, endpoint.as_ptr(), endpoint.len(), &mut handle) },
            0
        );
        let mut output = std::ptr::null_mut();
        let request = ipc::encode_request(&Request {
            version: CONTROL_VERSION,
            id: 7,
            deadline: 0,
            command: Command::List,
        })
        .unwrap();
        assert_eq!(
            unsafe { kb_call(handle, request.as_ptr(), request.len(), 5000, &mut output) },
            0
        );
        let response =
            unsafe { std::slice::from_raw_parts(kb_buffer_data(output), kb_buffer_len(output)) };
        assert_eq!(
            ipc::decode_response(response),
            Ok(Response {
                id: 7,
                result: Ok(Reply::Devices(vec![]))
            })
        );
        let owned = output;
        assert_eq!(
            unsafe { kb_call(handle, request.as_ptr(), request.len(), 5000, &mut output) },
            Error::Invalid as u16
        );
        assert!(output.is_null());
        assert_eq!(unsafe { kb_close(handle) }, 0);
        // Response storage survives closing its connection.
        assert!(unsafe { kb_buffer_len(owned) } > 0);
        unsafe {
            kb_buffer_free(owned);
        }
        worker.join().unwrap();
        drop(directory);
        std::fs::remove_dir_all(root).unwrap();
    }
}
