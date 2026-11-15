//! Native ISO URB validation and completion. No USB/IP framing enters the kernel.
use super::*;
use knightbus_windows_protocol::iso as packets;

// USBD status macros in usb.h are not emitted by the Rust bindings.
const ISOCH_REQUEST_FAILED: u32 = 0xc000_0b00;
const ISO_NOT_ACCESSED_BY_HW: u32 = 0xc002_0000;
const CRC_ERROR: u32 = 0xc000_0001;
const DATA_OVERRUN: u32 = 0xc000_0008;
const CANCELED: u32 = 0xc001_0000;
const INTERNAL_HC_ERROR: u32 = 0x8000_0800;
const BAD_START_FRAME: u32 = 0xc000_0a00;

// Per-packet result codes carried by the private channel.
mod packet_status {
    pub const SUCCESS: i32 = 0;
    pub const IO: i32 = -5;
    pub const NOT_ACCESSED: i32 = -18;
    pub const PROTOCOL: i32 = -71;
    pub const OVERFLOW: i32 = -75;
    pub const CONNECTION_RESET: i32 = -104;
    pub const CANCELLED: i32 = -125;
}

pub(super) unsafe fn request(request: WDFREQUEST) -> Result<*mut _URB_HEADER, NTSTATUS> {
    let mut parameters = WDF_REQUEST_PARAMETERS {
        Size: mem::size_of::<WDF_REQUEST_PARAMETERS>() as u16,
        ..Default::default()
    };
    unsafe {
        wdf!(WdfRequestGetParameters, request, &mut parameters);
    }
    if parameters.Type != _WDF_REQUEST_TYPE::WdfRequestTypeDeviceControlInternal {
        return Err(STATUS_INVALID_PARAMETER);
    }
    let header = unsafe { parameters.Parameters.Others.Arg1.cast::<_URB_HEADER>() };
    if header.is_null() || unsafe { usize::from((*header).Length) } < mem::size_of::<_URB_HEADER>()
    {
        return Err(STATUS_INVALID_PARAMETER);
    }
    Ok(header)
}

pub(super) unsafe fn layout(
    header: *mut _URB_HEADER,
    input: bool,
) -> Result<Option<*mut _URB_ISOCH_TRANSFER>, NTSTATUS> {
    if unsafe { (*header).Function } == URB_FUNCTION_BULK_OR_INTERRUPT_TRANSFER as u16 {
        return Ok(None);
    }
    if unsafe { (*header).Function } != URB_FUNCTION_ISOCH_TRANSFER as u16 {
        return Err(STATUS_NOT_SUPPORTED);
    }
    let base = mem::offset_of!(_URB_ISOCH_TRANSFER, IsoPacket);
    if unsafe { usize::from((*header).Length) } < base {
        return Err(STATUS_INVALID_BUFFER_SIZE);
    }
    let urb = header.cast::<_URB_ISOCH_TRANSFER>();
    let (count, length, flags) = unsafe {
        (
            (*urb).NumberOfPackets as usize,
            (*urb).TransferBufferLength,
            (*urb).TransferFlags,
        )
    };
    if count == 0
        || count > packets::MAX_PACKETS
        || length as usize > LIMIT
        || unsafe { usize::from((*header).Length) }
            < base + count * mem::size_of::<USBD_ISO_PACKET_DESCRIPTOR>()
        || (flags & USBD_TRANSFER_DIRECTION_IN != 0) != input
    {
        return Err(STATUS_INVALID_PARAMETER);
    }
    if flags & !(USBD_TRANSFER_DIRECTION_IN | USBD_SHORT_TRANSFER_OK | USBD_START_ISO_TRANSFER_ASAP)
        != 0
    {
        return Err(STATUS_NOT_SUPPORTED);
    }
    let descriptors = unsafe { core::slice::from_raw_parts((*urb).IsoPacket.as_ptr(), count) };
    for (i, p) in descriptors.iter().enumerate() {
        let end = descriptors.get(i + 1).map_or(length, |next| next.Offset);
        if p.Offset > end || end > length {
            return Err(STATUS_INVALID_PARAMETER);
        }
    }
    Ok(Some(urb))
}

