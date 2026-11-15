//! Physical descriptor inspection delegates to the native adapter and never captures.
use knightbus_core::{Device, Error, Result};
/// Enumerated identity and the result of descriptor access, independent of claiming.
#[derive(Debug)]
pub struct PhysicalDevice {
    /// Physical incarnation selected by the OS.
    pub registry: u64,
    /// Vendor ID for display, never authorization.
    pub vendor: u16,
    /// Product ID for display, never authorization.
    pub product: u16,
}
/// Enumerate physical devices without claiming interfaces.
#[cfg(target_os = "macos")]
pub fn list() -> Result<Vec<PhysicalDevice>> {
    Ok(knightbus_macos_exporter::native::enumerate()?
        .into_iter()
        .map(|d| PhysicalDevice {
            registry: d.registry_id,
            vendor: d.vendor,
            product: d.product,
        })
        .collect())
}
/// Enumerate physical devices; unsupported exporter platforms fail explicitly.
#[cfg(not(target_os = "macos"))]
pub fn list() -> Result<Vec<PhysicalDevice>> {
    Err(Error::Unsupported)
}
/// Read descriptors for an exact generation. Success does not prove that an
/// OS-owned interface can be captured; that is determined on authorized import.
#[cfg(target_os = "macos")]
pub fn inspect(registry: u64) -> Result<Device> {
    use knightbus_macos_exporter::native;
    let id = native::enumerate()?
        .into_iter()
        .find(|d| d.registry_id == registry)
        .ok_or(Error::NotFound)?;
    Ok(native::inspect(id, id.device_key())?.device)
}
/// Physical descriptor inspection requires an implemented exporter adapter.
#[cfg(not(target_os = "macos"))]
pub fn inspect(_: u64) -> Result<Device> {
    Err(Error::Unsupported)
}
