//! Pure Win32 + GDI+ window, system tray, segmented tabs, and UI UX Pro Max design system
//! (Minimalist Scientific · Variance 8 Bold/Asymmetric · Motion 2 Subtle · Density 3 Spacious).

use crate::config::{self, CustomDomain};
use crate::gfx::{Canvas, FontSet, RectF};
use crate::proxy::{
    self, APP_TITLE, CDN_GROUP_IDX, CUSTOM_GROUP_IDX, DIRECT_CDN, DnsState, DohProvider, ENABLED,
    GROUPS, PENDING, PORT, Stat, UI_DIRTY, WM_APP_NET_CHANGED, WM_APP_STATS, domain_matches, wide,
};
use crate::theme::{self, *};
use std::ptr::{null, null_mut};
use std::sync::atomic::Ordering;
use windows_sys::Win32::{
    Foundation::{HWND, LPARAM, LRESULT, POINT, RECT, WPARAM},
    Graphics::{Dwm::*, Gdi::*},
    System::{
        DataExchange::{CloseClipboard, GetClipboardData, OpenClipboard},
        LibraryLoader::GetModuleHandleW,
        Memory::{GlobalLock, GlobalUnlock},
    },
    UI::{
        Controls::{DRAWITEMSTRUCT, ODS_DISABLED, ODS_FOCUS, ODS_SELECTED},
        HiDpi::{AdjustWindowRectExForDpi, GetDpiForWindow},
        Input::KeyboardAndMouse::{
            EnableWindow, GetFocus, GetKeyState, SetFocus, TME_LEAVE, TRACKMOUSEEVENT, TrackMouseEvent, VK_CONTROL,
            VK_ESCAPE, VK_RETURN,
        },
        Shell::*,
        WindowsAndMessaging::*,
    },
};

const WM_MOUSELEAVE: u32 = 0x02A3;
const WM_CTLCOLOREDIT: u32 = 0x0133;
const WM_POWERBROADCAST: u32 = 0x0218;
const PBT_APMRESUMESUSPEND: usize = 0x0007;
const PBT_APMRESUMEAUTOMATIC: usize = 0x0012;
const CF_UNICODETEXT: u32 = 13;
const EM_REPLACESEL: u32 = 0x00C2;

const ES_LEFT: u32 = 0x0000;
const ES_AUTOHSCROLL: u32 = 0x0080;
const EN_SETFOCUS: u32 = 0x0100;
const EN_KILLFOCUS: u32 = 0x0200;
const EN_CHANGE: u32 = 0x0300;

pub const WND_CLASS: &str = "SteamBypassMainWnd";
pub const WM_APP_SHOW: u32 = WM_APP + 3;
const WM_APP_ADD_DOMAIN: u32 = WM_APP + 4;

const WM_TRAY: u32 = WM_APP + 1;
const ID_TRAY_ICON: u32 = 1;
const ID_TIMER_ANIM: usize = 1;
const ID_TIMER_NOTICE: usize = 2;
const ID_EDIT_DOMAIN: usize = 101;

const ID_MENU_OPEN: usize = 1;
const ID_MENU_TOGGLE: usize = 2;
const ID_MENU_TEST: usize = 3;
const ID_MENU_DIRECT_CDN: usize = 4;
const ID_MENU_AUTOSTART: usize = 5;
const ID_MENU_RESET_NET: usize = 6;
const ID_MENU_EXIT: usize = 7;
const ID_MENU_DOH_BASE: usize = 20;

// Spacious scientific grid anchors (Density 3/10 · Variance 8/10)
const HERO_Y: f32 = 20.0;
const HERO_H: f32 = 88.0;
const DOH_Y: f32 = 120.0;
const DOH_H: f32 = 44.0;
const NAV_Y: f32 = 178.0;
const NAV_H: f32 = 36.0;
const CARD_Y: f32 = 228.0;
const FOOTER_RESERVE: f32 = 54.0;

const INPUT_PAD: f32 = 14.0;
const INPUT_H: f32 = 36.0;
const ADD_BTN_W: f32 = 82.0;
const ADD_GAP: f32 = 10.0;

#[derive(Clone, Copy, PartialEq, Eq)]
enum Tab {
    Steam,
    Custom,
    Utilities,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Action {
    ToggleBypass,
    SelectDoh(usize),
    SelectTab(Tab),
    RunTest,
    FocusInput,
    AddDomain,
    ToggleGroup(usize),
    ToggleCustomDomain(usize),
    RemoveDomain(usize),
    Utility(usize),
    QuitApp,
}

struct UiState {
    dpi: u32,
    fonts: FontSet,
    tab: Tab,
    hover: Option<Action>,
    tracking_mouse: bool,
    expanded: [bool; GROUPS.len()],
    scroll_steam: f32,
    scroll_custom: f32,
    max_scroll: f32,
    scroll_custom_to_bottom: bool,
    toggle_t: f32,
    spin_deg: f32,
    anim_timer: bool,
    hits: Vec<(RectF, Action)>,
    row_targets: Vec<String>,
    taskbar_msg: u32,
    last_tray_on: bool,
    autostart: bool,
    update: Option<std::sync::mpsc::Receiver<String>>,
    edit_hwnd: HWND,
    edit_bg_brush: HBRUSH,
    edit_focused: bool,
    edit_has_text: bool,
    notice: Option<(String, Color)>,
}

impl UiState {
    fn new(dpi: u32, on: bool, autostart: bool, taskbar_msg: u32) -> Self {
        let edit_bg_brush = unsafe { CreateSolidBrush(theme::surface_inset().colorref()) };
        Self {
            dpi,
            fonts: FontSet::new(dpi),
            tab: Tab::Steam,
            hover: None,
            tracking_mouse: false,
            expanded: [false; GROUPS.len()],
            scroll_steam: 0.0,
            scroll_custom: 0.0,
            max_scroll: 0.0,
            scroll_custom_to_bottom: false,
            toggle_t: if on { 1.0 } else { 0.0 },
            spin_deg: 0.0,
            anim_timer: false,
            hits: Vec::with_capacity(36),
            row_targets: Vec::new(),
            taskbar_msg,
            last_tray_on: on,
            autostart,
            update: None,
            edit_hwnd: null_mut(),
            edit_bg_brush,
            edit_focused: false,
            edit_has_text: false,
            notice: None,
        }
    }

