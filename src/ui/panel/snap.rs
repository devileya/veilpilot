//! The screenshot hotkeys: capture a screen, an area, a window or a
//! scrolling page, then copy, save, edit or read it — the way a dedicated
//! screenshot app does, from the same panel that asks the model.

use std::sync::Arc;
use std::time::Duration;

use gpui::{AppContext as _, ClipboardItem, Context, Window};
use image::RgbaImage;

use super::{Panel, ToastKind};
use crate::capture::{self, Frame};
use crate::config::{SavedArea, SnapConfig};
use crate::platform::{self, Rect};
use crate::snap::stitch::{Step, Stitcher};
use crate::snap::{self, Capture, Mode, annot, ocr};
use crate::ui::region::{PickMode, RegionPicker, RegionResult};
use crate::ui::snap::Editor;

/// A scrolling capture in progress.
pub struct Scrolling {
    stop: Arc<std::sync::atomic::AtomicBool>,
}

#[derive(Default)]
pub struct SnapState {
    pub editor: Option<gpui::WindowHandle<Editor>>,
    pub last: Option<Capture>,
    pub busy: bool,
    pub scrolling: Option<Scrolling>,
}

impl Panel {
    /// Change the screenshot settings and save them.
    pub fn set_snap(&mut self, change: impl FnOnce(&mut SnapConfig)) {
        let before = self.cfg.snap.clone();
        change(&mut self.cfg.snap);
        if before != self.cfg.snap {
            self.save_config();
        }
    }

    pub fn snap_editor_open(&self, cx: &gpui::App) -> bool {
        self.snap.editor.is_some_and(|h| h.read(cx).is_ok())
    }

    pub fn snap(&mut self, mode: Mode, window: &mut Window, cx: &mut Context<Self>) {
        if mode == Mode::Scrolling && self.snap.scrolling.is_some() {
            self.finish_scrolling(cx);
            return;
        }
        if self.snap.busy || crate::ui::region::is_open() {
            return;
        }
        let mode = if mode == Mode::RepeatArea && self.cfg.snap.last_area.is_none() { Mode::Area } else { mode };
        self.snap.busy = true;
        self.capturing += 1;
        let repeat = self.cfg.snap.last_area.clone();
        let must_hide = self.is_visible() && !self.cfg.window.protection;

        cx.spawn_in(window, async move |this, cx| {
            if must_hide {
                let _ = this.update(cx, |this, cx| this.hide(cx));
                cx.background_executor().timer(Duration::from_millis(120)).await;
            }
            let result = grab(mode, repeat, cx).await;
            let _ = this.update_in(cx, |this, window, cx| {
                this.snap.busy = false;
                this.capturing = this.capturing.saturating_sub(1);
                if must_hide {
                    this.show(false, window, cx);
                }
                match result {
                    Ok(Grabbed::Nothing) => {}
                    Ok(Grabbed::Image { capture, area }) => {
                        if let Some(area) = area {
                            this.set_snap(|s| s.last_area = Some(area));
                        }
                        if mode == Mode::Ocr {
                            this.read_text(capture, cx);
                        } else {
                            this.deliver(capture, window, cx);
                        }
                    }
                    Ok(Grabbed::Scroll { frame, area }) => this.start_scrolling(frame, area, window, cx),
                    Err(err) => this.snap_failed(err, window, cx),
                }
            });
        })
        .detach();
    }

    /// A failed capture is never silent: the panel comes forward with the
    /// reason, and a missing permission opens the settings that fix it.
    fn snap_failed(&mut self, err: anyhow::Error, window: &mut Window, cx: &mut Context<Self>) {
        self.set_minimized(false, cx);
        self.show(true, window, cx);
        if capture::is_permission_error(&err) {
            self.open_settings(Some(crate::ui::settings::SettingsTab::Shots), window, cx);
        }
        self.toast(ToastKind::Error, format!("{err:#}"), cx);
    }

