//! Persistent configuration in HKCU Registry: Windows autostart, Direct CDN, and custom domains.

use crate::proxy::{DohProvider, is_builtin_domain, parse_endpoint, wide};
use std::{
    collections::HashSet,
    fs::{self, File},
    io::{self, Read, Write},
    path::Path,
    ptr::null_mut,
    time::{SystemTime, UNIX_EPOCH},
};
use windows_sys::Win32::Foundation::{ERROR_FILE_NOT_FOUND, ERROR_PATH_NOT_FOUND};
use windows_sys::Win32::System::Registry::*;

const RUN_KEY: &str = r"Software\Microsoft\Windows\CurrentVersion\Run";
const RUN_VALUE: &str = "SteamBypass";

pub(crate) const APP_KEY: &str = r"Software\SteamBypass";
const SETTINGS_VALUE: &str = "PortableSettings";
const BACKUP_HEADER: &str = "Steam Bypass Settings v1";
const BACKUP_LIMIT: usize = 1024 * 1024;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CustomDomain {
    pub domain: String,
    pub enabled: bool,
}

fn read_reg_sz(subkey: &str, value_name: &str) -> Option<String> {
    let (kind, data) = read_reg_string(subkey, value_name).ok()??;
    (kind == REG_SZ).then_some(data)
}

pub(crate) fn read_reg_string(subkey: &str, value_name: &str) -> Result<Option<(u32, String)>, ()> {
    let key_w = wide(subkey);
    let name_w = wide(value_name);
    unsafe {
        let mut hkey: HKEY = null_mut();
        let rc = RegOpenKeyExW(HKEY_CURRENT_USER, key_w.as_ptr(), 0, KEY_QUERY_VALUE, &mut hkey);
        if rc == ERROR_FILE_NOT_FOUND || rc == ERROR_PATH_NOT_FOUND {
            return Ok(None);
        }
        if rc != 0 {
            return Err(());
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
        if rc != 0 || !matches!(reg_type, REG_SZ | REG_EXPAND_SZ) {
            RegCloseKey(hkey);
            return if rc == ERROR_FILE_NOT_FOUND { Ok(None) } else { Err(()) };
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
        if rc2 != 0 || !matches!(reg_type, REG_SZ | REG_EXPAND_SZ) {
            return Err(());
        }
        let len = (actual as usize / 2).min(buf.len());
        let slice = &buf[..len];
        let trimmed = match slice.iter().position(|&c| c == 0) {
            Some(pos) => &slice[..pos],
            None => slice,
        };
        Ok(Some((reg_type, String::from_utf16(trimmed).map_err(|_| ())?)))
    }
}

fn write_reg_sz(subkey: &str, value_name: &str, value: &str) -> bool {
    write_reg_string(subkey, value_name, REG_SZ, value)
}

pub(crate) fn write_reg_string(subkey: &str, value_name: &str, kind: u32, value: &str) -> bool {
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
            kind,
            val_w.as_ptr().cast(),
            (val_w.len() * 2) as u32,
        ) == 0;
        RegCloseKey(hkey);
        ok
    }
}

