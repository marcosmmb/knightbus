//! Keep an owned software-controller slot reserved until its final WDF reference
//! disappears. PnP absence can precede that point while a USB client holds a file.
use core::{mem, ptr};
use wdk_sys::{ntddk::*, *};

// WDF zero-initializes this embedded context before device-add runs.
#[repr(C)]
pub(super) struct Retirement {
    handle: HANDLE,
    event: PKEVENT,
}
impl Retirement {
    pub(super) fn bound(&self) -> bool {
        !self.event.is_null()
    }
    /// Called at PASSIVE_LEVEL before publishing the controller's interfaces.
    pub(super) unsafe fn bind(&mut self, instance: &[u16; 200]) -> NTSTATUS {
        let prefix = b"SWD\\KNIGHTBUS\\SLOT";
        let equal = instance.iter().zip(prefix).all(|(a, b)| {
            let a = if (97..=122).contains(a) { *a - 32 } else { *a };
            a == u16::from(*b)
        });
        if !equal {
            return STATUS_SUCCESS; // Explicit preinstalled-controller diagnostics.
        }
        let suffix = &instance[prefix.len()..];
        let count = suffix.iter().position(|v| *v == 0).unwrap_or(0);
        if !(1..=2).contains(&count) || !suffix[..count].iter().all(|v| (48..=57).contains(v)) {
            return STATUS_INVALID_PARAMETER;
        }
        let slot = suffix[..count].iter().fold(0, |n, v| n * 10 + (*v - 48));
        if slot >= 64 || (count == 2 && suffix[0] == 48) {
            return STATUS_INVALID_PARAMETER;
        }
        // The OS-owned Device directory protects even vacant names from
        // unprivileged creation. The owner creates the manual-reset event there
        // with NtCreateEvent and a SYSTEM/Administrators-only ACL.
        // Open an existing event, never create one or accept a caller's handle.
        let prefix = b"\\Device\\KnightBus.Controller.";
        let mut name = [0u16; 64];
        for (out, b) in name.iter_mut().zip(prefix) {
            *out = u16::from(*b);
        }
        name[prefix.len()..prefix.len() + count].copy_from_slice(&suffix[..count]);
        let length = ((prefix.len() + count) * 2) as u16;
        let mut name = UNICODE_STRING {
            Length: length,
            MaximumLength: length + 2,
            Buffer: name.as_mut_ptr(),
        };
        let mut attributes = OBJECT_ATTRIBUTES {
            Length: mem::size_of::<OBJECT_ATTRIBUTES>() as u32,
            ObjectName: &mut name,
            Attributes: OBJ_KERNEL_HANDLE | OBJ_CASE_INSENSITIVE,
            ..Default::default()
        };
        let mut handle = ptr::null_mut();
        // SAFETY: Bounded, terminated name and live attributes. OBJ_KERNEL_HANDLE
        // makes this independent of the device-add thread and creator process.
        let status = unsafe {
            ZwOpenEvent(
                &mut handle,
                EVENT_MODIFY_STATE | SYNCHRONIZE,
                &mut attributes,
            )
        };
        if status < 0 {
            return status;
        }
        let mut event = ptr::null_mut();
        // SAFETY: This is our newly opened kernel handle, not a user-supplied
        // handle. Require the event object type and retain its dispatch pointer.
        let status = unsafe {
            ObReferenceObjectByHandle(
                handle,
                EVENT_MODIFY_STATE,
                *ExEventObjectType,
                0,
                &mut event,
                ptr::null_mut(),
            )
        };
        if status < 0 {
            unsafe {
                let _ = ZwClose(handle);
            }
            return status;
        }
        self.handle = handle;
        self.event = event.cast();
        STATUS_SUCCESS
    }
    /// Only the WDFDEVICE destroy callback may signal final retirement.
    pub(super) unsafe fn complete(&mut self) {
        if self.event.is_null() {
            return;
        }
        // SAFETY: WDFDEVICE destroy runs at PASSIVE_LEVEL with no remaining WDF
        // references. This unique handle keeps the slot name alive even after
        // creator death. Signal, release our pointer, then close the kernel handle.
        unsafe {
            KeSetEvent(self.event, 0, 0);
            ObfDereferenceObject(self.event.cast());
            let _ = ZwClose(self.handle);
        }
        self.event = ptr::null_mut();
        self.handle = ptr::null_mut();
    }
}
