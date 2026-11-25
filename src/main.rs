#![windows_subsystem = "windows"]
//! Steam Bypass — compact native Win32/GDI+ tray app for bypassing ISP DNS & SNI blocking.

mod config;
mod gfx;
mod pac;
mod proxy;
mod steam;
mod theme;
mod ui;
mod update;

// Product version shown in the desktop interface.
pub(crate) const DISPLAY_VERSION: &str = concat!("v", env!("STEAM_BYPASS_DISPLAY_VERSION"));

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
    let settings = config::Settings::load();
    theme::set_light(settings.light);
    proxy::apply_settings(settings);
    let startup_notice = match proxy::recover_network_proxy() {
        Ok(pac::Recovery::Restored) => Some(("Đã khôi phục proxy Windows. Proxy của app đang tắt.", true)),
        Ok(pac::Recovery::ChangedExternally) => Some(("Đã giữ proxy Windows hiện tại. Proxy của app đang tắt.", true)),
        Err(()) => Some(("Không thể khôi phục cấu hình proxy Windows.", false)),
        Ok(pac::Recovery::Clean) => {
            if proxy::set_enabled(true) {
                None
            } else {
                Some(("Không thể bật proxy Windows.", false))
            }
        }
    };
    proxy::start_network_watcher();

    let _gdip = gfx::GdiplusGuard::new();
    ui::run(start_minimized, startup_notice);
    proxy::set_enabled(false);
}
