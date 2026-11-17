//! Layouts from usbip-win2's pinned include/usbip/vhci.h, checked against MSVC.
use knightbus_core::{Error, Result, topology::Speed, usbip};

pub(crate) const WORD: usize = size_of::<u32>();
pub(crate) const MAX_PORTS: usize = u8::MAX as usize;
pub(crate) const MIN_CLASS_PORTS: usize = 1;
pub(crate) const MAX_CLASS_PORTS: usize = MAX_PORTS - MIN_CLASS_PORTS;
pub(crate) const DEFAULT_CLASS_PORTS: usize = 30; // Pinned driver registry default.
pub(crate) const SIZE_OFFSET: usize = 0;
pub(crate) const LOCATION_OFFSET: usize = WORD;
pub(crate) const PORT_OFFSET: usize = 0;
const LOCATION_HASH_OFFSET: usize = PORT_OFFSET + WORD;
pub(crate) const REQUEST_PORT_OFFSET: usize = LOCATION_OFFSET + PORT_OFFSET;
pub(crate) const LOCATION_BYTES: usize = 1100;
pub(crate) const BUS_OFFSET: usize = LOCATION_HASH_OFFSET + WORD;
pub(crate) const BUS_BYTES: usize = usbip::BUS_ID_BYTES;
pub(crate) const SERVICE_OFFSET: usize = BUS_OFFSET + BUS_BYTES;
pub(crate) const SERVICE_BYTES: usize = 32;
pub(crate) const HOST_OFFSET: usize = SERVICE_OFFSET + SERVICE_BYTES;
pub(crate) const HOST_BYTES: usize = 1025;
pub(crate) const PLUGIN_BYTES: usize = 1124;
pub(crate) const STOP_BYTES: usize = WORD + LOCATION_BYTES + WORD;
pub(crate) const IMPORTED_BYTES: usize = 1132;
pub(crate) const SPEED_OFFSET: usize = LOCATION_BYTES + WORD;
pub(crate) const LIST_HEADER_BYTES: usize = WORD;
pub(crate) const LIST_REQUEST_BYTES: usize = LIST_HEADER_BYTES + IMPORTED_BYTES;
pub(crate) const LIST_BYTES: usize = LIST_HEADER_BYTES + MAX_PORTS * IMPORTED_BYTES;
pub(crate) const PLUGIN_REPLY_BYTES: usize = REQUEST_PORT_OFFSET + WORD;
pub(crate) const PLUGOUT_BYTES: usize = REQUEST_PORT_OFFSET + WORD;
const DEVICE_UNKNOWN: u32 = 0x22;
const READ_WRITE_ACCESS: u32 = 3;
const DEVICE_SHIFT: u32 = 16;
const ACCESS_SHIFT: u32 = 14;
const FUNCTION_SHIFT: u32 = 2;
#[repr(u32)]
enum Function {
    PluginOnce = 0x806,
    Plugout = 0x801,
    List = 0x802,
    Stop = 0x805,
}
const fn ioctl(function: Function) -> u32 {
    (DEVICE_UNKNOWN << DEVICE_SHIFT)
        | (READ_WRITE_ACCESS << ACCESS_SHIFT)
        | ((function as u32) << FUNCTION_SHIFT)
}
pub(crate) const PLUGIN: u32 = ioctl(Function::PluginOnce); // One attempt, never persist an attachment.
pub(crate) const PLUGOUT: u32 = ioctl(Function::Plugout);
pub(crate) const LIST: u32 = ioctl(Function::List);
pub(crate) const STOP: u32 = ioctl(Function::Stop);
pub(crate) fn word(bytes: &[u8], offset: usize) -> u32 {
    u32::from_le_bytes(bytes[offset..offset + WORD].try_into().unwrap())
}
pub(crate) fn put(bytes: &mut [u8], offset: usize, value: u32) {
    bytes[offset..offset + WORD].copy_from_slice(&value.to_le_bytes());
}
pub(crate) fn location(bus: &str, service: &str) -> Result<[u8; LOCATION_BYTES]> {
    let mut out = [0; LOCATION_BYTES];
    for (value, offset, capacity) in [
        (bus, BUS_OFFSET, BUS_BYTES),
        (service, SERVICE_OFFSET, SERVICE_BYTES),
        ("127.0.0.1", HOST_OFFSET, HOST_BYTES),
    ] {
        if value.is_empty() || value.len() >= capacity || !value.is_ascii() || value.contains('\0')
        {
            return Err(Error::Invalid);
        }
        out[offset..offset + value.len()].copy_from_slice(value.as_bytes());
    }
    Ok(out)
}
pub(crate) fn request(location: &[u8; LOCATION_BYTES], size: usize) -> Vec<u8> {
    let mut out = vec![0; size];
    put(&mut out, SIZE_OFFSET, size as u32);
    out[LOCATION_OFFSET..LOCATION_OFFSET + LOCATION_BYTES].copy_from_slice(location);
    out
}
pub(crate) fn devices(bytes: &[u8]) -> Result<impl Iterator<Item = &[u8]>> {
    if bytes.len() < LIST_HEADER_BYTES
        || bytes.len() > LIST_BYTES
        || !(bytes.len() - LIST_HEADER_BYTES).is_multiple_of(IMPORTED_BYTES)
    {
        return Err(Error::Invalid);
    }
    let mut ports = [false; MAX_PORTS + 1];
    for record in bytes[LIST_HEADER_BYTES..].as_chunks::<IMPORTED_BYTES>().0 {
        let port = word(record, PORT_OFFSET) as usize;
        if port == 0 || port > MAX_PORTS || ports[port] {
            return Err(Error::Invalid);
        }
        ports[port] = true;
        Speed::try_from(word(record, SPEED_OFFSET)).map_err(|_| Error::Invalid)?;
    }
    Ok(bytes[LIST_HEADER_BYTES..]
        .as_chunks::<IMPORTED_BYTES>()
        .0
        .iter()
        .map(|record| record.as_slice()))
}
pub(crate) fn same_location(record: &[u8], location: &[u8; LOCATION_BYTES]) -> bool {
    record[BUS_OFFSET..HOST_OFFSET + HOST_BYTES] == location[BUS_OFFSET..HOST_OFFSET + HOST_BYTES]
}
pub(crate) fn admit(bytes: &[u8], speed: Speed, capacity: usize) -> Result<()> {
    let superspeed = speed as u32 >= Speed::Super as u32;
    let used = devices(bytes)?
        .filter(|d| (word(d, SPEED_OFFSET) >= Speed::Super as u32) == superspeed)
        .count();
    if capacity == 0 || used >= capacity {
        Err(Error::Exhausted)
    } else {
        Ok(())
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn port_classes_are_admitted_independently_and_full_class_is_rejected() {
        let mut list = vec![0; LIST_HEADER_BYTES + DEFAULT_CLASS_PORTS * IMPORTED_BYTES];
        for (index, device) in list[LIST_HEADER_BYTES..]
            .as_chunks_mut::<IMPORTED_BYTES>()
            .0
            .iter_mut()
            .enumerate()
        {
            put(device, PORT_OFFSET, index as u32 + 1);
            put(device, SPEED_OFFSET, Speed::Super as u32);
        }
        assert_eq!(
            admit(&list, Speed::SuperPlus, DEFAULT_CLASS_PORTS),
            Err(Error::Exhausted)
        );
        assert!(admit(&list, Speed::High, DEFAULT_CLASS_PORTS).is_ok());
        list.truncate(list.len() - IMPORTED_BYTES);
        assert!(admit(&list, Speed::Super, DEFAULT_CLASS_PORTS).is_ok());
        put(
            &mut list,
            LIST_HEADER_BYTES + PORT_OFFSET,
            MAX_PORTS as u32 + 1,
        );
        assert!(devices(&list).is_err());
    }
    #[test]
    fn ioctl_numbers_match_upstream() {
        assert_eq!(PLUGIN, 0x22e018);
        assert_eq!(PLUGOUT, 0x22e004);
        assert_eq!(LIST, 0x22e008);
        assert_eq!(STOP, 0x22e014);
        assert_eq!(PLUGIN_REPLY_BYTES, 8);
        assert_eq!(PLUGOUT_BYTES, 8);
        assert_eq!(LIST_REQUEST_BYTES, 1136);
        assert_eq!((MIN_CLASS_PORTS, MAX_CLASS_PORTS), (1, 254));
    }
    #[test]
    fn plugin_layout_matches_upstream_abi_and_cleanup_is_location_scoped() {
        // Independent sizeof/offsetof results from the upstream MSVC headers.
        const MSVC_BUS_OFFSET: usize = 12;
        const MSVC_SERVICE_OFFSET: usize = 44;
        const MSVC_HOST_OFFSET: usize = 76;
        const MSVC_WSK_EVENTS_OFFSET: usize = 1120;
        const BUS: &str = "uniqueBus";
        const SERVICE: &str = "33333";
        const HOST: &str = "127.0.0.1";
        let location = location(BUS, SERVICE).unwrap();
        let plugin = request(&location, PLUGIN_BYTES);
        assert_eq!(plugin.len(), 1124);
        assert_eq!(
            &plugin[MSVC_BUS_OFFSET..MSVC_BUS_OFFSET + BUS.len()],
            BUS.as_bytes()
        );
        assert_eq!(
            &plugin[MSVC_SERVICE_OFFSET..MSVC_SERVICE_OFFSET + SERVICE.len()],
            SERVICE.as_bytes()
        );
        assert_eq!(
            &plugin[MSVC_HOST_OFFSET..MSVC_HOST_OFFSET + HOST.len()],
            HOST.as_bytes()
        );
        assert_eq!(plugin[MSVC_WSK_EVENTS_OFFSET], 0);
        assert!(same_location(&plugin[LOCATION_OFFSET..], &location));
        assert_eq!(word(&request(&location, STOP_BYTES), SIZE_OFFSET), 1108);
        assert!(super::location("", SERVICE).is_err());
    }
}
