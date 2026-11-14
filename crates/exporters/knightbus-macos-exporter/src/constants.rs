//! Named USB wire values, resource bounds and diagnostic fixture parameters.
//! Linux completion codes are wire values, independent of the host libc errno.

/// Usb constants shared by the exporter and its diagnostic binary.
pub mod usb {
    /// IN direction in bmRequestType and endpoint addresses.
    pub const DIRECTION_IN: u8 = 0x80;
    /// Class/vendor request-type bits.
    pub const REQUEST_TYPE_MASK: u8 = 0x60;
    /// Standard request to the device.
    pub const RECIPIENT_DEVICE: u8 = 0;
    /// Standard request to an interface.
    pub const RECIPIENT_INTERFACE: u8 = 1;
    /// Standard request to an endpoint.
    pub const RECIPIENT_ENDPOINT: u8 = 2;
    /// CLEAR_FEATURE request code.
    pub const CLEAR_FEATURE: u8 = 1;
    /// SET_ADDRESS request code; never forwarded.
    pub const SET_ADDRESS: u8 = 5;
    /// GET_DESCRIPTOR request code.
    pub const GET_DESCRIPTOR: u8 = 6;
    /// GET_CONFIGURATION request code.
    pub const GET_CONFIGURATION: u8 = 8;
    /// SET_CONFIGURATION request code.
    pub const SET_CONFIGURATION: u8 = 9;
    /// GET_INTERFACE request code.
    pub const GET_INTERFACE: u8 = 10;
    /// SET_INTERFACE request code.
    pub const SET_INTERFACE: u8 = 11;
    /// ENDPOINT_HALT feature selector.
    pub const ENDPOINT_HALT: u16 = 0;
    /// Endpoint number bits.
    pub const ENDPOINT_NUMBER_MASK: u8 = 0x0f;
    /// Reserved endpoint address bits.
    pub const ENDPOINT_RESERVED_MASK: u8 = 0x70;
    /// Sixteen endpoint numbers in each direction.
    pub const ENDPOINT_DIRECTION_SLOTS: usize = 32;
    /// Index offset for IN endpoint scheduling.
    pub const IN_ENDPOINT_OFFSET: usize = 16;
    /// bmAttributes transfer-type bits.
    pub const TRANSFER_TYPE_MASK: u8 = 3;
    /// Isochronous transfer type.
    pub const TRANSFER_ISOCHRONOUS: u8 = 1;
    /// Bulk transfer type.
    pub const TRANSFER_BULK: u8 = 2;
    /// Interrupt transfer type.
    pub const TRANSFER_INTERRUPT: u8 = 3;
    /// Payload bits of wMaxPacketSize.
    pub const MAX_PACKET_SIZE_MASK: u16 = 0x07ff;
    /// Additional high-speed transactions field offset.
    pub const HS_TRANSACTIONS_SHIFT: u32 = 11;
    /// Additional high-speed transactions field mask.
    pub const HS_TRANSACTIONS_MASK: u16 = 3;
    /// Linux USB/IP URB_ISO_ASAP flag; not a macOS flag.
    pub const ISO_ASAP: u32 = 2;
    /// Device descriptor type.
    pub const DESCRIPTOR_DEVICE: u8 = 1;
    /// Configuration descriptor type.
    pub const DESCRIPTOR_CONFIGURATION: u8 = 2;
    /// Interface descriptor type.
    pub const DESCRIPTOR_INTERFACE: u8 = 4;
    /// Endpoint descriptor type.
    pub const DESCRIPTOR_ENDPOINT: u8 = 5;
    /// HID report descriptor type.
    pub const DESCRIPTOR_HID_REPORT: u8 = 0x22;
    /// USB device descriptor size.
    pub const DEVICE_DESCRIPTOR_LENGTH: usize = 18;
    /// USB configuration descriptor header size.
    pub const CONFIGURATION_DESCRIPTOR_LENGTH: usize = 9;
    /// USB interface descriptor size.
    pub const INTERFACE_DESCRIPTOR_LENGTH: usize = 9;
    /// Minimum USB endpoint descriptor size.
    pub const ENDPOINT_DESCRIPTOR_MIN_LENGTH: usize = 7;
    /// Common length/type descriptor header size.
    pub const DESCRIPTOR_HEADER_LENGTH: usize = 2;
    /// Required/reserved configuration attribute bits.
    pub const CONFIGURATION_ATTRIBUTES_MASK: u8 = 0x9f;
    /// Required configuration attribute bit.
    pub const CONFIGURATION_ATTRIBUTES_REQUIRED: u8 = 0x80;
    /// Fifteen non-control endpoints per direction.
    pub const MAX_INTERFACE_ENDPOINTS: u8 = 30;
    /// Linux USB/IP low speed.
    pub const SPEED_LOW: u32 = 1;
    /// Linux USB/IP full speed.
    pub const SPEED_FULL: u32 = 2;
    /// Linux USB/IP high speed.
    pub const SPEED_HIGH: u32 = 3;
    /// Linux USB/IP SuperSpeed.
    pub const SPEED_SUPER: u32 = 5;
    /// Linux USB/IP SuperSpeedPlus.
    pub const SPEED_SUPER_PLUS: u32 = 6;
    /// USB/IP device ID bus-number shift.
    pub const WIRE_BUS_SHIFT: u32 = 16;
    /// IORegistry location bus-number field shift.
    pub const LOCATION_BUS_SHIFT: u32 = 24;
    /// IORegistry location field used as wire device number.
    pub const LOCATION_DEVICE_SHIFT: u32 = 8;
    /// Wire device-number field mask.
    pub const LOCATION_DEVICE_MASK: u32 = 0xffff;
    /// USB control setup packet size.
    pub const SETUP_SIZE: usize = 8;
    /// bmRequestType offset.
    pub const SETUP_TYPE: usize = 0;
    /// bRequest offset.
    pub const SETUP_REQUEST: usize = 1;
    /// wValue little-endian offset.
    pub const SETUP_VALUE: usize = 2;
    /// wIndex little-endian offset.
    pub const SETUP_INDEX: usize = 4;
    /// wLength little-endian offset.
    pub const SETUP_LENGTH: usize = 6;

