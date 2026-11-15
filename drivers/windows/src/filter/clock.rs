//! Add a controller-local frame clock to the native USB bus interface.
//! All other operations keep the lower stack's context and behavior.
use super::*;
use core::sync::atomic::AtomicUsize;
use wdk_sys::ntddk::{ExAllocatePool2, ExFreePoolWithTag, IoSetCompletionRoutineEx};

const BUS_INTERFACE: GUID = GUID {
    Data1: 0xb1a96a13,
    Data2: 0x3de0,
    Data3: 0x4574,
    Data4: [0x9b, 1, 0xc0, 0x8f, 0xea, 0xb3, 0x18, 0xd6],
};
const MAX_CLOCK_INTERFACES: usize = 64;
// POOL_FLAG_NON_PAGED from wdm.h; not emitted by the Rust bindings.
const NONPAGED_POOL: u64 = 0x40;
const TAG: u32 = u32::from_le_bytes(*b"KBfc");
static INSTANCES: AtomicUsize = AtomicUsize::new(0);
struct ClockInterface {
    references: AtomicUsize,
    filter: WDFDEVICE,
    native: USB_BUS_INTERFACE_USBDI_V3,
    output: PINTERFACE,
    size: usize,
    version: u16,
}

pub(super) unsafe fn install(init: PWDFDEVICE_INIT) -> NTSTATUS {
    let mut minor = IRP_MN_QUERY_INTERFACE as u8;
    unsafe {
        wdf!(
            WdfDeviceInitAssignWdmIrpPreprocessCallback,
            init,
            Some(query),
            IRP_MJ_PNP as u8,
            &mut minor,
            1
        )
    }
}
unsafe extern "C" fn query(device: WDFDEVICE, irp: PIRP) -> NTSTATUS {
    let stack = unsafe {
        (*irp)
            .Tail
            .Overlay
            .__bindgen_anon_2
            .__bindgen_anon_1
            .CurrentStackLocation
    };
    let q = unsafe { (*stack).Parameters.QueryInterface };
    let supported = unsafe { (*super::context(device)).iso_speed.load(Ordering::Acquire) } != 0
        && !q.InterfaceType.is_null()
        && unsafe {
            (*q.InterfaceType).Data1 == BUS_INTERFACE.Data1
                && (*q.InterfaceType).Data2 == BUS_INTERFACE.Data2
                && (*q.InterfaceType).Data3 == BUS_INTERFACE.Data3
                && (*q.InterfaceType).Data4 == BUS_INTERFACE.Data4
        };
    let size = match q.Version {
        0 => mem::size_of::<USB_BUS_INTERFACE_USBDI_V0>(),
        1 => mem::size_of::<USB_BUS_INTERFACE_USBDI_V1>(),
        2 => mem::size_of::<USB_BUS_INTERFACE_USBDI_V2>(),
        3 => mem::size_of::<USB_BUS_INTERFACE_USBDI_V3>(),
        _ => 0,
    };
    if supported && size != 0 && !q.Interface.is_null() && usize::from(q.Size) >= size {
        let wrapper = unsafe { allocate(device, q.Interface, size, q.Version) };
        if !wrapper.is_null() {
            // Use the original IRP. A second synchronous query can reenter the
            // hub stack and exhaust the kernel stack during enumeration.
            let next = unsafe { stack.sub(1) };
            unsafe {
                ptr::copy_nonoverlapping(
                    stack.cast::<u8>(),
                    next.cast::<u8>(),
                    mem::offset_of!(IO_STACK_LOCATION, CompletionRoutine),
                );
                (*next).Control = 0;
                let object = wdf!(WdfDeviceWdmGetDeviceObject, device);
                let status =
                    IoSetCompletionRoutineEx(object, irp, Some(completed), wrapper.cast(), 1, 1, 1);
                if status >= 0 {
                    return wdf!(WdfDeviceWdmDispatchPreprocessedIrp, device, irp);
                }
                free(wrapper);
            }
        }
    }
    // IoSkipCurrentIrpStackLocation, using the SDK-generated IRP layout.
    unsafe {
        (*irp).CurrentLocation += 1;
        (*irp)
            .Tail
            .Overlay
            .__bindgen_anon_2
            .__bindgen_anon_1
            .CurrentStackLocation = stack.add(1);
        wdf!(WdfDeviceWdmDispatchPreprocessedIrp, device, irp)
    }
}

