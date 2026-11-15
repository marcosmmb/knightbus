//! Consent and selection inputs must fail closed before opening USB or sockets.
use knightbus_sdk::{
    config::{Configuration, Role},
    core::*,
};
#[test]
fn strict_configuration_requires_explicit_capture_and_separates_pairing_from_grants() {
    let state = std::env::temp_dir();
    let text = format!(
        "role=exporter\nstate={}\nusb=127.0.0.1:3240\ncontrol=127.0.0.1:3241\nregistry=0x12\ncapture=true\n",
        state.display()
    );
    let config = Configuration::parse(&text).unwrap();
    assert!(
        matches!(config.role, Role::Exporter { registry: 18, grants, .. } if grants.is_empty())
    );
    for invalid in [
        text.replace("capture=true", "capture=false"),
        text.replace("capture=true", ""),
        format!("{text}capture=true\n"),
        format!("{text}typo=true\n"),
        text.replace("registry=0x12", "registry=0"),
    ] {
        assert!(Configuration::parse(&invalid).is_err());
    }
    let with_peer = format!("{text}grant={}\n", "01".repeat(32));
    assert!(Configuration::parse(&with_peer).is_ok());
    assert!(Configuration::parse(&format!("{with_peer}grant={}\n", "01".repeat(32))).is_err());
    assert!(matches!(
        Configuration::parse(&"a".repeat(16_385)),
        Err(Error::Exhausted)
    ));
}
#[test]
fn command_parser_keeps_generations_and_epoch_tokens_exact() {
    let args = ["detach", "18446744073709551615", "0x10"].map(str::to_owned);
    assert_eq!(
        knightbus_sdk::cli::command(&args),
        Ok(Command::Detach(SessionId {
            epoch: u64::MAX,
            serial: 16
        }))
    );
    let bad = ["share", "1", "1", "01"].map(str::to_owned);
    assert_eq!(knightbus_sdk::cli::command(&bad), Err(Error::Invalid));
}
