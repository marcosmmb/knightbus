#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]
mod capture;
mod companion;
mod computer;
mod shutdown;

use knightbus_sdk::{
    ATTACH_TIMEOUT_MS, CONTROL_POLL_INTERVAL, CONTROL_TIMEOUT_MS, Client,
    core::{self, Command, Reply},
};
use serde::Deserialize;
use serde_json::{Value, json};
use std::{
    path::Path,
    sync::{Arc, Mutex},
    time::Instant,
};
use tauri::{
    Emitter, Manager,
    menu::{Menu, MenuItem},
    tray::TrayIconBuilder,
};

const MAX_ARGUMENTS: usize = 5;
const MAX_ARGUMENT_BYTES: usize = 256;
const MAX_ENDPOINT_BYTES: usize = 4096;
#[cfg(target_os = "macos")]
const SYSTEM_ENDPOINT: &str = knightbus_sdk::SYSTEM_ENDPOINT;

#[derive(Deserialize)]
#[serde(
    tag = "kind",
    content = "value",
    rename_all = "snake_case",
    deny_unknown_fields
)]
enum Request {
    Host(core::host::Request),
    Control(Vec<String>),
}
#[derive(Default)]
struct Link {
    endpoint: String,
    client: Option<Client>,
    last: Option<Instant>,
}
#[derive(Default)]
struct State(Arc<Mutex<Link>>);
fn failure(error: core::Error) -> String {
    format!("{error}: {}", knightbus_sdk::guidance(error))
}

