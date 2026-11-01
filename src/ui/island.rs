//! Notch mode: a black island that hugs the MacBook notch. The wings show
//! state (recording, auto-answer, name mode, unseen answers); new answers
//! drop down from it and fold back after a while. Hover keeps it open, a
//! click opens the full chat. Displays without a notch (and Windows) get the
//! same island as a pill under the menu bar.

use std::time::Duration;

use gpui::{
    AnyWindowHandle, Context, Entity, MouseButton, SharedString, Subscription, Task, WeakEntity, Window,
    WindowBackgroundAppearance, WindowBounds, WindowKind, WindowOptions, div, point, prelude::*, px, rgb, size,
};

use super::markdown;
use super::panel::{Latest, Panel};
use super::theme;
use super::widgets::caps;
use crate::hotkeys::pretty;
use crate::platform::{self, NativeWindow, Rect};

/// Width of each wing beside the notch.
const WING: f64 = 132.0;
const EXPANDED_WIDTH: f64 = 480.0;
const PILL_HEIGHT: f64 = 30.0;
/// How long a finished answer stays open before folding back.
const LINGER: Duration = Duration::from_secs(12);

#[derive(Clone, Copy, Debug)]
struct Geometry {
    display: Rect,
    /// The notch, if this display has one.
    notch: Option<Rect>,
    /// Top of the usable area (below the menu bar), for displays without one.
    work_top: f64,
}

fn geometry() -> Geometry {
    let displays = platform::displays();
    let pick = displays.iter().find(|d| d.notch.is_some()).or_else(|| displays.iter().find(|d| d.primary));
    match pick {
        Some(d) => Geometry { display: d.bounds, notch: d.notch, work_top: d.work_area.y },
        None => {
            Geometry { display: Rect { x: 0.0, y: 0.0, width: 1440.0, height: 900.0 }, notch: None, work_top: 25.0 }
        }
    }
}

impl Geometry {
    fn top_height(&self) -> f64 {
        self.notch.map(|n| n.height).unwrap_or(PILL_HEIGHT)
    }

    fn center_x(&self) -> f64 {
        self.notch.map(|n| n.x + n.width / 2.0).unwrap_or(self.display.x + self.display.width / 2.0)
    }

    fn collapsed(&self) -> Rect {
        let width = self.notch.map(|n| n.width + 2.0 * WING).unwrap_or(2.0 * WING + 40.0);
        let y = if self.notch.is_some() { self.display.y } else { self.work_top + 6.0 };
        Rect { x: self.center_x() - width / 2.0, y, width, height: self.top_height() }
    }

    fn expanded(&self, content_height: f64) -> Rect {
        let base = self.collapsed();
        let width = base.width.max(EXPANDED_WIDTH);
        let height = (base.height + content_height).min(self.display.height * 0.6);
        Rect { x: self.center_x() - width / 2.0, y: base.y, width, height }
    }
}

/// What the island shows, read from the panel.
#[derive(Clone, Debug, PartialEq)]
struct Snapshot {
    recording: bool,
    transcribing: bool,
    protection: bool,
    auto: bool,
    name: bool,
    unseen: usize,
    item: Option<Item>,
    open_key: String,
}

#[derive(Clone, Debug, PartialEq)]
struct Item {
    key: Latest,
    eyebrow: String,
    question: String,
    answer: String,
    done: bool,
    error: Option<String>,
}

fn snapshot(panel: &Panel) -> Snapshot {
    let item = panel.latest.and_then(|key| match key {
        Latest::Card(id) => panel.cards.iter().find(|c| c.id == id).map(|c| Item {
            key,
            eyebrow: c.label.clone(),
            question: c.prompt.clone(),
            answer: c.text.clone(),
            done: c.done,
            error: c.error.clone(),
        }),
        Latest::Live(id) => panel.live.items.iter().find(|i| i.id == id && i.visible()).map(|i| Item {
            key,
            eyebrow: format!("Live · {}", i.trigger.label()),
            question: i.line.clone(),
            answer: i.answer.clone(),
            done: i.done,
            error: i.error.clone(),
        }),
    });
    Snapshot {
        recording: panel.is_recording(),
        transcribing: panel.uploading > 0,
        protection: panel.cfg.window.protection,
        auto: panel.cfg.live.auto_answer,
        name: panel.cfg.live.answer_on_name,
        unseen: panel.live.unseen,
        item,
        open_key: panel.cfg.hotkeys.send.clone(),
    }
}

