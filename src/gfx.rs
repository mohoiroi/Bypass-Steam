//! Leak-free RAII wrappers around Win32 GDI+ (anti-aliased shapes) and GDI ClearType text.

use crate::proxy::wide;
use crate::theme::Color;
use std::ptr::null_mut;
use windows_sys::Win32::{
    Foundation::RECT,
    Foundation::SIZE,
    Graphics::{Gdi::*, GdiPlus::*},
};

pub struct GdiplusGuard(usize);

impl GdiplusGuard {
    pub fn new() -> Self {
        let mut token: usize = 0;
        let input = GdiplusStartupInput {
            GdiplusVersion: 1,
            DebugEventCallback: 0,
            SuppressBackgroundThread: 0,
            SuppressExternalCodecs: 0,
        };
        unsafe {
            GdiplusStartup(&mut token, &input, null_mut());
        }
        Self(token)
    }
}

impl Drop for GdiplusGuard {
    fn drop(&mut self) {
        if self.0 != 0 {
            unsafe {
                GdiplusShutdown(self.0);
            }
        }
    }
}

#[derive(Clone, Copy, Debug, Default)]
pub struct RectF {
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
}

impl RectF {
    pub const fn new(x: f32, y: f32, w: f32, h: f32) -> Self {
        Self { x, y, w, h }
    }

    pub fn right(self) -> f32 {
        self.x + self.w
    }

    pub fn bottom(self) -> f32 {
        self.y + self.h
    }

    pub fn cy(self) -> f32 {
        self.y + self.h * 0.5
    }

    pub fn contains(self, px: f32, py: f32) -> bool {
        px >= self.x && px <= self.right() && py >= self.y && py <= self.bottom()
    }

    pub fn inset(self, dx: f32, dy: f32) -> Self {
        Self {
            x: self.x + dx,
            y: self.y + dy,
            w: (self.w - dx * 2.0).max(0.0),
            h: (self.h - dy * 2.0).max(0.0),
        }
    }

    pub fn to_gdi_rect(self, scale: f32) -> RECT {
        RECT {
            left: (self.x * scale).round() as i32,
            top: (self.y * scale).round() as i32,
            right: (self.right() * scale).round() as i32,
            bottom: (self.bottom() * scale).round() as i32,
        }
    }
}

pub struct FontSet {
    pub kpi: HFONT,
    pub title: HFONT,
    pub group: HFONT,
    pub body: HFONT,
    pub caption: HFONT,
    pub badge: HFONT,
    pub mono: HFONT,
    pub mono_bold: HFONT,
}

fn make_font(face: &str, px: f32, weight: u32) -> HFONT {
    let face_w = wide(face);
    unsafe {
        CreateFontW(
            -(px.round() as i32).max(1),
            0,
            0,
            0,
            weight as i32,
            0,
            0,
            0,
            DEFAULT_CHARSET as u32,
            OUT_DEFAULT_PRECIS as u32,
            CLIP_DEFAULT_PRECIS as u32,
            CLEARTYPE_QUALITY as u32,
            DEFAULT_PITCH as u32,
            face_w.as_ptr(),
        )
    }
}

impl FontSet {
    pub fn new(dpi: u32) -> Self {
        let s = dpi as f32 / 96.0;
        Self {
            kpi: make_font("Consolas", 21.0 * s, FW_BOLD),
            title: make_font("Segoe UI", 16.5 * s, FW_BOLD),
            group: make_font("Segoe UI", 13.5 * s, FW_SEMIBOLD),
            body: make_font("Segoe UI", 12.5 * s, FW_NORMAL),
            caption: make_font("Segoe UI", 11.5 * s, FW_NORMAL),
            badge: make_font("Segoe UI", 11.0 * s, FW_SEMIBOLD),
            mono: make_font("Consolas", 11.0 * s, FW_NORMAL),
            mono_bold: make_font("Consolas", 11.5 * s, FW_BOLD),
        }
    }
}

impl Drop for FontSet {
    fn drop(&mut self) {
        unsafe {
            for h in [
                self.kpi,
                self.title,
                self.group,
                self.body,
                self.caption,
                self.badge,
                self.mono,
                self.mono_bold,
            ] {
                if !h.is_null() {
                    DeleteObject(h as HGDIOBJ);
                }
            }
        }
    }
}

