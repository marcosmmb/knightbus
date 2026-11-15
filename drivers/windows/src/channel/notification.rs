//! Owner-only wake event. Handle lookup must run in the requesting process.
use super::*;
use knightbus_windows_protocol::wire::NOTIFY;
use wdk_sys::ntddk::{KeSetEvent, ObReferenceObjectByHandle, ObfDereferenceObject};

// Caller holds the slot lock. The retained reference keeps signaling valid.
pub(super) unsafe fn signal(s: *mut State) {
    if !unsafe { (*s).wake_event.is_null() } {
        unsafe {
            KeSetEvent((*s).wake_event, 0, 0);
        }
    }
}

// Caller holds the passive owner gate. Remove before dropping the reference.
pub(super) unsafe fn clear(s: *mut State) {
    let event = unsafe {
        wdf!(WdfSpinLockAcquire, (*s).lock);
        signal(s);
        let event = mem::replace(&mut (*s).wake_event, ptr::null_mut());
        wdf!(WdfSpinLockRelease, (*s).lock);
        event
    };
    if !event.is_null() {
        unsafe {
            ObfDereferenceObject(event.cast());
        }
    }
}

unsafe fn register(device: WDFDEVICE, request: WDFREQUEST) -> Result<(), NTSTATUS> {
    let s = unsafe { state(device) };
    let file = unsafe { wdf!(WdfRequestGetFileObject, request) };
    if file.is_null() || unsafe { (*s).owner != file || (*s).quarantined != 0 } {
        return Err(STATUS_ACCESS_DENIED);
    }
    let mut buffer = ptr::null_mut();
    let status = unsafe {
        wdf!(
            WdfRequestRetrieveInputBuffer,
            request,
            8,
            &mut buffer,
            ptr::null_mut()
        )
    };
    if status < 0 {
        return Err(status);
    }
    let handle = u64::from_le_bytes(unsafe { *(buffer.cast::<[u8; 8]>()) });
    let mut event = ptr::null_mut();
    if handle != 0 {
        // UserMode is mandatory even for an administrator. Check both event
        // type and modify access; never trust or retain the raw user handle.
        let status = unsafe {
            ObReferenceObjectByHandle(
                handle as usize as HANDLE,
                EVENT_MODIFY_STATE,
                *ExEventObjectType,
                1,
                &mut event,
                ptr::null_mut(),
            )
        };
        if status < 0 {
            return Err(status);
        }
    }
    unsafe {
        clear(s);
        wdf!(WdfSpinLockAcquire, (*s).lock);
        (*s).wake_event = event.cast();
        signal(s); // Covers requests published before registration.
        wdf!(WdfSpinLockRelease, (*s).lock);
    }
    Ok(())
}

pub(super) unsafe extern "C" fn in_caller_context(device: WDFDEVICE, request: WDFREQUEST) {
    let mut parameters = WDF_REQUEST_PARAMETERS {
        Size: mem::size_of::<WDF_REQUEST_PARAMETERS>() as u16,
        ..Default::default()
    };
    unsafe {
        wdf!(WdfRequestGetParameters, request, &mut parameters);
    }
    if parameters.Type != _WDF_REQUEST_TYPE::WdfRequestTypeDeviceControl
        || unsafe { parameters.Parameters.DeviceIoControl.IoControlCode } != NOTIFY
    {
        let status = unsafe { wdf!(WdfDeviceEnqueueRequest, device, request) };
        if status < 0 {
            unsafe {
                wdf!(WdfRequestComplete, request, status);
            }
        }
        return;
    }
    let p = unsafe { parameters.Parameters.DeviceIoControl };
    if p.InputBufferLength != 8 || p.OutputBufferLength != 0 {
        unsafe {
            wdf!(WdfRequestComplete, request, STATUS_INVALID_BUFFER_SIZE);
        }
        return;
    }
    // User-originated callbacks run at PASSIVE_LEVEL in the caller's process.
    // Reject kernel requests rather than acquiring a passive lock at unknown IRQL.
    if unsafe { wdf!(WdfRequestGetRequestorMode, request) } != 1 {
        unsafe {
            wdf!(WdfRequestComplete, request, STATUS_ACCESS_DENIED);
        }
        return;
    }
    let s = unsafe { state(device) };
    let mut status = unsafe { wdf!(WdfWaitLockAcquire, (*s).gate, ptr::null_mut()) };
    if status >= 0 {
        status = match unsafe { register(device, request) } {
            Ok(()) => STATUS_SUCCESS,
            Err(e) => e,
        };
        unsafe {
            wdf!(WdfWaitLockRelease, (*s).gate);
        }
    }
    unsafe {
        wdf!(WdfRequestComplete, request, status);
    }
}
