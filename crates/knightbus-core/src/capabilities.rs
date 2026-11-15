//! Explicit native limits, checked before publication and transfer admission.
use crate::{DescriptorSet, Device, Error, Result};

/// Implemented native behavior. Adapters must declare every field explicitly.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Capabilities {
    /// Maximum bytes in one transfer.
    pub transfer_bytes: usize,
    /// Maximum outstanding requests in the device worker.
    pub requests: usize,
    /// Maximum ISO packets; zero disables ISO.
    pub iso_packets: usize,
    /// Supported USB/IP speed bits (`1 << speed`).
    pub speeds: u32,
    /// Maximum bytes in a configuration or BOS descriptor tree.
    pub descriptor_bytes: usize,
    /// Maximum configurations accepted from a device.
    pub configurations: usize,
    /// Native remote-wakeup support.
    pub remote_wakeup: bool,
}
impl Capabilities {
    /// Validate finite bounds before allocating workers or opening native objects.
    pub fn validate(self) -> Result<()> {
        if self.transfer_bytes == 0
            || self.transfer_bytes > i32::MAX as usize
            || self.requests == 0
            || self.requests > 64
            || self.iso_packets > 1024
            || self.descriptor_bytes < 9
            || self.descriptor_bytes > u16::MAX as usize
            || self.configurations == 0
            || self.configurations > 16
            || self.speeds == 0
            || self.speeds & !0b1101110 != 0
        {
            return Err(Error::Invalid);
        }
        Ok(())
    }
    /// Check advertised identity and configuration bounds.
    pub fn device(self, device: &Device) -> Result<()> {
        self.validate()?;
        if device.wire.speed >= 32
            || self.speeds & (1 << device.wire.speed) == 0
            || usize::from(device.wire.configurations) > self.configurations
        {
            return Err(Error::Unsupported);
        }
        Ok(())
    }
    /// Check raw descriptor policies before native publication.
    pub fn descriptors(self, descriptors: &DescriptorSet) -> Result<()> {
        self.validate()?;
        if descriptors.configuration().len() > self.descriptor_bytes
            || descriptors.bos().len() > self.descriptor_bytes
            || self.speeds & (1 << descriptors.speed as u32) == 0
            || usize::from(descriptors.device()[17]) > self.configurations
            || (!self.remote_wakeup && descriptors.configuration()[7] & 0x20 != 0)
        {
            return Err(Error::Unsupported);
        }
        Ok(())
    }
}
