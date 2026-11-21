//! Steam Bypass desktop palette and layout tokens.

#[derive(Clone, Copy, PartialEq, Eq)]
pub struct Color {
    pub r: u8,
    pub g: u8,
    pub b: u8,
}

impl Color {
    pub const fn rgb(r: u8, g: u8, b: u8) -> Self {
        Self { r, g, b }
    }

    const fn gray(value: u8) -> Self {
        Self::rgb(value, value, value)
    }

    const fn hex(value: u32) -> Self {
        Self::rgb((value >> 16) as u8, (value >> 8) as u8, value as u8)
    }

    /// 0xAARRGGBB for GDI+.
    pub const fn argb(self) -> u32 {
        0xff00_0000 | ((self.r as u32) << 16) | ((self.g as u32) << 8) | (self.b as u32)
    }

    /// 0x00BBGGRR for Win32 GDI / DWM.
    pub const fn colorref(self) -> u32 {
        ((self.b as u32) << 16) | ((self.g as u32) << 8) | (self.r as u32)
    }
}

pub const WIN_W: f32 = 832.0;
pub const WIN_H: f32 = 468.0;
pub const MIN_W: f32 = 716.0;
pub const MIN_H: f32 = 440.0;

pub const PAD_X: f32 = 20.0;
pub const PAD_Y: f32 = 16.0;
pub const R_PANEL: f32 = 10.0;
pub const R_BTN: f32 = 8.0;
pub const R_SM: f32 = 4.0;

thread_local! {
    static LIGHT: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

pub fn is_light() -> bool {
    LIGHT.get()
}

pub fn set_light(light: bool) {
    LIGHT.set(light);
}

macro_rules! theme_color {
    ($name:ident, $mode:ident, $dark:literal, $light:literal) => {
        pub fn $name() -> Color {
            Color::$mode(if is_light() { $light } else { $dark })
        }
    };
}

// Surfaces, controls, dividers and neutral text are strictly grayscale.
theme_color!(bg, gray, 0x00, 0xff);
theme_color!(surface, gray, 0x16, 0xf5);
theme_color!(surface_inset, gray, 0x0e, 0xfa);
theme_color!(surface_hover, gray, 0x27, 0xe9);
theme_color!(border, gray, 0x68, 0x88);
theme_color!(border_sub, gray, 0x31, 0xdb);
theme_color!(text, gray, 0xf7, 0x11);
theme_color!(text_sec, gray, 0xe0, 0x26);
theme_color!(text_muted, gray, 0xb8, 0x4d);
theme_color!(text_dim, gray, 0x9c, 0x62);
theme_color!(primary, gray, 0xf0, 0x1a);
theme_color!(primary_hover, gray, 0xd6, 0x3a);
theme_color!(on_primary, gray, 0x10, 0xff);
theme_color!(neutral, gray, 0x55, 0x77);
theme_color!(switch_thumb, gray, 0xf5, 0xff);

// Only text and status marks use chromatic colors.
theme_color!(info, hex, 0x9acbff, 0x154e99);
theme_color!(success, hex, 0x79dfa2, 0x11663a);
theme_color!(danger, hex, 0xff969d, 0xa51d33);
