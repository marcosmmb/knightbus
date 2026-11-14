//! One software controller per attachment. Fixed slot names bound persisted PnP
//! records; an administrator-only named object excludes other live owners.
use super::last_error;
use knightbus_core::{Error, Result};
use std::{
    ptr,
    sync::{Condvar, Mutex},
    thread,
    time::{Duration, Instant},
};
use windows_sys::{
    Wdk::{Foundation::OBJECT_ATTRIBUTES, Storage::FileSystem::NtCreateEvent},
    Win32::{
        Devices::{DeviceAndDriverInstallation::*, Enumeration::Pnp::*},
        Foundation::*,
        Security::Authorization::*,
        System::{
            Kernel::NotificationEvent,
            Registry::REG_MULTI_SZ,
            Threading::{
                EVENT_MODIFY_STATE, ResetEvent, SYNCHRONIZATION_SYNCHRONIZE, WaitForSingleObject,
            },
        },
    },
    core::w,
};

const MAX_CONTROLLERS: usize = 64; // Same ceiling as the kernel controller registry.
const ID_LENGTH: usize = MAX_DEVICE_ID_LEN as usize; // Includes terminator.
const TIMEOUT: Duration = Duration::from_secs(10);
const PNP_POLL_INTERVAL: Duration = Duration::from_millis(20);
const MAX_PROPERTY_UNITS: usize = 2048;
const MULTI_SZ_TERMINATOR: [u16; 2] = [0, 0];
type Id = [u16; ID_LENGTH];
type Created = (Mutex<Option<Result<Id>>>, Condvar);

