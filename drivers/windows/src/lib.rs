//! KnightBus KMDF/UdeCx virtual USB controller.
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
mod filter;
mod lifetime;

#[cfg(not(all(target_os = "windows", target_arch = "x86_64")))]
compile_error!("This driver requires the Windows x64 WDK ABI");

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
    if status < 0 {
        status
    } else {
        unsafe { filter::initialize(framework_driver) }
    }
}

unsafe extern "C" fn device_add(_driver: WDFDRIVER, mut init: PWDFDEVICE_INIT) -> NTSTATUS {
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
    // SAFETY: Framework invokes device-add at PASSIVE_LEVEL with a live init.
    let status = unsafe { initialize_ude(init) };
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
    // copy. Only SYSTEM and Administrators can open the private interface.
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
    if status < 0 {
        return status;
    }
    // Bind the kernel-held retirement event before a virtual hub/client can
    // retain this controller.
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
    if status < 0 {
        return status;
    }
    // SAFETY: Live initialized controller, before exposing any synthetic child.
    let status = unsafe { channel::initialize(device) };
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
    // No static streams, chained MDLs or wake capabilities are claimed.
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
