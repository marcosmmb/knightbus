//! Signed usbip-win2 controller and a bounded, private loopback USB/IP adapter.
use crate::{
    DriverWake,
    backend::Driver,
    channel::{MAX_MESSAGE, ReadFrame},
    driver_abi as abi,
};
use knightbus_core::{Budget, Buffer, DescriptorSet, Error, Result, topology as usb, usbip};
use std::{
    io::{Read, Write},
    net::{Shutdown, TcpListener, TcpStream},
    os::windows::io::AsRawSocket,
    ptr,
    sync::Arc,
    time::{Duration, Instant},
};
use windows_sys::{
    Win32::{
        Devices::DeviceAndDriverInstallation::*,
        Foundation::*,
        Networking::WinSock::*,
        Security::Cryptography::*,
        Storage::FileSystem::*,
        System::{IO::*, Registry::*, Threading::*},
    },
    core::{GUID, w},
};

const INTERFACE: GUID = GUID {
    data1: 0xb4030c06,
    data2: 0xdc5f,
    data3: 0x4fcc,
    data4: [0x87, 0xeb, 0xe5, 0x51, 0x5a, 0x09, 0x35, 0xc0],
};
const MAX_INTERFACE_UNITS: u32 = 16384;
const HARDWARE_ID_UNITS: usize = 512;
const IO_TIMEOUT_MS: u32 = 5000;
const CANCEL_TIMEOUT_MS: u32 = 1000;
const POLL_INTERVAL: Duration = Duration::from_millis(2);
const NONCE_BYTES: usize = 15; // 120 random bits, hex fits the driver's bus ID.
const TIMEOUT: Duration = Duration::from_millis(IO_TIMEOUT_MS as u64);

struct Handle(HANDLE);
// SAFETY: Windows handles are movable; each operation retains its own storage.
unsafe impl Send for Handle {}
impl Drop for Handle {
    fn drop(&mut self) {
        unsafe {
            CloseHandle(self.0);
        }
    }
}
struct Admission(Handle);
impl Admission {
    fn acquire() -> Result<Self> {
        // Serialize admission across KnightBus processes; the driver also bounds
        // allocation under its own lock, including callers outside KnightBus.
        let handle =
            unsafe { CreateMutexW(ptr::null(), 0, w!("Global\\KnightBusUsbipWin2Admission")) };
        if handle.is_null() {
            return Err(last_error());
        }
        let handle = Handle(handle);
        match unsafe { WaitForSingleObject(handle.0, IO_TIMEOUT_MS) } {
            WAIT_OBJECT_0 | WAIT_ABANDONED => Ok(Self(handle)),
            WAIT_TIMEOUT => Err(Error::Deadline),
            _ => Err(last_error()),
        }
    }
}
impl Drop for Admission {
    fn drop(&mut self) {
        unsafe {
            ReleaseMutex(self.0.0);
        }
    }
}
struct Io {
    overlapped: Box<OVERLAPPED>,
    buffer: Buffer,
    _event: Handle,
    pending: bool,
    ready: Option<usize>,
}
impl Io {
    fn start(
        handle: HANDLE,
        budget: &Budget,
        code: u32,
        input: &[u8],
        output: usize,
    ) -> Result<Self> {
        let event = unsafe { CreateEventW(ptr::null(), 1, 0, ptr::null()) };
        if event.is_null() {
            return Err(last_error());
        }
        let event = Handle(event);
        let mut io = Self {
            overlapped: Box::new(OVERLAPPED {
                hEvent: event.0,
                ..Default::default()
            }),
            buffer: budget.buffer(input.len().max(output))?,
            _event: event,
            pending: false,
            ready: None,
        };
        io.buffer.as_mut_slice()[..input.len()].copy_from_slice(input);
        let mut count = 0;
        let bytes = io.buffer.as_mut_slice();
        // SAFETY: METHOD_BUFFERED accepts shared input/output; storage is retained
        // until the overlapped request completes or cancellation is acknowledged.
        let success = unsafe {
            DeviceIoControl(
                handle,
                code,
                bytes.as_ptr().cast(),
                input.len() as u32,
                bytes.as_mut_ptr().cast(),
                output as u32,
                &mut count,
                &mut *io.overlapped,
            )
        };
        if success != 0 {
            io.ready = Some(count as usize);
        } else if unsafe { GetLastError() } == ERROR_IO_PENDING {
            io.pending = true;
        } else {
            return Err(last_error());
        }
        Ok(io)
    }
    fn poll(&mut self, handle: HANDLE, timeout: u32) -> Result<Option<usize>> {
        if let Some(count) = self.ready.take() {
            return Ok(Some(count));
        }
        if !self.pending {
            return Ok(None);
        }
        let mut count = 0;
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
        unsafe {
            CancelIoEx(handle, &*self.overlapped);
        }
        let result = self.poll(handle, CANCEL_TIMEOUT_MS);
        if self.pending {
            Err(Error::Cleanup)
        } else {
            result.map(|_| ()).or(Ok(()))
        }
    }
}

