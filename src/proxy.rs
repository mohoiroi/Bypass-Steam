//! Local HTTP/HTTPS proxy with DNS-over-HTTPS (WinHTTP) and TLS ClientHello splitting.

use crate::config::CustomDomain;
use std::{
    collections::{BTreeMap, HashMap},
    io::{Read, Write},
    net::{IpAddr, Ipv4Addr, Shutdown, SocketAddr, TcpListener, TcpStream, ToSocketAddrs},
    sync::{
        Mutex, OnceLock,
        atomic::{AtomicBool, AtomicIsize, AtomicUsize, Ordering},
    },
    thread,
    time::{Duration, Instant},
};
use windows_sys::Win32::{
    Foundation::HWND,
    Networking::{WinHttp::*, WinInet::*},
    System::Registry::*,
    UI::WindowsAndMessaging::{PostMessageW, WM_APP},
};

#[link(name = "iphlpapi")]
unsafe extern "system" {
    fn NotifyAddrChange(handle: *mut core::ffi::c_void, overlapped: *mut core::ffi::c_void) -> u32;
}

pub const APP_TITLE: &str = "Steam Bypass";
pub const PORT: u16 = 8899;
pub const WM_APP_STATS: u32 = WM_APP + 2;
pub const WM_APP_NET_CHANGED: u32 = WM_APP + 5;

const IE_KEY: &str = r"Software\Microsoft\Windows\CurrentVersion\Internet Settings";

const DOMAINS: &[&str] = &[
    "steampowered.com",
    "steamcommunity.com",
    "steamstatic.com",
    "steamcontent.com",
    "steamserver.net",
    "steamusercontent.com",
    "akamaihd.net",
    "valvesoftware.com",
    "steamgames.com",
    "steam-api.com",
    "steam.tv",
];

/// Game download CDN domains skipped when `DIRECT_CDN` is enabled so Steam uses ISP local caches.
const CDN_DOMAINS: &[&str] = &["steamcontent.com", "akamaihd.net"];

pub const PRESET_DOMAINS: &[&str] = &[
    "bbc.com",
    "medium.com",
    "reddit.com",
    "imgur.com",
    "pixiv.net",
];

pub const CDN_GROUP_IDX: usize = 3;
pub const CUSTOM_GROUP_IDX: usize = 6;

pub const GROUPS: [(&str, &str); 7] = [
    ("Cửa hàng", "Store, giỏ hàng, khuyến mãi"),
    ("Cộng đồng", "Hồ sơ, bạn bè, workshop"),
    ("Đăng nhập & API", "Tài khoản, Steam Guard, API"),
    ("Tải game (CDN)", "Máy chủ tải game & cập nhật"),
    ("Hình ảnh & media", "Ảnh, avatar, video"),
    ("Khác (Steam)", "Tên miền Valve khác"),
    ("Web tùy chỉnh", "Các trang web bạn thêm"),
];

pub const CATALOG: &[&str] = &[
    "store.steampowered.com",
    "steamcommunity.com",
    "login.steampowered.com",
    "api.steampowered.com",
    "help.steampowered.com",
    "steamcommunity-a.akamaihd.net",
    "steamcdn-a.akamaihd.net",
    "cdn.cloudflare.steamstatic.com",
    "cdn.akamai.steamstatic.com",
    "avatars.akamai.steamstatic.com",
];

pub fn domain_matches(host: &str, domain: &str) -> bool {
    host == domain
        || (host.len() > domain.len()
            && host.ends_with(domain)
            && host.as_bytes()[host.len() - domain.len() - 1] == b'.')
}

pub fn is_builtin_domain(host: &str) -> bool {
    let h = host.to_ascii_lowercase();
    DOMAINS.iter().any(|d| domain_matches(&h, d))
}

fn group_of(host: &str) -> usize {
    let h = host.to_ascii_lowercase();
    if domain_matches(&h, "store.steampowered.com") {
        0
    } else if domain_matches(&h, "steamcommunity.com") {
        1
    } else if domain_matches(&h, "steampowered.com") || domain_matches(&h, "steam-api.com") {
        2
    } else if domain_matches(&h, "steamcontent.com")
        || domain_matches(&h, "akamaihd.net")
        || domain_matches(&h, "steamserver.net")
    {
        3
    } else if domain_matches(&h, "steamstatic.com")
        || domain_matches(&h, "steamusercontent.com")
        || domain_matches(&h, "steam.tv")
    {
        4
    } else if domain_matches(&h, "valvesoftware.com") || domain_matches(&h, "steamgames.com") {
        5
    } else {
        CUSTOM_GROUP_IDX
    }
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub enum DnsState {
    #[default]
    Unknown,
    Clean,
    Poisoned,
    Failed,
}

#[derive(Default, Clone)]
pub struct Stat {
    pub group: usize,
    pub ok: u32,
    pub fail: u32,
    pub ip: String,
    pub dns: DnsState,
    pub last_ok: bool,
}

pub static STATS: Mutex<BTreeMap<String, Stat>> = Mutex::new(BTreeMap::new());
pub static CUSTOM_DOMAINS: Mutex<Vec<CustomDomain>> = Mutex::new(Vec::new());
pub static ENABLED: AtomicBool = AtomicBool::new(true);
pub static DIRECT_CDN: AtomicBool = AtomicBool::new(false);
pub static PENDING: AtomicUsize = AtomicUsize::new(0);

static PAC_VERSION: AtomicUsize = AtomicUsize::new(1);
static UI_HWND: AtomicIsize = AtomicIsize::new(0);
pub static UI_DIRTY: AtomicBool = AtomicBool::new(false);

type DnsCache = HashMap<String, (Vec<IpAddr>, Instant)>;
static CACHE: Mutex<Option<DnsCache>> = Mutex::new(None);
static WINHTTP_SESSION: OnceLock<usize> = OnceLock::new();

pub fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(Some(0)).collect()
}

