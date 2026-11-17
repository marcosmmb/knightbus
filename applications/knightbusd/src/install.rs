//! Explicit macOS administrator installation. Only the daemon is privileged.
use knightbus_sdk::{
    SYSTEM_ENDPOINT,
    core::{Error, Result},
    ipc::private::Directory,
    macos::{
        DAEMON_FILE, ENDPOINT_DIR, IDENTITY_DIR, LAUNCHER_FILE, LEGACY_LAUNCHD_LABEL,
        LEGACY_LAUNCHD_PLIST, OWNER_FILE, PROGRAM_DIR, STATE_DIR,
    },
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

const ROOT_UID: u32 = 0;
const PRIVATE_MODE: u32 = 0o700;
const PROGRAM_MODE: u32 = 0o755;
const LAUNCHER_MODE: u32 = 0o4755;
const PRIVATE_FILE_MODE: u32 = 0o600;
const MAX_UID_BYTES: u64 = 10;
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
    protected_directory(Path::new(ENDPOINT_DIR), PROGRAM_MODE)?;
    let identity = Path::new(IDENTITY_DIR);
    if identity.exists() {
        if security::display_fingerprint(Store::open(identity)?.identity()?.peer()) != expected {
            return Err(Error::Denied);
        }
    } else {
        let staging = Path::new(STATE_DIR).join(format!("identity-install-{}", std::process::id()));
        let result = snapshot(source, uid, &staging, expected);
        if result.is_err() {
            let _ = fs::remove_dir_all(&staging);
        }
        result?;
        fs::rename(staging, identity).map_err(|_| Error::Unavailable)?;
    }
    let source_program = std::env::current_exe().map_err(|_| Error::Unavailable)?;
    install_program(&source_program, Path::new(DAEMON_FILE), PROGRAM_MODE)?;
    install_program(
        &source_program.with_file_name("knightbus-launcher"),
        Path::new(LAUNCHER_FILE),
        LAUNCHER_MODE,
    )?;
    if Command::new("/bin/launchctl")
        .args(["print", LEGACY_LAUNCHD_LABEL])
        .output()
        .map_err(|_| Error::Unavailable)?
        .status
        .success()
        && !Command::new("/bin/launchctl")
            .args(["bootout", LEGACY_LAUNCHD_LABEL])
            .status()
            .map_err(|_| Error::Unavailable)?
            .success()
    {
        return Err(Error::State);
    }
    // A system launch daemon loses the app's user-session privacy identity.
    // Retire the legacy registration before starting the app-authorized helper.
    match fs::remove_file(LEGACY_LAUNCHD_PLIST) {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(_) => return Err(Error::Denied),
    }
    let owner_staging =
        Path::new(STATE_DIR).join(format!("desktop-user-install-{}", std::process::id()));
    write_new(
        &owner_staging,
        uid.to_string().as_bytes(),
        PRIVATE_FILE_MODE,
    )?;
    fs::rename(owner_staging, OWNER_FILE).map_err(|_| Error::Unavailable)?;
    Ok(())
}

pub(super) fn restart(uid: u32) -> Result<()> {
    if !rustix::process::geteuid().is_root() || uid == ROOT_UID {
        return Err(Error::Denied);
    }
    let owner = File::from(
        rustix::fs::open(
            OWNER_FILE,
            OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
            Mode::empty(),
        )
        .map_err(|_| Error::Denied)?,
    );
    let metadata = owner.metadata().map_err(|_| Error::Denied)?;
    if !metadata.is_file()
        || metadata.uid() != ROOT_UID
        || metadata.mode() & NON_OWNER_ACCESS != 0
        || metadata.len() > MAX_UID_BYTES
    {
        return Err(Error::Denied);
    }
    let mut bytes = String::new();
    owner
        .take(MAX_UID_BYTES)
        .read_to_string(&mut bytes)
        .map_err(|_| Error::Denied)?;
    if bytes.parse::<u32>() != Ok(uid) {
        return Err(Error::Denied);
    }
    let state = Path::new(IDENTITY_DIR);
    let fingerprint = security::display_fingerprint(Store::open(state)?.identity()?.peer());
    install(uid, state, &fingerprint)
}

fn install_program(source: &Path, target: &Path, mode: u32) -> Result<()> {
    let name = target.file_name().ok_or(Error::Invalid)?.to_string_lossy();
    let staged = Path::new(PROGRAM_DIR).join(format!("{name}-install-{}", std::process::id()));
    let mut output = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(PROGRAM_MODE)
        .open(&staged)
        .map_err(|_| Error::Denied)?;
    let mut input = File::open(source).map_err(|_| Error::Unavailable)?;
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
        let _ = fs::remove_file(&staged);
        return Err(Error::Denied);
    }
    // Set the launcher's privilege only after verification, in a root-owned directory.
    fs::set_permissions(&staged, fs::Permissions::from_mode(mode)).map_err(|_| Error::Denied)?;
    fs::rename(staged, target).map_err(|_| Error::Unavailable)
}

/// Recreate the volatile root-owned endpoint directory after a reboot.
pub(super) fn prepare_endpoint() -> Result<()> {
    if !rustix::process::geteuid().is_root() {
        return Err(Error::Denied);
    }
    protected_directory(Path::new(ENDPOINT_DIR), PROGRAM_MODE)
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
