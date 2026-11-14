//! Windows FFI boundary. All IOCTLs are synchronous on the dedicated worker;
//! buffers cannot escape a completed call, and only that worker owns the handle.
mod lifetime;
use crate::{backend::Driver, channel};
use knightbus_core::{Error, Result};
use std::ptr;
use windows_sys::{
    Win32::{
        Devices::DeviceAndDriverInstallation::*, Foundation::*, Storage::FileSystem::*,
        System::IO::*,
    },
    core::GUID,
};
const INTERFACE: GUID = GUID {
    data1: 0x53baf142,
    data2: 0xc628,
    data3: 0x47de,
    data4: [0xb8, 0x91, 0x5c, 0x71, 0xe3, 0x8b, 0x96, 0x20],
};
const MAX_INSTANCE_UNITS: usize = 4096;
const MAX_INTERFACE_LIST_UNITS: u32 = 16384;
const MULTI_SZ_TERMINATORS: u32 = 2;
const LIFECYCLE_COUNTERS: usize = 16;
const LIFECYCLE_BYTES: usize = LIFECYCLE_COUNTERS * size_of::<u64>();
/// Exclusive native channel, opened only by an authorized privileged host.
pub struct Controller {
    handle: HANDLE,
    owned: bool,
    lifetime: Option<lifetime::Lifetime>,
    retired: bool,
    cleanup_failed: bool,
}
// SAFETY: Unique handle ownership moves to one worker; no references, buffers or
// callbacks cross the move. This type is deliberately not Clone or Sync.
unsafe impl Send for Controller {}
impl Drop for Controller {
    fn drop(&mut self) {
        if self.owned && !self.retired {
            let _ = self.detach();
        }
        self.close_channel();
    }
}
impl Controller {
    /// Prepare a controller owned by one attachment. The signed driver package
    /// must already be staged. Creation occurs on the worker during `attach`,
    /// after native memory admission, and `detach` retires the entire controller.
    pub fn owned() -> Self {
        Self {
            handle: INVALID_HANDLE_VALUE,
            owned: true,
            lifetime: None,
            retired: false,
            cleanup_failed: false,
        }
    }
    fn close_channel(&mut self) {
        if self.handle != INVALID_HANDLE_VALUE {
            // SAFETY: Exclusive synchronous file, never inherited or aliased.
            unsafe {
                CloseHandle(self.handle);
            }
            self.handle = INVALID_HANDLE_VALUE;
        }
    }
    /// Open an installed KnightBus controller. A device instance ID selects one
    /// controller when several exist; omission requires exactly one match.
    pub fn open(instance: Option<&str>) -> Result<Self> {
        let id: Option<Vec<u16>> = instance.map(|s| s.encode_utf16().chain([0]).collect());
        if id
            .as_ref()
            .is_some_and(|v| v.len() > MAX_INSTANCE_UNITS || v[..v.len() - 1].contains(&0))
        {
            return Err(Error::Invalid);
        }
        let filter = id.as_ref().map_or(ptr::null(), |v| v.as_ptr());
        let mut count = 0;
        // SAFETY: Static GUID, optional terminated instance string, local output.
        let status = unsafe {
            CM_Get_Device_Interface_List_SizeW(
                &mut count,
                &INTERFACE,
                filter,
                CM_GET_DEVICE_INTERFACE_LIST_PRESENT,
            )
        };
        if status != CR_SUCCESS
            || !(MULTI_SZ_TERMINATORS..=MAX_INTERFACE_LIST_UNITS).contains(&count)
        {
            return Err(Error::Unavailable);
        }
        let mut paths = vec![0u16; count as usize];
        // SAFETY: Allocated capacity equals the checked OS size; no retention.
        let status = unsafe {
            CM_Get_Device_Interface_ListW(
                &INTERFACE,
                filter,
                paths.as_mut_ptr(),
                count,
                CM_GET_DEVICE_INTERFACE_LIST_PRESENT,
            )
        };
        if status != CR_SUCCESS {
            return Err(Error::Unavailable);
        }
        let mut found = paths.split(|c| *c == 0).filter(|p| !p.is_empty());
        let path = found.next().ok_or(Error::NotFound)?;
        if found.next().is_some() {
            return Err(Error::Invalid);
        }
        let path: Vec<u16> = path.iter().copied().chain([0]).collect();
        // SAFETY: Existing validated OS path, synchronous noninheritable handle.
        let handle = unsafe {
            CreateFileW(
                path.as_ptr(),
                GENERIC_READ | GENERIC_WRITE,
                FILE_SHARE_READ | FILE_SHARE_WRITE,
                ptr::null(),
                OPEN_EXISTING,
                0,
                ptr::null_mut(),
            )
        };
        if handle == INVALID_HANDLE_VALUE {
            Err(last_error())
        } else {
            Ok(Self {
                handle,
                owned: false,
                lifetime: None,
                retired: false,
                cleanup_failed: false,
            })
        }
    }
    fn ioctl(&mut self, code: u32, input: &[u8], output: &mut [u8]) -> Result<usize> {
        if input.len() > channel::MAX_TRANSFER + channel::HEADER
            || output.len() > channel::MAX_TRANSFER + channel::HEADER
        {
            return Err(Error::Exhausted);
        }
        let mut done = 0;
        // SAFETY: Synchronous exclusive handle; slices stay valid until terminal
        // return. METHOD_BUFFERED copies user buffers; no kernel pointer crosses.
        if unsafe {
            DeviceIoControl(
                self.handle,
                code,
                if input.is_empty() {
                    ptr::null()
                } else {
                    input.as_ptr().cast()
                },
                input.len() as u32,
                if output.is_empty() {
                    ptr::null_mut()
                } else {
                    output.as_mut_ptr().cast()
                },
                output.len() as u32,
                &mut done,
                ptr::null_mut(),
            )
        } == 0
        {
            return Err(last_error());
        }
        if done as usize > output.len() {
            return Err(Error::Invalid);
        }
        Ok(done as usize)
    }
    /// Read owned-object lifecycle totals for hardware regression tests.
    /// These counters do not replace debugger measurement of total WDF objects.
    pub fn lifecycle(&mut self) -> Result<[u64; LIFECYCLE_COUNTERS]> {
        let mut bytes = [0u8; LIFECYCLE_BYTES];
        if self.ioctl(channel::LIFECYCLE, &[], &mut bytes)? != LIFECYCLE_BYTES {
            return Err(Error::Invalid);
        }
        Ok(std::array::from_fn(|i| {
            channel::u64_at(&bytes, i * size_of::<u64>())
        }))
    }
}
impl Driver for Controller {
    fn attach(&mut self, bytes: &[u8]) -> Result<()> {
        if self.owned {
            if self.retired || self.lifetime.is_some() {
                return Err(Error::State);
            }
            self.lifetime = Some(lifetime::Lifetime::reserve()?);
            let instance = self.lifetime.as_mut().unwrap().start()?;
            let mut channel = Self::open(Some(&instance))?;
            self.handle = std::mem::replace(&mut channel.handle, INVALID_HANDLE_VALUE);
            // Refuse an older driver before publishing USB: the owned path
            // requires a kernel-held event and final-destruction acknowledgement.
            self.ioctl(channel::RETIREMENT_READY, &[], &mut [])?;
        }
        self.ioctl(channel::ATTACH, bytes, &mut []).map(|_| ())
    }
    fn fetch(&mut self, mask: u32, output: &mut [u8]) -> Result<usize> {
        self.ioctl(channel::FETCH, &mask.to_le_bytes(), output)
    }
    fn reply(&mut self, input: &[u8]) -> Result<()> {
        match self.ioctl(channel::REPLY, input, &mut []) {
            Ok(_) | Err(Error::NotFound | Error::Cancelled) => Ok(()),
            result => result.map(|_| ()),
        }
    }
    fn snapshot(&mut self, output: &mut [u8]) -> Result<()> {
        if self.ioctl(channel::STATS, &[], output)? != channel::STATS_BYTES {
            return Err(Error::Invalid);
        }
        Ok(())
    }
    fn detach(&mut self) -> Result<()> {
        if !self.owned {
            return self.ioctl(channel::DETACH, &[], &mut []).map(|_| ());
        }
        if self.retired {
            return if !self.cleanup_failed {
                Ok(())
            } else {
                Err(Error::Cleanup)
            };
        }
        if self.handle != INVALID_HANDLE_VALUE {
            let _ = self.ioctl(channel::DETACH, &[], &mut []);
            self.close_channel();
        }
        self.retired = true;
        if let Some(mut lifetime) = self.lifetime.take() {
            if lifetime.unplug().is_err() {
                // Keep the slot reserved after unproven retirement. The worker
                // separately retains its native budget. Process death closes
                // user handles; the kernel keeps the slot until final destruction.
                // Never assign this lifetime to another attachment.
                self.cleanup_failed = true;
                std::mem::forget(lifetime);
                return Err(Error::Cleanup);
            }
        }
        Ok(())
    }
}
fn last_error() -> Error {
    // SAFETY: Thread-local Win32 error immediately after failing native call.
    match unsafe { GetLastError() } {
        ERROR_ACCESS_DENIED => Error::Denied,
        ERROR_NOT_FOUND => Error::NotFound,
        ERROR_OPERATION_ABORTED => Error::Cancelled,
        ERROR_NOT_ENOUGH_MEMORY => Error::Exhausted,
        // STATUS_INVALID_BUFFER_SIZE from the driver's exact-length checks
        // maps to ERROR_INVALID_USER_BUFFER, not ERROR_INSUFFICIENT_BUFFER.
        ERROR_INVALID_PARAMETER | ERROR_INSUFFICIENT_BUFFER | ERROR_INVALID_USER_BUFFER => {
            Error::Invalid
        }
        ERROR_TIMEOUT | ERROR_SEM_TIMEOUT => Error::Deadline,
        _ => Error::Unavailable,
    }
}
