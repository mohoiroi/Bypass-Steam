//! Local HTTP/HTTPS proxy with DNS-over-HTTPS (WinHTTP) and TLS ClientHello splitting.

use crate::{config::{CustomDomain, Settings}, pac};
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
    Foundation::{GetLastError, HWND},
    Networking::WinHttp::*,
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DohProvider {
    Cloudflare = 0,
    Google = 1,
    AdGuard = 2,
    Custom = 3,
}

impl DohProvider {
    pub const ALL: [DohProvider; 4] = [
        DohProvider::Cloudflare,
        DohProvider::Google,
        DohProvider::AdGuard,
        DohProvider::Custom,
    ];

    pub fn from_id(id: &str) -> Self {
        match id.trim().to_ascii_lowercase().as_str() {
            "google" => Self::Google,
            "adguard" => Self::AdGuard,
            "custom" | "dnssb" => Self::Custom,
            _ => Self::Cloudflare,
        }
    }

    pub const fn index(self) -> usize {
        self as usize
    }

    pub const fn id(self) -> &'static str {
        match self {
            Self::Cloudflare => "cloudflare",
            Self::Google => "google",
            Self::AdGuard => "adguard",
            Self::Custom => "custom",
        }
    }

    pub const fn label(self) -> &'static str {
        match self {
            Self::Cloudflare => "Cloudflare",
            Self::Google => "Google",
            Self::AdGuard => "AdGuard",
            Self::Custom => "Custom",
        }
    }

    pub fn addr(self) -> String {
        match self {
            Self::Cloudflare => "1.1.1.1",
            Self::Google => "8.8.8.8",
            Self::AdGuard => "94.140.14.14",
            Self::Custom => return custom_url().split('/' ).nth(2).unwrap_or("Custom").to_owned(),
        }.to_owned()
    }

    /// Ordered primary -> secondary -> resilient fallback JSON DoH endpoints.
    pub const fn endpoints(self) -> &'static [(&'static str, &'static str)] {
        match self {
            Self::Cloudflare => &[
                ("1.1.1.1", "/dns-query"),
                ("1.0.0.1", "/dns-query"),
                ("8.8.8.8", "/resolve"),
            ],
            Self::Google => &[
                ("8.8.8.8", "/resolve"),
                ("8.8.4.4", "/resolve"),
                ("1.1.1.1", "/dns-query"),
            ],
            Self::AdGuard => &[
                ("94.140.14.14", "/resolve"),
                ("94.140.15.15", "/resolve"),
                ("1.1.1.1", "/dns-query"),
            ],
            Self::Custom => &[],
        }
    }
}

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
    pub latency_ms: Option<u32>,
}

pub static STATS: Mutex<BTreeMap<String, Stat>> = Mutex::new(BTreeMap::new());
pub static CUSTOM_DOMAINS: Mutex<Vec<CustomDomain>> = Mutex::new(Vec::new());
pub static ENABLED: AtomicBool = AtomicBool::new(false);
pub static DIRECT_CDN: AtomicBool = AtomicBool::new(false);
pub static PENDING: AtomicUsize = AtomicUsize::new(0);

static PAC_UPDATE: Mutex<()> = Mutex::new(());
static NEXT_SESSION_ID: AtomicUsize = AtomicUsize::new(0);
static ACTIVE_SESSIONS: Mutex<BTreeMap<usize, ProxySession>> = Mutex::new(BTreeMap::new());
static UI_HWND: AtomicIsize = AtomicIsize::new(0);
pub static UI_DIRTY: AtomicBool = AtomicBool::new(false);

type DnsCache = HashMap<(usize, String, String), (Vec<IpAddr>, Instant)>;
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