    /// descriptor type offset.
    pub const DESCRIPTOR_TYPE_OFFSET: usize = 1;
    /// config total length offset.
    pub const CONFIG_TOTAL_LENGTH_OFFSET: usize = 2;
    /// config interface count offset.
    pub const CONFIG_INTERFACE_COUNT_OFFSET: usize = 4;
    /// config value offset.
    pub const CONFIG_VALUE_OFFSET: usize = 5;
    /// config attributes offset.
    pub const CONFIG_ATTRIBUTES_OFFSET: usize = 7;
    /// interface number offset.
    pub const INTERFACE_NUMBER_OFFSET: usize = 2;
    /// interface alternate offset.
    pub const INTERFACE_ALTERNATE_OFFSET: usize = 3;
    /// interface endpoint count offset.
    pub const INTERFACE_ENDPOINT_COUNT_OFFSET: usize = 4;
    /// interface class offset.
    pub const INTERFACE_CLASS_OFFSET: usize = 5;
    /// interface subclass offset.
    pub const INTERFACE_SUBCLASS_OFFSET: usize = 6;
    /// interface protocol offset.
    pub const INTERFACE_PROTOCOL_OFFSET: usize = 7;
    /// endpoint address offset.
    pub const ENDPOINT_ADDRESS_OFFSET: usize = 2;
    /// endpoint attributes offset.
    pub const ENDPOINT_ATTRIBUTES_OFFSET: usize = 3;
    /// endpoint packet size offset.
    pub const ENDPOINT_PACKET_SIZE_OFFSET: usize = 4;
    /// endpoint interval offset.
    pub const ENDPOINT_INTERVAL_OFFSET: usize = 6;
    /// device class offset.
    pub const DEVICE_CLASS_OFFSET: usize = 4;
    /// device subclass offset.
    pub const DEVICE_SUBCLASS_OFFSET: usize = 5;
    /// device protocol offset.
    pub const DEVICE_PROTOCOL_OFFSET: usize = 6;
    /// device vendor offset.
    pub const DEVICE_VENDOR_OFFSET: usize = 8;
    /// device product offset.
    pub const DEVICE_PRODUCT_OFFSET: usize = 10;
    /// device bcd offset.
    pub const DEVICE_BCD_OFFSET: usize = 12;
    /// device configuration count offset.
    pub const DEVICE_CONFIGURATION_COUNT_OFFSET: usize = 17;
    /// Construct a control setup packet without opaque byte-array literals.
    pub const fn setup(
        request_type: u8,
        request: u8,
        value: u16,
        index: u16,
        length: u16,
    ) -> [u8; SETUP_SIZE] {
        let value = value.to_le_bytes();
        let index = index.to_le_bytes();
        let length = length.to_le_bytes();
        [
            request_type,
            request,
            value[0],
            value[1],
            index[0],
            index[1],
            length[0],
            length[1],
        ]
    }
    /// Read the complete device descriptor.
    pub const READ_DEVICE_DESCRIPTOR: [u8; SETUP_SIZE] = setup(
        DIRECTION_IN,
        GET_DESCRIPTOR,
        (DESCRIPTOR_DEVICE as u16) << u8::BITS,
        0,
        DEVICE_DESCRIPTOR_LENGTH as u16,
    );
    /// Read the currently selected configuration.
    pub const READ_CONFIGURATION: [u8; SETUP_SIZE] =
        setup(DIRECTION_IN, GET_CONFIGURATION, 0, 0, 1);
}

/// Status constants shared by the exporter and its diagnostic binary.
pub mod status {
    /// Successful USB/IP completion.
    pub const SUCCESS: i32 = 0;
    /// Linux EIO.
    pub const IO: i32 = -5;
    /// Linux ENOMEM.
    pub const NO_MEMORY: i32 = -12;
    /// Linux EACCES.
    pub const ACCESS: i32 = -13;
    /// Linux ENODEV.
    pub const NO_DEVICE: i32 = -19;
    /// Linux EINVAL.
    pub const INVALID: i32 = -22;
    /// Linux EPIPE (endpoint stall).
    pub const PIPE: i32 = -32;
    /// Linux EPROTO.
    pub const PROTOCOL: i32 = -71;
    /// Linux EOVERFLOW.
    pub const OVERFLOW: i32 = -75;
    /// Linux EOPNOTSUPP.
    pub const UNSUPPORTED: i32 = -95;
    /// Linux ETIMEDOUT.
    pub const TIMED_OUT: i32 = -110;
    /// Linux ECANCELED.
    pub const CANCELLED: i32 = -125;
}

