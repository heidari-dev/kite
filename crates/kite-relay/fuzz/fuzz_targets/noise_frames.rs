#![no_main]

libfuzzer_sys::fuzz_target!(|data: &[u8]| kite_relay::fuzz::noise_frames(data));
