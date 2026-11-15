//! Passive IOCTL dispatch. The owner gate covers validation through completion.
use super::*;
use knightbus_windows_protocol::wire::{
    self, ATTACH as ATTACH_PHYSICAL, DETACH, FETCH, HEADER, MAX_TRANSFER as LIMIT, REPLY,
    RETIREMENT_READY, SLOTS as ENDPOINT_COUNT, STATS, STATS_BYTES,
};

fn abi_status(error: wire::Error) -> NTSTATUS {
    match error {
        wire::Error::Length => STATUS_INVALID_BUFFER_SIZE,
        wire::Error::Fields => STATUS_INVALID_PARAMETER,
    }
}

// Raw pointers avoid borrowing the shared METHOD_BUFFERED buffer across writes.
unsafe fn input_buffer(request: WDFREQUEST, length: usize) -> Result<*const u8, NTSTATUS> {
    let mut pointer = ptr::null_mut();
    let status = unsafe {
        wdf!(
            WdfRequestRetrieveInputBuffer,
            request,
            length,
            &mut pointer,
            ptr::null_mut()
        )
    };
    if status < 0 {
        Err(status)
    } else {
        Ok(pointer.cast())
    }
}
unsafe fn output_buffer(request: WDFREQUEST, length: usize) -> Result<*mut u8, NTSTATUS> {
    let mut pointer = ptr::null_mut();
    let status = unsafe {
        wdf!(
            WdfRequestRetrieveOutputBuffer,
            request,
            length,
            &mut pointer,
            ptr::null_mut()
        )
    };
    if status < 0 {
        Err(status)
    } else {
        Ok(pointer.cast())
    }
}

/// # Safety
/// Live passive request, with the owner gate held and no published child.
// Keep descriptor parsing off the dispatch/removal stack. UDE removal can
// synchronously recurse through the native hub and registry paths.
#[inline(never)]
unsafe fn store_descriptors(
    s: *mut State,
    request: WDFREQUEST,
    input: usize,
) -> Result<(), NTSTATUS> {
    if !(wire::ATTACH_CONFIG + 9..=wire::ATTACH_MAX).contains(&input) {
        return Err(STATUS_INVALID_BUFFER_SIZE);
    }
    let pointer = unsafe { input_buffer(request, input)? };
    // WDF checked the complete buffer before the attachment parser reads any field.
    let bytes = unsafe { core::slice::from_raw_parts(pointer, input) };
    let attachment = wire::Attachment::decode(bytes).map_err(abi_status)?;
    // SAFETY: No child or owner exists; passive gate excludes attach
    // and cleanup. These arrays remain immutable while published.
    unsafe {
        (*s).physical_device.copy_from_slice(attachment.device);
        let config_len = attachment.configuration.len();
        (&mut (*s).physical_configuration)[..config_len].copy_from_slice(attachment.configuration);
        (*s).physical_length = config_len;
        (*s).bos_length = attachment.bos.len();
        (&mut (*s).physical_bos)[..attachment.bos.len()].copy_from_slice(attachment.bos);
        (*s).speed = attachment.speed as u32;
        (*s).endpoints = attachment.endpoints;
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
    if !unsafe { crate::filter::ready(device) } {
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
    Ok(0)
}

// Each handler runs with the passive owner gate held. Buffer helpers return raw
// pointers so no borrowed METHOD_BUFFERED slice survives a later buffer write.
unsafe fn readiness(s: *mut State, input: usize, output: usize) -> Result<usize, NTSTATUS> {
    if input != 0 || output != 0 {
        return Err(STATUS_INVALID_BUFFER_SIZE);
    }
    return if unsafe { (*s).retirement.bound() } {
        Ok(0)
    } else {
        Err(STATUS_NOT_SUPPORTED)
    };
}

unsafe fn fetch_or_stats(
    s: *mut State,
    request: WDFREQUEST,
    code: u32,
    input: usize,
    output: usize,
) -> Result<usize, NTSTATUS> {
    let needed = if code == FETCH {
        HEADER + LIMIT
    } else {
        STATS_BYTES
    };
    if (input != 0 && (code != FETCH || input != 4)) || output < needed || output > HEADER + LIMIT {
        return Err(STATUS_INVALID_BUFFER_SIZE);
    }
    // Read the mask before writing the shared input/output buffer.
    let mask = if input == 0 {
        wire::SLOT_MASK
    } else {
        let pointer = unsafe { input_buffer(request, input)? };
        wire::fetch_mask(unsafe { core::slice::from_raw_parts(pointer, input) })
            .map_err(abi_status)?
    };
    let buffer = unsafe { output_buffer(request, needed)? };
    if code == STATS {
        return unsafe { stats(s, buffer) };
    }
    unsafe { fetch(s, buffer, mask) }
}

unsafe fn stats(s: *mut State, buffer: *mut u8) -> Result<usize, NTSTATUS> {
    unsafe { wdf!(WdfSpinLockAcquire, (*s).lock) };
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
    Ok(STATS_BYTES)
}

unsafe fn fetch(s: *mut State, buffer: *mut u8, mask: u32) -> Result<usize, NTSTATUS> {
    unsafe { wdf!(WdfSpinLockAcquire, (*s).lock) };
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
            let out = unsafe { core::slice::from_raw_parts_mut(buffer.cast::<u8>(), HEADER) };
            out[0..4].copy_from_slice(&1u32.to_le_bytes());
            out[4..8].copy_from_slice(&(unsafe { (*s).endpoints[index] } as u32).to_le_bytes());
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
                wdf!(WdfSpinLockRelease, (*s).lock);
            }
            return Ok(HEADER + payload);
        }
    }
    unsafe { wdf!(WdfSpinLockRelease, (*s).lock) };
    Ok(0) // Nonblocking bounded poll, no pended userspace IOCTL.
}