pub fn set_ui_hwnd(hwnd: HWND) {
    UI_HWND.store(hwnd as isize, Ordering::SeqCst);
}

pub fn notify_ui() {
    let h = UI_HWND.load(Ordering::SeqCst);
    if h != 0 && !UI_DIRTY.swap(true, Ordering::AcqRel) {
        unsafe {
            PostMessageW(h as HWND, WM_APP_STATS, 0, 0);
        }
    }
}

pub fn clear_dns_cache() {
    if let Some(map) = CACHE.lock().unwrap_or_else(|e| e.into_inner()).as_mut() {
        map.clear();
    }
}

pub fn init_settings() {
    DIRECT_CDN.store(crate::config::load_direct_cdn(), Ordering::SeqCst);
    let loaded = crate::config::load_custom_domains();
    let mut list = CUSTOM_DOMAINS.lock().unwrap_or_else(|e| e.into_inner());
    *list = loaded;
}

pub fn set_direct_cdn(direct: bool) {
    DIRECT_CDN.store(direct, Ordering::SeqCst);
    let _ = crate::config::save_direct_cdn(direct);
    if ENABLED.load(Ordering::SeqCst) {
        set_pac(true);
    }
    notify_ui();
}

pub fn custom_domains() -> Vec<CustomDomain> {
    CUSTOM_DOMAINS
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .clone()
}

#[derive(Default)]
pub struct BatchAddResult {
    pub added: Vec<String>,
    pub skipped_dup: usize,
    pub skipped_builtin: usize,
    pub invalid: usize,
    pub first_err: Option<&'static str>,
}

pub fn add_custom_domains(raw: &str) -> BatchAddResult {
    let tokens = crate::config::split_domain_tokens(raw);
    let mut res = BatchAddResult::default();
    if tokens.is_empty() {
        res.invalid = 1;
        res.first_err = Some("Hãy nhập tên miền (vd: bbc.com, medium.com)");
        return res;
    }

    let mut newly_added: Vec<String> = Vec::new();
    {
        let mut list = CUSTOM_DOMAINS.lock().unwrap_or_else(|e| e.into_inner());
        for tok in tokens {
            match crate::config::normalize_domain(tok) {
                Ok(domain) => {
                    if is_builtin_domain(&domain) {
                        res.skipped_builtin += 1;
                        res.first_err.get_or_insert("Tên miền Steam đã được hỗ trợ sẵn");
                        continue;
                    }
                    if list.iter().any(|d| d.domain == domain) {
                        res.skipped_dup += 1;
                        res.first_err.get_or_insert("Tên miền này đã có trong danh sách");
                        continue;
                    }
                    list.push(CustomDomain {
                        domain: domain.clone(),
                        enabled: true,
                    });
                    newly_added.push(domain);
                }
                Err(e) => {
                    res.invalid += 1;
                    res.first_err.get_or_insert(e);
                }
            }
        }
        if !newly_added.is_empty() {
            let _ = crate::config::save_custom_domains(&list);
        }
    }

    if !newly_added.is_empty() {
        {
            let mut m = STATS.lock().unwrap_or_else(|e| e.into_inner());
            for d in &newly_added {
                m.entry(d.clone()).or_insert(Stat {
                    group: CUSTOM_GROUP_IDX,
                    ..Default::default()
                });
            }
        }
        if ENABLED.load(Ordering::SeqCst) {
            set_pac(true);
        }
        for d in &newly_added {
            test_single_host(d.clone());
        }
        notify_ui();
    }
    res.added = newly_added;
    res
}

pub fn toggle_custom_domain(domain: &str) -> Option<bool> {
    let target = domain.to_ascii_lowercase();
    let new_state;
    {
        let mut list = CUSTOM_DOMAINS.lock().unwrap_or_else(|e| e.into_inner());
        let item = list.iter_mut().find(|d| d.domain == target)?;
        item.enabled = !item.enabled;
        new_state = item.enabled;
        let _ = crate::config::save_custom_domains(&list);
    }
    if ENABLED.load(Ordering::SeqCst) {
        set_pac(true);
    }
    if new_state {
        test_single_host(target);
    }
    notify_ui();
    Some(new_state)
}

