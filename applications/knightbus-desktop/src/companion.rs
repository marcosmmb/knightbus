//! Explicit, unprivileged startup of the bundled daemon. Never starts USB sharing.
use knightbus_sdk::{CONTROL_POLL_INTERVAL, core::Error, ipc::private::Directory};
use serde_json::{Value, json};
use std::{
    fs::{File, OpenOptions},
    io::Read,
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    sync::{Arc, Mutex},
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};
use tauri::Manager;

const STARTUP_TIMEOUT: Duration = Duration::from_secs(10);
const MAX_STARTUP_LOG_BYTES: u64 = 8192;
#[cfg(windows)]
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

#[derive(Default)]
pub struct Launcher(Arc<Mutex<Option<Child>>>);

pub fn endpoint(directory: &Path) -> PathBuf {
    #[cfg(not(windows))]
    {
        directory.join("control")
    }
    #[cfg(windows)]
    {
        use sha2::{Digest, Sha256};
        // A stable per-account name; OS peer credentials still provide authentication.
        let name = Sha256::digest(directory.as_os_str().to_string_lossy().as_bytes());
        PathBuf::from(format!(r"\\.\pipe\knightbus-desktop-{name:x}"))
    }
}
fn executable() -> Result<PathBuf, String> {
    let current = std::env::current_exe().map_err(|e| e.to_string())?;
    let name = if cfg!(windows) {
        "knightbusd.exe"
    } else {
        "knightbusd"
    };
    Ok(current
        .parent()
        .ok_or("Cannot locate the app directory.")?
        .join(name))
}
fn startup_error(log: &Path, message: &str) -> String {
    let mut bytes = Vec::new();
    if let Ok(file) = File::open(log) {
        let _ = file.take(MAX_STARTUP_LOG_BYTES).read_to_end(&mut bytes);
    }
    format!(
        "{message}\n{}\nStartup log: {}",
        String::from_utf8_lossy(&bytes),
        log.display()
    )
}
fn start(directory: &Path, daemon: &Path, child: &mut Option<Child>) -> Result<Value, String> {
    let endpoint = endpoint(directory);
    match knightbus_sdk::connect_local(&endpoint) {
        Ok(_) => return Ok(json!({"endpoint":endpoint,"started":false})),
        Err(Error::Unavailable) => {}
        Err(error) => {
            return Err(format!(
                "The existing local service could not be verified: {error}. Repair its permissions or install matching app and service versions."
            ));
        }
    }
    if let Some(process) = child {
        if process.try_wait().map_err(|e| e.to_string())?.is_none() {
            return Err("The service process is still starting or is not responding. Retry connection before starting another process.".into());
        }
        *child = None;
    }
    if !daemon.is_file() {
        return Err(format!(
            "The companion is missing: {}. Reinstall the complete KnightBus package, then choose Start local service.",
            daemon.display()
        ));
    }
    let parent = directory
        .parent()
        .ok_or("Cannot locate the app data directory.")?;
    std::fs::create_dir_all(parent)
        .map_err(|e| format!("Cannot create the app data directory: {e}"))?;
    Directory::open(directory).map_err(|e| {
        format!(
            "Cannot access private service data: {e}. Check this account’s permissions for {}.",
            directory.display()
        )
    })?;
    // Never replace an endpoint that might belong to another running process.
    #[cfg(not(windows))]
    if endpoint.symlink_metadata().is_ok() {
        return Err(format!(
            "An existing endpoint at {} is not responding. Stop its owner through your service manager before repairing the endpoint. KnightBus will not replace it automatically.",
            endpoint.display()
        ));
    }
    let stamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|e| e.to_string())?
        .as_nanos();
    let log = directory.join(format!("startup-{}-{stamp}.log", std::process::id()));
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(knightbus_sdk::ipc::private::FILE_MODE);
    }
    let output = options
        .open(&log)
        .map_err(|e| format!("Cannot create startup log: {e}"))?;
    let mut command = Command::new(daemon);
    command
        .arg("--desktop")
        .arg(directory.join("identity"))
        .arg(&endpoint)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(output);
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(CREATE_NO_WINDOW);
    }
    *child = Some(command.spawn().map_err(|e| {
        format!("Cannot start the bundled service: {e}. Reinstall the complete package.")
    })?);
    let deadline = Instant::now() + STARTUP_TIMEOUT;
    loop {
        if child
            .as_mut()
            .ok_or("Service process was lost.")?
            .try_wait()
            .map_err(|e| e.to_string())?
            .is_some()
        {
            *child = None;
            return Err(startup_error(&log, "The service exited during startup."));
        }
        match knightbus_sdk::connect_local(&endpoint) {
            Ok(_) => return Ok(json!({"endpoint":endpoint,"started":true})),
            Err(Error::Unavailable) => {}
            Err(error) => {
                return Err(startup_error(
                    &log,
                    &format!(
                        "The service could not be verified: {error}. Install matching app and service versions."
                    ),
                ));
            }
        }
        if Instant::now() >= deadline {
            return Err(startup_error(
                &log,
                "The service has not responded yet. Retry connection; no second process was started.",
            ));
        }
        std::thread::sleep(CONTROL_POLL_INTERVAL);
    }
}
#[tauri::command]
pub async fn start_service(
    app: tauri::AppHandle,
    launcher: tauri::State<'_, Launcher>,
) -> Result<Value, String> {
    let directory = app.path().app_data_dir().map_err(|e| e.to_string())?;
    let daemon = executable()?;
    let launcher = launcher.0.clone();
    tauri::async_runtime::spawn_blocking(move || {
        let mut child = launcher
            .lock()
            .map_err(|_| "Service startup lock failed.")?;
        #[cfg(target_os = "macos")]
        if Path::new(knightbus_sdk::macos::LEGACY_LAUNCHD_PLIST).exists()
            || Path::new(knightbus_sdk::macos::LAUNCHER_FILE).exists()
        {
            return super::capture::start_installed();
        }
        #[cfg(windows)]
        if std::env::var_os("ProgramFiles")
            .is_some_and(|root| Path::new(&root).join("KnightBus/knightbusd.exe").is_file())
        {
            return start_installed_windows();
        }
        start(&directory, &daemon, &mut child)
    })
    .await
    .map_err(|e| e.to_string())?
}