pub struct Canvas {
    hdc: HDC,
    gp: *mut GpGraphics,
    pub scale: f32,
    clip: Option<RectF>,
    gdi_saved: i32,
}

impl Canvas {
    pub fn new(hdc: HDC, dpi: u32) -> Self {
        Self {
            hdc,
            gp: null_mut(),
            scale: dpi as f32 / 96.0,
            clip: None,
            gdi_saved: 0,
        }
    }

    /// GDI+ and GDI must not draw into the same HDC concurrently; flush GpGraphics before GDI text.
    fn flush_gp(&mut self) {
        if !self.gp.is_null() {
            unsafe {
                GdipDeleteGraphics(self.gp);
            }
            self.gp = null_mut();
        }
    }

    fn ensure_gp(&mut self) -> *mut GpGraphics {
        if self.gp.is_null() {
            unsafe {
                GdipCreateFromHDC(self.hdc, &mut self.gp);
                if !self.gp.is_null() {
                    GdipSetSmoothingMode(self.gp, SmoothingModeAntiAlias);
                    GdipSetPixelOffsetMode(self.gp, PixelOffsetModeHighQuality);
                    if let Some(r) = self.clip {
                        let s = self.scale;
                        GdipSetClipRect(self.gp, r.x * s, r.y * s, r.w * s, r.h * s, CombineModeReplace);
                    }
                }
            }
        }
        self.gp
    }

    pub fn set_clip(&mut self, clip: Option<RectF>) {
        self.flush_gp();
        unsafe {
            if self.gdi_saved != 0 {
                RestoreDC(self.hdc, self.gdi_saved);
                self.gdi_saved = 0;
            }
            self.clip = clip;
            if let Some(r) = clip {
                let saved = SaveDC(self.hdc);
                if saved != 0 {
                    self.gdi_saved = saved;
                }
                let gr = r.to_gdi_rect(self.scale);
                IntersectClipRect(self.hdc, gr.left, gr.top, gr.right, gr.bottom);
            }
        }
    }

    unsafe fn round_rect_path(&self, r: RectF, radius: f32) -> *mut GpPath {
        let s = self.scale;
        let x = r.x * s;
        let y = r.y * s;
        let w = r.w * s;
        let h = r.h * s;
        let rad = (radius * s).max(0.0).min(w.min(h) * 0.5);
        let mut path: *mut GpPath = null_mut();
        unsafe {
            GdipCreatePath(FillModeAlternate, &mut path);
            if path.is_null() {
                return null_mut();
            }
            if rad < 0.5 {
                GdipAddPathLine(path, x, y, x + w, y);
                GdipAddPathLine(path, x + w, y, x + w, y + h);
                GdipAddPathLine(path, x + w, y + h, x, y + h);
                GdipClosePathFigure(path);
            } else {
                let d = rad * 2.0;
                GdipAddPathArc(path, x, y, d, d, 180.0, 90.0);
                GdipAddPathArc(path, x + w - d, y, d, d, 270.0, 90.0);
                GdipAddPathArc(path, x + w - d, y + h - d, d, d, 0.0, 90.0);
                GdipAddPathArc(path, x, y + h - d, d, d, 90.0, 90.0);
                GdipClosePathFigure(path);
            }
        }
        path
    }

    pub fn fill_rect(&mut self, r: RectF, color: Color) {
        let gp = self.ensure_gp();
        if gp.is_null() {
            return;
        }
        let s = self.scale;
        unsafe {
            let mut brush: *mut GpSolidFill = null_mut();
            GdipCreateSolidFill(color.argb(), &mut brush);
            if !brush.is_null() {
                GdipFillRectangle(gp, brush as *mut GpBrush, r.x * s, r.y * s, r.w * s, r.h * s);
                GdipDeleteBrush(brush as *mut GpBrush);
            }
        }
    }