pub fn remove_custom_domain(domain: &str) -> bool {
    let target = domain.to_ascii_lowercase();
    let remaining: Vec<CustomDomain>;
    {
        let mut list = CUSTOM_DOMAINS.lock().unwrap_or_else(|e| e.into_inner());
        let before = list.len();
        list.retain(|d| d.domain != target);
        if list.len() == before {
            return false;
        }
        remaining = list.clone();
        let _ = crate::config::save_custom_domains(&list);
    }
    {
        let mut m = STATS.lock().unwrap_or_else(|e| e.into_inner());
        m.retain(|host, stat| {
            if stat.group != CUSTOM_GROUP_IDX {
                return true;
            }
            if domain_matches(host, &target) {
                return remaining.iter().any(|d| domain_matches(host, &d.domain));
            }
            true
        });
    }
    if ENABLED.load(Ordering::SeqCst) {
        set_pac(true);
    }
    notify_ui();
    true
}

pub fn snapshot() -> Vec<(String, Stat)> {
    let mut v: Vec<(String, Stat)> = STATS
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .clone()
        .into_iter()
        .collect();
    v.sort_by(|a, b| a.1.group.cmp(&b.1.group).then(a.0.cmp(&b.0)));
    v
}

pub fn seed_stats() {
    let custom = custom_domains();
    let mut m = STATS.lock().unwrap_or_else(|e| e.into_inner());
    for h in CATALOG {
        m.entry((*h).to_string()).or_insert(Stat {
            group: group_of(h),
            ..Default::default()
        });
    }
    for item in custom {
        m.entry(item.domain).or_insert(Stat {
            group: CUSTOM_GROUP_IDX,
            ..Default::default()
        });
    }
}

fn record(host: &str, ok: bool, ip: Option<IpAddr>, dns: Option<DnsState>) {
    {
        let mut m = STATS.lock().unwrap_or_else(|e| e.into_inner());
        let s = m.entry(host.to_string()).or_default();
        s.group = group_of(host);
        if ok {
            s.ok += 1;
            s.fail = 0;
        } else {
            s.fail += 1;
        }
        s.last_ok = ok;
        if let Some(ip) = ip {
            s.ip = ip.to_string();
        }
        if let Some(d) = dns {
            s.dns = d;
        }
    }
    notify_ui();
}

// ---------------- WinHTTP DoH ----------------
fn winhttp_session() -> Option<*mut core::ffi::c_void> {
    let ptr = *WINHTTP_SESSION.get_or_init(|| unsafe {
        let agent = wide("SteamBypass/1.0");
        let h = WinHttpOpen(
            agent.as_ptr(),
            WINHTTP_ACCESS_TYPE_NO_PROXY,
            std::ptr::null(),
            std::ptr::null(),
            0,
        );
        if !h.is_null() {
            let max_conns: u32 = 64;
            WinHttpSetOption(
                h,
                WINHTTP_OPTION_MAX_CONNS_PER_SERVER,
                (&max_conns as *const u32).cast(),
                std::mem::size_of::<u32>() as u32,
            );
        }
        h as usize
    });
    if ptr == 0 {
        None
    } else {
        Some(ptr as *mut core::ffi::c_void)
    }
}

fn query_doh_server(server: &str, endpoint: &str, host: &str) -> Vec<IpAddr> {
    let Some(session) = winhttp_session() else {
        return Vec::new();
    };
    let srv_w = wide(server);
    let verb_w = wide("GET");
    let path_w = wide(&format!("{endpoint}?name={host}&type=A"));
    let hdr_w = wide("Accept: application/dns-json\r\n");

    unsafe {
        let conn = WinHttpConnect(session, srv_w.as_ptr(), 443, 0);
        if conn.is_null() {
            return Vec::new();
        }
        let req = WinHttpOpenRequest(
            conn,
            verb_w.as_ptr(),
            path_w.as_ptr(),
            std::ptr::null(),
            std::ptr::null(),
            std::ptr::null(),
            WINHTTP_FLAG_SECURE,
        );
        if req.is_null() {
            WinHttpCloseHandle(conn);
            return Vec::new();
        }

        WinHttpSetTimeouts(req, 5000, 5000, 5000, 5000);
        let hdr_len = (hdr_w.len() - 1) as u32;
        let sent = WinHttpSendRequest(req, hdr_w.as_ptr(), hdr_len, std::ptr::null(), 0, 0, 0);
        let mut body = Vec::new();
        if sent != 0 && WinHttpReceiveResponse(req, std::ptr::null_mut()) != 0 {
            let mut avail: u32 = 0;
            while WinHttpQueryDataAvailable(req, &mut avail) != 0 && avail > 0 {
                let mut chunk = vec![0u8; (avail as usize).min(16384)];
                let mut read: u32 = 0;
                if WinHttpReadData(req, chunk.as_mut_ptr().cast(), chunk.len() as u32, &mut read) == 0 || read == 0 {
                    break;
                }
                body.extend_from_slice(&chunk[..read as usize]);
                if body.len() > 65536 {
                    break;
                }
            }
        }
        WinHttpCloseHandle(req);
        WinHttpCloseHandle(conn);

        match std::str::from_utf8(&body) {
            Ok(text) => parse_doh_a_records(text),
            Err(_) => Vec::new(),
        }
    }
}