#[cfg(windows)]
fn start_installed_windows() -> Result<Value, String> {
    use std::os::windows::process::CommandExt;
    const ENDPOINT: &str = r"\\.\pipe\knightbus-control";
    if knightbus_sdk::connect_local(Path::new(ENDPOINT)).is_ok() {
        return Ok(json!({"endpoint": ENDPOINT, "started": false}));
    }
    // Restart the existing account's installed task, preserving its identity and
    // driver access. Never substitute a second unprivileged companion.
    const SCRIPT: &str = r#"
$ErrorActionPreference='Stop'
$task=Get-ScheduledTask -TaskName 'KnightBus daemon'
$expected=Join-Path ([Environment]::GetFolderPath('ProgramFiles')) 'KnightBus\knightbusd.exe'
$identity=[Security.Principal.WindowsIdentity]::GetCurrent()
$owner=Get-KnightBusTaskOwnerSid -TaskName $task.TaskName -TaskPath $task.TaskPath
if ($owner -ne $identity.User.Value -or @($task.Actions).Count -ne 1 -or $task.Actions[0].Execute -ne $expected) { throw 'Repair the installed KnightBus service for this account.' }
Start-ScheduledTask -TaskName 'KnightBus daemon'
"#;
    let output = Command::new("powershell.exe")
        .args(["-NoProfile", "-NonInteractive", "-Command"])
        .arg(format!(
            "{}\n{SCRIPT}",
            include_str!("../../../tools/install/windows-installation.ps1")
        ))
        .creation_flags(CREATE_NO_WINDOW)
        .output()
        .map_err(|e| e.to_string())?;
    if !output.status.success() {
        return Err(format!(
            "Could not restart the installed service. Rerun KnightBus setup with administrator approval. {}",
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }
    let deadline = Instant::now() + STARTUP_TIMEOUT;
    loop {
        if knightbus_sdk::connect_local(Path::new(ENDPOINT)).is_ok() {
            return Ok(json!({"endpoint": ENDPOINT, "started": true}));
        }
        if Instant::now() >= deadline {
            return Err(
                "The installed service did not respond. Repair it with KnightBus setup.".into(),
            );
        }
        std::thread::sleep(CONTROL_POLL_INTERVAL);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use knightbus_sdk::{
        core::{Command as Control, Reply},
        ipc::local::Listener,
        service::Service,
    };
    use std::{
        sync::atomic::{AtomicU64, Ordering},
        thread,
    };
    static NEXT_FIXTURE: AtomicU64 = AtomicU64::new(0);

    fn directory() -> PathBuf {
        let stamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let directory = std::env::temp_dir().join(format!(
            "kb-s-{}-{}-{stamp}",
            std::process::id(),
            NEXT_FIXTURE.fetch_add(1, Ordering::Relaxed)
        ));
        Directory::open(&directory).unwrap();
        directory
    }
    #[test]
    fn missing_companion_never_initializes_identity() {
        let directory = directory();
        let failure = start(&directory, &directory.join("missing"), &mut None).unwrap_err();
        assert!(failure.contains("companion is missing"));
        assert!(!directory.join("identity").exists());
        std::fs::remove_dir_all(directory).unwrap();
    }
    #[cfg(unix)]
    #[test]
    fn exited_child_reports_failure_and_leaves_identity_absent() {
        let directory = directory();
        let mut child = None;
        let error = start(&directory, Path::new("/usr/bin/false"), &mut child).unwrap_err();
        assert!(error.contains("exited during startup"));
        assert!(error.contains("Startup log:"));
        assert!(child.is_none());
        assert!(!directory.join("identity").exists());
        std::fs::remove_dir_all(directory).unwrap();
    }
    #[cfg(unix)]
    #[test]
    fn existing_unresponsive_endpoint_is_never_replaced() {
        let directory = directory();
        let endpoint = endpoint(&directory);
        std::fs::write(&endpoint, b"existing").unwrap();
        let error = start(&directory, Path::new("/usr/bin/true"), &mut None).unwrap_err();
        assert!(error.contains("will not replace"));
        assert_eq!(std::fs::read(endpoint).unwrap(), b"existing");
        assert!(!directory.join("identity").exists());
        std::fs::remove_dir_all(directory).unwrap();
    }
    #[test]
    fn existing_authenticated_service_is_reused_without_a_companion() {
        let directory = directory();
        let listener = Listener::bind(&endpoint(&directory))
            .unwrap_or_else(|error| panic!("Bind {}: {error:?}", endpoint(&directory).display()));
        let service = Arc::new(Service::open(&directory.join("identity")).unwrap());
        let running = service.clone();
        let worker = thread::spawn(move || running.serve(&listener));
        let reply = start(&directory, &directory.join("missing"), &mut None).unwrap();
        assert_eq!(reply["started"], false);
        let mut client = knightbus_sdk::connect_local(&endpoint(&directory)).unwrap();
        let reply = client
            .request(Control::Sessions, knightbus_sdk::CONTROL_TIMEOUT_MS)
            .unwrap();
        assert!(matches!(reply, Reply::Sessions(ids) if ids.is_empty()));
        drop(client);
        service.shutdown();
        worker.join().unwrap().unwrap();
        std::fs::remove_dir_all(directory).unwrap();
    }
}
