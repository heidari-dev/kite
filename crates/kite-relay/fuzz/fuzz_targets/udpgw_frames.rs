#![no_main]

libfuzzer_sys::fuzz_target!(|data: &[u8]| kite_relay::fuzz::udpgw_frames(data));
