//! Windows: the build number, the "Transparency effects" switch, the DWM system backdrop and the
//! frame's dark mode (docs/appearance.md "Materials"). CI runs its clippy and tests on Windows;
//! the look is checked with the harness's foreground scenario on a Windows 11 PC.

use gpui::Window;
use windows_sys::Win32::Foundation::{ERROR_SUCCESS, HWND};
use windows_sys::Win32::Graphics::Dwm::{
    DWMSBT_NONE, DWMWA_SYSTEMBACKDROP_TYPE, DWMWA_USE_IMMERSIVE_DARK_MODE, DwmSetWindowAttribute,
};
use windows_sys::Win32::System::Registry::{HKEY_CURRENT_USER, RRF_RT_REG_DWORD, RegGetValueW};
use windows_sys::Win32::System::SystemInformation::OSVERSIONINFOW;

/// The running Windows' build number. `RtlGetVersion` reports the real one, unlike
/// `GetVersionEx` without an application manifest.
pub fn build_number() -> Option<u32> {
    let mut info = OSVERSIONINFOW {
        dwOSVersionInfoSize: size_of::<OSVERSIONINFOW>() as u32,
        dwMajorVersion: 0,
        dwMinorVersion: 0,
        dwBuildNumber: 0,
        dwPlatformId: 0,
        szCSDVersion: [0; 128],
    };
    /* SAFETY: `info` is a valid OSVERSIONINFOW with its size field set, as the call requires. */
    let status = unsafe { windows_sys::Wdk::System::SystemServices::RtlGetVersion(&mut info) };
    (status >= 0).then_some(info.dwBuildNumber)
}

/// Settings › Personalization › Colors › Transparency effects. A missing value means on, as
/// Windows treats it.
pub fn transparency_enabled() -> bool {
    let wide = |text: &str| -> Vec<u16> { text.encode_utf16().chain([0]).collect() };
    let key = wide(r"Software\Microsoft\Windows\CurrentVersion\Themes\Personalize");
    let value = wide("EnableTransparency");
    let mut data: u32 = 1;
    let mut size = size_of::<u32>() as u32;
    /*
     * SAFETY: both strings are NUL-terminated UTF-16 that outlive the call; `data` and `size`
     * describe a 4-byte buffer, which RRF_RT_REG_DWORD fills.
     */
    let status = unsafe {
        RegGetValueW(
            HKEY_CURRENT_USER,
            key.as_ptr(),
            value.as_ptr(),
            RRF_RT_REG_DWORD,
            std::ptr::null_mut(),
            (&mut data as *mut u32).cast(),
            &mut size,
        )
    };
    status != ERROR_SUCCESS || data != 0
}

/// Turns the DWM system backdrop (Mica) off for `window`; nothing where the attribute doesn't
/// exist. gpui's `Opaque` leaves it on.
pub fn clear_system_backdrop(window: &Window) {
    if build_number().is_none_or(|build| build < crate::MICA_BUILD) {
        return;
    }
    let Some(hwnd) = hwnd(window) else {
        return;
    };
    let backdrop = DWMSBT_NONE;
    /*
     * SAFETY: `hwnd` is this live window's handle and the value is the 4-byte
     * DWM_SYSTEMBACKDROP_TYPE the attribute takes.
     */
    let result = unsafe {
        DwmSetWindowAttribute(
            hwnd,
            DWMWA_SYSTEMBACKDROP_TYPE as u32,
            (&backdrop as *const i32).cast(),
            size_of_val(&backdrop) as u32,
        )
    };
    if result < 0 {
        log::warn!("couldn't clear the window's system backdrop: HRESULT {result:#x}");
    }
}

/// `DWMWA_USE_IMMERSIVE_DARK_MODE` for `window`: the frame and the backdrop's tint.
pub fn set_dark_mode(window: &Window, dark: bool) {
    let Some(hwnd) = hwnd(window) else {
        return;
    };
    let value: windows_sys::core::BOOL = dark.into();
    /*
     * SAFETY: `hwnd` is this live window's handle and the value is the 4-byte BOOL the attribute
     * takes.
     */
    let result = unsafe {
        DwmSetWindowAttribute(
            hwnd,
            DWMWA_USE_IMMERSIVE_DARK_MODE as u32,
            (&value as *const i32).cast(),
            size_of_val(&value) as u32,
        )
    };
    if result < 0 {
        log::warn!("couldn't set the window's dark mode: HRESULT {result:#x}");
    }
}

fn hwnd(window: &Window) -> Option<HWND> {
    use raw_window_handle::{HasWindowHandle, RawWindowHandle};

    // Fully qualified: `Window` also has an inherent `window_handle()`, gpui's own handle.
    match HasWindowHandle::window_handle(window).ok()?.as_raw() {
        RawWindowHandle::Win32(handle) => Some(handle.hwnd.get() as HWND),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn the_probe_reads_this_machine() {
        // `RtlGetVersion` answers, with a Windows 10 or later build (10240 is its first).
        let build = super::build_number();
        assert!(build.is_some_and(|build| build >= 10240), "{build:?}");
        // Whatever the switch says, reading it doesn't fail.
        let _ = super::transparency_enabled();
    }
}
