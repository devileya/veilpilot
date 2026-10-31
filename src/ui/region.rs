//! Drag-to-select over a frozen frame of the display. The screenshot is the
//! backdrop, so the selection happens against a still image — nothing moves
//! underneath while you drag.

use std::sync::Arc;

use futures::channel::oneshot;
use gpui::{
    AsyncWindowContext, Bounds, Context, FocusHandle, Focusable, ImageSource, KeyBinding, MouseButton, MouseDownEvent,
    MouseMoveEvent, MouseUpEvent, Pixels, Point, Window, WindowBackgroundAppearance, WindowBounds, WindowKind,
    WindowOptions, actions, div, img, point, prelude::*, px, size,
};

use super::theme;
use crate::capture::Frame;
use crate::platform::{NativeWindow, Rect};

actions!(region, [CancelRegion]);

pub fn bind_keys(cx: &mut gpui::App) {
    cx.bind_keys([KeyBinding::new("escape", CancelRegion, Some("RegionPicker"))]);
}

thread_local! {
    /// The picker on screen, if any (there is only ever one).
    static OPEN: std::cell::RefCell<Option<gpui::WindowHandle<RegionPicker>>> = const { std::cell::RefCell::new(None) };
}

pub fn is_open() -> bool {
    OPEN.with(|o| o.borrow().is_some())
}

thread_local! {
    static FRAME: std::cell::Cell<Option<(f64, f64, f64, f64)>> = const { std::cell::Cell::new(None) };
}

/// The picker's frame as last seen (dev diagnostics).
pub fn native_frame() -> Option<(f64, f64, f64, f64)> {
    FRAME.with(|f| f.get())
}

/// Send a keystroke to the open picker (dev control channel).
pub fn dispatch_keystroke(keystroke: gpui::Keystroke, cx: &mut gpui::App) -> Option<bool> {
    let handle: gpui::AnyWindowHandle = OPEN.with(|o| *o.borrow())?.into();
    // Through the untyped handle so the picker view is not leased while the
    // keystroke redraws it.
    cx.update_window(handle, |_, window, cx| window.dispatch_keystroke(keystroke, cx)).ok()
}

/// The open picker's native window, for dev snapshots.
pub fn native(cx: &mut gpui::App) -> Option<NativeWindow> {
    let handle = OPEN.with(|o| *o.borrow())?;
    handle.update(cx, |_, window, _| NativeWindow::from_window(window)).ok().flatten()
}

/// Finish the open picker from outside (the dev control channel).
pub fn complete(result: RegionResult, cx: &mut gpui::App) -> bool {
    let Some(handle) = OPEN.with(|o| *o.borrow()) else { return false };
    handle.update(cx, |picker, window, cx| picker.finish(result, window, cx)).is_ok()
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum RegionResult {
    Selected(Rect),
    Cancelled,
}

pub struct RegionPicker {
    image: Arc<gpui::RenderImage>,
    origin: Option<Point<Pixels>>,
    current: Option<Point<Pixels>>,
    done: Option<oneshot::Sender<RegionResult>>,
    focus_handle: FocusHandle,
    was_active: bool,
}

fn rect_from(a: Point<Pixels>, b: Point<Pixels>) -> Bounds<Pixels> {
    Bounds::from_corners(point(a.x.min(b.x), a.y.min(b.y)), point(a.x.max(b.x), a.y.max(b.y)))
}

impl RegionPicker {
    /// Open the picker over the frame's display and wait for a selection.
    pub async fn pick(frame: Arc<Frame>, cx: &mut AsyncWindowContext) -> RegionResult {
        let (tx, rx) = oneshot::channel();
        let image = crate::capture::render_image(&frame.image);
        let display = frame.display.bounds;

        let opened = cx.update(|_, cx| {
            let options = WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(Bounds::new(
                    point(px(0.), px(0.)),
                    size(px(display.width as f32), px(display.height as f32)),
                ))),
                titlebar: None,
                focus: true,
                show: true,
                kind: WindowKind::PopUp,
                is_movable: false,
                is_resizable: false,
                is_minimizable: false,
                window_background: WindowBackgroundAppearance::Opaque,
                ..Default::default()
            };
            cx.open_window(options, |window, cx| {
                if let Some(native) = NativeWindow::from_window(window) {
                    // Out of every screen share from the first frame.
                    native.set_content_protection(true);
                    // Above the panel, on this display — once GPUI has
                    // finished building the window.
                    cx.defer(move |_| {
                        native.configure_overlay(2, false);
                        native.make_borderless();
                        native.set_frame(display);
                        native.show(true);
                    });
                }
                cx.new(|cx| {
                    let focus_handle = cx.focus_handle();
                    window.focus(&focus_handle);
                    cx.observe_window_activation(window, |this: &mut RegionPicker, window, cx| {
                        // A click elsewhere (or Cmd-Tab) abandons the selection.
                        if window.is_window_active() {
                            this.was_active = true;
                        } else if this.was_active {
                            this.finish(RegionResult::Cancelled, window, cx);
                        }
                    })
                    .detach();
                    RegionPicker { image, origin: None, current: None, done: Some(tx), focus_handle, was_active: false }
                })
            })
        });

        match opened {
            Ok(Ok(handle)) => {
                OPEN.with(|o| *o.borrow_mut() = Some(handle));
                let result = rx.await.unwrap_or(RegionResult::Cancelled);
                OPEN.with(|o| *o.borrow_mut() = None);
                result
            }
            Ok(Err(err)) => {
                eprintln!("[region] could not open the picker: {err:#}");
                RegionResult::Cancelled
            }
            Err(_) => RegionResult::Cancelled,
        }
    }

    pub fn finish(&mut self, result: RegionResult, window: &mut Window, _: &mut Context<Self>) {
        if let Some(tx) = self.done.take() {
            let _ = tx.send(result);
            window.remove_window();
        }
    }

    fn on_down(&mut self, event: &MouseDownEvent, _: &mut Window, cx: &mut Context<Self>) {
        self.origin = Some(event.position);
        self.current = Some(event.position);
        cx.notify();
    }

    fn on_move(&mut self, event: &MouseMoveEvent, _: &mut Window, cx: &mut Context<Self>) {
        if self.origin.is_some() {
            self.current = Some(event.position);
            cx.notify();
        }
    }

    fn on_up(&mut self, event: &MouseUpEvent, window: &mut Window, cx: &mut Context<Self>) {
        let Some(origin) = self.origin.take() else { return };
        let r = rect_from(origin, event.position);
        let result = if r.size.width >= px(8.) && r.size.height >= px(8.) {
            RegionResult::Selected(Rect {
                x: f32::from(r.origin.x) as f64,
                y: f32::from(r.origin.y) as f64,
                width: f32::from(r.size.width) as f64,
                height: f32::from(r.size.height) as f64,
            })
        } else {
            RegionResult::Cancelled
        };
        self.finish(result, window, cx);
    }

    fn cancel(&mut self, _: &CancelRegion, window: &mut Window, cx: &mut Context<Self>) {
        self.finish(RegionResult::Cancelled, window, cx);
    }
}

