//! Filter only the virtual root hub whose parent is one of our controllers.
//! UDE consumes configuration, interface and endpoint-halt control requests.
//! Endpoint notifications do not replace those remote operations. Forward setup packets to
//! the remote device before allowing UDE to complete the original request.
use super::structure_size;
use core::{
    cell::UnsafeCell,
    mem, ptr,
    sync::atomic::{AtomicPtr, AtomicU32, Ordering},
};
use wdk_sys::{call_unsafe_wdf_function_binding as wdf, *};

mod clock;

const MAX_CONTROLLERS: usize = 64;
const ID_LENGTH: usize = 200; // MAX_DEVICE_ID_LEN, including the terminator.
// Public device property keys from the WDK's devpkey.h.
const INSTANCE_ID: DEVPROPKEY = DEVPROPKEY {
    fmtid: GUID {
        Data1: 0x78c34fc8,
        Data2: 0x104a,
        Data3: 0x4aca,
        Data4: [0x9e, 0xa4, 0x52, 0x4d, 0x52, 0x99, 0x6e, 0x57],
    },
    pid: 256,
};
const PARENT_ID: DEVPROPKEY = DEVPROPKEY {
    fmtid: GUID {
        Data1: 0x4340a6c5,
        Data2: 0x93fa,
        Data3: 0x4706,
        Data4: [0x97, 0x2c, 0x7b, 0x64, 0x80, 0x08, 0xa5, 0xa7],
    },
    pid: 8,
};
#[derive(Clone, Copy)]
struct Entry {
    controller: WDFDEVICE,
    filter: WDFDEVICE,
    id: [u16; ID_LENGTH],
}
const EMPTY: Entry = Entry {
    controller: ptr::null_mut(),
    filter: ptr::null_mut(),
    id: [0; ID_LENGTH],
};
struct Registry(UnsafeCell<[Entry; MAX_CONTROLLERS]>);
// SAFETY: Every access is at PASSIVE_LEVEL under LOCK. Entries do not own
// controller references: cleanup removes them before destruction. A lookup
// references its result while still locked; filter cleanup releases that ref.
unsafe impl Sync for Registry {}
static REGISTRY: Registry = Registry(UnsafeCell::new([EMPTY; MAX_CONTROLLERS]));
static LOCK: AtomicPtr<WDFWAITLOCK__> = AtomicPtr::new(ptr::null_mut());

