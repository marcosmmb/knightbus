//! Checked physical descriptor parsing. Class/vendor descriptors remain opaque.
use crate::constants::{limits, usb};
use knightbus_core::{
    Configuration, Device, DeviceKey, Endpoint, Error, Interface, Result, TransferKind, usbip,
};
use std::collections::BTreeSet;

/// Maximum descriptor bytes accepted per configuration.
pub const MAX_CONFIGURATION: usize = u16::MAX as usize;
/// Parse one complete configuration, rejecting ambiguous endpoint ownership.
pub fn configuration(bytes: &[u8]) -> Result<Configuration> {
    if bytes.len() < usb::CONFIGURATION_DESCRIPTOR_LENGTH
        || bytes.len() > MAX_CONFIGURATION
        || bytes[..usb::DESCRIPTOR_HEADER_LENGTH]
            != [
                usb::CONFIGURATION_DESCRIPTOR_LENGTH as u8,
                usb::DESCRIPTOR_CONFIGURATION,
            ]
        || usize::from(word(bytes, usb::CONFIG_TOTAL_LENGTH_OFFSET)) != bytes.len()
        || bytes[usb::CONFIG_VALUE_OFFSET] == 0
        || bytes[usb::CONFIG_ATTRIBUTES_OFFSET] & usb::CONFIGURATION_ATTRIBUTES_MASK
            != usb::CONFIGURATION_ATTRIBUTES_REQUIRED
    {
        return Err(Error::Invalid);
    }
    let mut result = Configuration {
        value: bytes[usb::CONFIG_VALUE_OFFSET],
        interfaces: Vec::new(),
    };
    let mut offset = usb::CONFIGURATION_DESCRIPTOR_LENGTH;
    let mut expected = 0;
    let mut identities = BTreeSet::new();
    while offset < bytes.len() {
        if bytes.len() - offset < usb::DESCRIPTOR_HEADER_LENGTH {
            return Err(Error::Invalid);
        }
        let len = usize::from(bytes[offset]);
        if len < usb::DESCRIPTOR_HEADER_LENGTH || len > bytes.len() - offset {
            return Err(Error::Invalid);
        }
        let d = &bytes[offset..offset + len];
        match d[usb::DESCRIPTOR_TYPE_OFFSET] {
            usb::DESCRIPTOR_INTERFACE => {
                if len != usb::INTERFACE_DESCRIPTOR_LENGTH
                    || result.interfaces.len() == limits::MAX_INTERFACE_ALTERNATES
                    || d[usb::INTERFACE_ENDPOINT_COUNT_OFFSET] > usb::MAX_INTERFACE_ENDPOINTS
                    || !identities.insert((
                        d[usb::INTERFACE_NUMBER_OFFSET],
                        d[usb::INTERFACE_ALTERNATE_OFFSET],
                    ))
                    || result
                        .interfaces
                        .last()
                        .is_some_and(|i| i.endpoints.len() != expected)
                {
                    return Err(Error::Invalid);
                }
                expected = usize::from(d[usb::INTERFACE_ENDPOINT_COUNT_OFFSET]);
                result.interfaces.push(Interface {
                    number: d[usb::INTERFACE_NUMBER_OFFSET],
                    alternate: d[usb::INTERFACE_ALTERNATE_OFFSET],
                    class: usbip::Interface {
                        class: d[usb::INTERFACE_CLASS_OFFSET],
                        subclass: d[usb::INTERFACE_SUBCLASS_OFFSET],
                        protocol: d[usb::INTERFACE_PROTOCOL_OFFSET],
                    },
                    endpoints: Vec::new(),
                });
            }
            usb::DESCRIPTOR_ENDPOINT => {
                let interface = result.interfaces.last_mut().ok_or(Error::Invalid)?;
                if len < usb::ENDPOINT_DESCRIPTOR_MIN_LENGTH
                    || d[usb::ENDPOINT_ADDRESS_OFFSET] & usb::ENDPOINT_RESERVED_MASK != 0
                    || d[usb::ENDPOINT_ADDRESS_OFFSET] & usb::ENDPOINT_NUMBER_MASK == 0
                    || word(d, usb::ENDPOINT_PACKET_SIZE_OFFSET) & usb::MAX_PACKET_SIZE_MASK == 0
                    || interface.endpoints.len() >= expected
                    || interface
                        .endpoints
                        .iter()
                        .any(|e| e.address == d[usb::ENDPOINT_ADDRESS_OFFSET])
                {
                    return Err(Error::Invalid);
                }
                let kind = match d[usb::ENDPOINT_ATTRIBUTES_OFFSET] & usb::TRANSFER_TYPE_MASK {
                    usb::TRANSFER_ISOCHRONOUS => TransferKind::Isochronous,
                    usb::TRANSFER_BULK => TransferKind::Bulk,
                    usb::TRANSFER_INTERRUPT => TransferKind::Interrupt,
                    _ => return Err(Error::Unsupported),
                };
                interface.endpoints.push(Endpoint {
                    address: d[usb::ENDPOINT_ADDRESS_OFFSET],
                    kind,
                    max_packet_size: word(d, usb::ENDPOINT_PACKET_SIZE_OFFSET),
                    interval: d[usb::ENDPOINT_INTERVAL_OFFSET],
                    allowed_flags: if kind == TransferKind::Isochronous {
                        usb::ISO_ASAP
                    } else {
                        0
                    },
                });
            }
            usb::DESCRIPTOR_DEVICE | usb::DESCRIPTOR_CONFIGURATION => return Err(Error::Invalid),
            _ => {} // Retain raw bytes separately; do not reinterpret class requests.
        }
        offset += len;
    }
    if result
        .interfaces
        .last()
        .is_some_and(|i| i.endpoints.len() != expected)
        || result
            .interfaces
            .iter()
            .filter(|i| i.alternate == 0)
            .count()
            != usize::from(bytes[usb::CONFIG_INTERFACE_COUNT_OFFSET])
        || identities
            .iter()
            .any(|(i, _)| !identities.contains(&(*i, 0)))
    {
        return Err(Error::Invalid);
    }
    // Endpoints may be reused by alternates of one interface, never by two
    // independently selected interfaces, even when initially inactive.
    for a in &result.interfaces {
        for b in &result.interfaces {
            if a.number != b.number
                && a.endpoints
                    .iter()
                    .any(|e| b.endpoints.iter().any(|f| e.address == f.address))
            {
                return Err(Error::Invalid);
            }
        }
    }
    Ok(result)
}
/// Build engine inventory from physical descriptor bytes and a generation.
pub fn device(
    key: DeviceKey,
    location: u32,
    speed: u32,
    descriptor: &[u8],
    configs: &[Vec<u8>],
    active: u8,
) -> Result<Device> {
    if descriptor.len() != usb::DEVICE_DESCRIPTOR_LENGTH
        || descriptor[..usb::DESCRIPTOR_HEADER_LENGTH]
            != [usb::DEVICE_DESCRIPTOR_LENGTH as u8, usb::DESCRIPTOR_DEVICE]
        || configs.is_empty()
        || configs.len() > limits::MAX_CONFIGURATIONS
        || usize::from(descriptor[usb::DEVICE_CONFIGURATION_COUNT_OFFSET]) != configs.len()
        || !matches!(
            speed,
            usb::SPEED_LOW
                | usb::SPEED_FULL
                | usb::SPEED_HIGH
                | usb::SPEED_SUPER
                | usb::SPEED_SUPER_PLUS
        )
    {
        return Err(Error::Invalid);
    }
    let configurations: Vec<_> = configs
        .iter()
        .map(|c| configuration(c))
        .collect::<Result<_>>()?;
    let mut seen = BTreeSet::new();
    if configurations.iter().any(|c| !seen.insert(c.value)) {
        return Err(Error::Invalid);
    }
    let current = configurations
        .iter()
        .find(|c| c.value == active)
        .ok_or(Error::Unsupported)?;
    Ok(Device {
        key,
        wire: usbip::Device {
            path: usbip::WireString::new(format!("/IOUSBHost/{location:08x}").as_bytes())
                .map_err(|_| Error::Invalid)?,
            bus_id: key.id.bus_id(),
            bus_number: location >> usb::LOCATION_BUS_SHIFT,
            device_number: (location >> usb::LOCATION_DEVICE_SHIFT) & usb::LOCATION_DEVICE_MASK,
            speed,
            vendor_id: word(descriptor, usb::DEVICE_VENDOR_OFFSET),
            product_id: word(descriptor, usb::DEVICE_PRODUCT_OFFSET),
            device_bcd: word(descriptor, usb::DEVICE_BCD_OFFSET),
            class: descriptor[usb::DEVICE_CLASS_OFFSET],
            subclass: descriptor[usb::DEVICE_SUBCLASS_OFFSET],
            protocol: descriptor[usb::DEVICE_PROTOCOL_OFFSET],
            configuration: active,
            configurations: descriptor[usb::DEVICE_CONFIGURATION_COUNT_OFFSET],
            interfaces: current
                .interfaces
                .iter()
                .filter(|i| i.alternate == 0)
                .count() as u8,
        },
        configurations,
    })
}
/// Read a little-endian word from previously bounds-checked descriptor fields.
pub(crate) fn word(bytes: &[u8], offset: usize) -> u16 {
    u16::from_le_bytes([bytes[offset], bytes[offset + 1]])
}
