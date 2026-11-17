//! Physical descriptor inspection delegates to the native adapter and never captures.
use knightbus_core::{Device, Error, Result};
/// Enumerated identity and the result of descriptor access, independent of claiming.
#[derive(Debug)]
pub struct PhysicalDevice {
    /// Stable device identity used by the exporter inventory.
    pub id: knightbus_core::DeviceId,
    /// Physical incarnation selected by the OS.
    pub registry: u64,
    /// Vendor ID for display, never authorization.
    pub vendor: u16,
    /// Product ID for display, never authorization.
    pub product: u16,
    /// Optional OS product name; reading it does not open the USB device.
    pub name: Option<String>,
}
/// Enumerate physical devices without claiming interfaces.
#[cfg(target_os = "macos")]
pub fn list() -> Result<Vec<PhysicalDevice>> {
    Ok(knightbus_macos_exporter::native::enumerate()?
        .into_iter()
        .map(|d| PhysicalDevice {
            id: d.device_key().id,
            registry: d.registry_id,
            vendor: d.vendor,
            product: d.product,
            name: knightbus_macos_exporter::native::display_name(d),
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

/// Devices suitable for the sharing UI; bus hubs and alternate-mode billboards
/// stay available to the raw inspector but are not offered for forwarding.
pub fn shareable() -> Result<Vec<PhysicalDevice>> {
    #[cfg(target_os = "macos")]
    {
        use knightbus_macos_exporter::native;
        Ok(native::enumerate()?
            .into_iter()
            .filter(|id| native::share_candidate(*id))
            .map(|id| PhysicalDevice {
                id: id.device_key().id,
                registry: id.registry_id,
                vendor: id.vendor,
                product: id.product,
                name: native::display_name(id),
            })
            .collect())
    }
    #[cfg(not(target_os = "macos"))]
    {
        Err(Error::Unsupported)
    }
}
