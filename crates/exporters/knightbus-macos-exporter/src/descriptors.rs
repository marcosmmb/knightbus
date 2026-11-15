//! IORegistry identity mapping and USB descriptor validation.
pub use knightbus_core::descriptors::{MAX_CONFIGURATION, configuration};
use knightbus_core::{Device, DeviceKey, Result};
/// Build portable inventory from the physical registry location.
pub fn device(
    key: DeviceKey,
    location: u32,
    speed: u32,
    descriptor: &[u8],
    configs: &[Vec<u8>],
    active: u8,
) -> Result<Device> {
    knightbus_core::descriptors::device(
        key,
        knightbus_core::descriptors::Address {
            path: format!("/IOUSBHost/{location:08x}").as_bytes(),
            bus: (location >> 24) as u16,
            device: ((location >> 8) & 0xffff) as u16,
        },
        speed,
        descriptor,
        configs,
        active,
    )
}