pub(crate) fn delete_reg_value(subkey: &str, value_name: &str) -> bool {
    let key_w = wide(subkey);
    let name_w = wide(value_name);
    unsafe {
        let mut hkey: HKEY = null_mut();
        let rc = RegOpenKeyExW(HKEY_CURRENT_USER, key_w.as_ptr(), 0, KEY_SET_VALUE, &mut hkey);
        if rc == ERROR_FILE_NOT_FOUND || rc == ERROR_PATH_NOT_FOUND {
            return true;
        }
        if rc != 0 {
            return false;
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

#[derive(Debug, PartialEq, Eq)]
pub struct Settings {
    pub domains: Vec<CustomDomain>,
    pub provider: DohProvider,
    pub url: String,
    pub direct_cdn: bool,
    pub light: bool,
}

impl Settings {
    pub fn load() -> Self {
        if let Some(text) = read_reg_sz(APP_KEY, SETTINGS_VALUE) {
            if let Ok(settings) = Self::decode(&text) {
                return settings;
            }
        }
        let id = read_reg_sz(APP_KEY, "DohProvider").unwrap_or_default();
        let mut provider = DohProvider::from_id(&id);
        let url = read_reg_sz(APP_KEY, "CustomDohUrl")
            .or_else(|| (id.trim().eq_ignore_ascii_case("dnssb")).then(|| "https://doh.dns.sb/dns-query".into()))
            .filter(|url| parse_endpoint(url).is_some())
            .unwrap_or_default();
        if provider == DohProvider::Custom && url.is_empty() {
            provider = DohProvider::Cloudflare;
        }
        Self {
            domains: parse_stored_domains(&read_reg_sz(APP_KEY, "CustomDomains").unwrap_or_default())
                .into_iter()
                .filter(|item| !is_builtin_domain(&item.domain))
                .collect(),
            provider,
            url,
            direct_cdn: read_reg_sz(APP_KEY, "DirectCdn").is_some_and(|value| value.trim() == "1"),
            light: read_reg_sz(APP_KEY, "Theme").as_deref() == Some("light"),
        }
    }

    pub fn encode(&self) -> String {
        format!(
            "{BACKUP_HEADER}\nDoH={}\nURL={}\nDirectCDN={}\nTheme={}\n[Domains]\n{}",
            self.provider.id(),
            self.url,
            u8::from(self.direct_cdn),
            if self.light { "light" } else { "dark" },
            serialize_stored_domains(&self.domains),
        )
    }

    pub fn decode(text: &str) -> Result<Self, &'static str> {
        if text.len() > BACKUP_LIMIT {
            return Err("Tệp sao lưu vượt quá 1 MB.");
        }
        let mut lines = text.trim_start_matches('\u{feff}').lines();
        if lines.next() != Some(BACKUP_HEADER) {
            return Err("Tệp không phải bản sao lưu Steam Bypass được hỗ trợ.");
        }
        let invalid = "Tệp sao lưu có cấu hình không hợp lệ.";
        let id = lines.next().and_then(|s| s.strip_prefix("DoH=")).ok_or(invalid)?;
        let provider = DohProvider::ALL.into_iter().find(|p| p.id() == id).ok_or(invalid)?;
        let url = lines.next().and_then(|s| s.strip_prefix("URL=")).ok_or(invalid)?;
        if (provider == DohProvider::Custom || !url.is_empty()) && parse_endpoint(url).is_none() {
            return Err("Địa chỉ Custom DoH trong bản sao lưu không hợp lệ.");
        }
        let direct_cdn = match lines.next() {
            Some("DirectCDN=1") => true,
            Some("DirectCDN=0") => false,
            _ => return Err(invalid),
        };
        let light = match lines.next() {
            Some("Theme=light") => true,
            Some("Theme=dark") => false,
            _ => return Err(invalid),
        };
        if lines.next() != Some("[Domains]") {
            return Err(invalid);
        }
        let mut seen = HashSet::new();
        let mut domains = Vec::new();
        for line in lines {
            let (domain, enabled) = line.strip_prefix('!').map_or((line, true), |host| (host, false));
            if validate_domain(domain).is_err()
                || is_builtin_domain(domain)
                || !seen.insert(domain)
            {
                return Err("Danh sách trong bản sao lưu chứa tên miền không hợp lệ hoặc trùng lặp.");
            }
            domains.push(CustomDomain { domain: domain.into(), enabled });
        }
        Ok(Self { domains, provider, url: url.into(), direct_cdn, light })
    }

    pub fn save(&self) -> bool {
        let text = self.encode();
        text.len() <= BACKUP_LIMIT && write_reg_sz(APP_KEY, SETTINGS_VALUE, &text)
    }

    pub fn read_backup(path: &Path) -> Result<Self, &'static str> {
        let mut bytes = Vec::new();
        File::open(path)
            .and_then(|file| file.take((BACKUP_LIMIT + 1) as u64).read_to_end(&mut bytes))
            .map_err(|_| "Không thể đọc tệp sao lưu.")?;
        let text = std::str::from_utf8(&bytes).map_err(|_| "Tệp sao lưu phải dùng mã hóa UTF-8.")?;
        Self::decode(text)
    }

    pub fn write_backup(&self, path: &Path) -> io::Result<()> {
        let text = self.encode();
        if text.len() > BACKUP_LIMIT {
            return Err(io::Error::new(io::ErrorKind::InvalidData, "Backup is too large"));
        }
        let stamp = SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_nanos();
        let temporary = path.with_file_name(format!(".steam-bypass-{}-{stamp}.tmp", std::process::id()));
        let mut file = File::create_new(&temporary)?;
        let result = file.write_all(text.as_bytes()).and_then(|_| file.sync_all());
        drop(file);

        // Keep the previous backup intact if writing or replacing the file fails.
        let result = result.and_then(|_| fs::rename(&temporary, path));
        if result.is_err() {
            let _ = fs::remove_file(&temporary);
        }
        result
    }
}

fn change_settings(change: impl FnOnce(&mut Settings)) -> bool {
    let mut settings = Settings::load();
    change(&mut settings);
    settings.save()
}

pub fn save_direct_cdn(direct: bool) -> bool {
    change_settings(|s| s.direct_cdn = direct)
}

pub fn save_doh_provider(provider: DohProvider) -> bool {
    change_settings(|s| s.provider = provider)
}

// ---------------- Custom Domains ----------------
/// Split a user-pasted string containing one or multiple domains separated by newlines, commas, semicolons, or spaces.
pub fn split_domain_tokens(raw: &str) -> Vec<&str> {
    raw.split(|c: char| c == ',' || c == ';' || c.is_whitespace())
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .collect()
}

/// Normalize user input (bare domain, wildcard, or pasted URL) into a clean lowercase domain.
pub fn normalize_domain(raw: &str) -> Result<String, &'static str> {
    let mut s = raw.trim().to_ascii_lowercase();
    if s.is_empty() {
        return Err("Hãy nhập tên miền (vd: bbc.com)");
    }
    if let Some(rest) = s.strip_prefix("https://") {
        s = rest.to_string();
    } else if let Some(rest) = s.strip_prefix("http://") {
        s = rest.to_string();
    }
    if let Some(idx) = s.find(|c: char| c == '/' || c == '?' || c == '#' || c.is_whitespace()) {
        s.truncate(idx);
    }
    if let Some((_, host)) = s.rsplit_once('@') {
        s = host.to_string();
    }
    if let Some((host, port)) = s.rsplit_once(':') {
        if !port.is_empty() && port.bytes().all(|b| b.is_ascii_digit()) {
            s = host.to_string();
        }
    }
    while let Some(rest) = s.strip_prefix("*.") {
        s = rest.to_string();
    }
    let s = s.trim_matches('.');
    let s = match s.strip_prefix("www.") {
        Some(rest) if rest.contains('.') => rest,
        _ => s,
    };

    validate_domain(s)?;
    Ok(s.to_string())
}

