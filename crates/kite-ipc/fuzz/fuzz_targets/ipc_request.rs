#![no_main]

libfuzzer_sys::fuzz_target!(|data: &[u8]| {
    let _ = kite_ipc::Request::parse(data);
});
