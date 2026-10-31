//! Fallbacks for platforms without a native implementation (Linux builds
//! compile, but capture protection is not available there).

use raw_window_handle::HasWindowHandle;

use super::{DisplayInfo, Permission, Rect};

pub fn displays() -> Vec<DisplayInfo> {
    xcap::Monitor::all()
        .unwrap_or_default()
        .into_iter()
        .map(|m| {
            let bounds = Rect {
                x: m.x().unwrap_or(0) as f64,
                y: m.y().unwrap_or(0) as f64,
                width: m.width().unwrap_or(0) as f64,
                height: m.height().unwrap_or(0) as f64,
            };
            DisplayInfo {
                id: m.id().unwrap_or(0).to_string(),
                bounds,
                work_area: bounds,
                scale: m.scale_factor().unwrap_or(1.0) as f64,
                primary: m.is_primary().unwrap_or(false),
                notch: None,
            }
        })
        .collect()
}

pub fn cursor_position() -> (f64, f64) {
    (0.0, 0.0)
}

pub fn set_accessory_app() {}

pub fn guard_key_window_deadlock() {}
pub fn activate_app() {}

pub fn screen_permission() -> Permission {
    Permission::Granted
}

pub fn request_screen_permission() -> bool {
    true
}

pub fn open_screen_settings() {}

pub fn microphone_permission() -> Permission {
    Permission::Granted
}

pub fn request_microphone() -> futures::channel::oneshot::Receiver<bool> {
    let (tx, rx) = futures::channel::oneshot::channel();
    let _ = tx.send(true);
    rx
}

#[derive(Clone, Copy, Debug)]
pub struct NativeWindow;

impl NativeWindow {
    pub fn from_window(_window: &impl HasWindowHandle) -> Option<Self> {
        None
    }
    pub fn configure_overlay(&self, _level_offset: isize, _resizable: bool) {}
    pub fn set_content_protection(&self, _on: bool) {}
    pub fn set_ignore_mouse(&self, _on: bool) {}
    pub fn set_opacity(&self, _value: f32) {}
    pub fn show(&self, _focus: bool) {}
    pub fn hide(&self) {}
    pub fn is_visible(&self) -> bool {
        true
    }
    pub fn is_key(&self) -> bool {
        true
    }
    pub fn frame(&self) -> Rect {
        Rect::default()
    }
    pub fn set_frame(&self, _rect: Rect) {}
    pub fn begin_drag(&self) {}
    pub fn make_borderless(&self) {}

    pub fn send_click(&self, _x: f64, _y: f64, _right: bool) {}

    pub fn send_scroll(&self, _x: f64, _y: f64, _lines: i32) {}

    pub fn window_number(&self) -> isize {
        0
    }
}