pub fn resolve_doh_all(host: &str) -> Vec<IpAddr> {
    if let Ok(ip) = host.parse::<IpAddr>() {
        return vec![ip];
    }
    if host.is_empty()
        || !host
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'.' || b == b'-')
    {
        return Vec::new();
    }
    if let Some(map) = CACHE.lock().unwrap_or_else(|e| e.into_inner()).as_ref() {
        if let Some((ips, t)) = map.get(host) {
            if t.elapsed() < Duration::from_secs(300) {
                return ips.clone();
            }
        }
    }

    let mut ips = query_doh_server("1.1.1.1", "/dns-query", host);
    if ips.is_empty() {
        ips = query_doh_server("1.0.0.1", "/dns-query", host);
    }
    if ips.is_empty() {
        ips = query_doh_server("8.8.8.8", "/resolve", host);
    }
    if !ips.is_empty() {
        CACHE
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get_or_insert_with(HashMap::new)
            .insert(host.to_string(), (ips.clone(), Instant::now()));
    }
    ips
}

#[cfg(test)]
pub fn resolve_doh(host: &str) -> Option<IpAddr> {
    resolve_doh_all(host).into_iter().next()
}

/// Minimal dependency-free parser for DoH DNS-JSON `"Answer"` A records (`type: 1`).
pub(crate) fn parse_doh_a_records(body: &str) -> Vec<IpAddr> {
    let mut out = Vec::new();
    let Some(ans_pos) = find_json_key(body, "Answer") else {
        return out;
    };
    let rest = body[ans_pos..].trim_start();
    if !rest.starts_with('[') {
        return out;
    }
    let Some(arr_end) = find_matching_delim(rest, b'[', b']') else {
        return out;
    };
    let arr = &rest[1..arr_end];

    let mut i = 0;
    let bytes = arr.as_bytes();
    while i < bytes.len() {
        if bytes[i] == b'{' {
            if let Some(rel_end) = find_matching_delim(&arr[i..], b'{', b'}') {
                let obj = &arr[i + 1..i + rel_end];
                if obj_is_type_a(obj) {
                    if let Some(data_val) = obj_get_string(obj, "data") {
                        if let Ok(ip) = data_val.parse::<Ipv4Addr>() {
                            let addr = IpAddr::V4(ip);
                            if !out.contains(&addr) {
                                out.push(addr);
                            }
                        }
                    }
                }
                i += rel_end + 1;
                continue;
            } else {
                break;
            }
        }
        i += 1;
    }
    out
}

#[cfg(test)]
pub(crate) fn parse_doh_a_record(body: &str) -> Option<IpAddr> {
    parse_doh_a_records(body).into_iter().next()
}

fn skip_str(s: &str, mut i: usize) -> usize {
    let b = s.as_bytes();
    i += 1; // skip opening quote
    while i < b.len() {
        if b[i] == b'\\' {
            i += 2;
        } else if b[i] == b'"' {
            return i + 1;
        } else {
            i += 1;
        }
    }
    b.len()
}

fn find_matching_delim(s: &str, open: u8, close: u8) -> Option<usize> {
    let b = s.as_bytes();
    let mut depth = 0usize;
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'"' {
            i = skip_str(s, i);
            continue;
        }
        if b[i] == open {
            depth += 1;
        } else if b[i] == close {
            depth -= 1;
            if depth == 0 {
                return Some(i);
            }
        }
        i += 1;
    }
    None
}

/// Find `"key" :` outside string literals / nested levels and return index just after `:`.
fn find_json_key(s: &str, key: &str) -> Option<usize> {
    let b = s.as_bytes();
    let mut i = 0;
    let mut depth = 0usize;
    while i < b.len() {
        if b[i] == b'"' {
            let end = skip_str(s, i);
            if depth <= 1 && &s[i + 1..end.saturating_sub(1)] == key {
                let after = s[end..].trim_start();
                if let Some(stripped) = after.strip_prefix(':') {
                    return Some(s.len() - stripped.len());
                }
            }
            i = end;
            continue;
        }
        if b[i] == b'{' || b[i] == b'[' {
            depth += 1;
        } else if (b[i] == b'}' || b[i] == b']') && depth > 0 {
            depth -= 1;
        }
        i += 1;
    }
    None
}

fn obj_is_type_a(obj: &str) -> bool {
    let Some(pos) = find_json_key(obj, "type") else {
        return false;
    };
    let rest = obj[pos..].trim_start();
    rest.starts_with('1')
        && !rest
            .as_bytes()
            .get(1)
            .is_some_and(|c| c.is_ascii_digit())
}

fn obj_get_string<'a>(obj: &'a str, key: &str) -> Option<&'a str> {
    let pos = find_json_key(obj, key)?;
    let rest = obj[pos..].trim_start();
    if !rest.starts_with('"') {
        return None;
    }
    let end = skip_str(rest, 0);
    if end >= 2 {
        Some(&rest[1..end - 1])
    } else {
        None
    }
}