pub(super) unsafe fn copy_layout(urb: *mut _URB_ISOCH_TRANSFER, slot: *mut Slot, frame: u32) {
    let count = unsafe { (*urb).NumberOfPackets as usize };
    let source = unsafe { core::slice::from_raw_parts((*urb).IsoPacket.as_ptr(), count) };
    unsafe {
        (*slot).iso_count = count as u32;
        for (i, p) in source.iter().enumerate() {
            let end = source
                .get(i + 1)
                .map_or((*urb).TransferBufferLength, |next| next.Offset);
            (*slot).packets[i] = packets::Packet {
                offset: p.Offset,
                length: end - p.Offset,
                actual: 0,
                status: 0,
            };
        }
        (*slot).start_frame = frame;
        (*urb).StartFrame = frame;
    }
}

pub(crate) fn current_frame() -> u32 {
    let mut counter = 0;
    // Monotonic controller-local milliseconds. Never compare with a peer clock.
    (unsafe { wdk_sys::ntddk::KeQueryInterruptTimePrecise(&mut counter) }
        / crate::TIME_UNITS_PER_MILLISECOND) as u32
}
fn usb_status(status: i32) -> USBD_STATUS {
    (match status {
        packet_status::SUCCESS => 0u32,
        packet_status::NOT_ACCESSED => ISO_NOT_ACCESSED_BY_HW,
        packet_status::PROTOCOL => CRC_ERROR,
        packet_status::OVERFLOW => DATA_OVERRUN,
        packet_status::CONNECTION_RESET | packet_status::CANCELLED => CANCELED,
        _ => INTERNAL_HC_ERROR,
    }) as USBD_STATUS
}

pub(super) unsafe fn complete(
    request: WDFREQUEST,
    input: bool,
    response: &knightbus_windows_protocol::wire::Reply<'_>,
) -> Result<(), NTSTATUS> {
    let urb = unsafe { layout(self::request(request)?, input)? }.ok_or(STATUS_INVALID_PARAMETER)?;
    let records = response.packets.ok_or(STATUS_INVALID_PARAMETER)?;
    if records.len() != unsafe { (*urb).NumberOfPackets as usize } {
        return Err(STATUS_INVALID_PARAMETER);
    }
    let mut target = ptr::null_mut::<u8>();
    let mut capacity = 0;
    if input && response.completed != 0 {
        let status = ude!(
            UdecxUrbRetrieveBufferTableIndex,
            PFN_UDECXURBRETRIEVEBUFFER,
            request,
            &mut target,
            &mut capacity
        )?;
        if status < 0 {
            return Err(status);
        }
        if target.is_null() || capacity < unsafe { (*urb).TransferBufferLength } {
            return Err(STATUS_INVALID_BUFFER_SIZE);
        }
    }
    let mut cursor = 0;
    for (i, p) in records.iter().enumerate() {
        // Request and payload lengths were checked against the frozen slot
        // before cancellation was unmarked. This is the sole completion owner.
        unsafe {
            if input && p.actual != 0 {
                ptr::copy_nonoverlapping(
                    response.payload.as_ptr().add(cursor),
                    target.add(p.offset as usize),
                    p.actual as usize,
                );
            }
            let packet = (*urb).IsoPacket.as_mut_ptr().add(i);
            (*packet).Length = p.actual;
            (*packet).Status = usb_status(p.status);
        }
        cursor += p.actual as usize;
    }
    unsafe {
        (*urb).TransferBufferLength = response.completed as u32;
        (*urb).ErrorCount = response.errors;
        (*urb).Hdr.Status = if response.status != 0 {
            usb_status(packet_status::IO)
        } else if response.errors as usize == records.len() {
            ISOCH_REQUEST_FAILED as USBD_STATUS
        } else {
            0
        };
        let r = request_state(request);
        (*r).iso_complete = 1;
        (*r).delay_completion &= u8::from(matches!(
            response.completion_timing((*r).due_frame),
            packets::CompletionTiming::Scheduled(_)
        ));
    }
    Ok(())
}

pub(super) unsafe fn finish(header: *mut _URB_HEADER, completed: bool, status: NTSTATUS) -> bool {
    if unsafe { (*header).Function } != URB_FUNCTION_ISOCH_TRANSFER as u16 {
        return false;
    }
    let base = mem::offset_of!(_URB_ISOCH_TRANSFER, IsoPacket);
    if unsafe { usize::from((*header).Length) } < base {
        return false;
    }
    let urb = header.cast::<_URB_ISOCH_TRANSFER>();
    let count = unsafe { (*urb).NumberOfPackets as usize };
    if count > packets::MAX_PACKETS
        || unsafe { usize::from((*header).Length) }
            < base + count * mem::size_of::<USBD_ISO_PACKET_DESCRIPTOR>()
    {
        return false;
    }
    if !completed {
        unsafe {
            let code = usb_status(if status == STATUS_CANCELLED {
                packet_status::CANCELLED
            } else {
                packet_status::IO
            });
            for i in 0..count {
                let p = (*urb).IsoPacket.as_mut_ptr().add(i);
                (*p).Length = 0;
                (*p).Status = code;
            }
            (*urb).TransferBufferLength = 0;
            (*urb).ErrorCount = count as u32;
            (*urb).Hdr.Status = code;
        }
    }
    true
}

