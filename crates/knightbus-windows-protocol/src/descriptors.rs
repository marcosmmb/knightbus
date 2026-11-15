//! UDE limits applied to validated USB descriptors.
pub use knightbus_usb::*;
/// Configuration storage supported by the private driver ABI.
pub const MAX_CONFIGURATION: usize = 4096;
/// BOS storage supported by the private driver ABI.
pub const MAX_BOS: usize = 4096;
/// Validate the bounded BOS envelope accepted by the native driver.
pub fn validate_bos(bytes: &[u8], speed: Speed) -> Result<(), &'static str> {
    if bytes.len() > MAX_BOS {
        return Err("BOS exceeds driver storage");
    }
    knightbus_usb::validate_bos(bytes, speed)
}
/// Check the descriptor subset implemented by the Windows driver.
pub fn validate(device: &[u8], config: &[u8], speed: Speed) -> Result<Topology, &'static str> {
    if config.len() > MAX_CONFIGURATION {
        return Err("configuration exceeds driver storage");
    }
    let topology = knightbus_usb::validate(device, config, speed)?;
    if device[device::CONFIGURATIONS] != 1 {
        return Err("multiple configurations are not implemented by this driver");
    }
    if config[configuration::ATTRIBUTES] & 0x20 != 0 {
        return Err("remote wake requires native power callbacks");
    }
    Ok(topology)
}