unsafe fn allocate(
    device: WDFDEVICE,
    output: PINTERFACE,
    size: usize,
    version: u16,
) -> *mut ClockInterface {
    if INSTANCES
        .fetch_update(Ordering::AcqRel, Ordering::Acquire, |n| {
            (n < MAX_CLOCK_INTERFACES).then_some(n + 1)
        })
        .is_err()
    {
        return ptr::null_mut();
    }
    let wrapper =
        unsafe { ExAllocatePool2(NONPAGED_POOL, mem::size_of::<ClockInterface>() as u64, TAG) }
            .cast::<ClockInterface>();
    if wrapper.is_null() {
        INSTANCES.fetch_sub(1, Ordering::Release);
        return wrapper;
    }
    let filter = device;
    unsafe {
        wdf!(
            WdfObjectReferenceActual,
            filter.cast(),
            ptr::null_mut(),
            0,
            ptr::null()
        );
        wrapper.write(ClockInterface {
            references: AtomicUsize::new(1),
            filter,
            native: USB_BUS_INTERFACE_USBDI_V3::default(),
            output,
            size,
            version,
        });
    }
    wrapper
}

unsafe fn free(wrapper: *mut ClockInterface) {
    let filter = unsafe { (*wrapper).filter };
    unsafe {
        ExFreePoolWithTag(wrapper.cast(), TAG);
    }
    INSTANCES.fetch_sub(1, Ordering::Release);
    unsafe {
        wdf!(
            WdfObjectDereferenceActual,
            filter.cast(),
            ptr::null_mut(),
            0,
            ptr::null()
        );
    }
}

unsafe extern "C" fn completed(_: PDEVICE_OBJECT, irp: PIRP, data: PVOID) -> NTSTATUS {
    let wrapper = data.cast::<ClockInterface>();
    if unsafe { (*irp).IoStatus.__bindgen_anon_1.Status } < 0 || !unsafe { expose(wrapper) } {
        unsafe { free(wrapper) };
    }
    if unsafe { (*irp).PendingReturned } != 0 {
        // IoMarkIrpPending: preserve the lower stack's asynchronous completion.
        unsafe {
            let stack = (*irp)
                .Tail
                .Overlay
                .__bindgen_anon_2
                .__bindgen_anon_1
                .CurrentStackLocation;
            (*stack).Control |= SL_PENDING_RETURNED as u8;
        }
    }
    STATUS_SUCCESS
}

