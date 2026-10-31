//! Win32 calls. Coordinates are virtual-screen pixels divided by the
//! monitor's DPI scale, which is what GPUI and Electron both call points.

use raw_window_handle::{HasWindowHandle, RawWindowHandle};
use windows::Win32::Foundation::{HWND, LPARAM, POINT, RECT, WPARAM};
use windows::Win32::Graphics::Gdi::{EnumDisplayMonitors, GetMonitorInfoW, HDC, HMONITOR, MONITORINFO};
use windows::Win32::UI::HiDpi::{GetDpiForMonitor, GetDpiForWindow, MDT_EFFECTIVE_DPI};
use windows::Win32::UI::Input::KeyboardAndMouse::ReleaseCapture;
use windows::Win32::UI::WindowsAndMessaging::{
    GWL_EXSTYLE, GetCursorPos, GetForegroundWindow, GetWindowLongPtrW, GetWindowRect, HTCAPTION, HWND_TOPMOST,
    IsWindowVisible, MONITORINFOF_PRIMARY, SW_HIDE, SW_SHOW, SW_SHOWNOACTIVATE, SWP_NOACTIVATE, SWP_NOMOVE, SWP_NOSIZE,
    SWP_NOZORDER, SendMessageW, SetForegroundWindow, SetWindowDisplayAffinity, SetWindowLongPtrW, SetWindowPos,
    ShowWindow, WDA_EXCLUDEFROMCAPTURE, WDA_MONITOR, WDA_NONE, WM_NCLBUTTONDOWN, WS_EX_APPWINDOW, WS_EX_LAYERED,
    WS_EX_TOOLWINDOW, WS_EX_TRANSPARENT,
};
use windows::core::BOOL;

use super::{DisplayInfo, Permission, Rect};

fn scale_for_monitor(monitor: HMONITOR) -> f64 {
    let (mut x, mut y) = (96u32, 96u32);
    unsafe {
        let _ = GetDpiForMonitor(monitor, MDT_EFFECTIVE_DPI, &mut x, &mut y);
    }
    x as f64 / 96.0
}

fn rect_to_points(r: RECT, scale: f64) -> Rect {
    Rect {
        x: r.left as f64 / scale,
        y: r.top as f64 / scale,
        width: (r.right - r.left) as f64 / scale,
        height: (r.bottom - r.top) as f64 / scale,
    }
}

unsafe extern "system" fn collect(monitor: HMONITOR, _: HDC, _: *mut RECT, data: LPARAM) -> BOOL {
    let out = unsafe { &mut *(data.0 as *mut Vec<DisplayInfo>) };
    let mut info = MONITORINFO { cbSize: std::mem::size_of::<MONITORINFO>() as u32, ..Default::default() };
    if unsafe { GetMonitorInfoW(monitor, &mut info) }.as_bool() {
        let scale = scale_for_monitor(monitor);
        out.push(DisplayInfo {
            id: (monitor.0 as isize).to_string(),
            bounds: rect_to_points(info.rcMonitor, scale),
            work_area: rect_to_points(info.rcWork, scale),
            scale,
            primary: info.dwFlags & MONITORINFOF_PRIMARY != 0,
            notch: None,
        });
    }
    BOOL(1)
}

pub fn displays() -> Vec<DisplayInfo> {
    let mut out: Vec<DisplayInfo> = Vec::new();
    unsafe {
        let _ = EnumDisplayMonitors(None, None, Some(collect), LPARAM(&mut out as *mut _ as isize));
    }
    out
}