    pub fn fill_round_rect(&mut self, r: RectF, radius: f32, color: Color) {
        let gp = self.ensure_gp();
        if gp.is_null() || r.w <= 0.0 || r.h <= 0.0 {
            return;
        }
        unsafe {
            let path = self.round_rect_path(r, radius);
            let mut brush: *mut GpSolidFill = null_mut();
            GdipCreateSolidFill(color.argb(), &mut brush);
            if !path.is_null() && !brush.is_null() {
                GdipFillPath(gp, brush as *mut GpBrush, path);
            }
            if !brush.is_null() {
                GdipDeleteBrush(brush as *mut GpBrush);
            }
            if !path.is_null() {
                GdipDeletePath(path);
            }
        }
    }

    pub fn stroke_round_rect(&mut self, r: RectF, radius: f32, width: f32, color: Color) {
        let gp = self.ensure_gp();
        if gp.is_null() || r.w <= 0.0 || r.h <= 0.0 {
            return;
        }
        let hw = width * 0.5;
        let ir = r.inset(hw, hw);
        let irad = (radius - hw).max(0.0);
        unsafe {
            let path = self.round_rect_path(ir, irad);
            let mut pen: *mut GpPen = null_mut();
            GdipCreatePen1(color.argb(), width * self.scale, UnitPixel, &mut pen);
            if !path.is_null() && !pen.is_null() {
                GdipDrawPath(gp, pen, path);
            }
            if !pen.is_null() {
                GdipDeletePen(pen);
            }
            if !path.is_null() {
                GdipDeletePath(path);
            }
        }
    }

    pub fn fill_circle(&mut self, cx: f32, cy: f32, radius: f32, color: Color) {
        let gp = self.ensure_gp();
        if gp.is_null() {
            return;
        }
        let s = self.scale;
        let d = radius * 2.0 * s;
        unsafe {
            let mut brush: *mut GpSolidFill = null_mut();
            GdipCreateSolidFill(color.argb(), &mut brush);
            if !brush.is_null() {
                GdipFillEllipse(gp, brush as *mut GpBrush, (cx - radius) * s, (cy - radius) * s, d, d);
                GdipDeleteBrush(brush as *mut GpBrush);
            }
        }
    }

    pub fn stroke_circle(&mut self, cx: f32, cy: f32, radius: f32, width: f32, color: Color) {
        let gp = self.ensure_gp();
        if gp.is_null() {
            return;
        }
        let s = self.scale;
        let d = radius * 2.0 * s;
        unsafe {
            let mut pen: *mut GpPen = null_mut();
            GdipCreatePen1(color.argb(), width * s, UnitPixel, &mut pen);
            if !pen.is_null() {
                GdipDrawEllipse(gp, pen, (cx - radius) * s, (cy - radius) * s, d, d);
                GdipDeletePen(pen);
            }
        }
    }

    pub fn draw_line(&mut self, x1: f32, y1: f32, x2: f32, y2: f32, width: f32, color: Color, round: bool) {
        let gp = self.ensure_gp();
        if gp.is_null() {
            return;
        }
        let s = self.scale;
        unsafe {
            let mut pen: *mut GpPen = null_mut();
            GdipCreatePen1(color.argb(), width * s, UnitPixel, &mut pen);
            if !pen.is_null() {
                if round {
                    GdipSetPenStartCap(pen, LineCapRound);
                    GdipSetPenEndCap(pen, LineCapRound);
                }
                GdipDrawLine(gp, pen, x1 * s, y1 * s, x2 * s, y2 * s);
                GdipDeletePen(pen);
            }
        }
    }