    fn current_scroll_mut(&mut self) -> Option<&mut f32> {
        match self.tab {
            Tab::Steam => Some(&mut self.scroll_steam),
            Tab::Custom => Some(&mut self.scroll_custom),
            Tab::Utilities => None,
        }
    }
}

impl Drop for UiState {
    fn drop(&mut self) {
        if !self.edit_bg_brush.is_null() {
            unsafe {
                DeleteObject(self.edit_bg_brush as HGDIOBJ);
            }
            self.edit_bg_brush = null_mut();
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
        Health::Ok => ("Đã bypass", theme::success(), tint(theme::success())),
        Health::Partial => ("Một phần", theme::warning(), tint(theme::warning())),
        Health::Fail => ("Bị chặn", theme::danger(), tint(theme::danger())),
        Health::Testing => ("Đang thử", theme::accent(), tint(theme::accent())),
        Health::Idle => ("Chờ…", theme::neutral(), theme::neutral().with_alpha(30)),
    }
}

fn host_dot_color(h: Health) -> Color {
    match h {
        Health::Ok => theme::success(),
        Health::Partial => theme::warning(),
        Health::Testing => theme::info(),
        Health::Fail => theme::danger(),
        Health::Idle => theme::neutral(),
    }
}

fn latency_color(ms: u32) -> Color {
    if ms < 60 {
        theme::success()
    } else if ms <= 150 {
        theme::warning()
    } else {
        theme::danger()
    }
}

fn avg_latency_ms(rows: &[&(String, Stat)]) -> Option<u32> {
    let mut sum: u64 = 0;
    let mut cnt: u64 = 0;
    for (_, s) in rows {
        if let Some(ms) = s.latency_ms {
            sum += ms as u64;
            cnt += 1;
        }
    }
    if cnt == 0 {
        None
    } else {
        Some(((sum + cnt / 2) / cnt) as u32)
    }
}

fn custom_input_rect(lw: f32) -> RectF {
    let cw = (lw - PAD_X * 2.0).max(200.0);
    let inner_x = PAD_X + INPUT_PAD;
    let inner_w = (cw - INPUT_PAD * 2.0).max(160.0);
    RectF::new(
        inner_x,
        CARD_Y + INPUT_PAD,
        (inner_w - ADD_BTN_W - ADD_GAP).max(100.0),
        INPUT_H,
    )
}

// ---------------- Pure UI Rendering ----------------
#[allow(clippy::too_many_arguments)]
fn paint_ui(
    hdc: HDC,
    phys_w: i32,
    phys_h: i32,
    state: &mut UiState,
    snap: &[(String, Stat)],
    custom_list: &[CustomDomain],
    on: bool,
    direct_cdn: bool,
    doh: DohProvider,
    pending: usize,
) {
    let scale = state.dpi as f32 / 96.0;
    let lw = (phys_w as f32 / scale).max(320.0);
    let lh = (phys_h as f32 / scale).max(360.0);
    state.hits.clear();
    state.row_targets.clear();

    let mut c = Canvas::new(hdc, state.dpi);

    // Window background (--color-background #0F172A)
    c.fill_rect(RectF::new(0.0, 0.0, lw, lh), theme::bg());

    let cx = PAD_X;
    let cw = (lw - PAD_X * 2.0).max(200.0);

    // Precompute aggregate telemetry
    let steam_rows: Vec<&(String, Stat)> = snap
        .iter()
        .filter(|(_, s)| s.group < CUSTOM_GROUP_IDX)
        .collect();
    let steam_ok = steam_rows
        .iter()
        .filter(|(_, s)| host_health(s, pending) == Health::Ok)
        .count();
    let steam_fail = steam_rows
        .iter()
        .filter(|(_, s)| host_health(s, pending) == Health::Fail)
        .count();

    let all_rows: Vec<&(String, Stat)> = snap.iter().collect();
    let avg_ms = avg_latency_ms(&all_rows);
    let total_ok = snap.iter().filter(|(_, s)| host_health(s, pending) == Health::Ok).count();
    let total_fail = snap.iter().filter(|(_, s)| host_health(s, pending) == Health::Fail).count();
    let dns_fixed_total = snap
        .iter()
        .filter(|(_, s)| matches!(s.dns, DnsState::Poisoned | DnsState::Failed))
        .count();

    // 1. Asymmetric Hero Bento Grid (Variance 8: 66% Control | 34% Live Latency Telemetry)
    let bento_gap = 12.0;
    let tele_w = ((cw - bento_gap) * 0.33).clamp(138.0, 168.0).round();
    let ctrl_w = (cw - bento_gap - tele_w).max(220.0);
    let hero_ctrl = RectF::new(cx, HERO_Y, ctrl_w, HERO_H);
    let hero_tele = RectF::new(hero_ctrl.right() + bento_gap, HERO_Y, tele_w, HERO_H);

    // 1a. Left Module — Primary Control Card
    c.fill_round_rect(hero_ctrl, R_CARD, theme::surface());
    let ctrl_border = if on {
        Color::lerp(theme::border(), theme::accent(), 0.32)
    } else {
        theme::border()
    };
    c.stroke_round_rect(hero_ctrl, R_CARD, 1.0, ctrl_border);

    // Scientific left-edge status rail (3px crisp vertical indicator)
    let rail_col = if on { theme::accent() } else { theme::neutral() };
    c.fill_round_rect(
        RectF::new(hero_ctrl.x + 1.0, hero_ctrl.y + 14.0, 3.0, hero_ctrl.h - 28.0),
        1.5,
        rail_col,
    );

    // Logo
    let lx = hero_ctrl.x + 16.0;
    let ly = hero_ctrl.y + 24.0;
    c.draw_logo(lx, ly, 40.0);

    // Main Power Toggle Switch (right of control card)
    let sw_w = 46.0;
    let sw_h = 24.0;
    let sw_x = hero_ctrl.right() - 16.0 - sw_w;
    let sw_y = hero_ctrl.cy() - sw_h * 0.5;
    let sw_rect = RectF::new(sw_x, sw_y, sw_w, sw_h);
    let sw_hovered = state.hover == Some(Action::ToggleBypass);
    let mut track = Color::lerp(theme::neutral(), theme::accent(), state.toggle_t);
    if sw_hovered {
        track = Color::lerp(track, WHITE, 0.12);
    }
    c.fill_round_rect(sw_rect, sw_h * 0.5, track);
    let knob_x = sw_x + 12.0 + (sw_w - 24.0) * state.toggle_t;
    c.fill_circle(knob_x, sw_y + 12.0, 9.0, WHITE);
    state.hits.push((sw_rect.inset(-4.0, -6.0), Action::ToggleBypass));

    // Title + compact status badge + clean subtitle
    let tx = lx + 52.0;
    let max_tx = sw_x - 12.0;
    c.draw_text(
        APP_TITLE,
        RectF::new(tx, hero_ctrl.y + 16.0, (max_tx - tx).max(60.0), 22.0),
        state.fonts.title,
        theme::text(),
        DT_LEFT,
    );

    let (st_label, st_col) = if on {
        ("ĐANG BẬT · PROXY 127.0.0.1", theme::accent())
    } else {
        ("ĐÃ TẮT · KẾT NỐI TRỰC TIẾP", theme::text_muted())
    };
    let status_line = if on {
        format!("ĐANG BẬT · :{PORT}")
    } else {
        st_label.to_string()
    };
    c.fill_circle(tx + 3.5, hero_ctrl.y + 46.0, 3.2, if on { theme::accent() } else { theme::neutral() });
    c.draw_text(
        &status_line,
        RectF::new(tx + 12.0, hero_ctrl.y + 38.0, (max_tx - tx - 12.0).max(40.0), 16.0),
        state.fonts.micro,
        st_col,
        DT_LEFT,
    );

    let (hero_sub, hero_sub_col) = if let Some((msg, col)) = &state.notice {
        (msg.clone(), *col)
    } else if pending > 0 {
        ("Đang đo độ trễ & xác thực kết nối…".to_string(), theme::info())
    } else if !on {
        ("Không qua proxy · DoH tạm dừng".to_string(), theme::text_muted())
    } else if direct_cdn {
        (format!("DoH {} · CDN đi thẳng", doh.addr()), theme::text_muted())
    } else {
        (format!("DoH {} · Chia gói TLS", doh.addr()), theme::text_muted())
    };
    c.draw_text(
        &hero_sub,
        RectF::new(tx, hero_ctrl.y + 57.0, (max_tx - tx).max(40.0), 18.0),
        state.fonts.caption,
        hero_sub_col,
        DT_LEFT,
    );

    // 1b. Right Module — Live Telemetry Readout Card
    c.fill_round_rect(hero_tele, R_CARD, theme::surface());
    c.stroke_round_rect(hero_tele, R_CARD, 1.0, theme::border());

    c.draw_text(
        "ĐỘ TRỄ TB",
        RectF::new(hero_tele.x + 14.0, hero_tele.y + 12.0, hero_tele.w - 36.0, 14.0),
        state.fonts.micro,
        theme::text_muted(),
        DT_LEFT,
    );
    let tele_dot = if !on {
        theme::neutral()
    } else if pending > 0 {
        theme::info()
    } else if let Some(ms) = avg_ms {
        latency_color(ms)
    } else {
        theme::neutral()
    };
    c.fill_circle(hero_tele.right() - 16.0, hero_tele.y + 19.0, 3.5, tele_dot);

    let (kpi_txt, kpi_col) = if !on {
        ("— ms".to_string(), theme::text_dim())
    } else if let Some(ms) = avg_ms {
        (format!("{ms} ms"), latency_color(ms))
    } else if pending > 0 {
        ("… ms".to_string(), theme::info())
    } else {
        ("— ms".to_string(), theme::text_dim())
    };
    c.draw_text(
        &kpi_txt,
        RectF::new(hero_tele.x + 14.0, hero_tele.y + 30.0, hero_tele.w - 28.0, 28.0),
        state.fonts.kpi,
        kpi_col,
        DT_LEFT,
    );

    let tele_sub = if !on {
        "Tạm ngưng".to_string()
    } else if total_fail > 0 {
        format!("{total_ok}/{} OK · {total_fail} lỗi", snap.len())
    } else if dns_fixed_total > 0 {
        format!("{total_ok}/{} OK · {dns_fixed_total} DNS", snap.len())
    } else {
        format!("{total_ok}/{} máy chủ OK", snap.len())
    };
    c.draw_text(
        &tele_sub,
        RectF::new(hero_tele.x + 14.0, hero_tele.y + 62.0, hero_tele.w - 28.0, 16.0),
        state.fonts.caption,
        if total_fail > 0 && on { theme::warning() } else { theme::text_muted() },
        DT_LEFT,
    );

    // 2. DoH Provider Selector Bar (4 Verified JSON DoH Resolvers)
    let doh_bar = RectF::new(cx, DOH_Y, cw, DOH_H);
    c.fill_round_rect(doh_bar, R_CARD, theme::surface());
    c.stroke_round_rect(doh_bar, R_CARD, 1.0, theme::border());

    let doh_cy = doh_bar.cy();
    c.draw_dns_icon(doh_bar.x + 19.0, doh_cy, 5.8, theme::info());
    c.draw_text(
        "DoH",
        RectF::new(doh_bar.x + 31.0, doh_bar.y, 34.0, doh_bar.h),
        state.fonts.micro,
        theme::text_muted(),
        DT_LEFT,
    );

    let seg_left = doh_bar.x + 68.0;
    let seg_right = doh_bar.right() - 8.0;
    let seg_total_w = (seg_right - seg_left).max(160.0);
    let pill_gap = 6.0;
    let pill_w = (seg_total_w - pill_gap * (DohProvider::ALL.len() as f32 - 1.0))
        / DohProvider::ALL.len() as f32;
    let pill_h = 28.0;
    let pill_y = doh_cy - pill_h * 0.5;

    for (idx, &prov) in DohProvider::ALL.iter().enumerate() {
        let px = seg_left + idx as f32 * (pill_w + pill_gap);
        let pr = RectF::new(px, pill_y, pill_w, pill_h);
        let active = prov == doh;
        let hov = state.hover == Some(Action::SelectDoh(idx));
        if active {
            c.fill_round_rect(pr, R_SM, theme::primary());
            c.stroke_round_rect(pr, R_SM, 1.0, theme::accent().with_alpha(190));
        } else if hov {
            c.fill_round_rect(pr, R_SM, theme::surface_hover());
            c.stroke_round_rect(pr, R_SM, 1.0, theme::border());
        } else {
            c.fill_round_rect(pr, R_SM, theme::surface_inset());
            c.stroke_round_rect(pr, R_SM, 1.0, theme::border_sub());
        }

        let lbl = if prov == DohProvider::Custom { "Custom…" } else { prov.label() };
        let (lw_p, _) = c.measure_text(lbl, state.fonts.badge);
        if active {
            let dot_gap = 6.0;
            let total_w = 6.0 + dot_gap + lw_p;
            let sx = pr.x + ((pr.w - total_w) * 0.5).max(5.0);
            c.fill_circle(sx + 2.5, pr.cy(), 2.8, theme::accent());
            c.draw_text(
                lbl,
                RectF::new(sx + 6.0 + dot_gap - 1.0, pr.y, lw_p + 4.0, pr.h),
                state.fonts.badge,
                theme::text(),
                DT_LEFT,
            );
        } else {
            c.draw_text(
                lbl,
                pr,
                state.fonts.caption,
                if hov { theme::text_sec() } else { theme::text_muted() },
                DT_CENTER,
            );
        }
        state.hits.push((pr, Action::SelectDoh(idx)));
    }

    // 3. Segmented Tab Bar (Left) + "Kiểm tra" Button (Right)
    let btn_w = 112.0;
    let seg_w = cw - btn_w - 12.0;
    let seg_rect = RectF::new(cx, NAV_Y, seg_w, NAV_H);
    c.fill_round_rect(seg_rect, R_BTN, theme::surface());
    c.stroke_round_rect(seg_rect, R_BTN, 1.0, theme::border());

    let pad = 3.0;
    let tab_w = (seg_w - pad * 2.0 - 4.0) / 3.0;
    let tab_h = NAV_H - pad * 2.0;
    let tab_steam_r = RectF::new(seg_rect.x + pad, seg_rect.y + pad, tab_w, tab_h);
    let tab_custom_r = RectF::new(tab_steam_r.right() + 2.0, seg_rect.y + pad, tab_w, tab_h);

    let steam_cnt = format!("{steam_ok}/{}", steam_rows.len());
    let steam_cnt_col = if steam_fail > 0 && steam_ok == 0 {
        theme::danger()
    } else if steam_fail > 0 {
        theme::warning()
    } else if steam_ok > 0 {
        theme::success()
    } else {
        theme::text_muted()
    };
    draw_tab(
        &mut c,
        &state.fonts,
        tab_steam_r,
        "Steam",
        &steam_cnt,
        steam_cnt_col,
        state.tab == Tab::Steam,
        state.hover == Some(Action::SelectTab(Tab::Steam)),
    );
    state.hits.push((tab_steam_r, Action::SelectTab(Tab::Steam)));

    let enabled_custom = custom_list.iter().filter(|d| d.enabled).count();
    let custom_cnt = if custom_list.is_empty() {
        "0".to_string()
    } else if enabled_custom == custom_list.len() {
        format!("{}", custom_list.len())
    } else {
        format!("{enabled_custom}/{}", custom_list.len())
    };
    let custom_cnt_col = if enabled_custom == 0 { theme::text_muted() } else { theme::info() };
    draw_tab(
        &mut c,
        &state.fonts,
        tab_custom_r,
        "Web khác",
        &custom_cnt,
        custom_cnt_col,
        state.tab == Tab::Custom,
        state.hover == Some(Action::SelectTab(Tab::Custom)),
    );
    state.hits.push((tab_custom_r, Action::SelectTab(Tab::Custom)));

    let utilities = RectF::new(tab_custom_r.right() + 2.0, tab_custom_r.y, tab_w, tab_h);
    let action = Action::SelectTab(Tab::Utilities);
    draw_tab(&mut c, &state.fonts, utilities, "Tiện ích", "", theme::text_muted(),
        state.tab == Tab::Utilities, state.hover == Some(action));
    state.hits.push((utilities, action));

    // "Kiểm tra" button (right)
    let btn = RectF::new(cx + cw - btn_w, NAV_Y, btn_w, NAV_H);
    let btn_hov = pending == 0 && state.hover == Some(Action::RunTest);
    let btn_fill = if pending > 0 {
        theme::surface_inset()
    } else if btn_hov {
        theme::primary_hover()
    } else {
        theme::btn_bg()
    };
    let btn_border = if btn_hov { theme::accent().with_alpha(150) } else { theme::border() };
    c.fill_round_rect(btn, R_BTN, btn_fill);
    c.stroke_round_rect(btn, R_BTN, 1.0, btn_border);

    let (btn_txt, txt_col) = if pending > 0 {
        ("Đang đo…", theme::text_muted())
    } else {
        ("Kiểm tra", theme::text())
    };
    let (btw, _) = c.measure_text(btn_txt, state.fonts.caption);
    let icon_d = 11.0;
    let gap = 7.0;
    let start_x = btn.x + ((btn.w - (icon_d + gap + btw)) * 0.5).max(8.0);
    let icon_cx = start_x + icon_d * 0.5;
    let icon_cy = btn.cy();
    if pending > 0 {
        c.stroke_circle(icon_cx, icon_cy, 5.2, 1.6, theme::border());
        c.draw_arc(icon_cx, icon_cy, 5.2, state.spin_deg, 240.0, 1.8, theme::info());
    } else {
        let ic_col = if btn_hov { theme::accent() } else { theme::text_sec() };
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

    // 4. Main Content Card
    let card_h = (lh - FOOTER_RESERVE - CARD_Y).max(180.0);
    let card = RectF::new(cx, CARD_Y, cw, card_h);
    c.fill_round_rect(card, R_CARD, theme::surface());
    c.stroke_round_rect(card, R_CARD, 1.0, theme::border());

    match state.tab {
        Tab::Steam => paint_steam_tab(&mut c, card, state, snap, direct_cdn, pending),
        Tab::Custom => paint_custom_tab(&mut c, lw, card, state, snap, custom_list, pending),
        Tab::Utilities => paint_utilities(&mut c, card, state, direct_cdn),
    }

    // 5. Clean Single-Line Footer
    let foot_y = lh - 38.0;
    let foot_h = 24.0;
    let quit_w = 52.0;
    let quit_h = 24.0;
    let quit_rect = RectF::new(cx + cw - quit_w, foot_y, quit_w, quit_h);
    let quit_hov = state.hover == Some(Action::QuitApp);
    if quit_hov {
        c.fill_round_rect(quit_rect, R_SM, DESTRUCTIVE.with_alpha(36));
    }
    c.draw_text(
        "Thoát",
        quit_rect,
        state.fonts.caption,
        if quit_hov { theme::danger() } else { theme::text_muted() },
        DT_CENTER,
    );
    state.hits.push((quit_rect, Action::QuitApp));

    let hint = "Đóng (×) ẩn khay";
    let (hint_w, _) = c.measure_text(hint, state.fonts.caption);
    c.draw_text(
        hint,
        RectF::new(quit_rect.x - hint_w - 12.0, foot_y, hint_w + 4.0, foot_h),
        state.fonts.caption,
        theme::text_dim(),
        DT_RIGHT,
    );
}

fn paint_utilities(c: &mut Canvas, card: RectF, state: &mut UiState, direct: bool) {
    state.max_scroll = 0.0;
    let fonts = &state.fonts;
    let row_x = card.x + 12.0;
    let row_w = card.w - 24.0;
    let rows = [
        ("Đi thẳng CDN tải game", "Dùng kết nối trực tiếp để tải game.", ID_MENU_DIRECT_CDN),
        ("Khởi chạy cùng Windows", "Mở ứng dụng trong khay khi đăng nhập.", ID_MENU_AUTOSTART),
        ("Giao diện", "Chọn màu hiển thị của ứng dụng.", 0),
        ("Cập nhật", concat!("Phiên bản đang dùng: ", env!("CARGO_PKG_VERSION")), ID_UPDATE),
    ];

    for (index, (title, description, command)) in rows.into_iter().enumerate() {
        let row = RectF::new(row_x, card.y + 12.0 + index as f32 * 58.0, row_w, 54.0);
        let action = Action::Utility(command);
        let is_switch = index < 2;
        let hovered = state.hover == Some(action);
        c.fill_round_rect(row, R_SM, if is_switch && hovered { theme::surface_hover() } else { theme::surface_inset() });
        c.stroke_round_rect(row, R_SM, 1.0, theme::border_sub());
        let caption_w = if is_switch { row.w - 113.0 } else { row.w - 148.0 };
        c.draw_text(title, RectF::new(row.x + 12.0, row.y + 7.0, caption_w, 19.0), fonts.group, theme::text(), DT_LEFT);
        c.draw_text(description, RectF::new(row.x + 12.0, row.y + 27.0, caption_w, 18.0), fonts.caption, theme::text_muted(), DT_LEFT);

        let controls_x = row.right() - 126.0;
        if is_switch {
            let on = if index == 0 { direct } else { state.autostart };
            c.draw_text(if on { "Bật" } else { "Tắt" }, RectF::new(row.right() - 92.0, row.y, 35.0, row.h),
                fonts.caption, theme::text_sec(), DT_RIGHT);
            draw_mini_toggle(c, RectF::new(row.right() - 47.0, row.cy() - 10.0, 35.0, 20.0), on, hovered);
            state.hits.push((row, action));
        } else if command == ID_UPDATE {
            let busy = state.update.is_some();
            let button = RectF::new(controls_x, row.cy() - 15.0, 114.0, 30.0);
            paint_utility_button(c, fonts, button, if busy { "Đang kiểm tra…" } else { "Kiểm tra cập nhật" },
                false, hovered && !busy);
            if !busy { state.hits.push((button, action)); }
        } else {
            let light = is_light();
            for (offset, label, cmd, active) in [(0.0, "Sáng", ID_LIGHT, light), (58.0, "Tối", ID_DARK, !light)] {
                let button = RectF::new(controls_x + offset, row.cy() - 15.0, 56.0, 30.0);
                let action = Action::Utility(cmd);
                paint_utility_button(c, fonts, button, label, active, state.hover == Some(action));
                state.hits.push((button, action));
            }
        }
    }
}

fn paint_utility_button(c: &mut Canvas, fonts: &FontSet, rect: RectF, label: &str, active: bool, hovered: bool) {
    let fill = if active { theme::primary() } else if hovered { theme::surface_hover() } else { theme::btn_bg() };
    c.fill_round_rect(rect, R_SM, fill);
    c.stroke_round_rect(rect, R_SM, 1.0, if active { theme::accent() } else { theme::border() });
    let label_rect = if active && rect.w <= 56.0 {
        c.draw_checkmark(rect.x + 11.0, rect.cy(), 7.0, 1.5, theme::accent());
        RectF::new(rect.x + 16.0, rect.y, rect.w - 20.0, rect.h)
    } else {
        rect
    };
    c.draw_text(label, label_rect, fonts.badge, theme::text(), DT_CENTER);
}

fn draw_mini_toggle(c: &mut Canvas, r: RectF, on: bool, hovered: bool) {
    let mut track = if on { theme::accent() } else { theme::neutral() };
    if hovered {
        track = Color::lerp(track, WHITE, 0.14);
    }
    c.fill_round_rect(r, r.h * 0.5, track);
    let rad = r.h * 0.5 - 2.5;
    let kx = if on { r.right() - r.h * 0.5 } else { r.x + r.h * 0.5 };
    c.fill_circle(kx, r.cy(), rad, WHITE);
}

fn draw_tab(
    c: &mut Canvas,
    fonts: &FontSet,
    r: RectF,
    label: &str,
    count: &str,
    count_col: Color,
    active: bool,
    hovered: bool,
) {
    if active {
        c.fill_round_rect(r, R_SM, theme::surface_elev());
        c.stroke_round_rect(r, R_SM, 1.0, theme::border());
    } else if hovered {
        c.fill_round_rect(r, R_SM, theme::surface_hover());
    }
    let (lw, _) = c.measure_text(label, fonts.badge);
    let (cw, _) = c.measure_text(count, fonts.mono);
    let gap = 6.0;
    let total = if count.is_empty() { lw } else { lw + gap + cw };
    let sx = r.x + ((r.w - total) * 0.5).max(6.0);
    c.draw_text(
        label,
        RectF::new(sx, r.y, lw + 4.0, r.h),
        fonts.badge,
        if active { theme::text() } else if hovered { theme::text_sec() } else { theme::text_muted() },
        DT_LEFT,
    );
    c.draw_text(
        count,
        RectF::new(sx + lw + gap, r.y, cw + 4.0, r.h),
        fonts.mono,
        count_col,
        DT_LEFT,
    );
}

fn paint_steam_tab(
    c: &mut Canvas,
    card: RectF,
    state: &mut UiState,
    snap: &[(String, Stat)],
    direct_cdn: bool,
    pending: usize,
) {
    let inner = RectF::new(card.x + 1.0, card.y + 4.0, card.w - 2.0, card.h - 8.0);
    let visible_groups: Vec<(usize, Vec<&(String, Stat)>)> = (0..CUSTOM_GROUP_IDX)
        .filter_map(|gi| {
            let rows: Vec<&(String, Stat)> = snap.iter().filter(|(_, s)| s.group == gi).collect();
            if rows.is_empty() { None } else { Some((gi, rows)) }
        })
        .collect();

    const GROUP_H: f32 = 58.0;
    const HOST_H: f32 = 36.0;
    let content_h: f32 = visible_groups
        .iter()
        .map(|(gi, rows)| {
            GROUP_H + if state.expanded[*gi] { rows.len() as f32 * HOST_H } else { 0.0 }
        })
        .sum();
    state.max_scroll = (content_h - inner.h).max(0.0);
    state.scroll_steam = state.scroll_steam.clamp(0.0, state.max_scroll);

    let scroll_pad = if state.max_scroll > 0.5 { 8.0 } else { 0.0 };
    c.set_clip(Some(inner));

    let mut y = inner.y - state.scroll_steam;
    for (idx, (gi, rows)) in visible_groups.iter().enumerate() {
        let gh_rect = RectF::new(inner.x, y, inner.w, GROUP_H);
        if idx > 0 && y >= inner.y - 2.0 && y <= inner.bottom() + 2.0 {
            c.draw_line(
                inner.x + 16.0,
                y,
                inner.right() - 16.0 - scroll_pad,
                y,
                1.0,
                theme::border_sub(),
                false,
            );
        }

        let is_exp = state.expanded[*gi];
        let is_hov = state.hover == Some(Action::ToggleGroup(*gi));
        if gh_rect.bottom() > inner.y && gh_rect.y < inner.bottom() {
            if is_hov {
                c.fill_round_rect(
                    RectF::new(inner.x + 8.0, y + 3.0, inner.w - 16.0 - scroll_pad, GROUP_H - 6.0),
                    R_SM,
                    theme::surface_hover(),
                );
            }
            let hit_top = gh_rect.y.max(inner.y);
            let hit_bot = gh_rect.bottom().min(inner.bottom());
            if hit_bot > hit_top {
                state.hits.push((
                    RectF::new(inner.x, hit_top, inner.w - scroll_pad, hit_bot - hit_top),
                    Action::ToggleGroup(*gi),
                ));
            }

            let is_direct_cdn = *gi == CDN_GROUP_IDX && direct_cdn;
            let (gh, ok_c, _fail_c, dns_fixed) = group_health(rows, pending);
            let grp_ms = avg_latency_ms(rows);
            let dot_c = if is_direct_cdn { theme::info() } else { host_dot_color(gh) };
            let gcy = y + GROUP_H * 0.5;
            let gx = inner.x + 16.0;

            c.draw_chevron(gx + 4.0, gcy, 3.5, is_exp, theme::text_muted());
            if is_direct_cdn {
                c.fill_circle(gx + 20.0, gcy, 7.0, theme::info().with_alpha(34));
            } else if gh == Health::Ok {
                c.fill_circle(gx + 20.0, gcy, 7.0, theme::success().with_alpha(34));
            }
            c.fill_circle(gx + 20.0, gcy, 3.8, dot_c);

            let (blabel, bfg, bbg) = if is_direct_cdn {
                ("Đi thẳng", theme::info(), tint(theme::info()))
            } else {
                health_badge(gh)
            };
            // Fixed scientific column widths so status badge, fraction, and latency pills align vertically
            let badge_col_w = 78.0;
            let badge_r = RectF::new(
                inner.right() - 16.0 - scroll_pad - badge_col_w,
                gcy - 10.5,
                badge_col_w,
                21.0,
            );
            c.fill_round_rect(badge_r, 10.5, bbg);
            c.draw_text(blabel, badge_r, state.fonts.badge, bfg, DT_CENTER);

            let frac_col_w = 26.0;
            let frac_r = RectF::new(badge_r.x - 10.0 - frac_col_w, y, frac_col_w, GROUP_H);
            let frac = format!("{ok_c}/{}", rows.len());
            c.draw_text(&frac, frac_r, state.fonts.mono, theme::text_dim(), DT_RIGHT);

            let mut right_edge = frac_r.x - 10.0;

            // Scientific tabular latency pill (fixed min width for crisp column alignment)
            if let Some(ms) = grp_ms {
                let lat_txt = format!("{ms}ms");
                let lcol = latency_color(ms);
                let (ltw, _) = c.measure_text(&lat_txt, state.fonts.mono_bold);
                let lat_w = (ltw + 12.0).ceil().max(44.0);
                let lat_h = 20.0;
                let lat_r = RectF::new(right_edge - lat_w, gcy - lat_h * 0.5, lat_w, lat_h);
                c.fill_round_rect(lat_r, R_SM, theme::surface_inset());
                c.stroke_round_rect(lat_r, R_SM, 1.0, lcol.with_alpha(70));
                c.draw_text(&lat_txt, lat_r, state.fonts.mono_bold, lcol, DT_CENTER);
                right_edge = lat_r.x - 10.0;
            }

            let text_left = gx + 34.0;
            let text_right = right_edge.max(text_left + 50.0);
            let (gname, gdesc) = GROUPS[*gi];
            c.draw_text(
                gname,
                RectF::new(text_left, y + 10.0, text_right - text_left, 20.0),
                state.fonts.group,
                theme::text(),
                DT_LEFT,
            );
            let sub = if is_direct_cdn {
                "Kết nối trực tiếp · Giữ max tốc độ tải game".to_string()
            } else if dns_fixed {
                format!("{gdesc} · DNS đã sửa")
            } else {
                gdesc.to_string()
            };
            c.draw_text(
                &sub,
                RectF::new(text_left, y + 31.0, text_right - text_left, 17.0),
                state.fonts.caption,
                theme::text_muted(),
                DT_LEFT,
            );
        }
        y += GROUP_H;

        if is_exp {
            let block_h = rows.len() as f32 * HOST_H;
            let block_rect = RectF::new(inner.x, y, inner.w, block_h);
            if block_rect.bottom() > inner.y && block_rect.y < inner.bottom() {
                c.fill_rect(block_rect, theme::surface_inset());
                c.draw_line(inner.x, y, inner.right(), y, 1.0, theme::border_sub(), false);
            }
            for (hi, (host, s)) in rows.iter().enumerate() {
                let hr = RectF::new(inner.x, y, inner.w, HOST_H);
                if hr.bottom() > inner.y && hr.y < inner.bottom() {
                    if hi > 0 {
                        c.draw_line(
                            inner.x + 28.0,
                            y,
                            inner.right() - 16.0 - scroll_pad,
                            y,
                            1.0,
                            theme::border_sub(),
                            false,
                        );
                    }
                    let hcy = y + HOST_H * 0.5;
                    let hh = host_health(s, pending);
                    c.fill_circle(inner.x + 28.0, hcy, 3.0, host_dot_color(hh));

                    let mut rx = inner.right() - 16.0 - scroll_pad;
                    if s.ok > 0 {
                        let cnt = format!("×{}", s.ok);
                        let (cw_txt, _) = c.measure_text(&cnt, state.fonts.caption);
                        c.draw_text(
                            &cnt,
                            RectF::new(rx - cw_txt - 2.0, y, cw_txt + 2.0, HOST_H),
                            state.fonts.caption,
                            theme::text_dim(),
                            DT_RIGHT,
                        );
                        rx -= cw_txt + 8.0;
                    }

                    // Per-host ping latency
                    if let Some(ms) = s.latency_ms {
                        let lat = format!("{ms}ms");
                        let (lw_m, _) = c.measure_text(&lat, state.fonts.mono_bold);
                        c.draw_text(
                            &lat,
                            RectF::new(rx - lw_m - 2.0, y, lw_m + 2.0, HOST_H),
                            state.fonts.mono_bold,
                            latency_color(ms),
                            DT_RIGHT,
                        );
                        rx -= lw_m + 10.0;
                    }

                    let ip = if s.ip.is_empty() { "—" } else { &s.ip };
                    let (ip_w, _) = c.measure_text(ip, state.fonts.mono);
                    c.draw_text(
                        ip,
                        RectF::new(rx - ip_w - 2.0, y, ip_w + 2.0, HOST_H),
                        state.fonts.mono,
                        theme::text_muted(),
                        DT_RIGHT,
                    );
                    rx -= ip_w + 10.0;

                    match s.dns {
                        DnsState::Poisoned | DnsState::Failed => {
                            let tag = "DNS sửa";
                            let (tw, _) = c.measure_text(tag, state.fonts.badge);
                            c.draw_text(
                                tag,
                                RectF::new(rx - tw - 2.0, y, tw + 2.0, HOST_H),
                                state.fonts.badge,
                                theme::warning(),
                                DT_RIGHT,
                            );
                            rx -= tw + 8.0;
                        }
                        DnsState::Clean => {
                            let tag = "DNS OK";
                            let (tw, _) = c.measure_text(tag, state.fonts.caption);
                            c.draw_text(
                                tag,
                                RectF::new(rx - tw - 2.0, y, tw + 2.0, HOST_H),
                                state.fonts.caption,
                                theme::text_dim(),
                                DT_RIGHT,
                            );
                            rx -= tw + 8.0;
                        }
                        DnsState::Unknown => {}
                    }

                    let host_left = inner.x + 40.0;
                    c.draw_text(
                        host,
                        RectF::new(host_left, y, (rx - host_left - 8.0).max(40.0), HOST_H),
                        state.fonts.body,
                        theme::text_sec(),
                        DT_LEFT,
                    );
                }
                y += HOST_H;
            }
        }
    }

    draw_scrollbar(c, inner, content_h, state.scroll_steam, state.max_scroll);
    c.set_clip(None);

}

fn paint_custom_tab(
    c: &mut Canvas,
    lw: f32,
    card: RectF,
    state: &mut UiState,
    snap: &[(String, Stat)],
    custom_list: &[CustomDomain],
    pending: usize,
) {
    // Top fixed input bar inside card (NO 1-click presets strip)
    let inp = custom_input_rect(lw);
    let inp_hov = state.hover == Some(Action::FocusInput);
    let inp_border = if state.edit_focused {
        theme::accent()
    } else if inp_hov {
        Color::lerp(theme::border(), theme::accent(), 0.45)
    } else {
        theme::border()
    };
    c.fill_round_rect(inp, R_BTN, theme::surface_inset());
    c.stroke_round_rect(inp, R_BTN, if state.edit_focused { 1.4 } else { 1.0 }, inp_border);
    if !state.edit_focused && !state.edit_has_text {
        c.draw_text(
            "Nhập hoặc dán tên miền (vd: bbc.com, medium.com)…",
            RectF::new(inp.x + 12.0, inp.y, (inp.w - 22.0).max(40.0), inp.h),
            state.fonts.caption,
            theme::text_dim(),
            DT_LEFT,
        );
    }
    state.hits.push((inp, Action::FocusInput));

    // Primary CTA button matching MASTER.md (.btn-primary: #22C55E bg, #0F172A fg)
    let add_btn = RectF::new(inp.right() + ADD_GAP, inp.y, ADD_BTN_W, INPUT_H);
    let add_hov = state.hover == Some(Action::AddDomain);
    c.fill_round_rect(add_btn, R_BTN, if add_hov { theme::accent_hover() } else { theme::accent() });
    let add_label = "Thêm";
    let (alw, _) = c.measure_text(add_label, state.fonts.badge);
    let plus_w = 10.0;
    let add_gap = 6.0;
    let add_sx = add_btn.x + ((add_btn.w - (plus_w + add_gap + alw)) * 0.5).max(8.0);
    c.draw_plus(add_sx + plus_w * 0.5, add_btn.cy(), 4.2, 1.8, theme::on_accent());
    c.draw_text(
        add_label,
        RectF::new(add_sx + plus_w + add_gap, add_btn.y, alw + 4.0, add_btn.h),
        state.fonts.badge,
        theme::on_accent(),
        DT_LEFT,
    );
    state.hits.push((add_btn, Action::AddDomain));

    let div_y = inp.bottom() + 14.0;
    c.draw_line(card.x + 1.0, div_y, card.right() - 1.0, div_y, 1.0, theme::border_sub(), false);

    // Bottom fixed informational note inside card
    let info_h = 30.0;
    let info_y = card.bottom() - info_h - 1.0;
    c.draw_line(card.x + 1.0, info_y, card.right() - 1.0, info_y, 1.0, theme::border_sub(), false);
    c.draw_text(
        "Chỉ vượt chặn DNS/DPI nhà mạng · Web tự chặn IP (403/WAF) cần VPN",
        RectF::new(card.x + 14.0, info_y, card.w - 28.0, info_h),
        state.fonts.caption,
        theme::text_dim(),
        DT_CENTER,
    );

    // Scrollable custom domains list
    let list_r = RectF::new(
        card.x + 1.0,
        div_y + 1.0,
        card.w - 2.0,
        (info_y - div_y - 2.0).max(40.0),
    );

    if custom_list.is_empty() {
        state.max_scroll = 0.0;
        state.scroll_custom = 0.0;
        let mid_x = list_r.x + list_r.w * 0.5;
        let mid_y = list_r.cy() - 4.0;
        c.fill_circle(mid_x, mid_y - 24.0, 18.0, theme::surface_inset());
        c.stroke_circle(mid_x, mid_y - 24.0, 18.0, 1.0, theme::border());
        c.draw_plus(mid_x, mid_y - 24.0, 5.5, 1.8, theme::text_muted());
        c.draw_text(
            "Chưa có tên miền tùy chỉnh",
            RectF::new(list_r.x + 16.0, mid_y + 2.0, list_r.w - 32.0, 20.0),
            state.fonts.group,
            theme::text_sec(),
            DT_CENTER,
        );
        c.draw_text(
            "Nhập hoặc dán 1 hay nhiều tên miền vào ô phía trên rồi nhấn Enter",
            RectF::new(list_r.x + 16.0, mid_y + 25.0, list_r.w - 32.0, 18.0),
            state.fonts.caption,
            theme::text_muted(),
            DT_CENTER,
        );
        return;
    }

    const ROW_H: f32 = 58.0;
    let content_h = custom_list.len() as f32 * ROW_H;
    state.max_scroll = (content_h - list_r.h).max(0.0);
    if state.scroll_custom_to_bottom {
        state.scroll_custom = state.max_scroll;
        state.scroll_custom_to_bottom = false;
    } else {
        state.scroll_custom = state.scroll_custom.clamp(0.0, state.max_scroll);
    }

    let scroll_pad = if state.max_scroll > 0.5 { 8.0 } else { 0.0 };
    c.set_clip(Some(list_r));

    let mut y = list_r.y - state.scroll_custom;
    for (idx, item) in custom_list.iter().enumerate() {
        let root = &item.domain;
        let rr = RectF::new(list_r.x, y, list_r.w, ROW_H);
        if idx > 0 && y >= list_r.y - 2.0 && y <= list_r.bottom() + 2.0 {
            c.draw_line(
                list_r.x + 16.0,
                y,
                list_r.right() - 16.0 - scroll_pad,
                y,
                1.0,
                theme::border_sub(),
                false,
            );
        }

        if rr.bottom() > list_r.y && rr.y < list_r.bottom() {
            let exact = snap.iter().find(|(h, _)| h.eq_ignore_ascii_case(root)).map(|(_, s)| s);
            let mut total_hits: u32 = 0;
            let mut any_ok = false;
            let mut any_fail = false;
            let mut best_ip = String::new();
            let mut best_dns = DnsState::Unknown;
            let mut best_ms: Option<u32> = None;

            if let Some(s) = exact {
                best_ip = s.ip.clone();
                best_dns = s.dns;
                best_ms = s.latency_ms;
            }
            for (h, s) in snap {
                if domain_matches(&h.to_ascii_lowercase(), root) {
                    total_hits += s.ok;
                    if host_health(s, pending) == Health::Ok {
                        any_ok = true;
                    } else if host_health(s, pending) == Health::Fail {
                        any_fail = true;
                    }
                    if best_ip.is_empty() && !s.ip.is_empty() {
                        best_ip = s.ip.clone();
                    }
                    if best_ms.is_none() {
                        best_ms = s.latency_ms;
                    }
                    if matches!(s.dns, DnsState::Poisoned | DnsState::Failed) {
                        best_dns = s.dns;
                    }
                }
            }

            let h = if !item.enabled {
                Health::Idle
            } else if any_ok {
                Health::Ok
            } else if any_fail {
                Health::Fail
            } else if pending > 0 {
                Health::Testing
            } else {
                Health::Idle
            };

            let rcy = y + ROW_H * 0.5;
            let dot_x = list_r.x + 20.0;
            if item.enabled && h == Health::Ok {
                c.fill_circle(dot_x, rcy, 7.0, theme::success().with_alpha(34));
            }
            c.fill_circle(dot_x, rcy, 3.8, host_dot_color(h));

            let target_idx = state.row_targets.len();
            state.row_targets.push(root.clone());

            // Right-most delete button (×)
            let del_sz = 26.0;
            let del_r = RectF::new(
                list_r.right() - 14.0 - scroll_pad - del_sz,
                rcy - del_sz * 0.5,
                del_sz,
                del_sz,
            );
            let del_hov = state.hover == Some(Action::RemoveDomain(target_idx));
            if del_hov {
                c.fill_round_rect(del_r, R_SM, DESTRUCTIVE.with_alpha(40));
            }
            c.draw_cross(
                del_r.x + del_sz * 0.5,
                del_r.cy(),
                3.8,
                1.6,
                if del_hov { theme::danger() } else { theme::text_dim() },
            );
            let ht = del_r.y.max(list_r.y);
            let hb = del_r.bottom().min(list_r.bottom());
            if hb > ht {
                state.hits.push((
                    RectF::new(del_r.x, ht, del_r.w, hb - ht),
                    Action::RemoveDomain(target_idx),
                ));
            }

            // Mini pause/resume toggle left of delete button
            let tg_w = 34.0;
            let tg_h = 18.0;
            let tg_r = RectF::new(del_r.x - 10.0 - tg_w, rcy - tg_h * 0.5, tg_w, tg_h);
            let tg_hov = state.hover == Some(Action::ToggleCustomDomain(target_idx));
            draw_mini_toggle(c, tg_r, item.enabled, tg_hov);
            let tht = (tg_r.y - 4.0).max(list_r.y);
            let thb = (tg_r.bottom() + 4.0).min(list_r.bottom());
            if thb > tht {
                state.hits.push((
                    RectF::new(tg_r.x - 3.0, tht, tg_r.w + 6.0, thb - tht),
                    Action::ToggleCustomDomain(target_idx),
                ));
            }

            // Latency metric pill left of mini toggle
            let mut right_limit = tg_r.x - 12.0;
            if item.enabled {
                if let Some(ms) = best_ms {
                    let lat_txt = format!("{ms}ms");
                    let lcol = latency_color(ms);
                    let (ltw, _) = c.measure_text(&lat_txt, state.fonts.mono_bold);
                    let lat_w = (ltw + 12.0).ceil().max(44.0);
                    let lat_h = 20.0;
                    let lat_r = RectF::new(right_limit - lat_w, rcy - lat_h * 0.5, lat_w, lat_h);
                    c.fill_round_rect(lat_r, R_SM, theme::surface_inset());
                    c.stroke_round_rect(lat_r, R_SM, 1.0, lcol.with_alpha(70));
                    c.draw_text(&lat_txt, lat_r, state.fonts.mono_bold, lcol, DT_CENTER);
                    right_limit = lat_r.x - 10.0;
                }
            }

            // Domain title + metadata sub-line
            let text_left = dot_x + 14.0;
            let text_w = (right_limit - text_left).max(60.0);
            c.draw_text(
                root,
                RectF::new(text_left, y + 10.0, text_w, 19.0),
                state.fonts.group,
                if item.enabled { theme::text() } else { theme::text_muted() },
                DT_LEFT,
            );

            let (meta, meta_col) = if !item.enabled {
                ("Đang tạm tắt · Đi thẳng không qua proxy".to_string(), theme::text_dim())
            } else {
                let status_str = match h {
                    Health::Ok => "Đã bypass",
                    Health::Fail => "Bị chặn",
                    Health::Testing => "Đang đo…",
                    _ => "Chờ…",
                };
                let ip_txt = if best_ip.is_empty() {
                    String::new()
                } else {
                    format!(" · {best_ip}")
                };
                let dns_txt = match best_dns {
                    DnsState::Poisoned | DnsState::Failed => " · DNS sửa",
                    DnsState::Clean => " · DNS OK",
                    DnsState::Unknown => "",
                };
                let hit_txt = if total_hits > 0 {
                    format!(" · ×{total_hits}")
                } else {
                    String::new()
                };
                let col = if h == Health::Fail {
                    theme::danger()
                } else if matches!(best_dns, DnsState::Poisoned | DnsState::Failed) {
                    theme::warning()
                } else {
                    theme::text_muted()
                };
                (format!("{status_str}{ip_txt}{dns_txt}{hit_txt}"), col)
            };
            c.draw_text(
                &meta,
                RectF::new(text_left, y + 31.0, text_w, 16.0),
                state.fonts.caption,
                meta_col,
                DT_LEFT,
            );
        }
        y += ROW_H;
    }

    draw_scrollbar(c, list_r, content_h, state.scroll_custom, state.max_scroll);
    c.set_clip(None);
}

fn draw_scrollbar(c: &mut Canvas, area: RectF, content_h: f32, scroll_y: f32, max_scroll: f32) {
    if max_scroll > 0.5 && content_h > area.h {
        let track_h = (area.h - 8.0).max(20.0);
        let thumb_h = ((area.h / content_h) * track_h).clamp(24.0, track_h);
        let ratio = scroll_y / max_scroll;
        let thumb_y = area.y + 4.0 + ratio * (track_h - thumb_h);
        c.fill_round_rect(
            RectF::new(area.right() - 7.0, thumb_y, 4.0, thumb_h),
            2.0,
            WHITE.with_alpha(52),
        );
    }
}

// ---------------- Win32 Window, Edit Control, Clipboard & Tray ----------------
fn get_state<'a>(hwnd: HWND) -> Option<&'a mut UiState> {
    let ptr = unsafe { GetWindowLongPtrW(hwnd, GWLP_USERDATA) } as *mut UiState;
    if ptr.is_null() { None } else { Some(unsafe { &mut *ptr }) }
}

/// If clipboard contains multi-line text, collapse newlines into `, ` and paste into the single-line EDIT control.
fn try_paste_multiline(hwnd: HWND, edit_hwnd: HWND) -> bool {
    unsafe {
        if OpenClipboard(hwnd) == 0 {
            return false;
        }
        let mut handled = false;
        let hdata = GetClipboardData(CF_UNICODETEXT);
        if !hdata.is_null() {
            let ptr = GlobalLock(hdata) as *const u16;
            if !ptr.is_null() {
                let mut len = 0usize;
                while len < 8192 && *ptr.add(len) != 0 {
                    len += 1;
                }
                let text = String::from_utf16_lossy(std::slice::from_raw_parts(ptr, len));
                GlobalUnlock(hdata);
                if text.contains('\n') || text.contains('\r') {
                    let flat = text
                        .lines()
                        .map(str::trim)
                        .filter(|s| !s.is_empty())
                        .collect::<Vec<_>>()
                        .join(", ");
                    let w = wide(&flat);
                    SendMessageW(edit_hwnd, EM_REPLACESEL, 1, w.as_ptr() as isize);
                    handled = true;
                }
            }
        }
        CloseClipboard();
        handled
    }
}

fn layout_edit(hwnd: HWND, state: &UiState) {
    if state.edit_hwnd.is_null() {
        return;
    }
    unsafe {
        let mut cr = RECT { left: 0, top: 0, right: 0, bottom: 0 };
        GetClientRect(hwnd, &mut cr);
        let scale = state.dpi as f32 / 96.0;
        let lw = ((cr.right - cr.left).max(1) as f32 / scale).max(320.0);
        let inp = custom_input_rect(lw);
        let eh = 18.0;
        let px = ((inp.x + 10.0) * scale).round() as i32;
        let py = ((inp.cy() - eh * 0.5) * scale).round() as i32;
        let pw = ((inp.w - 20.0).max(40.0) * scale).round() as i32;
        let ph = (eh * scale).round().max(14.0) as i32;
        SetWindowPos(
            state.edit_hwnd,
            null_mut(),
            px,
            py,
            pw,
            ph,
            SWP_NOZORDER | SWP_NOACTIVATE,
        );
    }
}

fn sync_edit_visibility(hwnd: HWND, state: &mut UiState) {
    if state.edit_hwnd.is_null() {
        return;
    }
    unsafe {
        if state.tab == Tab::Custom {
            layout_edit(hwnd, state);
            if state.edit_focused || state.edit_has_text {
                ShowWindow(state.edit_hwnd, SW_SHOW);
            } else {
                ShowWindow(state.edit_hwnd, SW_HIDE);
            }
        } else {
            if state.edit_focused {
                SetFocus(hwnd);
                state.edit_focused = false;
            }
            ShowWindow(state.edit_hwnd, SW_HIDE);
        }
    }
}

fn read_edit_text(edit: HWND) -> String {
    if edit.is_null() {
        return String::new();
    }
    unsafe {
        let len = GetWindowTextLengthW(edit);
        if len <= 0 {
            return String::new();
        }
        let mut buf = vec![0u16; len as usize + 1];
        let got = GetWindowTextW(edit, buf.as_mut_ptr(), buf.len() as i32);
        String::from_utf16_lossy(&buf[..got.max(0) as usize])
    }
}

fn set_notice(hwnd: HWND, state: &mut UiState, msg: String, color: Color) {
    state.notice = Some((msg, color));
    unsafe {
        SetTimer(hwnd, ID_TIMER_NOTICE, 3500, None);
    }
}

fn focus_domain_input(hwnd: HWND, state: &mut UiState) {
    if state.edit_hwnd.is_null() || state.tab != Tab::Custom {
        return;
    }
    layout_edit(hwnd, state);
    unsafe {
        ShowWindow(state.edit_hwnd, SW_SHOW);
        SetFocus(state.edit_hwnd);
    }
    state.edit_focused = true;
}

fn apply_batch_add_feedback(hwnd: HWND, state: &mut UiState, res: &proxy::BatchAddResult) {
    if !res.added.is_empty() {
        state.scroll_custom_to_bottom = true;
        let msg = if res.added.len() == 1 {
            format!("Đã thêm {} · Đang kiểm tra…", res.added[0])
        } else {
            format!("Đã thêm {} tên miền · Đang kiểm tra…", res.added.len())
        };
        set_notice(hwnd, state, msg, theme::success());
        sync_timer(hwnd, state);
    } else if let Some(err) = res.first_err {
        set_notice(hwnd, state, err.to_string(), theme::warning());
    }
}

fn submit_domain_input(hwnd: HWND) {
    let Some(state) = get_state(hwnd) else {
        return;
    };
    if state.tab != Tab::Custom {
        return;
    }
    let raw = read_edit_text(state.edit_hwnd);
    if raw.trim().is_empty() {
        focus_domain_input(hwnd, state);
        unsafe {
            InvalidateRect(hwnd, null(), 0);
        }
        return;
    }
    let res = proxy::add_custom_domains(&raw);
    if !res.added.is_empty() {
        unsafe {
            SetWindowTextW(state.edit_hwnd, wide("").as_ptr());
            if !state.edit_focused {
                ShowWindow(state.edit_hwnd, SW_HIDE);
            }
        }
        state.edit_has_text = false;
    }
    apply_batch_add_feedback(hwnd, state, &res);
    unsafe {
        InvalidateRect(hwnd, null(), 0);
    }
}

fn toggle_direct_cdn_ui(hwnd: HWND) {
    let next = !DIRECT_CDN.load(Ordering::SeqCst);
    proxy::set_direct_cdn(next);
    if let Some(state) = get_state(hwnd) {
        let msg = if next {
            "CDN tải game đi thẳng · Giữ max tốc độ nội mạng".to_string()
        } else {
            "CDN tải game đi qua proxy bypass".to_string()
        };
        set_notice(hwnd, state, msg, if next { theme::info() } else { theme::text_sec() });
    }
    unsafe {
        InvalidateRect(hwnd, null(), 0);
    }
}

const DOH_URL_FIELD: i32 = 202;
const DOH_HELP: i32 = 205;
const DOH_TEST: i32 = 206;
const DOH_TEST_TIMER: usize = 1;
const DOH_INVALID_URL: &str = "Nhập URL HTTPS đầy đủ, gồm tên máy chủ và đường dẫn DoH.";

type DohProbeResult = Result<usize, proxy::DohError>;
const DOH_EXAMPLE: &str = "Ví dụ: https://dns.google/dns-query";

struct DohDialog {
    dpi: u32,
    fonts: FontSet,
    background: HBRUSH,
    input: HBRUSH,
    help_color: std::cell::Cell<Color>,
    probe: std::cell::RefCell<Option<std::sync::mpsc::Receiver<DohProbeResult>>>,
    probe_matches_input: std::cell::Cell<bool>,
}

impl DohDialog {
    fn new(dpi: u32) -> Self {
        Self {
            dpi,
            fonts: FontSet::new(dpi),
            background: unsafe { CreateSolidBrush(theme::surface().colorref()) },
            input: unsafe { CreateSolidBrush(theme::surface_inset().colorref()) },
            help_color: std::cell::Cell::new(theme::text_muted()),
            probe: std::cell::RefCell::new(None),
            probe_matches_input: std::cell::Cell::new(false),
        }
    }

    fn show_help(&self, hwnd: HWND, message: &str, color: Color) {
        self.help_color.set(color);
        unsafe {
            SetDlgItemTextW(hwnd, DOH_HELP, wide(message).as_ptr());
            InvalidateRect(GetDlgItem(hwnd, DOH_HELP), null(), 1);
        }
    }
}

impl Drop for DohDialog {
    fn drop(&mut self) {
        unsafe {
            DeleteObject(self.background);
            DeleteObject(self.input);
        }
    }
}

fn doh_dialog_url(hwnd: HWND) -> Option<String> {
    let mut buf = [0u16; 2050];
    let len = unsafe { GetDlgItemTextW(hwnd, DOH_URL_FIELD, buf.as_mut_ptr(), buf.len() as i32) } as usize;
    let value = String::from_utf16_lossy(&buf[..len]);
    let url = value.trim();
    if len >= 2049 || proxy::parse_endpoint(url).is_none() {
        return None;
    }
    Some(url.to_owned())
}

fn doh_probe_error(error: proxy::DohError) -> String {
    use proxy::DohError::*;
    match error {
        InvalidUrl => DOH_INVALID_URL.into(),
        Certificate => "Không xác thực được chứng chỉ TLS của máy chủ.".into(),
        Timeout => "Hết thời gian chờ phản hồi từ máy chủ DoH.".into(),
        InvalidResponse => "Máy chủ trả về dữ liệu DoH không hợp lệ.".into(),
        NoAddress => "Máy chủ phản hồi nhưng không trả về địa chỉ IPv4 cho example.org.".into(),
        HttpStatus(code) => format!("Máy chủ trả về HTTP {code}. Hãy kiểm tra đường dẫn DoH."),
        Connection(0) => "Không thể khởi tạo kết nối DoH.".into(),
        Connection(code) => format!("Không thể kết nối tới máy chủ DoH (mã Windows {code})."),
    }
}

fn start_doh_probe(hwnd: HWND, state: &DohDialog) {
    if state.probe.borrow().is_some() {
        return;
    }
    let Some(url) = doh_dialog_url(hwnd) else {
        state.show_help(hwnd, DOH_INVALID_URL, theme::danger());
        unsafe { SetFocus(GetDlgItem(hwnd, DOH_URL_FIELD)); }
        return;
    };

    unsafe {
        if SetTimer(hwnd, DOH_TEST_TIMER, 75, None) == 0 {
            state.show_help(hwnd, "Không thể bắt đầu thử kết nối. Hãy thử lại.", theme::danger());
            return;
        }
        let (tx, rx) = std::sync::mpsc::channel();
        *state.probe.borrow_mut() = Some(rx);
        state.probe_matches_input.set(true);
        let button = GetDlgItem(hwnd, DOH_TEST);
        if GetFocus() == button {
            SetFocus(GetDlgItem(hwnd, DOH_URL_FIELD));
        }
        EnableWindow(button, 0);
        state.show_help(hwnd, "Đang thử phân giải example.org…", theme::info());
        let _ = std::thread::Builder::new().spawn(move || {
            let _ = tx.send(proxy::test_custom_doh(&url));
        });
    }
}

fn poll_doh_probe(hwnd: HWND, state: &DohDialog) {
    use std::sync::mpsc::TryRecvError;
    let result = state.probe.borrow().as_ref().map(|rx| rx.try_recv());
    let result = match result {
        Some(Ok(result)) => result,
        Some(Err(TryRecvError::Disconnected)) => Err(proxy::DohError::Connection(0)),
        _ => return,
    };
    state.probe.borrow_mut().take();
    unsafe {
        KillTimer(hwnd, DOH_TEST_TIMER);
        EnableWindow(GetDlgItem(hwnd, DOH_TEST), 1);
    }
    if state.probe_matches_input.get() {
        match result {
            Ok(count) => state.show_help(
                hwnd, &format!("Kết nối thành công · example.org trả về {count} địa chỉ IPv4."), theme::success(),
            ),
            Err(error) => state.show_help(hwnd, &doh_probe_error(error), theme::danger()),
        }
    }
}

unsafe extern "system" fn custom_doh_dialog(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> isize {
    // DialogBoxParamW owns the message loop, so its caller keeps this state alive.
    unsafe {
        let state = GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *const DohDialog;
        match msg {
            WM_INITDIALOG => {
                SetWindowLongPtrW(hwnd, GWLP_USERDATA, lp);
                let state = &*(lp as *const DohDialog);
                apply_dwm(hwnd);
                SetWindowTextW(hwnd, wide("Custom DoH").as_ptr());
                let labels = [
                    (203, "Máy chủ DoH tùy chỉnh", state.fonts.title),
                    (204, "Địa chỉ máy chủ (HTTPS)", state.fonts.group),
                    (DOH_HELP, DOH_EXAMPLE, state.fonts.caption),
                    (DOH_TEST, "Thử kết nối", state.fonts.badge),
                    (IDCANCEL, "Hủy", state.fonts.badge),
                    (IDOK, "Lưu và áp dụng", state.fonts.badge),
                ];
                for (id, label, font) in labels {
                    SetDlgItemTextW(hwnd, id, wide(label).as_ptr());
                    SendDlgItemMessageW(hwnd, id, WM_SETFONT, font as usize, 0);
                }
                let saved = proxy::custom_url();
                SetDlgItemTextW(hwnd, DOH_URL_FIELD, wide(&saved).as_ptr());
                SendDlgItemMessageW(hwnd, DOH_URL_FIELD, WM_SETFONT, state.fonts.body as usize, 0);
                SendDlgItemMessageW(hwnd, DOH_URL_FIELD, 0x00B1, 0, -1); // EM_SETSEL
                SendMessageW(hwnd, DM_SETDEFID, IDOK as usize, 0);
                1
            }
            WM_CTLCOLORDLG if !state.is_null() => (*state).background as isize,
            WM_CTLCOLORSTATIC | WM_CTLCOLOREDIT if !state.is_null() => {
                let hdc = wp as HDC;
                let edit = msg == WM_CTLCOLOREDIT;
                let hint = GetDlgCtrlID(lp as HWND) == DOH_HELP;
                let color = if hint { (*state).help_color.get() } else { theme::text() };
                SetTextColor(hdc, color.colorref());
                SetBkColor(hdc, if edit { theme::surface_inset() } else { theme::surface() }.colorref());
                if edit { (*state).input as isize } else { (*state).background as isize }
            }
            WM_DRAWITEM if !state.is_null() && matches!(wp as i32, IDOK | IDCANCEL | DOH_TEST) => {
                let item = &*(lp as *const DRAWITEMSTRUCT);
                let state = &*state;
                let scale = state.dpi as f32 / 96.0;
                let bounds = item.rcItem;
                FillRect(item.hDC, &bounds, state.background);
                let rect = RectF::new(bounds.left as f32 / scale, bounds.top as f32 / scale,
                    (bounds.right - bounds.left) as f32 / scale, (bounds.bottom - bounds.top) as f32 / scale);
                let mut c = Canvas::new(item.hDC, state.dpi);
                let primary = wp == IDOK as usize;
                let label = match wp as i32 {
                    IDOK => "Lưu và áp dụng",
                    DOH_TEST if item.itemState & ODS_DISABLED != 0 => "Đang thử…",
                    DOH_TEST => "Thử kết nối",
                    _ => "Hủy",
                };
                paint_utility_button(&mut c, &state.fonts, rect, label,
                    primary, item.itemState & ODS_SELECTED != 0);
                if item.itemState & ODS_FOCUS != 0 {
                    c.stroke_round_rect(RectF::new(rect.x + 3.0, rect.y + 3.0, rect.w - 6.0, rect.h - 6.0),
                        R_SM - 2.0, 1.0, theme::info());
                }
                1
            }
            WM_TIMER if wp == DOH_TEST_TIMER && !state.is_null() => {
                poll_doh_probe(hwnd, &*state);
                1
            }
            WM_COMMAND if !state.is_null() => match (wp & 0xffff) as i32 {
                IDOK => {
                    let state = &*state;
                    if let Some(url) = doh_dialog_url(hwnd) {
                        if proxy::save_custom_url(&url) {
                            EndDialog(hwnd, IDOK as isize);
                        } else {
                            state.show_help(hwnd, "Không thể lưu địa chỉ máy chủ. Hãy thử lại.", theme::danger());
                        }
                    } else {
                        state.show_help(hwnd, DOH_INVALID_URL, theme::danger());
                        SetFocus(GetDlgItem(hwnd, DOH_URL_FIELD));
                    }
                    1
                }
                DOH_TEST => {
                    start_doh_probe(hwnd, &*state);
                    1
                }
                DOH_URL_FIELD if (wp >> 16) as u32 == EN_CHANGE => {
                    let state = &*state;
                    state.probe_matches_input.set(false);
                    state.show_help(hwnd, DOH_EXAMPLE, theme::text_muted());
                    1
                }
                IDCANCEL => { EndDialog(hwnd, IDCANCEL as isize); 1 }
                _ => 0,
            },
            WM_CLOSE => { EndDialog(hwnd, IDCANCEL as isize); 1 }
            _ => 0,
        }
    }
}

fn select_doh_ui(hwnd: HWND, provider: DohProvider) {
    if provider == DohProvider::Custom {
        let dialog = DohDialog::new(unsafe { GetDpiForWindow(hwnd) }.max(96));
        let result = unsafe {
            DialogBoxParamW(GetModuleHandleW(null()), 201usize as *const u16, hwnd, Some(custom_doh_dialog),
                (&dialog as *const DohDialog) as LPARAM)
        };
        if result != IDOK as isize { return; }
    } else if proxy::doh_provider() == provider {
        return;
    }
    if !proxy::set_doh_provider(provider) {
        unsafe { MessageBoxW(hwnd, wide("Không thể lưu lựa chọn DoH.").as_ptr(), wide("DoH").as_ptr(), MB_OK); }
        return;
    }
    if ENABLED.load(Ordering::SeqCst) {
        proxy::run_test();
    }
    if let Some(state) = get_state(hwnd) {
        let msg = format!("DoH {} ({}) · Đang đo lại…", provider.label(), provider.addr());
        set_notice(hwnd, state, msg, theme::info());
        sync_timer(hwnd, state);
    }
    unsafe {
        InvalidateRect(hwnd, null(), 0);
    }
}

fn toggle_autostart_ui(hwnd: HWND) {
    let Some(state) = get_state(hwnd) else {
        return;
    };
    let next = !state.autostart;
    if config::set_autostart(next) {
        state.autostart = next;
        let msg = if next {
            "Đã bật khởi chạy cùng Windows (chạy ẩn)".to_string()
        } else {
            "Đã tắt khởi chạy cùng Windows".to_string()
        };
        set_notice(hwnd, state, msg, if next { theme::success() } else { theme::text_sec() });
    } else {
        set_notice(hwnd, state, "Không thể cập nhật khởi chạy cùng Windows".to_string(), theme::danger());
    }
    unsafe {
        InvalidateRect(hwnd, null(), 0);
    }
}

fn reset_network_ui(hwnd: HWND) {
    proxy::reset_network_proxy();
    update_tray_tip(hwnd);
    if let Some(state) = get_state(hwnd) {
        state.last_tray_on = false;
        sync_timer(hwnd, state);
        set_notice(
            hwnd,
            state,
            "Đã khôi phục cấu hình mạng mặc định (gỡ proxy)".to_string(),
            theme::success(),
        );
    }
    unsafe {
        InvalidateRect(hwnd, null(), 0);
    }
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
        if let Some(st) = get_state(hwnd) {
            sync_edit_visibility(hwnd, st);
        }
        InvalidateRect(hwnd, null(), 0);
    }
}

fn apply_dwm(hwnd: HWND) {
    unsafe {
        let dark = u32::from(!is_light());
        if DwmSetWindowAttribute(
            hwnd,
            DWMWA_USE_IMMERSIVE_DARK_MODE as u32,
            (&dark as *const u32).cast(),
            4,
        ) != 0
        {
            let _ = DwmSetWindowAttribute(hwnd, 19, (&dark as *const u32).cast(), 4);
        }
        let cap = theme::bg().colorref();
        let _ = DwmSetWindowAttribute(hwnd, DWMWA_CAPTION_COLOR as u32, (&cap as *const u32).cast(), 4);
        let brd = theme::border().colorref();
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

const ID_LIGHT: usize = 8;
const ID_DARK: usize = 9;
const ID_UPDATE: usize = 10;
const WM_UPDATE_DONE: u32 = WM_APP + 6;

fn utilities_menu(hwnd: HWND) -> HMENU {
    unsafe {
        let menu = CreatePopupMenu();
        let checked = |on| if on { MF_CHECKED } else { 0 };
        let state = get_state(hwnd);
        let auto = state.as_ref().is_some_and(|s| s.autostart);
        let busy = state.as_ref().is_some_and(|s| s.update.is_some());
        AppendMenuW(menu, MF_STRING | checked(DIRECT_CDN.load(Ordering::SeqCst)), ID_MENU_DIRECT_CDN,
            wide("Đi thẳng CDN tải game").as_ptr());
        AppendMenuW(menu, MF_STRING | checked(auto), ID_MENU_AUTOSTART, wide("Khởi chạy cùng Windows").as_ptr());
        AppendMenuW(menu, MF_SEPARATOR, 0, null());
        AppendMenuW(menu, MF_STRING | checked(is_light()), ID_LIGHT, wide("Giao diện sáng").as_ptr());
        AppendMenuW(menu, MF_STRING | checked(!is_light()), ID_DARK, wide("Giao diện tối").as_ptr());
        AppendMenuW(menu, MF_SEPARATOR, 0, null());
        let label = if busy { "Đang kiểm tra cập nhật…" } else { "Kiểm tra cập nhật…" };
        AppendMenuW(menu, if busy { MF_GRAYED } else { MF_STRING }, ID_UPDATE, wide(label).as_ptr());
        menu
    }
}

fn update_failed() -> String {
    "Không thể kiểm tra cập nhật. Hãy kiểm tra kết nối rồi thử lại.".into()
}

fn utility_command(hwnd: HWND, command: usize) {
    match command {
        ID_MENU_AUTOSTART => toggle_autostart_ui(hwnd),
        ID_MENU_DIRECT_CDN => toggle_direct_cdn_ui(hwnd),
        ID_LIGHT | ID_DARK => unsafe {
            let light = command == ID_LIGHT;
            if light == is_light() { return; }
            if !config::save_light_theme(light) {
                MessageBoxW(hwnd, wide("Không thể lưu giao diện.").as_ptr(), wide("Tiện ích").as_ptr(), MB_OK);
                return;
            }
            set_light(light);
            if let Some(s) = get_state(hwnd) {
                let brush = CreateSolidBrush(surface_inset().colorref());
                if !brush.is_null() {
                    DeleteObject(std::mem::replace(&mut s.edit_bg_brush, brush));
                }
                if !s.edit_hwnd.is_null() { InvalidateRect(s.edit_hwnd, null(), 1); }
            }
            apply_dwm(hwnd);
            InvalidateRect(hwnd, null(), 1);
        },
        ID_UPDATE => {
            let Some(s) = get_state(hwnd) else { return };
            if s.update.is_some() { return; }
            let (tx, rx) = std::sync::mpsc::channel();
            s.update = Some(rx);
            unsafe { InvalidateRect(hwnd, null(), 0); }
            let window = hwnd as usize;
            std::thread::spawn(move || {
                let result = crate::update::check().unwrap_or_else(update_failed);
                if tx.send(result).is_ok() {
                    unsafe { PostMessageW(window as HWND, WM_UPDATE_DONE, 0, 0); }
                }
            });
        }
        _ => {}
    }
}

fn show_tray_menu(hwnd: HWND) {
    let on = ENABLED.load(Ordering::SeqCst);
    let cur_doh = proxy::doh_provider();
    let pending = PENDING.load(Ordering::SeqCst) > 0;
    unsafe {
        let menu = CreatePopupMenu();
        let doh_menu = CreatePopupMenu();
        for (idx, &prov) in DohProvider::ALL.iter().enumerate() {
            let item_txt = if prov == DohProvider::Custom { "Custom…".to_owned() } else { format!("{} ({})", prov.label(), prov.addr()) };
            AppendMenuW(
                doh_menu,
                MF_STRING | if prov == cur_doh { MF_CHECKED } else { 0 },
                ID_MENU_DOH_BASE + idx,
                wide(&item_txt).as_ptr(),
            );
        }

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
        AppendMenuW(
            menu,
            MF_POPUP,
            doh_menu as usize,
            wide(&format!("Máy chủ DoH: {}", cur_doh.label())).as_ptr(),
        );
        AppendMenuW(
            menu,
            MF_POPUP,
            utilities_menu(hwnd) as usize,
            wide("Tiện ích").as_ptr(),
        );
        AppendMenuW(
            menu,
            MF_STRING,
            ID_MENU_RESET_NET,
            wide("Khôi phục mạng (Gỡ proxy)").as_ptr(),
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

        let cmd_id = cmd as usize;
        if cmd_id >= ID_MENU_DOH_BASE && cmd_id < ID_MENU_DOH_BASE + DohProvider::ALL.len() {
            select_doh_ui(hwnd, DohProvider::from_index(cmd_id - ID_MENU_DOH_BASE));
            return;
        }

        match cmd_id {
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
                proxy::clear_dns_cache();
                proxy::run_test();
                if let Some(st) = get_state(hwnd) {
                    sync_timer(hwnd, st);
                }
                InvalidateRect(hwnd, null(), 0);
            }
            ID_MENU_DIRECT_CDN | ID_MENU_AUTOSTART | ID_LIGHT | ID_DARK | ID_UPDATE => utility_command(hwnd, cmd_id),
            ID_MENU_RESET_NET => reset_network_ui(hwnd),
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
            WM_SIZE => {
                if let Some(state) = get_state(hwnd) {
                    layout_edit(hwnd, state);
                }
                0
            }
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
                        if !state.edit_hwnd.is_null() {
                            SendMessageW(state.edit_hwnd, WM_SETFONT, state.fonts.body as usize, 1);
                        }
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
                    layout_edit(hwnd, state);
                    InvalidateRect(hwnd, null(), 0);
                }
                0
            }
            WM_CTLCOLOREDIT => {
                let hdc = wp as HDC;
                SetTextColor(hdc, theme::text().colorref());
                SetBkColor(hdc, theme::surface_inset().colorref());
                if let Some(state) = get_state(hwnd) {
                    return state.edit_bg_brush as LRESULT;
                }
                DefWindowProcW(hwnd, msg, wp, lp)
            }
            WM_COMMAND => {
                let ctrl_id = (wp & 0xFFFF) as usize;
                let code = ((wp >> 16) & 0xFFFF) as u32;
                if ctrl_id == ID_EDIT_DOMAIN {
                    if let Some(state) = get_state(hwnd) {
                        match code {
                            EN_SETFOCUS => {
                                state.edit_focused = true;
                                InvalidateRect(hwnd, null(), 0);
                            }
                            EN_KILLFOCUS => {
                                state.edit_focused = false;
                                state.edit_has_text = GetWindowTextLengthW(state.edit_hwnd) > 0;
                                if !state.edit_has_text {
                                    ShowWindow(state.edit_hwnd, SW_HIDE);
                                }
                                InvalidateRect(hwnd, null(), 0);
                            }
                            EN_CHANGE => {
                                state.edit_has_text = GetWindowTextLengthW(state.edit_hwnd) > 0;
                            }
                            _ => {}
                        }
                    }
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
                    let custom_list = proxy::custom_domains();
                    let on = ENABLED.load(Ordering::SeqCst);
                    let direct_cdn = DIRECT_CDN.load(Ordering::SeqCst);
                    let doh = proxy::doh_provider();
                    let pending = PENDING.load(Ordering::SeqCst);
                    paint_ui(mem_dc, pw, ph, state, &snap, &custom_list, on, direct_cdn, doh, pending);
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
                    if state.hover.is_some() {
                        state.hover = None;
                        InvalidateRect(hwnd, null(), 0);
                    }
                }
                0
            }
            WM_SETCURSOR => {
                if (lp as u32 & 0xFFFF) == HTCLIENT {
                    let cur_id = match get_state(hwnd).and_then(|s| s.hover) {
                        Some(Action::FocusInput) => IDC_IBEAM,
                        Some(_) => IDC_HAND,
                        None => IDC_ARROW,
                    };
                    let cur = LoadCursorW(null_mut(), cur_id);
                    SetCursor(cur);
                    return 1;
                }
                DefWindowProcW(hwnd, msg, wp, lp)
            }
            WM_LBUTTONDOWN => {
                if let Some(state) = get_state(hwnd) {
                    let s = state.dpi as f32 / 96.0;
                    let lx = ((lp as u32 & 0xFFFF) as i16) as f32 / s;
                    let ly = (((lp as u32 >> 16) & 0xFFFF) as i16) as f32 / s;
                    let hit = hit_test(state, lx, ly);
                    if hit == Some(Action::FocusInput) {
                        focus_domain_input(hwnd, state);
                        InvalidateRect(hwnd, null(), 0);
                    } else if state.edit_focused && hit != Some(Action::AddDomain) {
                        SetFocus(hwnd);
                    }
                }
                0
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
                        Action::SelectDoh(idx) => {
                            select_doh_ui(hwnd, DohProvider::from_index(idx));
                            return 0;
                        }
                        Action::SelectTab(tab) => {
                            if let Some(state) = get_state(hwnd) {
                                if state.tab != tab {
                                    state.tab = tab;
                                    state.hover = None;
                                    sync_edit_visibility(hwnd, state);
                                }
                            }
                        }
                        Action::RunTest => {
                            proxy::clear_dns_cache();
                            proxy::run_test();
                            if let Some(state) = get_state(hwnd) {
                                sync_timer(hwnd, state);
                            }
                        }
                        Action::FocusInput => {
                            if let Some(state) = get_state(hwnd) {
                                focus_domain_input(hwnd, state);
                            }
                        }
                        Action::AddDomain => {
                            submit_domain_input(hwnd);
                            return 0;
                        }
                        Action::ToggleGroup(gi) => {
                            if let Some(state) = get_state(hwnd) {
                                state.expanded[gi] = !state.expanded[gi];
                            }
                        }
                        Action::ToggleCustomDomain(idx) => {
                            let domain = get_state(hwnd)
                                .and_then(|s| s.row_targets.get(idx).cloned());
                            if let Some(d) = domain {
                                if let Some(now_on) = proxy::toggle_custom_domain(&d) {
                                    if let Some(state) = get_state(hwnd) {
                                        let msg = if now_on {
                                            format!("Đã bật lại bypass cho {d}")
                                        } else {
                                            format!("Đã tạm tắt bypass cho {d}")
                                        };
                                        set_notice(hwnd, state, msg, if now_on { theme::success() } else { theme::text_sec() });
                                        sync_timer(hwnd, state);
                                    }
                                }
                            }
                        }
                        Action::RemoveDomain(idx) => {
                            let domain = get_state(hwnd)
                                .and_then(|s| s.row_targets.get(idx).cloned());
                            if let Some(d) = domain {
                                if proxy::remove_custom_domain(&d) {
                                    if let Some(state) = get_state(hwnd) {
                                        set_notice(hwnd, state, format!("Đã xóa {d}"), theme::text_sec());
                                    }
                                }
                            }
                        }
                        Action::Utility(command) => {
                            utility_command(hwnd, command);
                            return 0;
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
                        let max_s = state.max_scroll;
                        if let Some(cur) = state.current_scroll_mut() {
                            *cur = (*cur - (delta / 120.0) * 34.0).clamp(0.0, max_s);
                            InvalidateRect(hwnd, null(), 0);
                        }
                    }
                }
                0
            }
            WM_TIMER if wp == ID_TIMER_ANIM => {
                if let Some(state) = get_state(hwnd) {
                    let target = if ENABLED.load(Ordering::SeqCst) { 1.0 } else { 0.0 };
                    let diff = target - state.toggle_t;
                    // Subtle Motion (2/10): crisp ease-out (~140ms) without overshoot
                    if diff.abs() <= 0.015 {
                        state.toggle_t = target;
                    } else {
                        state.toggle_t += diff * 0.35;
                    }
                    if PENDING.load(Ordering::SeqCst) > 0 {
                        state.spin_deg = (state.spin_deg + 8.0) % 360.0;
                    }
                    sync_timer(hwnd, state);
                    InvalidateRect(hwnd, null(), 0);
                }
                0
            }
            WM_TIMER if wp == ID_TIMER_NOTICE => {
                KillTimer(hwnd, ID_TIMER_NOTICE);
                if let Some(state) = get_state(hwnd) {
                    state.notice = None;
                }
                InvalidateRect(hwnd, null(), 0);
                0
            }
            WM_POWERBROADCAST => {
                if wp == PBT_APMRESUMEAUTOMATIC || wp == PBT_APMRESUMESUSPEND {
                    proxy::on_system_resume();
                }
                1
            }
            WM_APP_NET_CHANGED => {
                if let Some(state) = get_state(hwnd) {
                    let msg = if wp == 1 {
                        "Đã làm mới kết nối sau khi thức dậy".to_string()
                    } else {
                        "Mạng vừa đổi · Đang kiểm tra lại…".to_string()
                    };
                    set_notice(hwnd, state, msg, theme::info());
                    sync_timer(hwnd, state);
                }
                InvalidateRect(hwnd, null(), 0);
                0
            }
            WM_UPDATE_DONE => {
                let rx = get_state(hwnd).and_then(|s| s.update.take());
                if let Some(rx) = rx {
                    let message = rx.try_recv().unwrap_or_else(|_| update_failed());
                    InvalidateRect(hwnd, null(), 0);
                    MessageBoxW(hwnd, wide(&message).as_ptr(), wide("Kiểm tra cập nhật").as_ptr(), MB_OK);
                }
                0
            }
            WM_APP_ADD_DOMAIN => {
                submit_domain_input(hwnd);
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
                KillTimer(hwnd, ID_TIMER_NOTICE);
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

pub fn run(start_minimized: bool) {
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
        let autostart = config::is_autostart_enabled();
        set_light(config::light_theme());
        let state = Box::new(UiState::new(
            96,
            ENABLED.load(Ordering::SeqCst),
            autostart,
            taskbar_msg,
        ));
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
        let edit_hwnd = CreateWindowExW(
            0,
            wide("EDIT").as_ptr(),
            null(),
            WS_CHILD | ES_LEFT | ES_AUTOHSCROLL,
            0,
            0,
            100,
            20,
            hwnd,
            ID_EDIT_DOMAIN as HMENU,
            hinst,
            null(),
        );
        if let Some(st) = get_state(hwnd) {
            st.dpi = dpi;
            st.fonts = FontSet::new(dpi);
            st.edit_hwnd = edit_hwnd;
            if !edit_hwnd.is_null() {
                SendMessageW(edit_hwnd, WM_SETFONT, st.fonts.body as usize, 1);
            }
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
        SetWindowPos(hwnd, null_mut(), sx, sy, ww, wh, SWP_NOZORDER | SWP_NOACTIVATE);
        if let Some(st) = get_state(hwnd) {
            layout_edit(hwnd, st);
        }

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

        if start_minimized {
            ShowWindow(hwnd, SW_HIDE);
        } else {
            ShowWindow(hwnd, SW_SHOW);
            UpdateWindow(hwnd);
        }

        let mut msg: MSG = std::mem::zeroed();
        while GetMessageW(&mut msg, null_mut(), 0, 0) > 0 {
            if msg.message == WM_KEYDOWN && !edit_hwnd.is_null() && msg.hwnd == edit_hwnd {
                if msg.wParam == VK_RETURN as usize {
                    SendMessageW(hwnd, WM_APP_ADD_DOMAIN, 0, 0);
                    continue;
                } else if msg.wParam == VK_ESCAPE as usize {
                    SetFocus(hwnd);
                    continue;
                } else if msg.wParam == b'V' as usize
                    && (GetKeyState(VK_CONTROL as i32) as u16 & 0x8000) != 0
                {
                    if try_paste_multiline(hwnd, edit_hwnd) {
                        continue;
                    }
                }
            }
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

    fn make_stat(group: usize, ok: u32, fail: u32, ip: &str, dns: DnsState, latency_ms: Option<u32>) -> Stat {
        Stat {
            group,
            ok,
            fail,
            ip: ip.to_string(),
            dns,
            last_ok: ok > 0 && fail == 0,
            latency_ms,
        }
    }

    fn sample_snap(with_custom: bool) -> Vec<(String, Stat)> {
        let mut v: Vec<(String, Stat)> = vec![
            ("store.steampowered.com".into(), make_stat(0, 4, 0, "23.15.142.182", DnsState::Poisoned, Some(28))),
            ("steamcommunity.com".into(), make_stat(1, 3, 0, "23.15.140.216", DnsState::Poisoned, Some(34))),
            ("api.steampowered.com".into(), make_stat(2, 2, 0, "23.35.148.174", DnsState::Clean, Some(26))),
            ("help.steampowered.com".into(), make_stat(2, 1, 0, "184.87.106.51", DnsState::Clean, Some(41))),
            ("login.steampowered.com".into(), make_stat(2, 5, 0, "23.35.148.153", DnsState::Poisoned, Some(24))),
            ("steamcommunity-a.akamaihd.net".into(), make_stat(3, 6, 0, "104.84.150.191", DnsState::Clean, Some(14))),
            ("steamcdn-a.akamaihd.net".into(), make_stat(3, 2, 0, "23.208.12.168", DnsState::Clean, Some(18))),
            ("avatars.akamai.steamstatic.com".into(), make_stat(4, 3, 0, "23.200.230.152", DnsState::Clean, Some(22))),
            ("cdn.akamai.steamstatic.com".into(), make_stat(4, 2, 0, "23.200.230.169", DnsState::Clean, Some(25))),
            ("cdn.cloudflare.steamstatic.com".into(), make_stat(4, 4, 0, "104.84.150.196", DnsState::Clean, Some(19))),
        ];
        if with_custom {
            v.push(("bbc.com".into(), make_stat(CUSTOM_GROUP_IDX, 3, 0, "151.101.0.81", DnsState::Clean, Some(46))));
            v.push(("medium.com".into(), make_stat(CUSTOM_GROUP_IDX, 2, 0, "162.159.152.4", DnsState::Poisoned, Some(78))));
            v.push(("imgur.com".into(), make_stat(CUSTOM_GROUP_IDX, 1, 0, "146.75.36.193", DnsState::Clean, Some(39))));
        }
        v.sort_by(|a, b| a.1.group.cmp(&b.1.group).then(a.0.cmp(&b.0)));
        v
    }

    #[allow(clippy::too_many_arguments)]
    fn render_bmp(
        path: &str,
        dpi: u32,
        tab: Tab,
        on: bool,
        direct_cdn: bool,
        doh: DohProvider,
        autostart: bool,
        expand_group: Option<usize>,
        snap: &[(String, Stat)],
        custom_list: &[CustomDomain],
    ) {
        let _gdip = GdiplusGuard::new();
        let s = dpi as f32 / 96.0;
        let pw = (WIN_W * s).round() as i32;
        let ph = (WIN_H * s).round() as i32;

        unsafe {
            let mem_dc = CreateCompatibleDC(null_mut());
            let mut bmi: BITMAPINFO = std::mem::zeroed();
            bmi.bmiHeader.biSize = std::mem::size_of::<BITMAPINFOHEADER>() as u32;
            bmi.bmiHeader.biWidth = pw;
            bmi.bmiHeader.biHeight = -ph;
            bmi.bmiHeader.biPlanes = 1;
            bmi.bmiHeader.biBitCount = 32;
            bmi.bmiHeader.biCompression = BI_RGB;

            let mut bits: *mut core::ffi::c_void = null_mut();
            let hbmp = CreateDIBSection(mem_dc, &bmi, DIB_RGB_COLORS, &mut bits, null_mut(), 0);
            assert!(!hbmp.is_null() && !bits.is_null());
            let old = SelectObject(mem_dc, hbmp as HGDIOBJ);

            let mut state = UiState::new(dpi, on, autostart, 0);
            state.tab = tab;
            if let Some(gi) = expand_group {
                state.expanded[gi] = true;
            }
            paint_ui(mem_dc, pw, ph, &mut state, snap, custom_list, on, direct_cdn, doh, 0);

            let src = std::slice::from_raw_parts(bits as *const u8, (pw * ph * 4) as usize);
            let row_stride = ((pw as usize * 3) + 3) & !3;
            let img_size = row_stride * ph as usize;
            let file_size = 54 + img_size;
            let mut out = Vec::with_capacity(file_size);

            out.extend_from_slice(b"BM");
            out.extend_from_slice(&(file_size as u32).to_le_bytes());
            out.extend_from_slice(&0u32.to_le_bytes());
            out.extend_from_slice(&54u32.to_le_bytes());
            out.extend_from_slice(&40u32.to_le_bytes());
            out.extend_from_slice(&(pw as i32).to_le_bytes());
            out.extend_from_slice(&(ph as i32).to_le_bytes());
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
        let snap_custom = sample_snap(true);
        let custom_list = vec![
            CustomDomain { domain: "bbc.com".into(), enabled: true },
            CustomDomain { domain: "medium.com".into(), enabled: true },
            CustomDomain { domain: "imgur.com".into(), enabled: false },
        ];

        render_bmp(
            &format!(r"{dir}\target\preview_v14_steam.bmp"),
            96,
            Tab::Steam,
            true,
            true,
            DohProvider::Cloudflare,
            true,
            None,
            &snap_custom,
            &custom_list,
        );
        render_bmp(
            &format!(r"{dir}\target\preview_v14_steam_exp.bmp"),
            96,
            Tab::Steam,
            true,
            false,
            DohProvider::AdGuard,
            true,
            Some(2),
            &snap_custom,
            &custom_list,
        );
        render_bmp(
            &format!(r"{dir}\target\preview_v14_custom.bmp"),
            96,
            Tab::Custom,
            true,
            true,
            DohProvider::Google,
            true,
            None,
            &snap_custom,
            &custom_list,
        );

        for (light, suffix) in [(false, "dark"), (true, "light")] {
            set_light(light);
            render_bmp(
                &format!(r"{dir}\target\preview_utilities_{suffix}.bmp"),
                96,
                Tab::Utilities,
                true,
                true,
                DohProvider::Custom,
                false,
                None,
                &snap_custom,
                &custom_list,
            );
        }
        set_light(false);
    }
}
