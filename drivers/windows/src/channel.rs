//! Safety model: WDF zero-initializes nonpaged contexts. Initialization happens
//! before I/O. The passive gate serializes our IOCTLs and owner cleanup. The spin
//! lock protects slots and the intrusive completion list; no borrowed context
//! references survive a lock boundary. Only successful UnmarkCancelable grants
//! the response path exclusive request ownership. Cancel callbacks own all other
//! cancel completions. Every URB completes from our separate DPC, never inline.

use super::{structure_size, ude_entry};
use core::{
    mem, ptr,
    sync::atomic::{AtomicU64, Ordering},
};
use wdk_sys::{call_unsafe_wdf_function_binding as wdf, *};

const LIMIT: usize = 65536;
const ENDPOINT_COUNT: usize = 31;
const QUEUE_COUNT: usize = ENDPOINT_COUNT * 2;
const STATS_BYTES: usize = (8 + ENDPOINT_COUNT) * 8;
const HEADER: usize = 32;
const SUBMIT_URB: u32 = 0x0022_0003; // USB IOCTL_INTERNAL_USB_SUBMIT_URB.
// FILE_DEVICE_UNKNOWN, METHOD_BUFFERED, FILE_READ_DATA | FILE_WRITE_DATA.
const FETCH: u32 = 0x0022_e404;
const REPLY: u32 = 0x0022_e408;
const DETACH: u32 = 0x0022_e40c;
const STATS: u32 = 0x0022_e410;
const LIFECYCLE: u32 = 0x0022_e414;
const ATTACH_PHYSICAL: u32 = 0x0022_e424;
const RETIREMENT_READY: u32 = 0x0022_e42c;
const CHILD_CREATED: usize = 0;
const CHILD_CLEANUP: usize = 1;
const CHILD_DESTROYED: usize = 2;
const DEFAULT_ADDED: usize = 3;
const ENDPOINT_CREATED: usize = 4;
const ENDPOINT_CLEANUP: usize = 5;
const ENDPOINT_DESTROYED: usize = 6;
const QUEUE_CREATED: usize = 7;
const QUEUE_CLEANUP: usize = 8;
const QUEUE_DESTROYED: usize = 9;
const PLUGOUT_CALLS: usize = 10;
const PLUGOUT_SUCCEEDED: usize = 11;
const UNPUBLISHED_DELETED: usize = 13;
// Image lifetime, not device-context lifetime: a destroy callback must not
// dereference its retired object or assume its controller context still exists.
// Atomics allocate nothing and are usable at every callback IRQL on this x64 ABI.
static LIFETIMES: [AtomicU64; 14] = [const { AtomicU64::new(0) }; 14];
fn observe(index: usize) {
    LIFETIMES[index].fetch_add(1, Ordering::Relaxed);
}
unsafe extern "C" fn child_cleanup_observation(_object: WDFOBJECT) {
    observe(CHILD_CLEANUP);
}
unsafe extern "C" fn child_destroy_observation(_object: WDFOBJECT) {
    observe(CHILD_DESTROYED);
}
unsafe extern "C" fn endpoint_cleanup_observation(_object: WDFOBJECT) {
    observe(ENDPOINT_CLEANUP);
}
unsafe extern "C" fn endpoint_destroy_observation(_object: WDFOBJECT) {
    observe(ENDPOINT_DESTROYED);
}
unsafe extern "C" fn queue_destroy_observation(_object: WDFOBJECT) {
    observe(QUEUE_DESTROYED);
}
const LAB_INTERFACE: GUID = GUID {
    Data1: 0x53baf142,
    Data2: 0xc628,
    Data3: 0x47de,
    Data4: [0xb8, 0x91, 0x5c, 0x71, 0xe3, 0x8b, 0x96, 0x20],
};

// SAFETY: This macro is used only with a matching generated SDK index/PFN,
// inside live framework callbacks. It adds no guessed foreign declaration.
macro_rules! ude {
    ($index:ident, $pfn:ty $(, $arg:expr)* $(,)?) => {{
        let (globals, raw) = unsafe { ude_entry(_UDECXFUNCENUM::$index) }?;
        let typed: $pfn = unsafe { mem::transmute(raw) };
        match typed {
            Some(call) => Ok(unsafe { call(globals, $($arg),*) }),
            None => Err(STATUS_DEVICE_NOT_READY),
        }
    }};
}

#[repr(C)]
struct Slot {
    request: WDFREQUEST,
    token: u64,
    length: u32,
    fetched: u8,
    input: u8,
    setup: [u8; 8],
    data: [u8; LIMIT],
}
#[repr(C)]
struct State {
    retirement: super::lifetime::Retirement,
    endpoints: [u8; ENDPOINT_COUNT], // Immutable from attach through final child retirement.
    physical_device: [u8; 18],
    physical_configuration: [u8; 4096],
    physical_length: usize,
    speed: u32,
    lock: WDFSPINLOCK,
    gate: WDFWAITLOCK,
    dpc: WDFDPC,
    head: WDFREQUEST,
    filtered_pending: u32, // Accepted filter requests, including DPC delivery.
    failed_head: PIRP,     // Context-allocation failures; owned, non-cancelable IRPs only.
    owner: WDFFILEOBJECT,
    quarantined: u8,
    closing: u8, // Protected by lock; rejects newly delivered URBs during teardown.
    child: UDECXUSBDEVICE,
    child_published: u8, // Only the passive owner gate reads/writes publication.
    queues: [WDFQUEUE; QUEUE_COUNT],
    queue_reservations: u64,
    next_token: u64,
    next_fetch: usize, // Round-robin cursor, protected by the slot spin lock.
    submitted: u64,
    completed: u64,
    canceled: u64,
    rejected: u64,
    dpc_wrong_irql: u64,
    maximum_length: [u32; ENDPOINT_COUNT],
    last_urb_error: [u32; ENDPOINT_COUNT],
    starts: [u64; ENDPOINT_COUNT],
    resets: [u64; ENDPOINT_COUNT],
    admitted_by_endpoint: [u64; ENDPOINT_COUNT],
    fetched_by_endpoint: [u64; ENDPOINT_COUNT],
    canceled_by_endpoint: [u64; ENDPOINT_COUNT],
    delivered: u64,
    queued_cancels: u64,
    slots: [Slot; ENDPOINT_COUNT],
}
#[repr(C)]
struct RequestState {
    next: WDFREQUEST,
    status: NTSTATUS,
    controller: WDFDEVICE,
    slot: usize,
    operation: u8, // 0=URB, 1=configuration/endpoint reset notification.
}
#[repr(C)]
struct QueueState {
    index: usize,
    storage: usize,
    controller: WDFDEVICE,
}
#[repr(C)]
struct EndpointState {
    queue: WDFQUEUE,
}
#[repr(C)]
struct ChildState {
    controller: WDFDEVICE,
    default_added: u8, // Protected by the controller spin lock while child is live.
}

// Image-lifetime identities; WDF reads these but never mutates them. Raw
// addresses avoid creating references to foreign mutable context storage.
macro_rules! context_type {
    ($name:ident, $ty:ty, $label:expr) => {
        static mut $name: WDF_OBJECT_CONTEXT_TYPE_INFO = WDF_OBJECT_CONTEXT_TYPE_INFO {
            Size: mem::size_of::<WDF_OBJECT_CONTEXT_TYPE_INFO>() as u32,
            ContextName: $label.as_ptr(),
            ContextSize: mem::size_of::<$ty>(),
            UniqueType: ptr::null(),
            EvtDriverGetUniqueContextType: None,
        };
    };
}
context_type!(STATE_TYPE, State, c"KnightBusState");
context_type!(REQUEST_TYPE, RequestState, c"KnightBusRequest");
context_type!(QUEUE_TYPE, QueueState, c"KnightBusQueue");
context_type!(ENDPOINT_TYPE, EndpointState, c"KnightBusEndpoint");
context_type!(CHILD_TYPE, ChildState, c"KnightBusChild");

// SAFETY: All callers use an object created with the exact matching attributes;
// WDF holds its context alive through callbacks and owned request completion.
unsafe fn state(device: WDFDEVICE) -> *mut State {
    unsafe {
        wdf!(
            WdfObjectGetTypedContextWorker,
            device.cast(),
            ptr::addr_of!(STATE_TYPE)
        )
        .cast()
    }
}
unsafe fn request_state(request: WDFREQUEST) -> *mut RequestState {
    unsafe {
        wdf!(
            WdfObjectGetTypedContextWorker,
            request.cast(),
            ptr::addr_of!(REQUEST_TYPE)
        )
        .cast()
    }
}
// A request forwarded from another WDF driver need not carry our device-init
// attributes. Attach our context before publication/marking it cancelable.
unsafe fn prepare_request(device: WDFDEVICE, request: WDFREQUEST, urb: bool) -> bool {
    if !unsafe { request_state(request) }.is_null() {
        return true;
    }
    let mut attributes = WDF_OBJECT_ATTRIBUTES {
        Size: structure_size::<WDF_OBJECT_ATTRIBUTES>(),
        ExecutionLevel: _WDF_EXECUTION_LEVEL::WdfExecutionLevelInheritFromParent,
        SynchronizationScope: _WDF_SYNCHRONIZATION_SCOPE::WdfSynchronizationScopeInheritFromParent,
        ContextTypeInfo: ptr::addr_of!(REQUEST_TYPE),
        ..Default::default()
    };
    let mut context: PVOID = ptr::null_mut();
    let status = unsafe {
        wdf!(
            WdfObjectAllocateContext,
            request.cast(),
            &mut attributes,
            &mut context
        )
    };
    if status >= 0 && !context.is_null() {
        return true;
    }
    if urb {
        // No fallback allocation or inline URB completion, even under OOM.
        unsafe { fail_without_context(device, request, STATUS_INSUFFICIENT_RESOURCES) };
    } else {
        // UDE management notifications are not endpoint URBs and may complete
        // synchronously. No request context is needed on this failure path.
        unsafe { wdf!(WdfRequestComplete, request, STATUS_INSUFFICIENT_RESOURCES) };
    }
    false
}

