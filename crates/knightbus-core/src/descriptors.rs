//! Checked physical descriptor parsing. Class/vendor descriptors remain opaque.
use knightbus_usb as usb;
pub(crate) mod limits {
    pub const MAX_CONFIGURATIONS: usize = 16;
    pub const MAX_INTERFACE_ALTERNATES: usize = 64;
}
use crate::{
    Configuration, Device, DeviceKey, Endpoint, Error, Interface, Result, TransferKind, usbip,
};
use std::collections::BTreeSet;

/// Maximum descriptor bytes accepted per configuration.
pub const MAX_CONFIGURATION: usize = u16::MAX as usize;
/// Parse one complete configuration, rejecting ambiguous endpoint ownership.
pub fn configuration(bytes: &[u8]) -> Result<Configuration> {
    if bytes.len() < usb::configuration::BYTES
        || bytes.len() > MAX_CONFIGURATION
        || bytes[..usb::DESCRIPTOR_HEADER_BYTES]
            != [usb::configuration::BYTES as u8, usb::configuration::TYPE]
        || usize::from(usb::u16_at(bytes, usb::configuration::TOTAL_LENGTH)) != bytes.len()
        || bytes[usb::configuration::VALUE] == 0
        || bytes[usb::configuration::ATTRIBUTES] & usb::CONFIG_ATTRIBUTES_MASK
            != usb::CONFIG_REQUIRED_BIT
    {
        return Err(Error::Invalid);
    }
    let mut result = Configuration {
        value: bytes[usb::configuration::VALUE],
        interfaces: Vec::new(),
    };
    let mut offset = usb::configuration::BYTES;
    let mut expected = 0;
    let mut previous_type = usb::configuration::TYPE;
    while offset < bytes.len() {
        if bytes.len() - offset < usb::DESCRIPTOR_HEADER_BYTES {
            return Err(Error::Invalid);
        }
        let len = usize::from(bytes[offset]);
        if len < usb::DESCRIPTOR_HEADER_BYTES || len > bytes.len() - offset {
            return Err(Error::Invalid);
        }
        let d = &bytes[offset..offset + len];
        match d[usb::DESCRIPTOR_TYPE] {
            usb::interface::TYPE => {
                if len != usb::interface::BYTES
                    || result.interfaces.len() == limits::MAX_INTERFACE_ALTERNATES
                    || usize::from(d[usb::interface::ENDPOINTS]) > usb::MAX_ENDPOINTS
                    || result.interfaces.iter().any(|i| {
                        i.number == d[usb::interface::NUMBER]
                            && i.alternate == d[usb::interface::ALTERNATE]
                    })
                    || result
                        .interfaces
                        .last()
                        .is_some_and(|i| i.endpoints.len() != expected)
                {
                    return Err(Error::Invalid);
                }
                expected = usize::from(d[usb::interface::ENDPOINTS]);
                result
                    .interfaces
                    .try_reserve_exact(1)
                    .map_err(|_| Error::Exhausted)?;
                let mut endpoints = Vec::new();
                endpoints
                    .try_reserve_exact(expected)
                    .map_err(|_| Error::Exhausted)?;
                result.interfaces.push(Interface {
                    number: d[usb::interface::NUMBER],
                    alternate: d[usb::interface::ALTERNATE],
                    class: usbip::Interface {
                        class: d[usb::interface::CLASS],
                        subclass: d[usb::interface::SUBCLASS],
                        protocol: d[usb::interface::PROTOCOL],
                    },
                    endpoints,
                });
            }
            usb::endpoint::TYPE => {
                let interface = result.interfaces.last_mut().ok_or(Error::Invalid)?;
                if len < usb::endpoint::MIN_BYTES
                    || d[usb::endpoint::ADDRESS] & usb::ENDPOINT_RESERVED_MASK != 0
                    || d[usb::endpoint::ADDRESS] & usb::ENDPOINT_NUMBER_MASK == 0
                    || usb::u16_at(d, usb::endpoint::MAX_PACKET) & usb::PACKET_SIZE_MASK == 0
                    || interface.endpoints.len() >= expected
                    || interface
                        .endpoints
                        .iter()
                        .any(|e| e.address == d[usb::endpoint::ADDRESS])
                {
                    return Err(Error::Invalid);
                }
                let kind = match d[usb::endpoint::ATTRIBUTES] & usb::TRANSFER_TYPE_MASK {
                    usb::TRANSFER_ISOCHRONOUS => TransferKind::Isochronous,
                    usb::TRANSFER_BULK => TransferKind::Bulk,
                    usb::TRANSFER_INTERRUPT => TransferKind::Interrupt,
                    _ => return Err(Error::Unsupported),
                };
                interface.endpoints.push(Endpoint {
                    address: d[usb::endpoint::ADDRESS],
                    kind,
                    max_packet_size: usb::u16_at(d, usb::endpoint::MAX_PACKET),
                    service_bytes: u32::from(
                        usb::u16_at(d, usb::endpoint::MAX_PACKET) & usb::PACKET_SIZE_MASK,
                    ) * (1 + u32::from(
                        (usb::u16_at(d, usb::endpoint::MAX_PACKET) >> usb::TRANSACTIONS_SHIFT)
                            & usb::TRANSACTIONS_MASK,
                    )),
                    interval: d[usb::endpoint::INTERVAL],
                    allowed_flags: if kind == TransferKind::Isochronous {
                        crate::usbip::ISO_ASAP
                    } else {
                        0
                    },
                });
            }
            usb::companion::TYPE => {
                let endpoint = result
                    .interfaces
                    .last_mut()
                    .and_then(|i| i.endpoints.last_mut())
                    .ok_or(Error::Invalid)?;
                if len != usb::companion::BYTES || previous_type != usb::endpoint::TYPE {
                    return Err(Error::Invalid);
                }
                if endpoint.kind == TransferKind::Isochronous
                    || endpoint.kind == TransferKind::Interrupt
                {
                    endpoint.service_bytes =
                        u32::from(usb::u16_at(d, usb::companion::BYTES_PER_INTERVAL));
                    if endpoint.service_bytes == 0 {
                        return Err(Error::Invalid);
                    }
                }
            }
            usb::device::TYPE | usb::configuration::TYPE => return Err(Error::Invalid),
            _ => {} // Retain raw bytes separately; do not reinterpret class requests.
        }
        previous_type = d[usb::DESCRIPTOR_TYPE];
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
            != usize::from(bytes[usb::configuration::INTERFACES])
        || result.interfaces.iter().any(|i| {
            !result
                .interfaces
                .iter()
                .any(|base| base.number == i.number && base.alternate == 0)
        })
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
/// Native identity used in portable inventory.
pub struct Address<'a> {
    /// Native path, encoded as bytes without a trailing NUL.
    pub path: &'a [u8],
    /// USB/IP bus number.
    pub bus: u16,
    /// USB/IP device number.
    pub device: u16,
}
/// Build engine inventory from physical descriptor bytes and a generation.
pub fn device(
    key: DeviceKey,
    address: Address<'_>,
    speed: u32,
    descriptor: &[u8],
    configs: &[Vec<u8>],
    active: u8,
) -> Result<Device> {
    if descriptor.len() != usb::device::BYTES
        || descriptor[..usb::DESCRIPTOR_HEADER_BYTES]
            != [usb::device::BYTES as u8, usb::device::TYPE]
        || configs.is_empty()
        || configs.len() > limits::MAX_CONFIGURATIONS
        || usize::from(descriptor[usb::device::CONFIGURATIONS]) != configs.len()
        || usb::Speed::try_from(speed).is_err()
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
            path: usbip::WireString::new(address.path).map_err(|_| Error::Invalid)?,
            bus_id: key.id.bus_id(),
            bus_number: u32::from(address.bus),
            device_number: u32::from(address.device),
            speed,
            vendor_id: usb::u16_at(descriptor, usb::device::VENDOR),
            product_id: usb::u16_at(descriptor, usb::device::PRODUCT),
            device_bcd: usb::u16_at(descriptor, usb::device::BCD),
            class: descriptor[usb::device::CLASS],
            subclass: descriptor[usb::device::SUBCLASS],
            protocol: descriptor[usb::device::PROTOCOL],
            configuration: active,
            configurations: descriptor[usb::device::CONFIGURATIONS],
            interfaces: current
                .interfaces
                .iter()
                .filter(|i| i.alternate == 0)
                .count() as u8,
        },
        configurations,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn companion_must_immediately_follow_its_endpoint() {
        let mut config = vec![
            9, 2, 31, 0, 1, 1, 0, 0x80, 50, 9, 4, 0, 0, 1, 0xff, 0, 0, 0, 7, 5, 0x81, 1, 0, 4, 1,
            6, 0x30, 1, 2, 0, 24,
        ];
        let parsed = configuration(&config).unwrap();
        assert_eq!(parsed.interfaces[0].endpoints[0].service_bytes, 6144);
        for extra in [&[2, 0x24][..], &[6, 0x30, 1, 2, 0, 24][..]] {
            let mut invalid = config.clone();
            invalid.splice(25..25, extra.iter().copied());
            invalid[2] = invalid.len() as u8;
            assert!(matches!(configuration(&invalid), Err(Error::Invalid)));
        }
        config[29..31].fill(0);
        assert!(matches!(configuration(&config), Err(Error::Invalid)));
    }
}
