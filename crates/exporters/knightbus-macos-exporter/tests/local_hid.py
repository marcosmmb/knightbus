#!/usr/bin/env python3
"""Independent local IOHID consumer; never captures USB or logs report payloads.

Checks only the Logitech 046d:c215 at location 02120000. This is a lab
observation of Apple's restored HID stack, not part of USB forwarding.
"""
import ctypes as C
import json
import os
import time


def observe(timeout=8):
    cf = C.CDLL('/System/Library/Frameworks/CoreFoundation.framework/CoreFoundation')
    io = C.CDLL('/System/Library/Frameworks/IOKit.framework/IOKit')
    ptr, index, integer = C.c_void_p, C.c_long, C.c_int32

    def bind(lib, name, result, *args):
        function = getattr(lib, name)
        function.restype, function.argtypes = result, list(args)
        return function

    release = bind(cf, 'CFRelease', None, ptr)
    string = bind(cf, 'CFStringCreateWithCString', ptr, ptr, C.c_char_p, C.c_uint32)
    number = bind(cf, 'CFNumberGetValue', C.c_bool, ptr, integer, ptr)
    count = bind(cf, 'CFSetGetCount', index, ptr)
    values = bind(cf, 'CFSetGetValues', None, ptr, ptr)
    loop = bind(cf, 'CFRunLoopGetCurrent', ptr)()
    mode = ptr.in_dll(cf, 'kCFRunLoopDefaultMode')
    run = bind(cf, 'CFRunLoopRunInMode', integer, ptr, C.c_double, C.c_bool)
    create = bind(io, 'IOHIDManagerCreate', ptr, ptr, C.c_uint32)
    match = bind(io, 'IOHIDManagerSetDeviceMatching', None, ptr, ptr)
    devices = bind(io, 'IOHIDManagerCopyDevices', ptr, ptr)
    property_value = bind(io, 'IOHIDDeviceGetProperty', ptr, ptr, ptr)
    open_device = bind(io, 'IOHIDDeviceOpen', integer, ptr, C.c_uint32)
    close_device = bind(io, 'IOHIDDeviceClose', integer, ptr, C.c_uint32)
    schedule = bind(io, 'IOHIDDeviceScheduleWithRunLoop', None, ptr, ptr, ptr)
    unschedule = bind(io, 'IOHIDDeviceUnscheduleFromRunLoop', None, ptr, ptr, ptr)
    callback_type = C.CFUNCTYPE(None, ptr, integer, ptr, integer, C.c_uint32, ptr, index)
    register = bind(io, 'IOHIDDeviceRegisterInputReportCallback', None, ptr, ptr, index, ptr, ptr)
    manager = create(None, 0)
    if not manager:
        raise RuntimeError('IOHIDManagerCreate failed')
    device_set, selected, opened = None, None, False
    keys = [string(None, name, 0x08000100) for name in [b'VendorID', b'ProductID', b'LocationID']]
    reports = []

    def received(_context, status, _sender, report_type, _report_id, _report, length):
        reports.append((status, report_type, length))

    callback = callback_type(received)
    buffer = (C.c_uint8 * 64)()
    try:
        match(manager, None)
        device_set = devices(manager)
        if not device_set:
            raise RuntimeError('no local HID devices')
        array = (ptr * count(device_set))()
        values(device_set, array)
        for device in array:
            actual = []
            for key in keys:
                value = property_value(device, key)
                result = C.c_int64()
                actual.append(result.value if value and number(value, 4, C.byref(result)) else None)
            if actual == [0x046d, 0xc215, 0x02120000]:
                selected = device
                break
        if not selected:
            raise RuntimeError('selected joystick not exposed by local HID stack')
        status = open_device(selected, 0)  # nonexclusive; never seize
        if status:
            raise RuntimeError(f'local HID open refused: 0x{status & 0xffffffff:08x}')
        opened = True
        register(selected, buffer, len(buffer), callback, None)
        schedule(selected, loop, mode)
        deadline = time.monotonic() + timeout
        while time.monotonic() < deadline and len(reports) < 3:
            run(mode, 0.1, True)
        if len(reports) < 3 or any(item != (0, 0, 7) for item in reports):
            raise RuntimeError(f'local HID reports missing/invalid: {reports}')
        return {'local_hid': 'PASS', 'uid': os.getuid(), 'reports': len(reports), 'bytes_per_report': 7}
    finally:
        if opened:
            unschedule(selected, loop, mode)
            register(selected, buffer, len(buffer), None, None)
            close_device(selected, 0)
        if device_set:
            release(device_set)
        for key in keys:
            release(key)
        release(manager)


if __name__ == '__main__':
    print(json.dumps(observe()), flush=True)
