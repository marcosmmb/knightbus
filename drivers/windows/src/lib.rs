//! KnightBus original KMDF/UdeCx controller. Production qualification is gated.
//!
//! Contracts: Microsoft's UDE controller DDIs and the measured WDK 28000 UDE 1.1
//! table ABI. Table entries are not ordinary exported functions. The official
//! stub binds them before the framework invokes our device callbacks.

#![no_std]
#![deny(unsafe_op_in_unsafe_fn)]

extern crate wdk_panic;

use core::{mem, ptr};
use wdk_sys::{call_unsafe_wdf_function_binding, *};

mod channel;
#[path = "../../../crates/importers/knightbus-windows-importer/src/descriptors.rs"]
mod descriptors;
mod filter;
mod lifetime;

#[cfg(not(all(target_os = "windows", target_arch = "x86_64")))]
compile_error!("Only the measured Windows x64 ABI is supported by this experiment");

// Official OS interface identifier, not an upstream forwarding project's ID.
const HOST_CONTROLLER_INTERFACE: GUID = GUID {
    Data1: 0x3abf6f2d,
    Data2: 0x71c4,
    Data3: 0x462a,
    Data4: [0x8a, 0x92, 0x1e, 0x68, 0x61, 0xe6, 0xaf, 0x27],
};

const fn structure_size<T>() -> ULONG {
    assert!(mem::size_of::<T>() <= ULONG::MAX as usize);
    mem::size_of::<T>() as ULONG
}

/// Fixed numeric lab events only; never log pointers or USB/device payloads.
///
/// # Safety
/// Called only inside live DriverEntry/device-add callbacks at PASSIVE_LEVEL,
/// with that callback's live OS driver object.
unsafe fn trace_stage(driver: PDRIVER_OBJECT, stage: ULONG, status: NTSTATUS) {
    // SAFETY: Trusted static ASCII format, two Windows unsigned-long arguments,
    // no retained buffers or user-controlled format. DbgPrintEx permits this
    // callback IRQL. The generated SDK supplies the component ID and ABI.
    unsafe {
        wdk_sys::ntddk::DbgPrintEx(
            _DPFLTR_TYPE::DPFLTR_IHVDRIVER_ID as ULONG,
            3,
            c"KnightBus stage=%lu status=0x%08lx\n".as_ptr(),
            stage,
            status as ULONG,
        );
    }
    // A local debugger can miss print-buffer data around a failed load. This
    // original lab diagnostic uses a customer-defined informational event code,
    // fixed stage marker and status only. It needs no message-resource DLL;
    // Event Viewer may show an unresolved description, so retain numeric data.
    let bytes = mem::size_of::<IO_ERROR_LOG_PACKET>() + mem::size_of::<ULONG>();
    if driver.is_null() || bytes > 128 {
        return;
    }
    // SAFETY: Live callback-owned driver object; conservative 128-byte lab cap
    // is within the WDK error-log entry limit and fits the UCHAR size argument.
    let entry = unsafe { wdk_sys::ntddk::IoAllocateErrorLogEntry(driver.cast(), bytes as UCHAR) };
    if entry.is_null() {
        return; // Logging allocation failure must not affect driver operation.
    }
    // SAFETY: I/O manager provides aligned storage of the requested size. The
    // C POD header is zero-valid. Extra ULONG storage accommodates DumpData's
    // first element independently of bindgen's trailing-array representation.
    // IoWriteErrorLogEntry consumes this allocation; never access it afterward.
    unsafe {
        ptr::write_bytes(entry.cast::<u8>(), 0, bytes);
        let packet = entry.cast::<IO_ERROR_LOG_PACKET>();
        (*packet).ErrorCode = 0x6000_004b_u32 as NTSTATUS;
        (*packet).UniqueErrorValue = 0x4b42_0000 | stage;
        (*packet).FinalStatus = status;
        (*packet).SequenceNumber = stage;
        (*packet).DumpDataSize = mem::size_of::<ULONG>() as USHORT;
        ptr::addr_of_mut!((*packet).DumpData)
            .cast::<ULONG>()
            .write(status as ULONG);
        wdk_sys::ntddk::IoWriteErrorLogEntry(entry);
    }
}

