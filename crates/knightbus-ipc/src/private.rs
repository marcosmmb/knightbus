//! Private on-disk host state. Filenames are fixed by the caller, never peer paths.
use knightbus_core::{Error, Result};
use std::{
    fs::File,
    io::{Read, Write},
    path::{Path, PathBuf},
};
#[cfg(unix)]
const DIRECTORY_MODE: u32 = 0o700;
#[cfg(unix)]
/// Owner-only read/write permissions for private files, including diagnostics.
pub const FILE_MODE: u32 = 0o600;
#[cfg(unix)]
pub(crate) const NON_OWNER_ACCESS: u32 = 0o077;
const PENDING_PREFIX: &str = ".pending-";

/// Directory protected for the current OS account (and Windows SYSTEM).
pub struct Directory {
    path: PathBuf,
    #[cfg(unix)]
    handle: File,
}
impl Directory {
    /// Create a new private directory or validate an existing one. Symlinks are rejected.
    pub fn open(path: &Path) -> Result<Self> {
        if !path.is_absolute() {
            return Err(Error::Invalid);
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::{DirBuilderExt, MetadataExt};
            match std::fs::DirBuilder::new().mode(DIRECTORY_MODE).create(path) {
                Ok(()) => {}
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {}
                Err(_) => return Err(Error::Unavailable),
            }
            let fd = rustix::fs::open(
                path,
                rustix::fs::OFlags::RDONLY
                    | rustix::fs::OFlags::DIRECTORY
                    | rustix::fs::OFlags::NOFOLLOW
                    | rustix::fs::OFlags::CLOEXEC,
                rustix::fs::Mode::empty(),
            )
            .map_err(|_| Error::Denied)?;
            let handle = File::from(fd);
            let metadata = handle.metadata().map_err(|_| Error::Denied)?;
            if metadata.uid() != rustix::process::geteuid().as_raw()
                || metadata.mode() & NON_OWNER_ACCESS != 0
            {
                return Err(Error::Denied);
            }
            Ok(Self {
                path: path.to_owned(),
                handle,
            })
        }
        #[cfg(windows)]
        {
            windows::protect(path)?;
            Ok(Self {
                path: path.to_owned(),
            })
        }
    }
    fn name(&self, name: &str) -> Result<PathBuf> {
        if name.is_empty()
            || !name
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'.' || b == b'-')
            || name == "."
            || name == ".."
        {
            return Err(Error::Invalid);
        }
        Ok(self.path.join(name))
    }
    fn file(&self, name: &str, create: bool) -> Result<File> {
        let path = self.name(name)?;
        #[cfg(unix)]
        {
            use rustix::fs::{Mode, OFlags, openat};
            use std::os::unix::fs::MetadataExt;
            let _ = path;
            let flags = if create {
                OFlags::WRONLY | OFlags::CREATE | OFlags::EXCL
            } else {
                OFlags::RDONLY
            };
            let fd = openat(
                &self.handle,
                name,
                flags | OFlags::NOFOLLOW | OFlags::CLOEXEC,
                Mode::from_raw_mode(FILE_MODE as _),
            )
            .map_err(|error| {
                if error == rustix::io::Errno::NOENT {
                    Error::NotFound
                } else {
                    Error::Denied
                }
            })?;
            let file = File::from(fd);
            let metadata = file.metadata().map_err(|_| Error::Denied)?;
            if !metadata.is_file()
                || metadata.uid() != rustix::process::geteuid().as_raw()
                || metadata.mode() & NON_OWNER_ACCESS != 0
            {
                return Err(Error::Denied);
            }
            Ok(file)
        }
        #[cfg(windows)]
        {
            if !create {
                let metadata = std::fs::symlink_metadata(&path).map_err(file_error)?;
                if !metadata.is_file() || metadata.file_type().is_symlink() {
                    return Err(Error::Denied);
                }
            }
            let file = std::fs::OpenOptions::new()
                .read(!create)
                .write(create)
                .create_new(create)
                .open(path)
                .map_err(file_error)?;
            if !create {
                windows::verify_file_access(&file)?;
            }
            Ok(file)
        }
    }
    /// Bounded read; metadata does not authorize an unbounded allocation.
    pub fn read(&self, name: &str, limit: usize) -> Result<Vec<u8>> {
        let file = self.file(name, false)?;
        let mut bytes = Vec::new();
        file.take(limit.checked_add(1).ok_or(Error::Invalid)? as u64)
            .read_to_end(&mut bytes)
            .map_err(|_| Error::Unavailable)?;
        if bytes.len() > limit {
            return Err(Error::Invalid);
        }
        Ok(bytes)
    }
    /// Publish a complete, synced file without replacing existing state.
    pub fn create(&self, name: &str, bytes: &[u8]) -> Result<()> {
        self.name(name)?;
        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_err(|_| Error::Internal)?
            .as_nanos();
        let temporary = format!("{PENDING_PREFIX}{}-{nonce}", std::process::id());
        let mut file = self.file(&temporary, true)?;
        let result = (|| {
            file.write_all(bytes)
                .and_then(|()| file.sync_all())
                .map_err(|_| Error::Unavailable)?;
            #[cfg(unix)]
            rustix::fs::linkat(
                &self.handle,
                temporary.as_str(),
                &self.handle,
                name,
                rustix::fs::AtFlags::empty(),
            )
            .map_err(|_| Error::Denied)?;
            #[cfg(windows)]
            std::fs::hard_link(self.name(&temporary)?, self.name(name)?)
                .map_err(|_| Error::Denied)?;
            Ok(())
        })();
        drop(file);
        let cleanup = self.remove(&temporary);
        result.and(cleanup)
    }
    /// Remove an explicitly selected record, then sync the directory on Unix.
    pub fn remove(&self, name: &str) -> Result<()> {
        let path = self.name(name)?;
        #[cfg(unix)]
        {
            let _ = path;
            rustix::fs::unlinkat(&self.handle, name, rustix::fs::AtFlags::empty())
                .map_err(|_| Error::Unavailable)?;
            self.handle.sync_all().map_err(|_| Error::Unavailable)?;
        }
        #[cfg(windows)]
        std::fs::remove_file(path).map_err(|_| Error::Unavailable)?;
        Ok(())
    }
    /// Enumerate a bounded number of record names. Contents still require validation.
    pub fn names(&self, limit: usize) -> Result<Vec<String>> {
        let mut names = Vec::new();
        for entry in std::fs::read_dir(&self.path).map_err(|_| Error::Unavailable)? {
            let name = entry
                .map_err(|_| Error::Unavailable)?
                .file_name()
                .into_string()
                .map_err(|_| Error::Invalid)?;
            self.name(&name)?;
            // Incomplete writes are never enrollment records, including after a crash.
            if name.starts_with(PENDING_PREFIX) {
                continue;
            }
            if names.len() >= limit {
                return Err(Error::Exhausted);
            }
            names.push(name);
        }
        Ok(names)
    }
}
#[cfg(windows)]
fn file_error(error: std::io::Error) -> Error {
    if error.kind() == std::io::ErrorKind::NotFound {
        Error::NotFound
    } else {
        Error::Denied
    }
}