// WDM documents DriverContext as four driver-owned pointers while an IRP is
// exclusively owned and not queued elsewhere. These requests have never been
// marked cancelable, or are handed to EvtIoCanceledOnQueue. Use only entries
// 0..2; entry 3 belongs to KMDF's cancel-queue bookkeeping. Clear our entries
// before completing through WDF. Never use these fields on an active slot.
unsafe fn fail_without_context(device: WDFDEVICE, request: WDFREQUEST, status: NTSTATUS) {
    let s = unsafe { state(device) };
    let irp = unsafe { wdf!(WdfRequestWdmGetIrp, request) };
    unsafe {
        let context = &mut (*irp)
            .Tail
            .Overlay
            .__bindgen_anon_1
            .__bindgen_anon_1
            .DriverContext;
        context[0] = request.cast();
        context[2] = status as u32 as usize as PVOID;
        wdf!(WdfSpinLockAcquire, (*s).lock);
        context[1] = (*s).failed_head.cast();
        (*s).failed_head = irp;
        wdf!(WdfSpinLockRelease, (*s).lock);
        let _ = wdf!(WdfDpcEnqueue, (*s).dpc);
    }
}

unsafe fn queue_index(queue: WDFQUEUE) -> usize {
    unsafe {
        (*wdf!(
            WdfObjectGetTypedContextWorker,
            queue.cast(),
            ptr::addr_of!(QUEUE_TYPE)
        )
        .cast::<QueueState>())
        .index
    }
}

/// # Safety
/// Live unconsumed init at passive level; no requests have been created yet.
pub(super) unsafe fn configure_init(init: PWDFDEVICE_INIT) {
    let mut attributes = WDF_OBJECT_ATTRIBUTES {
        Size: structure_size::<WDF_OBJECT_ATTRIBUTES>(),
        ExecutionLevel: _WDF_EXECUTION_LEVEL::WdfExecutionLevelInheritFromParent,
        SynchronizationScope: _WDF_SYNCHRONIZATION_SCOPE::WdfSynchronizationScopeInheritFromParent,
        ContextTypeInfo: ptr::addr_of!(REQUEST_TYPE),
        ..Default::default()
    };
    let mut files = WDF_FILEOBJECT_CONFIG {
        Size: structure_size::<WDF_FILEOBJECT_CONFIG>(),
        EvtFileCleanup: Some(file_cleanup),
        FileObjectClass: _WDF_FILEOBJECT_CLASS::WdfFileObjectWdfCanUseFsContext,
        AutoForwardCleanupClose: _WDF_TRI_STATE::WdfFalse,
        ..Default::default()
    };
    let mut file_attributes = WDF_OBJECT_ATTRIBUTES {
        Size: structure_size::<WDF_OBJECT_ATTRIBUTES>(),
        ExecutionLevel: _WDF_EXECUTION_LEVEL::WdfExecutionLevelPassive,
        SynchronizationScope: _WDF_SYNCHRONIZATION_SCOPE::WdfSynchronizationScopeNone,
        ..Default::default()
    };
    // SAFETY: Configuration is copied synchronously before WdfDeviceCreate.
    unsafe {
        wdf!(WdfDeviceInitSetRequestAttributes, init, &mut attributes);
        wdf!(
            WdfDeviceInitSetFileObjectConfig,
            init,
            &mut files,
            &mut file_attributes
        );
    }
}
pub(super) unsafe fn configure_attributes(attributes: &mut WDF_OBJECT_ATTRIBUTES) {
    attributes.ContextTypeInfo = ptr::addr_of!(STATE_TYPE);
    attributes.EvtCleanupCallback = Some(device_cleanup);
    attributes.EvtDestroyCallback = Some(device_destroy);
}

// The context is still exclusively owned by device-add at PASSIVE_LEVEL.
pub(super) unsafe fn bind_retirement(device: WDFDEVICE, instance: &[u16; 200]) -> NTSTATUS {
    unsafe { (*state(device)).retirement.bind(instance) }
}
unsafe extern "C" fn device_destroy(object: WDFOBJECT) {
    // Context access is allowed here; no methods operate on the retiring device.
    unsafe {
        (*state(object.cast())).retirement.complete();
    }
}

/// # Safety
/// Called once at passive level, before child/device-channel I/O is possible.
pub(super) unsafe fn initialize(device: WDFDEVICE) -> NTSTATUS {
    // SAFETY: Matching FDO context allocated by WDF during creation.
    let s = unsafe { state(device) };
    unsafe {
        (*s).closing = 1;
    } // No filtered forwarding before attach.
    let mut attributes = WDF_OBJECT_ATTRIBUTES {
        Size: structure_size::<WDF_OBJECT_ATTRIBUTES>(),
        ExecutionLevel: _WDF_EXECUTION_LEVEL::WdfExecutionLevelInheritFromParent,
        SynchronizationScope: _WDF_SYNCHRONIZATION_SCOPE::WdfSynchronizationScopeInheritFromParent,
        ParentObject: device.cast(),
        ..Default::default()
    };
    // SAFETY: Fixed parent-owned primitives; no callbacks can access them yet.
    let status = unsafe {
        wdf!(
            WdfSpinLockCreate,
            &mut attributes,
            ptr::addr_of_mut!((*s).lock)
        )
    };
    unsafe { initialization_stage(device, 8, status) };
    if status < 0 {
        return status;
    }
    let status = unsafe {
        wdf!(
            WdfWaitLockCreate,
            &mut attributes,
            ptr::addr_of_mut!((*s).gate)
        )
    };
    unsafe { initialization_stage(device, 9, status) };
    if status < 0 {
        return status;
    }
    let mut config = WDF_DPC_CONFIG {
        Size: structure_size::<WDF_DPC_CONFIG>(),
        EvtDpcFunc: Some(complete_dpc),
        AutomaticSerialization: 0,
        ..Default::default()
    };
    let status = unsafe {
        wdf!(
            WdfDpcCreate,
            &mut config,
            &mut attributes,
            ptr::addr_of_mut!((*s).dpc)
        )
    };
    unsafe { initialization_stage(device, 10, status) };
    if status < 0 {
        return status;
    }
    // SAFETY: Live FDO and our independently selected private lab GUID.
    unsafe {
        wdf!(
            WdfDeviceCreateDeviceInterface,
            device,
            &LAB_INTERFACE,
            ptr::null()
        )
    }
}

// SAFETY: Called only synchronously during passive initialization; no payload.
unsafe fn initialization_stage(device: WDFDEVICE, stage: u32, status: NTSTATUS) {
    let driver = unsafe { wdf!(WdfDeviceGetDriver, device) };
    let object = unsafe { wdf!(WdfDriverWdmGetDriverObject, driver) };
    unsafe { super::trace_stage(object, stage, status) };
}

// The caller owns the request exclusively, not marked cancelable. At <=DISPATCH
// queue its prepared framework context, requiring no allocation on failure.
unsafe fn finish(device: WDFDEVICE, request: WDFREQUEST, status: NTSTATUS) {
    let s = unsafe { state(device) };
    let r = unsafe { request_state(request) };
    // SAFETY: Exclusive request ownership; fixed header fields are private.
    unsafe {
        (*r).status = status;
        wdf!(WdfSpinLockAcquire, (*s).lock);
        (*r).next = (*s).head;
        (*s).head = request;
        wdf!(WdfSpinLockRelease, (*s).lock);
        let _ = wdf!(WdfDpcEnqueue, (*s).dpc);
    }
}

