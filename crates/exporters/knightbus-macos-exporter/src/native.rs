//! Apple IOUSBHost bindings. All Objective-C owners are confined to one worker.
//! No forwarding implementation is imported: objc2 supplies ordinary API/ABI
//! bindings. Native storage is retained until the terminal callback publishes
//! with release ordering; only then may Rust inspect or free the buffer.
use crate::constants::{limits, status, usb};
use crate::{Driver, MAX_ISO, MAX_TRANSFER, SLOTS, SubmitError, descriptors, journal};
use block2::RcBlock;
use knightbus_core::{
    Budget, Completion, Device, DeviceId, DeviceKey, Error, MemoryReservation, Result, Transfer,
    TransferId, TransferKind, usbip::Direction,
};
use objc2::{AnyThread, rc::Retained};
use objc2_foundation::{NSError, NSMutableData};
use objc2_io_kit::{IOUSBDeviceRequest, tIOUSBHostConnectionSpeed as HostSpeed};
use objc2_io_usb_host::*;
use std::{
    collections::BTreeMap,
    ffi::{c_char, c_void},
    ptr::{self, NonNull},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicU32, AtomicUsize, Ordering},
    },
    task::Waker,
    time::Instant,
};

// mach/message.h: user-client destination port no longer exists.
const MACH_SEND_INVALID_DEST: u32 = 0x10000003;
// IOUSBHostFamilyDefinitions.h kUSBHostReturnPipeStalled.
const USB_HOST_PIPE_STALLED: u32 = 0xe0005000;
// CoreFoundation kCFStringEncodingUTF8.
const CF_STRING_ENCODING_UTF8: u32 = 0x08000100;
// CoreFoundation kCFNumberSInt64Type.
const CF_NUMBER_SINT64: i32 = 4;

#[link(name = "IOKit", kind = "framework")]
unsafe extern "C" {
    fn IOServiceMatching(name: *const c_char) -> *mut c_void;
    fn IOServiceGetMatchingServices(port: u32, matching: *mut c_void, iterator: *mut u32) -> i32;
    fn IOServiceGetMatchingService(port: u32, matching: *mut c_void) -> u32;
    fn IORegistryEntryIDMatching(id: u64) -> *mut c_void;
    fn IOIteratorNext(iterator: u32) -> u32;
    fn IOObjectRelease(object: u32) -> i32;
    fn IORegistryEntryGetRegistryEntryID(entry: u32, id: *mut u64) -> i32;
    fn IORegistryEntryGetChildIterator(entry: u32, plane: *const c_char, iterator: *mut u32)
    -> i32;
    fn IOObjectConformsTo(object: u32, class: *const c_char) -> i32;
    fn IORegistryEntryCreateCFProperty(
        entry: u32,
        key: *const c_void,
        allocator: *const c_void,
        options: u32,
    ) -> *const c_void;
}
#[link(name = "CoreFoundation", kind = "framework")]
unsafe extern "C" {
    fn CFStringCreateWithCString(
        allocator: *const c_void,
        bytes: *const c_char,
        encoding: u32,
    ) -> *const c_void;
    fn CFRelease(value: *const c_void);
    fn CFGetTypeID(value: *const c_void) -> usize;
    fn CFNumberGetTypeID() -> usize;
    fn CFNumberGetValue(value: *const c_void, kind: i32, target: *mut i64) -> bool;
}
struct Service(u32);
impl Drop for Service {
    fn drop(&mut self) {
        if self.0 != 0 {
            unsafe {
                IOObjectRelease(self.0);
            }
        }
    }
}
fn property(entry: u32, key: &'static std::ffi::CStr) -> Option<u64> {
    // SAFETY: retained CF values, checked dynamic type, fixed UTF-8 keys.
    unsafe {
        let name = CFStringCreateWithCString(ptr::null(), key.as_ptr(), CF_STRING_ENCODING_UTF8);
        if name.is_null() {
            return None;
        }
        let value = IORegistryEntryCreateCFProperty(entry, name, ptr::null(), 0);
        CFRelease(name);
        if value.is_null() {
            return None;
        }
        let mut n = 0i64;
        let valid = CFGetTypeID(value) == CFNumberGetTypeID()
            && CFNumberGetValue(value, CF_NUMBER_SINT64, &mut n);
        CFRelease(value);
        if valid { u64::try_from(n).ok() } else { None }
    }
}