/// Callback-scoped access to the SDK-owned table. Never cached across unload.
///
/// # Safety
/// The official UDE 1.1 stub must have bound the class extension, and execution
/// must be inside a live framework device callback. The stub's real allocation
/// contains UdecxFunctionTableNumEntries entries. The generated zero-length
/// extern array describes only its address, not that allocation's length.
unsafe fn ude_entry(index: UDECXFUNCENUM) -> Result<(PUDECX_DRIVER_GLOBALS, UDECXFUNC), NTSTATUS> {
    if index >= _UDECXFUNCENUM::UdecxFunctionTableNumEntries {
        return Err(STATUS_INVALID_PARAMETER);
    }
    // SAFETY: The framework holds the bound driver alive throughout this callback.
    // Read the SDK's pointer slot, without making a Rust reference to a mutable
    // foreign static. No code here writes the slot or dereferences the globals.
    let globals = unsafe { ptr::read(ptr::addr_of!(UdecxDriverGlobals)) };
    if globals.is_null() {
        return Err(STATUS_DEVICE_NOT_READY);
    }
    // SAFETY: The checked SDK index is within the real UDE 1.1 allocation.
    // wrapping_add avoids asserting inbounds on bindgen's zero-length array;
    // no reference/slice of that array is ever constructed. Stub lifetime and
    // initialization, not a null check, justify reading the selected slot.
    let function = unsafe {
        ptr::addr_of!(UdecxFunctions)
            .cast::<UDECXFUNC>()
            .wrapping_add(index as usize)
            .read()
    };
    if function.is_none() {
        return Err(STATUS_DEVICE_NOT_READY);
    }
    Ok((globals, function))
}

/// # Safety
/// PASSIVE_LEVEL; the live framework-owned initialization object belongs to the
/// current EvtDriverDeviceAdd call and has not been consumed by WdfDeviceCreate.
unsafe fn initialize_ude(init: PWDFDEVICE_INIT) -> NTSTATUS {
    // SAFETY: This function's caller supplies the live callback/binding context.
    let (globals, raw) =
        match unsafe { ude_entry(_UDECXFUNCENUM::UdecxInitializeWdfDeviceInitTableIndex) } {
            Ok(entry) => entry,
            Err(status) => return status,
        };
    // SAFETY: This exact SDK table index has this generated PFN signature.
    // Both are nullable C function pointers on the measured x64 ABI; no guessed
    // extern declaration or generic transmute to an unchecked type is used.
    let typed: PFN_UDECXINITIALIZEWDFDEVICEINIT = unsafe { mem::transmute(raw) };
    match typed {
        // SAFETY: Above ABI/index proof plus the init ownership contract.
        Some(call) => unsafe { call(globals, init) },
        None => STATUS_DEVICE_NOT_READY,
    }
}

/// # Safety
/// PASSIVE_LEVEL; a newly created live WDFDEVICE and fully initialized config.
unsafe fn add_emulation(device: WDFDEVICE, config: &mut UDECX_WDF_DEVICE_CONFIG) -> NTSTATUS {
    // SAFETY: Called only within the live device-add callback after WdfDeviceCreate.
    let (globals, raw) =
        match unsafe { ude_entry(_UDECXFUNCENUM::UdecxWdfDeviceAddUsbDeviceEmulationTableIndex) } {
            Ok(entry) => entry,
            Err(status) => return status,
        };
    // SAFETY: The generated PFN matches this specific official SDK index.
    let typed: PFN_UDECXWDFDEVICEADDUSBDEVICEEMULATION = unsafe { mem::transmute(raw) };
    match typed {
        // SAFETY: The config is borrowed only for this synchronous DDI call;
        // its callbacks are static functions and remain in the driver image.
        Some(call) => unsafe { call(globals, device, config) },
        None => STATUS_DEVICE_NOT_READY,
    }
}

// SAFETY: This crate uniquely owns DriverEntry and follows the Windows x64 ABI.
// The OS provides live driver/registry arguments; no unwinding crosses this ABI.
#[unsafe(export_name = "DriverEntry")]
pub unsafe extern "system" fn driver_entry(
    driver: PDRIVER_OBJECT,
    registry: PCUNICODE_STRING,
) -> NTSTATUS {
    if driver.is_null() || registry.is_null() {
        return STATUS_INVALID_PARAMETER;
    }
    // SAFETY: OS driver object remains valid throughout DriverEntry, even if
    // framework creation later fails. Stage zero precedes the first WDF call.
    unsafe { trace_stage(driver, 0, STATUS_SUCCESS) };
    let mut config = WDF_DRIVER_CONFIG {
        Size: structure_size::<WDF_DRIVER_CONFIG>(),
        EvtDriverDeviceAdd: Some(device_add),
        ..Default::default()
    };
    // SAFETY: DriverEntry runs at PASSIVE_LEVEL. This is our first WDF call.
    // Initialized config lives through the call; no parent/output is requested.
    let mut framework_driver = ptr::null_mut();
    let status = unsafe {
        call_unsafe_wdf_function_binding!(
            WdfDriverCreate,
            driver,
            registry,
            ptr::null_mut(),
            &mut config,
            &mut framework_driver,
        )
    };
    // SAFETY: DriverEntry is still live at PASSIVE_LEVEL, regardless of status.
    unsafe { trace_stage(driver, 1, status) };
    if status < 0 {
        status
    } else {
        unsafe { filter::initialize(framework_driver) }
    }
}