unsafe extern "C" fn complete_dpc(dpc: WDFDPC) {
    // SAFETY: The framework's DPC parent is this FDO; it stays live throughout.
    let device = unsafe { wdf!(WdfDpcGetParentObject, dpc) }.cast();
    let s = unsafe { state(device) };
    loop {
        // SAFETY: Only list operations/counters occur under this lock. Never
        // complete while locked: completion can call another endpoint handler.
        let (request, mut status) = unsafe {
            wdf!(WdfSpinLockAcquire, (*s).lock);
            let pair = if !(*s).failed_head.is_null() {
                let irp = (*s).failed_head;
                let context = &mut (*irp)
                    .Tail
                    .Overlay
                    .__bindgen_anon_1
                    .__bindgen_anon_1
                    .DriverContext;
                let request = context[0].cast();
                let status = context[2] as usize as u32 as NTSTATUS;
                (*s).failed_head = context[1].cast();
                context[..3].fill(ptr::null_mut());
                (request, status)
            } else {
                let request = (*s).head;
                if request.is_null() {
                    (request, STATUS_SUCCESS)
                } else {
                    let r = request_state(request);
                    (*s).head = (*r).next;
                    (request, (*r).status)
                }
            };
            wdf!(WdfSpinLockRelease, (*s).lock);
            pair
        };
        if request.is_null() {
            break;
        }
        let context = unsafe { request_state(request) };
        let filtered = !context.is_null() && unsafe { (*context).operation == 2 };
        if filtered && status >= 0 {
            let closing = unsafe {
                wdf!(WdfSpinLockAcquire, (*s).lock);
                let closing = (*s).closing != 0;
                wdf!(WdfSpinLockRelease, (*s).lock);
                closing
            };
            if closing {
                status = STATUS_CANCELLED;
            } else {
                unsafe {
                    super::filter::forward(request);
                    wdf!(WdfSpinLockAcquire, (*s).lock);
                    (*s).filtered_pending -= 1;
                    wdf!(WdfSpinLockRelease, (*s).lock);
                }
                continue;
            }
        }
        let mut parameters = WDF_REQUEST_PARAMETERS {
            Size: mem::size_of::<WDF_REQUEST_PARAMETERS>() as u16,
            ..Default::default()
        };
        // SAFETY: We still exclusively own the uncompleted kernel request.
        unsafe { wdf!(WdfRequestGetParameters, request, &mut parameters) };
        if parameters.Type == _WDF_REQUEST_TYPE::WdfRequestTypeDeviceControlInternal {
            // SAFETY: This union view is the documented USB internal IOCTL ABI.
            let others = unsafe { parameters.Parameters.Others };
            if others.IoControlCode == SUBMIT_URB && !others.Arg1.is_null() {
                let urb = others.Arg1.cast::<_URB_HEADER>();
                // SAFETY: Only OS-created internal USB URBs reach this queue;
                // Argument1 is their kernel buffer, never a user channel pointer.
                // Check the declared size before writing its common status.
                unsafe {
                    if usize::from((*urb).Length) >= mem::size_of::<_URB_HEADER>() {
                        (*urb).Status = if status >= 0 {
                            0
                        } else if status == STATUS_CANCELLED {
                            0xc001_0000u32 // USBD_STATUS_CANCELED.
                        } else if status == STATUS_NOT_SUPPORTED {
                            0xc000_0004u32 // USBD_STATUS_STALL_PID.
                        } else {
                            0x8000_0800u32 // USBD_STATUS_INTERNAL_HC_ERROR.
                        } as USBD_STATUS;
                    }
                }
            }
        }
        // Sample outside the lock: taking the spin lock itself raises IRQL and
        // would otherwise hide the execution level at actual completion.
        let completion_irql = unsafe { wdk_sys::ntddk::KeGetCurrentIrql() };
        if completion_irql != 2 {
            unsafe {
                wdf!(WdfSpinLockAcquire, (*s).lock);
                (*s).dpc_wrong_irql += 1;
                wdf!(WdfSpinLockRelease, (*s).lock);
            }
        }
        // SAFETY: Sole completion owner, separate DPC at DISPATCH_LEVEL. No
        // UdeCx PASSIVE-only completion helper is called. No request access later.
        unsafe { wdf!(WdfRequestComplete, request, status) };
        // The request is invalid now; only the DPC-parented device context is
        // accessed. Count completed calls, rather than merely dequeued work.
        unsafe {
            wdf!(WdfSpinLockAcquire, (*s).lock);
            (*s).completed += 1;
            if filtered {
                (*s).filtered_pending -= 1;
            }
            wdf!(WdfSpinLockRelease, (*s).lock);
        }
    }
}

unsafe extern "C" fn cancel(request: WDFREQUEST) {
    // SAFETY: WDF invokes this only for a marked request still owned by us.
    let context = unsafe { request_state(request) };
    let device = unsafe { (*context).controller };
    let index = unsafe { (*context).slot };
    let s = unsafe { state(device) };
    // SAFETY: Index was fixed at queue creation; source queue is sequential.
    unsafe {
        wdf!(WdfSpinLockAcquire, (*s).lock);
        if index < ENDPOINT_COUNT && (*s).slots[index].request == request {
            (*s).slots[index].request = ptr::null_mut();
        }
        (*s).canceled += 1;
        if index < ENDPOINT_COUNT {
            (*s).canceled_by_endpoint[index] += 1;
        }
        wdf!(WdfSpinLockRelease, (*s).lock);
        finish(device, request, STATUS_CANCELLED);
    }
}
unsafe extern "C" fn canceled_on_queue(queue: WDFQUEUE, request: WDFREQUEST) {
    // SAFETY: Framework hands cancellation ownership to this callback. It was
    // never presented, so no slot points to it. Foreign requests may lack context.
    let device = unsafe { wdf!(WdfIoQueueGetDevice, queue) };
    let s = unsafe { state(device) };
    let index = unsafe { queue_index(queue) };
    unsafe {
        wdf!(WdfSpinLockAcquire, (*s).lock);
        (*s).canceled += 1;
        (*s).queued_cancels += 1;
        if index < ENDPOINT_COUNT {
            (*s).canceled_by_endpoint[index] += 1;
        }
        wdf!(WdfSpinLockRelease, (*s).lock);
        if request_state(request).is_null() {
            fail_without_context(device, request, STATUS_CANCELLED);
        } else {
            finish(device, request, STATUS_CANCELLED);
        }
    }
}

unsafe extern "C" fn urb(
    queue: WDFQUEUE,
    request: WDFREQUEST,
    _out: usize,
    _input: usize,
    code: u32,
) {
    // SAFETY: These queue callbacks were explicitly configured PASSIVE_LEVEL.
    let device = unsafe { wdf!(WdfIoQueueGetDevice, queue) };
    if !unsafe { prepare_request(device, request, true) } {
        return;
    }
    let s = unsafe { state(device) };
    unsafe {
        wdf!(WdfSpinLockAcquire, (*s).lock);
        (*s).delivered += 1;
        wdf!(WdfSpinLockRelease, (*s).lock);
    }
    if code != SUBMIT_URB {
        unsafe { finish(device, request, STATUS_INVALID_DEVICE_REQUEST) };
        return;
    }
    let index = unsafe { queue_index(queue) };
    let result = (|| -> Result<(), NTSTATUS> {
        if index >= ENDPOINT_COUNT {
            return Err(STATUS_INVALID_PARAMETER);
        }
        let mut setup = WDF_USB_CONTROL_SETUP_PACKET::default();
        let mut bytes = [0u8; 8];
        let input = if index == 0 {
            let status = ude!(
                UdecxUrbRetrieveControlSetupPacketTableIndex,
                PFN_UDECXURBRETRIEVECONTROLSETUPPACKET,
                request,
                &mut setup
            )?;
            if status < 0 {
                return Err(status);
            }
            // SAFETY: SDK union is exactly the eight USB setup packet bytes;
            // copying object representation avoids unaligned field references.
            unsafe {
                ptr::copy_nonoverlapping(ptr::addr_of!(setup).cast::<u8>(), bytes.as_mut_ptr(), 8)
            };
            bytes[0] & 0x80 != 0
        } else {
            unsafe { (*state(device)).endpoints[index] & 0x80 != 0 }
        };
        let mut buffer: *mut u8 = ptr::null_mut();
        let mut length = 0u32;
        // A no-data control transfer has no transfer buffer to map. In the
        // measured v6 run Get/SetLineCoding succeeded, but SetControlLineState
        // (wLength zero) failed before publication to userspace.
        if index != 0 || u16::from_le_bytes([bytes[6], bytes[7]]) != 0 {
            let status = ude!(
                UdecxUrbRetrieveBufferTableIndex,
                PFN_UDECXURBRETRIEVEBUFFER,
                request,
                &mut buffer,
                &mut length
            )?;
            if status < 0 {
                return Err(status);
            }
        }
        if index == 0 {
            let requested = u32::from(u16::from_le_bytes([bytes[6], bytes[7]]));
            if requested > length {
                return Err(STATUS_INVALID_BUFFER_SIZE);
            }
            length = requested;
        }
        let s = unsafe { state(device) };
        unsafe {
            wdf!(WdfSpinLockAcquire, (*s).lock);
            (*s).maximum_length[index] = (*s).maximum_length[index].max(length);
            wdf!(WdfSpinLockRelease, (*s).lock);
        }
        if length as usize > LIMIT || (length != 0 && buffer.is_null()) {
            return Err(STATUS_INVALID_BUFFER_SIZE);
        }
        let s = unsafe { state(device) };
        // SAFETY: Mapping belongs to our live URB. No copying after publishing
        // cancellation. The lock protects publication against FETCH/REPLY.
        let status = unsafe {
            wdf!(WdfSpinLockAcquire, (*s).lock);
            let token = (*s).next_token.checked_add(1);
            if (*s).closing != 0 {
                wdf!(WdfSpinLockRelease, (*s).lock);
                return Err(STATUS_CANCELLED);
            }
            if !(*s).slots[index].request.is_null() || token.is_none() {
                wdf!(WdfSpinLockRelease, (*s).lock);
                return Err(STATUS_DEVICE_BUSY);
            }
            let token = token.unwrap_or(0);
            (*s).next_token = token;
            let slot = ptr::addr_of_mut!((*s).slots[index]);
            (*slot).token = token;
            (*slot).length = length;
            (*slot).input = u8::from(input);
            (*slot).setup = bytes;
            (*slot).fetched = 0;
            if !input && length != 0 {
                ptr::copy_nonoverlapping(buffer, (*slot).data.as_mut_ptr(), length as usize);
            }
            (*slot).request = request;
            let context = request_state(request);
            (*context).controller = device;
            (*context).slot = index;
            (*context).operation = 0;
            (*s).submitted += 1;
            (*s).admitted_by_endpoint[index] += 1;
            // MarkCancelableEx never synchronously calls cancel on an already
            // canceled request. A concurrent cancel waits for this lock; it
            // cannot complete before publication is finished.
            let status = wdf!(WdfRequestMarkCancelableEx, request, Some(cancel));
            if status < 0 {
                (*slot).request = ptr::null_mut();
            }
            wdf!(WdfSpinLockRelease, (*s).lock);
            status
        };
        if status < 0 { Err(status) } else { Ok(()) }
    })();
    if let Err(status) = result {
        let s = unsafe { state(device) };
        unsafe {
            wdf!(WdfSpinLockAcquire, (*s).lock);
            if index < ENDPOINT_COUNT {
                (*s).last_urb_error[index] = status as u32;
            }
            wdf!(WdfSpinLockRelease, (*s).lock);
            finish(device, request, status);
        }
    }
}

