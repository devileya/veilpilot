//! A pinned screenshot: a borderless picture that floats above every other
//! window. Drag it anywhere; scroll over it to resize; Esc or a double-click
//! closes it.

use gpui::{
    App, AppContext as _, Bounds, ClipboardItem, Context, FocusHandle, Focusable, ImageSource, KeyDownEvent,
    MouseButton, MouseDownEvent, ScrollWheelEvent, Window, WindowBackgroundAppearance, WindowBounds, WindowKind,
    WindowOptions, div, img, point, prelude::*, px, size,
};

use crate::platform::{self, NativeWindow, Rect};
use crate::snap::{self, Capture};
use crate::ui::theme;
use crate::ui::widgets::{icon_button, tooltip};

pub struct Pin {
    capture: Capture,
    image: std::sync::Arc<gpui::RenderImage>,
    native: Option<NativeWindow>,
    focus: FocusHandle,
    /// Points per image pixel.
    zoom: f32,
    hovered: bool,
}

/// Open a pinned copy of `capture`, centred on the pointer's display.
pub fn open(capture: Capture, protection: bool, cx: &mut App) {
    let (w, h) = (capture.image.width() as f64, capture.image.height() as f64);
    let (cursor_x, cursor_y) = platform::cursor_position();
    let display = platform::display_at(cursor_x, cursor_y).map(|d| d.work_area).unwrap_or(Rect {
        x: 0.0,
        y: 0.0,
        width: 1440.0,
        height: 900.0,
    });
    // At the size it had on screen, unless that would not fit.
    let natural = 1.0 / capture.scale as f64;
    let zoom = natural.min(display.width * 0.8 / w).min(display.height * 0.8 / h);
    let frame = Rect {
        x: display.x + (display.width - w * zoom) / 2.0,
        y: display.y + (display.height - h * zoom) / 2.0,
        width: w * zoom,
        height: h * zoom,
    };
    let options = WindowOptions {
        window_bounds: Some(WindowBounds::Windowed(Bounds::new(
            point(px(0.), px(0.)),
            size(px(frame.width as f32), px(frame.height as f32)),
        ))),
        titlebar: None,
        focus: false,
        show: false,
        kind: WindowKind::PopUp,
        is_movable: true,
        is_resizable: false,
        is_minimizable: false,
        window_background: WindowBackgroundAppearance::Transparent,
        ..Default::default()
    };
    let opened = cx.open_window(options, |window, cx| {
        let native = NativeWindow::from_window(window);
        if let Some(native) = native {
            native.set_content_protection(protection);
            cx.defer(move |_| {
                native.configure_overlay(1, false);
                native.make_borderless();
                native.set_frame(frame);
                native.show(false);
            });
        }
        cx.new(|cx| Pin {
            image: crate::capture::render_image(&capture.image),
            capture,
            native,
            focus: cx.focus_handle(),
            zoom: zoom as f32,
            hovered: false,
        })
    });
    if let Err(err) = opened {
        eprintln!("[snap] could not pin: {err:#}");
    }
}

impl Pin {
    fn resize(&mut self, factor: f32, cx: &mut Context<Self>) {
        let Some(native) = self.native else { return };
        let (w, h) = (self.capture.image.width() as f32, self.capture.image.height() as f32);
        let min = (80.0 / w).max(60.0 / h).min(1.0);
        let zoom = (self.zoom * factor).clamp(min, 4.0);
        if (zoom - self.zoom).abs() < 1e-4 {
            return;
        }
        self.zoom = zoom;
        let old = native.frame();
        // Grow and shrink about the middle.
        let (nw, nh) = ((w * zoom) as f64, (h * zoom) as f64);
        let frame =
            Rect { x: old.x + (old.width - nw) / 2.0, y: old.y + (old.height - nh) / 2.0, width: nw, height: nh };
        cx.defer(move |_| native.set_frame(frame));
        cx.notify();
    }

    fn copy(&mut self, cx: &mut Context<Self>) {
        match snap::png_image(&self.capture.image) {
            Ok(png) => cx.write_to_clipboard(ClipboardItem::new_image(&png)),
            Err(err) => eprintln!("[snap] could not copy the pinned image: {err:#}"),
        }
    }
}

impl Focusable for Pin {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

impl Render for Pin {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .id("pin")
            .track_focus(&self.focus)
            .size_full()
            .relative()
            .rounded(px(6.))
            .overflow_hidden()
            .border_1()
            .border_color(theme::alpha(theme::read(), 0.25))
            .on_hover(cx.listener(|this, hovered: &bool, _, cx| {
                this.hovered = *hovered;
                cx.notify();
            }))
            .on_key_down(cx.listener(|_, event: &KeyDownEvent, window, _| {
                if event.keystroke.key == "escape" {
                    window.remove_window();
                }
            }))
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, event: &MouseDownEvent, window, cx| {
                    if event.click_count >= 2 {
                        window.remove_window();
                        return;
                    }
                    window.focus(&this.focus);
                    if let Some(native) = this.native {
                        cx.defer(move |_| native.begin_drag());
                    }
                }),
            )
            .on_mouse_down(MouseButton::Right, cx.listener(|_, _, window, _| window.remove_window()))
            .on_scroll_wheel(cx.listener(|this, event: &ScrollWheelEvent, window, cx| {
                let dy = f32::from(event.delta.pixel_delta(window.line_height()).y);
                this.resize((dy * 0.004).exp().clamp(0.8, 1.25), cx);
            }))
            .child(img(ImageSource::Render(self.image.clone())).size_full())
            .when(self.hovered, |d| {
                d.child(
                    div()
                        .absolute()
                        .top(px(6.))
                        .right(px(6.))
                        .flex()
                        .gap(px(2.))
                        .p(px(2.))
                        .rounded(px(9.))
                        .bg(theme::alpha(theme::void(), 0.82))
                        .font_family(theme::ui_family())
                        .child(
                            icon_button("pin-copy", "copy", false, cx.listener(|this, _, _, cx| this.copy(cx)))
                                .tooltip(tooltip("Copy", None::<&str>)),
                        )
                        .child(
                            icon_button(
                                "pin-close",
                                "close",
                                false,
                                cx.listener(|_, _, window, _| window.remove_window()),
                            )
                            .tooltip(tooltip("Close", Some("Esc"))),
                        ),
                )
            })
    }
}