#[cfg(windows)]
#[allow(unsafe_code)]
pub(crate) mod windows {
    use super::*;
    use std::{ffi::c_void, ptr};
    use windows_sys::Win32::{
        Foundation::{CloseHandle, ERROR_ALREADY_EXISTS, GetLastError, HANDLE, LocalFree},
        Security::{Authorization::*, *},
        Storage::FileSystem::{CreateDirectoryW, FILE_ALL_ACCESS},
        System::{
            SystemServices::ACCESS_ALLOWED_ACE_TYPE,
            Threading::{GetCurrentProcess, OpenProcessToken},
        },
    };
    const MAX_SECURITY_BYTES: u32 = 64 * 1024;

    struct Local(*mut c_void);
    impl Drop for Local {
        fn drop(&mut self) {
            unsafe {
                LocalFree(self.0);
            }
        }
    }
    struct Token(HANDLE);
    impl Drop for Token {
        fn drop(&mut self) {
            unsafe {
                CloseHandle(self.0);
            }
        }
    }
    fn wide(path: &Path) -> Result<Vec<u16>> {
        use std::os::windows::ffi::OsStrExt;
        let mut text: Vec<_> = path.as_os_str().encode_wide().collect();
        if text.contains(&0) {
            return Err(Error::Invalid);
        }
        text.push(0);
        Ok(text)
    }
    unsafe fn string(pointer: *const u16) -> String {
        // Windows returns an owned NUL-terminated string; caller retains LocalFree owner.
        let mut length = 0;
        while unsafe { *pointer.add(length) } != 0 {
            length += 1;
        }
        String::from_utf16_lossy(unsafe { std::slice::from_raw_parts(pointer, length) })
    }
    fn security_buffer(bytes: u32) -> Result<Vec<usize>> {
        if bytes == 0 || bytes > MAX_SECURITY_BYTES {
            return Err(Error::Denied);
        }
        Ok(vec![0; (bytes as usize).div_ceil(size_of::<usize>())])
    }

    // The caller keeps the OS-owned SID alive throughout conversion.
    unsafe fn sid_string(sid: PSID) -> Result<String> {
        let mut text = ptr::null_mut();
        if unsafe { ConvertSidToStringSidW(sid, &mut text) } == 0 {
            return Err(Error::Denied);
        }
        let _owner = Local(text.cast());
        Ok(unsafe { string(text) })
    }