pub fn cursor_position() -> (f64, f64) {
    let mut p = POINT::default();
    unsafe {
        let _ = GetCursorPos(&mut p);
    }
    let scale = super::displays()
        .into_iter()
        .find(|d| {
            let px = p.x as f64 / d.scale;
            let py = p.y as f64 / d.scale;
            d.bounds.contains(px, py)
        })
        .map(|d| d.scale)
        .unwrap_or(1.0);
    (p.x as f64 / scale, p.y as f64 / scale)
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
pub struct NativeWindow(HWND);

impl NativeWindow {
    pub fn from_window(window: &impl HasWindowHandle) -> Option<Self> {
        let handle = window.window_handle().ok()?;
        let RawWindowHandle::Win32(win32) = handle.as_raw() else { return None };
        Some(Self(HWND(win32.hwnd.get() as *mut _)))
    }

    fn scale(&self) -> f64 {
        let dpi = unsafe { GetDpiForWindow(self.0) };
        if dpi == 0 { 1.0 } else { dpi as f64 / 96.0 }
    }

    pub fn configure_overlay(&self, _level_offset: isize, _resizable: bool) {
        unsafe {
            // Tool window: no taskbar button, no Alt-Tab entry.
            let ex = GetWindowLongPtrW(self.0, GWL_EXSTYLE);
            let ex = (ex | WS_EX_TOOLWINDOW.0 as isize) & !(WS_EX_APPWINDOW.0 as isize);
            SetWindowLongPtrW(self.0, GWL_EXSTYLE, ex);
            let _ = SetWindowPos(self.0, Some(HWND_TOPMOST), 0, 0, 0, 0, SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE);
        }
    }

    /// WDA_EXCLUDEFROMCAPTURE on Windows 10 2004+; older builds only take
    /// WDA_MONITOR, which shows a black rectangle instead.
    pub fn set_content_protection(&self, on: bool) {
        unsafe {
            if !on {
                let _ = SetWindowDisplayAffinity(self.0, WDA_NONE);
            } else if SetWindowDisplayAffinity(self.0, WDA_EXCLUDEFROMCAPTURE).is_err() {
                let _ = SetWindowDisplayAffinity(self.0, WDA_MONITOR);
            }
        }
    }

    pub fn set_ignore_mouse(&self, on: bool) {
        unsafe {
            let ex = GetWindowLongPtrW(self.0, GWL_EXSTYLE);
            let bits = (WS_EX_TRANSPARENT.0 | WS_EX_LAYERED.0) as isize;
            let ex = if on { ex | bits } else { ex & !(WS_EX_TRANSPARENT.0 as isize) };
            SetWindowLongPtrW(self.0, GWL_EXSTYLE, ex);
        }
    }

    /// Opacity is applied in the view instead (see the panel root), because a
    /// layered window fights GPUI's DirectComposition swap chain.
    pub fn set_opacity(&self, _value: f32) {}

    pub fn show(&self, focus: bool) {
        unsafe {
            if focus {
                let _ = ShowWindow(self.0, SW_SHOW);
                let _ = SetForegroundWindow(self.0);
            } else {
                let _ = ShowWindow(self.0, SW_SHOWNOACTIVATE);
            }
            let _ = SetWindowPos(self.0, Some(HWND_TOPMOST), 0, 0, 0, 0, SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE);
        }
    }

    pub fn hide(&self) {
        unsafe {
            let _ = ShowWindow(self.0, SW_HIDE);
        }
    }

    pub fn is_visible(&self) -> bool {
        unsafe { IsWindowVisible(self.0).as_bool() }
    }

    pub fn is_key(&self) -> bool {
        unsafe { GetForegroundWindow() == self.0 }
    }

    pub fn frame(&self) -> Rect {
        let mut r = RECT::default();
        unsafe {
            let _ = GetWindowRect(self.0, &mut r);
        }
        rect_to_points(r, self.scale())
    }

    pub fn set_frame(&self, rect: Rect) {
        let s = self.scale();
        unsafe {
            let _ = SetWindowPos(
                self.0,
                None,
                (rect.x * s).round() as i32,
                (rect.y * s).round() as i32,
                (rect.width * s).round() as i32,
                (rect.height * s).round() as i32,
                SWP_NOZORDER | SWP_NOACTIVATE,
            );
        }
    }

    pub fn begin_drag(&self) {
        unsafe {
            let _ = ReleaseCapture();
            SendMessageW(self.0, WM_NCLBUTTONDOWN, Some(WPARAM(HTCAPTION as usize)), Some(LPARAM(0)));
        }
    }

    pub fn make_borderless(&self) {}

    pub fn send_click(&self, _x: f64, _y: f64, _right: bool) {}

    pub fn send_scroll(&self, _x: f64, _y: f64, _lines: i32) {}

    pub fn window_number(&self) -> isize {
        self.0.0 as isize
    }
}