unsafe fn reply(
    device: WDFDEVICE,
    request: WDFREQUEST,
    input: usize,
    output: usize,
) -> Result<usize, NTSTATUS> {
    let s = unsafe { state(device) };
    if output != 0 || !(HEADER..=HEADER + LIMIT).contains(&input) {
        return Err(STATUS_INVALID_BUFFER_SIZE);
    }
    let pointer = unsafe { input_buffer(request, input)? };
    let bytes = unsafe { core::slice::from_raw_parts(pointer, input) };
    let response = wire::Reply::decode(bytes).map_err(abi_status)?;
    let index = unsafe { (*s).endpoints }
        .iter()
        .position(|ep| u32::from(*ep) == response.endpoint)
        .ok_or(STATUS_INVALID_PARAMETER)?;
    // SAFETY: Slot ownership and cancellation share this short spin lock.
    // Unmark does not wait for cancel. STATUS_CANCELLED leaves sole ownership
    // with cancel, which cannot complete until after releasing this lock.
    let owned = unsafe {
        wdf!(WdfSpinLockAcquire, (*s).lock);
        let slot = ptr::addr_of_mut!((*s).slots[index]);
        if (*slot).request.is_null() || (*slot).token != response.token || (*slot).fetched == 0 {
            wdf!(WdfSpinLockRelease, (*s).lock);
            return Err(STATUS_NOT_FOUND);
        }
        if let Err(error) = response.validate_length((*slot).length as usize, (*slot).input != 0) {
            wdf!(WdfSpinLockRelease, (*s).lock);
            return Err(abi_status(error));
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
    let result = unsafe { complete_urb(owned.0, owned.1, &response) };
    unsafe { finish(device, owned.0, result.err().unwrap_or(STATUS_SUCCESS)) };
    Ok(0)
}

// The caller owns this URB after successful unmarking. Always finish it, even
// if mapping or response validation fails; cancellation no longer owns it.
unsafe fn complete_urb(
    urb: WDFREQUEST,
    input: bool,
    response: &wire::Reply<'_>,
) -> Result<(), NTSTATUS> {
    let completed = response.completed;
    let payload = response.payload.len();
    let reply_status = response.status;
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
        unsafe { ptr::copy_nonoverlapping(response.payload.as_ptr(), target, completed) };
    }
    ude!(
        UdecxUrbSetBytesCompletedTableIndex,
        PFN_UDECXURBSETBYTESCOMPLETED,
        urb,
        completed as u32
    )?;
    Ok(())
}

unsafe fn dispatch(
    device: WDFDEVICE,
    request: WDFREQUEST,
    output: usize,
    input: usize,
    code: u32,
) -> Result<usize, NTSTATUS> {
    let s = unsafe { state(device) };
    if unsafe { (*s).quarantined != 0 } {
        return Err(STATUS_DEVICE_NOT_READY);
    }
    let file = unsafe { wdf!(WdfRequestGetFileObject, request) };
    if file.is_null() {
        return Err(STATUS_ACCESS_DENIED);
    }
    match code {
        RETIREMENT_READY => return unsafe { readiness(s, input, output) },
        ATTACH_PHYSICAL => return unsafe { attach_physical(device, request, file, output, input) },
        _ => {}
    }
    if unsafe { (*s).owner != file } {
        return Err(STATUS_ACCESS_DENIED);
    }
    match code {
        DETACH => {
            if input != 0 || output != 0 {
                return Err(STATUS_INVALID_BUFFER_SIZE);
            }
            unsafe {
                notification::clear(s);
                remove_child(device)?;
                (*s).owner = ptr::null_mut();
            }
            Ok(0)
        }
        FETCH | STATS => unsafe { fetch_or_stats(s, request, code, input, output) },
        REPLY => unsafe { reply(device, request, input, output) },
        _ => Err(STATUS_INVALID_PARAMETER),
    }
}

pub(crate) unsafe fn ioctl(
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
        RETIREMENT_READY,
    ]
    .contains(&code)
    {
        return false;
    }
    let s = unsafe { state(device) };
    // The interface is published only after this gate is initialized.
    let status = unsafe { wdf!(WdfWaitLockAcquire, (*s).gate, ptr::null_mut()) };
    if status < 0 {
        unsafe { wdf!(WdfRequestComplete, request, status) };
        return true;
    }
    let result = unsafe { dispatch(device, request, output, input, code) };
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
