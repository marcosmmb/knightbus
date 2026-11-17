//! Fixed installation paths for the macOS privileged helper.
//!
//! These locations are protected by the root installer. They must not come from
//! an unprivileged caller's environment or preferences. Legacy launchd paths are
//! retained only to recognize and migrate older installations.

/// Root-owned directory containing the installed helper executables.
pub const PROGRAM_DIR: &str = "/Library/Application Support/KnightBus";
/// Installed daemon executable; it is not setuid.
pub const DAEMON_FILE: &str = "/Library/Application Support/KnightBus/knightbusd";
/// Restricted setuid launcher invoked directly by the desktop app.
pub const LAUNCHER_FILE: &str = "/Library/Application Support/KnightBus/knightbus-launcher";
/// Private root-owned service state directory.
pub const STATE_DIR: &str = "/var/db/knightbus";
/// Persistent identity and paired computers.
pub const IDENTITY_DIR: &str = "/var/db/knightbus/identity";
/// Installing account's UID, checked by the launcher before starting the daemon.
pub const OWNER_FILE: &str = "/var/db/knightbus/desktop-user";
/// Private service log.
pub const LOG_FILE: &str = "/var/db/knightbus/service.log";
/// Volatile directory recreated by the helper after a reboot.
pub const ENDPOINT_DIR: &str = "/var/run/knightbus";
/// Authenticated local control socket.
pub const CONTROL_ENDPOINT: &str = "/var/run/knightbus/control";
/// Previous launchd registration, removed during migration to the app launcher.
pub const LEGACY_LAUNCHD_PLIST: &str = "/Library/LaunchDaemons/io.knightbus.daemon.plist";
/// System-domain label used to stop the previous launchd service.
pub const LEGACY_LAUNCHD_LABEL: &str = "system/io.knightbus.daemon";
