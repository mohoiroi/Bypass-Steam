//! Persistent configuration in HKCU Registry: Windows autostart and custom bypass domains.

use crate::proxy::wide;
use std::ptr::null_mut;
use windows_sys::Win32::System::Registry::*;

const RUN_KEY: &str = r"Software\Microsoft\Windows\CurrentVersion\Run";
const RUN_VALUE: &str = "SteamBypass";

const APP_KEY: &str = r"Software\SteamBypass";
const CUSTOM_DOMAINS_VALUE: &str = "CustomDomains";

fn read_reg_sz(subkey: &str, value_name: &str) -> Option<String> {
    let key_w = wide(subkey);
    let name_w = wide(value_name);
    unsafe {
        let mut hkey: HKEY = null_mut();
        if RegOpenKeyExW(HKEY_CURRENT_USER, key_w.as_ptr(), 0, KEY_READ, &mut hkey) != 0
            || hkey.is_null()
        {
            return None;
        }
        let mut reg_type: u32 = 0;
        let mut size_bytes: u32 = 0;
        let rc = RegQueryValueExW(
            hkey,
            name_w.as_ptr(),
            null_mut(),
            &mut reg_type,
            null_mut(),
            &mut size_bytes,
        );
        if rc != 0 || reg_type != REG_SZ || size_bytes < 2 {
            RegCloseKey(hkey);
            return None;
        }
        let mut buf = vec![0u16; (size_bytes as usize + 1) / 2];
        let mut actual = (buf.len() * 2) as u32;
        let rc2 = RegQueryValueExW(
            hkey,
            name_w.as_ptr(),
            null_mut(),
            &mut reg_type,
            buf.as_mut_ptr().cast(),
            &mut actual,
        );
        RegCloseKey(hkey);
        if rc2 != 0 {
            return None;
        }
        let len = (actual as usize / 2).min(buf.len());
        let slice = &buf[..len];
        let trimmed = match slice.iter().position(|&c| c == 0) {
            Some(pos) => &slice[..pos],
            None => slice,
        };
        Some(String::from_utf16_lossy(trimmed))
    }
}

fn write_reg_sz(subkey: &str, value_name: &str, value: &str) -> bool {
    let key_w = wide(subkey);
    let name_w = wide(value_name);
    let val_w = wide(value);
    unsafe {
        let mut hkey: HKEY = null_mut();
        let rc = RegCreateKeyW(HKEY_CURRENT_USER, key_w.as_ptr(), &mut hkey);
        if rc != 0 || hkey.is_null() {
            return false;
        }
        let ok = RegSetValueExW(
            hkey,
            name_w.as_ptr(),
            0,
            REG_SZ,
            val_w.as_ptr().cast(),
            (val_w.len() * 2) as u32,
        ) == 0;
        RegCloseKey(hkey);
        ok
    }
}

fn delete_reg_value(subkey: &str, value_name: &str) -> bool {
    let key_w = wide(subkey);
    let name_w = wide(value_name);
    unsafe {
        let mut hkey: HKEY = null_mut();
        if RegOpenKeyExW(HKEY_CURRENT_USER, key_w.as_ptr(), 0, KEY_SET_VALUE, &mut hkey) != 0
            || hkey.is_null()
        {
            return true;
        }
        let rc = RegDeleteValueW(hkey, name_w.as_ptr());
        RegCloseKey(hkey);
        rc == 0 || rc == 2 // ERROR_FILE_NOT_FOUND
    }
}

// ---------------- Windows Autostart ----------------
pub fn is_autostart_enabled() -> bool {
    read_reg_sz(RUN_KEY, RUN_VALUE)
        .map(|v| !v.trim().is_empty())
        .unwrap_or(false)
}

pub fn set_autostart(enable: bool) -> bool {
    if !enable {
        return delete_reg_value(RUN_KEY, RUN_VALUE);
    }
    let Ok(exe) = std::env::current_exe() else {
        return false;
    };
    let cmd = format!("\"{}\" --minimized", exe.display());
    write_reg_sz(RUN_KEY, RUN_VALUE, &cmd)
}

