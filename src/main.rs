#![windows_subsystem = "windows"]
//! Steam Bypass — compact native Win32/GDI+ tray app for bypassing ISP DNS & SNI blocking.

mod config;
mod gfx;
mod proxy;
mod theme;
mod ui;
mod update;

use std::ptr::null;
use windows_sys::Win32::UI::{
    HiDpi::{DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2, SetProcessDpiAwarenessContext},
    WindowsAndMessaging::{FindWindowW, PostMessageW, SetForegroundWindow},
};

fn main() {
    if std::env::args().any(|a| a == "off") {
        proxy::reset_network_proxy();
        return;
    }
    let start_minimized = std::env::args().any(|a| a == "--minimized");

    unsafe {
        SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
    }

    if !proxy::start_proxy() {
        if !start_minimized {
            unsafe {
                let h = FindWindowW(proxy::wide(ui::WND_CLASS).as_ptr(), null());
                if !h.is_null() {
                    PostMessageW(h, ui::WM_APP_SHOW, 0, 0);
                    SetForegroundWindow(h);
                }
            }
        }
        return;
    }

    config::sync_autostart_path();
    proxy::init_settings();
    proxy::seed_stats();
    proxy::set_enabled(true);
    proxy::start_network_watcher();

    let _gdip = gfx::GdiplusGuard::new();
    ui::run(start_minimized);
    proxy::set_pac(false);
}