/// A rough height for the dropped-down answer, so the window fits it
/// without measuring text (the content scrolls if it is off).
fn content_height(item: &Item) -> f64 {
    let lines = |text: &str, per_line: usize| -> f64 {
        text.lines().map(|l| (l.chars().count().max(1) as f64 / per_line as f64).ceil()).sum::<f64>().max(1.0)
    };
    let question = if item.question.is_empty() { 0.0 } else { lines(&item.question, 72).min(2.0) * 16.0 + 4.0 };
    let body = item.error.as_deref().unwrap_or(&item.answer);
    let answer = lines(body, 58).min(16.0) * 20.0;
    14.0 + 16.0 + question + answer + 30.0
}

pub struct Island {
    panel: WeakEntity<Panel>,
    panel_window: AnyWindowHandle,
    native: Option<NativeWindow>,
    geometry: Geometry,
    expanded: bool,
    hovered: bool,
    /// The newest answer seen, and whether it had finished.
    seen: Option<(Latest, usize, bool)>,
    frame: Option<Rect>,
    collapse: Option<Task<()>>,
    _observe: Subscription,
}

impl Island {
    pub fn open(
        panel: Entity<Panel>,
        panel_window: AnyWindowHandle,
        protection: bool,
        cx: &mut gpui::App,
    ) -> Option<gpui::WindowHandle<Island>> {
        let geo = geometry();
        let frame = geo.collapsed();
        let options = WindowOptions {
            window_bounds: Some(WindowBounds::Windowed(gpui::Bounds::new(
                point(px(0.), px(0.)),
                size(px(frame.width as f32), px(frame.height as f32)),
            ))),
            titlebar: None,
            focus: false,
            show: false,
            kind: WindowKind::PopUp,
            is_movable: false,
            is_resizable: false,
            is_minimizable: false,
            window_background: WindowBackgroundAppearance::Transparent,
            ..Default::default()
        };
        cx.open_window(options, |window, cx| {
            let native = NativeWindow::from_window(window);
            if let Some(native) = native {
                native.set_content_protection(protection);
                cx.defer(move |_| {
                    // Above the panel and the menu bar, on every Space.
                    native.configure_overlay(2, false);
                    native.make_borderless();
                    // Again after the style change, before it is ever on screen.
                    native.set_content_protection(protection);
                    native.set_frame(frame);
                    native.show(false);
                });
            }
            // What is already there is old news: only newer answers drop down.
            let seen = snapshot(panel.read(cx)).item.map(|i| (i.key, i.answer.len(), i.done));
            cx.new(|cx| Island {
                _observe: cx.observe(&panel, |this: &mut Island, panel, cx| this.on_panel_change(panel, cx)),
                panel: panel.downgrade(),
                panel_window,
                native,
                geometry: geo,
                expanded: false,
                hovered: false,
                seen,
                frame: Some(frame),
                collapse: None,
            })
        })
        .map_err(|err| eprintln!("[island] could not open: {err:#}"))
        .ok()
    }

    pub fn native(&self) -> Option<NativeWindow> {
        self.native
    }

    fn on_panel_change(&mut self, panel: Entity<Panel>, cx: &mut Context<Self>) {
        let snap = snapshot(panel.read(cx));
        if let Some(item) = &snap.item {
            let grew = match self.seen {
                Some((key, len, _)) => key != item.key || item.answer.len() > len,
                None => true,
            };
            let just_finished = item.done && self.seen.is_some_and(|(key, _, done)| key == item.key && !done);
            if grew && !item.done {
                self.expanded = true;
                self.collapse = None;
            }
            if grew && item.done && self.seen.is_none_or(|(key, _, _)| key != item.key) {
                // A finished answer that arrived in one go (an error card).
                self.expanded = true;
                self.schedule_collapse(LINGER, cx);
            }
            if just_finished {
                self.schedule_collapse(LINGER, cx);
            }
            self.seen = Some((item.key, item.answer.len(), item.done));
        }
        self.relayout(&snap, cx);
        cx.notify();
    }

    fn schedule_collapse(&mut self, after: Duration, cx: &mut Context<Self>) {
        self.collapse = Some(cx.spawn(async move |this, cx| {
            cx.background_executor().timer(after).await;
            let _ = this.update(cx, |this, cx| {
                if this.hovered && this.cursor_inside() {
                    return;
                }
                this.hovered = false;
                this.expanded = false;
                if let Some(panel) = this.panel.upgrade() {
                    let snap = snapshot(panel.read(cx));
                    this.relayout(&snap, cx);
                }
                cx.notify();
            });
        }));
    }

