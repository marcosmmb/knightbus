//! Explicit macOS administrator installation. Only the daemon is privileged.
use knightbus_sdk::{
    SYSTEM_ENDPOINT,
    core::{Error, Result},
    ipc::private::Directory,
};
use knightbus_transport::security::{self, Store};
use rustix::fs::{Mode, OFlags};
use std::{
    fs::{self, File, OpenOptions},
    io::{Read, Write},
    os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt},
    path::Path,
    process::Command,
};

const PROGRAM_DIR: &str = "/Library/Application Support/KnightBus";
const STATE_DIR: &str = "/var/db/knightbus";
const PLIST: &str = "/Library/LaunchDaemons/io.knightbus.daemon.plist";
const LABEL: &str = "system/io.knightbus.daemon";
const ROOT_UID: u32 = 0;
const PRIVATE_MODE: u32 = 0o700;
const PROGRAM_MODE: u32 = 0o755;
const PLIST_MODE: u32 = 0o644;
const NON_OWNER_WRITE: u32 = 0o022;
const NON_OWNER_ACCESS: u32 = 0o077;
const IDENTITY_RECORDS: usize = 2;
const MAX_STATE_FILE: usize = size_of::<u32>() + security::MAX_CREDENTIAL_BYTES * 2;

fn protected_directory(path: &Path, mode: u32) -> Result<()> {
    match fs::create_dir(path) {
        Ok(()) => fs::set_permissions(path, fs::Permissions::from_mode(mode))
            .map_err(|_| Error::Denied)?,
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {}
        Err(_) => return Err(Error::Unavailable),
    }
    let metadata = fs::symlink_metadata(path).map_err(|_| Error::Denied)?;
    if !metadata.is_dir() || metadata.uid() != ROOT_UID || metadata.mode() & NON_OWNER_WRITE != 0 {
        return Err(Error::Denied);
    }
    Ok(())
}
fn snapshot(source: &Path, uid: u32, target: &Path, expected: &str) -> Result<()> {
    let handle = File::from(
        rustix::fs::open(
            source,
            OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
            Mode::empty(),
        )
        .map_err(|_| Error::Denied)?,
    );
    let metadata = handle.metadata().map_err(|_| Error::Denied)?;
    if metadata.uid() != uid || metadata.mode() & NON_OWNER_ACCESS != 0 {
        return Err(Error::Denied);
    }
    let directory = Directory::open(target)?;
    for (index, entry) in fs::read_dir(source).map_err(|_| Error::Denied)?.enumerate() {
        if index >= security::MAX_PEERS + IDENTITY_RECORDS {
            return Err(Error::Exhausted);
        }
        let name = entry
            .map_err(|_| Error::Denied)?
            .file_name()
            .into_string()
            .map_err(|_| Error::Invalid)?;
        if name != "identity.bin" && name != "certificate.der" {
            security::parse_fingerprint(name.strip_prefix("peer-").ok_or(Error::Invalid)?)?;
        }
        let file = File::from(
            rustix::fs::openat(
                &handle,
                name.as_str(),
                OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::NONBLOCK | OFlags::CLOEXEC,
                Mode::empty(),
            )
            .map_err(|_| Error::Denied)?,
        );
        let metadata = file.metadata().map_err(|_| Error::Denied)?;
        if !metadata.is_file()
            || metadata.uid() != uid
            || metadata.mode() & NON_OWNER_ACCESS != 0
            || metadata.len() > MAX_STATE_FILE as u64
        {
            return Err(Error::Denied);
        }
        let mut bytes = zeroize::Zeroizing::new(Vec::new());
        file.take(MAX_STATE_FILE as u64 + 1)
            .read_to_end(&mut bytes)
            .map_err(|_| Error::Unavailable)?;
        if bytes.len() > MAX_STATE_FILE {
            return Err(Error::Exhausted);
        }
        directory.create(&name, &bytes)?;
    }
    let store = Store::open(target)?;
    if security::display_fingerprint(store.identity()?.peer()) != expected {
        return Err(Error::Denied);
    }
    store.peers()?;
    Ok(())
}
fn write_new(path: &Path, bytes: &[u8], mode: u32) -> Result<()> {
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(mode)
        .open(path)
        .map_err(|_| Error::Denied)?;
    file.write_all(bytes).map_err(|_| Error::Unavailable)?;
    file.sync_all().map_err(|_| Error::Unavailable)
}