unsafe extern "C" fn queue_cleanup(object: WDFOBJECT) {
    observe(QUEUE_CLEANUP);
    // SAFETY: WDF invokes queue cleanup at PASSIVE_LEVEL with its context live;
    // its endpoint parent chain keeps the controller context alive. No queue
    // methods or payload access after this callback returns.
    let q = unsafe {
        wdf!(
            WdfObjectGetTypedContextWorker,
            object,
            ptr::addr_of!(QUEUE_TYPE)
        )
        .cast::<QueueState>()
    };
    let device = unsafe { (*q).controller };
    if device.is_null() {
        return;
    } // Construction failed before publication.
    let storage = unsafe { (*q).storage };
    let s = unsafe { state(device) };
    unsafe {
        wdf!(WdfSpinLockAcquire, (*s).lock);
        if storage < QUEUE_COUNT && (*s).queues[storage].cast::<core::ffi::c_void>() == object {
            (*s).queues[storage] = ptr::null_mut();
        }
        wdf!(WdfSpinLockRelease, (*s).lock);
    }
}

unsafe fn endpoint_queue(endpoint: UDECXUSBENDPOINT) -> WDFQUEUE {
    unsafe {
        (*wdf!(
            WdfObjectGetTypedContextWorker,
            endpoint.cast(),
            ptr::addr_of!(ENDPOINT_TYPE)
        )
        .cast::<EndpointState>())
        .queue
    }
}
unsafe extern "C" fn start(endpoint: UDECXUSBENDPOINT) {
    // SAFETY: UDE invokes start only after endpoint construction/queue binding.
    // This resumes our own endpoint queue; no additional forwarded queues exist.
    let queue = unsafe { endpoint_queue(endpoint) };
    if queue.is_null() {
        return;
    }
    let device = unsafe { wdf!(WdfIoQueueGetDevice, queue) };
    let s = unsafe { state(device) };
    let index = unsafe { queue_index(queue) };
    unsafe {
        wdf!(WdfSpinLockAcquire, (*s).lock);
        if index < ENDPOINT_COUNT {
            (*s).starts[index] += 1;
        }
        wdf!(WdfSpinLockRelease, (*s).lock);
        // UdeCx owns queue start/purge state. Do not start its queue manually.
    }
}
unsafe extern "C" fn reset(endpoint: UDECXUSBENDPOINT, request: WDFREQUEST) {
    let queue = unsafe { endpoint_queue(endpoint) };
    if queue.is_null() {
        unsafe { wdf!(WdfRequestComplete, request, STATUS_DEVICE_NOT_READY) };
        return;
    }
    let device = unsafe { wdf!(WdfIoQueueGetDevice, queue) };
    let s = unsafe { state(device) };
    let index = unsafe { queue_index(queue) };
    unsafe {
        wdf!(WdfSpinLockAcquire, (*s).lock);
        if index < ENDPOINT_COUNT {
            (*s).resets[index] += 1;
        }
        wdf!(WdfSpinLockRelease, (*s).lock);
    }

    // Endpoint halt recovery must reach the physical device, not just the
    // local virtual pipe. USB/IP has no independent bus-reset wire operation.
    let address = unsafe { (*s).endpoints[index] };
    if address == 0 {
        unsafe { wdf!(WdfRequestComplete, request, STATUS_NOT_SUPPORTED) };
    } else {
        let setup = [0x02, 1, 0, 0, address, 0, 0, 0]; // CLEAR_FEATURE(ENDPOINT_HALT).
        unsafe { forward_control(device, request, setup, 1) };
    }
}

unsafe extern "C" fn device_reset(
    _controller: WDFDEVICE,
    _child: UDECXUSBDEVICE,
    request: WDFREQUEST,
    _all_devices: BOOLEAN,
) {
    // UDE's default virtual reset cannot reset the physical USB/IP peer. Reject
    // post-enumeration bus reset rather than falsely acknowledging hardware work.
    // This is a framework management request, not an endpoint URB.
    unsafe { wdf!(WdfRequestComplete, request, STATUS_NOT_SUPPORTED) };
}

// A filter send failure still owns its request. Complete it on the controller
// DPC, including the allocation-free fallback for missing request context.
pub(super) unsafe fn fail_filtered(device: WDFDEVICE, request: WDFREQUEST, status: NTSTATUS) {
    if unsafe { prepare_request(device, request, true) } {
        unsafe {
            (*request_state(request)).operation = 0;
            finish(device, request, status);
        }
    }
}
// The filter owns this request until it is sent onward or completed. Descriptor
// storage is immutable while admission is open; inspect it under the same lock
// that closes admission before retirement and the next attach.
pub(super) unsafe fn forward_standard_control(
    device: WDFDEVICE,
    request: WDFREQUEST,
    setup: [u8; 8],
    length: u32,
) -> bool {
    let s = unsafe { state(device) };
    let valid = unsafe {
        wdf!(WdfSpinLockAcquire, (*s).lock);
        if (*s).closing != 0 {
            wdf!(WdfSpinLockRelease, (*s).lock);
            return false;
        }
        let valid = length == 0
            && setup[3] == 0
            && setup[5..8] == [0; 3]
            && match (setup[0], setup[1]) {
                (0, 9) => {
                    setup[4] == 0 && (setup[2] == 0 || setup[2] == (*s).physical_configuration[5])
                }
                (1, 11) => allowed_setting(device, setup[4], setup[2]),
                // CLEAR_FEATURE(ENDPOINT_HALT): only a nondefault endpoint
                // from this attachment may be cleared. No virtual-only success.
                (2, 1) => setup[2] == 0 && setup[4] & 15 != 0 && (*s).endpoints.contains(&setup[4]),
                _ => false,
            };
        wdf!(WdfSpinLockRelease, (*s).lock);
        valid
    };
    if !valid {
        unsafe { fail_filtered(device, request, STATUS_INVALID_PARAMETER) };
    } else {
        unsafe { forward_control(device, request, setup, 2) };
    }
    true
}
unsafe fn forward_control(device: WDFDEVICE, request: WDFREQUEST, setup: [u8; 8], operation: u8) {
    if !unsafe { prepare_request(device, request, operation == 2) } {
        return;
    }
    let s = unsafe { state(device) };
    let status = unsafe {
        wdf!(WdfSpinLockAcquire, (*s).lock);
        let context = request_state(request);
        (*context).operation = operation;
        if operation == 2 {
            (*s).filtered_pending += 1;
        }
        let slot = ptr::addr_of_mut!((*s).slots[0]);
        let result = if (*s).closing != 0 {
            STATUS_CANCELLED
        } else if !(*slot).request.is_null() || (*s).next_token == u64::MAX {
            STATUS_DEVICE_BUSY
        } else {
            (*s).next_token += 1;
            (*slot).token = (*s).next_token;
            (*slot).length = 0;
            (*slot).input = 0;
            (*slot).setup = setup;
            (*slot).fetched = 0;
            (*slot).request = request;
            let context = request_state(request);
            (*context).controller = device;
            (*context).slot = 0;
            (*context).operation = operation;
            (*s).submitted += 1;
            let result = wdf!(WdfRequestMarkCancelableEx, request, Some(cancel));
            if result < 0 {
                (*slot).request = ptr::null_mut();
            }
            result
        };
        wdf!(WdfSpinLockRelease, (*s).lock);
        result
    };
    if status < 0 {
        unsafe { finish(device, request, status) };
    }
}

// SAFETY: All of the following construction calls run at PASSIVE_LEVEL under
// the passive gate. Locals remain live through synchronous DDIs. UDE consumes
// init objects only on success; unconsumed init objects are always freed.
unsafe fn create_child(device: WDFDEVICE) -> Result<(), NTSTATUS> {
    let s = unsafe { state(device) };
    if unsafe { !(*s).child.is_null() } {
        return Err(STATUS_DEVICE_BUSY);
    }
    let mut init = ude!(
        UdecxUsbDeviceInitAllocateTableIndex,
        PFN_UDECXUSBDEVICEINITALLOCATE,
        device
    )?;
    if init.is_null() {
        return Err(STATUS_INSUFFICIENT_RESOURCES);
    }
    let result = (|| -> Result<(), NTSTATUS> {
        let mut callbacks = UDECX_USB_DEVICE_STATE_CHANGE_CALLBACKS {
            Size: structure_size::<UDECX_USB_DEVICE_STATE_CHANGE_CALLBACKS>(),
            ..Default::default()
        };
        callbacks.EvtUsbDeviceDefaultEndpointAdd = Some(default_endpoint_add);
        callbacks.EvtUsbDeviceEndpointAdd = Some(endpoint_add);
        callbacks.EvtUsbDeviceEndpointsConfigure = Some(endpoints_configure);
        callbacks.EvtUsbDeviceReset = Some(device_reset);
        ude!(
            UdecxUsbDeviceInitSetStateChangeCallbacksTableIndex,
            PFN_UDECXUSBDEVICEINITSETSTATECHANGECALLBACKS,
            init,
            &mut callbacks
        )?;
        ude!(
            UdecxUsbDeviceInitSetSpeedTableIndex,
            PFN_UDECXUSBDEVICEINITSETSPEED,
            init,
            match (*s).speed {
                1 => _UDECX_USB_DEVICE_SPEED::UdecxUsbLowSpeed,
                2 => _UDECX_USB_DEVICE_SPEED::UdecxUsbFullSpeed,
                _ => _UDECX_USB_DEVICE_SPEED::UdecxUsbHighSpeed,
            }
        )?;
        ude!(
            UdecxUsbDeviceInitSetEndpointsTypeTableIndex,
            PFN_UDECXUSBDEVICEINITSETENDPOINTSTYPE,
            init,
            _UDECX_ENDPOINT_TYPE::UdecxEndpointTypeDynamic
        )?;
        for (bytes, length) in [
            (
                unsafe { ptr::addr_of_mut!((*s).physical_device).cast::<u8>() },
                18usize,
            ),
            (
                unsafe { ptr::addr_of_mut!((*s).physical_configuration).cast::<u8>() },
                unsafe { (*s).physical_length },
            ),
        ] {
            let status = ude!(
                UdecxUsbDeviceInitAddDescriptorTableIndex,
                PFN_UDECXUSBDEVICEINITADDDESCRIPTOR,
                init,
                bytes,
                length as u16
            )?;
            if status < 0 {
                return Err(status);
            }
        }
        let mut child_attributes = WDF_OBJECT_ATTRIBUTES {
            Size: structure_size::<WDF_OBJECT_ATTRIBUTES>(),
            ExecutionLevel: _WDF_EXECUTION_LEVEL::WdfExecutionLevelInheritFromParent,
            SynchronizationScope:
                _WDF_SYNCHRONIZATION_SCOPE::WdfSynchronizationScopeInheritFromParent,
            ContextTypeInfo: ptr::addr_of!(CHILD_TYPE),
            EvtCleanupCallback: Some(child_cleanup_observation),
            EvtDestroyCallback: Some(child_destroy_observation),
            ..Default::default()
        };
        let status = ude!(
            UdecxUsbDeviceCreateTableIndex,
            PFN_UDECXUSBDEVICECREATE,
            &mut init,
            &mut child_attributes,
            ptr::addr_of_mut!((*s).child)
        )?;
        if status < 0 {
            return Err(status);
        }
        observe(CHILD_CREATED);
        unsafe {
            (*wdf!(
                WdfObjectGetTypedContextWorker,
                (*s).child.cast(),
                ptr::addr_of!(CHILD_TYPE)
            )
            .cast::<ChildState>())
            .controller = device;
        }
        let status = ude!(
            UdecxUsbDevicePlugInTableIndex,
            PFN_UDECXUSBDEVICEPLUGIN,
            (*s).child,
            ptr::null_mut()
        )?;
        if status < 0 {
            Err(status)
        } else {
            // Publication succeeded; only PlugOutAndDelete may retire this handle.
            unsafe { (*s).child_published = 1 };
            Ok(())
        }
    })();
    if !init.is_null() {
        ude!(
            UdecxUsbDeviceInitFreeTableIndex,
            PFN_UDECXUSBDEVICEINITFREE,
            init
        )?;
    }
    if result.is_err() {
        unsafe { remove_child(device) }?;
    }
    result
}