impl Focusable for RegionPicker {
    fn focus_handle(&self, _: &gpui::App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Render for RegionPicker {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let viewport = window.viewport_size();
        if let Some(native) = NativeWindow::from_window(window) {
            let f = native.frame();
            FRAME.with(|c| c.set(Some((f.x, f.y, f.width, f32::from(viewport.height) as f64 * 1000.0 + f.height))));
        }
        let selection = self.origin.zip(self.current).map(|(a, b)| rect_from(a, b));
        let dim = theme::dim_overlay();

        let shade = |x: Pixels, y: Pixels, w: Pixels, h: Pixels| {
            div().absolute().left(x).top(y).w(w.max(px(0.))).h(h.max(px(0.))).bg(dim)
        };

        let overlay = match selection {
            None => div().absolute().top_0().left_0().size_full().bg(dim).into_any_element(),
            Some(r) => {
                // Dimming lives around the selection, so the selected area stays
                // at full brightness.
                let (l, t, rgt, btm) = (r.left(), r.top(), r.right(), r.bottom());
                let tick = |d: gpui::Div| d.absolute().size(px(9.)).border_color(theme::lamp());
                let below = btm + px(10.);
                let label_top = if below + px(24.) > viewport.height { (t - px(26.)).max(px(4.)) } else { below };
                let label_left = l.min(viewport.width - px(90.));
                div()
                    .absolute()
                    .top_0()
                    .left_0()
                    .size_full()
                    .child(shade(px(0.), px(0.), viewport.width, t))
                    .child(shade(px(0.), btm, viewport.width, viewport.height - btm))
                    .child(shade(px(0.), t, l, btm - t))
                    .child(shade(rgt, t, viewport.width - rgt, btm - t))
                    .child(
                        div()
                            .absolute()
                            .left(l)
                            .top(t)
                            .w(r.size.width)
                            .h(r.size.height)
                            .border_1()
                            .border_color(theme::lamp())
                            .child(tick(div()).top(px(-1.)).left(px(-1.)).border_t_1().border_l_1())
                            .child(tick(div()).bottom(px(-1.)).right(px(-1.)).border_b_1().border_r_1()),
                    )
                    .child(
                        div()
                            .absolute()
                            .left(label_left)
                            .top(label_top)
                            .px(px(7.))
                            .py(px(3.))
                            .bg(theme::void())
                            .text_color(theme::lamp())
                            .border_1()
                            .border_color(theme::lamp_border())
                            .font_family(theme::mono_family())
                            .text_size(px(11.))
                            .child(format!(
                                "{} × {}",
                                f32::from(r.size.width).round(),
                                f32::from(r.size.height).round()
                            )),
                    )
                    .into_any_element()
            }
        };

        div()
            .id("region")
            .key_context("RegionPicker")
            .track_focus(&self.focus_handle)
            .on_action(cx.listener(Self::cancel))
            .size_full()
            .relative()
            .cursor(gpui::CursorStyle::Crosshair)
            .bg(theme::void())
            .font_family(theme::ui_family())
            .on_mouse_down(MouseButton::Left, cx.listener(Self::on_down))
            .on_mouse_move(cx.listener(Self::on_move))
            .on_mouse_up(MouseButton::Left, cx.listener(Self::on_up))
            // A right-click is the conventional cancel for a screenshot crosshair.
            .on_mouse_down(
                MouseButton::Right,
                cx.listener(|this, _, window, cx| this.finish(RegionResult::Cancelled, window, cx)),
            )
            .child(img(ImageSource::Render(self.image.clone())).absolute().top_0().left_0().size_full())
            .child(overlay)
            .when(selection.is_none(), |d| {
                d.child(
                    div().absolute().top(px(28.)).left_0().right_0().flex().justify_center().child(
                        div()
                            .px(px(16.))
                            .py(px(9.))
                            .bg(gpui::rgba(0x15131ce6))
                            .border_1()
                            .border_color(gpui::rgba(0xf2ede424))
                            .text_size(px(10.5))
                            .text_color(theme::read())
                            .flex()
                            .gap(px(6.))
                            .child("DRAG TO SELECT  ·")
                            .child(div().text_color(theme::lamp()).child("ESC"))
                            .child("CANCELS"),
                    ),
                )
            })
    }
}