pub fn apply_settings(settings: Settings) {
    *DOH_SETTINGS.lock().unwrap_or_else(|e| e.into_inner()) = (settings.provider, settings.url);
    {
        let mut domains = CUSTOM_DOMAINS.lock().unwrap_or_else(|e| e.into_inner());
        DIRECT_CDN.store(settings.direct_cdn, Ordering::SeqCst);
        *domains = settings.domains;
        STATS.lock().unwrap_or_else(|e| e.into_inner()).retain(|host, stat| {
            stat.group != CUSTOM_GROUP_IDX || domains.iter().any(|item| domain_matches(host, &item.domain))
        });
    }
    clear_dns_cache();
    seed_stats();
    refresh_pac();
    disconnect_disallowed_sessions();
    notify_ui();
}

pub fn doh_provider() -> DohProvider {
    DOH_SETTINGS.lock().unwrap_or_else(|e| e.into_inner()).0
}

pub fn set_doh_provider(provider: DohProvider) -> bool {
    if !crate::config::save_doh_provider(provider) { return false; }
    DOH_SETTINGS.lock().unwrap_or_else(|e| e.into_inner()).0 = provider;

    clear_dns_cache();
    notify_ui();
    true
}

pub fn set_direct_cdn(direct: bool) {
    DIRECT_CDN.store(direct, Ordering::SeqCst);
    let _ = crate::config::save_direct_cdn(direct);
    if direct {
        disconnect_disallowed_sessions();
    }
    refresh_pac();
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
        refresh_pac();
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
    if !new_state {
        disconnect_disallowed_sessions();
    }
    refresh_pac();
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
    disconnect_disallowed_sessions();
    refresh_pac();
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

fn record(
    host: &str,
    ok: bool,
    ip: Option<IpAddr>,
    dns: Option<DnsState>,
    latency_ms: Option<u32>,
) {
    {
        let mut m = STATS.lock().unwrap_or_else(|e| e.into_inner());
        let s = m.entry(host.to_string()).or_default();
        s.group = group_of(host);
        if ok {
            s.ok += 1;
            s.fail = 0;
            if let Some(ms) = latency_ms {
                s.latency_ms = Some(ms);
            }
        } else {
            s.fail += 1;
            s.latency_ms = None;
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
        let agent = wide(concat!("SteamBypass/", env!("CARGO_PKG_VERSION")));
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

static DOH_SETTINGS: Mutex<(DohProvider, String)> = Mutex::new((DohProvider::Cloudflare, String::new()));

pub fn custom_url() -> String {
    DOH_SETTINGS.lock().unwrap_or_else(|e| e.into_inner()).1.clone()
}

pub fn save_custom_url(url: &str) -> bool {
    if parse_endpoint(url).is_none() || !crate::config::save_doh_url(url) {
        return false;
    }
    DOH_SETTINGS.lock().unwrap_or_else(|e| e.into_inner()).1 = url.to_owned();
    true
}

pub fn parse_endpoint(url: &str) -> Option<(&str, u16, &str)> {
    if url.len() > 2048 || !url.bytes().all(|c| c.is_ascii_graphic()) || url.contains(['#', '\\']) {
        return None;
    }
    let rest = url.strip_prefix("https://")?;
    let slash = rest.find('/')?;
    let authority = &rest[..slash];
    let path = &rest[slash..];
    let (host, port) = if let Some(ip) = authority.strip_prefix('[') {
        let (host, suffix) = ip.split_once(']')?;
        host.parse::<std::net::Ipv6Addr>().ok()?;
        (host, suffix.strip_prefix(':').or(if suffix.is_empty() { Some("443") } else { None })?)
    } else {
        let (host, port) = authority.split_once(':').unwrap_or((authority, "443"));
        if host.len() > 253 || !host.split('.').all(|s| !s.is_empty() && s.len() <= 63
            && !s.starts_with('-') && !s.ends_with('-') && s.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-')) {
            return None;
        }
        (host, port)
    };
    let port = port.parse::<u16>().ok()?;
    (port > 0).then_some((host, port, path))
}

fn dns_question(host: &str) -> Option<Vec<u8>> {
    let mut msg = vec![0, 0, 1, 0, 0, 1, 0, 0, 0, 0, 0, 0];
    let host = host.strip_suffix('.').unwrap_or(host);
    if host.len() > 253 { return None; }
    for label in host.split('.') {
        if label.is_empty() || label.len() > 63 { return None; }
        msg.push(label.len() as u8);
        msg.extend_from_slice(label.as_bytes());
    }
    msg.extend_from_slice(&[0, 0, 1, 0, 1]);
    Some(msg)
}

fn skip_dns_name(data: &[u8], mut pos: usize) -> Option<usize> {
    loop {
        let len = *data.get(pos)? as usize;
        pos += 1;
        match len {
            0 => return Some(pos),
            1..=63 => { data.get(pos..pos + len)?; pos += len; }
            192..=255 => { data.get(pos)?; return Some(pos + 1); }
            _ => return None,
        }
    }
}

fn wire_addresses(data: &[u8]) -> Option<Vec<IpAddr>> {
    let header = data.get(..12)?;
    if header[..2] != [0, 0] || header[2] & 0xfa != 0x80 || header[4..6] != [0, 1] {
        return None;
    }
    if header[3] & 15 != 0 { return Some(Vec::new()); }
    let mut pos = skip_dns_name(data, 12)? + 4;
    data.get(..pos)?;
    let mut ips = Vec::new();
    for _ in 0..u16::from_be_bytes([header[6], header[7]]) {
        pos = skip_dns_name(data, pos)?;
        let rr = data.get(pos..pos + 10)?;
        let len = u16::from_be_bytes([rr[8], rr[9]]) as usize;
        pos += 10;
        let value = data.get(pos..pos + len)?;
        if rr[..4] == [0, 1, 0, 1] && len == 4 {
            let ip = IpAddr::V4(Ipv4Addr::new(value[0], value[1], value[2], value[3]));
            if !ips.contains(&ip) { ips.push(ip); }
        }
        pos += len;
    }
    Some(ips)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DohError {
    InvalidUrl,
    Certificate,
    Timeout,
    InvalidResponse,
    NoAddress,
    HttpStatus(u32),
    Connection(u32),
}

impl DohError {
    fn from_winhttp(code: u32) -> Self {
        match code {
            ERROR_WINHTTP_TIMEOUT => Self::Timeout,
            ERROR_WINHTTP_SECURE_FAILURE
            | ERROR_WINHTTP_SECURE_CERT_CN_INVALID
            | ERROR_WINHTTP_SECURE_CERT_DATE_INVALID
            | ERROR_WINHTTP_SECURE_CERT_REVOKED
            | ERROR_WINHTTP_SECURE_CERT_REV_FAILED
            | ERROR_WINHTTP_SECURE_CERT_WRONG_USAGE
            | ERROR_WINHTTP_SECURE_INVALID_CA
            | ERROR_WINHTTP_SECURE_INVALID_CERT
            | ERROR_WINHTTP_CLIENT_AUTH_CERT_NEEDED => Self::Certificate,
            ERROR_WINHTTP_INVALID_SERVER_RESPONSE => Self::InvalidResponse,
            _ => Self::Connection(code),
        }
    }

    fn last() -> Self {
        Self::from_winhttp(unsafe { GetLastError() })
    }
}

struct DohHandle(*mut core::ffi::c_void);

impl DohHandle {
    fn checked(handle: *mut core::ffi::c_void) -> Result<Self, DohError> {
        if handle.is_null() { Err(DohError::last()) } else { Ok(Self(handle)) }
    }
}

impl Drop for DohHandle {
    fn drop(&mut self) {
        unsafe { WinHttpCloseHandle(self.0); }
    }
}

fn query_doh_server(server: &str, port: u16, endpoint: &str, host: &str, wire: bool) -> Result<Vec<IpAddr>, DohError> {
    let session = winhttp_session().ok_or(DohError::Connection(0))?;
    let srv_w = wide(server);
    let payload = if wire { dns_question(host).ok_or(DohError::InvalidResponse)? } else { Vec::new() };
    let verb_w = wide(if wire { "POST" } else { "GET" });
    let sep = if endpoint.contains('?') { '&' } else { '?' };
    let path_w = if wire { wide(endpoint) } else { wide(&format!("{endpoint}{sep}name={host}&type=A")) };
    let hdr_w = wide(if wire {
        "Content-Type: application/dns-message\r\nAccept: application/dns-message\r\n"
    } else { "Accept: application/dns-json\r\n" });

    let mut body = Vec::new();
    unsafe {
        let conn = DohHandle::checked(WinHttpConnect(session, srv_w.as_ptr(), port, 0))?;
        let req = DohHandle::checked(WinHttpOpenRequest(
            conn.0,
            verb_w.as_ptr(),
            path_w.as_ptr(),
            std::ptr::null(),
            std::ptr::null(),
            std::ptr::null(),
            WINHTTP_FLAG_SECURE,
        ))?;
        if WinHttpSetTimeouts(req.0, 5000, 5000, 5000, 5000) == 0 {
            return Err(DohError::last());
        }
        let started = Instant::now();
        let payload_len = payload.len() as u32;
        if WinHttpSendRequest(
            req.0, hdr_w.as_ptr(), (hdr_w.len() - 1) as u32,
            payload.as_ptr().cast(), payload_len, payload_len, 0,
        ) == 0 || WinHttpReceiveResponse(req.0, std::ptr::null_mut()) == 0 {
            return Err(DohError::last());
        }
        let mut status = 0u32;
        let mut status_len = std::mem::size_of_val(&status) as u32;
        if WinHttpQueryHeaders(
            req.0, WINHTTP_QUERY_STATUS_CODE | WINHTTP_QUERY_FLAG_NUMBER, std::ptr::null(),
            (&mut status as *mut u32).cast(), &mut status_len, std::ptr::null_mut(),
        ) == 0 {
            return Err(DohError::last());
        }
        if !(200..300).contains(&status) {
            return Err(DohError::HttpStatus(status));
        }

        let mut chunk = [0u8; 8192];
        loop {
            let mut read = 0;
            if WinHttpReadData(req.0, chunk.as_mut_ptr().cast(), chunk.len() as u32, &mut read) == 0 {
                return Err(DohError::last());
            }
            if started.elapsed() >= Duration::from_secs(12) {
                return Err(DohError::Timeout);
            }
            if read == 0 { break; }
            if body.len() + read as usize > 65536 {
                return Err(DohError::InvalidResponse);
            }
            body.extend_from_slice(&chunk[..read as usize]);
        }
    }

    if wire {
        wire_addresses(&body).ok_or(DohError::InvalidResponse)
    } else {
        json_doh_addresses(&body).ok_or(DohError::InvalidResponse)
    }
}

fn json_doh_addresses(body: &[u8]) -> Option<Vec<IpAddr>> {
    let json = std::str::from_utf8(body).ok()?.trim();
    if !json.starts_with('{') || find_matching_delim(json, b'{', b'}')? + 1 != json.len() {
        return None;
    }
    let status = json[find_json_key(json, "Status")?..].trim_start();
    let status = status.split([',', '}']).next()?.trim().parse::<u16>().ok()?;

    if let Some(index) = find_json_key(json, "Answer") {
        let answer = json[index..].trim_start();
        if !answer.starts_with('[') || find_matching_delim(answer, b'[', b']').is_none() {
            return None;
        }
    }
    Some(if status == 0 { parse_doh_a_records(json) } else { Vec::new() })
}

fn query_custom_doh(server: &str, port: u16, path: &str, host: &str) -> Result<Vec<IpAddr>, DohError> {
    match query_doh_server(server, port, path, host, true) {
        Err(DohError::InvalidResponse | DohError::HttpStatus(_)) => {
            query_doh_server(server, port, path, host, false)
        }
        result => result,
    }
}

pub fn test_custom_doh(url: &str) -> Result<usize, DohError> {
    let (server, port, path) = parse_endpoint(url).ok_or(DohError::InvalidUrl)?;
    let addresses = query_custom_doh(server, port, path, "example.org")?;
    if addresses.is_empty() { Err(DohError::NoAddress) } else { Ok(addresses.len()) }
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
    let (provider, url) = {
        let settings = DOH_SETTINGS.lock().unwrap_or_else(|e| e.into_inner());
        let url = if settings.0 == DohProvider::Custom { settings.1.clone() } else { String::new() };
        (settings.0, url)
    };
    let cache_key = (provider.index(), url.clone(), host.to_owned());
    if let Some(map) = CACHE.lock().unwrap_or_else(|e| e.into_inner()).as_ref() {
        if let Some((ips, t)) = map.get(&cache_key) {
            if t.elapsed() < Duration::from_secs(300) {
                return ips.clone();
            }
        }
    }

    let mut ips = Vec::new();
    if let Some((server, port, path)) = parse_endpoint(&url) {
        ips = query_custom_doh(server, port, path, host).unwrap_or_default();
    }
    for &(server, endpoint) in provider.endpoints() {
        ips = query_doh_server(server, 443, endpoint, host, false).unwrap_or_default();
        if !ips.is_empty() {
            break;
        }
    }
    if !ips.is_empty() {
        CACHE
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get_or_insert_with(HashMap::new)
            .insert(cache_key, (ips.clone(), Instant::now()));
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
fn routing_rules(direct_cdn: bool, custom: &[CustomDomain]) -> Vec<(&str, bool)> {
    let mut rules: Vec<_> = DOMAINS
        .iter()
        .map(|&domain| (domain, !(direct_cdn && CDN_DOMAINS.contains(&domain))))
        .collect();
    let custom_start = rules.len();
    rules.extend(custom.iter().map(|item| (item.domain.as_str(), item.enabled)));
    // A specific custom entry overrides a broader custom suffix.
    rules[custom_start..].sort_by_key(|(domain, _)| std::cmp::Reverse(domain.len()));
    rules
}

fn routes_through_proxy(host: &str, rules: &[(&str, bool)]) -> bool {
    let host = host.trim_end_matches('.').to_ascii_lowercase();
    rules
        .iter()
        .find(|(domain, _)| domain_matches(&host, domain))
        .is_some_and(|(_, enabled)| *enabled)
}

pub(crate) fn build_pac(enabled: bool, direct_cdn: bool, extra_domains: &[CustomDomain]) -> String {
    let mut script = "function FindProxyForURL(url,host){".to_string();
    if enabled {
        script.push_str("host=host.toLowerCase().replace(/\\.+$/, '');");
        let proxy = format!("PROXY 127.0.0.1:{PORT}");
        for (domain, enabled) in routing_rules(direct_cdn, extra_domains) {
            let route = if enabled { &proxy } else { "DIRECT" };
            script.push_str(&format!(
                r#"if(dnsDomainIs(host,".{domain}")||host=="{domain}")return "{route}";"#
            ));
        }
    }
    script.push_str("return \"DIRECT\";}");
    script
}

fn pac(enabled: bool) -> String {
    let extra = custom_domains();
    let direct_cdn = DIRECT_CDN.load(Ordering::SeqCst);
    build_pac(enabled, direct_cdn, &extra)
}

fn refresh_pac() {
    let _update = PAC_UPDATE.lock().unwrap_or_else(|e| e.into_inner());
    if ENABLED.load(Ordering::SeqCst) && pac::WINDOWS.refresh() {
        pac::notify_windows();
    }
}

pub fn recover_network_proxy() -> Result<pac::Recovery, ()> {
    let _update = PAC_UPDATE.lock().unwrap_or_else(|e| e.into_inner());
    restore_pac()
}

fn restore_pac() -> Result<pac::Recovery, ()> {
    let restored = pac::WINDOWS.restore()?;
    if restored == pac::Recovery::Restored {
        pac::notify_windows();
    }
    Ok(restored)
}

pub fn reset_network_proxy() -> bool {
    clear_dns_cache();
    set_enabled(false)
}

pub fn set_enabled(on: bool) -> bool {
    let _update = PAC_UPDATE.lock().unwrap_or_else(|e| e.into_inner());
    let success = if on {
        ENABLED.store(true, Ordering::SeqCst);
        let installed = pac::WINDOWS.install();
        ENABLED.store(installed, Ordering::SeqCst);
        if installed {
            pac::notify_windows();
        }
        installed
    } else {
        ENABLED.store(false, Ordering::SeqCst);
        disconnect_disallowed_sessions();
        restore_pac().is_ok()
    };
    notify_ui();
    success
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
                refresh_pac();
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
                refresh_pac();
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
struct ProxySession {
    host: String,
    client: TcpStream,
    upstream: TcpStream,
}

struct SessionGuard(usize);

impl Drop for SessionGuard {
    fn drop(&mut self) {
        ACTIVE_SESSIONS
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(&self.0);
    }
}

fn current_route_allowed(host: &str) -> bool {
    let custom = CUSTOM_DOMAINS.lock().unwrap_or_else(|e| e.into_inner());
    ENABLED.load(Ordering::SeqCst)
        && routes_through_proxy(host, &routing_rules(DIRECT_CDN.load(Ordering::SeqCst), &custom))
}

fn register_session(host: &str, client: &TcpStream, upstream: &TcpStream) -> Option<SessionGuard> {
    // Serialize registration against closing sessions after a policy change.
    let mut sessions = ACTIVE_SESSIONS.lock().unwrap_or_else(|e| e.into_inner());
    if !current_route_allowed(host) {
        return None;
    }
    let client = client.try_clone().ok()?;
    let upstream = upstream.try_clone().ok()?;
    let id = NEXT_SESSION_ID.fetch_add(1, Ordering::Relaxed);
    sessions.insert(
        id,
        ProxySession {
            host: host.to_owned(),
            client,
            upstream,
        },
    );
    Some(SessionGuard(id))
}

fn close_disallowed_sessions(
    sessions: &mut BTreeMap<usize, ProxySession>,
    enabled: bool,
    rules: &[(&str, bool)],
) {
    sessions.retain(|_, session| {
        if enabled && routes_through_proxy(&session.host, rules) {
            return true;
        }
        let _ = session.client.shutdown(Shutdown::Both);
        let _ = session.upstream.shutdown(Shutdown::Both);
        false
    });
}

fn disconnect_disallowed_sessions() {
    let mut sessions = ACTIVE_SESSIONS.lock().unwrap_or_else(|e| e.into_inner());
    let custom = CUSTOM_DOMAINS.lock().unwrap_or_else(|e| e.into_inner());
    let rules = routing_rules(DIRECT_CDN.load(Ordering::SeqCst), &custom);
    close_disallowed_sessions(&mut sessions, ENABLED.load(Ordering::SeqCst), &rules);
}

fn open_proxy_session(
    client: &mut TcpStream,
    host: &str,
    port: u16,
) -> Option<(TcpStream, SessionGuard)> {
    if !current_route_allowed(host) {
        let _ = client.write_all(b"HTTP/1.1 403 Forbidden\r\nConnection: close\r\nContent-Length: 0\r\n\r\n");
        return None;
    }
    let Some(upstream) = connect_upstream(host, port) else {
        let _ = client.write_all(b"HTTP/1.1 502 Bad Gateway\r\nConnection: close\r\nContent-Length: 0\r\n\r\n");
        return None;
    };
    let session = register_session(host, client, &upstream)?;
    Some((upstream, session))
}

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
    let mut connected: Option<(TcpStream, IpAddr, u32)> = None;
    for ip in ips.iter().take(2) {
        let addr = SocketAddr::new(*ip, port);
        let t0 = Instant::now();
        if let Ok(s) = TcpStream::connect_timeout(&addr, Duration::from_secs(6)) {
            let ms = t0.elapsed().as_millis().clamp(1, 9999) as u32;
            connected = Some((s, *ip, ms));
            break;
        }
    }
    match connected {
        Some((s, ip, ms)) => {
            let _ = s.set_nodelay(true);
            record(host, true, Some(ip), None, Some(ms));
            Some(s)
        }
        None => {
            record(host, false, ips.first().copied(), None, None);
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
        let (mut up, _session) = open_proxy_session(&mut c, host, port)?;
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
        let (mut up, _session) = open_proxy_session(&mut c, host, port)?;
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
    let mut latency_ms = None;
    for ip in doh_ips.iter().take(2) {
        let t0 = Instant::now();
        if TcpStream::connect_timeout(&SocketAddr::new(*ip, 443), Duration::from_secs(6)).is_ok() {
            latency_ms = Some(t0.elapsed().as_millis().clamp(1, 9999) as u32);
            ok = true;
            used_ip = Some(*ip);
            break;
        }
    }
    record(host, ok, used_ip, Some(dns), latency_ms);
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
        let extra = [
            CustomDomain { domain: "example.test".into(), enabled: true },
            CustomDomain { domain: "news.example.test".into(), enabled: false },
        ];
        let full = build_pac(true, false, &extra);
        assert!(full.contains(r#"dnsDomainIs(host,".steampowered.com")"#));
        let proxy_rule = |domain: &str| format!(r#"host=="{domain}")return "PROXY 127.0.0.1:{PORT}";"#);
        let direct_rule = |domain: &str| format!(r#"host=="{domain}")return "DIRECT";"#);
        assert!(full.contains(&proxy_rule("steamcontent.com")));
        assert!(full.contains(&proxy_rule("akamaihd.net")));
        let specific = full.find(&direct_rule("news.example.test")).unwrap();
        let broad = full.find(&proxy_rule("example.test")).unwrap();
        assert!(specific < broad);

        let direct = build_pac(true, true, &extra);
        assert!(direct.contains(&proxy_rule("steampowered.com")));
        assert!(direct.contains(&direct_rule("steamcontent.com")));
        assert!(direct.contains(&direct_rule("akamaihd.net")));
        assert!(direct.contains(&proxy_rule("example.test")));

        let off = build_pac(false, false, &extra);
        assert_eq!(off, r#"function FindProxyForURL(url,host){return "DIRECT";}"#);

        let rules = routing_rules(true, &extra);
        assert!(routes_through_proxy("www.example.test", &rules));
        assert!(!routes_through_proxy("WWW.NEWS.EXAMPLE.TEST.", &rules));
        assert!(!routes_through_proxy("example.invalid", &rules));
        assert!(!routes_through_proxy("notexample.test", &rules));
        assert!(!routes_through_proxy("cdn.steamcontent.com", &rules));
        assert!(routes_through_proxy("store.steampowered.com", &rules));
    }

    #[test]
    fn turning_off_a_domain_closes_only_its_sessions() {
        let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let addr = listener.local_addr().unwrap();
        let socket_pair = || {
            let peer = TcpStream::connect(addr).unwrap();
            peer.set_read_timeout(Some(Duration::from_secs(2))).unwrap();
            (listener.accept().unwrap().0, peer)
        };
        let make_session = |host: &str| {
            let (client, client_peer) = socket_pair();
            let (upstream, upstream_peer) = socket_pair();
            (
                ProxySession { host: host.into(), client, upstream },
                client_peer,
                upstream_peer,
            )
        };
        let (disabled, mut disabled_client, mut disabled_upstream) = make_session("www.news.example.test");
        let (allowed, mut allowed_client, mut allowed_upstream) = make_session("www.example.test");
        // The relay keeps its own handles after a session is removed from the registry.
        let _relay_sockets = (disabled.client.try_clone().unwrap(), disabled.upstream.try_clone().unwrap());
        let mut sessions = BTreeMap::from([(1, disabled), (2, allowed)]);
        let rules = [("news.example.test", false), ("example.test", true)];
        close_disallowed_sessions(&mut sessions, true, &rules);

        let mut byte = [0];
        assert_eq!(disabled_client.read(&mut byte).unwrap(), 0);
        assert_eq!(disabled_upstream.read(&mut byte).unwrap(), 0);
        let allowed = sessions.get_mut(&2).unwrap();
        allowed.client.write_all(b"a").unwrap();
        allowed.upstream.write_all(b"b").unwrap();
        allowed_client.read_exact(&mut byte).unwrap();
        assert_eq!(&byte, b"a");
        allowed_upstream.read_exact(&mut byte).unwrap();
        assert_eq!(&byte, b"b");
        assert_eq!(sessions.len(), 1);
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
    fn parses_adguard_and_dnssb_payloads_and_provider_mapping() {
        let adguard = r#"{"Question":[{"name":"store.steampowered.com.","type":1}],"Answer":[{"name":"store.steampowered.com.","data":"118.68.82.144","TTL":60,"type":1,"class":1}],"Status":0}"#;
        assert_eq!(parse_doh_a_record(adguard), Some(IpAddr::V4(Ipv4Addr::new(118, 68, 82, 144))));

        let dnssb = r#"{"Status":0,"Answer":[{"name":"store.steampowered.com.","type":1,"TTL":20,"data":"23.35.102.114"}]}"#;
        assert_eq!(parse_doh_a_record(dnssb), Some(IpAddr::V4(Ipv4Addr::new(23, 35, 102, 114))));

        assert_eq!(DohProvider::from_id("cloudflare"), DohProvider::Cloudflare);
        assert_eq!(DohProvider::from_id("GOOGLE"), DohProvider::Google);
        assert_eq!(DohProvider::from_id("adguard"), DohProvider::AdGuard);
        assert_eq!(DohProvider::from_id("dnssb"), DohProvider::Custom);
        assert_eq!(DohProvider::from_id("unknown"), DohProvider::Cloudflare);
        for (i, p) in DohProvider::ALL.iter().enumerate() {
            assert_eq!(p.index(), i);
            assert_eq!(p.endpoints().is_empty(), *p == DohProvider::Custom);
        }
    }

    #[test]
    fn custom_endpoint_and_dns_message() {
        assert_eq!(parse_endpoint("https://[::1]:8443/my/profile"), Some(("::1", 8443, "/my/profile")));
        assert!(parse_endpoint("http://resolver.test/dns-query").is_none());
        assert!(parse_endpoint("https://user@resolver.test/dns-query").is_none());
        let mut reply = dns_question("example.org").unwrap();
        reply[2] = 0x81;
        reply[7] = 1;
        reply.extend_from_slice(&[192, 12, 0, 1, 0, 1, 0, 0, 0, 60, 0, 4, 1, 2, 3, 4]);
        assert_eq!(wire_addresses(&reply), Some(vec![IpAddr::V4(Ipv4Addr::new(1,2,3,4))]));
        reply.pop();
        assert!(wire_addresses(&reply).is_none());
    }

    #[test]
    fn reports_certificate_timeout_and_invalid_doh_response() {
        assert_eq!(DohError::from_winhttp(ERROR_WINHTTP_SECURE_INVALID_CA), DohError::Certificate);
        assert_eq!(DohError::from_winhttp(ERROR_WINHTTP_TIMEOUT), DohError::Timeout);
        assert_eq!(DohError::from_winhttp(ERROR_WINHTTP_INVALID_SERVER_RESPONSE), DohError::InvalidResponse);
        assert!(json_doh_addresses(b"<html></html>").is_none());
        assert!(json_doh_addresses(br#"{"Status":0,"Answer":"invalid"}"#).is_none());
        assert_eq!(json_doh_addresses(br#"{"Status":3}"#), Some(Vec::new()));
        assert_eq!(test_custom_doh("http://example.org/dns-query"), Err(DohError::InvalidUrl));
    }

    #[test]
    #[ignore = "requires network"]
    fn live_custom_doh_probe_supports_wire_and_json() {
        for url in ["https://dns.google/dns-query", "https://dns.google/resolve"] {
            assert!(test_custom_doh(url).unwrap() > 0, "{url}");
        }
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