// ---------------- PAC & Windows Registry ----------------
pub(crate) fn build_pac(enabled: bool, direct_cdn: bool, extra_domains: &[CustomDomain]) -> String {
    if !enabled {
        return "function FindProxyForURL(url,host){return \"DIRECT\";}".to_string();
    }
    let conds: Vec<String> = DOMAINS
        .iter()
        .copied()
        .filter(|d| !direct_cdn || !CDN_DOMAINS.contains(d))
        .chain(
            extra_domains
                .iter()
                .filter(|c| c.enabled)
                .map(|c| c.domain.as_str()),
        )
        .map(|d| format!("dnsDomainIs(host,\".{d}\")||host==\"{d}\""))
        .collect();
    if conds.is_empty() {
        return "function FindProxyForURL(url,host){return \"DIRECT\";}".to_string();
    }
    format!(
        "function FindProxyForURL(url,host){{if({}){{return \"PROXY 127.0.0.1:{PORT}\";}}return \"DIRECT\";}}",
        conds.join("||")
    )
}

fn pac(enabled: bool) -> String {
    let extra = custom_domains();
    let direct_cdn = DIRECT_CDN.load(Ordering::SeqCst);
    build_pac(enabled, direct_cdn, &extra)
}

pub fn set_pac(enable: bool) {
    let key_w = wide(IE_KEY);
    let name_w = wide("AutoConfigURL");
    unsafe {
        let mut hkey: HKEY = std::ptr::null_mut();
        let rc = RegCreateKeyW(HKEY_CURRENT_USER, key_w.as_ptr(), &mut hkey);
        if rc == 0 && !hkey.is_null() {
            if enable {
                let ver = PAC_VERSION.fetch_add(1, Ordering::SeqCst);
                let url = format!("http://127.0.0.1:{PORT}/proxy.pac?v={ver}");
                let val_w = wide(&url);
                RegSetValueExW(
                    hkey,
                    name_w.as_ptr(),
                    0,
                    REG_SZ,
                    val_w.as_ptr() as *const u8,
                    (val_w.len() * 2) as u32,
                );
            } else {
                RegDeleteValueW(hkey, name_w.as_ptr());
            }
            RegCloseKey(hkey);
        }
        InternetSetOptionW(std::ptr::null(), INTERNET_OPTION_SETTINGS_CHANGED, std::ptr::null(), 0);
        InternetSetOptionW(std::ptr::null(), INTERNET_OPTION_REFRESH, std::ptr::null(), 0);
    }
}

/// Full Windows proxy cleanup: remove AutoConfigURL, force ProxyEnable=0, flush WinInet, clear DNS cache.
pub fn reset_network_proxy() {
    ENABLED.store(false, Ordering::SeqCst);
    clear_dns_cache();
    let key_w = wide(IE_KEY);
    let pac_name_w = wide("AutoConfigURL");
    let enable_name_w = wide("ProxyEnable");
    unsafe {
        let mut hkey: HKEY = std::ptr::null_mut();
        let rc = RegCreateKeyW(HKEY_CURRENT_USER, key_w.as_ptr(), &mut hkey);
        if rc == 0 && !hkey.is_null() {
            RegDeleteValueW(hkey, pac_name_w.as_ptr());
            let zero: u32 = 0;
            RegSetValueExW(
                hkey,
                enable_name_w.as_ptr(),
                0,
                REG_DWORD,
                (&zero as *const u32).cast(),
                4,
            );
            RegCloseKey(hkey);
        }
        InternetSetOptionW(std::ptr::null(), INTERNET_OPTION_SETTINGS_CHANGED, std::ptr::null(), 0);
        InternetSetOptionW(std::ptr::null(), INTERNET_OPTION_REFRESH, std::ptr::null(), 0);
    }
    notify_ui();
}

pub fn set_enabled(on: bool) {
    ENABLED.store(on, Ordering::SeqCst);
    set_pac(on);
    notify_ui();
}

/// Background 0%-CPU listener for Windows IP/Wi-Fi changes via `NotifyAddrChange`.
pub fn start_network_watcher() {
    let _ = thread::Builder::new()
        .name("net-watch".into())
        .stack_size(256 * 1024)
        .spawn(|| loop {
            let rc = unsafe { NotifyAddrChange(std::ptr::null_mut(), std::ptr::null_mut()) };
            if rc != 0 {
                thread::sleep(Duration::from_secs(10));
                continue;
            }
            // Debounce DHCP / Wi-Fi association bursts
            thread::sleep(Duration::from_secs(2));
            clear_dns_cache();
            if ENABLED.load(Ordering::SeqCst) {
                set_pac(true);
                run_test();
            }
            let h = UI_HWND.load(Ordering::SeqCst);
            if h != 0 {
                unsafe {
                    PostMessageW(h as HWND, WM_APP_NET_CHANGED, 0, 0);
                }
            }
        });
}

/// Triggered on `WM_POWERBROADCAST` resume from sleep/hibernate.
pub fn on_system_resume() {
    let _ = thread::Builder::new()
        .name("net-resume".into())
        .stack_size(256 * 1024)
        .spawn(|| {
            thread::sleep(Duration::from_millis(2500));
            clear_dns_cache();
            if ENABLED.load(Ordering::SeqCst) {
                set_pac(true);
                run_test();
            }
            let h = UI_HWND.load(Ordering::SeqCst);
            if h != 0 {
                unsafe {
                    PostMessageW(h as HWND, WM_APP_NET_CHANGED, 1, 0);
                }
            }
        });
}