    pub fn draw_chevron(&mut self, cx: f32, cy: f32, r: f32, expanded: bool, color: Color) {
        let gp = self.ensure_gp();
        if gp.is_null() {
            return;
        }
        let s = self.scale;
        let pts = if expanded {
            [
                PointF { X: (cx - r * 0.9) * s, Y: (cy - r * 0.45) * s },
                PointF { X: cx * s, Y: (cy + r * 0.5) * s },
                PointF { X: (cx + r * 0.9) * s, Y: (cy - r * 0.45) * s },
            ]
        } else {
            [
                PointF { X: (cx - r * 0.45) * s, Y: (cy - r * 0.9) * s },
                PointF { X: (cx + r * 0.5) * s, Y: cy * s },
                PointF { X: (cx - r * 0.45) * s, Y: (cy + r * 0.9) * s },
            ]
        };
        unsafe {
            let mut pen: *mut GpPen = null_mut();
            GdipCreatePen1(color.argb(), 1.6 * s, UnitPixel, &mut pen);
            if !pen.is_null() {
                GdipSetPenStartCap(pen, LineCapRound);
                GdipSetPenEndCap(pen, LineCapRound);
                GdipSetPenLineJoin(pen, LineJoinRound);
                GdipDrawLines(gp, pen, pts.as_ptr(), 3);
                GdipDeletePen(pen);
            }
        }
    }

    pub fn draw_checkmark(&mut self, cx: f32, cy: f32, size: f32, width: f32, color: Color) {
        let gp = self.ensure_gp();
        if gp.is_null() {
            return;
        }
        let s = self.scale;
        let pts = [
            PointF { X: (cx - size * 0.42) * s, Y: (cy + size * 0.02) * s },
            PointF { X: (cx - size * 0.10) * s, Y: (cy + size * 0.34) * s },
            PointF { X: (cx + size * 0.44) * s, Y: (cy - size * 0.32) * s },
        ];
        unsafe {
            let mut pen: *mut GpPen = null_mut();
            GdipCreatePen1(color.argb(), width * s, UnitPixel, &mut pen);
            if !pen.is_null() {
                GdipSetPenStartCap(pen, LineCapRound);
                GdipSetPenEndCap(pen, LineCapRound);
                GdipSetPenLineJoin(pen, LineJoinRound);
                GdipDrawLines(gp, pen, pts.as_ptr(), 3);
                GdipDeletePen(pen);
            }
        }
    }

    pub fn draw_plus(&mut self, cx: f32, cy: f32, r: f32, width: f32, color: Color) {
        self.draw_line(cx - r, cy, cx + r, cy, width, color, true);
        self.draw_line(cx, cy - r, cx, cy + r, width, color, true);
    }

    pub fn draw_cross(&mut self, cx: f32, cy: f32, r: f32, width: f32, color: Color) {
        self.draw_line(cx - r, cy - r, cx + r, cy + r, width, color, true);
        self.draw_line(cx + r, cy - r, cx - r, cy + r, width, color, true);
    }

    pub fn measure_text(&mut self, text: &str, font: HFONT) -> (f32, f32) {
        if text.is_empty() {
            return (0.0, 0.0);
        }
        self.flush_gp();
        let w: Vec<u16> = text.encode_utf16().collect();
        let mut sz = SIZE { cx: 0, cy: 0 };
        unsafe {
            let prev = SelectObject(self.hdc, font as HGDIOBJ);
            GetTextExtentPoint32W(self.hdc, w.as_ptr(), w.len() as i32, &mut sz);
            SelectObject(self.hdc, prev);
        }
        (sz.cx as f32 / self.scale, sz.cy as f32 / self.scale)
    }

    pub fn draw_text(&mut self, text: &str, rect: RectF, font: HFONT, color: Color, align: u32) {
        if text.is_empty() || rect.w <= 1.0 || rect.h <= 1.0 {
            return;
        }
        self.flush_gp();
        let w: Vec<u16> = text.encode_utf16().collect();
        let mut r = rect.to_gdi_rect(self.scale);
        unsafe {
            let prev = SelectObject(self.hdc, font as HGDIOBJ);
            SetBkMode(self.hdc, TRANSPARENT as i32);
            SetTextColor(self.hdc, color.colorref());
            DrawTextW(
                self.hdc,
                w.as_ptr(),
                w.len() as i32,
                &mut r,
                DT_SINGLELINE | DT_VCENTER | DT_NOPREFIX | DT_END_ELLIPSIS | align,
            );
            SelectObject(self.hdc, prev);
        }
    }
}

impl Drop for Canvas {
    fn drop(&mut self) {
        self.flush_gp();
        if self.gdi_saved != 0 {
            unsafe {
                RestoreDC(self.hdc, self.gdi_saved);
            }
        }
    }
}
