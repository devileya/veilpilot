//! The capture overlay: a frozen frame of the display with a crosshair, a
//! pixel loupe and a size readout. Drag out an area, or — in window mode —
//! click the window under the pointer. The screenshot is the backdrop, so
//! the selection happens against a still image: nothing moves underneath.

use std::sync::Arc;

use futures::channel::oneshot;
use gpui::{
    AsyncWindowContext, Bounds, Context, Corners, FocusHandle, Focusable, KeyBinding, MouseButton, MouseDownEvent,
    MouseMoveEvent, MouseUpEvent, Pixels, Point, Window, WindowBackgroundAppearance, WindowBounds, WindowKind,
    WindowOptions, actions, canvas, div, fill, point, prelude::*, px, size,
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

/// What the picker is for.
#[derive(Clone, Debug, PartialEq)]
pub enum PickMode {
    /// Drag out a rectangle. The text says what happens with it.
    Area(&'static str),
    /// Click one of these windows (rectangles relative to the display, front
    /// to back); a drag still selects an area.
    Window(Vec<Rect>),
}

pub struct RegionPicker {
    image: Arc<gpui::RenderImage>,
    /// Pixels per point of the frame.
    scale: f32,
    mode: PickMode,
    origin: Option<Point<Pixels>>,
    current: Option<Point<Pixels>>,
    pointer: Option<Point<Pixels>>,
    done: Option<oneshot::Sender<RegionResult>>,
    focus_handle: FocusHandle,
    was_active: bool,
}

fn rect_from(a: Point<Pixels>, b: Point<Pixels>) -> Bounds<Pixels> {
    Bounds::from_corners(point(a.x.min(b.x), a.y.min(b.y)), point(a.x.max(b.x), a.y.max(b.y)))
}

fn to_rect(b: Bounds<Pixels>) -> Rect {
    Rect {
        x: f32::from(b.origin.x) as f64,
        y: f32::from(b.origin.y) as f64,
        width: f32::from(b.size.width) as f64,
        height: f32::from(b.size.height) as f64,
    }
}

fn to_bounds(r: Rect) -> Bounds<Pixels> {
    Bounds::new(point(px(r.x as f32), px(r.y as f32)), size(px(r.width as f32), px(r.height as f32)))
}

impl RegionPicker {
    /// Open the picker over the frame's display and wait for a selection.
    pub async fn pick(frame: Arc<Frame>, mode: PickMode, cx: &mut AsyncWindowContext) -> RegionResult {
        let (tx, rx) = oneshot::channel();
        let image = crate::capture::render_image(&frame.image);
        let display = frame.display.bounds;
        let scale = frame.image.width() as f32 / display.width.max(1.0) as f32;

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
                    RegionPicker {
                        image,
                        scale,
                        mode,
                        origin: None,
                        current: None,
                        pointer: None,
                        done: Some(tx),
                        focus_handle,
                        was_active: false,
                    }
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

    /// The window under the pointer, in window mode.
    fn window_at(&self, p: Point<Pixels>) -> Option<Rect> {
        let PickMode::Window(list) = &self.mode else { return None };
        let (x, y) = (f32::from(p.x) as f64, f32::from(p.y) as f64);
        list.iter().find(|r| r.contains(x, y)).copied()
    }

    fn dragging(&self) -> bool {
        self.origin.zip(self.current).is_some_and(|(a, b)| (a.x - b.x).abs() > px(3.) || (a.y - b.y).abs() > px(3.))
    }

    fn on_down(&mut self, event: &MouseDownEvent, _: &mut Window, cx: &mut Context<Self>) {
        self.origin = Some(event.position);
        self.current = Some(event.position);
        cx.notify();
    }

    fn on_move(&mut self, event: &MouseMoveEvent, _: &mut Window, cx: &mut Context<Self>) {
        self.pointer = Some(event.position);
        if self.origin.is_some() {
            self.current = Some(event.position);
        }
        cx.notify();
    }

    fn on_up(&mut self, event: &MouseUpEvent, window: &mut Window, cx: &mut Context<Self>) {
        let Some(origin) = self.origin.take() else { return };
        let r = rect_from(origin, event.position);
        let result = if r.size.width >= px(8.) && r.size.height >= px(8.) {
            RegionResult::Selected(to_rect(r))
        } else if let Some(hit) = self.window_at(event.position) {
            // A click (not a drag) in window mode takes the window.
            let viewport = window.viewport_size();
            let screen = Rect {
                x: 0.0,
                y: 0.0,
                width: f32::from(viewport.width) as f64,
                height: f32::from(viewport.height) as f64,
            };
            let x0 = hit.x.max(screen.x);
            let y0 = hit.y.max(screen.y);
            let x1 = (hit.x + hit.width).min(screen.width);
            let y1 = (hit.y + hit.height).min(screen.height);
            RegionResult::Selected(Rect { x: x0, y: y0, width: x1 - x0, height: y1 - y0 })
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

/// A frame of four 1.5 px lines.
fn outline(b: Bounds<Pixels>, color: gpui::Hsla, window: &mut Window) {
    let t = px(1.5);
    window.paint_quad(fill(Bounds::new(b.origin, size(b.size.width, t)), color));
    window.paint_quad(fill(Bounds::new(point(b.left(), b.bottom() - t), size(b.size.width, t)), color));
    window.paint_quad(fill(Bounds::new(b.origin, size(t, b.size.height)), color));
    window.paint_quad(fill(Bounds::new(point(b.right() - t, b.top()), size(t, b.size.height)), color));
}

impl Render for RegionPicker {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let viewport = window.viewport_size();
        if let Some(native) = NativeWindow::from_window(window) {
            let f = native.frame();
            FRAME.with(|c| c.set(Some((f.x, f.y, f.width, f32::from(viewport.height) as f64 * 1000.0 + f.height))));
        }
        let selection = self.origin.zip(self.current).filter(|_| self.dragging()).map(|(a, b)| rect_from(a, b));
        let hovered =
            if selection.is_none() { self.pointer.and_then(|p| self.window_at(p)).map(to_bounds) } else { None };
        let lit = selection.or(hovered);
        let pointer = self.pointer;
        let image = self.image.clone();
        let scale = self.scale;

        let scene = canvas(
            |_, _, _| {},
            move |bounds, _, window, _| {
                let _ = window.paint_image(bounds, Corners::default(), image.clone(), 0, false);
                let dim: gpui::Hsla = theme::dim_overlay().into();
                match lit {
                    None => window.paint_quad(fill(bounds, dim)),
                    Some(r) => {
                        // Dimming lives around the selection, so the selected
                        // area stays at full brightness.
                        let (l, t, rt, b) = (r.left(), r.top(), r.right(), r.bottom());
                        let (vw, vh) = (bounds.size.width, bounds.size.height);
                        for piece in [
                            Bounds::new(point(px(0.), px(0.)), size(vw, t)),
                            Bounds::new(point(px(0.), b), size(vw, (vh - b).max(px(0.)))),
                            Bounds::new(point(px(0.), t), size(l, b - t)),
                            Bounds::new(point(rt, t), size((vw - rt).max(px(0.)), b - t)),
                        ] {
                            window.paint_quad(fill(piece, dim));
                        }
                        outline(r, theme::lamp().into(), window);
                    }
                }
                // Crosshair and loupe while nothing is being dragged.
                if let (Some(p), None) = (pointer, selection) {
                    let line: gpui::Hsla = gpui::Hsla { a: 0.55, ..theme::white().into() };
                    window.paint_quad(fill(Bounds::new(point(px(0.), p.y), size(bounds.size.width, px(1.))), line));
                    window.paint_quad(fill(Bounds::new(point(p.x, px(0.)), size(px(1.), bounds.size.height)), line));

                    const LOUPE: f32 = 116.0;
                    const ZOOM: f32 = 8.0;
                    let mut at = point(p.x + px(22.), p.y + px(22.));
                    if at.x + px(LOUPE) > bounds.right() {
                        at.x = p.x - px(22. + LOUPE);
                    }
                    if at.y + px(LOUPE + 26.) > bounds.bottom() {
                        at.y = p.y - px(22. + LOUPE + 26.);
                    }
                    let loupe = Bounds::new(at, size(px(LOUPE), px(LOUPE)));
                    window.paint_quad(fill(loupe, theme::void()));
                    // The whole frame, scaled up so the pixel under the
                    // pointer lands in the middle, clipped to the loupe.
                    let factor = ZOOM / scale.max(0.5);
                    let shown = size(bounds.size.width * factor, bounds.size.height * factor);
                    let origin = point(
                        loupe.center().x - (p.x - bounds.origin.x) * factor,
                        loupe.center().y - (p.y - bounds.origin.y) * factor,
                    );
                    window.with_content_mask(Some(gpui::ContentMask { bounds: loupe }), |window| {
                        let _ =
                            window.paint_image(Bounds::new(origin, shown), Corners::default(), image.clone(), 0, false);
                    });
                    let pixel = px(ZOOM);
                    let c = loupe.center();
                    outline(
                        Bounds::new(point(c.x - pixel / 2., c.y - pixel / 2.), size(pixel, pixel)),
                        theme::lamp().into(),
                        window,
                    );
                    outline(loupe, gpui::Hsla { a: 0.9, ..theme::white().into() }, window);
                }
            },
        )
        .size_full();

        // The readout: size of the selection, or the pointer's position.
        let readout = match (selection, hovered, pointer) {
            (Some(r), _, _) => {
                Some((r, format!("{} × {}", f32::from(r.size.width).round(), f32::from(r.size.height).round())))
            }
            (None, Some(r), _) => Some((
                r,
                format!("Window · {} × {}", f32::from(r.size.width).round(), f32::from(r.size.height).round()),
            )),
            _ => None,
        };
        let label = readout.map(|(r, text)| {
            let below = r.bottom() + px(10.);
            let top = if below + px(28.) > viewport.height { (r.top() - px(32.)).max(px(6.)) } else { below };
            div()
                .absolute()
                .left(r.left().min(viewport.width - px(160.)).max(px(6.)))
                .top(top)
                .px(px(9.))
                .py(px(4.))
                .rounded(px(7.))
                .bg(theme::alpha(theme::void(), 0.92))
                .text_color(theme::read())
                .font_family(theme::mono_family())
                .text_size(px(11.5))
                .child(text)
        });
        let coords = match (pointer, selection) {
            (Some(p), None) => Some(
                div()
                    .absolute()
                    .left(p.x + px(22.))
                    .top(p.y + px(22. + 116. + 4.))
                    .when(p.x + px(160.) > viewport.width, |d| d.left(p.x - px(138.)))
                    .when(p.y + px(170.) > viewport.height, |d| d.top(p.y - px(48.)))
                    .px(px(7.))
                    .py(px(2.))
                    .rounded(px(5.))
                    .bg(theme::alpha(theme::void(), 0.92))
                    .font_family(theme::mono_family())
                    .text_size(px(10.5))
                    .text_color(theme::haze())
                    .child(format!("{}, {}", f32::from(p.x).round(), f32::from(p.y).round())),
            ),
            _ => None,
        };
        let help = match &self.mode {
            PickMode::Area(what) => *what,
            PickMode::Window(_) => "Click a window, or drag an area",
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
            .child(scene)
            .children(label)
            .children(coords)
            .when(selection.is_none(), |d| {
                d.child(
                    div().absolute().top(px(28.)).left_0().right_0().flex().justify_center().child(
                        div()
                            .flex()
                            .items_center()
                            .gap(px(8.))
                            .px(px(14.))
                            .py(px(8.))
                            .rounded(px(10.))
                            .bg(theme::alpha(theme::void(), 0.9))
                            .border_1()
                            .border_color(theme::hairline())
                            .text_size(px(12.))
                            .text_color(theme::read())
                            .child(help)
                            .child(div().text_color(theme::faint()).child("·"))
                            .child(crate::ui::widgets::key_badge("Esc"))
                            .child(div().text_color(theme::haze()).child("cancels")),
                    ),
                )
            })
    }
}
