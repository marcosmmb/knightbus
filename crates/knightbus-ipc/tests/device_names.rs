//! Friendly names stay bounded and survive the binary management contract.
use knightbus_core::*;
use knightbus_ipc::{decode_response, encode_response};
fn response(name: Option<String>) -> Response {
    Response {
        id: 1,
        result: Ok(Reply::Devices(vec![DeviceInfo {
            shared: false,
            name,
            key: DeviceKey {
                id: DeviceId(1),
                generation: 2,
            },
            busy: false,
            vendor: 0x291a,
            product: 0x3369,
            speed: 3,
            class: 0,
        }])),
    }
}
#[test]
fn names_roundtrip_as_utf8_and_absent_names_remain_optional() {
    for name in [
        None,
        Some("Câmera USB".into()),
        Some("x".repeat(host::MAX_DEVICE_NAME_BYTES)),
    ] {
        let reply = response(name);
        assert_eq!(
            decode_response(&encode_response(&reply).unwrap()).unwrap(),
            reply
        );
    }
    assert_eq!(
        encode_response(&response(Some("x".repeat(host::MAX_DEVICE_NAME_BYTES + 1)))),
        Err(Error::Invalid)
    );
    let mut encoded = encode_response(&response(Some("Camera".into()))).unwrap();
    *encoded.last_mut().unwrap() = 0xff;
    assert_eq!(decode_response(&encoded), Err(Error::Invalid));
}