unsafe fn create_endpoint(
    device: WDFDEVICE,
    init: &mut PUDECXUSBENDPOINT_INIT,
    index: usize,
    address: u8,
) -> Result<(), NTSTATUS> {
    let s = unsafe { state(device) };
    if index >= ENDPOINT_COUNT || (*init).is_null() {
        return Err(STATUS_INVALID_PARAMETER);
    }
    // UDE may create a replacement before reporting the old endpoint released.
    // Reserve at most two queue generations per logical endpoint, never reuse a
    // released queue, and keep the payload budget at one slot per endpoint.
    let storage = unsafe {
        wdf!(WdfSpinLockAcquire, (*s).lock);
        let free = (index * 2..index * 2 + 2).find(|position| {
            (*s).queues[*position].is_null() && (*s).queue_reservations & (1 << position) == 0
        });
        let Some(storage) = free else {
            wdf!(WdfSpinLockRelease, (*s).lock);
            return Err(STATUS_DEVICE_BUSY);
        };
        (*s).queue_reservations |= 1 << storage;
        wdf!(WdfSpinLockRelease, (*s).lock);
        storage
    };
    let result = (|| -> Result<(), NTSTATUS> {
        ude!(
            UdecxUsbEndpointInitSetEndpointAddressTableIndex,
            PFN_UDECXUSBENDPOINTINITSETENDPOINTADDRESS,
            *init,
            address
        )?;
        let mut callbacks = UDECX_USB_ENDPOINT_CALLBACKS {
            Size: structure_size::<UDECX_USB_ENDPOINT_CALLBACKS>(),
            EvtUsbEndpointReset: Some(reset),
            // UdeCx owns queue state; start only records a lifecycle observation.
            EvtUsbEndpointStart: Some(start),
            ..Default::default()
        };
        ude!(
            UdecxUsbEndpointInitSetCallbacksTableIndex,
            PFN_UDECXUSBENDPOINTINITSETCALLBACKS,
            *init,
            &mut callbacks
        )?;
        let mut endpoint: UDECXUSBENDPOINT = ptr::null_mut();
        let mut endpoint_attributes = WDF_OBJECT_ATTRIBUTES {
            Size: structure_size::<WDF_OBJECT_ATTRIBUTES>(),
            ExecutionLevel: _WDF_EXECUTION_LEVEL::WdfExecutionLevelInheritFromParent,
            SynchronizationScope:
                _WDF_SYNCHRONIZATION_SCOPE::WdfSynchronizationScopeInheritFromParent,
            ContextTypeInfo: ptr::addr_of!(ENDPOINT_TYPE),
            EvtCleanupCallback: Some(endpoint_cleanup_observation),
            EvtDestroyCallback: Some(endpoint_destroy_observation),
            ..Default::default()
        };
        let status = ude!(
            UdecxUsbEndpointCreateTableIndex,
            PFN_UDECXUSBENDPOINTCREATE,
            init as *mut PUDECXUSBENDPOINT_INIT,
            &mut endpoint_attributes,
            &mut endpoint
        )?;
        if status < 0 {
            return Err(status);
        }
        observe(ENDPOINT_CREATED);
        // Queue lifetime follows the endpoint, not the detach IOCTL. UDE may
        // issue release callbacks after PlugOutAndDelete returns.
        let mut config = WDF_IO_QUEUE_CONFIG {
            Size: structure_size::<WDF_IO_QUEUE_CONFIG>(),
            DispatchType: _WDF_IO_QUEUE_DISPATCH_TYPE::WdfIoQueueDispatchSequential,
            PowerManaged: _WDF_TRI_STATE::WdfFalse,
            EvtIoInternalDeviceControl: Some(urb),
            EvtIoCanceledOnQueue: Some(canceled_on_queue),
            ..Default::default()
        };
        let mut attributes = WDF_OBJECT_ATTRIBUTES {
            Size: structure_size::<WDF_OBJECT_ATTRIBUTES>(),
            ExecutionLevel: _WDF_EXECUTION_LEVEL::WdfExecutionLevelPassive,
            SynchronizationScope: _WDF_SYNCHRONIZATION_SCOPE::WdfSynchronizationScopeNone,
            ContextTypeInfo: ptr::addr_of!(QUEUE_TYPE),
            ParentObject: endpoint.cast(),
            EvtCleanupCallback: Some(queue_cleanup),
            EvtDestroyCallback: Some(queue_destroy_observation),
            ..Default::default()
        };
        let mut queue: WDFQUEUE = ptr::null_mut();
        let status = unsafe {
            wdf!(
                WdfIoQueueCreate,
                device,
                &mut config,
                &mut attributes,
                &mut queue
            )
        };
        if status < 0 {
            return Err(status);
        }
        observe(QUEUE_CREATED);
        unsafe {
            (*wdf!(
                WdfObjectGetTypedContextWorker,
                queue.cast(),
                ptr::addr_of!(QUEUE_TYPE)
            )
            .cast::<QueueState>())
            .index = index;
            (*wdf!(
                WdfObjectGetTypedContextWorker,
                queue.cast(),
                ptr::addr_of!(QUEUE_TYPE)
            )
            .cast::<QueueState>())
            .storage = storage;
            (*wdf!(
                WdfObjectGetTypedContextWorker,
                queue.cast(),
                ptr::addr_of!(QUEUE_TYPE)
            )
            .cast::<QueueState>())
            .controller = device;
            wdf!(WdfSpinLockAcquire, (*s).lock);
            (*s).queues[storage] = queue;
            wdf!(WdfSpinLockRelease, (*s).lock);
        }
        unsafe {
            (*wdf!(
                WdfObjectGetTypedContextWorker,
                endpoint.cast(),
                ptr::addr_of!(ENDPOINT_TYPE)
            )
            .cast::<EndpointState>())
            .queue = queue;
        }
        ude!(
            UdecxUsbEndpointSetWdfIoQueueTableIndex,
            PFN_UDECXUSBENDPOINTSETWDFIOQUEUE,
            endpoint,
            queue
        )?;
        Ok(())
    })();
    unsafe {
        wdf!(WdfSpinLockAcquire, (*s).lock);
        (*s).queue_reservations &= !(1 << storage);
        wdf!(WdfSpinLockRelease, (*s).lock);
    }
    result
}

