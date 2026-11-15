#![no_main]
use libfuzzer_sys::fuzz_target;
fuzz_target!(|data: &[u8]| {
    if let Ok(request) = knightbus_ipc::decode_request(data) {
        let bytes = knightbus_ipc::encode_request(&request).unwrap();
        assert_eq!(knightbus_ipc::decode_request(&bytes).unwrap(), request);
    }
    if let Ok(response) = knightbus_ipc::decode_response(data) {
        let bytes = knightbus_ipc::encode_response(&response).unwrap();
        assert_eq!(knightbus_ipc::decode_response(&bytes).unwrap(), response);
    }
});
