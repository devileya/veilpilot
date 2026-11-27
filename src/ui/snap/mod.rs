//! The screenshot tool's windows: the editor and pinned pictures.

mod editor;
mod editor_view;
pub mod pin;

pub use editor::Editor;

use gpui::{
    App, AppContext as _, Bounds, TitlebarOptions, WeakEntity, WindowBounds, WindowHandle, WindowKind, WindowOptions,
    px, size,
};

use crate::platform::{self, NativeWindow};
use crate::snap::Capture;
use crate::snap::annot::Color;
use crate::ui::panel::Panel;

/// Room the editor's own chrome takes around the picture, in points.
const CHROME: (f32, f32) = (56.0, 92.0 + 56.0);
const MIN_SIZE: (f32, f32) = (860.0, 520.0);

/// Open the editor on `capture`.
pub fn open_editor(
    capture: Capture,
    panel: WeakEntity<Panel>,
    color: Color,
    protection: bool,
    cx: &mut App,
) -> Option<WindowHandle<Editor>> {
    // Big enough to show the picture at the size it had on screen, within
    // what the display has room for.
    let (cursor_x, cursor_y) = platform::cursor_position();
    let room = platform::display_at(cursor_x, cursor_y).map(|d| d.work_area);
    let (max_w, max_h) = room.map(|r| (r.width as f32 * 0.92, r.height as f32 * 0.9)).unwrap_or((1280.0, 800.0));
    let want_w = capture.image.width() as f32 / capture.scale + CHROME.0;
    let want_h = capture.image.height() as f32 / capture.scale + CHROME.1;
    let window_size =
        size(px(want_w.clamp(MIN_SIZE.0, max_w.max(MIN_SIZE.0))), px(want_h.clamp(MIN_SIZE.1, max_h.max(MIN_SIZE.1))));

    let options = WindowOptions {
        window_bounds: Some(WindowBounds::Windowed(Bounds::centered(None, window_size, cx))),
        titlebar: Some(TitlebarOptions { title: Some("VeilPilot — Screenshot".into()), ..Default::default() }),
        focus: true,
        show: true,
        kind: WindowKind::Normal,
        is_movable: true,
        is_resizable: true,
        is_minimizable: true,
        window_min_size: Some(size(px(MIN_SIZE.0), px(MIN_SIZE.1))),
        app_id: Some("co.interopera.veilpilot".into()),
        ..Default::default()
    };
    let opened = cx.open_window(options, |window, cx| {
        if let Some(native) = NativeWindow::from_window(window) {
            native.set_content_protection(protection);
        }
        cx.new(|cx| Editor::new(capture, panel, color, window, cx))
    });
    match opened {
        Ok(handle) => {
            // The app has no Dock icon, so nothing else brings it forward.
            platform::activate_app();
            let _ = handle.update(cx, |_, window, _| window.activate_window());
            Some(handle)
        }
        Err(err) => {
            eprintln!("[snap] could not open the editor: {err:#}");
            None
        }
    }
}
