//! macOS authorizes a root-owned service; the UI remains unprivileged.
#[cfg(target_os = "macos")]
use knightbus_sdk::{
    CONTROL_TIMEOUT_MS, SYSTEM_ENDPOINT,
    core::{self, Command, Reply},
    macos::{LAUNCHER_FILE, LEGACY_LAUNCHD_LABEL, LEGACY_LAUNCHD_PLIST, LOG_FILE},
};
#[cfg(target_os = "macos")]
use std::{
    path::Path,
    process::{Child, Command as Process, Stdio},
    sync::Mutex,
    time::{Duration, Instant},
};
#[cfg(target_os = "macos")]
use tauri::Manager;
#[cfg(target_os = "macos")]
static START_LOCK: Mutex<()> = Mutex::new(());
#[cfg(target_os = "macos")]
const START_TIMEOUT: Duration = Duration::from_secs(15);
#[cfg(target_os = "macos")]
const START_POLL: Duration = Duration::from_millis(200);
#[cfg(target_os = "macos")]
fn shell_literal(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\\''"))
}

#[tauri::command]
pub async fn enable_capture(app: tauri::AppHandle, endpoint: String) -> Result<String, String> {
    #[cfg(target_os = "macos")]
    {
        let directory = app.path().app_data_dir().map_err(|e| e.to_string())?;
        let local = super::companion::endpoint(&directory);
        if Path::new(&endpoint) != local && endpoint != SYSTEM_ENDPOINT {
            return Err(
                "Select the installed or bundled local service before enabling USB sharing.".into(),
            );
        }
        tauri::async_runtime::spawn_blocking(move || prepare(directory, endpoint))
            .await
            .map_err(|e| e.to_string())?
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = (app, endpoint);
        Err("USB capture setup is only needed on macOS.".into())
    }
}

#[cfg(target_os = "macos")]
pub(crate) fn prepare(directory: std::path::PathBuf, endpoint: String) -> Result<String, String> {
    let _guard = START_LOCK
        .lock()
        .map_err(|_| "Service startup lock failed.")?;
    let mut client = knightbus_sdk::connect_local(Path::new(&endpoint)).map_err(super::failure)?;
    let Reply::Host(core::host::Reply::Status(status)) = client
        .request(
            Command::Host(core::host::Request::Status),
            CONTROL_TIMEOUT_MS,
        )
        .map_err(super::failure)?
    else {
        return Err("Update the service first.".into());
    };
    if status.capture_ready && status.storage_consent_ready {
        return Ok(endpoint);
    }
    // Refuse to migrate while any connection still owns a device.
    client.request(Command::Host(core::host::Request::Clear), CONTROL_TIMEOUT_MS).map_err(|_| "The USB sharing helper needs an update. Disconnect active devices, then try sharing again.".to_string())?;
    drop(client);
    super::shutdown::stop(Path::new(&endpoint))?;
    let daemon = std::env::current_exe()
        .map_err(|e| e.to_string())?
        .with_file_name("knightbusd");
    let uid = rustix::process::getuid().as_raw();
    let command = format!(
        "{} --install-desktop-service {uid} {} {}",
        shell_literal(&daemon.to_string_lossy()),
        shell_literal(&directory.join("identity").to_string_lossy()),
        shell_literal(&status.fingerprint)
    );
    // argv keeps shell text out of the AppleScript source. macOS owns the password prompt.
    let script =
        "on run argv\n do shell script (item 1 of argv) with administrator privileges\nend run";
    let result = Process::new("/usr/bin/osascript")
        .args(["-e", script, &command])
        .output()
        .map_err(|e| e.to_string())?;
    if !result.status.success() {
        return Err(format!(
            "USB sharing setup was not completed. {}",
            String::from_utf8_lossy(&result.stderr).trim()
        ));
    }
    let mut child = launch_installed()?;
    let deadline = Instant::now() + START_TIMEOUT;
    loop {
        if let Ok(mut client) = knightbus_sdk::connect_local(Path::new(SYSTEM_ENDPOINT))
            && let Ok(Reply::Host(core::host::Reply::Status(installed))) = client.request(
                Command::Host(core::host::Request::Status),
                CONTROL_TIMEOUT_MS,
            )
        {
            if installed.fingerprint != status.fingerprint
                || !installed.capture_ready
                || !installed.storage_consent_ready
            {
                reap(child);
                return Err(
                    "The installed service identity or capture permissions do not match.".into(),
                );
            }
            reap(child);
            return Ok(SYSTEM_ENDPOINT.into());
        }
        check_running(&mut child)?;
        if Instant::now() >= deadline {
            reap(child);
            return Err(format!(
                "The USB sharing service did not start. Check {LOG_FILE}."
            ));
        }
        std::thread::sleep(START_POLL);
    }
}