// SAFETY: Child context was assigned before PlugIn, when UDE can first request
// endpoint construction. The class extension owns each provided init until the
// successful create consumes it, or the failed callback returns it to UDE.
unsafe fn child_controller(child: UDECXUSBDEVICE) -> WDFDEVICE {
    unsafe {
        (*wdf!(
            WdfObjectGetTypedContextWorker,
            child.cast(),
            ptr::addr_of!(CHILD_TYPE)
        )
        .cast::<ChildState>())
        .controller
    }
}
// SAFETY: A live callback-owned or still-unpublished child, with its initialized
// controller context. Detach reads this readiness flag under the same lock.
unsafe fn mark_default_added(child: UDECXUSBDEVICE, device: WDFDEVICE) {
    let s = unsafe { state(device) };
    unsafe {
        wdf!(WdfSpinLockAcquire, (*s).lock);
        (*wdf!(
            WdfObjectGetTypedContextWorker,
            child.cast(),
            ptr::addr_of!(CHILD_TYPE)
        )
        .cast::<ChildState>())
        .default_added = 1;
        wdf!(WdfSpinLockRelease, (*s).lock);
    }
    observe(DEFAULT_ADDED);
}
unsafe extern "C" fn default_endpoint_add(
    child: UDECXUSBDEVICE,
    mut init: PUDECXUSBENDPOINT_INIT,
) -> NTSTATUS {
    let device = unsafe { child_controller(child) };
    if device.is_null() {
        return STATUS_DEVICE_NOT_READY;
    }
    let status = unsafe { create_endpoint(device, &mut init, 0, 0) }
        .err()
        .unwrap_or(STATUS_SUCCESS);
    if status >= 0 {
        // SAFETY: UDE holds this callback's child live; detach reads the flag
        // under the same lock before invoking the consuming disconnect API.
        unsafe { mark_default_added(child, device) };
    }
    unsafe { initialization_stage(device, 20, status) };
    status
}
unsafe extern "C" fn endpoint_add(
    child: UDECXUSBDEVICE,
    metadata: PUDECX_USB_ENDPOINT_INIT_AND_METADATA,
) -> NTSTATUS {
    let device = unsafe { child_controller(child) };
    if device.is_null() || metadata.is_null() {
        return STATUS_INVALID_PARAMETER;
    }
    // SAFETY: UDE supplies live kernel metadata. Bounds precede descriptor
    // access; packed descriptor bytes are copied through raw byte pointers.
    let length = unsafe { (*metadata).EndpointDescriptorBufferLength as usize };
    let descriptor = unsafe { (*metadata).EndpointDescriptor };
    if !(7..=255).contains(&length) || descriptor.is_null() {
        return STATUS_INVALID_BUFFER_SIZE;
    }
    let bytes = unsafe { core::slice::from_raw_parts(descriptor.cast::<u8>(), 7) };
    if bytes[0] < 7 || bytes[0] as usize > length || bytes[1] != 5 {
        return STATUS_INVALID_PARAMETER;
    }
    let address = bytes[2];
    let addresses = unsafe { (*state(device)).endpoints };
    let Some(index) = addresses.iter().position(|ep| *ep == address && *ep != 0) else {
        return STATUS_NOT_SUPPORTED;
    };
    let mut init = unsafe { (*metadata).UdecxUsbEndpointInit };
    let status = unsafe { create_endpoint(device, &mut init, index, address) }
        .err()
        .unwrap_or(STATUS_SUCCESS);
    unsafe { initialization_stage(device, 20 + index as u32, status) };
    status
}
unsafe extern "C" fn endpoints_configure(
    child: UDECXUSBDEVICE,
    request: WDFREQUEST,
    parameters: PUDECX_ENDPOINTS_CONFIGURE_PARAMS,
) {
    let device = unsafe { child_controller(child) };
    let status = if device.is_null() || parameters.is_null() {
        STATUS_INVALID_PARAMETER
    } else {
        // SAFETY: Callback-owned fixed structure; validate sizes/counts before
        // forming slices. Counts cannot exceed the fixed native endpoint table.
        unsafe {
            if (*parameters).Size != structure_size::<UDECX_ENDPOINTS_CONFIGURE_PARAMS>()
                || (*parameters).EndpointsToConfigureCount > ENDPOINT_COUNT as u32
                || (*parameters).ReleasedEndpointsCount > ENDPOINT_COUNT as u32
                || ((*parameters).EndpointsToConfigureCount != 0
                    && (*parameters).EndpointsToConfigure.is_null())
                || ((*parameters).ReleasedEndpointsCount != 0
                    && (*parameters).ReleasedEndpoints.is_null())
                || (*parameters).ConfigureType > _UDECX_ENDPOINTS_CONFIGURE_TYPE::UdecxEndpointsConfigureTypeEndpointsReleasedOnly
                || ((*parameters).ConfigureType == _UDECX_ENDPOINTS_CONFIGURE_TYPE::UdecxEndpointsConfigureTypeDeviceConfigurationChange && (*parameters).NewConfigurationValue != 0 && (*parameters).NewConfigurationValue != (*state(device)).physical_configuration[5])
                || ((*parameters).ConfigureType == _UDECX_ENDPOINTS_CONFIGURE_TYPE::UdecxEndpointsConfigureTypeInterfaceSettingChange && !allowed_setting(device, (*parameters).InterfaceNumber, (*parameters).NewInterfaceSetting))
            {
                STATUS_INVALID_PARAMETER
            } else {
                // Released endpoints are no longer used by UDE. Retire only
                // the callback-owned endpoint's still-live queue. Detach must
                // never delete it ahead of this callback.
                for i in 0..(*parameters).ReleasedEndpointsCount as usize {
                    let endpoint = (*parameters).ReleasedEndpoints.add(i).read();
                    let context = wdf!(WdfObjectGetTypedContextWorker, endpoint.cast(), ptr::addr_of!(ENDPOINT_TYPE)).cast::<EndpointState>();
                    let queue = (*context).queue;
                    (*context).queue = ptr::null_mut();
                    if !queue.is_null() { wdf!(WdfObjectDelete, queue.cast()); }
                }
                STATUS_SUCCESS
            }
        }
    };
    if status < 0 {
        unsafe { wdf!(WdfRequestComplete, request, status) };
        return;
    }
    // These callbacks manage local endpoint objects. Their setting fields can
    // describe an earlier request, including during unplug. The root-hub filter
    // forwards the actual standard control requests and waits for their replies.
    unsafe { wdf!(WdfRequestComplete, request, STATUS_SUCCESS) };
}
unsafe fn allowed_setting(device: WDFDEVICE, interface: u8, alternate: u8) -> bool {
    // Immutable descriptor storage until all callbacks retire; no allocation.
    let s = unsafe { state(device) };
    let topology = unsafe {
        super::descriptors::validate(
            &(*s).physical_device,
            &(&(*s).physical_configuration)[..(*s).physical_length],
            match super::descriptors::Speed::try_from((*s).speed) {
                Ok(speed) => speed,
                Err(_) => return false,
            },
        )
    };
    topology.is_ok_and(|t| {
        t.settings
            .iter()
            .flatten()
            .any(|setting| setting.interface == interface && setting.alternate == alternate)
    })
}

unsafe fn remove_child(device: WDFDEVICE) -> Result<(), NTSTATUS> {
    let s = unsafe { state(device) };
    // Native class-driver URBs belong to the OS, not to the userspace owner
    // file. Owner death therefore does not itself cancel those requests.
    // Stop admission and take completion ownership of every live slot before
    // requesting UDE removal. Never wait for a vanished worker to reply.
    let mut owned: [WDFREQUEST; ENDPOINT_COUNT] = [ptr::null_mut(); ENDPOINT_COUNT];
    let mut failure = STATUS_SUCCESS;
    unsafe {
        wdf!(WdfSpinLockAcquire, (*s).lock);
        (*s).closing = 1;
        for (index, output) in owned.iter_mut().enumerate() {
            let request = (*s).slots[index].request;
            if request.is_null() {
                continue;
            }
            // The same lock protects cancel's slot removal. Unmark never
            // waits for that callback. STATUS_CANCELLED leaves it sole owner.
            let status = wdf!(WdfRequestUnmarkCancelable, request);
            if status >= 0 {
                (*s).slots[index].request = ptr::null_mut();
                (*s).canceled += 1;
                (*s).canceled_by_endpoint[index] += 1;
                *output = request;
            } else if status != STATUS_CANCELLED {
                failure = status;
            }
        }
        wdf!(WdfSpinLockRelease, (*s).lock);
        for request in owned {
            if !request.is_null() {
                finish(device, request, STATUS_CANCELLED);
            }
        }
    }
    if failure < 0 {
        return Err(failure);
    }
    let child = unsafe { (*s).child };
    // One shared wait budget for endpoint readiness and subsequent queue
    // retirement. Never add a second five-second polling budget to detach.
    let mut remaining_waits = 100;
    // A selected request may already be in our completion DPC. Drain it before
    // deleting the UDE child so no successful forward uses a retired USB handle.
    loop {
        let pending = unsafe {
            wdf!(WdfSpinLockAcquire, (*s).lock);
            let pending = (*s).filtered_pending;
            wdf!(WdfSpinLockRelease, (*s).lock);
            pending
        };
        if pending == 0 {
            break;
        }
        if remaining_waits == 0 {
            return Err(STATUS_IO_TIMEOUT);
        }
        remaining_waits -= 1;
        let mut delay = LARGE_INTEGER { QuadPart: -500_000 };
        unsafe {
            let _ = wdk_sys::ntddk::KeDelayExecutionThread(0, 0, &mut delay);
        }
    }
    if !child.is_null() {
        if unsafe { (*s).child_published == 0 } {
            // Our created object was never successfully published. Its handle
            // is still owned here; the framework's normal deletion applies.
            // Never use this path after successful PlugIn/PlugOutAndDelete.
            unsafe {
                wdf!(WdfObjectDelete, child.cast());
                (*s).child = ptr::null_mut();
            }
            observe(UNPUBLISHED_DELETED);
            return Ok(());
        }
        loop {
            let ready = unsafe {
                wdf!(WdfSpinLockAcquire, (*s).lock);
                let ready = (*wdf!(
                    WdfObjectGetTypedContextWorker,
                    child.cast(),
                    ptr::addr_of!(CHILD_TYPE)
                )
                .cast::<ChildState>())
                .default_added
                    != 0;
                wdf!(WdfSpinLockRelease, (*s).lock);
                ready
            };
            if ready {
                break;
            }
            // Original lab readiness policy: stay within the documented live
            // handle lifetime instead of deleting it again after disconnect.
            // UDE endpoint callbacks do not acquire our passive owner gate.
            if remaining_waits == 0 {
                return Err(STATUS_IO_TIMEOUT);
            }
            remaining_waits -= 1;
            let mut delay = LARGE_INTEGER { QuadPart: -500_000 };
            unsafe {
                let _ = wdk_sys::ntddk::KeDelayExecutionThread(0, 0, &mut delay);
            }
        }
        observe(PLUGOUT_CALLS);
        let status = ude!(
            UdecxUsbDevicePlugOutAndDeleteTableIndex,
            PFN_UDECXUSBDEVICEPLUGOUTANDDELETE,
            child
        )?;
        if status < 0 {
            return Err(status);
        }
        observe(PLUGOUT_SUCCEEDED);
        unsafe {
            (*s).child = ptr::null_mut();
            (*s).child_published = 0;
        };
    }
    // PlugOutAndDelete invalidates the child handle; it is not a barrier
    // against later endpoint-release callbacks. Their queue cleanup callbacks
    // retire the exact ownership entries. Wait finitely at PASSIVE_LEVEL while
    // holding only the passive owner gate; UDE callbacks never acquire it.
    loop {
        let empty = unsafe {
            wdf!(WdfSpinLockAcquire, (*s).lock);
            let empty = (*s).queue_reservations == 0
                && (*s).queues.iter().all(|q| q.is_null())
                && (*s).head.is_null()
                && (*s).failed_head.is_null()
                && (*s).filtered_pending == 0;
            wdf!(WdfSpinLockRelease, (*s).lock);
            empty
        };
        if empty {
            return Ok(());
        }
        if remaining_waits == 0 {
            return Err(STATUS_IO_TIMEOUT);
        }
        remaining_waits -= 1;
        let mut delay = LARGE_INTEGER { QuadPart: -500_000 }; // 50 ms relative.
        unsafe {
            let _ = wdk_sys::ntddk::KeDelayExecutionThread(0, 0, &mut delay);
        }
    }
}