    fn relayout(&mut self, snap: &Snapshot, cx: &mut Context<Self>) {
        let frame = match (&snap.item, self.expanded) {
            (Some(item), true) => self.geometry.expanded(content_height(item)),
            (None, true) => self.geometry.expanded(60.0),
            _ => self.geometry.collapsed(),
        };
        if self.frame == Some(frame) {
            return;
        }
        self.frame = Some(frame);
        if let Some(native) = self.native {
            // After this update: AppKit answers setFrame: with callbacks.
            cx.defer(move |_| native.set_frame(frame));
        }
    }

    fn cursor_inside(&self) -> bool {
        let (x, y) = platform::cursor_position();
        self.frame.is_some_and(|f| f.contains(x, y))
    }

    fn open_panel(&mut self, cx: &mut Context<Self>) {
        let (panel, window) = (self.panel.clone(), self.panel_window);
        cx.defer(move |cx| {
            let _ = window.update(cx, |_, window, cx| {
                if let Some(panel) = panel.upgrade() {
                    panel.update(cx, |p, cx| p.focus_input(window, cx));
                }
            });
        });
    }
}

impl Render for Island {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let Some(panel) = self.panel.upgrade() else { return div().into_any_element() };
        let snap = snapshot(panel.read(cx));
        let geo = self.geometry;
        let top = px(geo.top_height() as f32);
        let notch_width = px(geo.notch.map(|n| n.width).unwrap_or(16.0) as f32);
        let has_notch = geo.notch.is_some();

        let dot = |color: gpui::Rgba| div().flex_none().size(px(6.)).rounded_full().bg(color);
        let (state, state_color) = if snap.recording {
            ("REC", theme::live())
        } else if snap.transcribing {
            ("…", theme::lamp())
        } else {
            ("VEILPILOT", theme::haze())
        };
        let left = div()
            .flex_1()
            .h_full()
            .flex()
            .items_center()
            .justify_end()
            .gap(px(6.))
            .pr(px(14.))
            .child(dot(state_color))
            .child(
                div().text_size(px(10.)).font_weight(gpui::FontWeight::SEMIBOLD).text_color(state_color).child(state),
            );
        let right = div()
            .flex_1()
            .h_full()
            .flex()
            .items_center()
            .gap(px(8.))
            .pl(px(14.))
            .when(snap.auto, |d| d.child(div().text_size(px(11.)).text_color(theme::lamp()).child("⚡")))
            .when(snap.name, |d| d.child(div().text_size(px(11.)).text_color(theme::lamp()).child("@")))
            .when(snap.unseen > 0, |d| {
                d.child(
                    div()
                        .px(px(6.))
                        .rounded(px(8.))
                        .bg(theme::lamp())
                        .text_color(rgb(0x000000))
                        .text_size(px(9.5))
                        .font_weight(gpui::FontWeight::BOLD)
                        .child(format!("{} new", snap.unseen)),
                )
            })
            .child(dot(if snap.protection { theme::lamp() } else { theme::live() }));

        let body = (self.expanded).then(|| {
            let content = match &snap.item {
                None => div()
                    .text_size(px(12.))
                    .text_color(theme::haze())
                    .child("No answers yet. Press a prompt key, or turn on ⚡ auto-answer.")
                    .into_any_element(),
                Some(item) => {
                    let answer: Vec<gpui::AnyElement> = match &item.error {
                        Some(err) => vec![div().text_color(theme::live()).child(err.clone()).into_any_element()],
                        None if item.answer.is_empty() => {
                            vec![div().text_color(theme::haze()).child("Thinking…").into_any_element()]
                        }
                        None => markdown::render(
                            &item.answer,
                            markdown::RenderOptions { id: "island".into(), caret: !item.done, caret_visible: true },
                        ),
                    };
                    div()
                        .flex()
                        .flex_col()
                        .gap(px(6.))
                        .child(caps(&item.eyebrow).text_color(theme::lamp()))
                        .when(!item.question.is_empty(), |d| {
                            d.child(
                                div()
                                    .text_size(px(11.5))
                                    .line_height(px(16.))
                                    .text_color(theme::haze())
                                    .font(theme::italic(theme::ui_font()))
                                    .child(SharedString::from(item.question.chars().take(180).collect::<String>())),
                            )
                        })
                        .child(
                            div()
                                .flex()
                                .flex_col()
                                .gap(px(6.))
                                .text_size(px(13.5))
                                .line_height(px(20.))
                                .text_color(theme::read())
                                .children(answer),
                        )
                        .into_any_element()
                }
            };
            div()
                .id("island-body")
                .flex_1()
                .min_h_0()
                .overflow_y_scroll()
                .px(px(18.))
                .pt(px(10.))
                .pb(px(8.))
                .child(content)
                .child(
                    div()
                        .mt(px(8.))
                        .text_size(px(10.))
                        .text_color(theme::edge())
                        .child(format!("{} open chat · hover to keep open", pretty(&snap.open_key))),
                )
        });