#[cfg(all(test, target_os = "macos"))]
mod tests {
    use super::*;
    #[test]
    fn administrator_command_quotes_apostrophes_and_shell_syntax() {
        assert_eq!(shell_literal("a'b$(touch nope)"), "'a'\\''b$(touch nope)'");
    }
}

#[cfg(target_os = "macos")]
fn launcher_ready() -> bool {
    Process::new(LAUNCHER_FILE)
        .arg("--check")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok_and(|status| status.success())
}

#[cfg(target_os = "macos")]
fn launch_installed() -> Result<Child, String> {
    if !launcher_ready() {
        return Err("The installed USB helper could not authorize this account. Run USB sharing setup again.".into());
    }
    // Launch directly from the GUI so macOS retains the requesting app's identity.
    Process::new(LAUNCHER_FILE)
        .env_clear()
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|e| format!("Could not start the USB helper: {e}"))
}

#[cfg(target_os = "macos")]
fn check_running(child: &mut Child) -> Result<(), String> {
    if let Some(status) = child.try_wait().map_err(|e| e.to_string())? {
        return Err(format!(
            "The USB helper exited ({status}). Check {LOG_FILE}."
        ));
    }
    Ok(())
}

#[cfg(target_os = "macos")]
fn reap(mut child: Child) {
    std::thread::spawn(move || {
        let _ = child.wait();
    });
}

#[cfg(target_os = "macos")]
pub(crate) fn start_installed() -> Result<serde_json::Value, String> {
    let _guard = START_LOCK
        .lock()
        .map_err(|_| "Service startup lock failed.")?;
    if knightbus_sdk::connect_local(Path::new(SYSTEM_ENDPOINT)).is_ok() {
        return Ok(serde_json::json!({"endpoint": SYSTEM_ENDPOINT, "started": false}));
    }
    let legacy = Path::new(LEGACY_LAUNCHD_PLIST).exists();
    if legacy || !launcher_ready() {
        let uid = rustix::process::getuid().as_raw();
        let command = if legacy {
            format!("/bin/launchctl kickstart {LEGACY_LAUNCHD_LABEL}")
        } else {
            let daemon = std::env::current_exe()
                .map_err(|e| e.to_string())?
                .with_file_name("knightbusd");
            format!(
                "{} --start-desktop-service {uid}",
                shell_literal(&daemon.to_string_lossy())
            )
        };
        let script =
            "on run argv\n do shell script (item 1 of argv) with administrator privileges\nend run";
        let result = Process::new("/usr/bin/osascript")
            .args(["-e", script, &command])
            .output()
            .map_err(|e| e.to_string())?;
        if !result.status.success() {
            return Err(String::from_utf8_lossy(&result.stderr).into_owned());
        }
    }
    let mut child = if legacy {
        None
    } else {
        Some(launch_installed()?)
    };
    let deadline = Instant::now() + START_TIMEOUT;
    loop {
        if knightbus_sdk::connect_local(Path::new(SYSTEM_ENDPOINT)).is_ok() {
            if let Some(child) = child {
                reap(child);
            }
            return Ok(serde_json::json!({"endpoint": SYSTEM_ENDPOINT, "started": true}));
        }
        if let Some(child) = child.as_mut() {
            check_running(child)?;
        }
        if Instant::now() >= deadline {
            if let Some(child) = child {
                reap(child);
            }
            return Err(format!(
                "The installed service did not start. Check {LOG_FILE}."
            ));
        }
        std::thread::sleep(START_POLL);
    }
}