// ---------------- Local Proxy Server ----------------
pub fn start_proxy() -> bool {
    let Ok(listener) = TcpListener::bind(("127.0.0.1", PORT)) else {
        return false;
    };
    let _ = thread::Builder::new()
        .name("proxy-accept".into())
        .stack_size(256 * 1024)
        .spawn(move || {
            for stream in listener.incoming().flatten() {
                let _ = thread::Builder::new()
                    .stack_size(512 * 1024)
                    .spawn(move || {
                        handle(stream);
                    });
            }
        });
    true
}

fn connect_upstream(host: &str, port: u16) -> Option<TcpStream> {
    let ips = resolve_doh_all(host);
    let mut connected: Option<(TcpStream, IpAddr)> = None;
    for ip in ips.iter().take(2) {
        let addr = SocketAddr::new(*ip, port);
        if let Ok(s) = TcpStream::connect_timeout(&addr, Duration::from_secs(6)) {
            connected = Some((s, *ip));
            break;
        }
    }
    match connected {
        Some((s, ip)) => {
            let _ = s.set_nodelay(true);
            record(host, true, Some(ip), None);
            Some(s)
        }
        None => {
            record(host, false, ips.first().copied(), None);
            None
        }
    }
}

fn relay_one_way(src: &mut TcpStream, dst: &mut TcpStream) {
    let mut buf = vec![0u8; 16 * 1024];
    loop {
        match src.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => {
                if dst.write_all(&buf[..n]).is_err() {
                    break;
                }
            }
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(_) => break,
        }
    }
    let _ = dst.shutdown(Shutdown::Write);
}

fn relay_bidirectional(mut a: TcpStream, mut b: TcpStream) {
    let idle = Some(Duration::from_secs(300));
    let wto = Some(Duration::from_secs(60));
    let _ = a.set_read_timeout(idle);
    let _ = a.set_write_timeout(wto);
    let _ = b.set_read_timeout(idle);
    let _ = b.set_write_timeout(wto);

    let (Ok(mut a_read), Ok(mut b_write)) = (a.try_clone(), b.try_clone()) else {
        return;
    };
    let t = thread::Builder::new()
        .stack_size(512 * 1024)
        .spawn(move || relay_one_way(&mut a_read, &mut b_write));
    relay_one_way(&mut b, &mut a);
    if let Ok(h) = t {
        let _ = h.join();
    }
}

fn handle(mut c: TcpStream) -> Option<()> {
    let _ = c.set_nodelay(true);
    let _ = c.set_read_timeout(Some(Duration::from_secs(15)));

    let mut buf = vec![0u8; 8192];
    let mut n = 0;
    loop {
        let r = c.read(&mut buf[n..]).ok()?;
        if r == 0 {
            return None;
        }
        n += r;
        if buf[..n].windows(4).any(|w| w == b"\r\n\r\n") || n == buf.len() {
            break;
        }
    }
    let head = String::from_utf8_lossy(&buf[..n]).to_string();
    let first = head.lines().next()?;
    let mut parts = first.split_whitespace();
    let method = parts.next()?;
    let target = parts.next()?;
    let version = parts.next().unwrap_or("HTTP/1.1");

    if method == "CONNECT" {
        let (raw_host, port) = target.rsplit_once(':')?;
        let host = raw_host.trim_matches(|ch| ch == '[' || ch == ']');
        let port: u16 = port.parse().ok()?;
        let Some(mut up) = connect_upstream(host, port) else {
            let _ = c.write_all(b"HTTP/1.1 502 Bad Gateway\r\n\r\n");
            return None;
        };
        c.write_all(b"HTTP/1.1 200 Connection Established\r\n\r\n")
            .ok()?;

        // First client packet = TLS ClientHello; split it so DPI cannot inspect the SNI.
        let mut hello = vec![0u8; 16384];
        let k = c.read(&mut hello).ok()?;
        if k == 0 {
            return None;
        }
        if hello[0] == 0x16 && k > 10 {
            up.write_all(&hello[..3]).ok()?;
            up.flush().ok()?;
            thread::sleep(Duration::from_millis(20));
            up.write_all(&hello[3..k]).ok()?;
        } else {
            up.write_all(&hello[..k]).ok()?;
        }
        relay_bidirectional(c, up);
    } else if target.starts_with("/proxy.pac") || target.contains("/proxy.pac") {
        let body = pac(ENABLED.load(Ordering::SeqCst));
        let resp = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: application/x-ns-proxy-autoconfig\r\nCache-Control: no-store\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
            body.len(),
            body
        );
        let _ = c.write_all(resp.as_bytes());
    } else if let Some(rest) = target.strip_prefix("http://") {
        let (hostport, path) = match rest.find('/') {
            Some(idx) => (&rest[..idx], &rest[idx..]),
            None => (rest, "/"),
        };
        let (raw_host, port) = match hostport.rsplit_once(':') {
            Some((h, p)) => (h, p.parse().unwrap_or(80)),
            None => (hostport, 80),
        };
        let host = raw_host.trim_matches(|ch| ch == '[' || ch == ']');
        let mut up = connect_upstream(host, port)?;
        let mut req_bytes = Vec::with_capacity(n);
        if let Some(crlf) = buf[..n].windows(2).position(|w| w == b"\r\n") {
            let new_req_line = format!("{method} {path} {version}");
            req_bytes.extend_from_slice(new_req_line.as_bytes());
            req_bytes.extend_from_slice(&buf[crlf..n]);
        } else {
            req_bytes.extend_from_slice(&buf[..n]);
        }
        if req_bytes.len() > 6 {
            up.write_all(&req_bytes[..3]).ok()?;
            up.flush().ok()?;
            thread::sleep(Duration::from_millis(15));
            up.write_all(&req_bytes[3..]).ok()?;
        } else {
            up.write_all(&req_bytes).ok()?;
        }
        relay_bidirectional(c, up);
    }
    Some(())
}

