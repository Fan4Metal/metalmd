//! The window caption in the app's theme. egui passes the theme to the
//! window at the end of a frame, and Windows 10 shows a changed caption
//! colour only at the next repaint of the frame (a resize, a change of
//! focus). So the caption is set while the window is still hidden, and
//! repainted after each change of theme. The same as qview's
//! `win::dark_caption` and disk_flashlight's `scan::win::refresh_caption`.

/// The window's handle, if it is a Win32 window.
pub fn hwnd(window: &impl raw_window_handle::HasWindowHandle) -> Option<isize> {
    match window.window_handle().ok()?.as_raw() {
        raw_window_handle::RawWindowHandle::Win32(w) => Some(w.hwnd.get()),
        _ => None,
    }
}

/// Give window `hwnd` the dark or light caption and frame
/// (`DWMWA_USE_IMMERSIVE_DARK_MODE`). Called while the window is hidden, it
/// is shown in these colours from the start.
#[cfg(windows)]
pub fn set_dark(hwnd: isize, dark: bool) {
    // dwmapi is declared by hand, to spare another `windows-sys` feature.
    #[link(name = "dwmapi")]
    unsafe extern "system" {
        fn DwmSetWindowAttribute(
            hwnd: isize,
            attribute: u32,
            value: *const core::ffi::c_void,
            size: u32,
        ) -> i32;
    }
    const DWMWA_USE_IMMERSIVE_DARK_MODE: u32 = 20;
    // The attribute's number before Windows 10 20H1 (1809 to 1909).
    const DWMWA_USE_IMMERSIVE_DARK_MODE_OLD: u32 = 19;
    let value = i32::from(dark);
    let set = |attribute| unsafe {
        DwmSetWindowAttribute(hwnd, attribute, (&raw const value).cast(), 4) >= 0
    };
    if !set(DWMWA_USE_IMMERSIVE_DARK_MODE) {
        set(DWMWA_USE_IMMERSIVE_DARK_MODE_OLD);
    }
}

/// Repaint the caption and frame of window `hwnd` after their colours
/// changed: flipping the caption to the other activation state and back
/// forces the repaint without changing the focus.
#[cfg(windows)]
pub fn refresh(hwnd: isize) {
    #[link(name = "user32")]
    unsafe extern "system" {
        fn SendMessageW(hwnd: isize, msg: u32, wparam: usize, lparam: isize) -> isize;
        fn GetActiveWindow() -> isize;
    }
    const WM_NCACTIVATE: u32 = 0x0086;
    unsafe {
        let active = GetActiveWindow() == hwnd;
        SendMessageW(hwnd, WM_NCACTIVATE, usize::from(!active), 0);
        SendMessageW(hwnd, WM_NCACTIVATE, usize::from(active), 0);
    }
}

/// The visible top-level windows of this thread, for the windows egui
/// makes for child viewports: eframe gives no handle for those. winit's
/// hidden message window is left out.
#[cfg(windows)]
pub fn thread_windows() -> Vec<isize> {
    #[link(name = "user32")]
    unsafe extern "system" {
        fn EnumThreadWindows(
            thread: u32,
            callback: unsafe extern "system" fn(isize, isize) -> i32,
            lparam: isize,
        ) -> i32;
        fn IsWindowVisible(hwnd: isize) -> i32;
    }
    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn GetCurrentThreadId() -> u32;
    }
    unsafe extern "system" fn collect(hwnd: isize, lparam: isize) -> i32 {
        // SAFETY: `lparam` is the pointer to `out` passed below, and the
        // callback runs only during the call.
        let out = unsafe { &mut *(lparam as *mut Vec<isize>) };
        if unsafe { IsWindowVisible(hwnd) } != 0 {
            out.push(hwnd);
        }
        1
    }
    let mut out: Vec<isize> = Vec::new();
    unsafe {
        EnumThreadWindows(GetCurrentThreadId(), collect, (&raw mut out) as isize);
    }
    out
}

#[cfg(not(windows))]
pub fn set_dark(_hwnd: isize, _dark: bool) {}

#[cfg(not(windows))]
pub fn thread_windows() -> Vec<isize> {
    Vec::new()
}

#[cfg(not(windows))]
pub fn refresh(_hwnd: isize) {}
