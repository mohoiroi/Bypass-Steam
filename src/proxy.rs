//! Local HTTP/HTTPS proxy with DNS-over-HTTPS (WinHTTP) and TLS ClientHello splitting.

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

pub const APP_TITLE: &str = "Steam Bypass";
pub const PORT: u16 = 8899;
pub const WM_APP_STATS: u32 = WM_APP + 2;

const PAC_URL: &str = "http://127.0.0.1:8899/proxy.pac";
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

pub const GROUPS: [(&str, &str); 6] = [
    ("Cửa hàng", "Store, giỏ hàng, khuyến mãi"),
    ("Cộng đồng", "Hồ sơ, bạn bè, workshop"),
    ("Đăng nhập & API", "Tài khoản, Steam Guard, API"),
    ("Tải game (CDN)", "Máy chủ tải game & cập nhật"),
    ("Hình ảnh & media", "Ảnh, avatar, video"),
    ("Khác", "Tên miền Valve khác"),
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

fn group_of(host: &str) -> usize {
    let h = host.to_ascii_lowercase();
    if h == "store.steampowered.com" || h.ends_with(".store.steampowered.com") {
        0
    } else if h.ends_with("steamcommunity.com") {
        1
    } else if h.ends_with("steampowered.com") || h.ends_with("steam-api.com") {
        2
    } else if h.ends_with("steamcontent.com") || h.ends_with("akamaihd.net") || h.ends_with("steamserver.net") {
        3
    } else if h.ends_with("steamstatic.com") || h.ends_with("steamusercontent.com") || h.ends_with("steam.tv") {
        4
    } else {
        5
    }
}

#[derive(Default, Clone, Copy, PartialEq, Eq)]
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
pub static ENABLED: AtomicBool = AtomicBool::new(true);
pub static PENDING: AtomicUsize = AtomicUsize::new(0);

static UI_HWND: AtomicIsize = AtomicIsize::new(0);
pub static UI_DIRTY: AtomicBool = AtomicBool::new(false);

type DnsCache = HashMap<String, (IpAddr, Instant)>;
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
    let mut m = STATS.lock().unwrap_or_else(|e| e.into_inner());
    for h in CATALOG {
        m.entry((*h).to_string()).or_insert(Stat {
            group: group_of(h),
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

fn query_doh_server(server: &str, endpoint: &str, host: &str) -> Option<IpAddr> {
    let session = winhttp_session()?;
    let srv_w = wide(server);
    let verb_w = wide("GET");
    let path_w = wide(&format!("{endpoint}?name={host}&type=A"));
    let hdr_w = wide("Accept: application/dns-json\r\n");

    unsafe {
        let conn = WinHttpConnect(session, srv_w.as_ptr(), 443, 0);
        if conn.is_null() {
            return None;
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
            return None;
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

        let text = std::str::from_utf8(&body).ok()?;
        parse_doh_a_record(text)
    }
}

pub fn resolve_doh(host: &str) -> Option<IpAddr> {
    if let Ok(ip) = host.parse::<IpAddr>() {
        return Some(ip);
    }
    if host.is_empty()
        || !host
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'.' || b == b'-')
    {
        return None;
    }
    if let Some(map) = CACHE.lock().unwrap_or_else(|e| e.into_inner()).as_ref() {
        if let Some((ip, t)) = map.get(host) {
            if t.elapsed() < Duration::from_secs(300) {
                return Some(*ip);
            }
        }
    }

    let ip = query_doh_server("1.1.1.1", "/dns-query", host)
        .or_else(|| query_doh_server("1.0.0.1", "/dns-query", host))
        .or_else(|| query_doh_server("8.8.8.8", "/resolve", host))?;
    CACHE
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .get_or_insert_with(HashMap::new)
        .insert(host.to_string(), (ip, Instant::now()));
    Some(ip)
}

/// Minimal dependency-free parser for Cloudflare DoH DNS-JSON `"Answer"` A records (`type: 1`).
pub(crate) fn parse_doh_a_record(body: &str) -> Option<IpAddr> {
    let ans_pos = find_json_key(body, "Answer")?;
    let rest = body[ans_pos..].trim_start();
    if !rest.starts_with('[') {
        return None;
    }
    let arr_end = find_matching_delim(rest, b'[', b']')?;
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
                            return Some(IpAddr::V4(ip));
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
    None
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
fn pac(enabled: bool) -> String {
    if !enabled {
        return "function FindProxyForURL(url,host){return \"DIRECT\";}".to_string();
    }
    let conds: Vec<String> = DOMAINS
        .iter()
        .map(|d| format!("dnsDomainIs(host,\".{d}\")||host==\"{d}\""))
        .collect();
    format!(
        "function FindProxyForURL(url,host){{if({}){{return \"PROXY 127.0.0.1:{PORT}\";}}return \"DIRECT\";}}",
        conds.join("||")
    )
}

pub fn set_pac(enable: bool) {
    let key_w = wide(IE_KEY);
    let name_w = wide("AutoConfigURL");
    unsafe {
        let mut hkey: HKEY = std::ptr::null_mut();
        let rc = RegCreateKeyW(HKEY_CURRENT_USER, key_w.as_ptr(), &mut hkey);
        if rc == 0 && !hkey.is_null() {
            if enable {
                let val_w = wide(PAC_URL);
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

pub fn set_enabled(on: bool) {
    ENABLED.store(on, Ordering::SeqCst);
    set_pac(on);
    notify_ui();
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
    let ip = resolve_doh(host);
    let addr = ip.map(|ip| SocketAddr::new(ip, port));
    let res = addr.and_then(|a| TcpStream::connect_timeout(&a, Duration::from_secs(8)).ok());
    record(host, res.is_some(), ip, None);
    let s = res?;
    let _ = s.set_nodelay(true);
    Some(s)
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
    } else if target.starts_with("/proxy.pac") || target.ends_with("/proxy.pac") {
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
        if let Some(crlf) = buf[..n].windows(2).position(|w| w == b"\r\n") {
            let new_req_line = format!("{method} {path} {version}");
            up.write_all(new_req_line.as_bytes()).ok()?;
            up.write_all(&buf[crlf..n]).ok()?;
        } else {
            up.write_all(&buf[..n]).ok()?;
        }
        relay_bidirectional(c, up);
    }
    Some(())
}

/// Active check of every catalog host: ISP DNS vs DoH, then a real TCP connect.
pub fn run_test() {
    if PENDING
        .compare_exchange(0, CATALOG.len(), Ordering::SeqCst, Ordering::SeqCst)
        .is_err()
    {
        return;
    }
    notify_ui();

    for host in CATALOG {
        let _ = thread::Builder::new()
            .stack_size(512 * 1024)
            .spawn(move || {
                let sys = (*host, 443u16)
                    .to_socket_addrs()
                    .ok()
                    .and_then(|mut it| it.find(|a| a.is_ipv4()))
                    .map(|a| a.ip());
                let doh = resolve_doh(host);
                let dns = match (sys, doh) {
                    (None, Some(_)) => DnsState::Failed,
                    (Some(a), Some(b)) if a == b => DnsState::Clean,
                    (Some(_), Some(_)) => DnsState::Poisoned,
                    _ => DnsState::Unknown,
                };
                let ok = match doh {
                    Some(ip) => TcpStream::connect_timeout(&SocketAddr::new(ip, 443), Duration::from_secs(6)).is_ok(),
                    None => false,
                };
                record(host, ok, doh, Some(dns));
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
