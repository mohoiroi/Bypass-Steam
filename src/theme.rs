//! Design system tokens generated via UI UX Pro Max (`MASTER.md`):
//! Minimalist Scientific Utility · Variance 8 (Bold/Asymmetric) · Motion 2 (Subtle) · Density 3 (Spacious).

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

// ---- Semantic color tokens (from design-system/steam-bypass/MASTER.md) ----
pub const BG: Color = Color::rgb(0x0F, 0x17, 0x2A);           // --color-background
pub const SURFACE: Color = Color::rgb(0x19, 0x21, 0x34);      // --color-card
pub const SURFACE_INSET: Color = Color::rgb(0x10, 0x19, 0x2E);// --color-muted
pub const SURFACE_HOVER: Color = Color::rgb(0x21, 0x2C, 0x44);
pub const SURFACE_ELEV: Color = Color::rgb(0x33, 0x41, 0x55); // --color-secondary
pub const PRIMARY: Color = Color::rgb(0x1E, 0x3A, 0x5F);      // --color-primary (Shield Navy)
pub const PRIMARY_HOVER: Color = Color::rgb(0x27, 0x4B, 0x7A);
pub const BTN_BG: Color = Color::rgb(0x1E, 0x29, 0x3B);
pub const BORDER: Color = Color::rgb(0x2A, 0x36, 0x4F);       // Crisp scientific hairline
pub const BORDER_SUB: Color = Color::rgb(0x1F, 0x2A, 0x40);

pub const TEXT: Color = Color::rgb(0xFF, 0xFF, 0xFF);         // --color-foreground (>15:1)
pub const TEXT_SEC: Color = Color::rgb(0xE2, 0xE8, 0xF0);     // (>12:1)
pub const TEXT_MUTED: Color = Color::rgb(0x94, 0xA3, 0xB8);   // --color-muted-foreground (>5.5:1)
pub const TEXT_DIM: Color = Color::rgb(0x64, 0x74, 0x8B);

pub const ACCENT: Color = Color::rgb(0x22, 0xC5, 0x5E);       // --color-accent (Connected Green)
pub const ACCENT_HOVER: Color = Color::rgb(0x34, 0xD3, 0x70);
pub const ON_ACCENT: Color = Color::rgb(0x0F, 0x17, 0x2A);    // --color-on-accent (>8:1)
pub const INFO: Color = Color::rgb(0x38, 0xBD, 0xF8);         // Telemetry Cyan / Direct CDN
pub const SUCCESS: Color = Color::rgb(0x22, 0xC5, 0x5E);
pub const WARNING: Color = Color::rgb(0xF5, 0x9E, 0x0B);
pub const DANGER: Color = Color::rgb(0xEF, 0x44, 0x44);       // Readable destructive on dark
pub const DESTRUCTIVE: Color = Color::rgb(0xDC, 0x26, 0x26);  // --color-destructive
pub const NEUTRAL: Color = Color::rgb(0x47, 0x55, 0x69);
pub const WHITE: Color = Color::rgb(0xFF, 0xFF, 0xFF);

pub const fn tint(c: Color) -> Color {
    c.with_alpha(30)
}

// ---- Window & layout tokens (Spacious Density 3/10, 8px base grid) ----
pub const WIN_W: f32 = 528.0;
pub const WIN_H: f32 = 620.0;
pub const MIN_W: f32 = 480.0;
pub const MIN_H: f32 = 540.0;

pub const PAD_X: f32 = 24.0;
pub const R_CARD: f32 = 10.0;
pub const R_BTN: f32 = 8.0;
pub const R_SM: f32 = 6.0;