fn is_bogon_v4(ip: Ipv4Addr) -> bool {
    let o = ip.octets();
    ip.is_unspecified()
        || ip.is_loopback()
        || ip.is_private()
        || ip.is_link_local()
        || (o[0] == 100 && (64..=127).contains(&o[1]))
}

pub(crate) fn classify_dns(sys_ips: &[IpAddr], doh_ips: &[IpAddr]) -> DnsState {
    if doh_ips.is_empty() {
        return DnsState::Unknown;
    }
    if sys_ips.is_empty() {
        return DnsState::Failed;
    }
    for ip in sys_ips {
        if let IpAddr::V4(v4) = ip {
            if is_bogon_v4(*v4) {
                return DnsState::Poisoned;
            }
        }
    }
    for s in sys_ips {
        if doh_ips.contains(s) {
            return DnsState::Clean;
        }
        if let IpAddr::V4(sv4) = s {
            let so = sv4.octets();
            for d in doh_ips {
                if let IpAddr::V4(dv4) = d {
                    let do_ = dv4.octets();
                    if so[0] == do_[0] && so[1] == do_[1] {
                        return DnsState::Clean;
                    }
                }
            }
        }
    }
    DnsState::Poisoned
}

fn probe_host(host: &str) {
    let sys_ips: Vec<IpAddr> = (host, 443u16)
        .to_socket_addrs()
        .ok()
        .map(|it| {
            it.filter(|a| a.is_ipv4())
                .map(|a| a.ip())
                .collect()
        })
        .unwrap_or_default();
    let mut doh_ips = resolve_doh_all(host);
    if doh_ips.is_empty() && !host.starts_with("www.") {
        doh_ips = resolve_doh_all(&format!("www.{host}"));
    }
    let dns = classify_dns(&sys_ips, &doh_ips);
    let mut ok = false;
    let mut used_ip = doh_ips.first().copied();
    for ip in doh_ips.iter().take(2) {
        if TcpStream::connect_timeout(&SocketAddr::new(*ip, 443), Duration::from_secs(6)).is_ok() {
            ok = true;
            used_ip = Some(*ip);
            break;
        }
    }
    record(host, ok, used_ip, Some(dns));
}

pub fn test_single_host(host: String) {
    PENDING.fetch_add(1, Ordering::SeqCst);
    notify_ui();
    let _ = thread::Builder::new()
        .stack_size(512 * 1024)
        .spawn(move || {
            probe_host(&host);
            PENDING.fetch_sub(1, Ordering::SeqCst);
            notify_ui();
        });
}