        div()
            .id("island")
            .size_full()
            .flex()
            .flex_col()
            .bg(rgb(0x000000))
            .when(has_notch, |d| d.rounded_b(px(18.)))
            .when(!has_notch, |d| d.rounded(px(15.)).border_1().border_color(theme::edge()))
            .overflow_hidden()
            .font_family(theme::ui_family())
            .text_color(theme::read())
            .cursor_pointer()
            .on_hover(cx.listener(|this, hovered: &bool, _, cx| {
                // A window that has not seen the mouse yet places it at its
                // top-left corner, inside the island: ask where it really is.
                this.hovered = *hovered && this.cursor_inside();
                if this.hovered {
                    this.expanded = true;
                    this.collapse = None;
                } else {
                    this.schedule_collapse(Duration::from_millis(1200), cx);
                }
                if let Some(panel) = this.panel.upgrade() {
                    let snap = snapshot(panel.read(cx));
                    this.relayout(&snap, cx);
                }
                cx.notify();
            }))
            .on_mouse_down(MouseButton::Left, cx.listener(|this, _, _, cx| this.open_panel(cx)))
            .child(div().flex_none().h(top).flex().child(left).child(div().flex_none().w(notch_width)).child(right))
            .children(body)
            .into_any_element()
    }
}

/* ---------- the panel's side ---------- */

impl Panel {
    pub fn island_visible(&self) -> bool {
        self.island_native().is_some_and(|n| n.is_visible())
    }

    pub fn island_native(&self) -> Option<NativeWindow> {
        self.island_native_cache
    }

    /// Show or hide the island, opening it first if needed.
    pub fn show_island(&mut self, visible: bool, cx: &mut Context<Self>) {
        if visible && self.island.is_none() {
            if self.island_opening {
                return;
            }
            // Opening a window renders it, and the island reads the panel:
            // that has to wait until this update of the panel is over.
            self.island_opening = true;
            let panel = cx.entity();
            let panel_window = self.window_handle;
            cx.defer(move |cx| {
                // Read now, not when scheduled: protection may have changed.
                let protection = panel.read(cx).cfg.window.protection;
                let handle = Island::open(panel.clone(), panel_window, protection, cx);
                let native = handle.and_then(|h| h.update(cx, |island, _, _| island.native()).ok().flatten());
                panel.update(cx, |this, cx| {
                    this.island_opening = false;
                    this.island = handle;
                    this.island_native_cache = native;
                    // Notch mode was turned off while it opened.
                    if !this.cfg.window.notch() {
                        this.close_island(cx);
                    }
                    cx.notify();
                });
            });
            return;
        }
        if let Some(native) = self.island_native_cache {
            cx.defer(move |_| if visible { native.show(false) } else { native.hide() });
        }
        cx.notify();
    }

    pub(super) fn close_island(&mut self, cx: &mut Context<Self>) {
        if let Some(handle) = self.island.take() {
            self.island_native_cache = None;
            cx.defer(move |cx| {
                let _ = handle.update(cx, |_, window, _| window.remove_window());
            });
        }
    }

    pub fn toggle_notch(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let notch = !self.cfg.window.notch();
        self.cfg.window.mode = if notch { "notch".into() } else { "panel".into() };
        self.save_config();
        if notch {
            self.hide(cx);
            self.show_island(true, cx);
            self.toast(super::panel::ToastKind::Info, "Notch mode — answers drop down from the top of the screen.", cx);
        } else {
            self.close_island(cx);
            self.show(true, window, cx);
            self.toast(super::panel::ToastKind::Info, "Panel mode.", cx);
        }
        cx.notify();
    }
}