    /// Do with a new capture what the settings say: copy it, save it, and
    /// open it in the editor.
    pub fn deliver(&mut self, capture: Capture, window: &mut Window, cx: &mut Context<Self>) {
        let cfg = self.cfg.snap.clone();
        self.snap.last = Some(capture.clone());
        if cfg.copy_after {
            match snap::png_image(&capture.image) {
                Ok(png) => cx.write_to_clipboard(ClipboardItem::new_image(&png)),
                Err(err) => self.toast(ToastKind::Error, format!("Could not copy: {err:#}"), cx),
            }
        }
        if cfg.save_after {
            let (image, folder, format) = (capture.image.clone(), cfg.folder_path(), cfg.format.clone());
            cx.spawn(async move |this, cx| {
                let saved = cx.background_spawn(async move { snap::save_to_folder(&image, &folder, &format) }).await;
                let _ = this.update(cx, |this, cx| match saved {
                    Ok(path) => this.toast(ToastKind::Info, format!("Saved {}", path.display()), cx),
                    Err(err) => this.toast(ToastKind::Error, format!("Could not save: {err:#}"), cx),
                });
            })
            .detach();
        }
        if cfg.show_editor {
            self.open_editor(capture, window, cx);
        } else if cfg.copy_after && !cfg.save_after {
            self.toast(ToastKind::Info, format!("{} screenshot copied.", capture.label), cx);
        }
    }

    pub fn open_editor(&mut self, capture: Capture, _window: &mut Window, cx: &mut Context<Self>) {
        let weak = cx.entity().downgrade();
        let color = annot::parse_hex(&self.cfg.snap.color).unwrap_or([255, 59, 48, 255]);
        let protection = self.cfg.window.protection;
        let existing = self.snap.editor;
        cx.defer(move |cx| {
            // One editor: a new capture replaces what it shows.
            if let Some(handle) = existing {
                let reused = handle
                    .update(cx, |editor, window, cx| {
                        editor.load(capture.clone(), cx);
                        window.activate_window();
                    })
                    .is_ok();
                if reused {
                    platform::activate_app();
                    return;
                }
            }
            let handle = crate::ui::snap::open_editor(capture, weak.clone(), color, protection, cx);
            let _ = weak.update(cx, |this, _| this.snap.editor = handle);
        });
    }

    /// The "show editor" key: the last capture, or the clipboard's image.
    pub fn show_editor(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(handle) = self.snap.editor.filter(|h| h.read(cx).is_ok()) {
            cx.defer(move |cx| {
                let _ = handle.update(cx, |_, window, _| window.activate_window());
                platform::activate_app();
            });
            return;
        }
        if let Some(capture) = self.snap.last.clone() {
            self.open_editor(capture, window, cx);
            return;
        }
        let clipboard = cx.read_from_clipboard().and_then(|item| {
            item.entries().iter().find_map(|e| match e {
                gpui::ClipboardEntry::Image(image) => image::load_from_memory(image.bytes()).ok(),
                _ => None,
            })
        });
        match clipboard {
            Some(image) => self.open_editor(Capture::new(image.to_rgba8(), 1.0, "Clipboard"), window, cx),
            None => {
                let key = crate::hotkeys::pretty(&self.cfg.hotkeys.snap_area);
                self.toast(ToastKind::Info, format!("No screenshot yet — take one with {key}."), cx);
            }
        }
    }

    /// Recognize text in a capture and copy it.
    fn read_text(&mut self, capture: Capture, cx: &mut Context<Self>) {
        let join = self.cfg.snap.ocr_join_lines;
        self.capturing += 1;
        cx.spawn(async move |this, cx| {
            let image = capture.image.clone();
            let read = cx.background_spawn(async move { ocr::recognize(&image) }).await;
            let _ = this.update(cx, |this, cx| {
                this.capturing = this.capturing.saturating_sub(1);
                match read {
                    Ok(found) if found.is_empty() => this.toast(ToastKind::Warn, "No text or QR code found there.", cx),
                    Ok(found) => {
                        let text = found.text(join);
                        let preview: String = text.chars().take(90).collect();
                        let more = if text.chars().count() > 90 { "…" } else { "" };
                        cx.write_to_clipboard(ClipboardItem::new_string(text));
                        let what = if found.codes.is_empty() { "Copied text" } else { "Copied the code" };
                        this.toast(ToastKind::Info, format!("{what}: {preview}{more}"), cx);
                    }
                    Err(err) => this.toast(ToastKind::Error, format!("{err:#}"), cx),
                }
            });
        })
        .detach();
    }