unsafe extern "C" fn file_cleanup(file: WDFFILEOBJECT) {
    // SAFETY: File callbacks were explicitly configured passive; FDO remains
    // alive. Owner cleanup serializes with all IOCTLs before synchronous detach.
    let device = unsafe { wdf!(WdfFileObjectGetDevice, file) };
    let s = unsafe { state(device) };
    if unsafe { (*s).gate.is_null() } {
        return;
    }
    let status = unsafe { wdf!(WdfWaitLockAcquire, (*s).gate, ptr::null_mut()) };
    if status < 0 {
        return;
    }
    if unsafe { (*s).owner == file } {
        let result = unsafe { remove_child(device) };
        // Never retain a retiring file handle as authorization identity. Failed
        // cleanup quarantines this controller until owned devnode removal.
        unsafe {
            (*s).quarantined = u8::from(result.is_err());
            (*s).owner = ptr::null_mut();
        }
    }
    unsafe { wdf!(WdfWaitLockRelease, (*s).gate) };
}
unsafe extern "C" fn device_cleanup(object: WDFOBJECT) {
    let device = object.cast();
    unsafe {
        super::filter::unregister(device);
    }
    let s = unsafe { state(device) };
    // SAFETY: KMDF device cleanup is passive. Partially initialized objects may
    // have no gate/DPC. Parent teardown owns further cleanup on detach failure.
    if !unsafe { (*s).gate.is_null() } {
        let status = unsafe { wdf!(WdfWaitLockAcquire, (*s).gate, ptr::null_mut()) };
        if status < 0 {
            return;
        }
        let _ = unsafe { remove_child(device) };
        unsafe { wdf!(WdfWaitLockRelease, (*s).gate) };
    }
    if !unsafe { (*s).dpc.is_null() } {
        unsafe {
            let _ = wdf!(WdfDpcCancel, (*s).dpc, 1u8);
        }
    }
}

// These functions parse fixed offsets, never cast untrusted data to a Rust
// reference or trust an advertised length. METHOD_BUFFERED gives live kernel
// buffers for this owned IOCTL only; maximum copied data is HEADER + LIMIT.
fn u32_at(bytes: &[u8], offset: usize) -> u32 {
    u32::from_le_bytes([
        bytes[offset],
        bytes[offset + 1],
        bytes[offset + 2],
        bytes[offset + 3],
    ])
}
fn token_at(bytes: &[u8]) -> u64 {
    u64::from_le_bytes(bytes[8..16].try_into().unwrap_or([0; 8]))
}

/// # Safety
/// Own passive controller queue, live request. Returns true only if completed.
// Keep descriptor parsing off the dispatch/removal stack. UDE removal can
// synchronously recurse through the native hub and registry paths.
#[inline(never)]
unsafe fn store_descriptors(
    s: *mut State,
    request: WDFREQUEST,
    input: usize,
) -> Result<(), NTSTATUS> {
    if !(20 + 18 + 9..=20 + 18 + 4096).contains(&input) {
        return Err(STATUS_INVALID_BUFFER_SIZE);
    }
    let mut pointer: PVOID = ptr::null_mut();
    let status = unsafe {
        wdf!(
            WdfRequestRetrieveInputBuffer,
            request,
            input,
            &mut pointer,
            ptr::null_mut()
        )
    };
    if status < 0 {
        return Err(status);
    }
    // SAFETY: WDF validates the input buffer size, fixed fields are
    // checked before slicing; no network parser lives in the kernel.
    let bytes = unsafe { core::slice::from_raw_parts(pointer.cast::<u8>(), input) };
    let config_len = u32_at(bytes, 16) as usize;
    if &bytes[..4] != b"KBWD"
        || u32_at(bytes, 4) != 1
        || u32_at(bytes, 12) != 18
        || config_len > 4096
        || 38usize.checked_add(config_len) != Some(input)
    {
        return Err(STATUS_INVALID_PARAMETER);
    }
    let physical = super::descriptors::validate(
        &bytes[20..38],
        &bytes[38..],
        super::descriptors::Speed::try_from(u32_at(bytes, 8))
            .map_err(|_| STATUS_INVALID_PARAMETER)?,
    )
    .map_err(|_| STATUS_INVALID_PARAMETER)?;
    let mut endpoints = [0u8; ENDPOINT_COUNT];
    for (i, endpoint) in physical.endpoints.iter().flatten().enumerate() {
        endpoints[i + 1] = endpoint.address;
    }
    // SAFETY: No child or owner exists; passive gate excludes attach
    // and cleanup. These arrays remain immutable while published.
    unsafe {
        (*s).physical_device.copy_from_slice(&bytes[20..38]);
        (&mut (*s).physical_configuration)[..config_len].copy_from_slice(&bytes[38..]);
        (*s).physical_length = config_len;
        (*s).speed = u32_at(bytes, 8);
        (*s).endpoints = endpoints;
    }
    Ok(())
}
#[inline(never)]
unsafe fn attach_physical(
    device: WDFDEVICE,
    request: WDFREQUEST,
    file: WDFFILEOBJECT,
    output: usize,
    input: usize,
) -> Result<usize, NTSTATUS> {
    let s = unsafe { state(device) };
    if output != 0 {
        return Err(STATUS_INVALID_BUFFER_SIZE);
    }
    if !unsafe { super::filter::ready(device) } {
        return Err(STATUS_DEVICE_NOT_READY);
    }
    if unsafe { !(*s).owner.is_null() || !(*s).child.is_null() } {
        return Err(STATUS_DEVICE_BUSY);
    }
    unsafe { store_descriptors(s, request, input) }?;
    unsafe {
        wdf!(WdfSpinLockAcquire, (*s).lock);
        (*s).closing = 0;
        wdf!(WdfSpinLockRelease, (*s).lock);
        (*s).owner = file;
    }
    if let Err(status) = unsafe { create_child(device) } {
        if unsafe { (*s).child.is_null() } {
            unsafe { (*s).owner = ptr::null_mut() };
        }
        return Err(status);
    }
    return Ok(0);
}

