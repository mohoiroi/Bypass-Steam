#![windows_subsystem = "windows"]
//! Steam Bypass — compact native Win32/GDI+ tray app for bypassing ISP DNS & SNI blocking.

mod gfx;
mod proxy;
mod theme;
mod ui;

use std::ptr::null;
use windows_sys::Win32::UI::{
    HiDpi::{DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2, SetProcessDpiAwarenessContext},
    WindowsAndMessaging::{FindWindowW, PostMessageW, SetForegroundWindow},
};

fn main() {
    if std::env::args().nth(1).as_deref() == Some("off") {
        proxy::set_pac(false);
        return;
    }

    unsafe {
        SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
    }

    if !proxy::start_proxy() {
        unsafe {
            let h = FindWindowW(proxy::wide(ui::WND_CLASS).as_ptr(), null());
            if !h.is_null() {
                PostMessageW(h, ui::WM_APP_SHOW, 0, 0);
                SetForegroundWindow(h);
            }
        }
        return;
    }

    proxy::seed_stats();
    proxy::set_enabled(true);

    let _gdip = gfx::GdiplusGuard::new();
    ui::run();
    proxy::set_pac(false);
}
