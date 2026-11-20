use crate::proxy::wide;
use std::{ffi::c_void, ptr::{null, null_mut}};
use windows_sys::Win32::Networking::WinInet::*;

const LATEST: &str = "https://github.com/mohoiroi/Bypass-Steam/releases/latest";
const TAG_BASE: &str = "https://github.com/mohoiroi/Bypass-Steam/releases/tag/";
const CURRENT: &str = env!("CARGO_PKG_VERSION");

struct InternetHandle(*mut c_void);

impl InternetHandle {
    fn new(raw: *mut c_void) -> Option<Self> {
        (!raw.is_null()).then_some(Self(raw))
    }
}

impl Drop for InternetHandle {
    fn drop(&mut self) {
        unsafe { InternetCloseHandle(self.0); }
    }
}

fn version(value: &str) -> Option<[u64; 3]> {
    let mut parts = value.strip_prefix('v').unwrap_or(value).split('.');
    let mut result = [0; 3];
    for number in &mut result {
        let part = parts.next()?;
        if !part.bytes().all(|c| c.is_ascii_digit()) { return None; }
        *number = part.parse().ok()?;
    }
    parts.next().is_none().then_some(result)
}

fn report(location: &str) -> Option<String> {
    let tag = location.strip_prefix(TAG_BASE)
        .or_else(|| location.strip_prefix("/mohoiroi/Bypass-Steam/releases/tag/"))?;
    let latest = version(tag)?;
    let current = version(CURRENT)?;
    Some(if latest > current {
        format!("Đã có bản {tag}. Phiên bản đang dùng là {CURRENT}.")
    } else {
        format!("Bạn đang dùng phiên bản {CURRENT}. Chưa có bản phát hành mới hơn.")
    })
}

pub fn check() -> Option<String> {
    unsafe {
        let session = InternetHandle::new(InternetOpenW(wide("Steam Bypass").as_ptr(),
            INTERNET_OPEN_TYPE_PRECONFIG, null(), null(), 0))?;
        let timeout = 8000u32;
        for option in [INTERNET_OPTION_CONNECT_TIMEOUT, INTERNET_OPTION_RECEIVE_TIMEOUT, INTERNET_OPTION_SEND_TIMEOUT] {
            InternetSetOptionW(session.0, option, (&timeout as *const u32).cast(), 4);
        }
        let flags = INTERNET_FLAG_NO_AUTO_REDIRECT | INTERNET_FLAG_RELOAD | INTERNET_FLAG_NO_CACHE_WRITE
            | INTERNET_FLAG_NO_COOKIES | INTERNET_FLAG_NO_UI;
        let req = InternetHandle::new(InternetOpenUrlW(session.0, wide(LATEST).as_ptr(), null(), 0, flags, 0))?;
        let mut location = [0u16; 1024];
        let mut bytes = std::mem::size_of_val(&location) as u32;
        if HttpQueryInfoW(req.0, HTTP_QUERY_LOCATION, location.as_mut_ptr().cast(), &mut bytes, null_mut()) == 0 {
            return None;
        }
        report(String::from_utf16(location.get(..bytes as usize / 2)?).ok()?.trim_end_matches('\0'))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    #[ignore = "requires access to GitHub Releases"]
    fn checks_published_release() {
        println!("{}", check().expect("GitHub release redirect"));
    }

    #[test]
    fn compares_numeric_release_tags() {
        assert!(version("v1.10.0") > version("1.9.9"));
        assert!(report(&format!("{TAG_BASE}v{CURRENT}")).unwrap().contains("Chưa có"));
        assert!(report(&format!("{TAG_BASE}v999.0.0")).unwrap().contains("Đã có"));
        assert!(report("https://example.org/v999.0.0").is_none());
        assert!(version("v1.4.1-beta").is_none());
    }
}
