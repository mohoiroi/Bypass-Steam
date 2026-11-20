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

pub const WIN_W: f32 = 568.0;
pub const WIN_H: f32 = 474.0;
pub const MIN_W: f32 = 492.0;
pub const MIN_H: f32 = 442.0;

pub const PAD_X: f32 = 16.0;
pub const R_PANEL: f32 = 8.0;
pub const R_BTN: f32 = 6.0;
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
theme_color!(surface, gray, 0x14, 0xf3);
theme_color!(surface_inset, gray, 0x0d, 0xfa);
theme_color!(surface_hover, gray, 0x1d, 0xec);
theme_color!(border, gray, 0x48, 0xc4);
theme_color!(border_sub, gray, 0x29, 0xde);
theme_color!(text, gray, 0xf7, 0x11);
theme_color!(text_sec, gray, 0xe0, 0x26);
theme_color!(text_muted, gray, 0xb0, 0x52);
theme_color!(text_dim, gray, 0x95, 0x65);
theme_color!(primary, gray, 0xf0, 0x1a);
theme_color!(primary_hover, gray, 0xd6, 0x3a);
theme_color!(on_primary, gray, 0x10, 0xff);
theme_color!(neutral, gray, 0x55, 0x77);
theme_color!(switch_thumb, gray, 0xf5, 0xff);

// Only text and status marks use chromatic colors.
theme_color!(info, hex, 0x8cbdff, 0x1559a9);
theme_color!(success, hex, 0x78dca5, 0x116d3e);
theme_color!(danger, hex, 0xff8a91, 0xb02135);
