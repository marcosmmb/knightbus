#![no_main]
#[path = "../support.rs"]
#[allow(dead_code)]
mod support;
libfuzzer_sys::fuzz_target!(|data: &[u8]| support::decode_target(data));
