//! Design system tokens: colors, spacing, radii, and window dimensions.

#[derive(Clone, Copy, PartialEq, Eq)]
pub struct Color {
    pub r: u8,
    pub g: u8,
    pub b: u8,
    pub a: u8,
}

impl Color {
    pub const fn rgb(r: u8, g: u8, b: u8) -> Self {
        Self { r, g, b, a: 255 }
    }

    pub const fn rgba(r: u8, g: u8, b: u8, a: u8) -> Self {
        Self { r, g, b, a }
    }

    pub const fn with_alpha(self, a: u8) -> Self {
        Self { a, ..self }
    }

    /// 0xAARRGGBB for GDI+.
    pub const fn argb(self) -> u32 {
        ((self.a as u32) << 24)
            | ((self.r as u32) << 16)
            | ((self.g as u32) << 8)
            | (self.b as u32)
    }

    /// 0x00BBGGRR for Win32 GDI / DWM.
    pub const fn colorref(self) -> u32 {
        ((self.b as u32) << 16) | ((self.g as u32) << 8) | (self.r as u32)
    }

    pub fn lerp(a: Color, b: Color, t: f32) -> Color {
        let t = t.clamp(0.0, 1.0);
        let ch = |x: u8, y: u8| -> u8 {
            (x as f32 + (y as f32 - x as f32) * t).round().clamp(0.0, 255.0) as u8
        };
        Color::rgba(ch(a.r, b.r), ch(a.g, b.g), ch(a.b, b.b), ch(a.a, b.a))
    }
}

// ---- Color tokens ----
pub const BG: Color = Color::rgb(0x0D, 0x10, 0x17);
pub const SURFACE: Color = Color::rgb(0x14, 0x19, 0x23);
pub const SURFACE_INSET: Color = Color::rgb(0x10, 0x14, 0x1D);
pub const SURFACE_HOVER: Color = Color::rgb(0x1B, 0x22, 0x30);
pub const BTN_BG: Color = Color::rgb(0x1B, 0x22, 0x30);
pub const BTN_HOVER: Color = Color::rgb(0x24, 0x2E, 0x42);
pub const BORDER: Color = Color::rgb(0x23, 0x2A, 0x38);
pub const BORDER_SUB: Color = Color::rgb(0x1C, 0x22, 0x30);

pub const TEXT: Color = Color::rgb(0xF1, 0xF5, 0xF9);
pub const TEXT_SEC: Color = Color::rgb(0xCB, 0xD5, 0xE1);
pub const TEXT_MUTED: Color = Color::rgb(0x8B, 0x95, 0xA7);
pub const TEXT_DIM: Color = Color::rgb(0x5B, 0x64, 0x75);

pub const ACCENT: Color = Color::rgb(0x3B, 0x82, 0xF6);
pub const ACCENT_HOVER: Color = Color::rgb(0x52, 0x94, 0xFF);
pub const SUCCESS: Color = Color::rgb(0x22, 0xC5, 0x5E);
pub const WARNING: Color = Color::rgb(0xF5, 0x9E, 0x0B);
pub const DANGER: Color = Color::rgb(0xEF, 0x44, 0x44);
pub const NEUTRAL: Color = Color::rgb(0x64, 0x74, 0x8B);
pub const WHITE: Color = Color::rgb(0xFF, 0xFF, 0xFF);

pub const fn tint(c: Color) -> Color {
    c.with_alpha(34)
}

// ---- Window & layout tokens (logical px at 96 DPI) ----
pub const WIN_W: f32 = 460.0;
pub const WIN_H: f32 = 476.0;
pub const MIN_W: f32 = 420.0;
pub const MIN_H: f32 = 440.0;

pub const PAD_X: f32 = 18.0;
pub const R_CARD: f32 = 12.0;
pub const R_BTN: f32 = 8.0;
pub const R_SM: f32 = 6.0;
