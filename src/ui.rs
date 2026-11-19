//! Pure Win32 + GDI+ window, system tray, and compact accordion UI.

use crate::gfx::{Canvas, FontSet, RectF};
use crate::proxy::{self, APP_TITLE, DnsState, ENABLED, GROUPS, PENDING, PORT, Stat, UI_DIRTY, WM_APP_STATS, wide};
use crate::theme::*;
use std::ptr::{null, null_mut};
use std::sync::atomic::Ordering;
use windows_sys::Win32::{
    Foundation::{HWND, LPARAM, LRESULT, POINT, RECT, WPARAM},
    Graphics::{Dwm::*, Gdi::*},
    System::LibraryLoader::GetModuleHandleW,
    UI::{
        HiDpi::{AdjustWindowRectExForDpi, GetDpiForWindow},
        Input::KeyboardAndMouse::{TME_LEAVE, TRACKMOUSEEVENT, TrackMouseEvent},
        Shell::*,
        WindowsAndMessaging::*,
    },
};

const WM_MOUSELEAVE: u32 = 0x02A3;

pub const WND_CLASS: &str = "SteamBypassMainWnd";
pub const WM_APP_SHOW: u32 = WM_APP + 3;

const WM_TRAY: u32 = WM_APP + 1;
const ID_TRAY_ICON: u32 = 1;
const ID_TIMER_ANIM: usize = 1;

const ID_MENU_OPEN: usize = 1;
const ID_MENU_TOGGLE: usize = 2;
const ID_MENU_TEST: usize = 3;
const ID_MENU_EXIT: usize = 4;

#[derive(Clone, Copy, PartialEq, Eq)]
enum Action {
    ToggleBypass,
    RunTest,
    ToggleGroup(usize),
    QuitApp,
}

struct UiState {
    dpi: u32,
    fonts: FontSet,
    hover: Option<Action>,
    tracking_mouse: bool,
    cursor_hand: bool,
    expanded: [bool; 6],
    scroll_y: f32,
    max_scroll: f32,
    services_rect: RectF,
    toggle_t: f32,
    spin_deg: f32,
    anim_timer: bool,
    hits: Vec<(RectF, Action)>,
    taskbar_msg: u32,
    last_tray_on: bool,
}

