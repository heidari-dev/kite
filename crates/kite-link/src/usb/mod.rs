//! ADB straight over USB, for computers without an adb server: the protocol in [`wire`], the
//! host's key in `key`, the handshake and the stream in `session`, and the USB device in `device`.

mod device;
mod key;
mod session;
pub mod wire;

pub use device::{devices, driver_refused, open};
