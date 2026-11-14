//! Capture admission without physical USB access.
#[cfg(target_os = "macos")]
#[test]
fn capture_cli_requires_valid_explicit_selector() {
    let result = std::process::Command::new(env!("CARGO_BIN_EXE_knightbus-macos-exporter"))
        .args([
            "serve",
            "not-a-registry",
            "43249",
            "--capture",
            "--insecure-loopback",
        ])
        .output()
        .unwrap();
    assert!(!result.status.success());
    assert!(String::from_utf8_lossy(&result.stderr).contains("invalid digit"));
}

#[cfg(target_os = "macos")]
#[test]
fn library_capture_obeys_budget_before_usb_access() {
    use knightbus_core::{Budget, DeviceId, DeviceKey, Error, ResourceLimits};
    use knightbus_macos_exporter::{
        descriptors,
        native::{Identity, Native, Snapshot},
    };
    let descriptor = vec![18, 1, 0, 2, 0, 0, 0, 64, 9, 0x12, 1, 0, 0, 1, 0, 0, 0, 1];
    let configurations = vec![vec![
        9, 2, 18, 0, 1, 1, 0, 0x80, 50, 9, 4, 0, 0, 0, 0xff, 0, 0, 0,
    ]];
    let device = descriptors::device(
        DeviceKey {
            id: DeviceId(1),
            generation: 1,
        },
        0x01100000,
        2,
        &descriptor,
        &configurations,
        1,
    )
    .unwrap();
    let snapshot = Snapshot {
        identity: Identity {
            registry_id: 0,
            location: 0,
            vendor: 0,
            product: 0,
            speed: 2,
        },
        device,
        descriptor,
        configurations,
    };
    let budget = Budget::new(ResourceLimits {
        bytes: 0,
        requests: 0,
    });
    assert!(matches!(
        Native::open(snapshot, true, budget.clone()),
        Err(Error::Exhausted)
    ));
    assert_eq!(budget.usage().bytes, 0);
}