impl UiState {
    fn new(dpi: u32, on: bool, taskbar_msg: u32) -> Self {
        Self {
            dpi,
            fonts: FontSet::new(dpi),
            hover: None,
            tracking_mouse: false,
            cursor_hand: false,
            expanded: [false; 6],
            scroll_y: 0.0,
            max_scroll: 0.0,
            services_rect: RectF::default(),
            toggle_t: if on { 1.0 } else { 0.0 },
            spin_deg: 0.0,
            anim_timer: false,
            hits: Vec::with_capacity(16),
            taskbar_msg,
            last_tray_on: on,
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Health {
    Ok,
    Partial,
    Fail,
    Testing,
    Idle,
}

fn host_health(s: &Stat, pending: usize) -> Health {
    if s.ok == 0 && s.fail == 0 {
        if pending > 0 { Health::Testing } else { Health::Idle }
    } else if s.last_ok || (s.ok > 0 && s.fail == 0) {
        Health::Ok
    } else {
        Health::Fail
    }
}

fn group_health(rows: &[&(String, Stat)], pending: usize) -> (Health, usize, usize, bool) {
    let mut ok = 0usize;
    let mut fail = 0usize;
    let mut dns_fixed = false;
    for (_, s) in rows {
        match host_health(s, pending) {
            Health::Ok => ok += 1,
            Health::Fail => fail += 1,
            _ => {}
        }
        if matches!(s.dns, DnsState::Poisoned | DnsState::Failed) {
            dns_fixed = true;
        }
    }
    let h = if fail > 0 && ok == 0 {
        Health::Fail
    } else if fail > 0 {
        Health::Partial
    } else if ok > 0 {
        Health::Ok
    } else if pending > 0 {
        Health::Testing
    } else {
        Health::Idle
    };
    (h, ok, fail, dns_fixed)
}

fn health_badge(h: Health) -> (&'static str, Color, Color) {
    match h {
        Health::Ok => ("Đã bypass", SUCCESS, tint(SUCCESS)),
        Health::Partial => ("Một phần", WARNING, tint(WARNING)),
        Health::Fail => ("Bị chặn", DANGER, tint(DANGER)),
        Health::Testing => ("Đang thử", ACCENT, tint(ACCENT)),
        Health::Idle => ("Chờ…", NEUTRAL, NEUTRAL.with_alpha(30)),
    }
}

fn host_dot_color(h: Health) -> Color {
    match h {
        Health::Ok => SUCCESS,
        Health::Partial => WARNING,
        Health::Testing => ACCENT,
        Health::Fail => DANGER,
        Health::Idle => NEUTRAL,
    }
}

// ---------------- Pure UI Rendering ----------------
fn paint_ui(
    hdc: HDC,
    phys_w: i32,
    phys_h: i32,
    state: &mut UiState,
    snap: &[(String, Stat)],
    on: bool,
    pending: usize,
) {
    let scale = state.dpi as f32 / 96.0;
    let lw = (phys_w as f32 / scale).max(320.0);
    let lh = (phys_h as f32 / scale).max(300.0);
    state.hits.clear();

    let mut c = Canvas::new(hdc, state.dpi);

    // Window background
    c.fill_rect(RectF::new(0.0, 0.0, lw, lh), BG);

    let cx = PAD_X;
    let cw = (lw - PAD_X * 2.0).max(200.0);

    // 1. Hero Control Card
    let hero = RectF::new(cx, 14.0, cw, 76.0);
    c.fill_round_rect(hero, R_CARD, SURFACE);
    if state.toggle_t > 0.01 {
        c.fill_round_rect(hero, R_CARD, SUCCESS.with_alpha((12.0 * state.toggle_t) as u8));
    }
    let hero_border = if on {
        Color::lerp(BORDER, SUCCESS, 0.25)
    } else {
        BORDER
    };
    c.stroke_round_rect(hero, R_CARD, 1.0, hero_border);

    // Logo + live dot
    let lx = hero.x + 14.0;
    let ly = hero.y + 18.0;
    c.draw_logo(lx, ly, 40.0);
    let dot_col = if on { SUCCESS } else { NEUTRAL };
    c.fill_circle(lx + 38.0, ly + 38.0, 6.5, SURFACE);
    c.fill_circle(lx + 38.0, ly + 38.0, 4.5, dot_col);

    // Toggle switch (right)
    let sw_w = 48.0;
    let sw_h = 26.0;
    let sw_x = hero.right() - 14.0 - sw_w;
    let sw_y = hero.cy() - sw_h * 0.5;
    let sw_rect = RectF::new(sw_x, sw_y, sw_w, sw_h);
    let sw_hovered = state.hover == Some(Action::ToggleBypass);
    let mut track = Color::lerp(NEUTRAL, ACCENT, state.toggle_t);
    if sw_hovered {
        track = Color::lerp(track, WHITE, 0.14);
    }
    c.fill_round_rect(sw_rect, 13.0, track);
    let knob_x = sw_x + 13.0 + (sw_w - 26.0) * state.toggle_t;
    c.fill_circle(knob_x, sw_y + 13.0, 10.0, WHITE);
    state.hits.push((sw_rect.inset(-4.0, -6.0), Action::ToggleBypass));

    // Hero title + inline pill + subtitle
    let tx = lx + 54.0;
    let max_tx = sw_x - 12.0;
    let (title_w, _) = c.measure_text(APP_TITLE, state.fonts.title);
    c.draw_text(
        APP_TITLE,
        RectF::new(tx, hero.y + 17.0, (max_tx - tx).max(40.0), 22.0),
        state.fonts.title,
        TEXT,
        DT_LEFT,
    );
    let pill_x = tx + title_w + 10.0;
    if pill_x + 74.0 <= max_tx {
        let (plabel, pfg, pbg, pdot) = if on {
            ("Đang bật", SUCCESS, tint(SUCCESS), SUCCESS)
        } else {
            ("Đã tắt", TEXT_MUTED, NEUTRAL.with_alpha(36), NEUTRAL)
        };
        c.draw_pill(pill_x, hero.y + 28.0, plabel, pfg, pbg, Some(pdot), state.fonts.badge, false);
    }

    let ok_total = snap.iter().filter(|(_, s)| host_health(s, pending) == Health::Ok).count();
    let fail_total = snap.iter().filter(|(_, s)| host_health(s, pending) == Health::Fail).count();
    let dns_fixed_total = snap
        .iter()
        .filter(|(_, s)| matches!(s.dns, DnsState::Poisoned | DnsState::Failed))
        .count();
    let conns_total: u32 = snap.iter().map(|(_, s)| s.ok).sum();

    let hero_sub = if pending > 0 {
        "Đang kiểm tra kết nối tới các máy chủ Steam…".to_string()
    } else if !on {
        "Đang kết nối trực tiếp · Không dùng proxy".to_string()
    } else if fail_total > 0 {
        format!("{ok_total}/{} tên miền hoạt động · {fail_total} lỗi", snap.len())
    } else if ok_total > 0 && dns_fixed_total > 0 {
        format!("{ok_total}/{} tên miền hoạt động · {dns_fixed_total} DNS đã sửa", snap.len())
    } else if ok_total > 0 {
        format!("{ok_total}/{} tên miền hoạt động · DoH & chia gói TLS", snap.len())
    } else {
        "Proxy cục bộ đang sẵn sàng · Chờ kết nối từ Steam".to_string()
    };
    c.draw_text(
        &hero_sub,
        RectF::new(tx, hero.y + 42.0, (max_tx - tx).max(40.0), 18.0),
        state.fonts.caption,
        TEXT_MUTED,
        DT_LEFT,
    );

    // 2. Action & Summary Bar
    let bar_y = 102.0;
    let bar_h = 32.0;
    let bar_cy = bar_y + bar_h * 0.5;
    let sec_title = "Thành phần Steam";
    let (sec_w, _) = c.measure_text(sec_title, state.fonts.group);
    c.draw_text(
        sec_title,
        RectF::new(cx, bar_y, sec_w + 4.0, bar_h),
        state.fonts.group,
        TEXT_SEC,
        DT_LEFT,
    );

    let sum_col = if pending > 0 {
        ACCENT
    } else if fail_total > 0 && ok_total == 0 {
        DANGER
    } else if fail_total > 0 {
        WARNING
    } else if ok_total > 0 {
        SUCCESS
    } else {
        TEXT_DIM
    };
    let frac = format!("{ok_total}/{}", snap.len());
    let (frac_w, _) = c.measure_text(&frac, state.fonts.mono);
    let sum_x = cx + sec_w + 10.0;
    c.draw_text(
        &frac,
        RectF::new(sum_x, bar_y, frac_w + 4.0, bar_h),
        state.fonts.mono,
        sum_col,
        DT_LEFT,
    );
    if conns_total > 0 {
        let conn_str = format!("· {conns_total} lượt");
        c.draw_text(
            &conn_str,
            RectF::new(sum_x + frac_w + 4.0, bar_y, 90.0, bar_h),
            state.fonts.caption,
            TEXT_DIM,
            DT_LEFT,
        );
    }

    // "Kiểm tra" button (right)
    let btn_w = 104.0;
    let btn_h = 30.0;
    let btn = RectF::new(cx + cw - btn_w, bar_cy - btn_h * 0.5, btn_w, btn_h);
    let btn_hov = pending == 0 && state.hover == Some(Action::RunTest);
    let btn_fill = if pending > 0 {
        SURFACE_INSET
    } else if btn_hov {
        BTN_HOVER
    } else {
        BTN_BG
    };
    let btn_border = if btn_hov { ACCENT.with_alpha(130) } else { BORDER };
    c.fill_round_rect(btn, R_BTN, btn_fill);
    c.stroke_round_rect(btn, R_BTN, 1.0, btn_border);

    let (btn_txt, txt_col) = if pending > 0 {
        ("Đang thử…", TEXT_MUTED)
    } else {
        ("Kiểm tra", TEXT)
    };
    let (btw, _) = c.measure_text(btn_txt, state.fonts.caption);
    let icon_d = 11.0;
    let gap = 7.0;
    let start_x = btn.x + ((btn.w - (icon_d + gap + btw)) * 0.5).max(8.0);
    let icon_cx = start_x + icon_d * 0.5;
    let icon_cy = btn.cy();
    if pending > 0 {
        c.stroke_circle(icon_cx, icon_cy, 5.2, 1.6, BORDER);
        c.draw_arc(icon_cx, icon_cy, 5.2, state.spin_deg, 240.0, 1.8, ACCENT);
    } else {
        let ic_col = if btn_hov { ACCENT_HOVER } else { TEXT_SEC };
        c.draw_arc(icon_cx, icon_cy, 5.0, -35.0, 275.0, 1.6, ic_col);
        c.draw_line(icon_cx + 3.6, icon_cy - 5.0, icon_cx + 4.4, icon_cy - 2.2, 1.5, ic_col, true);
        c.draw_line(icon_cx + 3.6, icon_cy - 5.0, icon_cx + 0.9, icon_cy - 3.5, 1.5, ic_col, true);
        state.hits.push((btn, Action::RunTest));
    }
    c.draw_text(
        btn_txt,
        RectF::new(start_x + icon_d + gap, btn.y, btw + 4.0, btn.h),
        state.fonts.caption,
        txt_col,
        DT_LEFT,
    );

    // 3. Steam Services Accordion Card
    let card_y = 144.0;
    let card_h = (lh - 44.0 - card_y).max(120.0);
    let card = RectF::new(cx, card_y, cw, card_h);
    c.fill_round_rect(card, R_CARD, SURFACE);
    c.stroke_round_rect(card, R_CARD, 1.0, BORDER);

    let inner = RectF::new(card.x + 1.0, card.y + 4.0, card.w - 2.0, card.h - 8.0);
    state.services_rect = inner;

    let visible_groups: Vec<(usize, Vec<&(String, Stat)>)> = (0..GROUPS.len())
        .filter_map(|gi| {
            let rows: Vec<&(String, Stat)> = snap.iter().filter(|(_, s)| s.group == gi).collect();
            if rows.is_empty() { None } else { Some((gi, rows)) }
        })
        .collect();

    const GROUP_H: f32 = 56.0;
    const HOST_H: f32 = 34.0;
    let content_h: f32 = visible_groups
        .iter()
        .map(|(gi, rows)| {
            GROUP_H + if state.expanded[*gi] { rows.len() as f32 * HOST_H } else { 0.0 }
        })
        .sum();
    state.max_scroll = (content_h - inner.h).max(0.0);
    state.scroll_y = state.scroll_y.clamp(0.0, state.max_scroll);

    let scroll_pad = if state.max_scroll > 0.5 { 8.0 } else { 0.0 };
    c.set_clip(Some(inner));

    let mut y = inner.y - state.scroll_y;
    for (idx, (gi, rows)) in visible_groups.iter().enumerate() {
        let gh_rect = RectF::new(inner.x, y, inner.w, GROUP_H);
        if idx > 0 && y >= inner.y - 2.0 && y <= inner.bottom() + 2.0 {
            c.draw_line(
                inner.x + 14.0,
                y,
                inner.right() - 14.0 - scroll_pad,
                y,
                1.0,
                BORDER_SUB,
                false,
            );
        }

        let is_exp = state.expanded[*gi];
        let is_hov = state.hover == Some(Action::ToggleGroup(*gi));
        if gh_rect.bottom() > inner.y && gh_rect.y < inner.bottom() {
            if is_hov {
                c.fill_round_rect(
                    RectF::new(inner.x + 6.0, y + 3.0, inner.w - 12.0 - scroll_pad, GROUP_H - 6.0),
                    R_SM,
                    SURFACE_HOVER,
                );
            }
            // Clip hit box to visible services area
            let hit_top = gh_rect.y.max(inner.y);
            let hit_bot = gh_rect.bottom().min(inner.bottom());
            if hit_bot > hit_top {
                state.hits.push((
                    RectF::new(inner.x, hit_top, inner.w - scroll_pad, hit_bot - hit_top),
                    Action::ToggleGroup(*gi),
                ));
            }

            let (gh, ok_c, _fail_c, dns_fixed) = group_health(rows, pending);
            let dot_c = host_dot_color(gh);
            let gcy = y + GROUP_H * 0.5;
            let gx = inner.x + 14.0;

            c.draw_chevron(gx + 4.0, gcy, 3.5, is_exp, TEXT_MUTED);
            if gh == Health::Ok {
                c.fill_circle(gx + 20.0, gcy, 7.0, SUCCESS.with_alpha(38));
            }
            c.fill_circle(gx + 20.0, gcy, 4.0, dot_c);

            // Right pill + count
            let (blabel, bfg, bbg) = health_badge(gh);
            let pill_rect = c.draw_pill(
                inner.right() - 14.0 - scroll_pad,
                gcy,
                blabel,
                bfg,
                bbg,
                None,
                state.fonts.badge,
                true,
            );
            let frac = format!("{ok_c}/{}", rows.len());
            let (fw, _) = c.measure_text(&frac, state.fonts.mono);
            c.draw_text(
                &frac,
                RectF::new(pill_rect.x - fw - 10.0, y, fw + 4.0, GROUP_H),
                state.fonts.mono,
                TEXT_DIM,
                DT_RIGHT,
            );

            // Left title + desc
            let text_left = gx + 34.0;
            let text_right = (pill_rect.x - fw - 18.0).max(text_left + 40.0);
            let (gname, gdesc) = GROUPS[*gi];
            c.draw_text(
                gname,
                RectF::new(text_left, y + 9.0, text_right - text_left, 20.0),
                state.fonts.group,
                TEXT,
                DT_LEFT,
            );
            let sub = if dns_fixed {
                format!("{gdesc} · DNS đã sửa")
            } else {
                gdesc.to_string()
            };
            c.draw_text(
                &sub,
                RectF::new(text_left, y + 29.0, text_right - text_left, 17.0),
                state.fonts.caption,
                TEXT_MUTED,
                DT_LEFT,
            );
        }
        y += GROUP_H;

        if is_exp {
            let block_h = rows.len() as f32 * HOST_H;
            let block_rect = RectF::new(inner.x, y, inner.w, block_h);
            if block_rect.bottom() > inner.y && block_rect.y < inner.bottom() {
                c.fill_rect(block_rect, SURFACE_INSET);
                c.draw_line(inner.x, y, inner.right(), y, 1.0, BORDER_SUB, false);
            }
            for (hi, (host, s)) in rows.iter().enumerate() {
                let hr = RectF::new(inner.x, y, inner.w, HOST_H);
                if hr.bottom() > inner.y && hr.y < inner.bottom() {
                    if hi > 0 {
                        c.draw_line(
                            inner.x + 24.0,
                            y,
                            inner.right() - 10.0 - scroll_pad,
                            y,
                            1.0,
                            BORDER_SUB,
                            false,
                        );
                    }
                    let hcy = y + HOST_H * 0.5;
                    let hh = host_health(s, pending);
                    c.fill_circle(inner.x + 24.0, hcy, 3.0, host_dot_color(hh));

                    // Right-aligned metadata
                    let mut rx = inner.right() - 10.0 - scroll_pad;
                    if s.ok > 0 {
                        let cnt = format!("×{}", s.ok);
                        let (cw_txt, _) = c.measure_text(&cnt, state.fonts.caption);
                        c.draw_text(
                            &cnt,
                            RectF::new(rx - cw_txt - 2.0, y, cw_txt + 2.0, HOST_H),
                            state.fonts.caption,
                            TEXT_DIM,
                            DT_RIGHT,
                        );
                        rx -= cw_txt + 6.0;
                    }

                    let ip = if s.ip.is_empty() { "—" } else { &s.ip };
                    let (ip_w, _) = c.measure_text(ip, state.fonts.mono);
                    c.draw_text(
                        ip,
                        RectF::new(rx - ip_w - 2.0, y, ip_w + 2.0, HOST_H),
                        state.fonts.mono,
                        TEXT_MUTED,
                        DT_RIGHT,
                    );
                    rx -= ip_w + 7.0;

                    match s.dns {
                        DnsState::Poisoned | DnsState::Failed => {
                            let tag = "DNS sửa";
                            let (tw, _) = c.measure_text(tag, state.fonts.badge);
                            c.draw_text(
                                tag,
                                RectF::new(rx - tw - 2.0, y, tw + 2.0, HOST_H),
                                state.fonts.badge,
                                WARNING,
                                DT_RIGHT,
                            );
                            rx -= tw + 7.0;
                        }
                        DnsState::Clean => {
                            let tag = "DNS OK";
                            let (tw, _) = c.measure_text(tag, state.fonts.caption);
                            c.draw_text(
                                tag,
                                RectF::new(rx - tw - 2.0, y, tw + 2.0, HOST_H),
                                state.fonts.caption,
                                TEXT_DIM,
                                DT_RIGHT,
                            );
                            rx -= tw + 7.0;
                        }
                        DnsState::Unknown => {}
                    }

                    let host_left = inner.x + 35.0;
                    c.draw_text(
                        host,
                        RectF::new(host_left, y, (rx - host_left - 4.0).max(40.0), HOST_H),
                        state.fonts.body,
                        TEXT_SEC,
                        DT_LEFT,
                    );
                }
                y += HOST_H;
            }
        }
    }

    // Slim scrollbar thumb
    if state.max_scroll > 0.5 && content_h > inner.h {
        let track_h = (inner.h - 8.0).max(20.0);
        let thumb_h = ((inner.h / content_h) * track_h).clamp(24.0, track_h);
        let ratio = state.scroll_y / state.max_scroll;
        let thumb_y = inner.y + 4.0 + ratio * (track_h - thumb_h);
        c.fill_round_rect(
            RectF::new(inner.right() - 7.0, thumb_y, 4.0, thumb_h),
            2.0,
            WHITE.with_alpha(52),
        );
    }

    c.set_clip(None);

    // 4. Minimal Footer
    let foot_y = lh - 34.0;
    let foot_h = 24.0;
    let quit_w = 50.0;
    let quit_h = 22.0;
    let quit_rect = RectF::new(cx + cw - quit_w, foot_y + (foot_h - quit_h) * 0.5, quit_w, quit_h);
    let quit_hov = state.hover == Some(Action::QuitApp);
    if quit_hov {
        c.fill_round_rect(quit_rect, R_SM, DANGER.with_alpha(30));
    }
    c.draw_text(
        "Thoát",
        quit_rect,
        state.fonts.caption,
        if quit_hov { DANGER } else { TEXT_MUTED },
        DT_CENTER,
    );
    state.hits.push((quit_rect, Action::QuitApp));

    let hint = "Đóng (×) để chạy nền";
    let (hint_w, _) = c.measure_text(hint, state.fonts.caption);
    c.draw_text(
        hint,
        RectF::new(quit_rect.x - hint_w - 10.0, foot_y, hint_w + 4.0, foot_h),
        state.fonts.caption,
        TEXT_DIM,
        DT_RIGHT,
    );

    let left_info = format!("127.0.0.1:{PORT} · DoH 1.1.1.1");
    let left_max = (quit_rect.x - hint_w - 18.0 - cx).max(60.0);
    c.draw_text(
        &left_info,
        RectF::new(cx, foot_y, left_max, foot_h),
        state.fonts.caption,
        TEXT_DIM,
        DT_LEFT,
    );
}

// ---------------- Win32 Window & Tray ----------------
fn get_state<'a>(hwnd: HWND) -> Option<&'a mut UiState> {
    let ptr = unsafe { GetWindowLongPtrW(hwnd, GWLP_USERDATA) } as *mut UiState;
    if ptr.is_null() { None } else { Some(unsafe { &mut *ptr }) }
}

fn sync_timer(hwnd: HWND, state: &mut UiState) {
    let target = if ENABLED.load(Ordering::SeqCst) { 1.0 } else { 0.0 };
    let need = (state.toggle_t - target).abs() > 0.01 || PENDING.load(Ordering::SeqCst) > 0;
    unsafe {
        if need && !state.anim_timer {
            SetTimer(hwnd, ID_TIMER_ANIM, 16, None);
            state.anim_timer = true;
        } else if !need && state.anim_timer {
            KillTimer(hwnd, ID_TIMER_ANIM);
            state.anim_timer = false;
        }
    }
}

fn hit_test(state: &UiState, lx: f32, ly: f32) -> Option<Action> {
    state
        .hits
        .iter()
        .rev()
        .find(|(r, _)| r.contains(lx, ly))
        .map(|(_, a)| *a)
}

fn tray_tip(on: bool) -> String {
    format!("Steam Bypass — {}", if on { "Đang bật" } else { "Đã tắt" })
}

unsafe fn build_tray_data(hwnd: HWND, on: bool) -> NOTIFYICONDATAW {
    let mut nid: NOTIFYICONDATAW = unsafe { std::mem::zeroed() };
    nid.cbSize = std::mem::size_of::<NOTIFYICONDATAW>() as u32;
    nid.hWnd = hwnd;
    nid.uID = ID_TRAY_ICON;
    nid.uFlags = NIF_ICON | NIF_MESSAGE | NIF_TIP;
    nid.uCallbackMessage = WM_TRAY;
    unsafe {
        let hinst = GetModuleHandleW(null());
        let cx = GetSystemMetrics(SM_CXSMICON);
        let cy = GetSystemMetrics(SM_CYSMICON);
        let h = LoadImageW(hinst, 1usize as *const u16, IMAGE_ICON, cx, cy, LR_DEFAULTCOLOR);
        nid.hIcon = if !h.is_null() {
            h as HICON
        } else {
            LoadIconW(null_mut(), IDI_APPLICATION)
        };
    }
    let tip = tray_tip(on);
    for (i, ch) in tip.encode_utf16().take(127).enumerate() {
        nid.szTip[i] = ch;
    }
    nid
}

fn add_tray(hwnd: HWND) {
    unsafe {
        let nid = build_tray_data(hwnd, ENABLED.load(Ordering::SeqCst));
        Shell_NotifyIconW(NIM_ADD, &nid);
        if !nid.hIcon.is_null() {
            DestroyIcon(nid.hIcon);
        }
    }
}

fn update_tray_tip(hwnd: HWND) {
    unsafe {
        let nid = build_tray_data(hwnd, ENABLED.load(Ordering::SeqCst));
        Shell_NotifyIconW(NIM_MODIFY, &nid);
        if !nid.hIcon.is_null() {
            DestroyIcon(nid.hIcon);
        }
    }
}

fn delete_tray(hwnd: HWND) {
    unsafe {
        let mut nid: NOTIFYICONDATAW = std::mem::zeroed();
        nid.cbSize = std::mem::size_of::<NOTIFYICONDATAW>() as u32;
        nid.hWnd = hwnd;
        nid.uID = ID_TRAY_ICON;
        Shell_NotifyIconW(NIM_DELETE, &nid);
    }
}

fn show_main(hwnd: HWND) {
    unsafe {
        ShowWindow(hwnd, SW_SHOW);
        ShowWindow(hwnd, SW_RESTORE);
        SetForegroundWindow(hwnd);
        InvalidateRect(hwnd, null(), 0);
    }
}

fn apply_dwm(hwnd: HWND) {
    unsafe {
        let dark: u32 = 1;
        if DwmSetWindowAttribute(
            hwnd,
            DWMWA_USE_IMMERSIVE_DARK_MODE as u32,
            (&dark as *const u32).cast(),
            4,
        ) != 0
        {
            let _ = DwmSetWindowAttribute(hwnd, 19, (&dark as *const u32).cast(), 4);
        }
        let cap = BG.colorref();
        let _ = DwmSetWindowAttribute(hwnd, DWMWA_CAPTION_COLOR as u32, (&cap as *const u32).cast(), 4);
        let brd = BORDER.colorref();
        let _ = DwmSetWindowAttribute(hwnd, DWMWA_BORDER_COLOR as u32, (&brd as *const u32).cast(), 4);
        let round = DWMWCP_ROUND as u32;
        let _ = DwmSetWindowAttribute(
            hwnd,
            DWMWA_WINDOW_CORNER_PREFERENCE as u32,
            (&round as *const u32).cast(),
            4,
        );
    }
}

fn show_tray_menu(hwnd: HWND) {
    let on = ENABLED.load(Ordering::SeqCst);
    let pending = PENDING.load(Ordering::SeqCst) > 0;
    unsafe {
        let menu = CreatePopupMenu();
        AppendMenuW(
            menu,
            MF_STRING | MF_GRAYED,
            0,
            wide(if on { "Trạng thái: Đang bật" } else { "Trạng thái: Đã tắt" }).as_ptr(),
        );
        AppendMenuW(menu, MF_SEPARATOR, 0, null());
        AppendMenuW(menu, MF_STRING, ID_MENU_OPEN, wide("Mở Steam Bypass").as_ptr());
        AppendMenuW(
            menu,
            MF_STRING,
            ID_MENU_TOGGLE,
            wide(if on { "Tắt bypass" } else { "Bật bypass" }).as_ptr(),
        );
        AppendMenuW(
            menu,
            MF_STRING | if pending { MF_GRAYED } else { 0 },
            ID_MENU_TEST,
            wide("Kiểm tra ngay").as_ptr(),
        );
        AppendMenuW(menu, MF_SEPARATOR, 0, null());
        AppendMenuW(menu, MF_STRING, ID_MENU_EXIT, wide("Thoát").as_ptr());

        let mut pt = POINT { x: 0, y: 0 };
        GetCursorPos(&mut pt);
        SetForegroundWindow(hwnd);
        let cmd = TrackPopupMenu(
            menu,
            TPM_RETURNCMD | TPM_NONOTIFY | TPM_RIGHTBUTTON,
            pt.x,
            pt.y,
            0,
            hwnd,
            null(),
        );
        DestroyMenu(menu);

        match cmd as usize {
            ID_MENU_OPEN => show_main(hwnd),
            ID_MENU_TOGGLE => {
                proxy::set_enabled(!on);
                update_tray_tip(hwnd);
                if let Some(st) = get_state(hwnd) {
                    sync_timer(hwnd, st);
                }
                InvalidateRect(hwnd, null(), 0);
            }
            ID_MENU_TEST => {
                proxy::run_test();
                if let Some(st) = get_state(hwnd) {
                    sync_timer(hwnd, st);
                }
                InvalidateRect(hwnd, null(), 0);
            }
            ID_MENU_EXIT => {
                DestroyWindow(hwnd);
            }
            _ => {}
        }
    }
}

unsafe extern "system" fn wndproc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    unsafe {
        if msg == WM_NCCREATE {
            let cs = &*(lp as *const CREATESTRUCTW);
            SetWindowLongPtrW(hwnd, GWLP_USERDATA, cs.lpCreateParams as isize);
            return DefWindowProcW(hwnd, msg, wp, lp);
        }

        if let Some(state) = get_state(hwnd) {
            if state.taskbar_msg != 0 && msg == state.taskbar_msg {
                add_tray(hwnd);
                return 0;
            }
        }

        match msg {
            WM_ERASEBKGND => 1,
            WM_GETMINMAXINFO => {
                if let Some(state) = get_state(hwnd) {
                    let s = state.dpi as f32 / 96.0;
                    let mut rc = RECT {
                        left: 0,
                        top: 0,
                        right: (MIN_W * s).round() as i32,
                        bottom: (MIN_H * s).round() as i32,
                    };
                    AdjustWindowRectExForDpi(&mut rc, WS_OVERLAPPEDWINDOW, 0, 0, state.dpi);
                    let mmi = &mut *(lp as *mut MINMAXINFO);
                    mmi.ptMinTrackSize.x = rc.right - rc.left;
                    mmi.ptMinTrackSize.y = rc.bottom - rc.top;
                }
                0
            }
            WM_DPICHANGED => {
                if let Some(state) = get_state(hwnd) {
                    let new_dpi = (wp & 0xFFFF) as u32;
                    if new_dpi >= 72 {
                        state.dpi = new_dpi;
                        state.fonts = FontSet::new(new_dpi);
                    }
                    let suggested = &*(lp as *const RECT);
                    SetWindowPos(
                        hwnd,
                        null_mut(),
                        suggested.left,
                        suggested.top,
                        suggested.right - suggested.left,
                        suggested.bottom - suggested.top,
                        SWP_NOZORDER | SWP_NOACTIVATE,
                    );
                    InvalidateRect(hwnd, null(), 0);
                }
                0
            }
            WM_PAINT => {
                let mut ps: PAINTSTRUCT = std::mem::zeroed();
                let hdc = BeginPaint(hwnd, &mut ps);
                let mut cr = RECT { left: 0, top: 0, right: 0, bottom: 0 };
                GetClientRect(hwnd, &mut cr);
                let pw = (cr.right - cr.left).max(1);
                let ph = (cr.bottom - cr.top).max(1);

                let mem_dc = CreateCompatibleDC(hdc);
                let bmp = CreateCompatibleBitmap(hdc, pw, ph);
                let old_bmp = SelectObject(mem_dc, bmp as HGDIOBJ);

                if let Some(state) = get_state(hwnd) {
                    let snap = proxy::snapshot();
                    let on = ENABLED.load(Ordering::SeqCst);
                    let pending = PENDING.load(Ordering::SeqCst);
                    paint_ui(mem_dc, pw, ph, state, &snap, on, pending);
                }

                BitBlt(hdc, 0, 0, pw, ph, mem_dc, 0, 0, SRCCOPY);
                SelectObject(mem_dc, old_bmp);
                DeleteObject(bmp as HGDIOBJ);
                DeleteDC(mem_dc);
                EndPaint(hwnd, &ps);
                0
            }
            WM_MOUSEMOVE => {
                if let Some(state) = get_state(hwnd) {
                    if !state.tracking_mouse {
                        let mut tme = TRACKMOUSEEVENT {
                            cbSize: std::mem::size_of::<TRACKMOUSEEVENT>() as u32,
                            dwFlags: TME_LEAVE,
                            hwndTrack: hwnd,
                            dwHoverTime: 0,
                        };
                        TrackMouseEvent(&mut tme);
                        state.tracking_mouse = true;
                    }
                    let s = state.dpi as f32 / 96.0;
                    let lx = ((lp as u32 & 0xFFFF) as i16) as f32 / s;
                    let ly = (((lp as u32 >> 16) & 0xFFFF) as i16) as f32 / s;
                    let new_hover = hit_test(state, lx, ly);
                    state.cursor_hand = new_hover.is_some();
                    if new_hover != state.hover {
                        state.hover = new_hover;
                        InvalidateRect(hwnd, null(), 0);
                    }
                }
                0
            }
            WM_MOUSELEAVE => {
                if let Some(state) = get_state(hwnd) {
                    state.tracking_mouse = false;
                    state.cursor_hand = false;
                    if state.hover.is_some() {
                        state.hover = None;
                        InvalidateRect(hwnd, null(), 0);
                    }
                }
                0
            }
            WM_SETCURSOR => {
                if (lp as u32 & 0xFFFF) == HTCLIENT {
                    let hand = get_state(hwnd).is_some_and(|s| s.cursor_hand);
                    let cur = LoadCursorW(null_mut(), if hand { IDC_HAND } else { IDC_ARROW });
                    SetCursor(cur);
                    return 1;
                }
                DefWindowProcW(hwnd, msg, wp, lp)
            }
            WM_LBUTTONUP => {
                let act = get_state(hwnd).and_then(|state| {
                    let s = state.dpi as f32 / 96.0;
                    let lx = ((lp as u32 & 0xFFFF) as i16) as f32 / s;
                    let ly = (((lp as u32 >> 16) & 0xFFFF) as i16) as f32 / s;
                    hit_test(state, lx, ly)
                });
                if let Some(a) = act {
                    match a {
                        Action::ToggleBypass => {
                            let new_on = !ENABLED.load(Ordering::SeqCst);
                            proxy::set_enabled(new_on);
                            update_tray_tip(hwnd);
                            if let Some(state) = get_state(hwnd) {
                                sync_timer(hwnd, state);
                            }
                        }
                        Action::RunTest => {
                            proxy::run_test();
                            if let Some(state) = get_state(hwnd) {
                                sync_timer(hwnd, state);
                            }
                        }
                        Action::ToggleGroup(gi) => {
                            if let Some(state) = get_state(hwnd) {
                                state.expanded[gi] = !state.expanded[gi];
                            }
                        }
                        Action::QuitApp => {
                            DestroyWindow(hwnd);
                            return 0;
                        }
                    }
                    InvalidateRect(hwnd, null(), 0);
                }
                0
            }
            WM_MOUSEWHEEL => {
                if let Some(state) = get_state(hwnd) {
                    if state.max_scroll > 0.5 {
                        let delta = ((wp >> 16) as u16) as i16 as f32;
                        state.scroll_y = (state.scroll_y - (delta / 120.0) * 34.0).clamp(0.0, state.max_scroll);
                        InvalidateRect(hwnd, null(), 0);
                    }
                }
                0
            }
            WM_TIMER if wp == ID_TIMER_ANIM => {
                if let Some(state) = get_state(hwnd) {
                    let target = if ENABLED.load(Ordering::SeqCst) { 1.0 } else { 0.0 };
                    let diff = target - state.toggle_t;
                    if diff.abs() <= 0.01 {
                        state.toggle_t = target;
                    } else {
                        state.toggle_t += diff.signum() * 0.16_f32.min(diff.abs());
                    }
                    if PENDING.load(Ordering::SeqCst) > 0 {
                        state.spin_deg = (state.spin_deg + 10.0) % 360.0;
                    }
                    sync_timer(hwnd, state);
                    InvalidateRect(hwnd, null(), 0);
                }
                0
            }
            WM_APP_STATS => {
                UI_DIRTY.store(false, Ordering::Release);
                let on = ENABLED.load(Ordering::SeqCst);
                let mut tray_changed = false;
                if let Some(state) = get_state(hwnd) {
                    sync_timer(hwnd, state);
                    if state.last_tray_on != on {
                        state.last_tray_on = on;
                        tray_changed = true;
                    }
                }
                if tray_changed {
                    update_tray_tip(hwnd);
                }
                InvalidateRect(hwnd, null(), 0);
                0
            }
            WM_APP_SHOW => {
                show_main(hwnd);
                0
            }
            WM_TRAY => {
                let ev = (lp as u32) & 0xFFFF;
                if ev == WM_LBUTTONUP || ev == WM_LBUTTONDBLCLK {
                    show_main(hwnd);
                } else if ev == WM_RBUTTONUP {
                    show_tray_menu(hwnd);
                }
                0
            }
            WM_CLOSE => {
                ShowWindow(hwnd, SW_HIDE);
                0
            }
            WM_QUERYENDSESSION | WM_ENDSESSION => {
                proxy::set_pac(false);
                DefWindowProcW(hwnd, msg, wp, lp)
            }
            WM_DESTROY => {
                if let Some(state) = get_state(hwnd) {
                    if state.anim_timer {
                        KillTimer(hwnd, ID_TIMER_ANIM);
                        state.anim_timer = false;
                    }
                }
                delete_tray(hwnd);
                proxy::set_pac(false);
                let ptr = SetWindowLongPtrW(hwnd, GWLP_USERDATA, 0) as *mut UiState;
                if !ptr.is_null() {
                    drop(Box::from_raw(ptr));
                }
                PostQuitMessage(0);
                0
            }
            _ => DefWindowProcW(hwnd, msg, wp, lp),
        }
    }
}

pub fn run() {
    unsafe {
        let hinst = GetModuleHandleW(null());
        let cls_w = wide(WND_CLASS);
        let title_w = wide(APP_TITLE);

        let big_icon = LoadIconW(hinst, 1usize as *const u16);
        let cx_sm = GetSystemMetrics(SM_CXSMICON);
        let cy_sm = GetSystemMetrics(SM_CYSMICON);
        let sm_icon = LoadImageW(hinst, 1usize as *const u16, IMAGE_ICON, cx_sm, cy_sm, LR_DEFAULTCOLOR) as HICON;

        let wc = WNDCLASSW {
            style: CS_HREDRAW | CS_VREDRAW,
            lpfnWndProc: Some(wndproc),
            cbClsExtra: 0,
            cbWndExtra: 0,
            hInstance: hinst,
            hIcon: if !big_icon.is_null() {
                big_icon
            } else {
                LoadIconW(null_mut(), IDI_APPLICATION)
            },
            hCursor: LoadCursorW(null_mut(), IDC_ARROW),
            hbrBackground: null_mut(),
            lpszMenuName: null(),
            lpszClassName: cls_w.as_ptr(),
        };
        RegisterClassW(&wc);

        let taskbar_msg = RegisterWindowMessageW(wide("TaskbarCreated").as_ptr());
        let state = Box::new(UiState::new(96, ENABLED.load(Ordering::SeqCst), taskbar_msg));
        let style = WS_OVERLAPPEDWINDOW | WS_CLIPCHILDREN | WS_CLIPSIBLINGS;

        let hwnd = CreateWindowExW(
            0,
            cls_w.as_ptr(),
            title_w.as_ptr(),
            style,
            CW_USEDEFAULT,
            CW_USEDEFAULT,
            WIN_W as i32,
            WIN_H as i32,
            null_mut(),
            null_mut(),
            hinst,
            Box::into_raw(state).cast(),
        );
        if hwnd.is_null() {
            return;
        }

        // Apply accurate Per-Monitor DPI size & center on primary monitor
        let dpi = GetDpiForWindow(hwnd).max(96);
        let s = dpi as f32 / 96.0;
        if let Some(st) = get_state(hwnd) {
            st.dpi = dpi;
            st.fonts = FontSet::new(dpi);
        }
        let mut rc = RECT {
            left: 0,
            top: 0,
            right: (WIN_W * s).round() as i32,
            bottom: (WIN_H * s).round() as i32,
        };
        AdjustWindowRectExForDpi(&mut rc, style, 0, 0, dpi);
        let ww = rc.right - rc.left;
        let wh = rc.bottom - rc.top;
        let sx = ((GetSystemMetrics(SM_CXSCREEN) - ww) / 2).max(0);
        let sy = ((GetSystemMetrics(SM_CYSCREEN) - wh) / 2).max(0);
        SetWindowPos(hwnd, null_mut(), sx, sy, ww, wh, SWP_NOZORDER);

        apply_dwm(hwnd);
        if !big_icon.is_null() {
            SendMessageW(hwnd, WM_SETICON, ICON_BIG as usize, big_icon as isize);
        }
        if !sm_icon.is_null() {
            SendMessageW(hwnd, WM_SETICON, ICON_SMALL as usize, sm_icon as isize);
        }

        add_tray(hwnd);
        proxy::set_ui_hwnd(hwnd);
        proxy::run_test();
        if let Some(st) = get_state(hwnd) {
            sync_timer(hwnd, st);
        }

        ShowWindow(hwnd, SW_SHOW);
        UpdateWindow(hwnd);

        let mut msg: MSG = std::mem::zeroed();
        while GetMessageW(&mut msg, null_mut(), 0, 0) > 0 {
            TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::gfx::GdiplusGuard;
    use std::fs::File;
    use std::io::Write;

    fn make_stat(group: usize, ok: u32, fail: u32, ip: &str, dns: DnsState) -> Stat {
        Stat {
            group,
            ok,
            fail,
            ip: ip.to_string(),
            dns,
            last_ok: ok > 0 && fail == 0,
        }
    }

    fn sample_snap(with_fail: bool) -> Vec<(String, Stat)> {
        let mut v: Vec<(String, Stat)> = vec![
            ("store.steampowered.com".into(), make_stat(0, 4, 0, "23.15.142.182", DnsState::Poisoned)),
            ("steamcommunity.com".into(), make_stat(1, if with_fail { 0 } else { 3 }, if with_fail { 1 } else { 0 }, "23.210.5.41", DnsState::Poisoned)),
            ("api.steampowered.com".into(), make_stat(2, 2, 0, "104.16.120.11", DnsState::Clean)),
            ("help.steampowered.com".into(), make_stat(2, if with_fail { 0 } else { 1 }, if with_fail { 1 } else { 0 }, "23.15.142.182", DnsState::Clean)),
            ("login.steampowered.com".into(), make_stat(2, 5, 0, "155.133.248.39", DnsState::Poisoned)),
            ("steamcommunity-a.akamaihd.net".into(), make_stat(3, 6, 0, "23.200.143.5", DnsState::Clean)),
            ("steamcdn-a.akamaihd.net".into(), make_stat(3, 2, 0, "23.200.143.11", DnsState::Clean)),
            ("avatars.akamai.steamstatic.com".into(), make_stat(4, 3, 0, "23.200.143.16", DnsState::Clean)),
            ("cdn.akamai.steamstatic.com".into(), make_stat(4, 2, 0, "23.200.143.16", DnsState::Clean)),
            ("cdn.cloudflare.steamstatic.com".into(), make_stat(4, 4, 0, "104.16.124.96", DnsState::Clean)),
        ];
        v.sort_by(|a, b| a.1.group.cmp(&b.1.group).then(a.0.cmp(&b.0)));
        v
    }

    fn render_bmp(path: &str, dpi: u32, on: bool, pending: usize, expanded: [bool; 6], snap: &[(String, Stat)]) {
        let _gdip = GdiplusGuard::new();
        let s = dpi as f32 / 96.0;
        let pw = (WIN_W * s).round() as i32;
        let ph = (WIN_H * s).round() as i32;

        unsafe {
            let mem_dc = CreateCompatibleDC(null_mut());
            let mut bmi: BITMAPINFO = std::mem::zeroed();
            bmi.bmiHeader.biSize = std::mem::size_of::<BITMAPINFOHEADER>() as u32;
            bmi.bmiHeader.biWidth = pw;
            bmi.bmiHeader.biHeight = -ph; // top-down DIB
            bmi.bmiHeader.biPlanes = 1;
            bmi.bmiHeader.biBitCount = 32;
            bmi.bmiHeader.biCompression = BI_RGB;

            let mut bits: *mut core::ffi::c_void = null_mut();
            let hbmp = CreateDIBSection(mem_dc, &bmi, DIB_RGB_COLORS, &mut bits, null_mut(), 0);
            assert!(!hbmp.is_null() && !bits.is_null());
            let old = SelectObject(mem_dc, hbmp as HGDIOBJ);

            let mut state = UiState::new(dpi, on, 0);
            state.expanded = expanded;
            paint_ui(mem_dc, pw, ph, &mut state, snap, on, pending);

            // Export 24-bit BGR BMP so ClearType alpha=0 bytes do not become transparent in viewers.
            let src = std::slice::from_raw_parts(bits as *const u8, (pw * ph * 4) as usize);
            let row_stride = ((pw as usize * 3) + 3) & !3;
            let img_size = row_stride * ph as usize;
            let file_size = 54 + img_size;
            let mut out = Vec::with_capacity(file_size);

            // BITMAPFILEHEADER (14 bytes)
            out.extend_from_slice(b"BM");
            out.extend_from_slice(&(file_size as u32).to_le_bytes());
            out.extend_from_slice(&0u32.to_le_bytes());
            out.extend_from_slice(&54u32.to_le_bytes());
            // BITMAPINFOHEADER (40 bytes)
            out.extend_from_slice(&40u32.to_le_bytes());
            out.extend_from_slice(&(pw as i32).to_le_bytes());
            out.extend_from_slice(&(ph as i32).to_le_bytes()); // bottom-up
            out.extend_from_slice(&1u16.to_le_bytes());
            out.extend_from_slice(&24u16.to_le_bytes());
            out.extend_from_slice(&0u32.to_le_bytes());
            out.extend_from_slice(&(img_size as u32).to_le_bytes());
            out.extend_from_slice(&[0u8; 16]);

            let pad = row_stride - (pw as usize * 3);
            for y in (0..ph as usize).rev() {
                let row = &src[y * pw as usize * 4..(y + 1) * pw as usize * 4];
                for x in 0..pw as usize {
                    out.push(row[x * 4]);
                    out.push(row[x * 4 + 1]);
                    out.push(row[x * 4 + 2]);
                }
                for _ in 0..pad {
                    out.push(0);
                }
            }
            let mut f = File::create(path).expect("create preview bmp");
            f.write_all(&out).unwrap();

            SelectObject(mem_dc, old);
            DeleteObject(hbmp as HGDIOBJ);
            DeleteDC(mem_dc);
        }
    }

    #[test]
    fn render_ui_previews() {
        let dir = env!("CARGO_MANIFEST_DIR");
        let ok_snap = sample_snap(false);
        let fail_snap = sample_snap(true);

        render_bmp(&format!(r"{dir}\target\preview_compact.bmp"), 96, true, 0, [false; 6], &ok_snap);
        render_bmp(
            &format!(r"{dir}\target\preview_expanded.bmp"),
            96,
            true,
            0,
            [false, false, false, true, false, false],
            &ok_snap,
        );
        render_bmp(
            &format!(r"{dir}\target\preview_off_125.bmp"),
            120,
            false,
            0,
            [false, true, false, false, false, false],
            &fail_snap,
        );
    }
}