pub(super) struct Lifetime {
    slot: HANDLE,
    name: Vec<u16>,
    software: HSWDEVICE,
    awaiting_identity: bool,
    callback: Box<Created>,
    instance: Option<Id>,
    hub: Option<Id>,
}
// SAFETY: The named event's existence reserves the slot without thread ownership;
// its handle and the software-device handle may move between threads.
// The callback accesses only its stable Box, synchronized through Mutex/Condvar.
unsafe impl Send for Lifetime {}
impl Lifetime {
    pub(super) fn reserve() -> Result<Self> {
        let mut security = ptr::null_mut();
        // SAFETY: Constant SDDL, local output freed after NtCreateEvent copies it.
        if unsafe {
            ConvertStringSecurityDescriptorToSecurityDescriptorW(
                w!("D:P(A;;GA;;;SY)(A;;GA;;;BA)"),
                SDDL_REVISION_1,
                &mut security,
                ptr::null_mut(),
            )
        } == 0
        {
            return Err(last_error());
        }
        let mut acquired = None;
        for slot in 0..MAX_CONTROLLERS {
            // Unlike BaseNamedObjects (Win32 Global\), the OS-owned \Device
            // directory denies object creation by unprivileged tokens. The
            // parent protects a vacant name; this event's ACL protects it after
            // creation. No user-created directory or namespace alias is trusted.
            let mut name = wide(&format!("\\Device\\KnightBus.Controller.{slot}"));
            let mut name = UNICODE_STRING {
                Length: ((name.len() - 1) * size_of::<u16>()) as u16,
                MaximumLength: (name.len() * size_of::<u16>()) as u16,
                Buffer: name.as_mut_ptr(),
            };
            let attributes = OBJECT_ATTRIBUTES {
                Length: std::mem::size_of::<OBJECT_ATTRIBUTES>() as u32,
                ObjectName: &mut name,
                SecurityDescriptor: security.cast(),
                // Deliberately omit OBJ_OPENIF: never adopt an existing event.
                Attributes: OBJ_CASE_INSENSITIVE,
                ..Default::default()
            };
            let mut handle = ptr::null_mut();
            // SAFETY: Live bounded Unicode name and descriptor. Native event
            // handles support the ordinary Win32 wait/reset/close operations.
            let status = unsafe {
                NtCreateEvent(
                    &mut handle,
                    EVENT_MODIFY_STATE | SYNCHRONIZATION_SYNCHRONIZE,
                    &attributes,
                    NotificationEvent,
                    false,
                )
            };
            match status {
                STATUS_OBJECT_NAME_COLLISION => continue,
                s if s >= 0 => acquired = Some(Ok((handle, wide(&format!("slot{slot}"))))),
                STATUS_ACCESS_DENIED => acquired = Some(Err(Error::Denied)),
                STATUS_NO_MEMORY | STATUS_INSUFFICIENT_RESOURCES => {
                    acquired = Some(Err(Error::Exhausted))
                }
                _ => acquired = Some(Err(Error::Unavailable)),
            }
            break;
        }
        unsafe {
            LocalFree(security);
        }
        let (slot, name) = acquired.ok_or(Error::Exhausted)??;
        Ok(Self {
            slot,
            name,
            software: ptr::null_mut(),
            awaiting_identity: false,
            callback: Box::new((Mutex::new(None), Condvar::new())),
            instance: None,
            hub: None,
        })
    }
    pub(super) fn start(&mut self) -> Result<String> {
        self.create()?;
        let controller =
            locate(self.instance.as_ref().ok_or(Error::State)?)?.ok_or(Error::NotFound)?;
        let hub = wait_hub(controller)?;
        self.hub = Some(device_id(hub)?);
        let mut filters = property(hub, CM_DRP_LOWERFILTERS)?;
        let filter = wide("KnightBusUde");
        if !filters
            .split(|v| *v == 0)
            .any(|v| v == &filter[..filter.len() - 1])
        {
            // Only our freshly created controller's exact child may be changed.
            // Preserve other filters. Recreate the empty controller once to load
            // its new per-device filter; no USB attachment exists at this point.
            while filters.last() == Some(&0) {
                filters.pop();
            }
            if !filters.is_empty() {
                filters.push(0);
            }
            filters.extend_from_slice(&filter);
            filters.push(0);
            if unsafe {
                CM_Set_DevNode_Registry_PropertyW(
                    hub,
                    CM_DRP_LOWERFILTERS,
                    filters.as_ptr().cast(),
                    (filters.len() * size_of::<u16>()) as u32,
                    0,
                )
            } != CR_SUCCESS
            {
                return Err(Error::Unavailable);
            }
            self.unplug()?;
            self.create()?;
            let controller =
                locate(self.instance.as_ref().ok_or(Error::State)?)?.ok_or(Error::NotFound)?;
            self.hub = Some(device_id(wait_hub(controller)?)?);
        }
        let id = self.instance.as_ref().ok_or(Error::State)?;
        let end = id.iter().position(|v| *v == 0).ok_or(Error::Invalid)?;
        String::from_utf16(&id[..end]).map_err(|_| Error::Invalid)
    }
    fn create(&mut self) -> Result<()> {
        // The preceding controller must have signaled final destruction before
        // this slot is reused for the empty-controller/filter restart.
        if unsafe { ResetEvent(self.slot) } == 0 {
            return Err(last_error());
        }
        self.instance = None;
        self.hub = None;
        *self.callback.0.lock().unwrap_or_else(|e| e.into_inner()) = None;
        let info = SW_DEVICE_CREATE_INFO {
            cbSize: std::mem::size_of::<SW_DEVICE_CREATE_INFO>() as u32,
            pszInstanceId: self.name.as_ptr(),
            pszzHardwareIds: w!("ROOT\\KNIGHTBUS_UDE\0"),
            CapabilityFlags: SWDeviceCapabilitiesDriverRequired as u32,
            pszDeviceDescription: w!("KnightBus attachment controller"),
            ..Default::default()
        };
        // SAFETY: The API copies creation parameters. Callback storage is boxed
        // before publication, and cannot be freed until SwDeviceClose returns.
        let result = unsafe {
            SwDeviceCreate(
                w!("KnightBus"),
                w!("HTREE\\ROOT\\0"),
                &info,
                0,
                ptr::null(),
                Some(created),
                (&*self.callback as *const Created).cast(),
                &mut self.software,
            )
        };
        if result < 0 {
            return Err(Error::Unavailable);
        }
        self.awaiting_identity = true;
        let result = self.callback.0.lock().unwrap_or_else(|e| e.into_inner());
        let (result, _) = self
            .callback
            .1
            .wait_timeout_while(result, TIMEOUT, |r| r.is_none())
            .unwrap_or_else(|e| e.into_inner());
        self.instance = Some(result.as_ref().copied().ok_or(Error::Deadline)??);
        self.awaiting_identity = false;
        Ok(())
    }
    pub(super) fn unplug(&mut self) -> Result<()> {
        if !self.software.is_null() {
            // SAFETY: Unique live software-device handle. This also joins any
            // in-progress create callback, including a timed-out creation.
            unsafe {
                SwDeviceClose(self.software);
            }
            self.software = ptr::null_mut();
            if self.instance.is_none() {
                self.instance = self
                    .callback
                    .0
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .as_ref()
                    .and_then(|v| v.as_ref().ok())
                    .copied();
            }
        }
        if self.instance.is_none() {
            // SwDeviceCreate failure did not accept a device. An accepted
            // creation with no recoverable identity remains quarantined.
            return if self.awaiting_identity {
                Err(Error::Cleanup)
            } else {
                Ok(())
            };
        }
        let deadline = Instant::now() + TIMEOUT;
        loop {
            let gone = [self.instance.as_ref(), self.hub.as_ref()]
                .into_iter()
                .flatten()
                .try_fold(true, |gone, id| {
                    Ok::<_, Error>(gone && locate(id)?.is_none())
                })?;
            if gone {
                // PnP absence alone is insufficient: an application may still
                // hold the USB function open. The driver retains this event's
                // name and signals it only in the controller destroy callback.
                let remaining = deadline.saturating_duration_since(Instant::now());
                return if unsafe { WaitForSingleObject(self.slot, remaining.as_millis() as u32) }
                    == WAIT_OBJECT_0
                {
                    Ok(())
                } else {
                    Err(Error::Cleanup)
                };
            }
            if Instant::now() >= deadline {
                return Err(Error::Cleanup);
            }
            thread::sleep(PNP_POLL_INTERVAL);
        }
    }
}
impl Drop for Lifetime {
    fn drop(&mut self) {
        // The caller only drops a retired lifetime. If that cannot be proved it
        // retains this entire owner, including the slot, until process exit.
        if !self.software.is_null() {
            unsafe {
                SwDeviceClose(self.software);
            }
        }
        unsafe {
            CloseHandle(self.slot);
        }
    }
}
unsafe extern "system" fn created(
    _handle: HSWDEVICE,
    status: i32,
    context: *const std::ffi::c_void,
    instance: *const u16,
) {
    // SAFETY: SwDeviceCreate supplies a terminated OS string during this callback;
    // the fixed bound rejects unexpected IDs. No pointer escapes the callback.
    let result = if status < 0 || instance.is_null() {
        Err(Error::Unavailable)
    } else {
        let mut id = [0; ID_LENGTH];
        let mut valid = false;
        for (index, value) in id.iter_mut().enumerate() {
            *value = unsafe { *instance.add(index) };
            if *value == 0 {
                valid = index != 0;
                break;
            }
        }
        if valid { Ok(id) } else { Err(Error::Invalid) }
    };
    let shared = unsafe { &*context.cast::<Created>() };
    *shared.0.lock().unwrap_or_else(|e| e.into_inner()) = Some(result);
    shared.1.notify_one();
}
fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain([0]).collect()
}
fn locate(id: &Id) -> Result<Option<u32>> {
    let mut node = 0;
    match unsafe { CM_Locate_DevNodeW(&mut node, id.as_ptr(), CM_LOCATE_DEVNODE_NORMAL) } {
        CR_SUCCESS => Ok(Some(node)),
        CR_NO_SUCH_DEVNODE => Ok(None),
        _ => Err(Error::Unavailable),
    }
}
fn device_id(node: u32) -> Result<Id> {
    let mut id = [0; ID_LENGTH];
    if unsafe { CM_Get_Device_IDW(node, id.as_mut_ptr(), ID_LENGTH as u32, 0) } != CR_SUCCESS {
        return Err(Error::Unavailable);
    }
    Ok(id)
}
fn property(node: u32, key: u32) -> Result<Vec<u16>> {
    let mut data = vec![0u16; MAX_PROPERTY_UNITS];
    let mut bytes = (data.len() * size_of::<u16>()) as u32;
    let mut kind = 0;
    let status = unsafe {
        CM_Get_DevNode_Registry_PropertyW(
            node,
            key,
            &mut kind,
            data.as_mut_ptr().cast(),
            &mut bytes,
            0,
        )
    };
    if status == CR_NO_SUCH_VALUE && key == CM_DRP_LOWERFILTERS {
        return Ok(Vec::new());
    }
    if status != CR_SUCCESS
        || kind != REG_MULTI_SZ
        || (bytes as usize) < size_of_val(&MULTI_SZ_TERMINATOR)
        || bytes as usize % size_of::<u16>() != 0
        || bytes as usize > data.len() * size_of::<u16>()
    {
        return Err(Error::Invalid);
    }
    data.truncate(bytes as usize / size_of::<u16>());
    if !data.ends_with(&MULTI_SZ_TERMINATOR) {
        return Err(Error::Invalid);
    }
    Ok(data)
}
fn wait_hub(controller: u32) -> Result<u32> {
    let deadline = Instant::now() + TIMEOUT;
    loop {
        let mut child = 0;
        let first = unsafe { CM_Get_Child(&mut child, controller, 0) };
        if first == CR_SUCCESS {
            // A KnightBus controller has exactly one virtual root hub. Refuse
            // an unexpected tree instead of searching/mutating physical hubs.
            let mut sibling = 0;
            if unsafe { CM_Get_Sibling(&mut sibling, child, 0) } != CR_NO_SUCH_DEVNODE {
                return Err(Error::Invalid);
            }
            let ids = property(child, CM_DRP_HARDWAREID)?;
            let expected = wide("USB\\ROOT_HUB30");
            if !ids
                .split(|v| *v == 0)
                .any(|id| id == &expected[..expected.len() - 1])
            {
                return Err(Error::Invalid);
            }
            let mut parent = 0;
            if unsafe { CM_Get_Parent(&mut parent, child, 0) } != CR_SUCCESS || parent != controller
            {
                return Err(Error::Invalid);
            }
            let (mut status, mut problem) = (0, 0);
            if unsafe { CM_Get_DevNode_Status(&mut status, &mut problem, child, 0) } == CR_SUCCESS
                && status & DN_STARTED != 0
                && problem == 0
            {
                return Ok(child);
            }
        } else if first != CR_NO_SUCH_DEVNODE {
            return Err(Error::Unavailable);
        }
        if Instant::now() >= deadline {
            return Err(Error::Deadline);
        }
        thread::sleep(PNP_POLL_INTERVAL);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle};
    use windows_sys::Win32::{Security::*, System::Threading::*};
    static TEST: Mutex<()> = Mutex::new(());
    fn owned(handle: HANDLE) -> OwnedHandle {
        assert!(!handle.is_null());
        // SAFETY: Successful API output, transferred exactly once to this guard.
        unsafe { OwnedHandle::from_raw_handle(handle) }
    }
    #[test]
    #[ignore = "requires administrator access and no active KnightBus attachment owners"]
    fn unprivileged_names_cannot_reserve_controller_slots() {
        let _guard = TEST.lock().unwrap();
        let mut token = ptr::null_mut();
        assert_ne!(
            unsafe {
                OpenProcessToken(
                    GetCurrentProcess(),
                    TOKEN_DUPLICATE | TOKEN_QUERY,
                    &mut token,
                )
            },
            0
        );
        let token = owned(token);
        let mut sid = [0u32; SECURITY_MAX_SID_SIZE as usize / size_of::<u32>()]; // Aligned for SID.
        let mut length = std::mem::size_of_val(&sid) as u32;
        assert_ne!(
            unsafe {
                CreateWellKnownSid(
                    WinBuiltinAdministratorsSid,
                    ptr::null_mut(),
                    sid.as_mut_ptr().cast(),
                    &mut length,
                )
            },
            0
        );
        let disable = SID_AND_ATTRIBUTES {
            Sid: sid.as_mut_ptr().cast(),
            Attributes: 0,
        };
        let mut restricted = ptr::null_mut();
        assert_ne!(
            unsafe {
                CreateRestrictedToken(
                    token.as_raw_handle(),
                    DISABLE_MAX_PRIVILEGE,
                    1,
                    &disable,
                    0,
                    ptr::null(),
                    0,
                    ptr::null(),
                    &mut restricted,
                )
            },
            0
        );
        let restricted = owned(restricted);
        struct Revert;
        impl Drop for Revert {
            fn drop(&mut self) {
                assert_ne!(unsafe { RevertToSelf() }, 0);
            }
        }
        assert_ne!(
            unsafe { ImpersonateLoggedOnUser(restricted.as_raw_handle()) },
            0
        );
        let impersonation = Revert;
        // Denied before any event exists: the protected parent, not an event
        // ACL, prevents precreation by the non-admin, privilege-stripped token.
        assert!(matches!(Lifetime::reserve(), Err(Error::Denied)));
        let squatters: Vec<_> = (0..MAX_CONTROLLERS)
            .map(|slot| {
                let name = wide(&format!("Global\\KnightBus.Controller.{slot}"));
                owned(unsafe { CreateMutexW(ptr::null(), 0, name.as_ptr()) })
            })
            .collect();
        drop(impersonation);
        // All old names are occupied by the wrong object type; elevated creation
        // still succeeds in every protected slot, including slot zero.
        let slots: Vec<_> = (0..MAX_CONTROLLERS)
            .map(|_| Lifetime::reserve().unwrap())
            .collect();
        assert_eq!(slots[0].name, wide("slot0"));
        drop(slots);
        drop(squatters);
    }
    #[test]
    #[ignore = "requires administrator access and no active KnightBus attachment owners"]
    fn slots_are_bounded_and_release_on_another_thread() {
        let _guard = TEST.lock().unwrap();
        let mut slots: Vec<_> = (0..MAX_CONTROLLERS)
            .map(|_| Lifetime::reserve().unwrap())
            .collect();
        assert!(matches!(Lifetime::reserve(), Err(Error::Exhausted)));
        let first = slots.remove(0);
        let name = first.name.clone();
        // The reservation owns a kernel handle, not thread-affine mutex state.
        thread::spawn(move || drop(first)).join().unwrap();
        let replacement = Lifetime::reserve().unwrap();
        assert_eq!(replacement.name, name);
        assert!(matches!(Lifetime::reserve(), Err(Error::Exhausted)));
    }
}
