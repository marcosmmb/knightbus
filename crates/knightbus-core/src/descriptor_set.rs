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
                    mask | (1 << (index + 1))
                } else {
                    mask
                }
            },
        )
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
                return Ok(
                    if d[descriptors::endpoint::ATTRIBUTES] == descriptors::TRANSFER_BULK {
                        0
                    } else if self.speed == Speed::High || self.speed.superspeed() {
                        1 << (d[descriptors::endpoint::INTERVAL] - 1)
                    } else {
                        d[descriptors::endpoint::INTERVAL].into()
                    },
                );
            }
            offset += d.len();
        }
        Err(Error::Invalid)
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
}