/// Limits constants shared by the exporter and its diagnostic binary.
pub mod limits {
    use std::time::Duration;
    /// Bounded configuration descriptor count.
    pub const MAX_CONFIGURATIONS: usize = 16;
    /// Bounded total interface/alternate descriptors.
    pub const MAX_INTERFACE_ALTERNATES: usize = 64;
    /// Bounded enumeration results.
    pub const MAX_ENUMERATED_DEVICES: usize = 256;
    /// Budget allowance for worker bookkeeping.
    pub const WORKER_METADATA_ALLOWANCE: usize = 4096;
    /// Budget multiplier for cloned descriptor inventories.
    pub const NATIVE_METADATA_MULTIPLIER: usize = 4;
    /// Budget allowance for native owner bookkeeping.
    pub const NATIVE_METADATA_ALLOWANCE: usize = 16384;
    /// Budget allowance per native operation.
    pub const TRANSFER_METADATA_ALLOWANCE: usize = 256;
    /// Retry interval only after failed abort/release APIs.
    pub const CLEANUP_RETRY_INTERVAL: Duration = Duration::from_millis(10);
    /// Native control completion timeout.
    pub const CONTROL_TIMEOUT_SECONDS: f64 = 2.0;
    /// No native pipe deadline; cancellation is engine-owned.
    pub const PIPE_TIMEOUT_SECONDS: f64 = 0.0;
    /// Maximum interface enumeration/reclaim wait.
    pub const INTERFACE_CLAIM_TIMEOUT: Duration = Duration::from_secs(2);
    /// Interface reclaim retry interval.
    pub const INTERFACE_CLAIM_RETRY: Duration = Duration::from_millis(5);
    /// Standalone Native drop callback-drain interval.
    pub const DIRECT_DRAIN_RETRY: Duration = Duration::from_millis(1);
}

/// Lab constants shared by the exporter and its diagnostic binary.
pub mod lab {
    /// Configuration selected by the exact joystick fixture.
    pub const CONFIGURATION: u16 = 1;
    /// Test-only peer fingerprint used inside the local qualification engine.
    pub const PEER_FINGERPRINT: [u8; 32] = [8; 32];
    /// Cancellation follows the successful probe reports.
    pub const PROBE_CANCEL_SEQUENCE: u32 = PROBE_REPORTS + 1;
    use std::time::Duration;
    /// Logitech qualification target VID; never authorization.
    pub const VENDOR: u16 = 0x046d;
    /// Extreme 3D Pro qualification target PID.
    pub const PRODUCT: u16 = 0xc215;
    /// Shared memory budget for physical diagnostics.
    pub const BUDGET_BYTES: usize = 4 * 1024 * 1024;
    /// Maximum explicitly requested qualification cycles.
    pub const MAX_CYCLES: u32 = 20;
    /// Engine transfer deadline in the diagnostic clock.
    pub const TRANSFER_DEADLINE_MS: u64 = 3000;
    /// Diagnostic attachment lease.
    pub const LEASE_MS: u64 = 60_000;
    /// Direct diagnostic polling interval (not worker polling).
    pub const POLL_INTERVAL: Duration = Duration::from_millis(1);
    /// Bounded diagnostic retirement/re-enumeration wait.
    pub const WAIT_TIMEOUT: Duration = Duration::from_secs(5);
    /// Probe cancellation fallback deadline.
    pub const CANCEL_AFTER: Duration = Duration::from_secs(3);
    /// Physical unplug readiness window.
    pub const UNPLUG_TIMEOUT: Duration = Duration::from_secs(35);
    /// Owner-exit fixture hold window.
    pub const HOLD_DURATION: Duration = Duration::from_secs(10);
    /// Expected actual joystick input report size.
    pub const REPORT_LENGTH: u32 = 7;
    /// Expected actual joystick HID report descriptor size.
    pub const REPORT_DESCRIPTOR_LENGTH: u16 = 122;
    /// Joystick interrupt IN endpoint number.
    pub const INPUT_ENDPOINT: u32 = 1;
    /// Joystick interrupt IN endpoint address.
    pub const INPUT_ADDRESS: u8 = 0x81;
    /// Joystick interrupt polling interval.
    pub const INPUT_INTERVAL: i32 = 10;
    /// Maximum attempts to observe cancellation winning a race.
    pub const ABORT_ATTEMPTS: usize = 8;
    /// Interrupt reads preceding the cancellation probe.
    pub const PROBE_REPORTS: u32 = 5;
    /// Deliberately unsupported descriptor used to provoke STALL.
    pub const RESERVED_DESCRIPTOR_TYPE: u8 = 0xff;
}