unsafe extern "C" fn device_add(driver: WDFDRIVER, mut init: PWDFDEVICE_INIT) -> NTSTATUS {
    if init.is_null() {
        return STATUS_INVALID_PARAMETER;
    }
    if let Some(status) = unsafe { filter::try_add(init) } {
        return status;
    }
    let instance = match unsafe { filter::instance(init) } {
        Ok(id) => id,
        Err(status) => return status,
    };
    // SAFETY: The framework holds this driver alive for the device-add callback;
    // retrieve its WDM object only for synchronous diagnostic calls below.
    let driver_object =
        unsafe { call_unsafe_wdf_function_binding!(WdfDriverWdmGetDriverObject, driver) };
    // SAFETY: Framework invokes device-add at PASSIVE_LEVEL with a live init.
    let status = unsafe { initialize_ude(init) };
    // SAFETY: Live device-add callback remains at PASSIVE_LEVEL.
    unsafe { trace_stage(driver_object, 2, status) };
    if status < 0 {
        return status;
    }
    // SAFETY: Device init is live, before WdfDeviceCreate; fixed framework
    // contexts/file callbacks are installed before any USB or user I/O exists.
    unsafe { channel::configure_init(init) };
    // Use an OS-generated name instead of guessing a free physical HCD name.
    // SAFETY: The init remains framework-owned and unconsumed; OR preserves UDE
    // characteristics. The subsequent ACL assignment requires a device name.
    unsafe {
        call_unsafe_wdf_function_binding!(
            WdfDeviceInitSetCharacteristics,
            init,
            FILE_AUTOGENERATED_DEVICE_NAME | FILE_DEVICE_SECURE_OPEN,
            1u8,
        );
    }
    let mut acl_buffer = [0u16; b"D:P(A;;GA;;;SY)(A;;GA;;;BA)\0".len()];
    for (out, byte) in acl_buffer.iter_mut().zip(b"D:P(A;;GA;;;SY)(A;;GA;;;BA)\0") {
        *out = u16::from(*byte);
    }
    let acl = UNICODE_STRING {
        Length: ((acl_buffer.len() - 1) * 2) as u16,
        MaximumLength: (acl_buffer.len() * 2) as u16,
        Buffer: acl_buffer.as_mut_ptr(),
    };
    // SAFETY: Fixed, bounded UTF-16 ACL remains live through this synchronous
    // copy. Only SYSTEM/Administrators get lab access; this is not service policy.
    let status =
        unsafe { call_unsafe_wdf_function_binding!(WdfDeviceInitAssignSDDLString, init, &acl) };
    if status < 0 {
        return status;
    }
    let mut attributes = WDF_OBJECT_ATTRIBUTES {
        Size: structure_size::<WDF_OBJECT_ATTRIBUTES>(),
        // UDE creates its own controller queues. Do not constrain all class-
        // extension children to PASSIVE_LEVEL; our queue opts in below.
        ExecutionLevel: _WDF_EXECUTION_LEVEL::WdfExecutionLevelDispatch,
        SynchronizationScope: _WDF_SYNCHRONIZATION_SCOPE::WdfSynchronizationScopeNone,
        ..Default::default()
    };
    // SAFETY: Only modifies our local attributes; context type has image lifetime.
    unsafe { channel::configure_attributes(&mut attributes) };
    let mut device: WDFDEVICE = ptr::null_mut();
    // SAFETY: Framework consumes init on success and parents the FDO/queues.
    // Return failures to WDF; do not free an FDO or its init as a control device.
    let status = unsafe {
        call_unsafe_wdf_function_binding!(WdfDeviceCreate, &mut init, &mut attributes, &mut device)
    };
    // SAFETY: Device creation returned within the live passive callback.
    unsafe { trace_stage(driver_object, 3, status) };
    if status < 0 {
        return status;
    }
    // Bind the kernel-held retirement event before a virtual hub/client can
    // retain this controller. The legacy diagnostic ROOT path has no event.
    let status = unsafe { channel::bind_retirement(device, &instance) };
    if status < 0 {
        return status;
    }
    let mut config = UDECX_WDF_DEVICE_CONFIG {
        Size: structure_size::<UDECX_WDF_DEVICE_CONFIG>(),
        NumberOfUsb20Ports: 1,
        // Match the documented SDK initializer's one-port-per-speed defaults.
        // An empty root-hub port is not a transfer-capability claim.
        NumberOfUsb30Ports: 1,
        EvtUdecxWdfDeviceQueryUsbCapability: Some(query_capability),
        ResetAction: _UDECX_WDF_DEVICE_RESET_ACTION::UdecxWdfDeviceResetActionResetEachUsbDevice,
        ..Default::default()
    };
    // SAFETY: FDO exists, UDE init preceded creation, config/callbacks are valid.
    let status = unsafe { add_emulation(device, &mut config) };
    // SAFETY: Still within the live passive callback; no payload is logged.
    unsafe { trace_stage(driver_object, 4, status) };
    if status < 0 {
        return status;
    }
    let mut queue = WDF_IO_QUEUE_CONFIG {
        Size: structure_size::<WDF_IO_QUEUE_CONFIG>(),
        DispatchType: _WDF_IO_QUEUE_DISPATCH_TYPE::WdfIoQueueDispatchSequential,
        PowerManaged: _WDF_TRI_STATE::WdfFalse,
        DefaultQueue: 1,
        EvtIoDeviceControl: Some(controller_ioctl),
        ..Default::default()
    };
    let mut queue_attributes = WDF_OBJECT_ATTRIBUTES {
        Size: structure_size::<WDF_OBJECT_ATTRIBUTES>(),
        ExecutionLevel: _WDF_EXECUTION_LEVEL::WdfExecutionLevelPassive,
        SynchronizationScope: _WDF_SYNCHRONIZATION_SCOPE::WdfSynchronizationScopeNone,
        ..Default::default()
    };
    // SAFETY: WDF owns/parents this sequential queue to the live FDO; inherited
    // passive callbacks selected explicitly for this queue. This queue has no
    // pended driver-owned requests/state.
    let status = unsafe {
        call_unsafe_wdf_function_binding!(
            WdfIoQueueCreate,
            device,
            &mut queue,
            &mut queue_attributes,
            ptr::null_mut()
        )
    };
    // SAFETY: Still within the live passive callback.
    unsafe { trace_stage(driver_object, 5, status) };
    if status < 0 {
        return status;
    }
    // SAFETY: Live FDO, official constant GUID, no reference string. No global
    // DOS HCD alias is created; legacy enumeration compatibility is unproven.
    let status = unsafe {
        call_unsafe_wdf_function_binding!(
            WdfDeviceCreateDeviceInterface,
            device,
            &HOST_CONTROLLER_INTERFACE,
            ptr::null()
        )
    };
    // SAFETY: Interface creation returned within the live passive callback.
    unsafe { trace_stage(driver_object, 6, status) };
    if status < 0 {
        return status;
    }
    // SAFETY: Live initialized controller, before exposing any synthetic child.
    let status = unsafe { channel::initialize(device) };
    unsafe { trace_stage(driver_object, 7, status) };
    if status < 0 {
        status
    } else {
        unsafe { filter::register(device, instance) }
    }
}

