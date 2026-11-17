//! Apple IOUSBHost bindings. All Objective-C owners are confined to one worker.
//! No forwarding implementation is imported: objc2 supplies ordinary API/ABI
//! bindings. Native storage is retained until the terminal callback publishes
//! with release ordering; only then may Rust inspect or free the buffer.
use crate::constants::limits;
use crate::{SLOTS, descriptors};
use block2::RcBlock;
use knightbus_core::exporter::{ControlError, Driver, SubmitError};
use knightbus_core::usbip::status;
use knightbus_core::{
    Budget, Completion, Device, DeviceId, DeviceKey, Error, MemoryReservation, Result, Transfer,
    TransferId, usbip::Direction,
};
use knightbus_usb as usb;
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
const C_STRING_TERMINATOR_BYTES: usize = 1;

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
    fn CFStringGetTypeID() -> usize;
    fn CFStringGetCString(
        value: *const c_void,
        buffer: *mut c_char,
        capacity: isize,
        encoding: u32,
    ) -> bool;
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
            HostSpeed::IOUSBHostConnectionSpeedFull => usb::Speed::Full as u32,
            HostSpeed::IOUSBHostConnectionSpeedLow => usb::Speed::Low as u32,
            HostSpeed::IOUSBHostConnectionSpeedHigh => usb::Speed::High as u32,
            HostSpeed::IOUSBHostConnectionSpeedSuper => usb::Speed::Super as u32,
            HostSpeed::IOUSBHostConnectionSpeedSuperPlus
            | HostSpeed::IOUSBHostConnectionSpeedSuperPlusBy2 => usb::Speed::SuperPlus as u32,
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
/// Exclude bus infrastructure without opening or displacing any OS driver.
/// A candidate may still need capture authorization or be temporarily in use.
pub fn share_candidate(id: Identity) -> bool {
    find(id)
        .ok()
        .and_then(|entry| property(entry.0, c"bDeviceClass"))
        .and_then(|class| u8::try_from(class).ok())
        .is_some_and(usb::shareable_class)
}
/// Read the cached OS product name without opening or claiming the device.
/// Names are untrusted display metadata and never identify an authorized device.
pub fn display_name(id: Identity) -> Option<String> {
    let entry = find(id).ok()?;
    // SAFETY: retained CF property, checked string type, bounded writable buffer.
    unsafe {
        let key = CFStringCreateWithCString(
            ptr::null(),
            c"USB Product Name".as_ptr(),
            CF_STRING_ENCODING_UTF8,
        );
        if key.is_null() {
            return None;
        }
        let value = IORegistryEntryCreateCFProperty(entry.0, key, ptr::null(), 0);
        CFRelease(key);
        if value.is_null() {
            return None;
        }
        let mut bytes =
            [0u8; knightbus_core::host::MAX_DEVICE_NAME_BYTES + C_STRING_TERMINATOR_BYTES];
        let valid = CFGetTypeID(value) == CFStringGetTypeID()
            && CFStringGetCString(
                value,
                bytes.as_mut_ptr().cast(),
                bytes.len() as isize,
                CF_STRING_ENCODING_UTF8,
            );
        CFRelease(value);
        if !valid {
            return None;
        }
        let length = bytes.iter().position(|byte| *byte == 0)?;
        let name = std::str::from_utf8(&bytes[..length]).ok()?.trim();
        if name.is_empty() || name.chars().any(char::is_control) {
            return None;
        }
        Some(name.to_owned())
    }
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
    let result = cached_snapshot(&device, id, key);
    unsafe {
        device.destroy();
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
    let mut error = None;
    let callback = RcBlock::new(
        move |_object: NonNull<IOUSBHostObject>, message: u32, _arg: *mut c_void| {
            if message == objc2_io_kit::kIOMessageServiceIsTerminated {
                gone.store(true, Ordering::Release);
                wake.notify();
            }
        },
    );
    unsafe {
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
    }
}
// Inventory uses Apple's cached descriptors. Sending endpoint-zero requests while
// the camera/audio driver owns the device can fail even though it is shareable.
fn cached_snapshot(device: &IOUSBHostDevice, id: Identity, key: DeviceKey) -> Result<Snapshot> {
    // SAFETY: IOUSBHost owns these immutable descriptor buffers until destroy.
    // Copy them before releasing the object; lengths are bounded and validated.
    unsafe {
        let descriptor = device.deviceDescriptor();
        if descriptor.is_null() {
            return Err(Error::Unavailable);
        }
        let descriptor =
            std::slice::from_raw_parts(descriptor.cast::<u8>(), usb::device::BYTES).to_vec();
        let count = usize::from(descriptor[usb::device::CONFIGURATIONS]);
        if count == 0 || count > limits::MAX_CONFIGURATIONS {
            return Err(Error::Invalid);
        }
        let mut configurations = Vec::with_capacity(count);
        for index in 0..count {
            let mut error = None;
            let config = device.configurationDescriptorWithIndex_error(index, Some(&mut error));
            if config.is_null() {
                return Err(native_error(error.as_deref()));
            }
            let header = std::slice::from_raw_parts(config.cast::<u8>(), usb::configuration::BYTES);
            let length = usize::from(usb::u16_at(header, usb::configuration::TOTAL_LENGTH));
            if !(usb::configuration::BYTES..=descriptors::MAX_CONFIGURATION).contains(&length) {
                return Err(Error::Invalid);
            }
            configurations.push(std::slice::from_raw_parts(config.cast::<u8>(), length).to_vec());
        }
        let selected = device.configurationDescriptor();
        let active = if selected.is_null() {
            0
        } else {
            (*selected).bConfigurationValue
        };
        let model = descriptors::device(
            key,
            id.location,
            id.speed,
            &descriptor,
            &configurations,
            active,
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
}
fn read_snapshot(device: &IOUSBHostDevice, id: Identity, key: DeviceKey) -> Result<Snapshot> {
    let descriptor = control(device, usb::READ_DEVICE_DESCRIPTOR)?;
    if descriptor.len() != usb::device::BYTES
        || descriptor[usb::device::CONFIGURATIONS] == 0
        || descriptor[usb::device::CONFIGURATIONS] as usize > limits::MAX_CONFIGURATIONS
    {
        return Err(Error::Invalid);
    }
    let mut configurations = Vec::new();
    for index in 0..descriptor[usb::device::CONFIGURATIONS] {
        let header = control(
            device,
            usb::setup(
                usb::DIRECTION_IN,
                usb::GET_DESCRIPTOR,
                (u16::from(usb::configuration::TYPE) << u8::BITS) | u16::from(index),
                0,
                usb::configuration::BYTES as u16,
            ),
        )?;
        if header.len() != usb::configuration::BYTES {
            return Err(Error::Invalid);
        }
        let length = usb::u16_at(&header, usb::configuration::TOTAL_LENGTH);
        if usize::from(length) < usb::configuration::BYTES {
            return Err(Error::Invalid);
        }
        configurations.push(control(
            device,
            usb::setup(
                usb::DIRECTION_IN,
                usb::GET_DESCRIPTOR,
                (u16::from(usb::configuration::TYPE) << u8::BITS) | u16::from(index),
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
    unsafe {
        let outcome = device.sendDeviceRequest_data_bytesTransferred_completionTimeout_error(
            setup(s),
            Some(&data),
            &mut count,
            limits::CONTROL_TIMEOUT_SECONDS,
        );
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
/// OS handles owned by one interface, never by a neighbouring interface.
pub struct NativeInterface {
    handle: Retained<IOUSBHostInterface>,
    alternate: u8,
    pipes: BTreeMap<u8, Retained<IOUSBHostPipe>>,
}
impl NativeInterface {
    fn rebuild(&mut self, setting: &knightbus_core::Interface) -> Result<()> {
        self.pipes.clear();
        for endpoint in &setting.endpoints {
            let pipe = unsafe {
                self.handle
                    .copyPipeWithAddress_error(endpoint.address.into())
            }
            .map_err(|e| native_error(Some(&e)))?;
            self.pipes.insert(endpoint.address, pipe);
        }
        self.alternate = setting.alternate;
        Ok(())
    }
}
impl knightbus_core::exporter::InterfaceDriver for NativeInterface {
    fn select_alternate(
        &mut self,
        setting: &knightbus_core::Interface,
    ) -> std::result::Result<(), ControlError> {
        unsafe {
            self.handle
                .selectAlternateSetting_error(setting.alternate.into())
        }
        .map_err(|e| ControlError::from(usb_status(e.code() as u32)))?;
        self.rebuild(setting).map_err(ControlError::from)
    }
    fn clear_halt(&mut self, address: u8) -> std::result::Result<(), ControlError> {
        let pipe = self.pipes.get(&address).ok_or(Error::Invalid)?;
        unsafe { pipe.clearStallWithError() }
            .map_err(|e| ControlError::from(usb_status(e.code() as u32)))
    }
}
impl Drop for NativeInterface {
    fn drop(&mut self) {
        self.pipes.clear();
        unsafe { self.handle.destroy() };
    }
}
fn pipe<'a>(
    interfaces: &'a BTreeMap<u8, NativeInterface>,
    routes: &[Option<u8>; usb::ENDPOINT_DIRECTION_SLOTS],
    address: u8,
) -> Option<&'a IOUSBHostPipe> {
    let number = routes[pipe_index(address)]?;
    interfaces
        .get(&number)?
        .pipes
        .get(&address)
        .map(|pipe| &**pipe)
}
fn pipe_index(address: u8) -> usize {
    usize::from(address & usb::ENDPOINT_NUMBER_MASK)
        + if address & usb::DIRECTION_IN != 0 {
            usize::from(usb::IN_ENDPOINT_BIT_OFFSET)
        } else {
            0
        }
}

/// Claimed physical device. Construct on the backend worker, never on a GUI
/// thread. Root capture is explicit; normal opens never escalate themselves.
pub struct Native {
    device: Retained<IOUSBHostDevice>,
    snapshot: Snapshot,
    interfaces: BTreeMap<u8, NativeInterface>,
    routes: [Option<u8>; usb::ENDPOINT_DIRECTION_SLOTS],
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
    pub fn open(snapshot: Snapshot, capture: bool, budget: Budget) -> Result<Self> {
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
            routes: [None; usb::ENDPOINT_DIRECTION_SLOTS],
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
                    let interface = interface.ok_or_else(|| native_error(error.as_deref()))?;
                    self.interfaces.insert(
                        number,
                        NativeInterface {
                            handle: interface,
                            alternate: 0,
                            pipes: BTreeMap::new(),
                        },
                    );
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
                    self.interfaces.get_mut(&number).unwrap().alternate = alt[0];
                }
            }
            if Instant::now() >= deadline {
                return Err(Error::Deadline);
            }
            if self.interfaces.len() != expected.len() {
                std::thread::sleep(limits::INTERFACE_CLAIM_RETRY);
            }
        }
        // Addresses retain their interface owner across alternate settings.
        // Build the lookup once, keeping ordinary transfers allocation-free.
        for setting in &config.interfaces {
            for endpoint in &setting.endpoints {
                let route = &mut self.routes[pipe_index(endpoint.address)];
                if route.is_some_and(|number| number != setting.number) {
                    return Err(Error::Invalid);
                }
                *route = Some(setting.number);
            }
        }
        for (&number, interface) in &mut self.interfaces {
            let setting = config
                .interfaces
                .iter()
                .find(|i| i.number == number && i.alternate == interface.alternate)
                .ok_or(Error::Invalid)?;
            interface.rebuild(setting)?;
        }
        Ok(())
    }
    fn close_interfaces(&mut self) {
        self.interfaces.clear();
        self.routes.fill(None);
    }
    /// Physical reset terminates this generation. Re-enumeration requires a new
    /// inventory entry and authorization; never silently reconnect a stale lease.
    pub fn reset(&mut self) -> Result<()> {
        if self.operations.iter().any(Option::is_some) {
            return Err(Error::State);
        }
        self.close_interfaces();
        let result = unsafe { self.device.resetWithError() }.map_err(|e| native_error(Some(&e)));
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
            .filter(|i| {
                self.interfaces
                    .get(&i.number)
                    .is_some_and(|native| native.alternate == i.alternate)
            })
            .flat_map(|i| &i.endpoints)
            .find(|e| e.address == address)
    }
    fn validate(&self, transfer: &Transfer) -> Result<u8> {
        let header = transfer.header();
        let address = header.endpoint as u8
            | if header.direction == Direction::In {
                usb::DIRECTION_IN
            } else {
                0
            };
        transfer.validate_native(
            (self.snapshot.device.wire.bus_number << knightbus_core::usbip::DEVICE_BUS_SHIFT)
                | self.snapshot.device.wire.device_number,
            self.endpoint(address),
            Self::CAPABILITIES,
        )
    }
}
impl Native {
    fn enqueue(&self, op: &mut Operation) -> (bool, Option<Retained<NSError>>) {
        let address = op.address;
        let s = op.transfer.header().setup;
        let signal = op.signal.clone();
        let wake = self.wake.clone();
        let callback = RcBlock::new(move |status: i32, count: usize| {
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
                pipe(&self.interfaces, &self.routes, address)
                    .unwrap()
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
                        signal.status.store(status as u32, Ordering::Relaxed);
                        signal.done.store(true, Ordering::Release);
                        wake.notify();
                    },
                );
                pipe(&self.interfaces, &self.routes, address).unwrap().enqueueIORequestWithData_transactionList_transactionListCount_firstFrameNumber_options_error_completionHandler(&op.data,NonNull::new(op.iso.as_mut_ptr()).unwrap(),op.iso.len(),0,IOUSBHostIsochronousTransferOptions::None,Some(&mut error),RcBlock::as_ptr(&iso_callback))
            }
        };
        (accepted, error)
    }
}

impl Driver for Native {
    type Interface = NativeInterface;
    fn configuration(&self) -> u8 {
        self.active
    }
    fn interface(&mut self, number: u8) -> Result<&mut NativeInterface> {
        self.interfaces.get_mut(&number).ok_or(Error::Invalid)
    }
    fn configure(
        &mut self,
        configuration: Option<&knightbus_core::Configuration>,
    ) -> std::result::Result<(), ControlError> {
        let value = configuration.map_or(0, |c| c.value);
        self.close_interfaces();
        unsafe {
            self.device
                .configureWithValue_matchInterfaces_error(value.into(), false)
        }
        .map_err(|e| ControlError::from(usb_status(e.code() as u32)))?;
        self.active = value;
        self.claim_interfaces().map_err(ControlError::from)
    }
    fn endpoint(&self, address: u8) -> Option<knightbus_core::Endpoint> {
        Native::endpoint(self, address).cloned()
    }
    const CAPABILITIES: knightbus_core::Capabilities = knightbus_core::Capabilities {
        transfer_bytes: 1024 * 1024,
        requests: SLOTS,
        iso_packets: 1024,
        speeds: usb::Speed::ALL_MASK,
        descriptor_bytes: 65535,
        configurations: 16,
        remote_wakeup: true,
    };
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
        let depth = if transfer.layout().is_empty() {
            1
        } else {
            knightbus_core::exporter::ISO_QUEUE_DEPTH
        };
        let mut active = 0;
        for op in self
            .operations
            .iter()
            .flatten()
            .filter(|o| o.address == address)
        {
            if op.cancelled {
                return Err((SubmitError::Busy, transfer));
            }
            active += 1;
            if active >= depth {
                return Err((SubmitError::Busy, transfer));
            }
        }
        let Some(slot) = self.operations.iter().position(Option::is_none) else {
            return Err((SubmitError::Busy, transfer));
        };
        let charge = transfer.buffer().len()
            + transfer.layout().len() * std::mem::size_of::<IOUSBHostIsochronousTransaction>()
            + std::mem::size_of::<Operation>()
            + limits::TRANSFER_METADATA_ALLOWANCE;
        let credit = match transfer.reserve_native_memory(charge) {
            Ok(c) => c,
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
        let (accepted, error) = self.enqueue(&mut op);
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
        let result = unsafe {
            if op.address == 0 {
                self.device
                    .abortDeviceRequestsWithOption_error(IOUSBHostAbortOption::Asynchronous)
            } else {
                pipe(&self.interfaces, &self.routes, op.address)
                    .unwrap()
                    .abortWithOption_error(IOUSBHostAbortOption::Asynchronous)
            }
        };
        // Retry failed aborts. Never manufacture retirement on an API error.
        if result.is_ok() {
            let address = op.address;
            // IOUSBHost aborts the entire pipe. Hold new work until all affected
            // callbacks retire their buffers, including uncancelled siblings.
            for op in self
                .operations
                .iter_mut()
                .flatten()
                .filter(|o| o.address == address)
            {
                if !op.signal.done.load(Ordering::Acquire) {
                    op.cancelled = true;
                }
            }
        }
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
            unsafe { interface.handle.queue() }.exec_sync(|| {});
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
            unsafe {
                self.device.destroy();
            }
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

impl Identity {
    /// Identity within this boot: location selects the port; registry ID selects
    /// its physical generation. Hosts persisting inventory must supply their own key.
    pub fn device_key(self) -> DeviceKey {
        DeviceKey {
            id: DeviceId(u64::from(self.location)),
            generation: self.registry_id,
        }
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