    /// Attach an image (from the editor) to the next ask.
    pub fn attach_image(&mut self, image: RgbaImage, window: &mut Window, cx: &mut Context<Self>) {
        let max_dimension = self.cfg.capture.max_dimension;
        let keep_last = self.cfg.capture.keep_last;
        cx.spawn_in(window, async move |this, cx| {
            let shot = cx.background_spawn(async move { capture::finish(image, max_dimension) }).await;
            let _ = this.update_in(cx, |this, window, cx| match shot {
                Ok(shot) => {
                    let shot = this.session.add_shot(shot, keep_last);
                    this.attachments.push(shot);
                    if this.attachments.len() > 4 {
                        let drop = this.attachments.len() - 4;
                        this.attachments.drain(..drop);
                    }
                    this.focus_input(window, cx);
                }
                Err(err) => this.toast(ToastKind::Error, format!("{err:#}"), cx),
            });
        })
        .detach();
    }

    /* ---------- scrolling ---------- */

    fn start_scrolling(&mut self, frame: Arc<Frame>, area: Rect, window: &mut Window, cx: &mut Context<Self>) {
        let Some(first) = capture::crop(&frame, area) else { return };
        let stop = Arc::new(std::sync::atomic::AtomicBool::new(false));
        self.snap.scrolling = Some(Scrolling { stop: stop.clone() });
        let auto = platform::can_post_events();
        if !auto {
            platform::request_post_events();
        }
        let key = crate::hotkeys::pretty(&self.cfg.hotkeys.snap_scrolling);
        self.toast(
            ToastKind::Info,
            if auto {
                format!("Scrolling capture: scrolling for you. Press {key} to finish early.")
            } else {
                format!("Scrolling capture: scroll the page slowly, then press {key} to finish.")
            },
            cx,
        );
        let display = frame.display.clone();
        let scale = first.width() as f32 / area.width.max(1.0) as f32;
        let max = self.cfg.snap.scroll_max_height;
        // The middle of the area, in global coordinates, is where the wheel
        // events go.
        let (mx, my) = (display.bounds.x + area.x + area.width / 2.0, display.bounds.y + area.y + area.height / 2.0);
        let step = ((area.height * 0.45).round() as i32).max(40);
        let (cursor_x, cursor_y) = platform::cursor_position();

        cx.spawn_in(window, async move |this, cx| {
            let mut stitcher = Stitcher::new(first, max);
            let mut still = 0u32;
            let mut lost = 0u32;
            if auto {
                platform::move_cursor(mx, my);
            }
            loop {
                if stop.load(std::sync::atomic::Ordering::Relaxed) {
                    break;
                }
                if auto {
                    platform::post_scroll(mx, my, step);
                }
                cx.background_executor().timer(Duration::from_millis(if auto { 260 } else { 180 })).await;
                let display = display.clone();
                let grabbed = cx.background_spawn(async move { capture::grab(&display) }).await;
                let Ok(frame) = grabbed else { break };
                let Some(next) = capture::crop(&frame, area) else { break };
                match stitcher.push(next) {
                    Step::Added(_) => {
                        still = 0;
                        lost = 0;
                    }
                    Step::Still => {
                        still += 1;
                        // Scrolling for you: the end of the page. By hand:
                        // a long pause.
                        if (auto && still >= 3) || still >= 40 {
                            break;
                        }
                    }
                    Step::Lost => {
                        lost += 1;
                        if auto && lost >= 4 {
                            break;
                        }
                    }
                    Step::Full => break,
                }
            }
            if auto {
                platform::move_cursor(cursor_x, cursor_y);
            }
            let image = cx.background_spawn(async move { stitcher.finish() }).await;
            let _ = this.update_in(cx, |this, window, cx| {
                this.snap.scrolling = None;
                this.deliver(Capture::new(image, scale, "Scrolling"), window, cx);
            });
        })
        .detach();
    }

