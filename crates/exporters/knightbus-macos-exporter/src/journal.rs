//! Opt-in crash-reproduction journal. Each BEGIN reaches persistent local storage
//! and an authenticated SSH-forwarded remote sink before its operation executes.
//! This deliberately changes timing and is not a performance measurement mode.
use crate::constants::lab;
use std::{
    fs::{File, OpenOptions},
    io::{self, BufRead, BufReader, Write},
    os::{
        fd::AsRawFd,
        unix::{fs::OpenOptionsExt, net::UnixStream},
    },
    path::Path,
    sync::{
        Mutex, OnceLock,
        atomic::{AtomicBool, Ordering},
    },
    time::{Instant, SystemTime, UNIX_EPOCH},
};

const F_FULLFSYNC: i32 = 51;
const TOKEN_HEX_LENGTH: usize = 64;
const MAX_RECORD_BYTES: usize = 16384;
const JOURNAL_FILE_MODE: u32 = 0o644;
const ACK_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(5);

static JOURNAL: OnceLock<Mutex<Journal>> = OnceLock::new();
static FAILED: AtomicBool = AtomicBool::new(false);
struct Journal {
    file: File,
    remote: BufReader<UnixStream>,
    sequence: u64,
    start: Instant,
    target: (u32, u16, u16),
}