/// Physical selector. Registry ID is an incarnation token, not an authorization
/// identity. Location is inventory metadata; VID/PID never grants access.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Identity {
    /// Current IORegistry entry ID; changes after removal/reset.
    pub registry_id: u64,
    /// Physical port location.
    pub location: u32,
    /// Descriptor vendor ID.
    pub vendor: u16,
    /// Descriptor product ID.
    pub product: u16,
    /// Linux USB/IP speed value.
    pub speed: u32,
}
/// Enumerate without opening or capturing devices. No serials or payloads read.
pub fn enumerate() -> Result<Vec<Identity>> {
    unsafe {
        let mut iterator = 0;
        if IOServiceGetMatchingServices(
            0,
            IOServiceMatching(c"IOUSBHostDevice".as_ptr()),
            &mut iterator,
        ) != 0
        {
            return Err(Error::Unavailable);
        }
        let iterator = Service(iterator);
        let mut result = Vec::new();
        loop {
            let entry = Service(IOIteratorNext(iterator.0));
            if entry.0 == 0 {
                break;
            }
            if result.len() == limits::MAX_ENUMERATED_DEVICES {
                return Err(Error::Exhausted);
            }
            if let Some(identity) = identity(&entry) {
                result.push(identity);
            }
        }
        Ok(result)
    }
}
fn identity(entry: &Service) -> Option<Identity> {
    let mut registry_id = 0;
    if unsafe { IORegistryEntryGetRegistryEntryID(entry.0, &mut registry_id) } != 0 {
        return None;
    }
    // IOUSBHostConnectionSpeed differs from the legacy Device Speed enum.
    Some(Identity {
        registry_id,
        location: property(entry.0, c"locationID")? as u32,
        vendor: property(entry.0, c"idVendor")? as u16,
        product: property(entry.0, c"idProduct")? as u16,
        speed: match HostSpeed(u32::try_from(property(entry.0, c"USBSpeed")?).ok()?) {
            HostSpeed::IOUSBHostConnectionSpeedFull => usb::SPEED_FULL,
            HostSpeed::IOUSBHostConnectionSpeedLow => usb::SPEED_LOW,
            HostSpeed::IOUSBHostConnectionSpeedHigh => usb::SPEED_HIGH,
            HostSpeed::IOUSBHostConnectionSpeedSuper => usb::SPEED_SUPER,
            HostSpeed::IOUSBHostConnectionSpeedSuperPlus
            | HostSpeed::IOUSBHostConnectionSpeedSuperPlusBy2 => usb::SPEED_SUPER_PLUS,
            _ => return None,
        },
    })
}
fn find(id: Identity) -> Result<Service> {
    let service = Service(unsafe {
        IOServiceGetMatchingService(0, IORegistryEntryIDMatching(id.registry_id))
    });
    if service.0 == 0 || identity(&service) != Some(id) {
        return Err(Error::NotFound);
    }
    Ok(service)
}
/// Physical descriptors and validated inventory, without any native handles.
#[derive(Clone, Debug)]
pub struct Snapshot {
    /// Exact physical incarnation.
    pub identity: Identity,
    /// Engine metadata. Host assigns stable DeviceId and monotonic generation.
    pub device: Device,
    /// Unmodified device descriptor.
    pub descriptor: Vec<u8>,
    /// Unmodified complete configurations, including class/vendor descriptors.
    pub configurations: Vec<Vec<u8>>,
}
/// Retrieve descriptors with ordinary device open. Does not claim interfaces or
/// try capture automatically. Refusal preserves the OS owner.
pub fn inspect(id: Identity, key: DeviceKey) -> Result<Snapshot> {
    let gone = Arc::new(AtomicBool::new(false));
    let device = open_device(id, false, gone, Arc::default())?;
    let result = read_snapshot(&device, id, key);
    let span = journal::begin(
        "device.destroy.inspect",
        &format!("registry={:x}", id.registry_id),
    )
    .ok();
    unsafe {
        device.destroy();
    }
    if let Some(span) = span {
        span.finish("returned");
    }
    result
}
fn open_device(
    id: Identity,
    capture: bool,
    gone: Arc<AtomicBool>,
    wake: Arc<NativeWake>,
) -> Result<Retained<IOUSBHostDevice>> {
    let service = find(id)?;
    let span = journal::begin(
        "device.open",
        &format!(
            "registry={:x} location={:08x} vid={:04x} pid={:04x} service={} capture={capture}",
            id.registry_id, id.location, id.vendor, id.product, service.0
        ),
    )?;
    let mut error = None;
    let callback = RcBlock::new(
        move |_object: NonNull<IOUSBHostObject>, message: u32, _arg: *mut c_void| {
            journal::note(
                "callback",
                "device.interest",
                &format!("registry={:x} message={message:08x}", id.registry_id),
            );
            if message == objc2_io_kit::kIOMessageServiceIsTerminated {
                gone.store(true, Ordering::Release);
                wake.notify();
            }
        },
    );
    let result = unsafe {
        IOUSBHostDevice::initWithIOService_options_queue_error_interestHandler(
            IOUSBHostDevice::alloc(),
            service.0,
            if capture {
                IOUSBHostObjectInitOptions::DeviceCapture
            } else {
                IOUSBHostObjectInitOptions::None
            },
            None,
            Some(&mut error),
            RcBlock::as_ptr(&callback),
        )
        .ok_or_else(|| native_error(error.as_deref()))
    };
    span.finish(&format!(
        "success={} ioreturn={:?}",
        result.is_ok(),
        error.as_ref().map(|e| e.code() as u32)
    ));
    result
}
fn read_snapshot(device: &IOUSBHostDevice, id: Identity, key: DeviceKey) -> Result<Snapshot> {
    let descriptor = control(device, usb::READ_DEVICE_DESCRIPTOR)?;
    if descriptor.len() != usb::DEVICE_DESCRIPTOR_LENGTH
        || descriptor[usb::DEVICE_CONFIGURATION_COUNT_OFFSET] == 0
        || descriptor[usb::DEVICE_CONFIGURATION_COUNT_OFFSET] as usize > limits::MAX_CONFIGURATIONS
    {
        return Err(Error::Invalid);
    }
    let mut configurations = Vec::new();
    for index in 0..descriptor[usb::DEVICE_CONFIGURATION_COUNT_OFFSET] {
        let header = control(
            device,
            usb::setup(
                usb::DIRECTION_IN,
                usb::GET_DESCRIPTOR,
                (u16::from(usb::DESCRIPTOR_CONFIGURATION) << u8::BITS) | u16::from(index),
                0,
                usb::CONFIGURATION_DESCRIPTOR_LENGTH as u16,
            ),
        )?;
        if header.len() != usb::CONFIGURATION_DESCRIPTOR_LENGTH {
            return Err(Error::Invalid);
        }
        let length = descriptors::word(&header, usb::CONFIG_TOTAL_LENGTH_OFFSET);
        if usize::from(length) < usb::CONFIGURATION_DESCRIPTOR_LENGTH {
            return Err(Error::Invalid);
        }
        configurations.push(control(
            device,
            usb::setup(
                usb::DIRECTION_IN,
                usb::GET_DESCRIPTOR,
                (u16::from(usb::DESCRIPTOR_CONFIGURATION) << u8::BITS) | u16::from(index),
                0,
                length,
            ),
        )?);
    }
    let current = control(device, usb::READ_CONFIGURATION)?;
    if current.len() != 1 {
        return Err(Error::Invalid);
    }
    let model = descriptors::device(
        key,
        id.location,
        id.speed,
        &descriptor,
        &configurations,
        current[0],
    )?;
    if model.wire.vendor_id != id.vendor || model.wire.product_id != id.product {
        return Err(Error::Invalid);
    }
    Ok(Snapshot {
        identity: id,
        device: model,
        descriptor,
        configurations,
    })
}
fn setup(s: [u8; usb::SETUP_SIZE]) -> IOUSBDeviceRequest {
    IOUSBDeviceRequest {
        bmRequestType: s[usb::SETUP_TYPE],
        bRequest: s[usb::SETUP_REQUEST],
        wValue: u16::from_le_bytes([s[usb::SETUP_VALUE], s[usb::SETUP_VALUE + 1]]),
        wIndex: u16::from_le_bytes([s[usb::SETUP_INDEX], s[usb::SETUP_INDEX + 1]]),
        wLength: u16::from_le_bytes([s[usb::SETUP_LENGTH], s[usb::SETUP_LENGTH + 1]]),
    }
}
fn control(device: &IOUSBHostDevice, s: [u8; usb::SETUP_SIZE]) -> Result<Vec<u8>> {
    let data = NSMutableData::dataWithLength(usize::from(u16::from_le_bytes([
        s[usb::SETUP_LENGTH],
        s[usb::SETUP_LENGTH + 1],
    ])))
    .ok_or(Error::Exhausted)?;
    let mut count = 0;
    let span = journal::begin("control.sync", &format!("setup={s:02x?}"))?;
    unsafe {
        let outcome = device.sendDeviceRequest_data_bytesTransferred_completionTimeout_error(
            setup(s),
            Some(&data),
            &mut count,
            limits::CONTROL_TIMEOUT_SECONDS,
        );
        span.finish(&format!(
            "bytes={count} ioreturn={:?}",
            outcome.as_ref().err().map(|e| e.code() as u32)
        ));
        outcome.map_err(|e| native_error(Some(&e)))?;
        if count > data.length() {
            return Err(Error::Invalid);
        }
        Ok(data.as_bytes_unchecked()[..count].to_vec())
    }
}

