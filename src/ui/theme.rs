//! VeilPilot's look: a dark, quiet glass panel with one warm accent.
//!
//! Neutrals are a soft graphite with a trace of violet; amber marks whatever
//! is active, selected or the thing to press. Text is sentence case at sizes
//! you can read at a glance, and every control has a visible shape.

use gpui::{Font, FontStyle, FontWeight, Hsla, Rgba, font, px, rgb, rgba};

pub fn void() -> Rgba {
    rgb(0x131218)
}
pub fn glass() -> Rgba {
    rgb(0x1d1b25)
}
pub fn raise() -> Rgba {
    rgb(0x292632)
}
pub fn edge() -> Rgba {
    rgb(0x383444)
}
pub fn haze() -> Rgba {
    rgb(0xa6a0b8)
}
pub fn read() -> Rgba {
    rgb(0xf4f1ea)
}
pub fn lamp() -> Rgba {
    rgb(0xf0a93b)
}
pub fn live() -> Rgba {
    rgb(0xef5b50)
}
pub fn white() -> Rgba {
    rgb(0xffffff)
}
/// Tertiary text: hints and captions. Lighter than a border so it can
/// actually be read.
pub fn faint() -> Rgba {
    rgb(0x7d7890)
}
/// Success: saved, granted, done.
pub fn ok() -> Rgba {
    rgb(0x4cc38a)
}
/// A control's resting fill, and what it becomes under the pointer.
pub fn control() -> Rgba {
    rgba(0xf4f1ea0f)
}
pub fn control_hover() -> Rgba {
    rgba(0xf4f1ea1c)
}
/// `color` at `alpha`.
pub fn alpha(color: Rgba, alpha: f32) -> Rgba {
    Rgba { a: alpha, ..color }
}

/// The shell: graphite glass at 94%.
pub fn shell_bg() -> Rgba {
    rgba(0x131218f4)
}
pub fn hairline() -> Rgba {
    rgba(0xf4f1ea1f)
}
pub fn lamp_soft() -> Rgba {
    rgba(0xf0a93b1f)
}
pub fn lamp_border() -> Rgba {
    rgba(0xf0a93b4d)
}
pub fn lamp_dim() -> Rgba {
    rgba(0xf0a93b59)
}
pub fn dim_overlay() -> Rgba {
    rgba(0x0c0b1094)
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

pub const RAIL_H: f32 = 42.0;
pub const PAD: f32 = 14.0;
/// Corner radius of buttons, fields and chips; of cards and menus.
pub const RADIUS: f32 = 7.0;
pub const RADIUS_LG: f32 = 10.0;

/// The size of labels and other chrome text.
pub fn label_size() -> gpui::Pixels {
    px(11.)
}