#[tauri::command]
async fn request(
    app: tauri::AppHandle,
    endpoint: String,
    request: Request,
    state: tauri::State<'_, State>,
) -> Result<Value, String> {
    if endpoint.len() > MAX_ENDPOINT_BYTES {
        return Err(failure(core::Error::Invalid));
    }
    #[cfg(not(target_os = "macos"))]
    let _ = app;
    let state = state.0.clone();
    tauri::async_runtime::spawn_blocking(move || {
        let command = match request {
            Request::Host(request) => Command::Host(request),
            Request::Control(args) => {
                if args.len() > MAX_ARGUMENTS || args.iter().any(|s| s.len() > MAX_ARGUMENT_BYTES) {
                    return Err(failure(core::Error::Invalid));
                }
                knightbus_sdk::cli::command(&args).map_err(failure)?
            }
        };
        let timeout = if matches!(command, Command::Attach(_)) {
            ATTACH_TIMEOUT_MS
        } else {
            CONTROL_TIMEOUT_MS
        };
        #[cfg(target_os = "macos")]
        if endpoint == SYSTEM_ENDPOINT && matches!(command, Command::Host(core::host::Request::Export { .. })) {
            capture::prepare(app.path().app_data_dir().map_err(|e| e.to_string())?, endpoint.clone())?;
            // Helper migration replaces the socket; discard the previous metadata link.
            state.lock().map_err(|_| failure(core::Error::Internal))?.client = None;
            let storage = matches!(command, Command::Host(core::host::Request::Export { storage_unmounted: true, .. }));
            let mut client = knightbus_sdk::Client::connect_system(Path::new(&endpoint)).map_err(failure)?;
            return client.request_export(command).map(present).map_err(|error| match error {
                core::Error::Denied if storage => "macOS blocked removable-drive access; the device was not shared. Check Removable Volumes under System Settings → Privacy & Security → Files & Folders → KnightBus. If it is already allowed, quit KnightBus completely and restart its helper from the app.".into(),
                core::Error::Deadline => "Sharing timed out. Refresh local devices before trying again.".into(),
                other => failure(other),
            });
        }
        let mut link = state.lock().map_err(|_| failure(core::Error::Internal))?;
        if let Some(last) = link.last {
            std::thread::sleep(CONTROL_POLL_INTERVAL.saturating_sub(last.elapsed()));
        }
        if link.endpoint != endpoint {
            link.client = None;
            link.endpoint = endpoint.clone();
        }
        if link.client.is_none() {
            let client = knightbus_sdk::connect_local(Path::new(&endpoint));
            link.client = Some(client.map_err(failure)?);
        }
        let result = link
            .client
            .as_mut()
            .ok_or_else(|| failure(core::Error::Unavailable))?
            .request(command, timeout);
        link.last = Some(Instant::now());
        match result {
            Ok(reply) => Ok(present(reply)),
            Err(error) => {
                link.client = None;
                Err(failure(error))
            }
        }
    })
    .await
    .map_err(|_| failure(core::Error::Internal))?
}
fn key(key: core::DeviceKey) -> Value {
    json!({"id":key.id.0.to_string(),"generation":key.generation.to_string()})
}
fn session(id: core::SessionId) -> Value {
    json!({"epoch":id.epoch.to_string(),"serial":id.serial.to_string()})
}
fn present(reply: Reply) -> Value {
    match reply {
        Reply::Host(value) => json!({"host":value}),
        Reply::Devices(devices) => {
            json!({"devices":devices.iter().map(|d| json!({"key":key(d.key),"name":d.name,"busy":d.busy,"shared":d.shared,"vendor":d.vendor,"product":d.product,"speed":d.speed,"class":d.class})).collect::<Vec<_>>()})
        }
        Reply::Inspection(device) => json!({
            "text":knightbus_sdk::cli::format_reply(&Reply::Inspection(device.clone())),
            "storage":device.configurations.iter().flat_map(|c| &c.interfaces).any(|i| i.class.class == core::topology::CLASS_MASS_STORAGE),
            "speed":device.wire.speed,
        }),
        Reply::Sessions(ids) => {
            json!({"sessions":ids.into_iter().map(session).collect::<Vec<_>>()})
        }
        Reply::Session(s) => {
            json!({"session":{"id":session(s.id),"peer":fingerprint(s.peer),"state":format!("{:?}",s.state),"error":s.error.map(failure),"devices":s.devices.into_iter().map(key).collect::<Vec<_>>()}})
        }
        Reply::Peers(peers) => {
            json!({"peers":peers.into_iter().map(fingerprint).collect::<Vec<_>>()})
        }
        Reply::Info { version, epoch, .. } => {
            json!({"version":version,"epoch":epoch.to_string(),"build":env!("CARGO_PKG_VERSION"),"os":std::env::consts::OS,"arch":std::env::consts::ARCH})
        }
        other => json!({"text":knightbus_sdk::cli::format_reply(&other)}),
    }
}
fn fingerprint(peer: core::PeerId) -> String {
    knightbus_sdk::service::hex(&peer.0)
}
#[tauri::command]
async fn defaults(app: tauri::AppHandle) -> Result<Value, String> {
    #[cfg(target_os = "macos")]
    let endpoint = SYSTEM_ENDPOINT;
    #[cfg(windows)]
    let endpoint = r"\\.\pipe\knightbus-control";
    #[cfg(not(any(windows, target_os = "macos")))]
    let endpoint = "/run/knightbus/control";
    let directory = app.path().app_data_dir().map_err(|e| e.to_string())?;
    tauri::async_runtime::spawn_blocking(move || {
        json!({"endpoint":endpoint,"localEndpoint":companion::endpoint(&directory),"name":computer::name(),"address":computer::address()})
    }).await.map_err(|e| e.to_string())
}
fn main() {
    tauri::Builder::default()
        .manage(State::default())
        .manage(companion::Launcher::default())
        .invoke_handler(tauri::generate_handler![
            request,
            defaults,
            companion::start_service,
            shutdown::quit_service,
            capture::enable_capture
        ])
        .setup(|app| {
            let show = MenuItem::with_id(app, "show", "Open KnightBus", true, None::<&str>)?;
            let quit = MenuItem::with_id(
                app,
                "quit",
                "Quit UI (sharing continues)",
                true,
                None::<&str>,
            )?;
            let stop = MenuItem::with_id(app, "stop", "Quit KnightBus…", true, None::<&str>)?;
            let menu = Menu::with_items(app, &[&show, &quit, &stop])?;
            let image = app
                .default_window_icon()
                .cloned()
                .ok_or("Missing application icon")?;
            TrayIconBuilder::new()
                .icon(image)
                .tooltip("KnightBus — USB sharing")
                .menu(&menu)
                .on_menu_event(|app, event| match event.id.as_ref() {
                    "show" => {
                        if let Some(window) = app.get_webview_window("main") {
                            let _ = window.show();
                            let _ = window.set_focus();
                        }
                    }
                    "quit" => app.exit(0),
                    "stop" => {
                        if let Some(window) = app.get_webview_window("main") {
                            let _ = window.show();
                            let _ = window.set_focus();
                        }
                        let _ = app.emit("quit-requested", ());
                    }
                    _ => {}
                })
                .build(app)?;
            Ok(())
        })
        .on_window_event(|window, event| {
            if let tauri::WindowEvent::CloseRequested { api, .. } = event {
                api.prevent_close();
                let _ = window.hide();
            }
        })
        .run(tauri::generate_context!())
        .expect("Could not start KnightBus desktop");
}
