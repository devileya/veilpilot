//! Native window and OS calls GPUI does not expose: capture protection,
//! window level, click-through, opacity, show-without-focus, and the screen /
//! microphone permission checks.
//!
//! All geometry here is in global "top-left" points: the origin is the top-left
//! corner of the primary display and y grows downward. That is the coordinate
//! space Electron saved window bounds in, and the one CoreGraphics and Win32
//! report cursor and display positions in.

#[cfg(target_os = "macos")]
mod macos;
#[cfg(target_os = "macos")]
pub use macos::*;

#[cfg(windows)]
mod windows;
#[cfg(windows)]
pub use self::windows::*;

#[cfg(not(any(target_os = "macos", windows)))]
mod other;
#[cfg(not(any(target_os = "macos", windows)))]
pub use other::*;

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Rect {
    pub x: f64,
    pub y: f64,
    pub width: f64,
    pub height: f64,
}

impl Rect {
    pub fn contains(&self, x: f64, y: f64) -> bool {
        x >= self.x && x < self.x + self.width && y >= self.y && y < self.y + self.height
    }
}

#[derive(Clone, Debug)]
pub struct DisplayInfo {
    /// Stable within a session; matches xcap's monitor id.
    pub id: String,
    pub bounds: Rect,
    /// Bounds minus the menu bar / dock / taskbar.
    pub work_area: Rect,
    /// Points to pixels; Windows needs it to convert cursor positions.
    #[cfg_attr(not(windows), allow(dead_code))]
    pub scale: f64,
    pub primary: bool,
    /// The camera housing cut into the top edge (MacBooks since 2021), in
    /// the same global coordinates; `None` on displays without one.
    pub notch: Option<Rect>,
}

/// Another app's window, in the same global top-left points as displays.
#[derive(Clone, Debug, PartialEq)]
pub struct WindowInfo {
    /// The id screen capture knows it by.
    pub id: u32,
    pub pid: u32,
    pub app: String,
    pub title: String,
    pub bounds: Rect,
}

/// Windows as xcap lists them, for the platforms without a native listing.
/// xcap reports physical pixels there, so positions are scaled back to the
/// points everything else here uses.
#[cfg(not(target_os = "macos"))]
pub fn windows() -> Vec<WindowInfo> {
    let mut list: Vec<(i32, WindowInfo)> = xcap::Window::all()
        .unwrap_or_default()
        .into_iter()
        .filter(|w| !w.is_minimized().unwrap_or(false))
        .filter_map(|w| {
            let scale = w.current_monitor().ok().and_then(|m| m.scale_factor().ok()).unwrap_or(1.0) as f64;
            let scale = if cfg!(windows) { scale.max(1.0) } else { 1.0 };
            let bounds = Rect {
                x: w.x().ok()? as f64 / scale,
                y: w.y().ok()? as f64 / scale,
                width: w.width().ok()? as f64 / scale,
                height: w.height().ok()? as f64 / scale,
            };
            if bounds.width < 40.0 || bounds.height < 40.0 {
                return None;
            }
            let info = WindowInfo {
                id: w.id().ok()?,
                pid: w.pid().unwrap_or(0),
                app: w.app_name().unwrap_or_default(),
                title: w.title().unwrap_or_default(),
                bounds,
            };
            Some((w.z().unwrap_or(0), info))
        })
        .collect();
    // Front to back.
    list.sort_by(|a, b| b.0.cmp(&a.0));
    list.into_iter().map(|(_, info)| info).collect()
}

#[cfg(not(target_os = "macos"))]
pub fn frontmost_pid() -> Option<u32> {
    xcap::Window::all().ok()?.into_iter().find(|w| w.is_focused().unwrap_or(false))?.pid().ok()
}

/// Sending input to other apps is a macOS-only path here; elsewhere a
/// scrolling capture follows the user's own scrolling.
#[cfg(not(target_os = "macos"))]
pub fn can_post_events() -> bool {
    false
}

#[cfg(not(target_os = "macos"))]
pub fn request_post_events() -> bool {
    false
}

#[cfg(not(target_os = "macos"))]
pub fn move_cursor(_x: f64, _y: f64) {}

#[cfg(not(target_os = "macos"))]
pub fn post_scroll(_x: f64, _y: f64, _pixels: i32) {}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Permission {
    Granted,
    Denied,
    Restricted,
    NotDetermined,
}

impl Permission {
    pub fn as_str(self) -> &'static str {
        match self {
            Permission::Granted => "granted",
            Permission::Denied => "denied",
            Permission::Restricted => "restricted",
            Permission::NotDetermined => "not-determined",
        }
    }
}

pub fn primary_display() -> Option<DisplayInfo> {
    let all = displays();
    all.iter().find(|d| d.primary).cloned().or_else(|| all.into_iter().next())
}

pub fn display_at(x: f64, y: f64) -> Option<DisplayInfo> {
    let all = displays();
    if let Some(hit) = all.iter().find(|d| d.bounds.contains(x, y)) {
        return Some(hit.clone());
    }
    // Nearest by center distance when the point is in a gap between screens.
    all.into_iter().min_by(|a, b| {
        let da = dist2(&a.bounds, x, y);
        let db = dist2(&b.bounds, x, y);
        da.total_cmp(&db)
    })
}

fn dist2(r: &Rect, x: f64, y: f64) -> f64 {
    let cx = r.x + r.width / 2.0;
    let cy = r.y + r.height / 2.0;
    (cx - x).powi(2) + (cy - y).powi(2)
}

/// Open a URL or a settings pane with the OS default handler.
pub fn open_external(target: &str) {
    #[cfg(target_os = "macos")]
    let result = std::process::Command::new("open").arg(target).spawn();
    #[cfg(windows)]
    let result = std::process::Command::new("cmd").args(["/C", "start", "", target]).spawn();
    #[cfg(not(any(target_os = "macos", windows)))]
    let result = std::process::Command::new("xdg-open").arg(target).spawn();
    if let Err(err) = result {
        eprintln!("[platform] could not open {target}: {err}");
    }
}