/// Active check of every catalog & enabled custom host: ISP DNS vs DoH, then a real TCP connect.
pub fn run_test() {
    let mut hosts: Vec<String> = CATALOG.iter().map(|s| (*s).to_string()).collect();
    for item in custom_domains() {
        if item.enabled && !hosts.contains(&item.domain) {
            hosts.push(item.domain);
        }
    }
    if PENDING
        .compare_exchange(0, hosts.len(), Ordering::SeqCst, Ordering::SeqCst)
        .is_err()
    {
        return;
    }
    notify_ui();

    for host in hosts {
        let _ = thread::Builder::new()
            .stack_size(512 * 1024)
            .spawn(move || {
                probe_host(&host);
                if PENDING.fetch_sub(1, Ordering::SeqCst) == 1 {
                    notify_ui();
                    let all_failed = STATS
                        .lock()
                        .unwrap_or_else(|e| e.into_inner())
                        .values()
                        .all(|s| s.ok == 0);
                    if all_failed && ENABLED.load(Ordering::SeqCst) {
                        let _ = thread::Builder::new()
                            .stack_size(512 * 1024)
                            .spawn(|| {
                                thread::sleep(Duration::from_secs(4));
                                if ENABLED.load(Ordering::SeqCst)
                                    && STATS
                                        .lock()
                                        .unwrap_or_else(|e| e.into_inner())
                                        .values()
                                        .all(|s| s.ok == 0)
                                {
                                    run_test();
                                }
                            });
                    }
                } else {
                    notify_ui();
                }
            });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classifies_cdn_and_poisoned_dns_accurately() {
        let doh = vec![
            IpAddr::V4(Ipv4Addr::new(37, 19, 207, 37)),
            IpAddr::V4(Ipv4Addr::new(37, 19, 207, 38)),
        ];
        // Rotating edge IP in same /16 -> Clean
        assert_eq!(
            classify_dns(&[IpAddr::V4(Ipv4Addr::new(37, 19, 207, 99))], &doh),
            DnsState::Clean
        );
        // Empty system DNS -> Failed
        assert_eq!(classify_dns(&[], &doh), DnsState::Failed);
        // Bogon / loopback / private -> Poisoned
        assert_eq!(
            classify_dns(&[IpAddr::V4(Ipv4Addr::new(127, 0, 0, 1))], &doh),
            DnsState::Poisoned
        );
        assert_eq!(
            classify_dns(&[IpAddr::V4(Ipv4Addr::new(0, 0, 0, 0))], &doh),
            DnsState::Poisoned
        );
    }

    #[test]
    fn pac_respects_direct_cdn_and_custom_enabled() {
        let extra = vec![
            CustomDomain { domain: "reddit.com".into(), enabled: true },
            CustomDomain { domain: "bbc.com".into(), enabled: false },
        ];
        let full = build_pac(true, false, &extra);
        assert!(full.contains(r#"dnsDomainIs(host,".steampowered.com")"#));
        assert!(full.contains(r#"dnsDomainIs(host,".steamcontent.com")"#));
        assert!(full.contains(r#"dnsDomainIs(host,".akamaihd.net")"#));
        assert!(full.contains(r#"dnsDomainIs(host,".reddit.com")||host=="reddit.com""#));
        assert!(!full.contains("bbc.com"));

        let direct = build_pac(true, true, &extra);
        assert!(direct.contains(r#"dnsDomainIs(host,".steampowered.com")"#));
        assert!(!direct.contains("steamcontent.com"));
        assert!(!direct.contains("akamaihd.net"));
        assert!(direct.contains("reddit.com"));

        let off = build_pac(false, false, &extra);
        assert_eq!(off, r#"function FindProxyForURL(url,host){return "DIRECT";}"#);
    }

    #[test]
    fn parses_direct_a_record() {
        let json = r#"{"Status":0,"Answer":[{"name":"store.steampowered.com","type":1,"TTL":10,"data":"23.15.142.182"}]}"#;
        assert_eq!(parse_doh_a_record(json), Some(IpAddr::V4(Ipv4Addr::new(23, 15, 142, 182))));
    }

    #[test]
    fn skips_cname_and_picks_a_record() {
        let json = r#"{"Status":0,"Answer":[{"name":"a.com","type":5,"TTL":30,"data":"b.com."},{"name":"b.com","type":1,"TTL":20,"data":"104.16.1.2"},{"name":"b.com","type":1,"TTL":20,"data":"104.16.1.3"}]}"#;
        assert_eq!(parse_doh_a_record(json), Some(IpAddr::V4(Ipv4Addr::new(104, 16, 1, 2))));
    }

    #[test]
    fn ignores_type_16_and_15_and_handles_spaces() {
        let json = r#"{ "Status" : 0 , "Answer" : [ { "name": "x", "type" : 16, "data": "1.2.3.4" } , { "name": "x", "type" : 15, "data": "9.9.9.9" }, { "name": "x", "type": 1, "data": "23.200.143.11" } ] }"#;
        assert_eq!(parse_doh_a_record(json), Some(IpAddr::V4(Ipv4Addr::new(23, 200, 143, 11))));
    }

    #[test]
    fn handles_nxdomain() {
        let json = r#"{"Status":3,"Authority":[{"name":"com","type":6}]}"#;
        assert_eq!(parse_doh_a_record(json), None);
    }

    #[test]
    fn live_doh_resolves_steam() {
        let ip = resolve_doh("store.steampowered.com");
        println!("live DoH store.steampowered.com -> {ip:?}");
        assert!(matches!(ip, Some(IpAddr::V4(_))));
    }

    #[test]
    fn relay_handles_large_payload_without_stack_overflow() {
        let up_ln = TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let up_addr = up_ln.local_addr().unwrap();
        let proxy_ln = TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let proxy_addr = proxy_ln.local_addr().unwrap();

        // Echo server
        let echo = thread::spawn(move || {
            let (mut s, _) = up_ln.accept().unwrap();
            let mut data = Vec::new();
            s.read_to_end(&mut data).unwrap();
            s.write_all(&data).unwrap();
            let _ = s.shutdown(Shutdown::Write);
        });

        // Relay thread
        let relay_t = thread::Builder::new()
            .stack_size(512 * 1024)
            .spawn(move || {
                let (client, _) = proxy_ln.accept().unwrap();
                let upstream = TcpStream::connect(up_addr).unwrap();
                relay_bidirectional(client, upstream);
            })
            .unwrap();

        let payload: Vec<u8> = (0..262_144usize).map(|i| (i % 251) as u8).collect();
        let mut c = TcpStream::connect(proxy_addr).unwrap();
        c.write_all(&payload).unwrap();
        c.shutdown(Shutdown::Write).unwrap();
        let mut got = Vec::new();
        c.read_to_end(&mut got).unwrap();
        assert_eq!(got.len(), payload.len());
        assert_eq!(got, payload);

        echo.join().unwrap();
        relay_t.join().unwrap();
    }
}
