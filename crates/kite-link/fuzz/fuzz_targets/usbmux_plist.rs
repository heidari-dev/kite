#![no_main]

libfuzzer_sys::fuzz_target!(|data: &[u8]| {
    let _ = kite_link::usbmux::parse_devices(data);
    let _ = kite_link::usbmux::parse_result(data);
});
