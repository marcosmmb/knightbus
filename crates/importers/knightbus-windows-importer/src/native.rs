//! Windows handle adapter for the unmodified, Microsoft-signed usbip-win UDE driver.
use crate::{
    DriverWake,
    backend::Driver,
    channel::{self, *},
};
use knightbus_core::{Budget, Buffer, DescriptorSet, Error, Result};
use std::{ptr, sync::Arc};
use windows_sys::{
    Win32::{
        Devices::DeviceAndDriverInstallation::*,
        Foundation::*,
        Storage::FileSystem::*,
        System::{IO::*, Threading::*},
    },
    core::GUID,
};

const INTERFACE: GUID = GUID {
    data1: 0xd35f7840,
    data2: 0x6a0c,
    data3: 0x11d2,
    data4: [0xb8, 0x41, 0x00, 0xc0, 0x4f, 0xad, 0x51, 0x71],
};
const MAX_INTERFACE_UNITS: u32 = 16384;
const HARDWARE_ID_UNITS: usize = 512;
const IO_TIMEOUT_MS: u32 = 5000;
const CANCEL_TIMEOUT_MS: u32 = 1000;
const PORT_STATUS_BYTES: usize = 128;
const IOCTL_DEVICE_SHIFT: u32 = 16;
const IOCTL_ACCESS_SHIFT: u32 = 14;
const IOCTL_FUNCTION_SHIFT: u32 = 2;
const BUS_EXTENDER: u32 = 0x2a;
const READ_ACCESS: u32 = 1;
const fn ioctl(function: u32) -> u32 {
    (BUS_EXTENDER << IOCTL_DEVICE_SHIFT)
        | (READ_ACCESS << IOCTL_ACCESS_SHIFT)
        | (function << IOCTL_FUNCTION_SHIFT)
}
const PLUGIN: u32 = ioctl(0);
const SHUTDOWN: u32 = ioctl(2); // Scoped to this handle; never unplug every port.
const PORT_STATUS: u32 = ioctl(3);

struct Event(HANDLE);
// SAFETY: owned event handles can be moved and signaled across threads.
unsafe impl Send for Event {}
impl Drop for Event {
    fn drop(&mut self) {
        unsafe {
            CloseHandle(self.0);
        }
    }
}
enum EventOwner {
    Owned(Event),
    Wake(Arc<DriverWake>),
}
impl EventOwner {
    fn handle(&self) -> HANDLE {
        match self {
            Self::Owned(e) => e.0,
            Self::Wake(w) => w.handle(),
        }
    }
}
struct Io {
    overlapped: Box<OVERLAPPED>,
    buffer: Buffer,
    _event: EventOwner,
    pending: bool,
    ready: Option<usize>,
}
// SAFETY: heap addresses remain stable; one worker owns each I/O operation.
unsafe impl Send for Io {}
impl Io {
    fn new(budget: &Budget, size: usize, wake: Option<Arc<DriverWake>>) -> Result<Self> {
        let event = if let Some(wake) = wake {
            EventOwner::Wake(wake)
        } else {
            // SAFETY: unnamed, non-inherited manual-reset completion event.
            let event = unsafe { CreateEventW(ptr::null(), 1, 0, ptr::null()) };
            if event.is_null() {
                return Err(last_error());
            }
            EventOwner::Owned(Event(event))
        };
        Ok(Self {
            overlapped: Box::new(OVERLAPPED {
                hEvent: event.handle(),
                ..Default::default()
            }),
            buffer: budget.buffer(size)?,
            _event: event,
            pending: false,
            ready: None,
        })
    }
    fn started(&mut self, success: i32, count: u32) -> Result<()> {
        if success != 0 {
            self.ready = Some(count as usize);
            Ok(())
        } else if unsafe { GetLastError() } == ERROR_IO_PENDING {
            self.pending = true;
            Ok(())
        } else {
            Err(last_error())
        }
    }
    fn poll(&mut self, handle: HANDLE, timeout: u32) -> Result<Option<usize>> {
        if let Some(count) = self.ready.take() {
            return Ok(Some(count));
        }
        if !self.pending {
            return Ok(None);
        }
        let mut count = 0;
        // SAFETY: the handle, pinned OVERLAPPED and buffer outlive completion.
        let success =
            unsafe { GetOverlappedResultEx(handle, &*self.overlapped, &mut count, timeout, 0) };
        if success != 0 {
            self.pending = false;
            return Ok(Some(count as usize));
        }
        if matches!(
            unsafe { GetLastError() },
            ERROR_IO_INCOMPLETE | WAIT_TIMEOUT
        ) {
            return Ok(None);
        }
        self.pending = false;
        Err(last_error())
    }
    fn cancel(&mut self, handle: HANDLE) -> Result<()> {
        if !self.pending {
            return Ok(());
        }
        // SAFETY: cancel exactly this operation, then retain storage until terminal.
        unsafe {
            CancelIoEx(handle, &*self.overlapped);
        }
        match self.poll(handle, CANCEL_TIMEOUT_MS) {
            Ok(Some(_)) | Err(Error::Cancelled | Error::NotFound) => Ok(()),
            Err(_) if !self.pending => Ok(()),
            _ => Err(Error::Cleanup),
        }
    }
}