/// If autostart is already enabled, keep the registered path in sync if the user moved the `.exe`.
pub fn sync_autostart_path() {
    if is_autostart_enabled() {
        let _ = set_autostart(true);
    }
}

// ---------------- Custom Domains ----------------
/// Normalize user input (bare domain, wildcard, or pasted URL) into a clean lowercase domain.
pub fn normalize_domain(raw: &str) -> Result<String, &'static str> {
    let mut s = raw.trim().to_ascii_lowercase();
    if s.is_empty() {
        return Err("Hãy nhập tên miền (vd: reddit.com)");
    }
    if let Some(rest) = s.strip_prefix("https://") {
        s = rest.to_string();
    } else if let Some(rest) = s.strip_prefix("http://") {
        s = rest.to_string();
    }
    // Strip path, query, fragment, whitespace
    if let Some(idx) = s.find(|c: char| c == '/' || c == '?' || c == '#' || c.is_whitespace()) {
        s.truncate(idx);
    }
    // Strip optional userinfo@
    if let Some((_, host)) = s.rsplit_once('@') {
        s = host.to_string();
    }
    // Strip optional :port
    if let Some((host, port)) = s.rsplit_once(':') {
        if !port.is_empty() && port.bytes().all(|b| b.is_ascii_digit()) {
            s = host.to_string();
        }
    }
    // Strip leading wildcards / dots and trailing dots
    while let Some(rest) = s.strip_prefix("*.") {
        s = rest.to_string();
    }
    let s = s.trim_matches('.');
    // Strip leading "www." if at least two labels remain (e.g. www.reddit.com -> reddit.com)
    let s = match s.strip_prefix("www.") {
        Some(rest) if rest.contains('.') => rest,
        _ => s,
    };

    if s.len() < 3 || s.len() > 253 || !s.contains('.') {
        return Err("Tên miền không hợp lệ (vd: reddit.com)");
    }
    let labels: Vec<&str> = s.split('.').collect();
    if labels.len() < 2 {
        return Err("Tên miền không hợp lệ");
    }
    for label in &labels {
        if label.is_empty()
            || label.len() > 63
            || label.starts_with('-')
            || label.ends_with('-')
            || !label
                .bytes()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
        {
            return Err("Tên miền chứa ký tự không hợp lệ");
        }
    }
    let tld = labels.last().copied().unwrap_or("");
    if tld.len() < 2 || !tld.bytes().any(|b| b.is_ascii_alphabetic()) {
        return Err("Đuôi tên miền không hợp lệ");
    }
    Ok(s.to_string())
}

pub fn load_custom_domains() -> Vec<String> {
    let Some(raw) = read_reg_sz(APP_KEY, CUSTOM_DOMAINS_VALUE) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for line in raw.lines() {
        if let Ok(d) = normalize_domain(line) {
            if !out.contains(&d) {
                out.push(d);
            }
        }
    }
    out
}

pub fn save_custom_domains(domains: &[String]) -> bool {
    let joined = domains.join("\n");
    write_reg_sz(APP_KEY, CUSTOM_DOMAINS_VALUE, &joined)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalizes_various_inputs() {
        assert_eq!(normalize_domain("reddit.com").unwrap(), "reddit.com");
        assert_eq!(normalize_domain("  HTTPS://WWW.Reddit.com/r/rust?a=1 ").unwrap(), "reddit.com");
        assert_eq!(normalize_domain("*.medium.com").unwrap(), "medium.com");
        assert_eq!(normalize_domain("http://bbc.co.uk:8080/news").unwrap(), "bbc.co.uk");
        assert_eq!(normalize_domain("x.com").unwrap(), "x.com");
        assert_eq!(normalize_domain("sub.example-site.org.").unwrap(), "sub.example-site.org");
    }

    #[test]
    fn rejects_invalid_domains() {
        assert!(normalize_domain("").is_err());
        assert!(normalize_domain("localhost").is_err());
        assert!(normalize_domain("1.1.1.1").is_err());
        assert!(normalize_domain("-bad.com").is_err());
        assert!(normalize_domain("bad..com").is_err());
        assert!(normalize_domain("bad host.com").is_err());
    }
}
