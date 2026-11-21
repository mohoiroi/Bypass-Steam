//! Native Windows interface, system tray and Custom DoH dialog.

use crate::{DISPLAY_VERSION, config::{self, CustomDomain}};
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
        Controls::{DRAWITEMSTRUCT, MEASUREITEMSTRUCT, ODS_CHECKED, ODS_DISABLED, ODS_FOCUS, ODS_SELECTED, ODT_MENU},
        HiDpi::{AdjustWindowRectExForDpi, GetDpiForWindow},
        Input::KeyboardAndMouse::{
            EnableWindow, GetFocus, GetKeyState, SetFocus, TME_LEAVE, TRACKMOUSEEVENT, TrackMouseEvent, VK_CONTROL,
            VK_ESCAPE, VK_NEXT, VK_PRIOR, VK_RETURN, VK_SHIFT, VK_SPACE, VK_TAB,
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

// Main window sections, in DPI-independent pixels.
const COLUMN_GAP: f32 = 18.0;
const PANE_HEADER_H: f32 = 50.0;
const WORKSPACE_PAD: f32 = 12.0;
const SIDEBAR_PAD: f32 = 10.0;
const DOH_SECTION_H: f32 = 116.0;
const FOOTER_H: f32 = 48.0;
const PROXY_CONTROL_H: f32 = 96.0;
const UTILITY_ROW_H: f32 = 52.0;
const NAV_ITEMS: [(Tab, &str); 3] = [
    (Tab::Steam, "Steam"),
    (Tab::Custom, "Web khác"),
    (Tab::Utilities, "Tiện ích"),
];

const INPUT_PAD: f32 = 8.0;
const INPUT_H: f32 = 30.0;
const ADD_BTN_W: f32 = 72.0;
const ADD_GAP: f32 = 8.0;

#[derive(Clone, Copy, PartialEq, Eq)]
enum Tab {
    Steam,
    Custom,
    Utilities,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Action {
    ToggleBypass,
    OpenDohMenu,
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
    keyboard_focus: Option<Action>,
    tracking_mouse: bool,
    expanded: [bool; GROUPS.len()],
    scroll_steam: f32,
    scroll_custom: f32,
    max_scroll: f32,
    scroll_custom_to_bottom: bool,
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
            keyboard_focus: None,
            tracking_mouse: false,
            expanded: [false; GROUPS.len()],
            scroll_steam: 0.0,
            scroll_custom: 0.0,
            max_scroll: 0.0,
            scroll_custom_to_bottom: false,
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

fn health_label(h: Health) -> (&'static str, Color) {
    match h {
        Health::Ok => ("Đã bypass", theme::success()),
        Health::Partial => ("Một phần", theme::danger()),
        Health::Fail => ("Bị chặn", theme::danger()),
        Health::Testing => ("Đang đo…", theme::info()),
        Health::Idle => ("Chờ…", theme::text_dim()),
    }
}

fn host_dot_color(h: Health) -> Color {
    health_label(h).1
}

fn latency_color(ms: u32) -> Color {
    if ms < 60 {
        theme::success()
    } else if ms <= 150 {
        theme::info()
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

fn split_layout(width: f32, height: f32) -> (RectF, RectF) {
    let side_width = (width * 0.28).clamp(210.0, 228.0);
    let sidebar = RectF::new(PAD_X, PAD_Y, side_width, height - 2.0 * PAD_Y);
    let workspace_x = sidebar.right() + COLUMN_GAP;
    let workspace = RectF::new(workspace_x, sidebar.y, width - PAD_X - workspace_x, sidebar.h);
    (sidebar, workspace)
}

fn workspace_content(workspace: RectF) -> RectF {
    RectF::new(
        workspace.x + WORKSPACE_PAD,
        workspace.y + PANE_HEADER_H + WORKSPACE_PAD,
        workspace.w - 2.0 * WORKSPACE_PAD,
        workspace.h - PANE_HEADER_H - WORKSPACE_PAD - FOOTER_H,
    )
}

fn custom_input_rect(card: RectF) -> RectF {
    RectF::new(
        card.x + INPUT_PAD, card.y + INPUT_PAD,
        card.w - 2.0 * INPUT_PAD - ADD_BTN_W - ADD_GAP, INPUT_H,
    )
}

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
    let width = (phys_w as f32 / scale).max(MIN_W);
    let height = (phys_h as f32 / scale).max(MIN_H);
    state.hits.clear();
    state.row_targets.clear();

    let mut c = Canvas::new(hdc, state.dpi);
    c.fill_rect(RectF::new(0.0, 0.0, width, height), theme::bg());

    let (sidebar, workspace) = split_layout(width, height);
    paint_connection_sidebar(&mut c, sidebar, state, snap, on, doh, pending);
    paint_workspace_tabs(&mut c, workspace, state);
    let content = workspace_content(workspace);
    match state.tab {
        Tab::Steam => paint_steam_tab(&mut c, content, state, snap, direct_cdn, pending),
        Tab::Custom => paint_custom_tab(&mut c, content, state, snap, custom_list, pending),
        Tab::Utilities => paint_utilities(&mut c, content, state, direct_cdn),
    }
    let past_results = !on && state.tab != Tab::Utilities
        && snap.iter().any(|(_, stat)| stat.ok > 0 || stat.fail > 0);
    paint_workspace_footer(&mut c, content, state, past_results);

    if let Some(focus) = state.keyboard_focus {
        if let Some((rect, _)) = state.hits.iter().find(|(_, action)| *action == focus) {
            if focus != Action::FocusInput {
                c.stroke_round_rect(rect.inset(2.0, 2.0), R_SM, 1.0, theme::info());
            }
        } else {
            state.keyboard_focus = None;
        }
    }
}

fn paint_workspace_footer(c: &mut Canvas, content: RectF, state: &mut UiState, past_results: bool) {
    let rule_y = content.bottom() + 4.0;
    c.draw_line(content.x, rule_y, content.right(), rule_y, 1.0, theme::border_sub(), false);

    let quit = RectF::new(content.right() - 72.0, rule_y + 6.0, 72.0, 30.0);
    let action = Action::QuitApp;
    paint_button(c, &state.fonts, quit, "Thoát", false, state.hover == Some(action));
    state.hits.push((quit, action));

    let notice = state.notice.as_ref().map(|(message, color)| (message.as_str(), *color));
    if let Some((message, color)) = notice.or_else(|| {
        past_results.then(|| ("Kết quả kết nối đã ghi nhận", theme::text_muted()))
    }) {
        c.draw_text(
            message, RectF::new(content.x, quit.y, quit.x - content.x - 16.0, quit.h),
            state.fonts.caption, color, DT_LEFT,
        );
    }
}

fn paint_connection_sidebar(
    c: &mut Canvas,
    sidebar: RectF,
    state: &mut UiState,
    snap: &[(String, Stat)],
    on: bool,
    doh: DohProvider,
    pending: usize,
) {
    c.fill_round_rect(sidebar, 14.0, theme::surface_inset());
    c.stroke_round_rect(sidebar, 14.0, 1.0, theme::border_sub());

    let left = sidebar.x + SIDEBAR_PAD;
    let content_w = sidebar.w - 2.0 * SIDEBAR_PAD;
    let header_bottom = sidebar.y + PANE_HEADER_H;
    let doh_top = sidebar.bottom() - DOH_SECTION_H;
    for y in [header_bottom, doh_top] {
        c.draw_line(sidebar.x + 1.0, y, sidebar.right() - 1.0, y, 1.0, theme::border_sub(), false);
    }

    let band_h = (doh_top - header_bottom) * 0.5;
    let power_y = header_bottom + (band_h - PROXY_CONTROL_H) * 0.5;
    let power = RectF::new(left, power_y, content_w, PROXY_CONTROL_H);
    let metrics = RectF::new(left, header_bottom + band_h, content_w, band_h);
    c.draw_line(left, metrics.y, metrics.right(), metrics.y, 1.0, theme::border_sub(), false);
    paint_power_control(c, power, state, on);

    let fonts = &state.fonts;
    let title = RectF::new(left + 2.0, sidebar.y, content_w - 60.0, PANE_HEADER_H);
    let version = RectF::new(left + content_w - 52.0, sidebar.y, 50.0, PANE_HEADER_H);
    c.draw_text("Kết nối", title, fonts.title, theme::text(), DT_LEFT);
    c.draw_text(DISPLAY_VERSION, version, fonts.mono, theme::text_muted(), DT_RIGHT);

    let (responding, failed) = paint_overview_metrics(c, metrics, fonts, snap, on, pending);

    let doh_label = RectF::new(left + 2.0, doh_top + 10.0, content_w - 4.0, 22.0);
    c.draw_text("Máy chủ DoH", doh_label, fonts.group, theme::text_sec(), DT_LEFT);
    let control_y = doh_label.bottom() + 4.0;
    let test_width = 76.0;
    let doh_button = RectF::new(left, control_y, content_w - test_width - 8.0, 38.0);
    let test_button = RectF::new(doh_button.right() + 8.0, control_y, test_width, doh_button.h);

    paint_button_frame(c, doh_button, false, state.hover == Some(Action::OpenDohMenu));
    c.draw_text(
        doh.label(), RectF::new(doh_button.x + 12.0, doh_button.y, doh_button.w - 36.0, doh_button.h),
        fonts.group, theme::text(), DT_LEFT,
    );
    c.draw_chevron(doh_button.right() - 14.0, doh_button.cy(), 4.0, true, theme::text_sec());
    state.hits.push((doh_button, Action::OpenDohMenu));

    let can_test = on && pending == 0;
    if can_test {
        paint_button_frame(c, test_button, false, state.hover == Some(Action::RunTest));
    } else {
        c.fill_round_rect(test_button, R_BTN, theme::surface_inset());
        c.stroke_round_rect(test_button, R_BTN, 1.0, theme::border_sub());
    }
    let ink = if can_test { theme::text() } else { theme::text_dim() };
    c.draw_text("Kiểm tra", test_button, fonts.body, ink, DT_CENTER);
    if can_test {
        state.hits.push((test_button, Action::RunTest));
    }

    let (fallback, color) = if !on {
        ("Bật proxy để kiểm tra kết nối".into(), theme::text_dim())
    } else if pending > 0 {
        ("Đang kiểm tra kết nối…".into(), theme::info())
    } else if failed > 0 {
        (format!("{failed} máy chủ chưa phản hồi"), theme::danger())
    } else if responding > 0 && responding == snap.len() {
        ("Tất cả máy chủ đã phản hồi".into(), theme::success())
    } else {
        ("Bấm Kiểm tra để đo kết nối".into(), theme::text_dim())
    };
    let feedback = RectF::new(left + 13.0, doh_button.bottom() + 5.0, content_w - 13.0, 21.0);
    c.fill_circle(left + 3.0, feedback.cy(), 3.0, color);
    c.draw_text(&fallback, feedback, fonts.caption, color, DT_LEFT);
}

fn paint_power_control(c: &mut Canvas, area: RectF, state: &mut UiState, on: bool) {
    let fonts = &state.fonts;
    let hovered = state.hover == Some(Action::ToggleBypass);
    if hovered {
        c.fill_round_rect(area, R_BTN, theme::surface_hover());
    }

    let content = area.inset(12.0, 0.0);
    let center_x = content.x + content.w * 0.5;
    let status_h = 22.0;
    let endpoint_h = 18.0;
    let track_h = 24.0;
    let track_w = 48.0;
    let switch_gap = 8.0;
    let content_h = status_h + endpoint_h + switch_gap + track_h;
    let top = content.cy() - content_h * 0.5;

    let label = if on { "Proxy đang bật" } else { "Proxy đã tắt" };
    let indicator_space = 14.0;
    let label_w = c.measure_text(label, fonts.group).0.min(content.w - indicator_space);
    let status_x = center_x - (indicator_space + label_w) * 0.5;
    let status = RectF::new(status_x + indicator_space, top, label_w, status_h);
    let indicator = if on { theme::success() } else { theme::neutral() };
    c.fill_circle(status_x + 3.0, status.cy(), 3.0, indicator);
    c.draw_text(label, status, fonts.group, theme::text(), DT_LEFT);

    let endpoint = RectF::new(content.x, status.bottom(), content.w, endpoint_h);
    let address = if on { format!("127.0.0.1:{PORT}") } else { "Kết nối trực tiếp".into() };
    c.draw_text(
        &address, endpoint, if on { fonts.mono } else { fonts.caption }, theme::text_muted(), DT_CENTER,
    );

    let track = RectF::new(center_x - track_w * 0.5, endpoint.bottom() + switch_gap, track_w, track_h);
    draw_mini_toggle(c, track, on, hovered);
    state.hits.push((area, Action::ToggleBypass));
}

fn paint_overview_metrics(
    c: &mut Canvas,
    rect: RectF,
    fonts: &FontSet,
    snap: &[(String, Stat)],
    on: bool,
    pending: usize,
) -> (usize, usize) {
    let mut measured = 0u64;
    let mut latency_sum = 0u64;
    let mut ok = 0usize;
    let mut failed = 0usize;
    for (_, stat) in snap {
        if let Some(ms) = stat.latency_ms {
            latency_sum += u64::from(ms);
            measured += 1;
        }
        match host_health(stat, pending) {
            Health::Ok => ok += 1,
            Health::Fail => failed += 1,
            _ => {}
        }
    }

    let (latency, latency_ink, latency_unit) = if on && measured > 0 {
        let ms = ((latency_sum + measured / 2) / measured) as u32;
        (ms.to_string(), latency_color(ms), "ms")
    } else {
        ("—".into(), theme::text_muted(), "")
    };
    let (hosts, host_ink) = if on && !snap.is_empty() {
        (format!("{ok}/{}", snap.len()), theme::text())
    } else {
        ("—".into(), theme::text_muted())
    };
    let cell_w = rect.w * 0.5;
    let divider = rect.x + cell_w;
    let text_top = rect.y + (rect.h - 60.0) * 0.5;
    c.draw_line(divider, text_top + 8.0, divider, text_top + 52.0, 1.0, theme::border_sub(), false);
    for (x, label, value, ink, unit) in [
        (rect.x, "Độ trễ TB", &latency, latency_ink, latency_unit),
        (divider, "Đã phản hồi", &hosts, host_ink, ""),
    ] {
        let label_rect = RectF::new(x + 12.0, text_top + 6.0, cell_w - 24.0, 20.0);
        c.draw_text(label, label_rect, fonts.caption, theme::text_muted(), DT_CENTER);
        let unit_w = c.measure_text(unit, fonts.mono).0;
        let unit_space = if unit.is_empty() { 0.0 } else { unit_w + 4.0 };
        let value_w = label_rect.w - unit_space;
        let kpi_w = c.measure_text(value, fonts.kpi).0;
        let (value_font, number_w) = if kpi_w <= value_w {
            (fonts.kpi, kpi_w)
        } else {
            (fonts.mono_bold, c.measure_text(value, fonts.mono_bold).0.min(value_w))
        };
        let value_x = x + (cell_w - number_w - unit_space) * 0.5;
        c.draw_text(value, RectF::new(value_x, text_top + 25.0, number_w, 28.0), value_font, ink, DT_LEFT);
        if !unit.is_empty() {
            c.draw_text(unit, RectF::new(value_x + number_w + 4.0, text_top + 29.0, unit_w, 28.0),
                fonts.mono, theme::text_muted(), DT_LEFT);
        }
    }
    (ok, failed)
}

fn paint_workspace_tabs(c: &mut Canvas, pane: RectF, state: &mut UiState) {
    c.fill_round_rect(pane, 14.0, theme::surface_inset());
    c.stroke_round_rect(pane, 14.0, 1.0, theme::border_sub());
    let bar = RectF::new(pane.x + WORKSPACE_PAD, pane.y + 5.0,
        pane.w - 2.0 * WORKSPACE_PAD, PANE_HEADER_H - 10.0);
    let rule_y = pane.y + PANE_HEADER_H;
    c.draw_line(pane.x + 1.0, rule_y, pane.right() - 1.0, rule_y, 1.0, theme::border_sub(), false);
    let item_w = bar.w / NAV_ITEMS.len() as f32;
    for (index, (tab, label)) in NAV_ITEMS.into_iter().enumerate() {
        let cell = RectF::new(bar.x + index as f32 * item_w, bar.y, item_w, bar.h);
        let button = cell.inset(4.0, 4.0);
        let action = Action::SelectTab(tab);
        let selected = state.tab == tab;
        if state.hover == Some(action) {
            c.fill_round_rect(button, R_BTN, theme::surface_hover());
        } else if selected {
            c.fill_round_rect(button, R_BTN, theme::surface());
            c.stroke_round_rect(button, R_BTN, 1.0, theme::border_sub());
        }
        let ink = if selected { theme::info() } else { theme::text_sec() };
        let (label_w, _) = c.measure_text(label, state.fonts.body);
        let text_x = cell.x + (cell.w - label_w + 23.0) * 0.5;
        draw_nav_icon(c, tab, text_x - 15.0, cell.cy(), ink);
        c.draw_text(label, RectF::new(text_x, button.y, label_w + 4.0, button.h),
            state.fonts.body, ink, DT_LEFT);
        state.hits.push((button, action));
    }
}

fn draw_nav_icon(c: &mut Canvas, tab: Tab, x: f32, y: f32, ink: Color) {
    match tab {
        Tab::Steam => {
            let pad = RectF::new(x - 7.0, y - 4.5, 14.0, 9.0);
            c.stroke_round_rect(pad, 2.5, 1.1, ink);
            c.draw_plus(x - 3.0, y, 1.6, 1.1, ink);
            c.fill_circle(x + 3.0, y - 0.9, 0.7, ink);
            c.fill_circle(x + 4.7, y + 0.9, 0.7, ink);
        }
        Tab::Custom => {
            c.stroke_circle(x, y, 6.5, 1.1, ink);
            c.draw_line(x - 6.0, y, x + 6.0, y, 1.0, ink, true);
            c.draw_line(x, y - 6.0, x, y + 6.0, 1.0, ink, true);
        }
        Tab::Utilities => {
            for (offset, knob) in [(-4.5, 2.5), (0.0, -2.5), (4.5, 2.5)] {
                c.draw_line(x - 6.5, y + offset, x + 6.5, y + offset, 1.1, ink, true);
                c.draw_line(x + knob, y + offset - 1.6, x + knob, y + offset + 1.6, 1.5, ink, true);
            }
        }
    }
}

fn paint_utilities(c: &mut Canvas, card: RectF, state: &mut UiState, direct: bool) {
    state.max_scroll = 0.0;
    let fonts = &state.fonts;
    let rows = [
        ("Đi thẳng CDN tải game", "Tận dụng cache nội mạng.", ID_MENU_DIRECT_CDN),
        ("Khởi chạy cùng Windows", "Mở ứng dụng trong khay khi đăng nhập.", ID_MENU_AUTOSTART),
        ("Giao diện", "", 0),
        ("Cập nhật", DISPLAY_VERSION, ID_UPDATE),
    ];

    let panel = RectF::new(card.x, card.y, card.w, rows.len() as f32 * UTILITY_ROW_H + 10.0);
    c.fill_round_rect(panel, R_PANEL, theme::surface_inset());
    c.stroke_round_rect(panel, R_PANEL, 1.0, theme::border_sub());

    for (index, (title, description, command)) in rows.into_iter().enumerate() {
        let row = RectF::new(panel.x + 10.0, panel.y + 5.0 + index as f32 * UTILITY_ROW_H, panel.w - 20.0, UTILITY_ROW_H);
        let action = Action::Utility(command);
        let is_switch = index < 2;
        let hovered = state.hover == Some(action);
        if is_switch && hovered {
            c.fill_round_rect(row.inset(0.0, 2.0), R_SM, theme::surface_hover());
        }
        if index > 0 {
            c.draw_line(row.x, row.y, row.right(), row.y, 1.0, theme::border_sub(), false);
        }
        let caption_w = if is_switch { row.w - 113.0 } else { row.w - 148.0 };
        let title_y = if description.is_empty() { row.cy() - 9.5 } else { row.y + 6.0 };
        c.draw_text(title, RectF::new(row.x + 4.0, title_y, caption_w, 19.0), fonts.group, theme::text(), DT_LEFT);
        c.draw_text(description, RectF::new(row.x + 4.0, row.y + 24.0, caption_w, 17.0), fonts.caption, theme::text_muted(), DT_LEFT);

        let controls_x = row.right() - 126.0;
        if is_switch {
            let on = if index == 0 { direct } else { state.autostart };
            c.draw_text(if on { "Bật" } else { "Tắt" }, RectF::new(row.right() - 92.0, row.y, 35.0, row.h),
                fonts.caption, theme::text_sec(), DT_RIGHT);
            draw_mini_toggle(c, RectF::new(row.right() - 47.0, row.cy() - 10.0, 35.0, 20.0), on, hovered);
            state.hits.push((row, action));
        } else if command == ID_UPDATE {
            let busy = state.update.is_some();
            let button = RectF::new(controls_x, row.cy() - 15.0, 116.0, 30.0);
            paint_button(c, fonts, button, if busy { "Đang kiểm tra…" } else { "Kiểm tra cập nhật" },
                false, hovered && !busy);
            if !busy { state.hits.push((button, action)); }
        } else {
            let light = is_light();
            for (offset, label, cmd, active) in [(0.0, "Sáng", ID_LIGHT, light), (58.0, "Tối", ID_DARK, !light)] {
                let button = RectF::new(controls_x + offset, row.cy() - 15.0, 56.0, 30.0);
                let action = Action::Utility(cmd);
                paint_button(c, fonts, button, label, active, state.hover == Some(action));
                state.hits.push((button, action));
            }
        }
    }
}

fn paint_button_frame(c: &mut Canvas, rect: RectF, primary: bool, hovered: bool) {
    let (fill, border) = match (primary, hovered) {
        (true, true) => (theme::primary_hover(), theme::primary_hover()),
        (true, false) => (theme::primary(), theme::primary()),
        (false, true) => (theme::surface_hover(), theme::text_dim()),
        (false, false) => (theme::surface_inset(), theme::border()),
    };
    c.fill_round_rect(rect, R_BTN, fill);
    c.stroke_round_rect(rect, R_BTN, 1.0, border);
}

fn paint_button(c: &mut Canvas, fonts: &FontSet, rect: RectF, label: &str, primary: bool, hovered: bool) {
    paint_button_frame(c, rect, primary, hovered);
    c.draw_text(label, rect, fonts.badge,
        if primary { theme::on_primary() } else { theme::text() }, DT_CENTER);
}

fn draw_mini_toggle(c: &mut Canvas, r: RectF, on: bool, hovered: bool) {
    let track = match (on, hovered) {
        (true, true) => theme::primary_hover(),
        (true, false) => theme::primary(),
        (false, true) => theme::text_dim(),
        (false, false) => theme::neutral(),
    };
    c.fill_round_rect(r, r.h * 0.5, track);
    let radius = r.h * 0.5 - 2.5;
    let knob_x = if on { r.right() - r.h * 0.5 } else { r.x + r.h * 0.5 };
    c.fill_circle(knob_x, r.cy(), radius, if on { theme::on_primary() } else { theme::switch_thumb() });
}

fn paint_steam_tab(
    c: &mut Canvas,
    card: RectF,
    state: &mut UiState,
    snap: &[(String, Stat)],
    direct_cdn: bool,
    pending: usize,
) {
    const HEAD_H: f32 = 34.0;
    let visible_groups: Vec<(usize, Vec<&(String, Stat)>)> = (0..CUSTOM_GROUP_IDX)
        .filter_map(|gi| {
            let rows: Vec<&(String, Stat)> = snap.iter().filter(|(_, s)| s.group == gi).collect();
            if rows.is_empty() { None } else { Some((gi, rows)) }
        })
        .collect();

    const GROUP_H: f32 = 48.0;
    const HOST_H: f32 = 44.0;
    let content_h: f32 = visible_groups
        .iter()
        .map(|(gi, rows)| {
            GROUP_H + if state.expanded[*gi] { rows.len() as f32 * HOST_H } else { 0.0 }
        })
        .sum();
    let table_h = if visible_groups.is_empty() { card.h } else { (HEAD_H + content_h + 2.0).min(card.h) };
    let table = RectF::new(card.x, card.y, card.w, table_h);
    let inner = RectF::new(table.x + 1.0, table.y + HEAD_H, table.w - 2.0, table.h - HEAD_H - 2.0);
    c.fill_round_rect(table, R_PANEL, theme::surface_inset());
    c.stroke_round_rect(table, R_PANEL, 1.0, theme::border_sub());
    state.max_scroll = (content_h - inner.h).max(0.0);
    state.scroll_steam = state.scroll_steam.clamp(0.0, state.max_scroll);

    let scroll_pad = if state.max_scroll > 0.5 { 8.0 } else { 0.0 };
    let right = inner.right() - 14.0 - scroll_pad;
    let column_gap = (8.0 + (table.w - 414.0) * 0.12).clamp(8.0, 22.0);
    let status = RectF::new(right - 84.0, card.y, 84.0, HEAD_H);
    let replies = RectF::new(status.x - column_gap - 54.0, card.y, 54.0, HEAD_H);
    let latency = RectF::new(replies.x - column_gap - 54.0, card.y, 54.0, HEAD_H);
    let details_x = inner.x + 32.0;
    let details = RectF::new(details_x, card.y, latency.x - details_x - 10.0, HEAD_H);
    c.draw_text("Dịch vụ", details, state.fonts.body, theme::text_muted(), DT_LEFT);
    for (label, column) in [("Độ trễ", latency), ("Phản hồi", replies), ("Trạng thái", status)] {
        c.draw_text(label, column, state.fonts.body, theme::text_muted(), DT_RIGHT);
    }
    c.draw_line(inner.x, inner.y, inner.right(), inner.y, 1.0, theme::border_sub(), false);

    if visible_groups.is_empty() {
        c.draw_text("Chưa có kết quả. Bấm Kiểm tra ở cột bên trái.", inner, state.fonts.body,
            theme::text_muted(), DT_CENTER);
    }
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
            if is_hov || is_exp {
                c.fill_rect(gh_rect, if is_hov { theme::surface_hover() } else { theme::surface() });
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
            let (health, ok, _, dns_fixed) = group_health(rows, pending);
            let (label, ink) = if is_direct_cdn {
                ("Đi thẳng", theme::info())
            } else {
                health_label(health)
            };
            let mid_y = y + GROUP_H * 0.5;
            c.draw_chevron(inner.x + 16.0, mid_y, 4.0, is_exp, theme::text_muted());

            let row_cell = |column: RectF| RectF::new(column.x, y + 5.0, column.w, 22.0);
            if let Some(ms) = avg_latency_ms(rows) {
                c.draw_text(&format!("{ms} ms"), row_cell(latency),
                    state.fonts.mono_bold, latency_color(ms), DT_RIGHT);
            } else {
                c.draw_text("—", row_cell(latency), state.fonts.mono, theme::text_muted(), DT_RIGHT);
            }
            c.draw_text(&format!("{ok}/{}", rows.len()), row_cell(replies),
                state.fonts.mono, theme::text_sec(), DT_RIGHT);
            let (label_w, _) = c.measure_text(label, state.fonts.badge);
            let status_w = (label_w + 20.0).min(status.w);
            let badge = RectF::new(status.right() - status_w, y + 5.0, status_w, 22.0);
            c.fill_round_rect(badge, 11.0, theme::surface_hover());
            c.draw_text(label, badge, state.fonts.badge, ink, DT_CENTER);

            let (name, description) = GROUPS[*gi];
            let subtitle = if is_direct_cdn {
                "Kết nối trực tiếp để tải game".into()
            } else if dns_fixed {
                format!("DNS sửa · {description}")
            } else {
                description.to_owned()
            };
            c.draw_text(name, row_cell(details),
                state.fonts.group, theme::text(), DT_LEFT);
            c.draw_text(&subtitle, RectF::new(details.x, y + 27.0, right - details.x, 16.0),
                state.fonts.caption, theme::text_muted(), DT_LEFT);
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
                    let text_x = inner.x + 35.0;
                    let text_right = inner.right() - 16.0 - scroll_pad;
                    c.fill_circle(inner.x + 25.0, y + 16.0, 3.0, host_dot_color(host_health(s, pending)));

                    let mut host_right = text_right;
                    if let Some(ms) = s.latency_ms {
                        let latency = format!("{ms}ms");
                        let (width, _) = c.measure_text(&latency, state.fonts.mono_bold);
                        c.draw_text(&latency, RectF::new(text_right - width - 2.0, y + 7.0, width + 2.0, 18.0),
                            state.fonts.mono_bold, latency_color(ms), DT_RIGHT);
                        host_right -= width + 10.0;
                    }
                    c.draw_text(host, RectF::new(text_x, y + 7.0, host_right - text_x, 18.0),
                        state.fonts.body, theme::text_sec(), DT_LEFT);

                    let (dns, dns_color) = match s.dns {
                        DnsState::Poisoned | DnsState::Failed => (" · DNS sửa", theme::info()),
                        DnsState::Clean => (" · DNS OK", theme::text_muted()),
                        DnsState::Unknown => ("", theme::text_muted()),
                    };
                    let ip = if s.ip.is_empty() { "—" } else { &s.ip };
                    let details = if s.ok > 0 { format!("{ip}{dns} · ×{}", s.ok) } else { format!("{ip}{dns}") };
                    c.draw_text(&details, RectF::new(text_x, y + 26.0, text_right - text_x, 15.0),
                        state.fonts.caption, dns_color, DT_LEFT);
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
    card: RectF,
    state: &mut UiState,
    snap: &[(String, Stat)],
    custom_list: &[CustomDomain],
    pending: usize,
) {
    c.fill_round_rect(card, R_PANEL, theme::surface_inset());
    c.stroke_round_rect(card, R_PANEL, 1.0, theme::border_sub());

    // Domain input stays above the scrollable list.
    let inp = custom_input_rect(card);
    let inp_hov = state.hover == Some(Action::FocusInput);
    let inp_border = if state.edit_focused {
        theme::text()
    } else if inp_hov {
        theme::text_dim()
    } else {
        theme::border()
    };
    c.fill_round_rect(inp, R_BTN, theme::surface_inset());
    c.stroke_round_rect(inp, R_BTN, if state.edit_focused { 1.4 } else { 1.0 }, inp_border);
    if !state.edit_focused && !state.edit_has_text {
        c.draw_text(
            "Nhập hoặc dán tên miền…",
            RectF::new(inp.x + 12.0, inp.y, (inp.w - 22.0).max(40.0), inp.h),
            state.fonts.caption,
            theme::text_dim(),
            DT_LEFT,
        );
    }
    state.hits.push((inp, Action::FocusInput));

    let add_button = RectF::new(inp.right() + ADD_GAP, inp.y, ADD_BTN_W, INPUT_H);
    let add_hovered = state.hover == Some(Action::AddDomain);
    paint_button(c, &state.fonts, add_button, "Thêm", true, add_hovered);
    state.hits.push((add_button, Action::AddDomain));

    let div_y = inp.bottom() + INPUT_PAD;
    c.draw_line(card.x + 1.0, div_y, card.right() - 1.0, div_y, 1.0, theme::border_sub(), false);

    // Bottom fixed informational note inside card
    let info_h = 24.0;
    let info_y = card.bottom() - info_h - 1.0;
    c.draw_line(card.x + 1.0, info_y, card.right() - 1.0, info_y, 1.0, theme::border_sub(), false);
    c.draw_text(
        "Vượt chặn DNS/DPI. Trang chặn IP có thể cần VPN.",
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

    state.row_targets.extend(custom_list.iter().map(|item| item.domain.clone()));
    const ROW_H: f32 = 42.0;
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
            c.fill_circle(dot_x, rcy, 3.8, host_dot_color(h));

            // Right-most delete button (×)
            let del_sz = 26.0;
            let del_r = RectF::new(
                list_r.right() - 14.0 - scroll_pad - del_sz,
                rcy - del_sz * 0.5,
                del_sz,
                del_sz,
            );
            let del_hov = state.hover == Some(Action::RemoveDomain(idx));
            if del_hov {
                c.fill_round_rect(del_r, R_SM, theme::surface_hover());
            }
            c.draw_cross(
                del_r.x + del_sz * 0.5,
                del_r.cy(),
                3.8,
                1.6,
                if del_hov { theme::danger() } else { theme::text_dim() },
            );
            // Mini pause/resume toggle left of delete button
            let tg_w = 34.0;
            let tg_h = 18.0;
            let tg_r = RectF::new(del_r.x - 10.0 - tg_w, rcy - tg_h * 0.5, tg_w, tg_h);
            let tg_hov = state.hover == Some(Action::ToggleCustomDomain(idx));
            draw_mini_toggle(c, tg_r, item.enabled, tg_hov);
            let tht = (tg_r.y - 4.0).max(list_r.y);
            let thb = (tg_r.bottom() + 4.0).min(list_r.bottom());
            if thb > tht {
                state.hits.push((
                    RectF::new(tg_r.x - 3.0, tht, tg_r.w + 6.0, thb - tht),
                    Action::ToggleCustomDomain(idx),
                ));
            }

            let ht = del_r.y.max(list_r.y);
            let hb = del_r.bottom().min(list_r.bottom());
            if hb > ht {
                state.hits.push((
                    RectF::new(del_r.x, ht, del_r.w, hb - ht),
                    Action::RemoveDomain(idx),
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
                    c.stroke_round_rect(lat_r, R_SM, 1.0, theme::border());
                    c.draw_text(&lat_txt, lat_r, state.fonts.mono_bold, lcol, DT_CENTER);
                    right_limit = lat_r.x - 10.0;
                }
            }

            // Domain title + metadata sub-line
            let text_left = dot_x + 14.0;
            let text_w = (right_limit - text_left).max(60.0);
            c.draw_text(
                root,
                RectF::new(text_left, y + 2.0, text_w, 19.0),
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
                    theme::info()
                } else {
                    theme::text_muted()
                };
                (format!("{status_str}{ip_txt}{dns_txt}{hit_txt}"), col)
            };
            c.draw_text(
                &meta,
                RectF::new(text_left, y + 20.0, text_w, 16.0),
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
        let thumb_h = ((area.h / content_h) * track_h).clamp(20.0, track_h);
        let ratio = scroll_y / max_scroll;
        let thumb_y = area.y + 4.0 + ratio * (track_h - thumb_h);
        c.fill_round_rect(
            RectF::new(area.right() - 7.0, thumb_y, 4.0, thumb_h),
            2.0,
            theme::neutral(),
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
        let width = ((cr.right - cr.left) as f32 / scale).max(MIN_W);
        let height = ((cr.bottom - cr.top) as f32 / scale).max(MIN_H);
        let (_, workspace) = split_layout(width, height);
        let inp = custom_input_rect(workspace_content(workspace));
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
    state.edit_focused = true;
    let edit = state.edit_hwnd;
    unsafe {
        ShowWindow(edit, SW_SHOW);
        SetFocus(edit);
    }
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
    } else if let Some(err) = res.first_err {
        set_notice(hwnd, state, err.to_string(), theme::danger());
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

const DOH_DIALOG_ID: usize = 201;
const DOH_URL_FIELD: i32 = 202;
const DOH_URL_LABEL: i32 = 204;
const DOH_HELP: i32 = 205;
const DOH_TEST: i32 = 206;
const DOH_TEST_TIMER: usize = 1;
const WM_DOH_LAYOUT: u32 = WM_APP + 7;
const DOH_INVALID_URL: &str = "Nhập URL HTTPS đầy đủ với tên máy chủ và đường dẫn DoH.";

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
            background: unsafe { CreateSolidBrush(theme::bg().colorref()) },
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
        Certificate => "Không xác thực được chứng chỉ TLS.".into(),
        Timeout => "Hết thời gian chờ phản hồi.".into(),
        InvalidResponse => "Máy chủ trả về dữ liệu DoH không hợp lệ.".into(),
        NoAddress => "Máy chủ không trả về địa chỉ IPv4.".into(),
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
                hwnd, &format!("Kết nối thành công. Nhận {count} địa chỉ IPv4."), theme::success(),
            ),
            Err(error) => state.show_help(hwnd, &doh_probe_error(error), theme::danger()),
        }
    }
}

fn doh_control_rect(hwnd: HWND, id: i32, scale: f32) -> Option<RectF> {
    unsafe {
        let child = GetDlgItem(hwnd, id);
        let mut bounds = RECT::default();
        if GetWindowRect(child, &mut bounds) == 0 {
            return None;
        }
        MapWindowPoints(null_mut(), hwnd, (&mut bounds as *mut RECT).cast(), 2);
        Some(RectF::new(
            bounds.left as f32 / scale, bounds.top as f32 / scale,
            (bounds.right - bounds.left) as f32 / scale,
            (bounds.bottom - bounds.top) as f32 / scale,
        ))
    }
}

fn doh_input_frame(hwnd: HWND, scale: f32) -> Option<RectF> {
    let label = doh_control_rect(hwnd, DOH_URL_LABEL, scale)?;
    let feedback = doh_control_rect(hwnd, DOH_HELP, scale)?;
    let top = label.bottom() + 6.0;
    Some(RectF::new(label.x, top, label.w, feedback.y - top - 6.0))
}

fn layout_doh_input(hwnd: HWND) {
    let scale = unsafe { GetDpiForWindow(hwnd) }.max(96) as f32 / 96.0;
    let Some(frame) = doh_input_frame(hwnd, scale) else { return };
    let bounds = frame.inset(8.0, 0.0).to_gdi_rect(scale);
    unsafe {
        let edit = GetDlgItem(hwnd, DOH_URL_FIELD);
        if edit.is_null() {
            return;
        }
        let hdc = GetDC(edit);
        if hdc.is_null() {
            return;
        }
        let font = SendMessageW(edit, WM_GETFONT, 0, 0) as HFONT;
        let previous_font = SelectObject(hdc, font as HGDIOBJ);
        let mut metrics = TEXTMETRICW::default();
        let measured = GetTextMetricsW(hdc, &mut metrics) != 0;
        if !previous_font.is_null() {
            SelectObject(hdc, previous_font);
        }
        ReleaseDC(edit, hdc);

        let height = metrics.tmHeight;
        if measured && height > 0 && height <= bounds.bottom - bounds.top {
            // A single-line EDIT draws from the top of its text area.
            let y = bounds.top + (bounds.bottom - bounds.top - height) / 2;
            SetWindowPos(
                edit, null_mut(), bounds.left, y, bounds.right - bounds.left, height,
                SWP_NOZORDER | SWP_NOACTIVATE,
            );
        }
    }
}

fn paint_doh_dialog(hwnd: HWND, hdc: HDC, state: &DohDialog) {
    unsafe {
        let mut client = RECT::default();
        GetClientRect(hwnd, &mut client);
        FillRect(hdc, &client, state.background);
    }
    let scale = state.dpi as f32 / 96.0;
    let (Some(field), Some(save)) = (
        doh_input_frame(hwnd, scale),
        doh_control_rect(hwnd, IDOK, scale),
    ) else {
        return;
    };
    let focused = unsafe { GetFocus() == GetDlgItem(hwnd, DOH_URL_FIELD) };
    let mut c = Canvas::new(hdc, state.dpi);
    c.fill_round_rect(field, R_BTN, theme::surface_inset());
    c.stroke_round_rect(field, R_BTN, 1.0, if focused { theme::info() } else { theme::border() });
    let footer_y = save.y - 9.0;
    c.draw_line(field.x, footer_y, save.right(), footer_y, 1.0, theme::border_sub(), false);
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
                    (DOH_URL_LABEL, "Địa chỉ máy chủ DoH", state.fonts.group),
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
                layout_doh_input(hwnd);
                SendDlgItemMessageW(hwnd, DOH_URL_FIELD, 0x00B1, 0, -1); // EM_SETSEL
                SendMessageW(hwnd, DM_SETDEFID, IDOK as usize, 0);
                1
            }
            WM_DPICHANGED if !state.is_null() => {
                // Recenter after the dialog manager has rescaled the native controls.
                PostMessageW(hwnd, WM_DOH_LAYOUT, 0, 0);
                0
            }
            WM_DOH_LAYOUT if !state.is_null() => {
                layout_doh_input(hwnd);
                InvalidateRect(hwnd, null(), 0);
                1
            }
            WM_PAINT if !state.is_null() => {
                let mut paint = PAINTSTRUCT::default();
                let hdc = BeginPaint(hwnd, &mut paint);
                paint_doh_dialog(hwnd, hdc, &*state);
                EndPaint(hwnd, &paint);
                1
            }
            WM_PRINTCLIENT if !state.is_null() => {
                paint_doh_dialog(hwnd, wp as HDC, &*state);
                1
            }
            WM_LBUTTONDOWN if !state.is_null() => {
                let scale = (*state).dpi as f32 / 96.0;
                let x = (lp as i16) as f32 / scale;
                let y = ((lp >> 16) as i16) as f32 / scale;
                if doh_input_frame(hwnd, scale).is_some_and(|frame| frame.contains(x, y)) {
                    SetFocus(GetDlgItem(hwnd, DOH_URL_FIELD));
                    return 1;
                }
                0
            }
            WM_CTLCOLORDLG if !state.is_null() => (*state).background as isize,
            WM_CTLCOLORSTATIC | WM_CTLCOLOREDIT if !state.is_null() => {
                let hdc = wp as HDC;
                let edit = msg == WM_CTLCOLOREDIT;
                let hint = GetDlgCtrlID(lp as HWND) == DOH_HELP;
                let color = if hint { (*state).help_color.get() } else { theme::text() };
                SetTextColor(hdc, color.colorref());
                SetBkColor(hdc, if edit { theme::surface_inset() } else { theme::bg() }.colorref());
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
                let disabled = item.itemState & ODS_DISABLED != 0;
                let label = match wp as i32 {
                    IDOK => "Lưu và áp dụng",
                    DOH_TEST if disabled => "Đang thử…",
                    DOH_TEST => "Thử kết nối",
                    _ => "Hủy",
                };
                if disabled {
                    c.fill_round_rect(rect, R_BTN, theme::surface_inset());
                    c.stroke_round_rect(rect, R_BTN, 1.0, theme::border_sub());
                    c.draw_text(label, rect, state.fonts.badge, theme::text_dim(), DT_CENTER);
                } else {
                    paint_button(&mut c, &state.fonts, rect, label,
                        primary, item.itemState & ODS_SELECTED != 0);
                }
                if item.itemState & ODS_FOCUS != 0 {
                    c.stroke_round_rect(rect.inset(3.0, 3.0),
                        R_SM, 1.0, if primary { theme::on_primary() } else { theme::info() });
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
                DOH_URL_FIELD if matches!((wp >> 16) as u32, EN_SETFOCUS | EN_KILLFOCUS) => {
                    InvalidateRect(hwnd, null(), 0);
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
            DialogBoxParamW(GetModuleHandleW(null()), DOH_DIALOG_ID as *const u16, hwnd, Some(custom_doh_dialog),
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

fn hit_test(state: &UiState, lx: f32, ly: f32) -> Option<Action> {
    state
        .hits
        .iter()
        .rev()
        .find(|(rect, _)| rect.contains(lx, ly))
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
        let ink = theme::text().colorref();
        let _ = DwmSetWindowAttribute(hwnd, DWMWA_TEXT_COLOR as u32, (&ink as *const u32).cast(), 4);
        let border = DWMWA_COLOR_NONE;
        let _ = DwmSetWindowAttribute(hwnd, DWMWA_BORDER_COLOR as u32, (&border as *const u32).cast(), 4);
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

fn doh_menu_label(provider: DohProvider) -> String {
    if provider == DohProvider::Custom {
        "Custom DoH…".into()
    } else {
        format!("{} ({})", provider.label(), provider.addr())
    }
}

fn create_doh_menu(current: DohProvider, owner_drawn: bool) -> HMENU {
    unsafe {
        let menu = CreatePopupMenu();
        if menu.is_null() {
            return menu;
        }
        for (index, provider) in DohProvider::ALL.into_iter().enumerate() {
            let id = ID_MENU_DOH_BASE + index;
            let flags = MF_STRING | if provider == current { MF_CHECKED } else { 0 };
            AppendMenuW(menu, flags, id, wide(&doh_menu_label(provider)).as_ptr());
            if owner_drawn {
                let item = MENUITEMINFOW {
                    cbSize: std::mem::size_of::<MENUITEMINFOW>() as u32,
                    fMask: MIIM_FTYPE,
                    fType: MFT_OWNERDRAW,
                    ..Default::default()
                };
                SetMenuItemInfoW(menu, id as u32, 0, &item);
            }
        }
        menu
    }
}

fn doh_menu_provider(command: usize) -> Option<DohProvider> {
    DohProvider::ALL.get(command.checked_sub(ID_MENU_DOH_BASE)?).copied()
}

fn show_doh_menu(hwnd: HWND) {
    let Some(bounds) = get_state(hwnd).and_then(|state| {
        let (rect, _) = state.hits.iter().find(|(_, action)| *action == Action::OpenDohMenu)?;
        Some(rect.to_gdi_rect(state.dpi as f32 / 96.0))
    }) else { return };
    unsafe {
        let mut anchor = POINT { x: bounds.left, y: bounds.bottom + 1 };
        if ClientToScreen(hwnd, &mut anchor) == 0 {
            return;
        }

        let menu = create_doh_menu(proxy::doh_provider(), true);
        if menu.is_null() {
            return;
        }
        let background = CreateSolidBrush(theme::surface().colorref());
        let menu_info = MENUINFO {
            cbSize: std::mem::size_of::<MENUINFO>() as u32,
            fMask: MIM_BACKGROUND | MIM_STYLE,
            dwStyle: MNS_NOCHECK,
            hbrBack: background,
            ..Default::default()
        };
        SetMenuInfo(menu, &menu_info);
        let command = TrackPopupMenu(menu, TPM_RETURNCMD | TPM_LEFTALIGN,
            anchor.x, anchor.y, 0, hwnd, null()) as usize;
        DestroyMenu(menu);
        if !background.is_null() {
            DeleteObject(background);
        }
        if let Some(provider) = doh_menu_provider(command) {
            select_doh_ui(hwnd, provider);
        }
    }
}

fn draw_doh_menu_item(item: &DRAWITEMSTRUCT, provider: DohProvider, state: &UiState) {
    let scale = state.dpi as f32 / 96.0;
    let bounds = item.rcItem;
    let r = RectF::new(bounds.left as f32 / scale, bounds.top as f32 / scale,
        (bounds.right - bounds.left) as f32 / scale, (bounds.bottom - bounds.top) as f32 / scale);
    let mut c = Canvas::new(item.hDC, state.dpi);
    let selected = item.itemState & ODS_SELECTED != 0;
    let checked = item.itemState & ODS_CHECKED != 0;
    let custom = provider == DohProvider::Custom;
    c.fill_rect(r, theme::surface());
    let row = if custom {
        c.draw_line(r.x + 10.0, r.y + 2.0, r.right() - 10.0, r.y + 2.0, 1.0, theme::border_sub(), false);
        RectF::new(r.x, r.y + 5.0, r.w, r.h - 5.0)
    } else {
        r
    };
    if selected {
        c.fill_round_rect(row.inset(4.0, 2.0), R_SM, theme::surface_hover());
    }
    if checked {
        c.draw_checkmark(row.x + 16.0, row.cy(), 8.0, 1.5, theme::info());
    }
    let name = if custom { "Custom DoH" } else { provider.label() };
    let trailing_width = if custom { 24.0 } else { 86.0 };
    let label = RectF::new(row.x + 30.0, row.y, row.w - 30.0 - trailing_width - 10.0, row.h);
    c.draw_text(name, label, state.fonts.group,
        if checked { theme::info() } else { theme::text() }, DT_LEFT);
    if custom {
        c.draw_chevron(row.right() - 16.0, row.cy(), 4.0, false, theme::text_muted());
    } else {
        let address = RectF::new(row.right() - 88.0, row.y, 76.0, row.h);
        c.draw_text(&provider.addr(), address, state.fonts.mono, theme::text_muted(), DT_RIGHT);
    }
}

fn show_tray_menu(hwnd: HWND) {
    let on = ENABLED.load(Ordering::SeqCst);
    let cur_doh = proxy::doh_provider();
    let pending = PENDING.load(Ordering::SeqCst) > 0;
    unsafe {
        let menu = CreatePopupMenu();
        let doh_menu = create_doh_menu(cur_doh, false);

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
        if let Some(provider) = doh_menu_provider(cmd_id) {
            select_doh_ui(hwnd, provider);
            return;
        }

        match cmd_id {
            ID_MENU_OPEN => show_main(hwnd),
            ID_MENU_TOGGLE => activate_action(hwnd, Action::ToggleBypass),
            ID_MENU_TEST => activate_action(hwnd, Action::RunTest),
            ID_MENU_DIRECT_CDN | ID_MENU_AUTOSTART | ID_LIGHT | ID_DARK | ID_UPDATE => utility_command(hwnd, cmd_id),
            ID_MENU_RESET_NET => reset_network_ui(hwnd),
            ID_MENU_EXIT => {
                DestroyWindow(hwnd);
            }
            _ => {}
        }
    }
}

fn activate_action(hwnd: HWND, action: Action) {
    match action {
        Action::ToggleBypass => proxy::set_enabled(!ENABLED.load(Ordering::SeqCst)),
        Action::OpenDohMenu => show_doh_menu(hwnd),
        Action::SelectTab(tab) => {
            if let Some(state) = get_state(hwnd) {
                if state.tab != tab {
                    state.keyboard_focus = state.keyboard_focus.map(|_| Action::SelectTab(tab));
                    state.tab = tab;
                    state.hover = None;
                    state.hits.clear();
                    sync_edit_visibility(hwnd, state);
                }
            }
        }
        Action::RunTest => {
            if PENDING.load(Ordering::SeqCst) == 0 {
                proxy::clear_dns_cache();
                proxy::run_test();
            }
        }
        Action::FocusInput => {
            if let Some(state) = get_state(hwnd) {
                focus_domain_input(hwnd, state);
            }
        }
        Action::AddDomain => submit_domain_input(hwnd),
        Action::ToggleGroup(index) => {
            if let Some(state) = get_state(hwnd) {
                state.expanded[index] = !state.expanded[index];
            }
        }
        Action::ToggleCustomDomain(index) => {
            let domain = get_state(hwnd).and_then(|state| state.row_targets.get(index).cloned());
            if let Some(domain) = domain {
                if let Some(on) = proxy::toggle_custom_domain(&domain) {
                    if let Some(state) = get_state(hwnd) {
                        let notice = if on {
                            format!("Đã bật lại bypass cho {domain}")
                        } else {
                            format!("Đã tạm tắt bypass cho {domain}")
                        };
                        set_notice(hwnd, state, notice, if on { theme::success() } else { theme::text_sec() });
                    }
                }
            }
        }
        Action::RemoveDomain(index) => {
            let domain = get_state(hwnd).and_then(|state| state.row_targets.get(index).cloned());
            if let Some(domain) = domain {
                if proxy::remove_custom_domain(&domain) {
                    if let Some(state) = get_state(hwnd) {
                        state.keyboard_focus = None;
                        state.hover = None;
                        set_notice(hwnd, state, format!("Đã xóa {domain}"), theme::text_sec());
                    }
                }
            }
        }
        Action::Utility(command) => utility_command(hwnd, command),
        Action::QuitApp => {
            unsafe { DestroyWindow(hwnd); }
            return;
        }
    }
    unsafe { InvalidateRect(hwnd, null(), 0); }
}

fn move_control_focus(hwnd: HWND, backward: bool) {
    let Some(state) = get_state(hwnd) else { return };
    let count = state.hits.len();
    if count == 0 {
        return;
    }
    let current = if state.edit_focused { Some(Action::FocusInput) } else { state.keyboard_focus };
    let position = state.hits.iter().position(|(_, action)| Some(*action) == current);
    let next = match (position, backward) {
        (Some(index), true) => (index + count - 1) % count,
        (Some(index), false) => (index + 1) % count,
        (None, true) => count - 1,
        (None, false) => 0,
    };
    let action = state.hits[next].1;
    state.keyboard_focus = Some(action);
    if action == Action::FocusInput {
        focus_domain_input(hwnd, state);
    } else {
        unsafe { SetFocus(hwnd); }
    }
    unsafe { InvalidateRect(hwnd, null(), 0); }
}

fn scroll_list(hwnd: HWND, delta: f32) {
    let Some(state) = get_state(hwnd) else { return };
    let max = state.max_scroll;
    if let Some(scroll) = state.current_scroll_mut() {
        let next = (*scroll + delta).clamp(0.0, max);
        if next != *scroll {
            *scroll = next;
            unsafe { InvalidateRect(hwnd, null(), 0); }
        }
    }
}

fn handle_navigation_key(hwnd: HWND, msg: &MSG, editing: bool) -> bool {
    match msg.wParam as u16 {
        VK_TAB => {
            let backward = unsafe { GetKeyState(VK_SHIFT as i32) < 0 };
            move_control_focus(hwnd, backward);
        }
        VK_PRIOR if !editing => scroll_list(hwnd, -200.0),
        VK_NEXT if !editing => scroll_list(hwnd, 200.0),
        VK_ESCAPE if !editing => {
            if get_state(hwnd).is_none_or(|state| state.tab == Tab::Steam) {
                return false;
            }
            activate_action(hwnd, Action::SelectTab(Tab::Steam));
        }
        VK_RETURN | VK_SPACE if !editing => {
            let focused = get_state(hwnd).and_then(|state| {
                let focus = state.keyboard_focus?;
                state.hits.iter().any(|(_, action)| *action == focus).then_some(focus)
            });
            if let Some(action) = focused {
                if msg.lParam & (1 << 30) == 0 {
                    activate_action(hwnd, action);
                }
            }
        }
        _ => return false,
    }
    true
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
            WM_MEASUREITEM if wp == 0 && lp != 0 => {
                let item = &mut *(lp as *mut MEASUREITEMSTRUCT);
                if item.CtlType == ODT_MENU {
                    if let Some(provider) = doh_menu_provider(item.itemID as usize) {
                        let dpi = GetDpiForWindow(hwnd).max(72) as f32 / 96.0;
                        let row_height = if provider == DohProvider::Custom { 36.0 } else { 30.0 };
                        item.itemWidth = (198.0 * dpi).round() as u32;
                        item.itemHeight = (row_height * dpi).round() as u32;
                        return 1;
                    }
                }
                DefWindowProcW(hwnd, msg, wp, lp)
            }
            WM_DRAWITEM if wp == 0 && lp != 0 => {
                let item = &*(lp as *const DRAWITEMSTRUCT);
                if item.CtlType == ODT_MENU {
                    if let (Some(state), Some(provider)) = (get_state(hwnd), doh_menu_provider(item.itemID as usize)) {
                        draw_doh_menu_item(item, provider, state);
                        return 1;
                    }
                }
                DefWindowProcW(hwnd, msg, wp, lp)
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
                                if state.keyboard_focus != Some(Action::FocusInput) {
                                    state.keyboard_focus = None;
                                }
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
                    if state.keyboard_focus.take().is_some() {
                        InvalidateRect(hwnd, null(), 0);
                    }
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
                if let Some(action) = act {
                    activate_action(hwnd, action);
                }
                0
            }
            WM_MOUSEWHEEL => {
                let delta = ((wp >> 16) as u16) as i16 as f32;
                scroll_list(hwnd, -(delta / 120.0) * 34.0);
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

        // Fit the DPI-scaled window inside its monitor's work area.
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
        let monitor = MonitorFromWindow(hwnd, MONITOR_DEFAULTTONEAREST);
        let mut info = MONITORINFO { cbSize: std::mem::size_of::<MONITORINFO>() as u32, ..Default::default() };
        let work = if GetMonitorInfoW(monitor, &mut info) != 0 {
            info.rcWork
        } else {
            RECT { left: 0, top: 0, right: GetSystemMetrics(SM_CXSCREEN), bottom: GetSystemMetrics(SM_CYSCREEN) }
        };
        let available_w = work.right - work.left;
        let available_h = work.bottom - work.top;
        let ww = (rc.right - rc.left).min(available_w);
        let wh = (rc.bottom - rc.top).min(available_h);
        let sx = work.left + (available_w - ww) / 2;
        let sy = work.top + (available_h - wh) / 2;
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

        if start_minimized {
            ShowWindow(hwnd, SW_HIDE);
        } else {
            ShowWindow(hwnd, SW_SHOW);
            UpdateWindow(hwnd);
        }

        let mut msg: MSG = std::mem::zeroed();
        while GetMessageW(&mut msg, null_mut(), 0, 0) > 0 {
            let editing = !edit_hwnd.is_null() && msg.hwnd == edit_hwnd;
            if msg.message == WM_KEYDOWN && (msg.hwnd == hwnd || editing)
                && handle_navigation_key(hwnd, &msg, editing)
            {
                continue;
            }
            if msg.message == WM_KEYDOWN && editing {
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
            ("steamgames.com".into(), make_stat(5, 2, 0, "23.15.142.182", DnsState::Clean, Some(28))),
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
        size: (f32, f32),
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
        let s = dpi as f32 / 96.0;
        let pw = (size.0 * s).round() as i32;
        let ph = (size.1 * s).round() as i32;

        write_preview_bmp(path, pw, ph, |mem_dc| {
            let mut state = UiState::new(dpi, on, autostart, 0);
            state.tab = tab;
            if let Some(gi) = expand_group {
                state.expanded[gi] = true;
            }
            paint_ui(mem_dc, pw, ph, &mut state, snap, custom_list, on, direct_cdn, doh, 0);

            let has_action = |required| state.hits.iter().any(|(_, action)| *action == required);
            for (section, _) in NAV_ITEMS {
                assert!(has_action(Action::SelectTab(section)));
            }
            let (sidebar, workspace) = split_layout(size.0, size.1);
            let (switch, _) = state.hits.iter().find(|(_, action)| *action == Action::ToggleBypass).unwrap();
            assert_eq!(switch.h, PROXY_CONTROL_H);
            let center_x = switch.x + switch.w * 0.5;
            assert!(hit_test(&state, center_x, switch.y + switch.h * 0.75) == Some(Action::ToggleBypass));
            assert!(hit_test(&state, center_x, switch.y + switch.h * 0.25) == Some(Action::ToggleBypass));
            let (provider_button, _) = state.hits.iter().find(|(_, action)| *action == Action::OpenDohMenu).unwrap();
            assert!(switch.bottom() < provider_button.y);
            assert!(provider_button.y >= sidebar.bottom() - DOH_SECTION_H);
            assert!(provider_button.bottom() < sidebar.bottom());
            assert_eq!(has_action(Action::RunTest), on);
            assert_eq!(has_action(Action::FocusInput), tab == Tab::Custom);
            if tab == Tab::Steam && expand_group.is_none() && size == (WIN_W, WIN_H) {
                assert_eq!(state.max_scroll, 0.0, "collapsed groups fit the default window");
            }
            let (quit, _) = state.hits.iter().find(|(_, action)| *action == Action::QuitApp).unwrap();
            assert!(quit.y > workspace_content(workspace).bottom());
            assert!(hit_test(&state, quit.x + quit.w * 0.5, quit.cy()) == Some(Action::QuitApp));
            for (bounds, action) in &state.hits {
                assert!(bounds.x >= 0.0 && bounds.right() <= size.0);
                assert!(bounds.y >= 0.0 && bounds.bottom() <= size.1);
                if matches!(action, Action::ToggleBypass | Action::OpenDohMenu | Action::RunTest) {
                    assert!(bounds.right() <= sidebar.right());
                } else {
                    assert!(bounds.x >= workspace.x && bounds.right() <= workspace.right());
                }
            }
        });
    }

    fn write_preview_bmp(path: &str, pw: i32, ph: i32, render: impl FnOnce(HDC)) {
        let _gdip = GdiplusGuard::new();
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
            render(mem_dc);
            let src = std::slice::from_raw_parts(bits as *const u8, (pw * ph * 4) as usize);
            let bg = theme::bg();
            assert_eq!(&src[..3], &[bg.b, bg.g, bg.r], "preview background");
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

    fn render_doh_preview(path: &str, probe_result: DohProbeResult) {
        unsafe {
            let state = DohDialog::new(96);
            let hwnd = CreateDialogParamW(
                GetModuleHandleW(null()), DOH_DIALOG_ID as *const u16, null_mut(),
                Some(custom_doh_dialog), (&state as *const DohDialog) as LPARAM,
            );
            assert!(!hwnd.is_null());

            let field = doh_input_frame(hwnd, 1.0).unwrap();
            let input = doh_control_rect(hwnd, DOH_URL_FIELD, 1.0).unwrap();
            let test = doh_control_rect(hwnd, DOH_TEST, 1.0).unwrap();
            let feedback = doh_control_rect(hwnd, DOH_HELP, 1.0).unwrap();
            let cancel = doh_control_rect(hwnd, IDCANCEL, 1.0).unwrap();
            let save = doh_control_rect(hwnd, IDOK, 1.0).unwrap();
            assert!(field.bottom() < feedback.y && feedback.bottom() < cancel.y - 9.0);
            assert!(cancel.right() < test.x && test.right() < save.x);
            assert!(cancel.y == test.y && test.y == save.y);
            assert!(cancel.h == test.h && test.h == save.h && save.h <= 32.0);
            let edit = GetDlgItem(hwnd, DOH_URL_FIELD);
            assert_eq!(GetNextDlgTabItem(hwnd, edit, 0), GetDlgItem(hwnd, IDCANCEL));
            assert_eq!(GetNextDlgTabItem(hwnd, GetDlgItem(hwnd, IDCANCEL), 0), GetDlgItem(hwnd, DOH_TEST));
            assert_eq!(GetNextDlgTabItem(hwnd, GetDlgItem(hwnd, DOH_TEST), 0), GetDlgItem(hwnd, IDOK));

            let help = GetDlgItem(hwnd, DOH_HELP);
            SetWindowTextW(edit, wide("").as_ptr());
            assert!(doh_dialog_url(hwnd).is_none());
            assert_eq!(read_edit_text(help), DOH_EXAMPLE);

            SetWindowTextW(edit, wide("not a URL").as_ptr());
            SendMessageW(hwnd, WM_COMMAND, DOH_TEST as usize, 0);
            assert_eq!(read_edit_text(help), DOH_INVALID_URL);
            let url = "https://dns.google/dns-query";
            SetWindowTextW(edit, wide(url).as_ptr());
            assert_eq!(doh_dialog_url(hwnd).as_deref(), Some(url));
            assert_eq!(read_edit_text(help), DOH_EXAMPLE);
            let origin = SendMessageW(edit, windows_sys::Win32::UI::Controls::EM_POSFROMCHAR, 0, 0);
            assert_ne!(origin, -1);
            let line_top = input.y + ((origin >> 16) as i16) as f32;
            assert!(
                (line_top + input.h * 0.5 - field.cy()).abs() <= 1.0,
                "native URL text is vertically centered",
            );

            let expected_color = if probe_result.is_ok() { theme::success() } else { theme::danger() };
            let (tx, rx) = std::sync::mpsc::channel();
            tx.send(probe_result).unwrap();
            *state.probe.borrow_mut() = Some(rx);
            state.probe_matches_input.set(true);
            poll_doh_probe(hwnd, &state);
            assert!(state.probe.borrow().is_none() && state.help_color.get() == expected_color);

            let mut client = RECT::default();
            GetClientRect(hwnd, &mut client);
            assert!(client.right <= 420 && client.bottom <= 185, "compact DoH dialog");
            assert!(save.bottom() < client.bottom as f32);
            write_preview_bmp(path, client.right, client.bottom, |hdc| {
                let mut text_bounds = RECT { right: feedback.w as i32, ..Default::default() };
                let text = wide(DOH_INVALID_URL);
                let previous_font = SelectObject(hdc, state.fonts.caption as HGDIOBJ);
                DrawTextW(hdc, text.as_ptr(), (text.len() - 1) as i32, &mut text_bounds,
                    DT_CALCRECT | DT_WORDBREAK | DT_NOPREFIX);
                SelectObject(hdc, previous_font);
                assert!(text_bounds.bottom <= feedback.h as i32, "validation feedback fits");

                SendMessageW(hwnd, WM_PRINTCLIENT, hdc as WPARAM, 0);
                for id in [DOH_URL_LABEL, DOH_URL_FIELD, DOH_HELP, IDCANCEL, DOH_TEST, IDOK] {
                    let bounds = doh_control_rect(hwnd, id, 1.0).unwrap().to_gdi_rect(1.0);
                    let saved = SaveDC(hdc);
                    IntersectClipRect(hdc, bounds.left, bounds.top, bounds.right, bounds.bottom);
                    SetViewportOrgEx(hdc, bounds.left, bounds.top, null_mut());
                    if id == DOH_URL_FIELD {
                        // A hidden native edit does not print its text into the offscreen DC.
                        let text = read_edit_text(edit);
                        let mut canvas = Canvas::new(hdc, 96);
                        canvas.draw_text(&text, RectF::new(1.0, 0.0, input.w - 2.0, input.h),
                            state.fonts.body, theme::text(), DT_LEFT);
                    } else {
                        SendMessageW(GetDlgItem(hwnd, id), WM_PRINT, hdc as WPARAM, (PRF_CLIENT | PRF_ERASEBKGND) as LPARAM);
                    }
                    RestoreDC(hdc, saved);
                }
            });
            DestroyWindow(hwnd);
        }
    }

    #[test]
    fn keyboard_switches_workspace_tabs() {
        unsafe {
            let window = CreateWindowExW(
                0, wide("STATIC").as_ptr(), null(), 0, 0, 0, 0, 0,
                HWND_MESSAGE, null_mut(), GetModuleHandleW(null()), null(),
            );
            assert!(!window.is_null());
            let mut state = UiState::new(96, true, false, 0);
            let bounds = RectF::new(0.0, 0.0, 50.0, 30.0);
            state.hits.extend([
                (bounds, Action::SelectTab(Tab::Steam)),
                (bounds, Action::SelectTab(Tab::Custom)),
                (bounds, Action::OpenDohMenu),
            ]);
            SetWindowLongPtrW(window, GWLP_USERDATA, (&mut state as *mut UiState) as isize);

            let tab = MSG { hwnd: window, message: WM_KEYDOWN, wParam: VK_TAB as WPARAM, ..Default::default() };
            handle_navigation_key(window, &tab, false);
            assert!(state.keyboard_focus == Some(Action::SelectTab(Tab::Steam)));
            handle_navigation_key(window, &tab, false);
            let enter = MSG { wParam: VK_RETURN as WPARAM, ..tab };
            handle_navigation_key(window, &enter, false);
            assert!(state.tab == Tab::Custom);
            assert!(state.keyboard_focus == Some(Action::SelectTab(Tab::Custom)));
            let escape = MSG { wParam: VK_ESCAPE as WPARAM, ..tab };
            assert!(handle_navigation_key(window, &escape, false));
            assert!(state.tab == Tab::Steam);
            assert!(state.keyboard_focus == Some(Action::SelectTab(Tab::Steam)));

            DestroyWindow(window);
        }
    }

    #[test]
    fn themed_doh_menu_preserves_names_and_checked_provider() {
        unsafe {
            let menu = create_doh_menu(DohProvider::Google, true);
            assert!(!menu.is_null());
            for (index, provider) in DohProvider::ALL.into_iter().enumerate() {
                let command = ID_MENU_DOH_BASE + index;
                assert!(doh_menu_provider(command) == Some(provider));
                let mut label = [0u16; 80];
                let mut item = MENUITEMINFOW {
                    cbSize: std::mem::size_of::<MENUITEMINFOW>() as u32,
                    fMask: MIIM_FTYPE | MIIM_STATE | MIIM_STRING,
                    dwTypeData: label.as_mut_ptr(),
                    cch: label.len() as u32,
                    ..Default::default()
                };
                assert_ne!(GetMenuItemInfoW(menu, command as u32, 0, &mut item), 0);
                assert_ne!(item.fType & MFT_OWNERDRAW, 0);
                assert_eq!(String::from_utf16_lossy(&label[..item.cch as usize]), doh_menu_label(provider));
                assert_eq!(item.fState & MFS_CHECKED != 0, provider == DohProvider::Google);
            }
            DestroyMenu(menu);
        }
    }

    #[test]
    fn render_ui_previews() {
        let dir = env!("CARGO_MANIFEST_DIR");
        let snap_custom = sample_snap(true);
        let mut snap_steam = sample_snap(false);
        let (_, failed) = snap_steam.last_mut().unwrap();
        *failed = make_stat(failed.group, 0, 1, "", DnsState::Unknown, None);
        let custom_list = vec![
            CustomDomain { domain: "bbc.com".into(), enabled: true },
            CustomDomain { domain: "medium.com".into(), enabled: true },
            CustomDomain { domain: "imgur.com".into(), enabled: false },
        ];

        render_bmp(
            &format!(r"{dir}\target\preview_proxy_strip_dark.bmp"),
            (WIN_W, WIN_H),
            96,
            Tab::Steam,
            true,
            true,
            DohProvider::Cloudflare,
            true,
            None,
            &snap_steam,
            &custom_list,
        );
        render_bmp(
            &format!(r"{dir}\target\preview_proxy_strip_steam.bmp"),
            (MIN_W, MIN_H),
            120,
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
            &format!(r"{dir}\target\preview_proxy_strip_web.bmp"),
            (WIN_W, WIN_H),
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

        set_light(true);
        render_bmp(
            &format!(r"{dir}\target\preview_proxy_strip_light.bmp"),
            (WIN_W, WIN_H),
            96,
            Tab::Steam,
            true,
            true,
            DohProvider::Cloudflare,
            true,
            None,
            &snap_steam,
            &custom_list,
        );
        set_light(false);

        for (light, suffix) in [(false, "dark"), (true, "light")] {
            set_light(light);
            render_bmp(
                &format!(r"{dir}\target\preview_proxy_strip_utilities_{suffix}.bmp"),
                (WIN_W, WIN_H),
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
        render_bmp(
            &format!(r"{dir}\target\preview_proxy_strip_minimum.bmp"),
            (MIN_W, MIN_H),
            144,
            Tab::Steam,
            false,
            false,
            DohProvider::Custom,
            false,
            None,
            &snap_steam,
            &custom_list,
        );
        render_doh_preview(&format!(r"{dir}\target\preview_doh_actions_dark.bmp"), Ok(1));
        set_light(true);
        render_doh_preview(&format!(r"{dir}\target\preview_doh_actions_light.bmp"), Err(proxy::DohError::Timeout));
        set_light(false);
    }
}