/// One upstream device handle, owned by a single attachment worker.
pub struct Controller {
    handle: HANDLE,
    budget: Option<Budget>,
    reader: Option<Io>,
    read_frame: ReadFrame,
    wake: Option<Arc<DriverWake>>,
    failed: bool,
    attached: bool,
}
// SAFETY: unique handle and pinned buffers move together to one worker.
unsafe impl Send for Controller {}
impl Controller {
    /// Defer native access until descriptor validation and resource admission.
    pub fn owned() -> Self {
        Self {
            handle: INVALID_HANDLE_VALUE,
            budget: None,
            reader: None,
            read_frame: ReadFrame::default(),
            wake: None,
            failed: false,
            attached: false,
        }
    }
    fn transaction(&mut self, code: Option<u32>, input: &[u8], output: usize) -> Result<Vec<u8>> {
        if self.failed {
            return Err(Error::Cleanup);
        }
        let mut io = Io::new(
            self.budget.as_ref().ok_or(Error::State)?,
            input.len().max(output),
            None,
        )?;
        io.buffer.as_mut_slice()[..input.len()].copy_from_slice(input);
        let mut count = 0;
        let bytes = io.buffer.as_mut_slice();
        // SAFETY: private stable storage. METHOD_BUFFERED permits aliased input/output.
        let success = unsafe {
            if let Some(code) = code {
                DeviceIoControl(
                    self.handle,
                    code,
                    bytes.as_ptr().cast(),
                    input.len() as u32,
                    bytes.as_mut_ptr().cast(),
                    output as u32,
                    &mut count,
                    &mut *io.overlapped,
                )
            } else {
                WriteFile(
                    self.handle,
                    bytes.as_ptr(),
                    input.len() as u32,
                    &mut count,
                    &mut *io.overlapped,
                )
            }
        };
        io.started(success, count)?;
        let result = io.poll(self.handle, IO_TIMEOUT_MS);
        match result {
            Ok(Some(count)) => {
                if (code.is_none() && count != input.len()) || (code.is_some() && count > output) {
                    return Err(Error::Invalid);
                }
                Ok(io.buffer.as_slice()[..output.min(count)].to_vec())
            }
            result => {
                if io.cancel(self.handle).is_err() {
                    // An uncooperative driver must not retain a freed user buffer/event.
                    std::mem::forget(io);
                    self.failed = true;
                    return Err(Error::Cleanup);
                }
                Err(result.err().unwrap_or(Error::Deadline))
            }
        }
    }
    fn start_read(&mut self) -> Result<()> {
        let io = self.reader.as_mut().ok_or(Error::State)?;
        if io.pending || io.ready.is_some() {
            return Ok(());
        }
        let mut count = 0;
        let remaining = self.read_frame.remaining();
        // SAFETY: buffer and OVERLAPPED are heap-owned until completion/cancel.
        let success = unsafe {
            ReadFile(
                self.handle,
                io.buffer.as_mut_slice()[remaining.clone()].as_mut_ptr(),
                remaining.len() as u32,
                &mut count,
                &mut *io.overlapped,
            )
        };
        io.started(success, count)?;
        if io.ready.is_some() {
            self.wake.as_ref().unwrap().notify();
        }
        Ok(())
    }
}
impl Driver for Controller {
    fn attach(&mut self, descriptors: &DescriptorSet, budget: &Budget) -> Result<()> {
        if self.handle != INVALID_HANDLE_VALUE {
            return Err(Error::State);
        }
        self.read_frame = ReadFrame::default();
        let plug = channel::plugin(descriptors, budget)?;
        self.budget = Some(budget.clone());
        self.handle = open()?;
        let ports = self.transaction(Some(PORT_STATUS), &[], PORT_STATUS_BYTES)?;
        if ports.len() != PORT_STATUS_BYTES
            || ports[0] == 0
            || usize::from(ports[0]) >= PORT_STATUS_BYTES
        {
            return Err(Error::Version);
        }
        // SHUTDOWN is safe even if plugin fails after allocating a child.
        self.attached = true;
        let reply = self.transaction(Some(PLUGIN), plug.as_slice(), PLUG_SIZE)?;
        if reply.len() != PLUG_SIZE || reply[PLUG_PORT] >= ports[0] {
            return Err(Error::Invalid);
        }
        Ok(())
    }
    fn notifications(&mut self, wake: Arc<DriverWake>) -> Result<()> {
        self.reader = Some(Io::new(
            self.budget.as_ref().ok_or(Error::State)?,
            MAX_MESSAGE,
            Some(wake.clone()),
        )?);
        self.wake = Some(wake);
        self.start_read()
    }
    fn read(&mut self, output: &mut [u8]) -> Result<usize> {
        self.start_read()?;
        let io = self.reader.as_mut().unwrap();
        let Some(count) = io.poll(self.handle, 0)? else {
            return Ok(0);
        };
        let complete = self.read_frame.advance(io.buffer.as_slice(), count)?;
        if let Some(length) = complete {
            output
                .get_mut(..length)
                .ok_or(Error::Invalid)?
                .copy_from_slice(&io.buffer.as_slice()[..length]);
        }
        self.start_read()?;
        Ok(complete.unwrap_or(0))
    }
    fn write(&mut self, input: &[u8]) -> Result<()> {
        self.transaction(None, input, 0).map(|_| ())
    }
    fn detach(&mut self) -> Result<()> {
        if self.handle == INVALID_HANDLE_VALUE {
            return if self.failed {
                Err(Error::Cleanup)
            } else {
                Ok(())
            };
        }
        // Cancel the read before shutdown: the upstream shutdown path also retires
        // pending reads, so these operations must not race one another.
        if let Some(mut io) = self.reader.take()
            && io.cancel(self.handle).is_err()
        {
            std::mem::forget(io);
            self.failed = true;
        }
        if self.attached && self.transaction(Some(SHUTDOWN), &[], 0).is_err() {
            self.failed = true;
        }
        unsafe {
            CloseHandle(self.handle);
        }
        self.handle = INVALID_HANDLE_VALUE;
        if self.failed {
            Err(Error::Cleanup)
        } else {
            Ok(())
        }
    }
}
impl Drop for Controller {
    fn drop(&mut self) {
        let _ = self.detach();
    }
}