fn validate_domain(s: &str) -> Result<(), &'static str> {
    if s.len() < 3 || s.len() > 253 || !s.contains('.') {
        return Err("Tên miền không hợp lệ (vd: bbc.com)");
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
    Ok(())
}

pub(crate) fn parse_stored_domains(raw: &str) -> Vec<CustomDomain> {
    let mut out: Vec<CustomDomain> = Vec::new();
    for line in raw.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        let (enabled, rest) = match trimmed.strip_prefix('!') {
            Some(r) => (false, r),
            None => (true, trimmed),
        };
        if let Ok(d) = normalize_domain(rest) {
            if !out.iter().any(|x| x.domain == d) {
                out.push(CustomDomain { domain: d, enabled });
            }
        }
    }
    out
}

pub(crate) fn serialize_stored_domains(domains: &[CustomDomain]) -> String {
    domains
        .iter()
        .map(|d| {
            if d.enabled {
                d.domain.clone()
            } else {
                format!("!{}", d.domain)
            }
        })
        .collect::<Vec<_>>()
        .join("\n")
}

pub fn save_custom_domains(domains: &[CustomDomain]) -> bool {
    change_settings(|s| s.domains = domains.to_vec())
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

    #[test]
    fn splits_and_serializes_multi_domains() {
        let tokens = split_domain_tokens("bbc.com, https://medium.com/a;\nimgur.com  pixiv.net");
        assert_eq!(tokens, vec!["bbc.com", "https://medium.com/a", "imgur.com", "pixiv.net"]);

        let list = vec![
            CustomDomain { domain: "bbc.com".into(), enabled: true },
            CustomDomain { domain: "medium.com".into(), enabled: false },
        ];
        let raw = serialize_stored_domains(&list);
        assert_eq!(raw, "bbc.com\n!medium.com");
        assert_eq!(parse_stored_domains(&raw), list);
    }

    #[test]
    fn backup_round_trip_and_validation() {
        let mut settings = Settings {
            domains: vec![
                CustomDomain { domain: "example.test".into(), enabled: true },
                CustomDomain { domain: "news.example.test".into(), enabled: false },
            ],
            provider: DohProvider::Custom,
            url: "https://resolver.test/dns-query?profile=private".into(),
            direct_cdn: true,
            light: true,
        };
        let data = settings.encode();
        assert_eq!(Settings::decode(&data), Ok(settings));
        assert!(Settings::decode(&format!("{data}\nnews.example.test")).is_err());
        assert!(Settings::decode(&data.replace("https://", "http://")).is_err());
        assert!(Settings::decode(&data.replace("v1", "v2")).is_err());
        assert!(Settings::decode(&data.replace("example.test", "steampowered.com")).is_err());
        settings = Settings::decode(&data).unwrap();

        let folder = Path::new(env!("CARGO_MANIFEST_DIR")).join("target");
        fs::create_dir_all(&folder).unwrap();
        let path = folder.join(format!("backup-test-{}-cấu hình.sbconf", std::process::id()));
        struct BackupFile(std::path::PathBuf);
        impl Drop for BackupFile {
            fn drop(&mut self) { let _ = fs::remove_file(&self.0); }
        }
        let file = BackupFile(path);
        settings.write_backup(&file.0).unwrap();
        assert_eq!(&Settings::read_backup(&file.0).unwrap(), &settings);

        settings.domains.clear();
        settings.write_backup(&file.0).unwrap();
        assert_eq!(&Settings::read_backup(&file.0).unwrap(), &settings);
        assert_eq!(Settings::decode(&settings.encode()), Ok(settings));
    }
}

pub fn save_light_theme(light: bool) -> bool {
    change_settings(|s| s.light = light)
}

pub fn save_doh_url(url: &str) -> bool {
    change_settings(|s| s.url = url.into())
}
