//! Validated raw USB descriptors with charged storage.
use crate::{Budget, Buffer, Error, Result};
use knightbus_usb::{self as descriptors, Speed, Topology};
/// Owns descriptor storage independently of any native attachment encoding.
pub struct DescriptorSet {
    bytes: Buffer,
    configuration_len: usize,
    /// Checked topology of the selected configuration.
    pub(crate) topology: Topology,
    /// Actual negotiated speed.
    pub(crate) speed: Speed,
}
impl DescriptorSet {
    /// Validate descriptors before allocating their shared storage.
    pub fn new(
        budget: &Budget,
        device: &[u8],
        config: &[u8],
        bos: &[u8],
        speed: Speed,
    ) -> Result<Self> {
        let topology =
            descriptors::validate(device, config, speed).map_err(|_| Error::Unsupported)?;
        descriptors::validate_bos(bos, speed).map_err(|_| Error::Unsupported)?;
        let mut bytes = budget.buffer(device.len() + config.len() + bos.len())?;
        let (d, tail) = bytes.as_mut_slice().split_at_mut(device.len());
        d.copy_from_slice(device);
        let (c, b) = tail.split_at_mut(config.len());
        c.copy_from_slice(config);
        b.copy_from_slice(bos);
        Ok(Self {
            bytes,
            configuration_len: config.len(),
            topology,
            speed,
        })
    }
    /// Checked topology; callers cannot mutate validated metadata.
    pub fn topology(&self) -> &Topology {
        &self.topology
    }
    /// Actual negotiated USB speed.
    pub fn speed(&self) -> Speed {
        self.speed
    }
    /// Complete device descriptor.
    pub fn device(&self) -> &[u8] {
        &self.bytes.as_slice()[..descriptors::device::BYTES]
    }
    /// Selected configuration and all its alternate settings.
    pub fn configuration(&self) -> &[u8] {
        &self.bytes.as_slice()
            [descriptors::device::BYTES..descriptors::device::BYTES + self.configuration_len]
    }
    /// BOS descriptor tree, empty when absent.
    pub fn bos(&self) -> &[u8] {
        &self.bytes.as_slice()[descriptors::device::BYTES + self.configuration_len..]
    }
    /// Native slots belonging to the selected alternate settings.
    pub fn active_slots(&self, alternates: &[u8; descriptors::MAX_SETTINGS]) -> u32 {
        self.topology.endpoints.iter().flatten().enumerate().fold(
            1, // Endpoint zero must remain available to change settings.
            |mask, (index, endpoint)| {
                let bit = 1u32
                    << ((endpoint.address & descriptors::ENDPOINT_NUMBER_MASK)
                        + if endpoint.address & descriptors::DIRECTION_IN != 0 {
                            descriptors::IN_ENDPOINT_BIT_OFFSET
                        } else {
                            0
                        });
                if self.topology.settings.iter().flatten().any(|setting| {
                    setting.interface == endpoint.interface
                        && setting.alternate == alternates[endpoint.interface as usize]
                        && setting.endpoints & bit != 0
                }) {
                    if self
                        .endpoint(endpoint.address, alternates)
                        .is_ok_and(|e| e.kind == crate::TransferKind::Isochronous)
                    {
                        // ISO may use several native slots for the same endpoint.
                        let first_spare = self.topology.endpoints.iter().flatten().count() + 1;
                        let spare = ((1u32 << 31) - 1) & !((1u32 << first_spare) - 1);
                        mask | (1 << (index + 1)) | spare
                    } else {
                        mask | (1 << (index + 1))
                    }
                } else {
                    mask
                }
            },
        )
    }
    /// Endpoint properties from the currently selected alternate setting.
    pub fn endpoint(
        &self,
        address: u8,
        alternates: &[u8; descriptors::MAX_SETTINGS],
    ) -> Result<crate::Endpoint> {
        let config = self.configuration();
        let mut offset = descriptors::configuration::BYTES;
        let mut selected = false;
        while offset < config.len() {
            let d = &config[offset..offset + config[offset] as usize];
            if d[descriptors::DESCRIPTOR_TYPE] == descriptors::interface::TYPE {
                selected = alternates[d[descriptors::interface::NUMBER] as usize]
                    == d[descriptors::interface::ALTERNATE];
            }
            if d[descriptors::DESCRIPTOR_TYPE] == descriptors::endpoint::TYPE
                && selected
                && d[descriptors::endpoint::ADDRESS] == address
            {
                let kind =
                    match d[descriptors::endpoint::ATTRIBUTES] & descriptors::TRANSFER_TYPE_MASK {
                        descriptors::TRANSFER_ISOCHRONOUS => crate::TransferKind::Isochronous,
                        descriptors::TRANSFER_INTERRUPT => crate::TransferKind::Interrupt,
                        _ => crate::TransferKind::Bulk,
                    };
                let packet = descriptors::u16_at(d, descriptors::endpoint::MAX_PACKET);
                let service_bytes = if self.speed.superspeed() && kind != crate::TransferKind::Bulk
                {
                    u32::from(descriptors::u16_at(&config[offset + d.len()..], 4))
                } else {
                    u32::from(packet & descriptors::PACKET_SIZE_MASK)
                        * (1 + u32::from(
                            (packet >> descriptors::TRANSACTIONS_SHIFT)
                                & descriptors::TRANSACTIONS_MASK,
                        ))
                };
                return Ok(crate::Endpoint {
                    address,
                    kind,
                    max_packet_size: packet,
                    service_bytes,
                    interval: d[descriptors::endpoint::INTERVAL],
                    allowed_flags: if kind == crate::TransferKind::Isochronous {
                        crate::usbip::ISO_ASAP
                    } else {
                        0
                    },
                });
            }
            offset += d.len();
        }
        Err(Error::Invalid)
    }
    /// USB/IP polling interval for the currently selected endpoint.
    pub fn interval(
        &self,
        address: u8,
        alternates: &[u8; descriptors::MAX_SETTINGS],
    ) -> Result<i32> {
        if address == 0 {
            return Ok(0);
        }
        let e = self.endpoint(address, alternates)?;
        Ok(if e.kind == crate::TransferKind::Bulk {
            0
        } else if self.speed == Speed::High
            || self.speed.superspeed()
            || e.kind == crate::TransferKind::Isochronous
        {
            1 << (e.interval - 1)
        } else {
            i32::from(e.interval)
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn interrupt_period_tracks_speed_and_selected_alternate() {
        let budget = Budget::new(crate::ResourceLimits {
            bytes: 4096,
            requests: 4,
        });
        let device = [18, 1, 0, 2, 0, 0, 0, 64, 9, 0x12, 1, 0, 0, 1, 0, 0, 0, 1];
        let mut config = [
            9, 2, 41, 0, 1, 1, 0, 0x80, 50, 9, 4, 0, 0, 1, 0xff, 0, 0, 0, 7, 5, 0x81, 3, 0, 4, 7,
            9, 4, 0, 1, 1, 0xff, 0, 0, 0, 7, 5, 0x81, 3, 0, 4, 4,
        ];
        let d = DescriptorSet::new(&budget, &device, &config, &[], Speed::High).unwrap();
        let mut alternates = [0; descriptors::MAX_SETTINGS];
        assert_eq!(d.interval(0x81, &alternates), Ok(64));
        alternates[0] = 1;
        assert_eq!(d.interval(0x81, &alternates), Ok(8));
        alternates[0] = 2;
        assert_eq!(d.interval(0x81, &alternates), Err(Error::Invalid));
        assert_eq!(d.interval(0, &alternates), Ok(0));
        config[22] = 8;
        config[23] = 0;
        config[38] = 8;
        config[39] = 0;
        let d = DescriptorSet::new(&budget, &device, &config, &[], Speed::Full).unwrap();
        alternates[0] = 0;
        assert_eq!(d.interval(0x81, &alternates), Ok(7));
    }
    #[test]
    fn endpoint_type_and_service_size_follow_selected_alternate() {
        let budget = Budget::new(crate::ResourceLimits {
            bytes: 4096,
            requests: 4,
        });
        let device = [18, 1, 0, 2, 0, 0, 0, 64, 9, 0x12, 1, 0, 0, 1, 0, 0, 0, 1];
        let config = [
            9, 2, 41, 0, 1, 1, 0, 0x80, 50, 9, 4, 0, 0, 1, 0xff, 0, 0, 0, 7, 5, 0x81, 2, 0, 2, 0,
            9, 4, 0, 1, 1, 0xff, 0, 0, 0, 7, 5, 0x81, 1, 0, 0x14, 4,
        ];
        let set = DescriptorSet::new(&budget, &device, &config, &[], Speed::High).unwrap();
        let mut alternates = [0; descriptors::MAX_SETTINGS];
        let endpoint = set.endpoint(0x81, &alternates).unwrap();
        assert_eq!(endpoint.kind, crate::TransferKind::Bulk);
        assert_eq!(endpoint.service_bytes, 512);
        assert_eq!(set.interval(0x81, &alternates), Ok(0));
        alternates[0] = 1;
        let endpoint = set.endpoint(0x81, &alternates).unwrap();
        assert_eq!(endpoint.kind, crate::TransferKind::Isochronous);
        assert_eq!(endpoint.service_bytes, 3072);
        assert_eq!(set.interval(0x81, &alternates), Ok(8));
        assert_eq!(endpoint.allowed_flags, crate::usbip::ISO_ASAP);
        assert_ne!(set.topology().iso_endpoints, 0);
    }
}