#[repr(C)]
struct Context {
    controller: WDFDEVICE,
    iso_speed: AtomicU32,
}
// Image-lifetime type identity; never mutated, addressed without references.
static mut CONTEXT_TYPE: WDF_OBJECT_CONTEXT_TYPE_INFO = WDF_OBJECT_CONTEXT_TYPE_INFO {
    Size: mem::size_of::<WDF_OBJECT_CONTEXT_TYPE_INFO>() as ULONG,
    ContextName: c"KnightBusRootHub".as_ptr(),
    ContextSize: mem::size_of::<Context>(),
    UniqueType: ptr::null(),
    EvtDriverGetUniqueContextType: None,
};
unsafe fn context(device: WDFDEVICE) -> *mut Context {
    unsafe {
        wdf!(
            WdfObjectGetTypedContextWorker,
            device.cast(),
            ptr::addr_of!(CONTEXT_TYPE)
        )
        .cast()
    }
}
// DriverEntry completes before any device callbacks. The framework owns the
// lock through the last device cleanup and unloads this image afterward.
pub(super) unsafe fn initialize(driver: WDFDRIVER) -> NTSTATUS {
    let mut a = WDF_OBJECT_ATTRIBUTES {
        Size: structure_size::<WDF_OBJECT_ATTRIBUTES>(),
        ParentObject: driver.cast(),
        ExecutionLevel: _WDF_EXECUTION_LEVEL::WdfExecutionLevelInheritFromParent,
        SynchronizationScope: _WDF_SYNCHRONIZATION_SCOPE::WdfSynchronizationScopeInheritFromParent,
        ..Default::default()
    };
    let mut lock = ptr::null_mut();
    let status = unsafe { wdf!(WdfWaitLockCreate, &mut a, &mut lock) };
    if status >= 0 {
        LOCK.store(lock, Ordering::Release);
    }
    status
}
unsafe fn acquire() -> WDFWAITLOCK {
    let lock = LOCK.load(Ordering::Acquire);
    // No timeout/alertable wait: a live lock always returns success.
    unsafe {
        let _ = wdf!(WdfWaitLockAcquire, lock, ptr::null_mut());
    }
    lock
}
pub(super) unsafe fn instance(init: PWDFDEVICE_INIT) -> Result<[u16; ID_LENGTH], NTSTATUS> {
    unsafe { property(init, &INSTANCE_ID) }
}
unsafe fn property(init: PWDFDEVICE_INIT, key: &DEVPROPKEY) -> Result<[u16; ID_LENGTH], NTSTATUS> {
    let mut value = [0u16; ID_LENGTH];
    let mut property = WDF_DEVICE_PROPERTY_DATA {
        Size: structure_size::<WDF_DEVICE_PROPERTY_DATA>(),
        PropertyKey: key,
        ..Default::default()
    };
    let mut length = 0;
    let mut kind = 0;
    let status = unsafe {
        wdf!(
            WdfFdoInitQueryPropertyEx,
            init,
            &mut property,
            mem::size_of_val(&value) as u32,
            value.as_mut_ptr().cast(),
            &mut length,
            &mut kind
        )
    };
    if status < 0 {
        return Err(status);
    }
    if kind != DEVPROP_TYPE_STRING
        || length < 2
        || length as usize > mem::size_of_val(&value)
        || length % 2 != 0
        || value[length as usize / 2 - 1] != 0
    {
        return Err(STATUS_INVALID_PARAMETER);
    }
    Ok(value)
}
fn equal_id(a: &[u16; ID_LENGTH], b: &[u16; ID_LENGTH]) -> bool {
    // Our generated controller instance IDs are ASCII. Case-fold without allocation; unknown
    // Unicode IDs simply fail to match rather than choosing another controller.
    fn upper(c: u16) -> u16 {
        if (97..=122).contains(&c) { c - 32 } else { c }
    }
    a.iter().zip(b).all(|(a, b)| upper(*a) == upper(*b))
}
pub(super) unsafe fn register(device: WDFDEVICE, id: [u16; ID_LENGTH]) -> NTSTATUS {
    let lock = unsafe { acquire() };
    let entries = unsafe { &mut *REGISTRY.0.get() };
    let status = if entries
        .iter()
        .any(|e| !e.controller.is_null() && equal_id(&e.id, &id))
    {
        STATUS_OBJECT_NAME_COLLISION
    } else if let Some(entry) = entries.iter_mut().find(|e| e.controller.is_null()) {
        *entry = Entry {
            controller: device,
            filter: ptr::null_mut(),
            id,
        };
        STATUS_SUCCESS
    } else {
        STATUS_INSUFFICIENT_RESOURCES
    };
    unsafe { wdf!(WdfWaitLockRelease, lock) };
    status
}
pub(super) unsafe fn unregister(device: WDFDEVICE) {
    let lock = unsafe { acquire() };
    for e in unsafe { &mut *REGISTRY.0.get() } {
        if e.controller == device {
            *e = EMPTY;
        }
    }
    unsafe { wdf!(WdfWaitLockRelease, lock) };
}
pub(super) unsafe fn ready(device: WDFDEVICE) -> bool {
    let lock = unsafe { acquire() };
    let found = unsafe { &*REGISTRY.0.get() }
        .iter()
        .any(|e| e.controller == device && !e.filter.is_null());
    unsafe { wdf!(WdfWaitLockRelease, lock) };
    found
}
// Zero leaves the native bus interface unchanged for non-ISO devices.
pub(super) unsafe fn set_iso_clock(device: WDFDEVICE, speed: u32) {
    let lock = unsafe { acquire() };
    for entry in unsafe { &*REGISTRY.0.get() } {
        if entry.controller == device && !entry.filter.is_null() {
            unsafe {
                (*context(entry.filter))
                    .iso_speed
                    .store(speed, Ordering::Release);
            }
        }
    }
    unsafe {
        wdf!(WdfWaitLockRelease, lock);
    }
}
// Called before consuming the init object. Only the recorded parent's hub can
// bind; a matching hardware ID on a physical controller is insufficient.
pub(super) unsafe fn try_add(init: PWDFDEVICE_INIT) -> Option<NTSTATUS> {
    let mut ids = [0u16; 512];
    let mut needed = 0;
    let status = unsafe {
        wdf!(
            WdfFdoInitQueryProperty,
            init,
            DEVICE_REGISTRY_PROPERTY::DevicePropertyHardwareID,
            mem::size_of_val(&ids) as u32,
            ids.as_mut_ptr().cast(),
            &mut needed
        )
    };
    if status < 0 {
        return Some(status);
    }
    if !ids
        .iter()
        .zip(b"USB\\ROOT_HUB30")
        .all(|(a, b)| *a == u16::from(*b))
    {
        return None;
    }
    let id = match unsafe { property(init, &PARENT_ID) } {
        Ok(id) => id,
        Err(s) => return Some(s),
    };
    let lock = unsafe { acquire() };
    let controller = unsafe { &*REGISTRY.0.get() }
        .iter()
        .find(|e| !e.controller.is_null() && e.filter.is_null() && equal_id(&e.id, &id))
        .map_or(ptr::null_mut(), |e| e.controller);
    if !controller.is_null() {
        unsafe {
            wdf!(
                WdfObjectReferenceActual,
                controller.cast(),
                ptr::null_mut(),
                0,
                ptr::null()
            )
        };
    }
    unsafe { wdf!(WdfWaitLockRelease, lock) };
    if controller.is_null() {
        return Some(STATUS_NO_SUCH_DEVICE);
    }
    Some(unsafe { add(init, controller) })
}
unsafe fn add(mut init: PWDFDEVICE_INIT, controller: WDFDEVICE) -> NTSTATUS {
    unsafe { wdf!(WdfFdoInitSetFilter, init) };
    let status = unsafe { clock::install(init) };
    if status < 0 {
        unsafe {
            wdf!(
                WdfObjectDereferenceActual,
                controller.cast(),
                ptr::null_mut(),
                0,
                ptr::null()
            );
        }
        return status;
    }
    let mut a = WDF_OBJECT_ATTRIBUTES {
        Size: structure_size::<WDF_OBJECT_ATTRIBUTES>(),
        ExecutionLevel: _WDF_EXECUTION_LEVEL::WdfExecutionLevelDispatch,
        SynchronizationScope: _WDF_SYNCHRONIZATION_SCOPE::WdfSynchronizationScopeNone,
        ContextTypeInfo: ptr::addr_of!(CONTEXT_TYPE),
        EvtCleanupCallback: Some(cleanup),
        ..Default::default()
    };
    let mut device = ptr::null_mut();
    let status = unsafe { wdf!(WdfDeviceCreate, &mut init, &mut a, &mut device) };
    if status < 0 {
        unsafe {
            wdf!(
                WdfObjectDereferenceActual,
                controller.cast(),
                ptr::null_mut(),
                0,
                ptr::null()
            )
        };
        return status;
    }
    unsafe {
        (*context(device)).controller = controller;
    }
    let mut q = WDF_IO_QUEUE_CONFIG {
        Size: structure_size::<WDF_IO_QUEUE_CONFIG>(),
        DispatchType: _WDF_IO_QUEUE_DISPATCH_TYPE::WdfIoQueueDispatchParallel,
        PowerManaged: _WDF_TRI_STATE::WdfFalse,
        DefaultQueue: 1,
        EvtIoInternalDeviceControl: Some(internal),
        ..Default::default()
    };
    // Zero-initialization would allow no presented requests and deadlock hub
    // startup. Ordinary traffic is sent onward immediately; only intercepted control
    // requests wait for remote completion (bounded further by the EP0 slot).
    q.Settings.Parallel.NumberOfPresentedRequests = 32;
    let status = unsafe {
        wdf!(
            WdfIoQueueCreate,
            device,
            &mut q,
            ptr::null_mut(),
            ptr::null_mut()
        )
    };
    if status < 0 {
        return status;
    }
    let lock = unsafe { acquire() };
    let entry = unsafe { &mut *REGISTRY.0.get() }
        .iter_mut()
        .find(|e| e.controller == controller && e.filter.is_null());
    let status = if let Some(entry) = entry {
        entry.filter = device;
        STATUS_SUCCESS
    } else {
        STATUS_NO_SUCH_DEVICE
    };
    unsafe { wdf!(WdfWaitLockRelease, lock) };
    status
}
unsafe extern "C" fn cleanup(object: WDFOBJECT) {
    // Device cleanup is PASSIVE_LEVEL. Drop the reference only after removing
    // publication, and never touch controller-owned child objects here.
    let controller = unsafe { (*context(object.cast())).controller };
    if controller.is_null() {
        return;
    }
    let lock = unsafe { acquire() };
    for e in unsafe { &mut *REGISTRY.0.get() } {
        if e.controller == controller && e.filter == object.cast() {
            e.filter = ptr::null_mut();
        }
    }
    unsafe {
        wdf!(WdfWaitLockRelease, lock);
        wdf!(
            WdfObjectDereferenceActual,
            controller.cast(),
            ptr::null_mut(),
            0,
            ptr::null()
        );
    }
}
unsafe extern "C" fn internal(
    queue: WDFQUEUE,
    request: WDFREQUEST,
    _out: usize,
    _input: usize,
    code: u32,
) {
    let mut p = WDF_REQUEST_PARAMETERS {
        Size: mem::size_of::<WDF_REQUEST_PARAMETERS>() as u16,
        ..Default::default()
    };
    unsafe { wdf!(WdfRequestGetParameters, request, &mut p) };
    if code == 0x0022_0003 {
        // IOCTL_INTERNAL_USB_SUBMIT_URB, kernel callers only.
        let header = unsafe { p.Parameters.Others.Arg1.cast::<_URB_HEADER>() };
        if !header.is_null() {
            // Frame numbers are controller-local milliseconds. Keep queries and
            // ASAP completion metadata on the same monotonic clock.
            if unsafe { (*header).Function } == 0x0007
                && unsafe {
                    (*context(wdf!(WdfIoQueueGetDevice, queue)))
                        .iso_speed
                        .load(Ordering::Acquire)
                } != 0
            {
                let status = if unsafe { usize::from((*header).Length) }
                    >= mem::size_of::<_URB_GET_CURRENT_FRAME_NUMBER>()
                {
                    unsafe {
                        (*header.cast::<_URB_GET_CURRENT_FRAME_NUMBER>()).FrameNumber =
                            super::channel::current_frame();
                    }
                    STATUS_SUCCESS
                } else {
                    STATUS_INVALID_PARAMETER
                };
                unsafe {
                    let device = wdf!(WdfIoQueueGetDevice, queue);
                    super::channel::complete_filtered(
                        (*context(device)).controller,
                        request,
                        status,
                    );
                }
                return;
            }
            let setup = unsafe {
                match (*header).Function {
                    0x0008
                        if usize::from((*header).Length)
                            >= mem::size_of::<_URB_CONTROL_TRANSFER>() =>
                    {
                        Some((
                            (*header.cast::<_URB_CONTROL_TRANSFER>()).SetupPacket,
                            (*header.cast::<_URB_CONTROL_TRANSFER>()).TransferBufferLength,
                        ))
                    }
                    0x0032
                        if usize::from((*header).Length)
                            >= mem::size_of::<_URB_CONTROL_TRANSFER_EX>() =>
                    {
                        Some((
                            (*header.cast::<_URB_CONTROL_TRANSFER_EX>()).SetupPacket,
                            (*header.cast::<_URB_CONTROL_TRANSFER_EX>()).TransferBufferLength,
                        ))
                    }
                    // URB_FUNCTION_CLEAR_FEATURE_TO_ENDPOINT has a typed
                    // feature/index pair instead of a raw control setup packet.
                    0x0012
                        if usize::from((*header).Length)
                            >= mem::size_of::<_URB_CONTROL_FEATURE_REQUEST>() =>
                    {
                        let feature = &*header.cast::<_URB_CONTROL_FEATURE_REQUEST>();
                        let value = feature.FeatureSelector.to_le_bytes();
                        let index = feature.Index.to_le_bytes();
                        Some(([2, 1, value[0], value[1], index[0], index[1], 0, 0], 0))
                    }
                    _ => None,
                }
            };
            if let Some((setup, length)) = setup {
                if matches!((setup[0], setup[1]), (0, 9) | (1, 11) | (2, 1)) {
                    let device = unsafe { wdf!(WdfIoQueueGetDevice, queue) };
                    let controller = unsafe { (*context(device)).controller };
                    if unsafe {
                        super::channel::forward_standard_control(controller, request, setup, length)
                    } {
                        return;
                    }
                }
            }
        }
    }
    unsafe { forward(request) };
}
// Exclusive, non-cancelable request. Send-and-forget relinquishes it on success;
// the lower stack (including UDE) still processes the original setup packet.
pub(super) unsafe fn forward(request: WDFREQUEST) {
    unsafe {
        let queue = wdf!(WdfRequestGetIoQueue, request);
        let device = wdf!(WdfIoQueueGetDevice, queue);
        let target = wdf!(WdfDeviceGetIoTarget, device);
        wdf!(WdfRequestFormatRequestUsingCurrentType, request);
        let mut options = WDF_REQUEST_SEND_OPTIONS {
            Size: structure_size::<WDF_REQUEST_SEND_OPTIONS>(),
            Flags: _WDF_REQUEST_SEND_OPTIONS_FLAGS::WDF_REQUEST_SEND_OPTION_SEND_AND_FORGET as u32,
            ..Default::default()
        };
        if wdf!(WdfRequestSend, request, target, &mut options) == 0 {
            let status = wdf!(WdfRequestGetStatus, request);
            let controller = (*context(device)).controller;
            super::channel::complete_filtered(controller, request, status);
        }
    }
}
