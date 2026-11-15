//! Decode device-management requests before they reach a native adapter.
use super::{ControlError, Driver, InterfaceDriver, endpoint_bit};
use crate::{Device, Error, Result};
use knightbus_usb as usb;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Change {
    Configuration(u8),
    Alternate { interface: u8, alternate: u8 },
    ClearHalt(u8),
}
const CONTROL_PIPE: u32 = 1;
const ALL_PIPES: u32 = u32::MAX;

impl Change {
    pub fn decode(s: &[u8; usb::SETUP_SIZE]) -> Result<Option<Self>> {
        if s[usb::SETUP_TYPE] & usb::REQUEST_TYPE_MASK != 0 {
            return Ok(None);
        }
        let value = usb::u16_at(s, usb::SETUP_VALUE);
        let index = usb::u16_at(s, usb::SETUP_INDEX);
        let change = match s[usb::SETUP_REQUEST] {
            usb::SET_CONFIGURATION => {
                if s[usb::SETUP_TYPE] != usb::RECIPIENT_DEVICE
                    || value > u16::from(u8::MAX)
                    || index != 0
                {
                    return Err(Error::Invalid);
                }
                Self::Configuration(value as u8)
            }
            usb::SET_INTERFACE => {
                if s[usb::SETUP_TYPE] != usb::RECIPIENT_INTERFACE
                    || value > u16::from(u8::MAX)
                    || index > u16::from(u8::MAX)
                {
                    return Err(Error::Invalid);
                }
                Self::Alternate {
                    interface: index as u8,
                    alternate: value as u8,
                }
            }
            usb::CLEAR_FEATURE
                if s[usb::SETUP_TYPE] & usb::RECIPIENT_MASK == usb::RECIPIENT_ENDPOINT =>
            {
                if s[usb::SETUP_TYPE] != usb::RECIPIENT_ENDPOINT
                    || value != usb::ENDPOINT_HALT
                    || index > u16::from(u8::MAX)
                    || index as u8 & usb::ENDPOINT_RESERVED_MASK != 0
                    || index as u8 & usb::ENDPOINT_NUMBER_MASK == 0
                {
                    return Err(Error::Invalid);
                }
                Self::ClearHalt(index as u8)
            }
            _ => return Ok(None),
        };
        if usb::u16_at(s, usb::SETUP_LENGTH) != 0 {
            return Err(Error::Invalid);
        }
        Ok(Some(change))
    }
    pub fn scope(self, device: &Device, configuration: u8) -> u32 {
        match self {
            Self::Configuration(_) => ALL_PIPES,
            Self::Alternate { interface, .. } => device
                .configurations
                .iter()
                .filter(|c| c.value == configuration)
                .flat_map(|c| &c.interfaces)
                .filter(|i| i.number == interface)
                .flat_map(|i| &i.endpoints)
                .fold(CONTROL_PIPE, |mask, e| mask | endpoint_bit(e.address)),
            Self::ClearHalt(address) => CONTROL_PIPE | endpoint_bit(address),
        }
    }
    // The boolean reports a failed native topology change. Its state may be
    // partial, so the worker must retire the device rather than keep using it.
    pub fn execute<D: Driver>(
        self,
        driver: &mut D,
        device: &Device,
    ) -> (std::result::Result<(), ControlError>, bool) {
        let configuration = device
            .configurations
            .iter()
            .find(|c| c.value == driver.configuration());
        let result = match self {
            Self::Configuration(value) => {
                let selected = device.configurations.iter().find(|c| c.value == value);
                if value != 0 && selected.is_none() {
                    return (Err(Error::Invalid.into()), false);
                }
                driver.configure(selected)
            }
            Self::Alternate {
                interface,
                alternate,
            } => {
                let setting = configuration.and_then(|c| {
                    c.interfaces
                        .iter()
                        .find(|i| i.number == interface && i.alternate == alternate)
                });
                let Some(setting) = setting else {
                    return (Err(Error::Invalid.into()), false);
                };
                match driver.interface(interface) {
                    Ok(native) => native.select_alternate(setting),
                    Err(error) => return (Err(error.into()), false),
                }
            }
            Self::ClearHalt(address) => {
                let interface = configuration.and_then(|c| {
                    c.interfaces
                        .iter()
                        .find(|i| i.endpoints.iter().any(|e| e.address == address))
                });
                let Some(interface) = interface else {
                    return (Err(Error::Invalid.into()), false);
                };
                if driver.endpoint(address).is_none() {
                    return (Err(Error::Invalid.into()), false);
                }
                let result = driver
                    .interface(interface.number)
                    .map_err(ControlError::from)
                    .and_then(|native| native.clear_halt(address));
                let removed = result
                    .as_ref()
                    .is_err_and(|error| error.0 == crate::usbip::status::NO_DEVICE);
                return (result, removed);
            }
        };
        let invalidated = result.is_err();
        (result, invalidated)
    }
}
