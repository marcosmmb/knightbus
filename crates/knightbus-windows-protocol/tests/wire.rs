use knightbus_windows_protocol::{descriptors::Speed, wire::*};

fn put(bytes: &mut [u8], offset: usize, value: u32) {
    bytes[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
}
fn attachment() -> Vec<u8> {
    let device = [18, 1, 0, 2, 0, 0, 0, 64, 9, 0x12, 1, 0, 0, 1, 0, 0, 0, 1];
    let configuration = [9, 2, 18, 0, 1, 1, 0, 0x80, 50, 9, 4, 0, 0, 0, 0xff, 0, 0, 0];
    let mut bytes = b"KBWD".to_vec();
    for value in [1u32, 2, 18, 18] {
        bytes.extend_from_slice(&value.to_le_bytes());
    }
    bytes.extend_from_slice(&device);
    bytes.extend_from_slice(&configuration);
    bytes
}
#[test]
fn attachment_checks_every_boundary_before_exposing_descriptors() {
    let bytes = attachment();
    let parsed = Attachment::decode(&bytes).unwrap();
    assert_eq!(parsed.speed, Speed::Full);
    assert_eq!(parsed.endpoints, [0; SLOTS]);
    assert_eq!(parsed.device, &bytes[20..38]);
    assert_eq!(parsed.configuration, &bytes[38..]);
    for end in 0..bytes.len() {
        assert!(Attachment::decode(&bytes[..end]).is_err(), "{end}");
    }
    for (offset, value) in [(0, 0), (4, 3), (8, 4), (12, 17), (16, 0), (16, u32::MAX)] {
        let mut malformed = bytes.clone();
        put(&mut malformed, offset, value);
        assert!(Attachment::decode(&malformed).is_err(), "{offset} {value}");
    }
    let mut extra = bytes.clone();
    extra.push(0);
    assert!(Attachment::decode(&extra).is_err());
    put(&mut extra, 4, 2);
    assert!(Attachment::decode(&extra).is_err());
    let mut v2 = bytes;
    put(&mut v2, 4, 2);
    assert!(
        Attachment::decode(&v2).is_ok(),
        "v2 accepts USB2 with no BOS"
    );
    put(&mut v2, 8, 5);
    assert!(
        Attachment::decode(&v2).is_err(),
        "speed must agree with descriptors and BOS"
    );
}
fn reply(completed: u32, payload: usize, status: u32) -> Vec<u8> {
    let mut bytes = vec![0; HEADER + payload];
    for (offset, value) in [
        (0, 1),
        (4, 0x81),
        (8, 42),
        (16, completed),
        (20, payload as u32),
        (24, status),
    ] {
        put(&mut bytes, offset, value);
    }
    bytes[HEADER..].fill(0xa5);
    bytes
}
#[test]
fn reply_validates_envelope_before_slot_ownership_changes() {
    let bytes = reply(3, 3, 0);
    let parsed = Reply::decode(&bytes).unwrap();
    assert_eq!(parsed.token, 42);
    assert_eq!(parsed.endpoint, 0x81);
    assert_eq!(parsed.payload, [0xa5; 3]);
    for end in 0..bytes.len() {
        assert!(Reply::decode(&bytes[..end]).is_err());
    }
    for (offset, value) in [(0, 0), (20, 2), (20, u32::MAX), (24, 3), (28, 1)] {
        let mut malformed = bytes.clone();
        put(&mut malformed, offset, value);
        assert!(Reply::decode(&malformed).is_err());
    }
    let mut trailing = bytes;
    trailing.push(0);
    assert!(Reply::decode(&trailing).is_err());
    assert!(Reply::decode(&reply(0, MAX_TRANSFER + 1, 0)).is_err());
}
#[test]
fn reply_length_matrix_preserves_direction_status_and_short_transfer_rules() {
    for requested in [0, 1, 8, MAX_TRANSFER] {
        for completed in [0, 1, 8, MAX_TRANSFER, MAX_TRANSFER + 1] {
            for payload in [0, 1, 8] {
                for status in 0..=2 {
                    for input in [false, true] {
                        let bytes = reply(completed as u32, payload, status);
                        let parsed = Reply::decode(&bytes).unwrap();
                        let valid = completed <= requested
                            && (status == 0 || (completed == 0 && payload == 0))
                            && payload == if input { completed } else { 0 };
                        assert_eq!(
                            parsed.validate_length(requested, input).is_ok(),
                            valid,
                            "requested={requested} completed={completed} payload={payload} status={status} input={input}"
                        );
                    }
                }
            }
        }
    }
}
#[test]
fn fetch_admits_only_implemented_slots_and_preserves_empty_default() {
    assert_eq!(fetch_mask(&[]), Ok(SLOT_MASK));
    for slot in 0..SLOTS {
        assert_eq!(fetch_mask(&(1u32 << slot).to_le_bytes()), Ok(1 << slot));
    }
    for mask in [0u32, 1 << 31, u32::MAX] {
        assert_eq!(fetch_mask(&mask.to_le_bytes()), Err(Error::Fields));
    }
    for count in [1, 2, 3, 5] {
        assert_eq!(fetch_mask(&vec![1; count]), Err(Error::Length));
    }
}

#[test]
fn native_descriptor_policy_stays_stricter_than_usb_structure() {
    use knightbus_windows_protocol::descriptors as native;
    let bytes = attachment();
    let mut device = bytes[20..38].to_vec();
    let mut config = bytes[38..].to_vec();
    device[17] = 2;
    assert!(knightbus_usb::validate(&device, &config, Speed::Full).is_ok());
    assert!(native::validate(&device, &config, Speed::Full).is_err());
    device[17] = 1;
    config[7] |= 0x20;
    assert!(knightbus_usb::validate(&device, &config, Speed::Full).is_ok());
    assert!(native::validate(&device, &config, Speed::Full).is_err());
    config[7] = 0x80;
    for _ in 0..2040 {
        config.extend_from_slice(&[2, 0x24]);
    }
    let length = config.len() as u16;
    config[2..4].copy_from_slice(&length.to_le_bytes());
    assert!(knightbus_usb::validate(&device, &config, Speed::Full).is_ok());
    assert!(native::validate(&device, &config, Speed::Full).is_err());
    let mut bos = vec![5, 15, 0, 0, 17];
    for _ in 0..17 {
        let mut capability = [0; 255];
        capability[..3].copy_from_slice(&[255, 16, 0xff]);
        bos.extend_from_slice(&capability);
    }
    let length = bos.len() as u16;
    bos[2..4].copy_from_slice(&length.to_le_bytes());
    assert!(knightbus_usb::validate_bos(&bos, Speed::Full).is_ok());
    assert!(native::validate_bos(&bos, Speed::Full).is_err());
}

#[test]
fn native_attachment_accepts_iso_in_every_alternate_and_direction() {
    for address in [1, 0x81] {
        for alternate in [0, 1] {
            let base = attachment();
            let device = &base[20..38];
            let mut config = vec![9, 2, 0, 0, 1, 1, 0, 0x80, 50];
            if alternate != 0 {
                config.extend([9, 4, 0, 0, 1, 0xff, 0, 0, 0, 7, 5, address, 2, 64, 0, 0]);
            }
            config.extend([
                9, 4, 0, alternate, 1, 0xff, 0, 0, 0, 7, 5, address, 1, 64, 0, 1,
            ]);
            config[2] = config.len() as u8;
            assert!(knightbus_usb::validate(device, &config, Speed::Full).is_ok());
            let mut bytes = base[..38].to_vec();
            bytes[16..20].copy_from_slice(&(config.len() as u32).to_le_bytes());
            bytes.extend(config);
            assert!(Attachment::decode(&bytes).is_ok());
        }
    }
}