    pub(crate) fn user() -> Result<String> {
        let mut raw = ptr::null_mut();
        // The process token handle is owned here and queried into aligned bounded storage.
        if unsafe { OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut raw) } == 0 {
            return Err(Error::Denied);
        }
        let token = Token(raw);
        let mut needed = 0;
        unsafe {
            GetTokenInformation(token.0, TokenUser, ptr::null_mut(), 0, &mut needed);
        }
        let mut storage = security_buffer(needed)?;
        if unsafe {
            GetTokenInformation(
                token.0,
                TokenUser,
                storage.as_mut_ptr().cast(),
                needed,
                &mut needed,
            )
        } == 0
        {
            return Err(Error::Denied);
        }
        let token_user = unsafe { &*storage.as_ptr().cast::<TOKEN_USER>() };
        unsafe { sid_string(token_user.User.Sid) }
    }
    fn dacl_string(sd: PSECURITY_DESCRIPTOR) -> Result<String> {
        let mut text = ptr::null_mut();
        if unsafe {
            ConvertSecurityDescriptorToStringSecurityDescriptorW(
                sd,
                SDDL_REVISION_1,
                DACL_SECURITY_INFORMATION,
                &mut text,
                ptr::null_mut(),
            )
        } == 0
        {
            return Err(Error::Denied);
        }
        let _owner = Local(text.cast());
        Ok(unsafe { string(text) })
    }
    pub(crate) fn verify_file_access(file: &File) -> Result<()> {
        use std::os::windows::io::AsRawHandle;
        let mut sd = ptr::null_mut();
        // Query the open handle, so replacing a pathname cannot change the inspected file.
        if unsafe {
            GetSecurityInfo(
                file.as_raw_handle(),
                SE_FILE_OBJECT,
                DACL_SECURITY_INFORMATION,
                ptr::null_mut(),
                ptr::null_mut(),
                ptr::null_mut(),
                ptr::null_mut(),
                &mut sd,
            )
        } != 0
        {
            return Err(Error::Denied);
        }
        let _sd = Local(sd);
        // Compare access entries and binary SIDs, not SDDL aliases or entry order.
        unsafe { verify_file_acl(sd, &user()?) }
    }

    unsafe fn verify_file_acl(sd: PSECURITY_DESCRIPTOR, user: &str) -> Result<()> {
        let text = widestring::U16CString::from_str(user).map_err(|_| Error::Denied)?;
        let mut user_sid = ptr::null_mut();
        if unsafe { ConvertStringSidToSidW(text.as_ptr(), &mut user_sid) } == 0 {
            return Err(Error::Denied);
        }
        let _user = Local(user_sid);
        let mut present = 0;
        let mut defaulted = 0;
        let mut acl = ptr::null_mut();
        if unsafe { GetSecurityDescriptorDacl(sd, &mut present, &mut acl, &mut defaulted) } == 0
            || present == 0
            || acl.is_null()
            || unsafe { IsValidAcl(acl) } == 0
        {
            return Err(Error::Denied);
        }
        let mut owner_allowed = false;
        let mut system_allowed = false;
        for index in 0..unsafe { (*acl).AceCount } {
            let mut raw = ptr::null_mut();
            if unsafe { GetAce(acl, u32::from(index), &mut raw) } == 0 {
                return Err(Error::Denied);
            }
            let header = unsafe { &*raw.cast::<ACE_HEADER>() };
            if header.AceType != ACCESS_ALLOWED_ACE_TYPE as u8
                || header.AceFlags & !(INHERITED_ACE as u8) != 0
                || usize::from(header.AceSize) < size_of::<ACCESS_ALLOWED_ACE>()
            {
                return Err(Error::Denied);
            }
            let ace = raw.cast::<ACCESS_ALLOWED_ACE>();
            let sid = unsafe { ptr::addr_of_mut!((*ace).SidStart).cast() };
            if unsafe { (*ace).Mask } != FILE_ALL_ACCESS || unsafe { IsValidSid(sid) } == 0 {
                return Err(Error::Denied);
            }
            let owner = unsafe { EqualSid(sid, user_sid) } != 0;
            let system = unsafe { IsWellKnownSid(sid, WinLocalSystemSid) } != 0;
            if !owner && !system {
                return Err(Error::Denied);
            }
            owner_allowed |= owner;
            system_allowed |= system;
        }
        if !owner_allowed || !system_allowed {
            return Err(Error::Denied);
        }
        Ok(())
    }
    pub(crate) fn verify_pipe_owner(handle: HANDLE) -> Result<()> {
        let mut needed = 0;
        // Query the connected object, not a PID that could be reused after exit.
        unsafe {
            GetKernelObjectSecurity(
                handle,
                OWNER_SECURITY_INFORMATION,
                ptr::null_mut(),
                0,
                &mut needed,
            );
        }
        let mut storage = security_buffer(needed)?;
        if unsafe {
            GetKernelObjectSecurity(
                handle,
                OWNER_SECURITY_INFORMATION,
                storage.as_mut_ptr().cast(),
                needed,
                &mut needed,
            )
        } == 0
        {
            return Err(Error::Denied);
        }
        let mut owner = ptr::null_mut();
        let mut defaulted = 0;
        if unsafe {
            GetSecurityDescriptorOwner(storage.as_mut_ptr().cast(), &mut owner, &mut defaulted)
        } == 0
        {
            return Err(Error::Denied);
        }
        if unsafe { sid_string(owner)? } != user()? {
            return Err(Error::Denied);
        }
        Ok(())
    }
    pub fn protect(path: &Path) -> Result<()> {
        use interprocess::os::windows::security_descriptor::{
            AsSecurityDescriptor, SecurityDescriptor,
        };
        let sid = user()?;
        let sddl = widestring::U16CString::from_str(format!(
            "O:{sid}D:P(A;OICI;FA;;;SY)(A;OICI;FA;;;{sid})"
        ))
        .map_err(|_| Error::Invalid)?;
        let descriptor = SecurityDescriptor::deserialize(&sddl).map_err(|_| Error::Denied)?;
        let expected = dacl_string(descriptor.as_sd().cast_mut())?;
        let attributes = SECURITY_ATTRIBUTES {
            nLength: std::mem::size_of::<SECURITY_ATTRIBUTES>() as u32,
            lpSecurityDescriptor: descriptor.as_sd().cast_mut(),
            bInheritHandle: 0,
        };
        let path_wide = wide(path)?;
        // Descriptor and path remain alive throughout the synchronous call.
        if unsafe { CreateDirectoryW(path_wide.as_ptr(), &attributes) } == 0
            && unsafe { GetLastError() } != ERROR_ALREADY_EXISTS
        {
            return Err(Error::Unavailable);
        }
        let metadata = std::fs::symlink_metadata(path).map_err(|_| Error::Denied)?;
        if !metadata.is_dir() || metadata.file_type().is_symlink() {
            return Err(Error::Denied);
        }
        let mut sd = ptr::null_mut();
        let mut owner = ptr::null_mut();
        if unsafe {
            GetNamedSecurityInfoW(
                path_wide.as_ptr(),
                SE_FILE_OBJECT,
                DACL_SECURITY_INFORMATION | OWNER_SECURITY_INFORMATION,
                &mut owner,
                ptr::null_mut(),
                ptr::null_mut(),
                ptr::null_mut(),
                &mut sd,
            )
        } != 0
        {
            return Err(Error::Denied);
        }
        let _owner = Local(sd);
        if dacl_string(sd)? != expected {
            return Err(Error::Denied);
        }
        if unsafe { sid_string(owner)? } != sid {
            return Err(Error::Denied);
        }
        Ok(())
    }
    #[cfg(test)]
    mod tests {
        use super::*;
        use interprocess::os::windows::security_descriptor::{
            AsSecurityDescriptor, SecurityDescriptor,
        };

        fn check(sddl: &str, user: &str) -> Result<()> {
            let text = widestring::U16CString::from_str(sddl).unwrap();
            let descriptor = SecurityDescriptor::deserialize(&text).unwrap();
            unsafe { verify_file_acl(descriptor.as_sd().cast_mut(), user) }
        }

        #[test]
        fn private_acl_checks_sids_not_aliases_or_entry_order() {
            let ordinary = user().unwrap();
            let (domain, _) = ordinary.rsplit_once('-').unwrap();
            let administrator = format!("{domain}-500");
            for sid in [&ordinary, &administrator, "S-1-5-18"] {
                assert_eq!(
                    check(&format!("D:P(A;;FA;;;SY)(A;;FA;;;{sid})"), sid),
                    Ok(())
                );
                assert_eq!(
                    check(&format!("D:P(A;ID;FA;;;{sid})(A;ID;FA;;;SY)"), sid),
                    Ok(())
                );
            }
        }

        #[test]
        fn private_acl_rejects_missing_permissions_and_additional_trustees() {
            let sid = user().unwrap();
            for sddl in [
                "D:P".to_owned(),
                format!("D:P(A;;FA;;;SY)(A;;FR;;;{sid})"),
                format!("D:P(A;;FA;;;SY)(A;;FA;;;{sid})(A;;FR;;;WD)"),
                format!("D:P(A;;FA;;;SY)(A;IO;FA;;;{sid})"),
                "D:NO_ACCESS_CONTROL".to_owned(),
            ] {
                assert_eq!(check(&sddl, &sid), Err(Error::Denied), "{sddl}");
            }
        }
    }
}
