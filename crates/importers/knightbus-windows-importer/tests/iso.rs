//! Checked packet envelopes from the native driver.
use knightbus_windows_importer::{
    channel::Request,
    descriptors::{Speed, validate},
};
use knightbus_windows_protocol::iso::{self, Packet};

fn word(bytes: &mut [u8], at: usize, value: u32) {
    bytes[at..at + 4].copy_from_slice(&value.to_le_bytes());
}

#[test]
fn native_iso_envelope_validates_every_boundary_before_borrowing() {
    let device = [18, 1, 0, 2, 0, 0, 0, 64, 9, 0x12, 1, 0, 0, 1, 0, 0, 0, 1];
    for address in [1, 0x81] {
        let config = [
            9, 2, 25, 0, 1, 1, 0, 0x80, 50, 9, 4, 0, 0, 1, 0xff, 0, 0, 0, 7, 5, address, 1, 64, 0,
            1,
        ];
        let topology = validate(&device, &config, Speed::Full).unwrap();
        let payload = if address == 1 { 16 } else { 0 };
        let mut bytes = vec![0; iso::HEADER + payload + 32];
        for (offset, value) in [
            (0, iso::VERSION),
            (4, address as u32),
            (8, 1),
            (16, 16),
            (20, payload as u32),
            (32, 2),
            (36, iso::ASAP),
            (44, 30),
        ] {
            word(&mut bytes, offset, value);
        }
        bytes[iso::HEADER..iso::HEADER + payload].fill(0x5a);
        for (p, out) in [
            Packet {
                offset: 0,
                length: 4,
                ..Default::default()
            },
            Packet {
                offset: 12,
                length: 4,
                ..Default::default()
            },
        ]
        .iter()
        .zip(bytes[iso::HEADER + payload..].chunks_exact_mut(16))
        {
            p.write(out);
        }
        let request = Request::decode(&bytes, &topology).unwrap();
        assert_eq!(request.slot, 30);
        assert_eq!(request.payload, vec![0x5a; payload]);
        assert_eq!(request.packets.unwrap().len(), 2);
        for end in 0..bytes.len() {
            assert!(Request::decode(&bytes[..end], &topology).is_err(), "{end}");
        }
        for (offset, value) in [
            (0, 0),
            (4, 0),
            (8, 0),
            (16, u32::MAX),
            (20, u32::MAX),
            (24, 1),
            (32, 0),
            (32, 1025),
            (36, 0),
            (40, 1),
            (44, 0),
            (44, 31),
        ] {
            let mut bad = bytes.clone();
            word(&mut bad, offset, value);
            assert!(
                Request::decode(&bad, &topology).is_err(),
                "{offset}/{value}"
            );
        }
        let mut extra = bytes.clone();
        extra.push(0);
        assert!(Request::decode(&extra, &topology).is_err());
    }
}