unsafe fn expose(wrapper: *mut ClockInterface) -> bool {
    let c = unsafe { &mut *wrapper };
    let mut native = USB_BUS_INTERFACE_USBDI_V3::default();
    unsafe {
        ptr::copy_nonoverlapping(
            c.output.cast::<u8>(),
            (&mut native as *mut USB_BUS_INTERFACE_USBDI_V3).cast::<u8>(),
            c.size,
        );
    }
    // Keep unknown interfaces unchanged, including their original reference.
    if native.Size as usize != c.size
        || native.Version != c.version
        || native.InterfaceReference.is_none()
        || native.InterfaceDereference.is_none()
    {
        return false;
    }
    c.native = native;
    let mut exposed = native;
    exposed.BusContext = wrapper.cast();
    exposed.InterfaceReference = Some(reference);
    exposed.InterfaceDereference = Some(dereference);
    exposed.QueryBusTime = Some(frame);
    exposed.QueryBusTimeEx = Some(microframe);
    if native.GetUSBDIVersion.is_some() {
        exposed.GetUSBDIVersion = Some(version_info);
    }
    if native.SubmitIsoOutUrb.is_some() {
        exposed.SubmitIsoOutUrb = Some(submit);
    }
    if native.QueryBusInformation.is_some() {
        exposed.QueryBusInformation = Some(bus_info);
    }
    if native.IsDeviceHighSpeed.is_some() {
        exposed.IsDeviceHighSpeed = Some(high_speed);
    }
    if native.EnumLogEntry.is_some() {
        exposed.EnumLogEntry = Some(log_entry);
    }
    if native.QueryControllerType.is_some() {
        exposed.QueryControllerType = Some(controller_type);
    }
    unsafe {
        ptr::copy_nonoverlapping(
            (&exposed as *const USB_BUS_INTERFACE_USBDI_V3).cast::<u8>(),
            c.output.cast(),
            c.size,
        );
    }
    true
}
unsafe extern "C" fn reference(context: PVOID) {
    let c = unsafe { &*context.cast::<ClockInterface>() };
    c.references.fetch_add(1, Ordering::Relaxed);
    unsafe { c.native.InterfaceReference.unwrap()(c.native.BusContext) };
}
unsafe extern "C" fn dereference(context: PVOID) {
    let c = unsafe { &*context.cast::<ClockInterface>() };
    unsafe { c.native.InterfaceDereference.unwrap()(c.native.BusContext) };
    if c.references.fetch_sub(1, Ordering::AcqRel) == 1 {
        unsafe { free(context.cast()) };
    }
}
unsafe extern "C" fn frame(_: PVOID, output: PULONG) -> NTSTATUS {
    if !output.is_null() {
        unsafe {
            *output = crate::channel::current_frame();
        }
    }
    STATUS_SUCCESS
}
unsafe extern "C" fn microframe(_: PVOID, output: PULONG) -> NTSTATUS {
    if !output.is_null() {
        let mut qpc = 0;
        unsafe {
            *output = (wdk_sys::ntddk::KeQueryInterruptTimePrecise(&mut qpc)
                / crate::TIME_UNITS_PER_MICROFRAME) as u32;
        }
    }
    STATUS_SUCCESS
}
unsafe extern "C" fn version_info(
    context: PVOID,
    version: PUSBD_VERSION_INFORMATION,
    capabilities: PULONG,
) {
    let c = unsafe { &*context.cast::<ClockInterface>() };
    unsafe { c.native.GetUSBDIVersion.unwrap()(c.native.BusContext, version, capabilities) };
}
unsafe extern "C" fn submit(context: PVOID, urb: PURB) -> NTSTATUS {
    let c = unsafe { &*context.cast::<ClockInterface>() };
    unsafe { c.native.SubmitIsoOutUrb.unwrap()(c.native.BusContext, urb) }
}
unsafe extern "C" fn bus_info(
    context: PVOID,
    level: ULONG,
    buffer: PVOID,
    length: PULONG,
    actual: PULONG,
) -> NTSTATUS {
    let c = unsafe { &*context.cast::<ClockInterface>() };
    unsafe {
        c.native.QueryBusInformation.unwrap()(c.native.BusContext, level, buffer, length, actual)
    }
}
unsafe extern "C" fn high_speed(context: PVOID) -> BOOLEAN {
    let c = unsafe { &*context.cast::<ClockInterface>() };
    // The lower interface describes the virtual root hub. Clients need the
    // attached peripheral's negotiated speed, especially for ISO frame lengths.
    let speed = unsafe {
        (*super::context(c.filter))
            .iso_speed
            .load(Ordering::Acquire)
    };
    if speed == 0 {
        unsafe { c.native.IsDeviceHighSpeed.unwrap()(c.native.BusContext) }
    } else {
        u8::from(speed >= knightbus_windows_protocol::descriptors::Speed::High as u32)
    }
}
unsafe extern "C" fn log_entry(
    context: PVOID,
    tag: ULONG,
    event: ULONG,
    a: ULONG,
    b: ULONG,
) -> NTSTATUS {
    let c = unsafe { &*context.cast::<ClockInterface>() };
    unsafe { c.native.EnumLogEntry.unwrap()(c.native.BusContext, tag, event, a, b) }
}
unsafe extern "C" fn controller_type(
    context: PVOID,
    flags: PULONG,
    vendor: PUSHORT,
    device: PUSHORT,
    class: PUCHAR,
    subclass: PUCHAR,
    revision: PUCHAR,
    prog: PUCHAR,
) -> NTSTATUS {
    let c = unsafe { &*context.cast::<ClockInterface>() };
    unsafe {
        c.native.QueryControllerType.unwrap()(
            c.native.BusContext,
            flags,
            vendor,
            device,
            class,
            subclass,
            revision,
            prog,
        )
    }
}
