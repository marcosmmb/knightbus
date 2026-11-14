//! SuperSpeed framing is checked before any kernel device is published.
use knightbus_core::{Budget, ResourceLimits};
use knightbus_windows_importer::{
    channel::Descriptors,
    descriptors::{self, Speed},
};
const DEVICE: [u8; 18] = [
    18, 1, 0x20, 3, 0, 0, 0, 9, 0xda, 0x0b, 0x10, 0x92, 1, 0xf0, 1, 2, 3, 1,
];
const CONFIG: [u8; 44] = [
    9, 2, 44, 0, 1, 1, 0, 0x80, 0x70, 9, 4, 0, 0, 2, 8, 6, 0x50, 0, 7, 5, 0x81, 2, 0, 4, 0, 6,
    0x30, 15, 0, 0, 0, 7, 5, 2, 2, 0, 4, 0, 6, 0x30, 15, 0, 0, 0,
];
const BOS: [u8; 15] = [5, 15, 15, 0, 1, 10, 16, 3, 0, 14, 0, 1, 10, 0, 0];

#[test]
fn superspeed_requires_complete_companions_and_bos() {
    for speed in [Speed::Super, Speed::SuperPlus] {
        let t = descriptors::validate(&DEVICE, &CONFIG, speed).unwrap();
        assert_eq!(t.endpoints.iter().flatten().count(), 2);
        assert_eq!(t.settings[0].unwrap().stream_endpoints, 0);
        descriptors::validate_bos(&BOS, speed).unwrap();
        let budget = Budget::new(ResourceLimits {
            bytes: 8192,
            requests: 1,
        });
        assert!(Descriptors::new(&budget, &DEVICE, &CONFIG, speed).is_err());
        let d = Descriptors::with_bos(&budget, &DEVICE, &CONFIG, &BOS, speed).unwrap();
        drop(d);
        assert_eq!(budget.usage().bytes, 0);
        for n in 0..CONFIG.len() {
            assert!(descriptors::validate(&DEVICE, &CONFIG[..n], speed).is_err());
        }
        for n in 0..BOS.len() {
            assert!(descriptors::validate_bos(&BOS[..n], speed).is_err());
        }
    }
    assert!(descriptors::validate(&DEVICE, &CONFIG, Speed::High).is_err());
    let mut bad = CONFIG;
    bad[26] = 0x24; // Replacing a required companion with an opaque class descriptor.
    assert!(descriptors::validate(&DEVICE, &bad, Speed::Super).is_err());
    bad = CONFIG;
    bad[27] = 16;
    assert!(descriptors::validate(&DEVICE, &bad, Speed::Super).is_err());
    bad = CONFIG;
    bad[28] = 17;
    assert!(descriptors::validate(&DEVICE, &bad, Speed::Super).is_err());
    bad = CONFIG;
    bad[29] = 1;
    assert!(descriptors::validate(&DEVICE, &bad, Speed::Super).is_err());
    let mut bos = BOS;
    bos[4] = 2;
    assert!(descriptors::validate_bos(&bos, Speed::Super).is_err());
    bos = BOS;
    bos[9] = 6;
    assert!(descriptors::validate_bos(&bos, Speed::Super).is_err());
}

#[test]
fn streams_are_recorded_per_setting_without_rewriting_descriptors() {
    let mut config = CONFIG;
    config[28] = 5;
    let topology = descriptors::validate(&DEVICE, &config, Speed::SuperPlus).unwrap();
    assert_eq!(topology.settings[0].unwrap().stream_endpoints, 1 << 17);
}