pub(super) fn install(uid: u32, source: &Path, expected: &str) -> Result<()> {
    if !rustix::process::geteuid().is_root() || uid == ROOT_UID || !source.is_absolute() {
        return Err(Error::Denied);
    }
    security::parse_fingerprint(expected)?;
    // An existing listener must be stopped through authenticated IPC first.
    if Path::new(SYSTEM_ENDPOINT).symlink_metadata().is_ok() {
        return Err(Error::State);
    }
    protected_directory(Path::new(PROGRAM_DIR), PROGRAM_MODE)?;
    protected_directory(Path::new(STATE_DIR), PRIVATE_MODE)?;
    protected_directory(Path::new("/var/run/knightbus"), PROGRAM_MODE)?;
    let identity = Path::new(STATE_DIR).join("identity");
    if identity.exists() {
        if security::display_fingerprint(Store::open(&identity)?.identity()?.peer()) != expected {
            return Err(Error::Denied);
        }
    } else {
        let staging = Path::new(STATE_DIR).join(format!("identity-install-{}", std::process::id()));
        let result = snapshot(source, uid, &staging, expected);
        if result.is_err() {
            let _ = fs::remove_dir_all(&staging);
        }
        result?;
        fs::rename(staging, &identity).map_err(|_| Error::Unavailable)?;
    }
    let daemon = Path::new(PROGRAM_DIR).join("knightbusd");
    let staged = Path::new(PROGRAM_DIR).join(format!("knightbusd-install-{}", std::process::id()));
    let mut output = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(PROGRAM_MODE)
        .open(&staged)
        .map_err(|_| Error::Denied)?;
    let mut input = File::open(std::env::current_exe().map_err(|_| Error::Unavailable)?)
        .map_err(|_| Error::Unavailable)?;
    std::io::copy(&mut input, &mut output).map_err(|_| Error::Unavailable)?;
    output.sync_all().map_err(|_| Error::Unavailable)?;
    drop(output);
    if !Command::new("/usr/bin/codesign")
        .args(["--verify", "--strict"])
        .arg(&staged)
        .status()
        .map_err(|_| Error::Unavailable)?
        .success()
    {
        return Err(Error::Denied);
    }
    fs::rename(staged, &daemon).map_err(|_| Error::Unavailable)?;
    let plist = format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0"><dict>
<key>Label</key><string>io.knightbus.daemon</string>
<key>ProgramArguments</key><array><string>{PROGRAM_DIR}/knightbusd</string><string>--desktop-user</string><string>{STATE_DIR}/identity</string><string>{SYSTEM_ENDPOINT}</string><string>{uid}</string></array>
<key>RunAtLoad</key><true/>
<key>StandardOutPath</key><string>{STATE_DIR}/service.log</string>
<key>StandardErrorPath</key><string>{STATE_DIR}/service.log</string>
</dict></plist>"#
    );
    let staged_plist =
        Path::new(STATE_DIR).join(format!("service-install-{}.plist", std::process::id()));
    write_new(&staged_plist, plist.as_bytes(), PLIST_MODE)?;
    if Command::new("/bin/launchctl")
        .args(["print", LABEL])
        .output()
        .map_err(|_| Error::Unavailable)?
        .status
        .success()
        && !Command::new("/bin/launchctl")
            .args(["bootout", LABEL])
            .status()
            .map_err(|_| Error::Unavailable)?
            .success()
    {
        return Err(Error::State);
    }
    fs::rename(staged_plist, PLIST).map_err(|_| Error::Unavailable)?;
    if !Command::new("/bin/launchctl")
        .args(["bootstrap", "system", PLIST])
        .status()
        .map_err(|_| Error::Unavailable)?
        .success()
    {
        return Err(Error::Unavailable);
    }
    // No KeepAlive: an explicit UI shutdown stays stopped until started again.
    Ok(())
}

/// Recreate the volatile root-owned endpoint directory after a reboot.
pub(super) fn prepare_endpoint() -> Result<()> {
    if !rustix::process::geteuid().is_root() {
        return Err(Error::Denied);
    }
    protected_directory(Path::new("/var/run/knightbus"), PROGRAM_MODE)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn migration_preserves_identity_and_peers_and_rejects_symlinks() {
        use std::os::unix::fs::symlink;
        let parent = std::env::temp_dir().join(format!(
            "knightbus-migration-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir(&parent).unwrap();
        let source = parent.join("source");
        let peer_dir = parent.join("peer");
        let fingerprint = security::display_fingerprint(Store::initialize(&source).unwrap());
        Store::initialize(&peer_dir).unwrap();
        let peer = Store::open(&peer_dir).unwrap().identity().unwrap();
        Store::open(&source)
            .unwrap()
            .pair(peer.certificate().to_vec(), peer.peer())
            .unwrap();
        let uid = rustix::process::geteuid().as_raw();
        let target = parent.join("target");
        snapshot(&source, uid, &target, &fingerprint).unwrap();
        let copied = Store::open(&target).unwrap();
        assert_eq!(
            security::display_fingerprint(copied.identity().unwrap().peer()),
            fingerprint
        );
        assert!(copied.peers().unwrap().contains(peer.peer()));
        assert_eq!(
            fs::read(source.join("identity.bin")).unwrap(),
            fs::read(target.join("identity.bin")).unwrap()
        );
        fs::remove_file(source.join("certificate.der")).unwrap();
        symlink(
            peer_dir.join("certificate.der"),
            source.join("certificate.der"),
        )
        .unwrap();
        assert_eq!(
            snapshot(&source, uid, &parent.join("refused"), &fingerprint),
            Err(Error::Denied)
        );
        fs::remove_dir_all(parent).unwrap();
    }

    #[test]
    fn installer_rejects_invalid_authority_before_writing() {
        assert_eq!(
            install(ROOT_UID, Path::new("relative"), "invalid"),
            Err(Error::Denied)
        );
    }
}
