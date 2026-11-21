//! What the user chose in Windows settings that the desktop app should follow.

use std::ffi::c_void;
use windows_sys::Win32::Foundation::ERROR_SUCCESS;
use windows_sys::Win32::System::Registry::{RegGetValueW, HKEY_CURRENT_USER, RRF_RT_REG_DWORD};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    SystemParametersInfoW, SPI_GETCLIENTAREAANIMATION,
};

/// Whether "Animation effects" is on; `None` when Windows does not say.
pub fn animations() -> Option<bool> {
    let mut on: i32 = 1;
    // SAFETY: for SPI_GETCLIENTAREAANIMATION `pvParam` is a pointer to a BOOL (an i32) that
    // Windows writes; `on` lives through the call and uiParam and fWinIni are 0 for a get.
    let ok = unsafe {
        SystemParametersInfoW(
            SPI_GETCLIENTAREAANIMATION,
            0,
            (&raw mut on).cast::<c_void>(),
            0,
        )
    };
    (ok != 0).then_some(on != 0)
}

/// The "Make text bigger" scale in percent (100 to 225); `None` while the user never set it.
pub fn text_scale_percent() -> Option<u32> {
    let key: Vec<u16> = "Software\\Microsoft\\Accessibility\0"
        .encode_utf16()
        .collect();
    let value: Vec<u16> = "TextScaleFactor\0".encode_utf16().collect();
    let mut data: u32 = 0;
    let mut size = size_of::<u32>() as u32;
    // SAFETY: `key` and `value` are NUL-terminated UTF-16 that outlive the call; `data` is a
    // u32 and `size` says so (4 bytes), and RRF_RT_REG_DWORD makes Windows write at most that.
    let status = unsafe {
        RegGetValueW(
            HKEY_CURRENT_USER,
            key.as_ptr(),
            value.as_ptr(),
            RRF_RT_REG_DWORD,
            std::ptr::null_mut(),
            (&raw mut data).cast::<c_void>(),
            &raw mut size,
        )
    };
    (status == ERROR_SUCCESS).then_some(data)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn windows_answers_both_questions() {
        // The animation switch always has an answer; the text scale only once it was set.
        assert!(animations().is_some());
        assert!(text_scale_percent().is_none_or(|p| (100..=225).contains(&p)));
    }
}