// Only ISO devices allocate a timer. Bulk and interrupt requests never arm it.
pub(super) unsafe fn initialize(device: WDFDEVICE) -> Result<(), NTSTATUS> {
    let s = unsafe { state(device) };
    if !unsafe { (*s).iso_timer.is_null() } {
        return Ok(());
    }
    let mut config = WDF_TIMER_CONFIG {
        Size: structure_size::<WDF_TIMER_CONFIG>(),
        EvtTimerFunc: Some(due),
        AutomaticSerialization: 0,
        UseHighResolutionTimer: 1,
        ..Default::default()
    };
    let mut attributes = WDF_OBJECT_ATTRIBUTES {
        Size: structure_size::<WDF_OBJECT_ATTRIBUTES>(),
        ParentObject: device.cast(),
        ContextTypeInfo: ptr::addr_of!(WORK_TYPE),
        ExecutionLevel: _WDF_EXECUTION_LEVEL::WdfExecutionLevelDispatch,
        SynchronizationScope: _WDF_SYNCHRONIZATION_SCOPE::WdfSynchronizationScopeNone,
        ..Default::default()
    };
    let status = unsafe {
        wdf!(
            WdfTimerCreate,
            &mut config,
            &mut attributes,
            &mut (*s).iso_timer
        )
    };
    if status < 0 {
        Err(status)
    } else {
        unsafe {
            (*work((*s).iso_timer.cast())).controller = device;
        }
        Ok(())
    }
}

// Only successful early ISO replies wait. Errors, cancellation and teardown
// complete immediately. Physical requests are queued ahead through USB/IP ASAP.
pub(super) unsafe fn waiting(s: *mut State, r: *mut RequestState) -> bool {
    let timing = unsafe { completion_timing(s, r) };
    // Ordinary transfers never read the ISO clock.
    timing != packets::CompletionTiming::Immediate && timing.waiting(current_frame())
}
unsafe fn completion_timing(s: *mut State, r: *mut RequestState) -> packets::CompletionTiming {
    unsafe {
        if (*s).closing == 0
            && (*r).status >= 0
            && (*r).iso_complete != 0
            && (*r).delay_completion != 0
        {
            packets::CompletionTiming::Scheduled((*r).due_frame)
        } else {
            packets::CompletionTiming::Immediate
        }
    }
}
// Caller holds the slot lock. One one-shot timer covers all early completions.
pub(super) unsafe fn arm(s: *mut State) {
    if unsafe { (*s).closing != 0 || (*s).iso_timer.is_null() } {
        return;
    }
    let now = current_frame();
    let mut earliest = u32::MAX;
    let mut request = unsafe { (*s).head };
    while !request.is_null() {
        let r = unsafe { request_state(request) };
        if let Some(delay) = unsafe { completion_timing(s, r) }.wake_after(now) {
            earliest = earliest.min(delay);
        }
        request = unsafe { (*r).next };
    }
    if earliest != u32::MAX {
        unsafe {
            let _ = wdf!(
                WdfTimerStart,
                (*s).iso_timer,
                -i64::from(earliest) * crate::TIME_UNITS_PER_MILLISECOND as i64
            );
        }
    }
}
unsafe extern "C" fn due(timer: WDFTIMER) {
    // Parent cleanup joins the timer before releasing controller storage.
    let device = unsafe { (*work(timer.cast())).controller };
    unsafe {
        let _ = wdf!(WdfDpcEnqueue, (*state(device)).dpc);
    }
}
pub(super) unsafe fn bad_start(request: WDFREQUEST, urb: *mut _URB_ISOCH_TRANSFER) {
    unsafe {
        for i in 0..(*urb).NumberOfPackets as usize {
            let p = (*urb).IsoPacket.as_mut_ptr().add(i);
            (*p).Length = 0;
            (*p).Status = ISO_NOT_ACCESSED_BY_HW as USBD_STATUS;
        }
        (*urb).TransferBufferLength = 0;
        (*urb).ErrorCount = (*urb).NumberOfPackets;
        (*urb).Hdr.Status = BAD_START_FRAME as USBD_STATUS;
        (*request_state(request)).iso_complete = 1;
    }
}