unsafe extern "C" fn query_capability(
    _device: WDFDEVICE,
    _capability: PGUID,
    _length: ULONG,
    _buffer: PVOID,
    result_length: PULONG,
) -> NTSTATUS {
    if !result_length.is_null() {
        // SAFETY: UDE supplies this writable output for this callback only.
        unsafe { result_length.write(0) };
    }
    // No chained MDLs, USB3, wake or transfer capabilities are claimed.
    STATUS_NOT_SUPPORTED
}

unsafe extern "C" fn controller_ioctl(
    queue: WDFQUEUE,
    request: WDFREQUEST,
    output_length: usize,
    input_length: usize,
    code: ULONG,
) {
    // SAFETY: A passive sequential WDF queue callback owns a live request; the
    // queue/device remain parented for this callback. No URB is processed here.
    let device = unsafe { call_unsafe_wdf_function_binding!(WdfIoQueueGetDevice, queue) };
    // SAFETY: Our own queue is passive and owns this request. The channel
    // handles only its exact private codes; other controller I/O goes to UDE.
    if unsafe { channel::ioctl(device, request, output_length, input_length, code) } {
        return;
    }
    // SAFETY: Live framework callback keeps this driver's class binding valid.
    let result = unsafe { ude_entry(_UDECXFUNCENUM::UdecxWdfDeviceTryHandleUserIoctlTableIndex) };
    if let Ok((globals, raw)) = result {
        // SAFETY: Exact generated PFN for the selected SDK table entry.
        let typed: PFN_UDECXWDFDEVICETRYHANDLEUSERIOCTL = unsafe { mem::transmute(raw) };
        if let Some(call) = typed {
            // SAFETY: UDE accepts live standard controller requests, and if it
            // returns TRUE takes completion ownership. Never touch request then.
            if unsafe { call(globals, device, request) } != 0 {
                return;
            }
        }
    }
    // SAFETY: FALSE/error leaves this controller request owned by this callback.
    // Complete it once, synchronously. This is not a USB URB completion path and
    // does not resolve the separate UDE guide's transfer completion IRQL issue.
    unsafe {
        call_unsafe_wdf_function_binding!(
            WdfRequestComplete,
            request,
            STATUS_INVALID_DEVICE_REQUEST
        )
    };
}
