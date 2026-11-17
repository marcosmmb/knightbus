//! Graceful shutdown keeps the app open until the daemon process has exited.
use knightbus_sdk::{
    CONTROL_TIMEOUT_MS,
    core::{self, Command, Reply},
};
use std::{path::Path, process::Command as Process, time::Duration};
static STOPPING: std::sync::Mutex<Option<(std::path::PathBuf, u32)>> = std::sync::Mutex::new(None);
const STOP_TIMEOUT: Duration = Duration::from_secs(30);
#[cfg(unix)]
const STOP_POLL: Duration = Duration::from_millis(200);

pub(crate) fn stop(endpoint: &Path) -> Result<(), String> {
    let mut stopping = STOPPING
        .lock()
        .map_err(|_| "Service shutdown lock failed.")?;
    if let Some((previous, pid)) = stopping.as_ref() {
        if previous != endpoint {
            return Err("Wait for the previous service to finish stopping.".into());
        }
        wait_stopped(*pid)?;
        *stopping = None;
        return Ok(());
    }
    let mut client = knightbus_sdk::connect_local(endpoint).map_err(super::failure)?;
    let Reply::Host(core::host::Reply::Status(status)) = client
        .request(
            Command::Host(core::host::Request::Status),
            CONTROL_TIMEOUT_MS,
        )
        .map_err(super::failure)?
    else {
        return Err(
            "The service does not support full shutdown. Update the app and service.".into(),
        );
    };
    if status.process_id == 0 || status.process_id == std::process::id() {
        return Err("Update the service before using Quit KnightBus.".into());
    }
    client
        .request(Command::Shutdown, CONTROL_TIMEOUT_MS)
        .map_err(super::failure)?;
    *stopping = Some((endpoint.to_owned(), status.process_id));
    drop(client);
    wait_stopped(status.process_id)?;
    *stopping = None;
    Ok(())
}
fn wait_stopped(pid: u32) -> Result<(), String> {
    #[cfg(unix)]
    {
        let deadline = std::time::Instant::now() + STOP_TIMEOUT;
        loop {
            // Read process state without interrupting native cleanup.
            let result = Process::new("/bin/ps")
                .args(["-p", &pid.to_string(), "-o", "stat="])
                .output()
                .map_err(|e| e.to_string())?;
            let state = String::from_utf8_lossy(&result.stdout);
            if !result.status.success() || state.trim_start().starts_with('Z') {
                return Ok(());
            }
            if std::time::Instant::now() >= deadline {
                break;
            }
            std::thread::sleep(STOP_POLL);
        }
    }
    #[cfg(windows)]
    {
        wait_windows_process(pid, STOP_TIMEOUT)
    }
    #[cfg(unix)]
    Err("The service is still releasing USB devices. KnightBus stayed open; wait for cleanup, then quit again.".into())
}
#[cfg(windows)]
fn wait_windows_process(pid: u32, timeout: Duration) -> Result<(), String> {
    use std::os::windows::process::CommandExt;
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    const WAIT_TIMED_OUT: i32 = 2;
    const WAIT_FAILED: i32 = 3;
    // Get-Process/Wait-Process can leave PowerShell's exit status at failure when
    // the process has already exited. Use explicit results and a process handle.
    let script = format!(
        r#"
$ErrorActionPreference='Stop'
try {{ $process=[Diagnostics.Process]::GetProcessById({pid}) }}
catch [ArgumentException] {{ exit 0 }}
catch {{ [Console]::Error.WriteLine($_.Exception.Message); exit {WAIT_FAILED} }}
try {{
 if ($process.WaitForExit({timeout_ms})) {{ exit 0 }}
 exit {WAIT_TIMED_OUT}
}} catch {{ [Console]::Error.WriteLine($_.Exception.Message); exit {WAIT_FAILED} }}
finally {{ $process.Dispose() }}
"#,
        timeout_ms = timeout.as_millis()
    );
    let result = Process::new("powershell.exe")
        .args(["-NoProfile", "-NonInteractive", "-Command", &script])
        .creation_flags(CREATE_NO_WINDOW)
        .output()
        .map_err(|e| format!("Could not check whether the service stopped: {e}"))?;
    match result.status.code() {
        Some(0) => Ok(()),
        Some(WAIT_TIMED_OUT) => Err("The service is still releasing USB devices. KnightBus stayed open; wait for cleanup, then quit again.".into()),
        _ => Err(format!("Could not check whether the service stopped: {}", String::from_utf8_lossy(&result.stderr).trim())),
    }
}

#[cfg(all(test, windows))]
mod tests {
    use super::*;
    use std::os::windows::process::CommandExt;
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    const SHORT_WAIT: Duration = Duration::from_millis(100);
    const CHILD_SLEEP_SECONDS: u64 = 60;
    fn child(script: &str) -> std::process::Child {
        Process::new("powershell.exe")
            .args(["-NoProfile", "-NonInteractive", "-Command", script])
            .creation_flags(CREATE_NO_WINDOW)
            .spawn()
            .unwrap()
    }
    #[test]
    fn exited_service_and_repeated_quit_checks_succeed() {
        let mut service = child("exit 0");
        let pid = service.id();
        service.wait().unwrap();
        assert_eq!(wait_windows_process(pid, SHORT_WAIT), Ok(()));
        assert_eq!(wait_windows_process(pid, SHORT_WAIT), Ok(()));
    }
    #[test]
    fn waits_for_a_live_process_and_reports_a_real_timeout() {
        let mut exiting = child("exit 0");
        assert_eq!(wait_windows_process(exiting.id(), STOP_TIMEOUT), Ok(()));
        exiting.wait().unwrap();
        let mut running = child(&format!("Start-Sleep -Seconds {CHILD_SLEEP_SECONDS}"));
        let result = wait_windows_process(running.id(), SHORT_WAIT);
        // Only this test's child is terminated, never an actual service.
        running.kill().unwrap();
        running.wait().unwrap();
        assert!(result.unwrap_err().contains("still releasing USB devices"));
    }
}

#[tauri::command]
pub async fn quit_service(app: tauri::AppHandle, endpoint: String) -> Result<(), String> {
    if endpoint.len() > super::MAX_ENDPOINT_BYTES {
        return Err(super::failure(core::Error::Invalid));
    }
    tauri::async_runtime::spawn_blocking(move || stop(Path::new(&endpoint)))
        .await
        .map_err(|e| e.to_string())??;
    app.exit(0);
    Ok(())
}