pub(super) unsafe fn ioctl(
    device: WDFDEVICE,
    request: WDFREQUEST,
    output: usize,
    input: usize,
    code: u32,
) -> bool {
    if ![
        ATTACH_PHYSICAL,
        FETCH,
        REPLY,
        DETACH,
        STATS,
        LIFECYCLE,
        RETIREMENT_READY,
    ]
    .contains(&code)
    {
        return false;
    }
    let s = unsafe { state(device) };
    // SAFETY: Private interface is exposed only after gate initialization.
    let status = unsafe { wdf!(WdfWaitLockAcquire, (*s).gate, ptr::null_mut()) };
    if status < 0 {
        unsafe { wdf!(WdfRequestComplete, request, status) };
        return true;
    }
    let result = (|| -> Result<usize, NTSTATUS> {
        if unsafe { (*s).quarantined != 0 } && code != LIFECYCLE {
            return Err(STATUS_DEVICE_NOT_READY);
        }
        let file = unsafe { wdf!(WdfRequestGetFileObject, request) };
        if file.is_null() {
            return Err(STATUS_ACCESS_DENIED);
        }
        if code == RETIREMENT_READY {
            if input != 0 || output != 0 {
                return Err(STATUS_INVALID_BUFFER_SIZE);
            }
            return if unsafe { (*s).retirement.bound() } {
                Ok(0)
            } else {
                Err(STATUS_NOT_SUPPORTED)
            };
        }
        if code == LIFECYCLE {
            if input != 0 || output != 128 {
                return Err(STATUS_INVALID_BUFFER_SIZE);
            }
            // Metadata-only lab observations remain readable after detach.
            // An attached child's owner still excludes every other file.
            if unsafe { !(*s).owner.is_null() && (*s).owner != file } {
                return Err(STATUS_ACCESS_DENIED);
            }
            let mut buffer: PVOID = ptr::null_mut();
            let status = unsafe {
                wdf!(
                    WdfRequestRetrieveOutputBuffer,
                    request,
                    128usize,
                    &mut buffer,
                    ptr::null_mut()
                )
            };
            if status < 0 {
                return Err(status);
            }
            let mut values = [0u64; 16];
            values[0] = 1;
            for (i, counter) in LIFETIMES.iter().enumerate() {
                values[i + 1] = counter.load(Ordering::Relaxed);
            }
            unsafe {
                wdf!(WdfSpinLockAcquire, (*s).lock);
                values[15] = (*s).queues.iter().enumerate().fold(0, |mask, (i, q)| {
                    mask | if q.is_null() { 0 } else { 1 << i }
                });
                wdf!(WdfSpinLockRelease, (*s).lock);
            }
            // SAFETY: WDF owns the validated 128-byte output for this call;
            // encode fields individually, with no pointers or borrowed data.
            let bytes = unsafe { core::slice::from_raw_parts_mut(buffer.cast::<u8>(), 128) };
            for (i, value) in values.iter().enumerate() {
                bytes[i * 8..i * 8 + 8].copy_from_slice(&value.to_le_bytes());
            }
            return Ok(128);
        }
        if code == ATTACH_PHYSICAL {
            return unsafe { attach_physical(device, request, file, output, input) };
        }
        if unsafe { (*s).owner != file } {
            return Err(STATUS_ACCESS_DENIED);
        }
        if code == DETACH {
            if input != 0 || output != 0 {
                return Err(STATUS_INVALID_BUFFER_SIZE);
            }
            unsafe { remove_child(device) }?;
            unsafe { (*s).owner = ptr::null_mut() };
            return Ok(0);
        }
        if code == FETCH || code == STATS {
            let needed = if code == FETCH {
                HEADER + LIMIT
            } else {
                STATS_BYTES
            };
            if (input != 0 && (code != FETCH || input != 4))
                || output < needed
                || output > HEADER + LIMIT
            {
                return Err(STATUS_INVALID_BUFFER_SIZE);
            }
            let mask = if input == 4 {
                let mut pointer: PVOID = ptr::null_mut();
                let status = unsafe {
                    wdf!(
                        WdfRequestRetrieveInputBuffer,
                        request,
                        4usize,
                        &mut pointer,
                        ptr::null_mut()
                    )
                };
                if status < 0 {
                    return Err(status);
                }
                // Copy before overwriting the shared METHOD_BUFFERED buffer.
                let mask = unsafe { ptr::read_unaligned(pointer.cast::<u32>()) };
                if mask == 0 || mask & !(u32::MAX >> 1) != 0 {
                    return Err(STATUS_INVALID_PARAMETER);
                }
                mask
            } else {
                u32::MAX >> 1
            };
            let mut buffer: PVOID = ptr::null_mut();
            let status = unsafe {
                wdf!(
                    WdfRequestRetrieveOutputBuffer,
                    request,
                    needed,
                    &mut buffer,
                    ptr::null_mut()
                )
            };
            if status < 0 {
                return Err(status);
            }
            unsafe { wdf!(WdfSpinLockAcquire, (*s).lock) };
            if code == STATS {
                let mut values = [0u64; 8 + ENDPOINT_COUNT];
                unsafe {
                    values[..8].copy_from_slice(&[
                        1,
                        (*s).submitted,
                        (*s).completed,
                        (*s).canceled,
                        (*s).rejected,
                        (*s).dpc_wrong_irql,
                        (*s).next_token,
                        (0..ENDPOINT_COUNT).fold(0, |mask, i| {
                            mask | if (*s).slots[i].request.is_null() {
                                0
                            } else {
                                1u64 << i
                            }
                        }),
                    ]);
                    for i in 0..ENDPOINT_COUNT {
                        values[8 + i] = (*s).slots[i].token;
                    }
                }
                for (i, value) in values.iter().enumerate() {
                    // Initialize every returned byte; no read of the OS buffer.
                    unsafe {
                        ptr::copy_nonoverlapping(
                            value.to_le_bytes().as_ptr(),
                            buffer.cast::<u8>().add(i * 8),
                            8,
                        )
                    };
                }
                unsafe { wdf!(WdfSpinLockRelease, (*s).lock) };
                return Ok(STATS_BYTES);
            }
            for offset in 0..ENDPOINT_COUNT {
                let index = (unsafe { (*s).next_fetch } + offset) % ENDPOINT_COUNT;
                if mask & (1 << index) == 0 {
                    continue;
                }
                let slot = unsafe { ptr::addr_of_mut!((*s).slots[index]) };
                if unsafe { !(*slot).request.is_null() && (*slot).fetched == 0 } {
                    let length = unsafe { (*slot).length as usize };
                    let payload = if unsafe { (*slot).input } != 0 {
                        0
                    } else {
                        length
                    };
                    // Initialize only the returned header and payload. Never
                    // form a byte slice over the unused METHOD_BUFFERED tail.
                    unsafe { ptr::write_bytes(buffer.cast::<u8>(), 0, HEADER) };
                    let out =
                        unsafe { core::slice::from_raw_parts_mut(buffer.cast::<u8>(), HEADER) };
                    out[0..4].copy_from_slice(&1u32.to_le_bytes());
                    out[4..8]
                        .copy_from_slice(&(unsafe { (*s).endpoints[index] } as u32).to_le_bytes());
                    out[8..16].copy_from_slice(&unsafe { (*slot).token }.to_le_bytes());
                    out[16..20].copy_from_slice(&(length as u32).to_le_bytes());
                    out[20..24].copy_from_slice(&(payload as u32).to_le_bytes());
                    out[24..32].copy_from_slice(&unsafe { (*slot).setup });
                    unsafe {
                        ptr::copy_nonoverlapping(
                            (*slot).data.as_ptr(),
                            buffer.cast::<u8>().add(HEADER),
                            payload,
                        );
                        (*slot).fetched = 1;
                        (*s).next_fetch = (index + 1) % ENDPOINT_COUNT;
                        (*s).fetched_by_endpoint[index] += 1;
                        wdf!(WdfSpinLockRelease, (*s).lock);
                    }
                    return Ok(HEADER + payload);
                }
            }
            unsafe { wdf!(WdfSpinLockRelease, (*s).lock) };
            return Ok(0); // Nonblocking bounded poll, no pended userspace IOCTL.
        }
        if output != 0 || !(HEADER..=HEADER + LIMIT).contains(&input) {
            return Err(STATUS_INVALID_BUFFER_SIZE);
        }
        let mut buffer: PVOID = ptr::null_mut();
        let status = unsafe {
            wdf!(
                WdfRequestRetrieveInputBuffer,
                request,
                HEADER,
                &mut buffer,
                ptr::null_mut()
            )
        };
        if status < 0 {
            return Err(status);
        }
        let bytes = unsafe { core::slice::from_raw_parts(buffer.cast::<u8>(), input) };
        let addresses = unsafe { (*s).endpoints };
        let index = addresses
            .iter()
            .position(|ep| u32::from(*ep) == u32_at(bytes, 4))
            .ok_or(STATUS_INVALID_PARAMETER)?;
        let completed = u32_at(bytes, 16) as usize;
        let payload = u32_at(bytes, 20) as usize;
        let reply_status = u32_at(bytes, 24);
        if u32_at(bytes, 0) != 1
            || payload > LIMIT
            || HEADER.checked_add(payload) != Some(input)
            || reply_status > 2
            || bytes[28..32] != [0; 4]
        {
            return Err(STATUS_INVALID_PARAMETER);
        }
        // SAFETY: Slot ownership and cancellation share this short spin lock.
        // Unmark does not wait for cancel. STATUS_CANCELLED leaves sole ownership
        // with cancel, which cannot complete until after releasing this lock.
        let owned = unsafe {
            wdf!(WdfSpinLockAcquire, (*s).lock);
            let slot = ptr::addr_of_mut!((*s).slots[index]);
            if (*slot).request.is_null() || (*slot).token != token_at(bytes) || (*slot).fetched == 0
            {
                wdf!(WdfSpinLockRelease, (*s).lock);
                return Err(STATUS_NOT_FOUND);
            }
            if completed > (*slot).length as usize
                || (reply_status != 0 && (completed != 0 || payload != 0))
                || ((*slot).input != 0 && completed != payload)
                || ((*slot).input == 0 && payload != 0)
            {
                wdf!(WdfSpinLockRelease, (*s).lock);
                return Err(STATUS_INVALID_BUFFER_SIZE);
            }
            let owned = (*slot).request;
            let status = wdf!(WdfRequestUnmarkCancelable, owned);
            if status < 0 {
                wdf!(WdfSpinLockRelease, (*s).lock);
                return Err(status);
            }
            (*slot).request = ptr::null_mut();
            let input = (*slot).input != 0;
            wdf!(WdfSpinLockRelease, (*s).lock);
            (owned, input)
        };
        let result = (|| -> Result<(), NTSTATUS> {
            let (urb, input) = owned;
            if reply_status != 0 {
                return Err(if reply_status == 1 {
                    STATUS_NOT_SUPPORTED
                } else {
                    STATUS_IO_DEVICE_ERROR
                });
            }
            if unsafe { (*request_state(urb)).operation != 0 } {
                return if completed == 0 && payload == 0 {
                    Ok(())
                } else {
                    Err(STATUS_INVALID_PARAMETER)
                };
            }
            let mut target: *mut u8 = ptr::null_mut();
            let mut length = 0u32;
            if completed != 0 {
                let status = ude!(
                    UdecxUrbRetrieveBufferTableIndex,
                    PFN_UDECXURBRETRIEVEBUFFER,
                    urb,
                    &mut target,
                    &mut length
                )?;
                if status < 0 {
                    return Err(status);
                }
            }
            if completed > length as usize || (completed != 0 && target.is_null()) {
                return Err(STATUS_INVALID_BUFFER_SIZE);
            }
            if input && completed != 0 {
                // SAFETY: Exclusive URB and mapped buffer, both checked lengths;
                // independent IOCTL buffer cannot overlap this kernel URB data.
                unsafe { ptr::copy_nonoverlapping(bytes[HEADER..].as_ptr(), target, completed) };
            }
            ude!(
                UdecxUrbSetBytesCompletedTableIndex,
                PFN_UDECXURBSETBYTESCOMPLETED,
                urb,
                completed as u32
            )?;
            Ok(())
        })();
        unsafe { finish(device, owned.0, result.err().unwrap_or(STATUS_SUCCESS)) };
        Ok(0)
    })();
    let (status, information) = match result {
        Ok(n) => (STATUS_SUCCESS, n),
        Err(e) => (e, 0),
    };
    if status < 0 {
        unsafe {
            wdf!(WdfSpinLockAcquire, (*s).lock);
            (*s).rejected += 1;
            wdf!(WdfSpinLockRelease, (*s).lock);
        }
    }
    unsafe {
        wdf!(WdfWaitLockRelease, (*s).gate);
        wdf!(
            WdfRequestCompleteWithInformation,
            request,
            status,
            information as ULONG_PTR
        );
    }
    true
}
