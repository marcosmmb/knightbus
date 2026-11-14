//! Admission guards and the disabled historical hardware batch.
#[cfg(target_os = "macos")]
#[test]
fn capture_cli_requires_valid_explicit_selector() {
    let result = std::process::Command::new(env!("CARGO_BIN_EXE_knightbus-macos-exporter"))
        .args(["qualify", "not-a-location", "1", "--capture"])
        .output()
        .unwrap();
    assert!(!result.status.success());
    assert!(String::from_utf8_lossy(&result.stderr).contains("invalid digit"));
}

#[cfg(unix)]
#[test]
fn hardware_batch_is_disabled_before_privilege_or_argument_processing() {
    let result = std::process::Command::new("bash")
        .arg(concat!(env!("CARGO_MANIFEST_DIR"), "/tests/macos-lab.sh"))
        .output()
        .unwrap();
    assert_eq!(result.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&result.stderr).contains("DISABLED"));
}

#[cfg(target_os = "macos")]
#[test]
fn library_capture_obeys_budget_without_a_lab_recorder() {
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