/// One attachment's private listener, stream and controller handle.
pub struct Controller {
    handle: Option<Handle>,
    budget: Option<Budget>,
    stream: Option<TcpStream>,
    listener: Option<TcpListener>,
    reader: Option<Buffer>,
    frame: ReadFrame,
    wake: Option<Arc<DriverWake>>,
    location: Option<[u8; abi::LOCATION_BYTES]>,
    failed: bool,
}
impl Controller {
    /// Native access starts only after core descriptor and budget admission.
    pub fn owned() -> Self {
        Self {
            handle: None,
            budget: None,
            stream: None,
            listener: None,
            reader: None,
            frame: ReadFrame::default(),
            wake: None,
            location: None,
            failed: false,
        }
    }
    fn finish(&mut self, mut io: Io, deadline: Instant) -> Result<Vec<u8>> {
        let handle = self.handle.as_ref().ok_or(Error::State)?.0;
        let wait = deadline
            .saturating_duration_since(Instant::now())
            .as_millis()
            .min(IO_TIMEOUT_MS as u128) as u32;
        match io.poll(handle, wait) {
            Ok(Some(count)) if count <= io.buffer.as_slice().len() => {
                Ok(io.buffer.as_slice()[..count].to_vec())
            }
            result => {
                if io.cancel(handle).is_err() {
                    // The OS still owns these addresses. Retain them on a broken
                    // cancellation path and prevent reporting successful cleanup.
                    std::mem::forget(io);
                    self.failed = true;
                    return Err(Error::Cleanup);
                }
                Err(result.err().unwrap_or(Error::Deadline))
            }
        }
    }
    fn transaction(&mut self, code: u32, input: &[u8], output: usize) -> Result<Vec<u8>> {
        if self.failed {
            return Err(Error::Cleanup);
        }
        let io = Io::start(
            self.handle.as_ref().ok_or(Error::State)?.0,
            self.budget.as_ref().ok_or(Error::State)?,
            code,
            input,
            output,
        )?;
        self.finish(io, Instant::now() + TIMEOUT)
    }
    fn devices(&mut self) -> Result<Vec<u8>> {
        let mut input = [0; abi::LIST_HEADER_BYTES];
        abi::put(&mut input, abi::SIZE_OFFSET, abi::LIST_REQUEST_BYTES as u32);
        let bytes = self.transaction(abi::LIST, &input, abi::LIST_BYTES)?;
        let _ = abi::devices(&bytes)?;
        Ok(bytes)
    }
    fn bootstrap(
        &mut self,
        listener: &TcpListener,
        bus: &str,
        descriptors: &DescriptorSet,
        deadline: Instant,
    ) -> Result<()> {
        let mut stream = loop {
            match listener.accept() {
                Ok((s, peer)) if peer.ip().is_loopback() => break s,
                Ok(_) => return Err(Error::Denied),
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => pause(deadline)?,
                Err(_) => return Err(Error::Unavailable),
            }
        };
        stream
            .set_nonblocking(true)
            .map_err(|_| Error::Unavailable)?;
        stream.set_nodelay(true).map_err(|_| Error::Unavailable)?;
        let mut request = [0; usbip::IMPORT_REQUEST_BYTES];
        read_exact_until(&mut stream, &mut request, deadline)?;
        let expected = usbip::Message::ImportRequest(
            usbip::WireString::new(bus.as_bytes()).map_err(|_| Error::Invalid)?,
        );
        let (message, used) = usbip::decode(
            &request,
            usbip::Context::Management,
            usbip::Limits::default(),
            &mut |_| Ok(()),
        )
        .map_err(|_| Error::Invalid)?;
        if message != expected || used != request.len() {
            return Err(Error::Denied);
        }
        let device = descriptors.device();
        let config = descriptors.configuration();
        let reply = usbip::Message::ImportReply {
            status: 0,
            device: Some(Box::new(usbip::Device {
                path: usbip::WireString::new(b"knightbus").map_err(|_| Error::Invalid)?,
                bus_id: usbip::WireString::new(bus.as_bytes()).map_err(|_| Error::Invalid)?,
                bus_number: 0,
                device_number: crate::channel::LOCAL_DEVICE_ID,
                speed: descriptors.speed() as u32,
                vendor_id: usb::u16_at(device, usb::device::VENDOR),
                product_id: usb::u16_at(device, usb::device::PRODUCT),
                device_bcd: usb::u16_at(device, usb::device::BCD),
                class: device[usb::device::CLASS],
                subclass: device[usb::device::SUBCLASS],
                protocol: device[usb::device::PROTOCOL],
                configuration: config[usb::configuration::VALUE],
                configurations: device[usb::device::CONFIGURATIONS],
                interfaces: config[usb::configuration::INTERFACES],
            })),
        };
        let reply = self
            .budget
            .as_ref()
            .unwrap()
            .encode(&reply, usbip::Limits::default())
            .map_err(|_| Error::Exhausted)?;
        write_all_until(&mut stream, reply.as_slice(), deadline)?;
        self.stream = Some(stream);
        Ok(())
    }
}
impl Driver for Controller {
    fn attach(&mut self, descriptors: &DescriptorSet, budget: &Budget) -> Result<()> {
        if self.handle.is_some() {
            return Err(Error::State);
        }
        crate::backend::CAPABILITIES.descriptors(descriptors)?;
        let _admission = Admission::acquire()?;
        self.handle = Some(Handle(open()?));
        self.budget = Some(budget.clone());
        let devices = self.devices()?;
        abi::admit(
            &devices,
            descriptors.speed(),
            port_capacity(descriptors.speed())?,
        )?;
        let listener = TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0))
            .map_err(|_| Error::Unavailable)?;
        listener
            .set_nonblocking(true)
            .map_err(|_| Error::Unavailable)?;
        let mut nonce = [0; NONCE_BYTES];
        if unsafe {
            BCryptGenRandom(
                ptr::null_mut(),
                nonce.as_mut_ptr(),
                nonce.len() as u32,
                BCRYPT_USE_SYSTEM_PREFERRED_RNG,
            )
        } < 0
        {
            return Err(Error::Unavailable);
        }
        let bus: String = nonce.iter().map(|b| format!("{b:02x}")).collect();
        let service = listener
            .local_addr()
            .map_err(|_| Error::Unavailable)?
            .port()
            .to_string();
        let location = abi::location(&bus, &service)?;
        let request = abi::request(&location, abi::PLUGIN_BYTES);
        self.location = Some(location);
        let deadline = Instant::now() + TIMEOUT;
        let io = Io::start(
            self.handle.as_ref().unwrap().0,
            budget,
            abi::PLUGIN,
            &request,
            abi::PLUGIN_BYTES,
        )?;
        let bootstrap = self.bootstrap(&listener, &bus, descriptors, deadline);
        // Keep the ephemeral port reserved until native cleanup finishes.
        self.listener = Some(listener);
        // Always retire the IOCTL, even if the local handshake failed.
        let result = self.finish(io, deadline);
        bootstrap?;
        let reply = result?;
        if reply.len() != abi::PLUGIN_REPLY_BYTES
            || abi::word(&reply, abi::REQUEST_PORT_OFFSET) == 0
            || abi::word(&reply, abi::REQUEST_PORT_OFFSET) as usize > abi::MAX_PORTS
        {
            return Err(Error::Invalid);
        }
        Ok(())
    }
    fn notifications(&mut self, wake: Arc<DriverWake>) -> Result<()> {
        self.reader = Some(
            self.budget
                .as_ref()
                .ok_or(Error::State)?
                .buffer(MAX_MESSAGE)?,
        );
        let socket = self.stream.as_ref().ok_or(Error::State)?.as_raw_socket() as SOCKET;
        // WSAEventSelect keeps this socket nonblocking; the worker owns reads.
        if unsafe {
            WSAEventSelect(
                socket,
                wake.handle() as WSAEVENT,
                (FD_READ | FD_CLOSE) as i32,
            )
        } != 0
        {
            return Err(Error::Unavailable);
        }
        self.wake = Some(wake);
        Ok(())
    }
    fn read(&mut self, output: &mut [u8]) -> Result<usize> {
        let stream = self.stream.as_mut().ok_or(Error::State)?;
        let mut events = WSANETWORKEVENTS::default();
        if unsafe {
            WSAEnumNetworkEvents(
                stream.as_raw_socket() as SOCKET,
                self.wake.as_ref().ok_or(Error::State)?.handle() as WSAEVENT,
                &mut events,
            )
        } != 0
        {
            return Err(Error::Unavailable);
        }
        let buffer = self.reader.as_mut().ok_or(Error::State)?;
        loop {
            match stream.read(&mut buffer.as_mut_slice()[self.frame.remaining()]) {
                Ok(0) => return Err(Error::NotFound),
                Ok(count) => {
                    if let Some(length) = self.frame.advance(buffer.as_slice(), count)? {
                        output
                            .get_mut(..length)
                            .ok_or(Error::Invalid)?
                            .copy_from_slice(&buffer.as_slice()[..length]);
                        return Ok(length);
                    }
                }
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => return Ok(0),
                Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
                Err(_) => return Err(Error::Unavailable),
            }
        }
    }
    fn write(&mut self, input: &[u8]) -> Result<()> {
        write_all_until(
            self.stream.as_mut().ok_or(Error::State)?,
            input,
            Instant::now() + TIMEOUT,
        )
    }
    fn detach(&mut self) -> Result<()> {
        if self.failed {
            return Err(Error::Cleanup);
        }
        if let Some(location) = self.location {
            let _admission = Admission::acquire()?;
            let devices = self.devices()?;
            // Match the whole private location before using a global port number.
            if let Some(device) = abi::devices(&devices)?.find(|d| abi::same_location(d, &location))
            {
                let mut request = [0; abi::PLUGOUT_BYTES];
                abi::put(&mut request, abi::SIZE_OFFSET, abi::PLUGOUT_BYTES as u32);
                abi::put(
                    &mut request,
                    abi::REQUEST_PORT_OFFSET,
                    abi::word(device, abi::PORT_OFFSET),
                );
                self.transaction(abi::PLUGOUT, &request, 0)?;
            }
            if let Some(stream) = self.stream.take() {
                let _ = stream.shutdown(Shutdown::Both);
            }
            let stop = abi::request(&location, abi::STOP_BYTES);
            self.transaction(abi::STOP, &stop, abi::STOP_BYTES)?;
            let devices = self.devices()?;
            if abi::devices(&devices)?.any(|d| abi::same_location(d, &location)) {
                return Err(Error::Cleanup);
            }
            self.location = None;
        }
        self.listener = None;
        self.reader = None;
        self.wake = None;
        self.handle = None;
        self.budget = None;
        Ok(())
    }
}
impl Drop for Controller {
    fn drop(&mut self) {
        let _ = self.detach();
    }
}
fn pause(deadline: Instant) -> Result<()> {
    let remaining = deadline
        .checked_duration_since(Instant::now())
        .ok_or(Error::Deadline)?;
    std::thread::sleep(POLL_INTERVAL.min(remaining));
    Ok(())
}
fn read_exact_until(
    stream: &mut TcpStream,
    mut output: &mut [u8],
    deadline: Instant,
) -> Result<()> {
    while !output.is_empty() {
        if Instant::now() >= deadline {
            return Err(Error::Deadline);
        }
        match stream.read(output) {
            Ok(0) => return Err(Error::NotFound),
            Ok(n) => output = &mut output[n..],
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => pause(deadline)?,
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {}
            Err(_) => return Err(Error::Unavailable),
        }
    }
    Ok(())
}
fn write_all_until(stream: &mut TcpStream, mut input: &[u8], deadline: Instant) -> Result<()> {
    while !input.is_empty() {
        if Instant::now() >= deadline {
            return Err(Error::Deadline);
        }
        match stream.write(input) {
            Ok(0) => return Err(Error::NotFound),
            Ok(n) => input = &input[n..],
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => pause(deadline)?,
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {}
            Err(_) => return Err(Error::Unavailable),
        }
    }
    Ok(())
}
fn port_capacity(speed: usb::Speed) -> Result<usize> {
    fn configured(name: *const u16) -> Result<usize> {
        let mut value = abi::DEFAULT_CLASS_PORTS as u32;
        let mut size = size_of_val(&value) as u32;
        let status = unsafe {
            RegGetValueW(
                HKEY_LOCAL_MACHINE,
                w!("SYSTEM\\CurrentControlSet\\Services\\usbip2_ude\\Parameters"),
                name,
                RRF_RT_REG_DWORD,
                ptr::null_mut(),
                (&mut value as *mut u32).cast(),
                &mut size,
            )
        };
        if !matches!(
            status,
            ERROR_SUCCESS | ERROR_FILE_NOT_FOUND | ERROR_PATH_NOT_FOUND
        ) {
            return Err(Error::Unavailable);
        }
        // Upstream clamps signed registry values and assigns USB 3 ports first.
        Ok((value as i32).clamp(abi::MIN_CLASS_PORTS as i32, abi::MAX_CLASS_PORTS as i32) as usize)
    }
    let superspeed = configured(w!("NumberOfUsb30Ports"))?;
    if speed as u32 >= usb::Speed::Super as u32 {
        Ok(superspeed)
    } else {
        Ok(configured(w!("NumberOfUsb20Ports"))?.min(abi::MAX_PORTS - superspeed))
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
            if !hardware.split(|c| *c == 0).any(|id| {
                String::from_utf16_lossy(id).eq_ignore_ascii_case("ROOT\\USBIP_WIN2\\UDE")
            }) {
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
    // SAFETY: validated terminated PnP path; shared controller with per-operation overlapped storage.
    let handle = unsafe {
        CreateFileW(
            path.as_ptr(),
            GENERIC_READ | GENERIC_WRITE,
            FILE_SHARE_READ | FILE_SHARE_WRITE,
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