unsafe extern "C" {
    fn fcntl(fd: i32, command: i32, ...) -> i32;
}
fn sync(file: &File) -> io::Result<()> {
    file.sync_all()?;
    // Apple's F_FULLFSYNC asks the storage device to flush its write cache.
    // A remote durable ACK remains independently required; neither is absolute
    // protection against every hardware/storage failure.
    if unsafe { fcntl(file.as_raw_fd(), F_FULLFSYNC) } == -1 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

/// Initialize optional recording once from a prepared trial directory.
/// Once requested, the mirror must authenticate and acknowledge readiness.
pub fn initialize(directory: &Path) -> io::Result<()> {
    if JOURNAL.get().is_some() {
        return Err(io::Error::other("journal already initialized"));
    }
    let token = std::fs::read_to_string(directory.join("token"))?;
    let target = std::fs::read_to_string(directory.join("target"))?;
    let parts: Vec<_> = target.split_whitespace().collect();
    if parts.len() != 3 {
        return Err(io::Error::other("invalid lab target"));
    }
    let parse =
        |s: &str| u32::from_str_radix(s, 16).map_err(|_| io::Error::other("invalid lab target"));
    let target = (parse(parts[0])?, parse(parts[1])?, parse(parts[2])?);
    if target.1 != u32::from(lab::VENDOR) || target.2 != u32::from(lab::PRODUCT) {
        return Err(io::Error::other(
            "lab capture is scoped to the selected Logitech joystick",
        ));
    }
    let target = (target.0, target.1 as u16, target.2 as u16);
    if token.trim().len() != TOKEN_HEX_LENGTH
        || !token.trim().bytes().all(|b| b.is_ascii_hexdigit())
    {
        return Err(io::Error::other("invalid prepared journal token"));
    }
    let socket = std::fs::read_to_string(directory.join("socket-path"))?;
    let mut stream = UnixStream::connect(socket.trim())?;
    stream.set_read_timeout(Some(ACK_TIMEOUT))?;
    stream.set_write_timeout(Some(ACK_TIMEOUT))?;
    writeln!(stream, "{}", token.trim())?;
    let mut remote = BufReader::new(stream);
    let mut ready = String::new();
    remote.read_line(&mut ready)?;
    if ready != "READY\n" {
        return Err(io::Error::other("remote journal refused authentication"));
    }
    let file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(JOURNAL_FILE_MODE)
        .open(directory.join(format!("native-{}.jsonl", std::process::id())))?;
    sync(&file)?;
    File::open(directory)?.sync_all()?;
    JOURNAL
        .set(Mutex::new(Journal {
            file,
            remote,
            sequence: 0,
            start: Instant::now(),
            target,
        }))
        .map_err(|_| io::Error::other("journal already initialized"))?;
    record("journal.ready", "", "local=F_FULLFSYNC remote=fsync")?;
    Ok(())
}

/// Whether this process has an operational crash recorder; default is false.
pub fn enabled() -> bool {
    JOURNAL.get().is_some() && !FAILED.load(Ordering::Acquire)
}
/// Ordinary explicit capture needs no recorder. Once initialized, recording
/// stays mandatory and restricts capture to the selected target, even on failure.
pub fn capture_allowed(location: u32, vendor: u16, product: u16) -> bool {
    JOURNAL.get().is_none()
        || (enabled()
            && JOURNAL
                .get()
                .and_then(|j| j.lock().ok())
                .is_some_and(|j| j.target == (location, vendor, product)))
}
fn quoted(value: &str) -> String {
    let mut out = String::from("\"");
    for c in value.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            c if c < ' ' => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}
fn record(event: &str, operation: &str, detail: &str) -> io::Result<u64> {
    let Some(journal) = JOURNAL.get() else {
        return Ok(0);
    };
    let result = (|| {
        let mut j = journal
            .lock()
            .map_err(|_| io::Error::other("journal lock poisoned"))?;
        j.sequence += 1;
        let seq = j.sequence;
        let thread = std::thread::current();
        let line = format!(
            "{{\"seq\":{seq},\"pid\":{},\"wall_ns\":{},\"elapsed_us\":{},\"thread\":{},\"event\":{},\"operation\":{},\"detail\":{}}}\n",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos(),
            j.start.elapsed().as_micros(),
            quoted(&format!(
                "{:?}:{}",
                thread.id(),
                thread.name().unwrap_or("unnamed")
            )),
            quoted(event),
            quoted(operation),
            quoted(detail)
        );
        if line.len() > MAX_RECORD_BYTES {
            return Err(io::Error::other("journal record too large"));
        }
        j.file.write_all(line.as_bytes())?;
        sync(&j.file)?;
        // Local record is durable even if the remote connection breaks here.
        j.remote.get_mut().write_all(line.as_bytes())?;
        let mut ack = String::new();
        j.remote.read_line(&mut ack)?;
        if ack != format!("OK {seq}\n") {
            return Err(io::Error::other("missing durable remote ACK"));
        }
        Ok(seq)
    })();
    if result.is_err() {
        FAILED.store(true, Ordering::Release);
    }
    result
}

/// Mark an operation before entering the OS. Failure prevents new work.
pub fn begin(operation: &'static str, detail: &str) -> knightbus_core::Result<Span> {
    if FAILED.load(Ordering::Acquire) {
        return Err(knightbus_core::Error::Unavailable);
    }
    let sequence =
        record("begin", operation, detail).map_err(|_| knightbus_core::Error::Unavailable)?;
    Ok(Span {
        operation,
        sequence,
    })
}
/// Correlates a flushed pre-operation marker with its returned result.
pub struct Span {
    operation: &'static str,
    sequence: u64,
}
impl Span {
    /// Record a result without unwinding across a callback or skipping cleanup.
    pub fn finish(self, detail: &str) {
        note(
            "end",
            self.operation,
            &format!("begin={} {detail}", self.sequence),
        );
    }
}
/// Best-effort cleanup/callback evidence. Recording failure disables subsequent
/// capture/submission, but must not prevent already-owned resources retiring.
pub fn note(event: &str, operation: &str, detail: &str) {
    if let Err(error) = record(event, operation, detail) {
        eprintln!("journal_error={error} operation={operation}");
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn json_strings_escape_control_bytes_and_preserve_unicode() {
        assert_eq!(super::quoted("a\n\"\\\té"), "\"a\\u000a\\\"\\\\\\u0009é\"");
    }
}
