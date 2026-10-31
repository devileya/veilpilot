//! VeilPilot — smoked instrument glass.
//!
//! The panel reads like a teleprompter's beamsplitter: warm paper-white text
//! floating on graphite-violet glass, with one amber instrument lamp for state.
//! All chrome is text and hairlines; every affordance is a printed hotkey.

use gpui::{Font, FontStyle, FontWeight, Hsla, Rgba, font, px, rgb, rgba};

pub fn void() -> Rgba {
    rgb(0x15131c)
}
pub fn glass() -> Rgba {
    rgb(0x211e2b)
}
pub fn raise() -> Rgba {
    rgb(0x2a2636)
}
pub fn edge() -> Rgba {
    rgb(0x3a3548)
}
pub fn haze() -> Rgba {
    rgb(0x9a93ae)
}
pub fn read() -> Rgba {
    rgb(0xf2ede4)
}
pub fn lamp() -> Rgba {
    rgb(0xe8a33d)
}
pub fn live() -> Rgba {
    rgb(0xe2574c)
}
pub fn white() -> Rgba {
    rgb(0xffffff)
}

/// The shell: graphite glass at 94%.
pub fn shell_bg() -> Rgba {
    rgba(0x15131cf0)
}
pub fn hairline() -> Rgba {
    rgba(0xf2ede41a)
}
pub fn lamp_soft() -> Rgba {
    rgba(0xe8a33d1a)
}
pub fn lamp_border() -> Rgba {
    rgba(0xe8a33d42)
}
pub fn lamp_dim() -> Rgba {
    rgba(0xe8a33d4d)
}
pub fn dim_overlay() -> Rgba {
    rgba(0x15131c94)
}

pub fn hsla(c: Rgba) -> Hsla {
    c.into()
}

pub fn ui_family() -> &'static str {
    if cfg!(target_os = "macos") {
        ".SystemUIFont"
    } else if cfg!(windows) {
        "Segoe UI"
    } else {
        "sans-serif"
    }
}

pub fn mono_family() -> &'static str {
    if cfg!(target_os = "macos") {
        "Menlo"
    } else if cfg!(windows) {
        "Consolas"
    } else {
        "monospace"
    }
}

pub fn ui_font() -> Font {
    font(ui_family())
}

pub fn mono_font() -> Font {
    font(mono_family())
}

pub fn with_weight(mut f: Font, weight: FontWeight) -> Font {
    f.weight = weight;
    f
}

pub fn italic(mut f: Font) -> Font {
    f.style = FontStyle::Italic;
    f
}

pub const RAIL_H: f32 = 38.0;
pub const PAD: f32 = 14.0;

/// The small-caps label style used for chrome (9.5px, tracked, uppercase).
pub fn label_size() -> gpui::Pixels {
    px(9.5)
}
