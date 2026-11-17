//! macOS authorizes a root-owned service; the UI remains unprivileged.
#[cfg(target_os = "macos")]
use knightbus_sdk::{
    CONTROL_TIMEOUT_MS, SYSTEM_ENDPOINT,
    core::{self, Command, Reply},
};
#[cfg(target_os = "macos")]
use std::{
    path::Path,
    process::Command as Process,
    time::{Duration, Instant},
};
#[cfg(target_os = "macos")]
use tauri::Manager;
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
        tauri::async_runtime::spawn_blocking(move || {
            let mut client = knightbus_sdk::connect_local(Path::new(&endpoint)).map_err(super::failure)?;
            let Reply::Host(core::host::Reply::Status(status)) = client.request(Command::Host(core::host::Request::Status), CONTROL_TIMEOUT_MS).map_err(super::failure)? else { return Err("Update the service first.".into()); };
            if status.capture_ready { return Ok(endpoint); }
            // Refuse to migrate while any connection still owns a device.
            client.request(Command::Host(core::host::Request::Clear), CONTROL_TIMEOUT_MS).map_err(super::failure)?;
            drop(client);
            super::shutdown::stop(Path::new(&endpoint))?;
            let daemon = std::env::current_exe().map_err(|e| e.to_string())?.with_file_name("knightbusd");
            let uid = rustix::process::getuid().as_raw();
            let command = format!("{} --install-desktop-service {uid} {} {}", shell_literal(&daemon.to_string_lossy()), shell_literal(&directory.join("identity").to_string_lossy()), shell_literal(&status.fingerprint));
            // argv keeps shell text out of the AppleScript source. macOS owns the password prompt.
            let script = "on run argv\n do shell script (item 1 of argv) with administrator privileges\nend run";
            let result = Process::new("/usr/bin/osascript").args(["-e", script, &command]).output().map_err(|e| e.to_string())?;
            if !result.status.success() { return Err(format!("USB sharing setup was not completed. {}", String::from_utf8_lossy(&result.stderr).trim())); }
            let deadline = Instant::now() + START_TIMEOUT;
            loop {
                if let Ok(mut client) = knightbus_sdk::connect_local(Path::new(SYSTEM_ENDPOINT))
                    && let Ok(Reply::Host(core::host::Reply::Status(installed))) = client.request(Command::Host(core::host::Request::Status), CONTROL_TIMEOUT_MS)
                {
                    if installed.fingerprint != status.fingerprint || !installed.capture_ready { return Err("The installed service identity or capture permissions do not match.".into()); }
                    return Ok(SYSTEM_ENDPOINT.into());
                }
                if Instant::now() >= deadline { return Err("The USB sharing service did not start. Check /var/db/knightbus/service.log.".into()); }
                std::thread::sleep(START_POLL);
            }
        }).await.map_err(|e| e.to_string())?
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = (app, endpoint);
        Err("USB capture setup is only needed on macOS.".into())
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
pub(crate) fn start_installed() -> Result<serde_json::Value, String> {
    if knightbus_sdk::connect_local(Path::new(SYSTEM_ENDPOINT)).is_ok() {
        return Ok(serde_json::json!({"endpoint": SYSTEM_ENDPOINT, "started": false}));
    }
    let script = "do shell script \"/bin/launchctl kickstart system/io.knightbus.daemon\" with administrator privileges";
    let result = Process::new("/usr/bin/osascript")
        .args(["-e", script])
        .output()
        .map_err(|e| e.to_string())?;
    if !result.status.success() {
        return Err(String::from_utf8_lossy(&result.stderr).into_owned());
    }
    let deadline = Instant::now() + START_TIMEOUT;
    loop {
        if knightbus_sdk::connect_local(Path::new(SYSTEM_ENDPOINT)).is_ok() {
            return Ok(serde_json::json!({"endpoint": SYSTEM_ENDPOINT, "started": true}));
        }
        if Instant::now() >= deadline {
            return Err(
                "The installed service did not start. Check /var/db/knightbus/service.log.".into(),
            );
        }
        std::thread::sleep(START_POLL);
    }
}