    fn finish_scrolling(&mut self, cx: &mut Context<Self>) {
        if let Some(scrolling) = &self.snap.scrolling {
            scrolling.stop.store(true, std::sync::atomic::Ordering::Relaxed);
            self.toast(ToastKind::Info, "Finishing the scrolling capture…", cx);
        }
    }
}

enum Grabbed {
    Nothing,
    Image { capture: Capture, area: Option<SavedArea> },
    Scroll { frame: Arc<Frame>, area: Rect },
}

/// The display a capture starts from: the one under the pointer.
fn display_under_pointer() -> anyhow::Result<platform::DisplayInfo> {
    capture::pick_display("cursor").ok_or_else(|| anyhow::anyhow!("No display found"))
}

async fn grab(mode: Mode, repeat: Option<SavedArea>, cx: &mut gpui::AsyncWindowContext) -> anyhow::Result<Grabbed> {
    let scale_of = |frame: &Frame| frame.image.width() as f32 / frame.display.bounds.width.max(1.0) as f32;
    match mode {
        Mode::Fullscreen => {
            let display = display_under_pointer()?;
            let frame = cx.background_spawn(async move { capture::grab(&display) }).await?;
            let scale = scale_of(&frame);
            Ok(Grabbed::Image { capture: Capture::new(frame.image, scale, "Fullscreen"), area: None })
        }
        Mode::Window => {
            let target = snap::active_window().ok_or_else(|| anyhow::anyhow!("There is no window to capture."))?;
            let width = target.bounds.width;
            let image = cx.background_spawn(async move { snap::grab_window(&target) }).await?;
            let scale = image.width() as f32 / width.max(1.0) as f32;
            Ok(Grabbed::Image { capture: Capture::new(image, scale, "Window"), area: None })
        }
        Mode::RepeatArea => {
            let area = repeat.ok_or_else(|| anyhow::anyhow!("No area to repeat yet."))?;
            let display = platform::displays()
                .into_iter()
                .find(|d| d.id == area.display)
                .or_else(|| capture::pick_display("cursor"))
                .ok_or_else(|| anyhow::anyhow!("No display found"))?;
            let frame = cx.background_spawn(async move { capture::grab(&display) }).await?;
            let rect = Rect { x: area.x, y: area.y, width: area.width, height: area.height };
            let image = capture::crop(&frame, rect).ok_or_else(|| anyhow::anyhow!("The saved area is off screen."))?;
            Ok(Grabbed::Image { capture: Capture::new(image, scale_of(&frame), "Area"), area: None })
        }
        Mode::Area | Mode::AnyWindow | Mode::Scrolling | Mode::Ocr => {
            let display = display_under_pointer()?;
            let frame = Arc::new(cx.background_spawn(async move { capture::grab(&display) }).await?);
            let pick = match mode {
                Mode::AnyWindow => {
                    let origin = frame.display.bounds;
                    let windows = snap::candidate_windows()
                        .into_iter()
                        .map(|w| Rect {
                            x: w.bounds.x - origin.x,
                            y: w.bounds.y - origin.y,
                            width: w.bounds.width,
                            height: w.bounds.height,
                        })
                        .collect();
                    PickMode::Window(windows)
                }
                Mode::Scrolling => PickMode::Area("Drag over the part of the page that scrolls"),
                Mode::Ocr => PickMode::Area("Drag over the text or QR code to copy"),
                _ => PickMode::Area("Drag to capture an area"),
            };
            let RegionResult::Selected(rect) = RegionPicker::pick(frame.clone(), pick, cx).await else {
                return Ok(Grabbed::Nothing);
            };
            if mode == Mode::Scrolling {
                return Ok(Grabbed::Scroll { frame, area: rect });
            }
            let image = capture::crop(&frame, rect).ok_or_else(|| anyhow::anyhow!("That area is too small."))?;
            let area = (mode == Mode::Area).then(|| SavedArea {
                display: frame.display.id.clone(),
                x: rect.x,
                y: rect.y,
                width: rect.width,
                height: rect.height,
            });
            Ok(Grabbed::Image { capture: Capture::new(image, scale_of(&frame), mode.label()), area })
        }
    }
}