fn path() -> Result<Vec<u16>> {
    struct Info(HDEVINFO);
    impl Drop for Info {
        fn drop(&mut self) {
            unsafe {
                SetupDiDestroyDeviceInfoList(self.0);
            }
        }
    }
    // SAFETY: owned SetupAPI set; every variable output is bounded before use.
    unsafe {
        let set = SetupDiGetClassDevsW(
            &INTERFACE,
            ptr::null(),
            ptr::null_mut(),
            DIGCF_DEVICEINTERFACE | DIGCF_PRESENT,
        );
        if set == INVALID_HANDLE_VALUE as HDEVINFO {
            return Err(last_error());
        }
        let set = Info(set);
        let mut result = None;
        let mut index = 0;
        loop {
            let mut interface = SP_DEVICE_INTERFACE_DATA {
                cbSize: size_of::<SP_DEVICE_INTERFACE_DATA>() as u32,
                ..Default::default()
            };
            if SetupDiEnumDeviceInterfaces(set.0, ptr::null(), &INTERFACE, index, &mut interface)
                == 0
            {
                if GetLastError() == ERROR_NO_MORE_ITEMS {
                    break;
                }
                return Err(last_error());
            }
            index += 1;
            let mut count = 0;
            let mut device = SP_DEVINFO_DATA {
                cbSize: size_of::<SP_DEVINFO_DATA>() as u32,
                ..Default::default()
            };
            SetupDiGetDeviceInterfaceDetailW(
                set.0,
                &interface,
                ptr::null_mut(),
                0,
                &mut count,
                &mut device,
            );
            if count < size_of::<SP_DEVICE_INTERFACE_DETAIL_DATA_W>() as u32
                || count > MAX_INTERFACE_UNITS * size_of::<u16>() as u32
            {
                return Err(Error::Invalid);
            }
            // usize storage provides the alignment required by the native structure.
            let mut detail = vec![0usize; (count as usize).div_ceil(size_of::<usize>())];
            let pointer = detail
                .as_mut_ptr()
                .cast::<SP_DEVICE_INTERFACE_DETAIL_DATA_W>();
            (*pointer).cbSize = size_of::<SP_DEVICE_INTERFACE_DETAIL_DATA_W>() as u32;
            if SetupDiGetDeviceInterfaceDetailW(
                set.0,
                &interface,
                pointer,
                count,
                &mut count,
                &mut device,
            ) == 0
            {
                return Err(last_error());
            }
            let mut hardware = [0u16; HARDWARE_ID_UNITS];
            let mut kind = 0;
            if SetupDiGetDeviceRegistryPropertyW(
                set.0,
                &device,
                SPDRP_HARDWAREID,
                &mut kind,
                hardware.as_mut_ptr().cast(),
                size_of_val(&hardware) as u32,
                ptr::null_mut(),
            ) == 0
            {
                return Err(last_error());
            }
            if !hardware
                .split(|c| *c == 0)
                .any(|id| String::from_utf16_lossy(id).eq_ignore_ascii_case("ROOT\\VHCI_ude"))
            {
                continue;
            }
            let path_offset = std::mem::offset_of!(SP_DEVICE_INTERFACE_DETAIL_DATA_W, DevicePath);
            let units = (count as usize - path_offset) / size_of::<u16>();
            let path = std::slice::from_raw_parts((*pointer).DevicePath.as_ptr(), units);
            let end = path.iter().position(|c| *c == 0).ok_or(Error::Invalid)?;
            if end == 0 || result.is_some() {
                return Err(Error::State);
            }
            result = Some(path[..=end].to_vec());
        }
        result.ok_or(Error::NotFound)
    }
}
fn open() -> Result<HANDLE> {
    let path = path()?;
    // SAFETY: validated terminated PnP path; exclusive per-attachment overlapped file.
    let handle = unsafe {
        CreateFileW(
            path.as_ptr(),
            GENERIC_READ | GENERIC_WRITE,
            0,
            ptr::null(),
            OPEN_EXISTING,
            FILE_FLAG_OVERLAPPED,
            ptr::null_mut(),
        )
    };
    if handle == INVALID_HANDLE_VALUE {
        Err(last_error())
    } else {
        Ok(handle)
    }
}
/// Check for a running UDE controller without attaching or changing a device.
pub fn driver_staged() -> Result<bool> {
    match path() {
        Ok(_) => Ok(true),
        Err(Error::NotFound) => Ok(false),
        Err(e) => Err(e),
    }
}
fn last_error() -> Error {
    match unsafe { GetLastError() } {
        ERROR_ACCESS_DENIED => Error::Denied,
        ERROR_NOT_FOUND | ERROR_FILE_NOT_FOUND | ERROR_DEVICE_NOT_CONNECTED => Error::NotFound,
        ERROR_OPERATION_ABORTED => Error::Cancelled,
        ERROR_NOT_ENOUGH_MEMORY => Error::Exhausted,
        ERROR_INVALID_PARAMETER | ERROR_INSUFFICIENT_BUFFER | ERROR_INVALID_USER_BUFFER => {
            Error::Invalid
        }
        ERROR_TIMEOUT | ERROR_SEM_TIMEOUT => Error::Deadline,
        _ => Error::Unavailable,
    }
}
