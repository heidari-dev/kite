#![no_main]

libfuzzer_sys::fuzz_target!(|data: &[u8]| kite_relay::fuzz::socks5_request(data));