#[derive(Default)]
struct NativeWake(Mutex<Option<Waker>>);
impl NativeWake {
    fn notify(&self) {
        let waker = self.0.lock().unwrap().clone();
        if let Some(waker) = waker {
            waker.wake();
        }
    }
}

struct Signal {
    done: AtomicBool,
    status: AtomicU32,
    count: AtomicUsize,
}
struct Operation {
    id: TransferId,
    transfer: Transfer,
    address: u8,
    data: Retained<NSMutableData>,
    iso: Vec<IOUSBHostIsochronousTransaction>,
    signal: Arc<Signal>,
    cancelled: bool,
    _credit: MemoryReservation,
}
/// Claimed physical device. Construct on the backend worker, never on a GUI
/// thread. Root capture is explicit; normal opens never escalate themselves.
pub struct Native {
    device: Retained<IOUSBHostDevice>,
    snapshot: Snapshot,
    interfaces: BTreeMap<u8, Retained<IOUSBHostInterface>>,
    alternates: BTreeMap<u8, u8>,
    pipes: BTreeMap<u8, Retained<IOUSBHostPipe>>,
    operations: [Option<Operation>; SLOTS],
    _metadata: MemoryReservation,
    gone: Arc<AtomicBool>,
    wake: Arc<NativeWake>,
    active: u8,
    released: bool,
}
impl Native {
    /// Claim every interface of the selected configuration transactionally.
    /// `capture` explicitly opts into displacing the OS owner. The hosting engine
    /// must authorize this exact generation first; this function never elevates.
    /// An initialized diagnostic journal additionally restricts the target.
    pub fn open(snapshot: Snapshot, capture: bool, budget: Budget) -> Result<Self> {
        if capture
            && !journal::capture_allowed(
                snapshot.identity.location,
                snapshot.identity.vendor,
                snapshot.identity.product,
            )
        {
            return Err(Error::Unavailable);
        }
        let bytes = limits::NATIVE_METADATA_MULTIPLIER
            * (snapshot.descriptor.capacity()
                + snapshot.configurations.capacity() * std::mem::size_of::<Vec<u8>>()
                + snapshot
                    .configurations
                    .iter()
                    .map(Vec::capacity)
                    .sum::<usize>()
                + snapshot.device.storage_bytes()?)
            + std::mem::size_of::<Self>()
            + limits::NATIVE_METADATA_ALLOWANCE;
        let metadata = budget.reserve_memory(bytes)?;
        let gone = Arc::new(AtomicBool::new(false));
        let wake = Arc::default();
        let device = open_device(snapshot.identity, capture, gone.clone(), Arc::clone(&wake))?;
        let mut result = Self {
            device,
            active: snapshot.device.wire.configuration,
            snapshot,
            interfaces: BTreeMap::new(),
            alternates: BTreeMap::new(),
            pipes: BTreeMap::new(),
            operations: std::array::from_fn(|_| None),
            _metadata: metadata,
            gone,
            wake,
            released: false,
        };
        let actual = read_snapshot(
            &result.device,
            result.snapshot.identity,
            result.snapshot.device.key,
        )?;
        if actual.descriptor != result.snapshot.descriptor
            || actual.configurations != result.snapshot.configurations
            || actual.device != result.snapshot.device
        {
            return Err(Error::Invalid);
        }
        result.claim_interfaces()?;
        Ok(result)
    }
    fn claim_interfaces(&mut self) -> Result<()> {
        if self.active == 0 {
            return Ok(());
        }
        let config = self
            .snapshot
            .device
            .configurations
            .iter()
            .find(|c| c.value == self.active)
            .ok_or(Error::Invalid)?;
        let expected: Vec<_> = config
            .interfaces
            .iter()
            .filter(|i| i.alternate == 0)
            .map(|i| i.number)
            .collect();
        let deadline = Instant::now() + limits::INTERFACE_CLAIM_TIMEOUT;
        while self.interfaces.len() != expected.len() {
            if self.gone.load(Ordering::Acquire) {
                return Err(Error::Unavailable);
            }
            let mut iterator = 0;
            unsafe {
                if IORegistryEntryGetChildIterator(
                    self.device.ioService(),
                    c"IOService".as_ptr(),
                    &mut iterator,
                ) != 0
                {
                    return Err(Error::Unavailable);
                }
                let iterator = Service(iterator);
                loop {
                    let service = Service(IOIteratorNext(iterator.0));
                    if service.0 == 0 {
                        break;
                    }
                    if IOObjectConformsTo(service.0, c"IOUSBHostInterface".as_ptr()) == 0 {
                        continue;
                    }
                    let number =
                        property(service.0, c"bInterfaceNumber").ok_or(Error::Invalid)? as u8;
                    if !expected.contains(&number) {
                        return Err(Error::Invalid);
                    }
                    if self.interfaces.contains_key(&number) {
                        continue;
                    }
                    let span = journal::begin(
                        "interface.open",
                        &format!("number={number} service={}", service.0),
                    )?;
                    let mut error = None;
                    let interface =
                        IOUSBHostInterface::initWithIOService_options_queue_error_interestHandler(
                            IOUSBHostInterface::alloc(),
                            service.0,
                            IOUSBHostObjectInitOptions::None,
                            None,
                            Some(&mut error),
                            ptr::null_mut(),
                        );
                    span.finish(&format!(
                        "success={} ioreturn={:?}",
                        interface.is_some(),
                        error.as_ref().map(|e| e.code() as u32)
                    ));
                    let interface = interface.ok_or_else(|| native_error(error.as_deref()))?;
                    self.interfaces.insert(number, interface);
                    let alt = control(
                        &self.device,
                        usb::setup(
                            usb::DIRECTION_IN | usb::RECIPIENT_INTERFACE,
                            usb::GET_INTERFACE,
                            0,
                            u16::from(number),
                            1,
                        ),
                    )?;
                    if alt.len() != 1 {
                        return Err(Error::Invalid);
                    }
                    self.alternates.insert(number, alt[0]);
                }
            }
            if Instant::now() >= deadline {
                return Err(Error::Deadline);
            }
            if self.interfaces.len() != expected.len() {
                std::thread::sleep(limits::INTERFACE_CLAIM_RETRY);
            }
        }
        self.rebuild_pipes()
    }
    fn rebuild_pipes(&mut self) -> Result<()> {
        self.pipes.clear();
        let config = self
            .snapshot
            .device
            .configurations
            .iter()
            .find(|c| c.value == self.active)
            .ok_or(Error::Invalid)?;
        for (&number, &alternate) in &self.alternates {
            let setting = config
                .interfaces
                .iter()
                .find(|i| i.number == number && i.alternate == alternate)
                .ok_or(Error::Invalid)?;
            for endpoint in &setting.endpoints {
                let span = journal::begin(
                    "pipe.copy",
                    &format!(
                        "interface={number} alternate={alternate} address={:02x}",
                        endpoint.address
                    ),
                )?;
                let pipe = unsafe {
                    self.interfaces[&number]
                        .copyPipeWithAddress_error(usize::from(endpoint.address))
                };
                span.finish(&format!(
                    "success={} ioreturn={:?}",
                    pipe.is_ok(),
                    pipe.as_ref().err().map(|e| e.code() as u32)
                ));
                let pipe = pipe.map_err(|e| native_error(Some(&e)))?;
                if self.pipes.insert(endpoint.address, pipe).is_some() {
                    return Err(Error::Invalid);
                }
            }
        }
        Ok(())
    }
    fn close_interfaces(&mut self) {
        journal::note(
            "begin",
            "pipes.release",
            &format!("count={}", self.pipes.len()),
        );
        self.pipes.clear();
        journal::note("end", "pipes.release", "");
        for (number, interface) in std::mem::take(&mut self.interfaces) {
            journal::note("begin", "interface.destroy", &format!("number={number}"));
            unsafe {
                interface.destroy();
            }
            journal::note("end", "interface.destroy", &format!("number={number}"));
        }
        self.alternates.clear();
    }
    /// Physical reset terminates this generation. Re-enumeration requires a new
    /// inventory entry and authorization; never silently reconnect a stale lease.
    pub fn reset(&mut self) -> Result<()> {
        if self.operations.iter().any(Option::is_some) {
            return Err(Error::State);
        }
        self.close_interfaces();
        let span = journal::begin(
            "device.reset",
            &format!("registry={:x}", self.snapshot.identity.registry_id),
        )?;
        let result = unsafe { self.device.resetWithError() }.map_err(|e| native_error(Some(&e)));
        span.finish(&format!("result={result:?}"));
        self.gone.store(true, Ordering::Release);
        result
    }
    fn endpoint(&self, address: u8) -> Option<&knightbus_core::Endpoint> {
        self.snapshot
            .device
            .configurations
            .iter()
            .find(|c| c.value == self.active)?
            .interfaces
            .iter()
            .filter(|i| self.alternates.get(&i.number) == Some(&i.alternate))
            .flat_map(|i| &i.endpoints)
            .find(|e| e.address == address)
    }
    fn validate(&self, t: &Transfer) -> Result<u8> {
        let h = t.header();
        if h.endpoint > u32::from(usb::ENDPOINT_NUMBER_MASK)
            || h.transfer_buffer_length as usize != t.buffer().len()
            || t.buffer().len() > MAX_TRANSFER
            || t.layout().len() > MAX_ISO
            || h.device_id
                != ((self.snapshot.device.wire.bus_number << usb::WIRE_BUS_SHIFT)
                    | self.snapshot.device.wire.device_number)
        {
            return Err(Error::Invalid);
        }
        if h.endpoint == 0 {
            if h.transfer_flags != 0
                || !t.layout().is_empty()
                || usize::from(u16::from_le_bytes([
                    h.setup[usb::SETUP_LENGTH],
                    h.setup[usb::SETUP_LENGTH + 1],
                ])) != t.buffer().len()
                || (h.setup[usb::SETUP_TYPE] & usb::DIRECTION_IN != 0)
                    != (h.direction == Direction::In)
            {
                return Err(Error::Invalid);
            }
            if h.setup[usb::SETUP_TYPE] & usb::REQUEST_TYPE_MASK == 0
                && h.setup[usb::SETUP_REQUEST] == usb::SET_ADDRESS
            {
                return Err(Error::Unsupported);
            }
            return Ok(0);
        }
        let address = h.endpoint as u8
            | if h.direction == Direction::In {
                usb::DIRECTION_IN
            } else {
                0
            };
        let e = self.endpoint(address).ok_or(Error::Invalid)?;
        if h.setup != [0; usb::SETUP_SIZE]
            || h.transfer_flags & !e.allowed_flags != 0
            || (e.kind == TransferKind::Isochronous) == t.layout().is_empty()
        {
            return Err(Error::Unsupported);
        }
        if e.kind == TransferKind::Isochronous {
            // USB/IP frame numbers are importer-local. Only ASAP is safely
            // translatable without an explicit cross-host frame-clock contract.
            if h.transfer_flags & usb::ISO_ASAP == 0 {
                return Err(Error::Unsupported);
            }
            let packet = (e.max_packet_size & usb::MAX_PACKET_SIZE_MASK) as u32
                * (1 + u32::from(
                    (e.max_packet_size >> usb::HS_TRANSACTIONS_SHIFT) & usb::HS_TRANSACTIONS_MASK,
                ));
            if t.layout().iter().any(|p| p.length > packet) {
                return Err(Error::Invalid);
            }
        }
        Ok(address)
    }
    fn special(&mut self, s: [u8; usb::SETUP_SIZE]) -> Option<Result<()>> {
        if s[usb::SETUP_TYPE] & usb::REQUEST_TYPE_MASK != 0 {
            return None;
        }
        let value = u16::from_le_bytes([s[usb::SETUP_VALUE], s[usb::SETUP_VALUE + 1]]);
        let index = u16::from_le_bytes([s[usb::SETUP_INDEX], s[usb::SETUP_INDEX + 1]]);
        let mut changed = false;
        let result = match s[usb::SETUP_REQUEST] {
            usb::SET_CONFIGURATION => Some((|| {
                if s[usb::SETUP_TYPE] != 0
                    || value > u16::from(u8::MAX)
                    || index != 0
                    || s[usb::SETUP_LENGTH..] != [0, 0]
                    || (value != 0
                        && !self
                            .snapshot
                            .device
                            .configurations
                            .iter()
                            .any(|c| u16::from(c.value) == value))
                {
                    return Err(Error::Invalid);
                }
                let span = journal::begin("configuration.select", &format!("value={value}"))?;
                self.close_interfaces();
                changed = true;
                unsafe {
                    self.device
                        .configureWithValue_matchInterfaces_error(value as usize, false)
                }
                .map_err(|e| native_error(Some(&e)))?;
                span.finish("OS configuration returned success; reacquiring interfaces");
                self.active = value as u8;
                self.claim_interfaces()
            })()),
            usb::SET_INTERFACE => Some((|| {
                if s[usb::SETUP_TYPE] != usb::RECIPIENT_INTERFACE
                    || value > u16::from(u8::MAX)
                    || index > u16::from(u8::MAX)
                    || s[usb::SETUP_LENGTH..] != [0, 0]
                {
                    return Err(Error::Invalid);
                }
                if !self
                    .snapshot
                    .device
                    .configurations
                    .iter()
                    .filter(|c| c.value == self.active)
                    .flat_map(|c| &c.interfaces)
                    .any(|i| u16::from(i.number) == index && u16::from(i.alternate) == value)
                {
                    return Err(Error::Invalid);
                }
                let interface = self.interfaces.get(&(index as u8)).ok_or(Error::Invalid)?;
                let span = journal::begin(
                    "alternate.select",
                    &format!("interface={index} value={value}"),
                )?;
                changed = true;
                unsafe { interface.selectAlternateSetting_error(value as usize) }
                    .map_err(|e| native_error(Some(&e)))?;
                span.finish("OS alternate returned success; rebuilding pipes");
                self.alternates.insert(index as u8, value as u8);
                self.rebuild_pipes()
            })()),
            usb::CLEAR_FEATURE
                if s[usb::SETUP_TYPE] == usb::RECIPIENT_ENDPOINT && value == usb::ENDPOINT_HALT =>
            {
                Some((|| {
                    if index > u16::from(u8::MAX) || s[usb::SETUP_LENGTH..] != [0, 0] {
                        return Err(Error::Invalid);
                    }
                    let pipe = self.pipes.get(&(index as u8)).ok_or(Error::Invalid)?;
                    let span =
                        journal::begin("pipe.clear-stall", &format!("endpoint={index:02x}"))?;
                    let result =
                        unsafe { pipe.clearStallWithError() }.map_err(|e| native_error(Some(&e)));
                    span.finish(&format!("result={result:?}"));
                    result
                })())
            }
            _ => None,
        };
        if changed && result.as_ref().is_some_and(|r| r.is_err()) {
            self.gone.store(true, Ordering::Release);
        }
        result
    }
}
impl Driver for Native {
    fn set_waker(&mut self, waker: Waker) {
        *self.wake.0.lock().unwrap() = Some(waker);
    }
    fn submit(
        &mut self,
        id: TransferId,
        transfer: Transfer,
    ) -> std::result::Result<(), (SubmitError, Transfer)> {
        if self.gone.load(Ordering::Acquire) || self.released {
            return Err((SubmitError::Failed(Error::Unavailable), transfer));
        }
        let address = match self.validate(&transfer) {
            Ok(a) => a,
            Err(e) => return Err((SubmitError::Failed(e), transfer)),
        };
        if self
            .operations
            .iter()
            .flatten()
            .any(|o| o.address == address)
        {
            return Err((SubmitError::Busy, transfer));
        }
        let Some(slot) = self.operations.iter().position(Option::is_none) else {
            return Err((SubmitError::Busy, transfer));
        };
        let s = transfer.header().setup;
        let special = address == 0
            && s[usb::SETUP_TYPE] & usb::REQUEST_TYPE_MASK == 0
            && matches!(
                s[usb::SETUP_REQUEST],
                usb::CLEAR_FEATURE | usb::SET_CONFIGURATION | usb::SET_INTERFACE
            );
        if special && self.operations.iter().any(Option::is_some) {
            return Err((SubmitError::Busy, transfer));
        }
        let charge = transfer.buffer().len()
            + transfer.layout().len() * std::mem::size_of::<IOUSBHostIsochronousTransaction>()
            + std::mem::size_of::<Operation>()
            + limits::TRANSFER_METADATA_ALLOWANCE;
        let credit = match transfer.reserve_native_memory(charge) {
            Ok(c) => c,
            Err(e) => return Err((SubmitError::Failed(e), transfer)),
        };
        let span = match journal::begin(
            "transfer.submit",
            &format!(
                "token={id:?} address={address:02x} length={} setup={s:02x?}",
                transfer.buffer().len()
            ),
        ) {
            Ok(span) => span,
            Err(e) => return Err((SubmitError::Failed(e), transfer)),
        };
        let data = NSMutableData::with_bytes(transfer.buffer());
        let mut op = Operation {
            id,
            transfer,
            address,
            data,
            iso: Vec::new(),
            signal: Arc::new(Signal {
                done: AtomicBool::new(false),
                status: AtomicU32::new(0),
                count: AtomicUsize::new(0),
            }),
            cancelled: false,
            _credit: credit,
        };
        if special {
            LAST_ERROR.set(None);
            if let Some(result) = self.special(s) {
                // Once a topology API has run, an unsuccessful reclaim cannot
                // leave the old topology usable. Drain the physical generation.
                op.signal.status.store(
                    result.err().map_or(0, |e| {
                        LAST_ERROR.get().unwrap_or(crate::backend::status(e) as u32)
                    }),
                    Ordering::Relaxed,
                );
                // Distinguish normalized synthetic status from IOReturn below.
                op.signal.done.store(true, Ordering::Release);
                span.finish(&format!("special=true result={result:?}"));
                self.operations[slot] = Some(op);
                return Ok(());
            }
        }
        let signal = op.signal.clone();
        let wake = self.wake.clone();
        let callback = RcBlock::new(move |status: i32, count: usize| {
            journal::note(
                "callback",
                "transfer.complete",
                &format!("token={id:?} status={status:08x} bytes={count}"),
            );
            signal.status.store(status as u32, Ordering::Relaxed);
            signal.count.store(count, Ordering::Relaxed);
            signal.done.store(true, Ordering::Release);
            wake.notify();
        });
        let mut error = None;
        let accepted = unsafe {
            if address == 0 {
                self.device
                    .enqueueDeviceRequest_data_completionTimeout_error_completionHandler(
                        setup(s),
                        Some(&op.data),
                        limits::CONTROL_TIMEOUT_SECONDS,
                        Some(&mut error),
                        RcBlock::as_ptr(&callback),
                    )
            } else if op.transfer.layout().is_empty() {
                self.pipes[&address]
                    .enqueueIORequestWithData_completionTimeout_error_completionHandler(
                        Some(&op.data),
                        limits::PIPE_TIMEOUT_SECONDS,
                        Some(&mut error),
                        RcBlock::as_ptr(&callback),
                    )
            } else {
                op.iso = op
                    .transfer
                    .layout()
                    .iter()
                    .map(|p| IOUSBHostIsochronousTransaction {
                        status: 0,
                        requestCount: p.length,
                        offset: p.offset,
                        completeCount: 0,
                        timeStamp: 0,
                        options: IOUSBHostIsochronousTransactionOptions::None,
                    })
                    .collect();
                let signal = op.signal.clone();
                let wake = self.wake.clone();
                let iso_callback = RcBlock::new(
                    move |status: i32, _list: NonNull<IOUSBHostIsochronousTransaction>| {
                        journal::note(
                            "callback",
                            "transfer.iso.complete",
                            &format!("token={id:?} status={status:08x}"),
                        );
                        signal.status.store(status as u32, Ordering::Relaxed);
                        signal.done.store(true, Ordering::Release);
                        wake.notify();
                    },
                );
                self.pipes[&address].enqueueIORequestWithData_transactionList_transactionListCount_firstFrameNumber_options_error_completionHandler(&op.data,NonNull::new(op.iso.as_mut_ptr()).unwrap(),op.iso.len(),0,IOUSBHostIsochronousTransferOptions::None,Some(&mut error),RcBlock::as_ptr(&iso_callback))
            }
        };
        span.finish(&format!(
            "accepted={accepted} ioreturn={:?}",
            error.as_ref().map(|e| e.code() as u32)
        ));
        if !accepted {
            // A rejected enqueue schedules no callback. Preserve its exact USB
            // error as a retired completion, rather than flattening stalls into
            // an admission failure or waiting for a callback that cannot arrive.
            let code = error
                .as_ref()
                .map_or(status::IO as u32, |e| e.code() as u32);
            op.signal.status.store(code, Ordering::Relaxed);
            op.signal.done.store(true, Ordering::Release);
        }
        self.operations[slot] = Some(op);
        Ok(())
    }
    fn cancel(&mut self, id: TransferId) -> Result<()> {
        let Some(op) = self.operations.iter_mut().flatten().find(|o| o.id == id) else {
            return Ok(());
        };
        if op.cancelled || op.signal.done.load(Ordering::Acquire) {
            return Ok(());
        }
        journal::note(
            "begin",
            "transfer.abort",
            &format!("token={id:?} address={:02x}", op.address),
        );
        let result = unsafe {
            if op.address == 0 {
                self.device
                    .abortDeviceRequestsWithOption_error(IOUSBHostAbortOption::Asynchronous)
            } else {
                self.pipes[&op.address].abortWithOption_error(IOUSBHostAbortOption::Asynchronous)
            }
        };
        journal::note(
            "end",
            "transfer.abort",
            &format!(
                "token={id:?} success={} ioreturn={:?}",
                result.is_ok(),
                result.as_ref().err().map(|e| e.code() as u32)
            ),
        );
        // Retry failed aborts. Never manufacture retirement on an API error.
        op.cancelled = result.is_ok();
        result.map_err(|error| native_error(Some(&error)))
    }
    fn completion(&mut self) -> Option<(TransferId, Completion)> {
        let i = self.operations.iter().position(|o| {
            o.as_ref()
                .is_some_and(|o| o.signal.done.load(Ordering::Acquire))
        })?;
        // Drain the serial callback queue before releasing any native storage.
        unsafe { self.device.queue() }.exec_sync(|| {});
        for interface in self.interfaces.values() {
            unsafe { interface.queue() }.exec_sync(|| {});
        }
        let mut op = self.operations[i].take().unwrap();
        let native = op.signal.status.load(Ordering::Relaxed);
        let mut status = usb_status(native);
        let mut count = op.signal.count.load(Ordering::Relaxed);
        if !op.iso.is_empty() {
            count = 0;
            for (p, t) in op.transfer.packets_mut().iter_mut().zip(&op.iso) {
                if t.completeCount > p.length {
                    status = status::OVERFLOW;
                    p.actual_length = 0;
                    p.status = status::OVERFLOW;
                } else {
                    p.actual_length = t.completeCount;
                    p.status = usb_status(t.status as u32);
                }
                count += p.actual_length as usize;
            }
        }
        if count > op.transfer.buffer().len() {
            count = 0;
            status = status::OVERFLOW;
        }
        // SAFETY: terminal callback release/acquire retires native access. The
        // retained NSMutableData is never mutated from Rust while I/O is active.
        if op.transfer.header().direction == Direction::In {
            unsafe {
                op.transfer
                    .buffer_mut()
                    .copy_from_slice(op.data.as_bytes_unchecked());
            }
        }
        if status == status::NO_DEVICE {
            self.gone.store(true, Ordering::Release);
        }
        Some((
            op.id,
            Completion {
                transfer: op.transfer,
                status,
                actual_length: count as u32,
                start_frame: 0,
            },
        ))
    }
    fn removed(&self) -> bool {
        self.gone.load(Ordering::Acquire)
    }
    fn release(&mut self) -> Result<()> {
        if self.operations.iter().any(Option::is_some) {
            return Err(Error::Cleanup);
        }
        if !self.released {
            self.close_interfaces();
            journal::note(
                "begin",
                "device.destroy.capture",
                &format!("registry={:x}", self.snapshot.identity.registry_id),
            );
            unsafe {
                self.device.destroy();
            }
            journal::note(
                "end",
                "device.destroy.capture",
                &format!("registry={:x}", self.snapshot.identity.registry_id),
            );
            self.released = true;
        }
        Ok(())
    }
}
impl Drop for Native {
    fn drop(&mut self) {
        // Normal worker teardown reaches here only after callbacks. Direct users
        // also drain; a stalled OS callback retains ownership rather than freeing
        // native-accessible memory. Run this type only on a dedicated worker.
        while self.operations.iter().any(Option::is_some) {
            let ids: Vec<_> = self.operations.iter().flatten().map(|o| o.id).collect();
            for id in ids {
                let _ = self.cancel(id);
            }
            while self.completion().is_some() {}
            if self.operations.iter().any(Option::is_some) {
                std::thread::sleep(limits::DIRECT_DRAIN_RETRY);
            }
        }
        let _ = self.release();
    }
}
thread_local! { static LAST_ERROR: std::cell::Cell<Option<u32>> = const { std::cell::Cell::new(None) }; }
/// Last exact IOReturn observed on this thread, for explicit access diagnostics.
pub fn last_native_error() -> Option<u32> {
    LAST_ERROR.get()
}
fn native_error(error: Option<&NSError>) -> Error {
    let code = error.map(|e| e.code() as u32);
    LAST_ERROR.set(code);
    match code {
        Some(
            objc2_io_kit::kIOReturnNotPrivileged
            | objc2_io_kit::kIOReturnNotPermitted
            | objc2_io_kit::kIOReturnExclusiveAccess
            | objc2_io_kit::kIOReturnInternalError,
        ) => Error::Denied,
        Some(
            objc2_io_kit::kIOReturnNoDevice
            | objc2_io_kit::kIOReturnNotOpen
            | objc2_io_kit::kIOReturnOffline
            | MACH_SEND_INVALID_DEST,
        ) => Error::Unavailable,
        Some(objc2_io_kit::kIOReturnTimeout) => Error::Deadline,
        Some(objc2_io_kit::kIOReturnUnsupported) => Error::Unsupported,
        _ => Error::Internal,
    }
}
/// Normalize IOReturn without treating arbitrary negative values as Linux errno.
pub fn usb_status(code: u32) -> i32 {
    match code {
        0 => status::SUCCESS,
        objc2_io_kit::kIOReturnAborted => status::CANCELLED,
        objc2_io_kit::kIOReturnTimeout | objc2_io_kit::kIOUSBTransactionTimeout => {
            status::TIMED_OUT
        }
        // IOUSBHost can report MACH_SEND_INVALID_DEST after unplug destroys
        // the user-client port, before its separate termination callback.
        objc2_io_kit::kIOReturnNoDevice
        | objc2_io_kit::kIOReturnNotOpen
        | objc2_io_kit::kIOReturnOffline
        | MACH_SEND_INVALID_DEST => status::NO_DEVICE,
        objc2_io_kit::kIOReturnNotResponding => status::PROTOCOL,
        objc2_io_kit::kIOReturnUnderrun => status::SUCCESS,
        // IOUSBHostFamilyDefinitions.h defines a distinct modern STALL code;
        // USB.h's legacy kIOUSBPipeStalled is still accepted for older paths.
        objc2_io_kit::kIOUSBPipeStalled | USB_HOST_PIPE_STALLED => status::PIPE,
        objc2_io_kit::kIOReturnOverrun => status::OVERFLOW,
        // Worker-local special-operation errors use this small known errno set.
        v if [
            status::IO,
            status::NO_MEMORY,
            status::ACCESS,
            status::NO_DEVICE,
            status::INVALID,
            status::UNSUPPORTED,
            status::TIMED_OUT,
            status::CANCELLED,
        ]
        .contains(&(v as i32)) =>
        {
            v as i32
        }
        _ => status::IO,
    }
}

/// Convenience identity for a stand-alone lab process. Production hosts must
/// persist their own inventory mapping rather than authorize by VID/PID.
pub fn lab_key(id: Identity) -> DeviceKey {
    DeviceKey {
        id: DeviceId(u64::from(id.location)),
        generation: id.registry_id,
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn disconnected_user_client_port_retires_the_device_generation() {
        // Rejected enqueue observed after physical unplug in
        // m8-1113-231955-20b4; mach/message.h names this INVALID_DEST.
        assert_eq!(super::usb_status(0x10000003), -19);
        assert_eq!(super::usb_status(0xe00002ed), -71);
        assert_eq!(super::usb_status(0x10000004), -5);
    }
    #[test]
    fn physical_host_stall_is_preserved_as_epipe() {
        // Observed callback in trial m8-1113-230930-eff4. Mapping it to generic
        // EIO loses the condition the remote USB driver needs for recovery.
        assert_eq!(super::usb_status(0xe0005000), -32);
        assert_eq!(super::usb_status(0xe000404f), -32);
        assert_eq!(super::usb_status(0xe0005001), -5);
    }
}
