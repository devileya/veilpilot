//! The screenshot editor: a document of annotations over a base image, the
//! tools that make and reshape them, undo, and what leaves the window —
//! clipboard, file, a pinned copy, recognized text, or an ask to the model.
//!
//! This file is state and input. Drawing is in `editor_view`.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use gpui::{
    AppContext as _, Bounds, ClipboardItem, Context, FocusHandle, KeyDownEvent, Modifiers, MouseButton, MouseDownEvent,
    MouseMoveEvent, MouseUpEvent, Pixels, Point, ScrollWheelEvent, Task, WeakEntity, Window, point, px,
};
use image::RgbaImage;

use crate::capture;
use crate::config::SnapConfig;
use crate::platform::NativeWindow;
use crate::snap::annot::{Annot, ArrowStyle, Color, FillMode, Handle, Kind, Pt, RectF, format_color, to_hex};
use crate::snap::raster::{self, Backdrop};
use crate::snap::{self, Capture, ocr, text};
use crate::ui::panel::Panel;

/// Rows per GPU texture: a scrolling capture is far taller than a texture
/// may be, so the base image is drawn as a stack of strips.
const STRIP_ROWS: u32 = 4096;
/// Room left around the image when it is fitted to the window, in points.
pub(super) const FIT_MARGIN: f32 = 28.0;
const MAX_UNDO: usize = 60;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Tool {
    /// Pick, move and reshape annotations; drag on the image to select an
    /// area.
    Select,
    Make(Kind),
    Ruler,
    Picker,
}

/// (tool, icon, name, key) in toolbar order; `None` is a divider.
pub(super) const TOOLS: &[Option<(Tool, &str, &str, &str)>] = &[
    Some((Tool::Select, "select", "Select, move, crop", "V")),
    None,
    Some((Tool::Make(Kind::Arrow), "arrow", "Arrow", "A")),
    Some((Tool::Make(Kind::Line), "line", "Line", "L")),
    Some((Tool::Make(Kind::Rect), "rect", "Rectangle", "R")),
    Some((Tool::Make(Kind::Oval), "oval", "Oval", "O")),
    Some((Tool::Make(Kind::Draw), "pen", "Freehand", "D")),
    Some((Tool::Make(Kind::Text), "text", "Text", "T")),
    Some((Tool::Make(Kind::Counter), "counter", "Numbered step", "N")),
    None,
    Some((Tool::Make(Kind::Highlight), "highlight", "Highlighter", "H")),
    Some((Tool::Make(Kind::Blur), "blur", "Blur", "B")),
    Some((Tool::Make(Kind::Pixelate), "pixelate", "Pixelate", "X")),
    Some((Tool::Make(Kind::Erase), "erase", "Erase (fill with the background)", "E")),
    Some((Tool::Make(Kind::Spotlight), "spotlight", "Spotlight", "S")),
    Some((Tool::Make(Kind::Magnify), "magnify", "Magnifier", "M")),
    None,
    Some((Tool::Ruler, "ruler", "Ruler: measure the space under the pointer", "U")),
    Some((Tool::Picker, "pipette", "Colour picker (Tab copies from any tool)", "I")),
];

pub(super) const SWATCHES: [Color; 8] = [
    [0xFF, 0x3B, 0x30, 255],
    [0xFF, 0x95, 0x00, 255],
    [0xFF, 0xD6, 0x0A, 255],
    [0x34, 0xC7, 0x59, 255],
    [0x0A, 0x84, 0xFF, 255],
    [0xBF, 0x5A, 0xF2, 255],
    [0x1C, 0x1C, 0x1E, 255],
    [0xFF, 0xFF, 0xFF, 255],
];

/// Stroke sizes, in points: (name, value).
pub(super) const SIZES: [(&str, f32); 3] = [("S", 2.0), ("M", 4.0), ("L", 7.0)];

#[derive(Clone)]
pub(super) struct Doc {
    pub base: Arc<RgbaImage>,
    pub annots: Vec<Annot>,
    pub next_counter: u32,
}

enum Drag {
    Create(u64),
    Move { id: u64, last: Pt, moved: bool },
    Reshape { id: u64, handle: Handle },
    Marquee { start: Pt },
    Pan { last: Point<Pixels> },
}

pub(super) struct CachedTile {
    key: Annot,
    generation: u64,
    pub image: Arc<gpui::RenderImage>,
    pub rect: RectF,
}

/// Where the image sits in the window.
#[derive(Clone, Copy, Debug)]
pub(super) struct View {
    /// Window position of the image's top-left pixel.
    pub origin: Point<Pixels>,
    /// Points per image pixel.
    pub zoom: f32,
}

impl View {
    pub fn to_screen(&self, p: Pt) -> Point<Pixels> {
        point(self.origin.x + px(p.x * self.zoom), self.origin.y + px(p.y * self.zoom))
    }
    pub fn to_image(&self, p: Point<Pixels>) -> Pt {
        Pt::new(f32::from(p.x - self.origin.x) / self.zoom, f32::from(p.y - self.origin.y) / self.zoom)
    }
    pub fn rect(&self, r: RectF) -> Bounds<Pixels> {
        Bounds::new(self.to_screen(Pt::new(r.x, r.y)), gpui::size(px(r.w * self.zoom), px(r.h * self.zoom)))
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Popup {
    Backdrop,
    Resize,
    Color,
}

pub struct Editor {
    pub(super) panel: WeakEntity<Panel>,
    pub(super) focus: FocusHandle,
    native: Option<NativeWindow>,

    pub(super) doc: Doc,
    /// Pixels per point of the screen the capture came from.
    pub(super) scale: f32,
    undo: Vec<Doc>,
    redo: Vec<Doc>,
    /// Bumps whenever the base image changes.
    pub(super) generation: u64,
    next_id: u64,

    pub(super) strips: Vec<(Arc<gpui::RenderImage>, u32, u32)>,
    strips_generation: u64,
    pub(super) tiles: HashMap<u64, CachedTile>,
    /// GPU images no longer shown, released on the next frame.
    stale: Vec<Arc<gpui::RenderImage>>,

    pub(super) fit: bool,
    zoom: f32,
    pan: (f32, f32),
    pub(super) canvas: Bounds<Pixels>,
    pub(super) scale_factor: f32,

    pub(super) tool: Tool,
    pub(super) color: Color,
    /// Stroke size in points.
    pub(super) size: f32,
    pub(super) fill: FillMode,
    pub(super) arrow: ArrowStyle,
    drag: Option<Drag>,
    pub(super) selected: Option<u64>,
    pub(super) editing: Option<u64>,
    pub(super) marquee: Option<RectF>,
    pub(super) hover: Option<Pt>,
    pub(super) popup: Option<Popup>,
    pub(super) backdrop: Backdrop,
    pub(super) note: Option<(u64, String, bool)>,
    note_seq: u64,
    pub(super) caret_on: bool,
    pub(super) busy: bool,
    _blink: Task<()>,
}

impl Editor {
    pub fn new(
        capture: Capture,
        panel: WeakEntity<Panel>,
        color: Color,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let focus = cx.focus_handle();
        window.focus(&focus);
        let blink = cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor().timer(Duration::from_millis(530)).await;
                let alive = this.update(cx, |this: &mut Editor, cx| {
                    this.caret_on = !this.caret_on;
                    if this.editing.is_some() {
                        cx.notify();
                    }
                });
                if alive.is_err() {
                    break;
                }
            }
        });
        Self {
            panel,
            focus,
            native: NativeWindow::from_window(window),
            doc: Doc { base: capture.image.clone(), annots: Vec::new(), next_counter: 1 },
            scale: capture.scale,
            undo: Vec::new(),
            redo: Vec::new(),
            generation: 1,
            next_id: 1,
            strips: Vec::new(),
            strips_generation: 0,
            tiles: HashMap::new(),
            stale: Vec::new(),
            fit: true,
            zoom: 1.0,
            pan: (0.0, 0.0),
            canvas: Bounds::default(),
            scale_factor: window.scale_factor(),
            tool: Tool::Make(Kind::Arrow),
            color,
            size: 4.0,
            fill: FillMode::Outline,
            arrow: ArrowStyle::Shape,
            drag: None,
            selected: None,
            editing: None,
            marquee: None,
            hover: None,
            popup: None,
            backdrop: Backdrop {
                padding: 0.06,
                radius: 0.02,
                shadow: true,
                from: raster::BACKDROPS[0].1,
                to: raster::BACKDROPS[0].2,
            },
            note: None,
            note_seq: 0,
            caret_on: true,
            busy: false,
            _blink: blink,
        }
    }

    pub fn native(&self) -> Option<NativeWindow> {
        self.native
    }

    /// Replace the document with a new capture.
    pub fn load(&mut self, capture: Capture, cx: &mut Context<Self>) {
        self.commit_text();
        self.doc = Doc { base: capture.image.clone(), annots: Vec::new(), next_counter: 1 };
        self.scale = capture.scale;
        self.undo.clear();
        self.redo.clear();
        self.generation += 1;
        self.selected = None;
        self.marquee = None;
        self.drag = None;
        self.popup = None;
        self.fit = true;
        cx.notify();
    }

    fn snap_config(&self, cx: &gpui::App) -> SnapConfig {
        self.panel.upgrade().map(|p| p.read(cx).cfg.snap.clone()).unwrap_or_default()
    }

    /* ---------- notes ---------- */

    pub(super) fn say(&mut self, text: impl Into<String>, error: bool, cx: &mut Context<Self>) {
        self.note_seq += 1;
        let id = self.note_seq;
        self.note = Some((id, text.into(), error));
        cx.spawn(async move |this, cx| {
            cx.background_executor().timer(Duration::from_millis(if error { 6000 } else { 2600 })).await;
            let _ = this.update(cx, |this, cx| {
                if this.note.as_ref().is_some_and(|n| n.0 == id) {
                    this.note = None;
                    cx.notify();
                }
            });
        })
        .detach();
        cx.notify();
    }

    /* ---------- the view ---------- */

    pub(super) fn image_size(&self) -> (f32, f32) {
        (self.doc.base.width() as f32, self.doc.base.height() as f32)
    }

    /// One image pixel on one screen pixel.
    fn actual_size_zoom(&self) -> f32 {
        1.0 / self.scale_factor.max(0.5)
    }

    fn fit_zoom(&self) -> f32 {
        let (w, h) = self.image_size();
        let cw = (f32::from(self.canvas.size.width) - 2.0 * FIT_MARGIN).max(40.0);
        let ch = (f32::from(self.canvas.size.height) - 2.0 * FIT_MARGIN).max(40.0);
        (cw / w.max(1.0)).min(ch / h.max(1.0)).min(self.actual_size_zoom())
    }

    pub(super) fn zoom(&self) -> f32 {
        if self.fit { self.fit_zoom() } else { self.zoom }
    }

    /// Zoom as the status bar shows it: 100 is one image pixel per screen
    /// pixel.
    pub(super) fn zoom_percent(&self) -> f32 {
        self.zoom() * self.scale_factor * 100.0
    }

    pub(super) fn view(&self) -> View {
        let zoom = self.zoom();
        let (w, h) = self.image_size();
        let (cw, ch) = (f32::from(self.canvas.size.width), f32::from(self.canvas.size.height));
        let (sw, sh) = (w * zoom, h * zoom);
        // Centred while it fits; otherwise panned, never past its own edges.
        let x = if sw <= cw { (cw - sw) / 2.0 } else { self.pan.0.clamp(cw - sw - FIT_MARGIN, FIT_MARGIN) };
        let y = if sh <= ch { (ch - sh) / 2.0 } else { self.pan.1.clamp(ch - sh - FIT_MARGIN, FIT_MARGIN) };
        View { origin: point(self.canvas.origin.x + px(x), self.canvas.origin.y + px(y)), zoom }
    }

    /// Zoom to `zoom`, keeping the image point under `anchor` where it is.
    fn set_zoom(&mut self, zoom: f32, anchor: Option<Point<Pixels>>, cx: &mut Context<Self>) {
        let before = self.view();
        let anchor = anchor.unwrap_or_else(|| self.canvas.center());
        let at = before.to_image(anchor);
        let min = self.fit_zoom().min(0.05);
        self.zoom = zoom.clamp(min, 16.0 * self.actual_size_zoom());
        self.fit = false;
        self.pan = (
            f32::from(anchor.x - self.canvas.origin.x) - at.x * self.zoom,
            f32::from(anchor.y - self.canvas.origin.y) - at.y * self.zoom,
        );
        cx.notify();
    }

    pub(super) fn zoom_by(&mut self, factor: f32, cx: &mut Context<Self>) {
        self.set_zoom(self.zoom() * factor, None, cx);
    }

    pub(super) fn zoom_fit(&mut self, cx: &mut Context<Self>) {
        self.fit = true;
        cx.notify();
    }

    pub(super) fn zoom_actual(&mut self, cx: &mut Context<Self>) {
        self.set_zoom(self.actual_size_zoom(), None, cx);
    }

    /* ---------- GPU images ---------- */

    /// Bring the textures in line with the document. Called before a frame.
    pub(super) fn sync(&mut self, window: &mut Window) {
        self.scale_factor = window.scale_factor();
        for image in self.stale.drain(..) {
            let _ = window.drop_image(image);
        }
        if self.strips_generation != self.generation {
            self.stale.extend(self.strips.drain(..).map(|s| s.0));
            let base = &self.doc.base;
            let mut y = 0;
            while y < base.height() {
                let h = STRIP_ROWS.min(base.height() - y);
                let strip = if y == 0 && h == base.height() {
                    capture::render_image(base)
                } else {
                    capture::render_image(&image::imageops::crop_imm(base.as_ref(), 0, y, base.width(), h).to_image())
                };
                self.strips.push((strip, y, h));
                y += h;
            }
            self.strips_generation = self.generation;
        }
        let live: std::collections::HashSet<u64> = self.doc.annots.iter().map(|a| a.id).collect();
        let gone: Vec<u64> = self.tiles.keys().filter(|id| !live.contains(id)).copied().collect();
        for id in gone {
            if let Some(tile) = self.tiles.remove(&id) {
                self.stale.push(tile.image);
            }
        }
        for annot in self.doc.annots.iter().filter(|a| a.kind.is_tile()) {
            let fresh = self
                .tiles
                .get(&annot.id)
                .is_some_and(|t| t.key == *annot && (!annot.kind.is_effect() || t.generation == self.generation));
            if fresh {
                continue;
            }
            if let Some(old) = self.tiles.remove(&annot.id) {
                self.stale.push(old.image);
            }
            if let Some(tile) = raster::tile(annot, &self.doc.base) {
                let rect =
                    RectF::new(tile.x as f32, tile.y as f32, tile.image.width() as f32, tile.image.height() as f32);
                self.tiles.insert(
                    annot.id,
                    CachedTile {
                        key: annot.clone(),
                        generation: self.generation,
                        image: capture::render_image(&tile.image),
                        rect,
                    },
                );
            }
        }
    }

    /* ---------- the document ---------- */

    fn checkpoint(&mut self) {
        self.undo.push(self.doc.clone());
        if self.undo.len() > MAX_UNDO {
            self.undo.remove(0);
        }
        self.redo.clear();
    }

    fn restore(&mut self, doc: Doc) {
        if !Arc::ptr_eq(&doc.base, &self.doc.base) {
            self.generation += 1;
            self.fit = true;
        }
        self.doc = doc;
        self.selected = None;
        self.editing = None;
        self.marquee = None;
        self.drag = None;
    }

    pub(super) fn can_undo(&self) -> bool {
        !self.undo.is_empty()
    }

    pub(super) fn can_redo(&self) -> bool {
        !self.redo.is_empty()
    }

    pub(super) fn undo(&mut self, cx: &mut Context<Self>) {
        self.commit_text();
        if let Some(doc) = self.undo.pop() {
            self.redo.push(self.doc.clone());
            self.restore(doc);
            cx.notify();
        }
    }

    pub(super) fn redo(&mut self, cx: &mut Context<Self>) {
        if let Some(doc) = self.redo.pop() {
            self.undo.push(self.doc.clone());
            self.restore(doc);
            cx.notify();
        }
    }

    fn annot(&self, id: u64) -> Option<&Annot> {
        self.doc.annots.iter().find(|a| a.id == id)
    }

    fn annot_mut(&mut self, id: u64) -> Option<&mut Annot> {
        self.doc.annots.iter_mut().find(|a| a.id == id)
    }

    pub(super) fn text_size_of(annot: &Annot) -> Option<(f32, f32)> {
        (annot.kind == Kind::Text)
            .then(|| text::measure(if annot.text.is_empty() { " " } else { &annot.text }, annot.width))
            .flatten()
    }

    /// The annotation under `p`, topmost first: shapes and labels are above
    /// the effects.
    fn hit(&self, p: Pt) -> Option<u64> {
        let tol = 5.0 / self.zoom().max(0.01);
        let test = |a: &&Annot| a.hit(p, tol, Self::text_size_of(a));
        self.doc
            .annots
            .iter()
            .rev()
            .filter(|a| !a.kind.is_effect() && a.kind != Kind::Spotlight)
            .find(test)
            .or_else(|| {
                self.doc.annots.iter().rev().filter(|a| a.kind.is_effect() || a.kind == Kind::Spotlight).find(test)
            })
            .map(|a| a.id)
    }

    /// Stroke width, font size or radius for a new annotation, in image
    /// pixels.
    fn width_for(&self, kind: Kind) -> f32 {
        match kind {
            Kind::Text => (10.0 + self.size * 4.0) * self.scale,
            _ => self.size * self.scale,
        }
    }

    fn selected_kind(&self) -> Option<Kind> {
        self.selected.and_then(|id| self.annot(id)).map(|a| a.kind)
    }

    /// The kind whose style the toolbar is showing: the selection's, or the
    /// tool's.
    pub(super) fn style_kind(&self) -> Option<Kind> {
        self.selected_kind().or(match self.tool {
            Tool::Make(kind) => Some(kind),
            _ => None,
        })
    }

    pub(super) fn set_tool(&mut self, tool: Tool, cx: &mut Context<Self>) {
        self.commit_text();
        self.tool = tool;
        self.selected = None;
        if tool != Tool::Select {
            self.marquee = None;
        }
        self.popup = None;
        cx.notify();
    }

    pub(super) fn set_color(&mut self, color: Color, cx: &mut Context<Self>) {
        self.color = color;
        self.popup = None;
        if let Some(id) = self.selected.or(self.editing) {
            self.checkpoint();
            if let Some(a) = self.annot_mut(id) {
                a.color = color;
            }
        }
        if let Some(panel) = self.panel.upgrade() {
            let hex = to_hex(color);
            panel.update(cx, |panel, _| panel.set_snap(|snap| snap.color = hex));
        }
        cx.notify();
    }

    pub(super) fn set_size(&mut self, size: f32, cx: &mut Context<Self>) {
        self.size = size.clamp(1.0, 14.0);
        if let Some(id) = self.selected.or(self.editing) {
            self.checkpoint();
            let kind = self.annot(id).map(|a| a.kind);
            let width = kind.map(|k| self.width_for(k)).unwrap_or(self.size);
            if let Some(a) = self.annot_mut(id) {
                a.width = width;
            }
        }
        cx.notify();
    }

    /// Cycle the style of the selection or the tool: an arrow's head, or how
    /// a shape or a label is filled.
    pub(super) fn cycle_style(&mut self, cx: &mut Context<Self>) {
        let Some(kind) = self.style_kind() else { return };
        if kind == Kind::Arrow {
            self.arrow = self.arrow.next();
        } else if matches!(kind, Kind::Rect | Kind::Oval | Kind::Text) {
            self.fill = self.fill.next();
        } else {
            return;
        }
        if let Some(id) = self.selected.or(self.editing) {
            self.checkpoint();
            let (arrow, fill) = (self.arrow, self.fill);
            if let Some(a) = self.annot_mut(id) {
                a.arrow = arrow;
                a.fill = fill;
            }
        }
        cx.notify();
    }

    pub(super) fn delete_selected(&mut self, cx: &mut Context<Self>) {
        if let Some(id) = self.selected.take() {
            self.checkpoint();
            self.doc.annots.retain(|a| a.id != id);
            cx.notify();
        }
    }

    /* ---------- text ---------- */

    /// Leave text editing; an empty label is dropped.
    pub(super) fn commit_text(&mut self) {
        if let Some(id) = self.editing.take()
            && self.annot(id).is_some_and(|a| a.text.trim().is_empty())
        {
            self.doc.annots.retain(|a| a.id != id);
            // The checkpoint taken when it was created is now a no-op.
            if self.undo.last().is_some_and(|d| d.annots.len() == self.doc.annots.len()) {
                self.undo.pop();
            }
            self.selected = None;
        }
    }

    fn type_text(&mut self, typed: &str, cx: &mut Context<Self>) {
        if let Some(a) = self.editing.and_then(|id| self.annot_mut(id)) {
            a.text.push_str(typed);
            self.caret_on = true;
            cx.notify();
        }
    }

    /* ---------- mouse ---------- */

    pub(super) fn mouse_down(&mut self, event: &MouseDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        window.focus(&self.focus);
        self.popup = None;
        let view = self.view();
        let p = view.to_image(event.position);
        if event.button != MouseButton::Left {
            // Any other button drags the image around.
            self.drag = Some(Drag::Pan { last: event.position });
            return;
        }
        if self.editing.is_some() {
            let inside =
                self.editing.and_then(|id| self.annot(id)).is_some_and(|a| a.hit(p, 4.0, Self::text_size_of(a)));
            if inside {
                return;
            }
            self.commit_text();
            cx.notify();
            // The click that ends a label does not also start the next thing.
            return;
        }
        match self.tool {
            Tool::Select => {
                let tol = 9.0 / view.zoom.max(0.01);
                let grabbed = self.selected.and_then(|id| self.annot(id)).and_then(|a| {
                    a.handles().into_iter().find(|(_, at)| at.dist(p) <= tol).map(|(handle, _)| (a.id, handle))
                });
                if let Some((id, handle)) = grabbed {
                    self.checkpoint();
                    self.drag = Some(Drag::Reshape { id, handle });
                } else if let Some(id) = self.hit(p) {
                    self.selected = Some(id);
                    self.marquee = None;
                    if event.click_count >= 2 && self.annot(id).is_some_and(|a| a.kind == Kind::Text) {
                        self.checkpoint();
                        self.editing = Some(id);
                    } else {
                        self.drag = Some(Drag::Move { id, last: p, moved: false });
                    }
                } else {
                    self.selected = None;
                    self.marquee = None;
                    self.drag = Some(Drag::Marquee { start: self.clamp(p) });
                }
            }
            Tool::Make(Kind::Text) => {
                if !text::available() {
                    self.say("No usable system font was found, so text cannot be drawn.", true, cx);
                    return;
                }
                if let Some(id) = self.hit(p).filter(|id| self.annot(*id).is_some_and(|a| a.kind == Kind::Text)) {
                    self.checkpoint();
                    self.selected = Some(id);
                    self.editing = Some(id);
                } else {
                    self.checkpoint();
                    let width = self.width_for(Kind::Text);
                    // The click lands on the first line's middle.
                    let mut annot = self.new_annot(Kind::Text, Pt::new(p.x, p.y - width * 0.62));
                    annot.width = width;
                    self.selected = Some(annot.id);
                    self.editing = Some(annot.id);
                    self.doc.annots.push(annot);
                }
                self.caret_on = true;
            }
            Tool::Make(Kind::Counter) => {
                self.checkpoint();
                let mut annot = self.new_annot(Kind::Counter, p);
                annot.number = self.doc.next_counter;
                self.doc.next_counter += 1;
                self.selected = Some(annot.id);
                self.drag = Some(Drag::Move { id: annot.id, last: p, moved: true });
                self.doc.annots.push(annot);
            }
            Tool::Make(kind) => {
                self.checkpoint();
                let annot = self.new_annot(kind, if kind.is_effect() { self.clamp(p) } else { p });
                self.selected = None;
                self.drag = Some(Drag::Create(annot.id));
                self.doc.annots.push(annot);
            }
            Tool::Ruler => {
                if let Some((l, r, u, d)) = raster::measure(&self.doc.base, p) {
                    let text = format!("{} × {}", l + r + 1, u + d + 1);
                    cx.write_to_clipboard(ClipboardItem::new_string(text.clone()));
                    self.say(format!("Copied {text} px"), false, cx);
                }
            }
            Tool::Picker => self.copy_color(cx),
        }
        cx.notify();
    }

    fn new_annot(&mut self, kind: Kind, at: Pt) -> Annot {
        let id = self.next_id;
        self.next_id += 1;
        let mut annot = Annot::new(id, kind, at, self.color, self.width_for(kind));
        annot.fill = self.fill;
        annot.arrow = self.arrow;
        if kind == Kind::Highlight && self.color == SWATCHES[0] {
            // A red marker hides what it marks; the default one is yellow.
            annot.color = SWATCHES[2];
        }
        annot
    }

    fn clamp(&self, p: Pt) -> Pt {
        let (w, h) = self.image_size();
        Pt::new(p.x.clamp(0.0, w), p.y.clamp(0.0, h))
    }

    pub(super) fn mouse_move(&mut self, event: &MouseMoveEvent, cx: &mut Context<Self>) {
        let view = self.view();
        let p = view.to_image(event.position);
        let hover = self.canvas.contains(&event.position).then_some(p);
        if hover != self.hover {
            self.hover = hover;
            cx.notify();
        }
        let shift = event.modifiers.shift;
        match &mut self.drag {
            None => {}
            Some(Drag::Pan { last }) => {
                let d = event.position - *last;
                *last = event.position;
                let origin = view.origin - self.canvas.origin;
                self.pan = (f32::from(origin.x + d.x), f32::from(origin.y + d.y));
                cx.notify();
            }
            Some(Drag::Create(id)) => {
                let id = *id;
                let clamped = self.clamp(p);
                if let Some(a) = self.annot_mut(id) {
                    if a.kind == Kind::Draw {
                        if a.points.last().is_none_or(|last| last.dist(p) > 1.5) {
                            a.points.push(p);
                            a.b = p;
                        }
                    } else {
                        a.b = constrain(a.kind, a.a, if a.kind.is_effect() { clamped } else { p }, shift);
                    }
                }
                cx.notify();
            }
            Some(Drag::Move { id, last, moved }) => {
                let d = p.sub(*last);
                if d.len() > 0.0 {
                    let (id, first) = (*id, !*moved);
                    *last = p;
                    *moved = true;
                    if first {
                        self.checkpoint();
                    }
                    if let Some(a) = self.annot_mut(id) {
                        a.translate(d);
                    }
                    cx.notify();
                }
            }
            Some(Drag::Reshape { id, handle }) => {
                let (id, handle) = (*id, *handle);
                if let Some(a) = self.annot_mut(id) {
                    let target = match handle {
                        Handle::A => constrain(a.kind, a.b, p, shift),
                        Handle::B => constrain(a.kind, a.a, p, shift),
                        Handle::Corner(_) => p,
                    };
                    a.drag_handle(handle, target);
                }
                // A corner drag re-anchors the box, so from here on the
                // moving corner is simply `b`.
                if let Handle::Corner(_) = handle {
                    self.drag = Some(Drag::Reshape { id, handle: Handle::B });
                }
                cx.notify();
            }
            Some(Drag::Marquee { start }) => {
                let start = *start;
                let r = RectF::from_points(start, self.clamp(p));
                self.marquee = (r.w >= 2.0 && r.h >= 2.0).then_some(r);
                cx.notify();
            }
        }
    }

    pub(super) fn mouse_up(&mut self, _: &MouseUpEvent, cx: &mut Context<Self>) {
        match self.drag.take() {
            Some(Drag::Create(id)) => {
                if self.annot(id).is_some_and(|a| a.is_degenerate()) {
                    self.doc.annots.retain(|a| a.id != id);
                    self.undo.pop();
                } else {
                    self.selected = Some(id);
                }
            }
            Some(Drag::Marquee { .. }) if self.marquee.is_some_and(|r| r.w < 6.0 || r.h < 6.0) => {
                self.marquee = None;
            }
            _ => {}
        }
        cx.notify();
    }

    pub(super) fn scroll(&mut self, event: &ScrollWheelEvent, window: &mut Window, cx: &mut Context<Self>) {
        let delta = event.delta.pixel_delta(window.line_height());
        if event.modifiers.secondary() || event.modifiers.control {
            let factor = (f32::from(delta.y) * 0.01).exp().clamp(0.5, 2.0);
            self.set_zoom(self.zoom() * factor, Some(event.position), cx);
        } else if !self.fit {
            let view = self.view();
            let origin = view.origin - self.canvas.origin;
            self.pan = (f32::from(origin.x + delta.x), f32::from(origin.y + delta.y));
            cx.notify();
        }
    }

    /* ---------- keyboard ---------- */

    pub(super) fn key_down(&mut self, event: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        let ks = &event.keystroke;
        let key = ks.key.as_str();
        let m = ks.modifiers;
        let command = m.secondary();
        cx.stop_propagation();

        if self.editing.is_some() && !command {
            match key {
                "escape" => {
                    self.commit_text();
                    cx.notify();
                }
                "enter" if m.shift || m.alt => self.type_text("\n", cx),
                "enter" => {
                    self.commit_text();
                    cx.notify();
                }
                "backspace" => {
                    if let Some(a) = self.editing.and_then(|id| self.annot_mut(id)) {
                        a.text.pop();
                    }
                    self.caret_on = true;
                    cx.notify();
                }
                "tab" => self.type_text("    ", cx),
                _ => {
                    if let Some(typed) = ks.key_char.clone().filter(|c| !c.chars().any(char::is_control)) {
                        self.type_text(&typed, cx);
                    }
                }
            }
            return;
        }

        if command {
            match (key, m.shift) {
                ("z", false) => self.undo(cx),
                ("z", true) | ("y", _) => self.redo(cx),
                ("c", _) => self.copy_image(cx),
                ("s", false) => self.save(cx),
                ("s", true) => self.save_as(cx),
                ("o", _) => self.open_file(cx),
                ("v", _) => self.paste(cx),
                ("p", _) => self.pin(cx),
                ("t", _) => self.recognize(cx),
                ("b", _) => self.toggle_popup(Popup::Backdrop, cx),
                ("k", _) => self.crop(cx),
                ("0", _) => self.zoom_fit(cx),
                ("1", _) => self.zoom_actual(cx),
                ("=", _) | ("+", _) => self.zoom_by(1.25, cx),
                ("-", _) => self.zoom_by(0.8, cx),
                ("w", _) => window.remove_window(),
                _ => {}
            }
            return;
        }

        match key {
            "escape" => self.escape(window, cx),
            "enter" => self.crop(cx),
            "backspace" | "delete" => self.delete_selected(cx),
            "tab" => self.copy_color(cx),
            "[" => self.set_size(self.size - 1.0, cx),
            "]" => self.set_size(self.size + 1.0, cx),
            "f" => self.cycle_style(cx),
            "left" | "right" | "up" | "down" => self.nudge(key, m, cx),
            _ => {
                let tool = TOOLS.iter().flatten().find(|t| t.3.eq_ignore_ascii_case(key)).map(|t| t.0);
                if let Some(tool) = tool {
                    self.set_tool(tool, cx);
                }
            }
        }
    }

    fn nudge(&mut self, key: &str, m: Modifiers, cx: &mut Context<Self>) {
        let Some(id) = self.selected else { return };
        let step = if m.shift { 10.0 } else { 1.0 };
        let d = match key {
            "left" => Pt::new(-step, 0.0),
            "right" => Pt::new(step, 0.0),
            "up" => Pt::new(0.0, -step),
            _ => Pt::new(0.0, step),
        };
        self.checkpoint();
        if let Some(a) = self.annot_mut(id) {
            a.translate(d);
        }
        cx.notify();
    }

    /// Esc backs out one step at a time; with nothing left to back out of
    /// it closes the editor, copying first if that is the setting.
    fn escape(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.popup.take().is_some() || self.marquee.take().is_some() || self.selected.take().is_some() {
            cx.notify();
            return;
        }
        if self.drag.take().is_some() {
            cx.notify();
            return;
        }
        if self.snap_config(cx).copy_on_esc {
            // Copying is quick, but the window must not close under it.
            let image = self.flatten();
            match snap::png_image(&image) {
                Ok(png) => cx.write_to_clipboard(ClipboardItem::new_image(&png)),
                Err(err) => eprintln!("[snap] could not copy on close: {err:#}"),
            }
        }
        window.remove_window();
    }

    /* ---------- results ---------- */

    pub fn flatten(&self) -> RgbaImage {
        raster::flatten(&self.doc.base, &self.doc.annots)
    }

    /// Flatten off the main thread, then hand the image to `then`.
    fn with_flat(
        &mut self,
        cx: &mut Context<Self>,
        then: impl FnOnce(&mut Self, RgbaImage, &mut Context<Self>) + 'static,
    ) {
        self.commit_text();
        let (base, annots) = (self.doc.base.clone(), self.doc.annots.clone());
        cx.spawn(async move |this, cx| {
            let image = cx.background_spawn(async move { raster::flatten(&base, &annots) }).await;
            let _ = this.update(cx, |this, cx| then(this, image, cx));
        })
        .detach();
    }

    pub(super) fn copy_image(&mut self, cx: &mut Context<Self>) {
        self.with_flat(cx, |this, image, cx| {
            cx.spawn(async move |this, cx| {
                let png = cx.background_spawn(async move { snap::png_image(&image) }).await;
                let _ = this.update(cx, |this, cx| match png {
                    Ok(png) => {
                        cx.write_to_clipboard(ClipboardItem::new_image(&png));
                        this.say("Copied to the clipboard", false, cx);
                    }
                    Err(err) => this.say(format!("Could not copy: {err:#}"), true, cx),
                });
            })
            .detach();
            let _ = this;
        });
    }

    pub(super) fn save(&mut self, cx: &mut Context<Self>) {
        let snap_cfg = self.snap_config(cx);
        self.with_flat(cx, move |_, image, cx| {
            cx.spawn(async move |this, cx| {
                let folder = snap_cfg.folder_path();
                let format = snap_cfg.format.clone();
                let saved = cx.background_spawn(async move { snap::save_to_folder(&image, &folder, &format) }).await;
                let _ = this.update(cx, |this, cx| match saved {
                    Ok(path) => this.say(format!("Saved {}", path.display()), false, cx),
                    Err(err) => this.say(format!("Could not save: {err:#}"), true, cx),
                });
            })
            .detach();
        });
    }

    pub(super) fn save_as(&mut self, cx: &mut Context<Self>) {
        let snap_cfg = self.snap_config(cx);
        let extension = if snap_cfg.format == "jpg" { "jpg" } else { "png" };
        let chosen =
            cx.prompt_for_new_path(&snap_cfg.folder_path(), Some(&format!("{}.{extension}", snap::file_stem())));
        self.with_flat(cx, move |_, image, cx| {
            cx.spawn(async move |this, cx| {
                let Ok(Ok(Some(path))) = chosen.await else { return };
                let target = path.clone();
                let saved = cx.background_spawn(async move { snap::save_as(&image, &target) }).await;
                let _ = this.update(cx, |this, cx| match saved {
                    Ok(()) => this.say(format!("Saved {}", path.display()), false, cx),
                    Err(err) => this.say(format!("Could not save: {err:#}"), true, cx),
                });
            })
            .detach();
        });
    }

    pub(super) fn open_file(&mut self, cx: &mut Context<Self>) {
        let chosen = cx.prompt_for_paths(gpui::PathPromptOptions {
            files: true,
            directories: false,
            multiple: false,
            prompt: Some("Open".into()),
        });
        cx.spawn(async move |this, cx| {
            let Ok(Ok(Some(paths))) = chosen.await else { return };
            let Some(path) = paths.into_iter().next() else { return };
            let loaded = cx
                .background_spawn(async move {
                    let bytes = std::fs::read(&path)?;
                    anyhow::Ok(image::load_from_memory(&bytes)?.to_rgba8())
                })
                .await;
            let _ = this.update(cx, |this, cx| match loaded {
                Ok(image) => this.replace_image(image, cx),
                Err(err) => this.say(format!("Could not open that file: {err:#}"), true, cx),
            });
        })
        .detach();
    }

    /// Paste an image as a new document, or text into the label being typed.
    pub(super) fn paste(&mut self, cx: &mut Context<Self>) {
        let Some(item) = cx.read_from_clipboard() else { return };
        if self.editing.is_some() {
            if let Some(text) = item.text() {
                self.type_text(&text, cx);
            }
            return;
        }
        let image = item.entries().iter().find_map(|entry| match entry {
            gpui::ClipboardEntry::Image(image) => Some(image.clone()),
            _ => None,
        });
        let Some(image) = image else {
            self.say("There is no image on the clipboard.", true, cx);
            return;
        };
        match image::load_from_memory(image.bytes()) {
            Ok(decoded) => self.replace_image(decoded.to_rgba8(), cx),
            Err(err) => self.say(format!("Could not read the clipboard image: {err}"), true, cx),
        }
    }

    /// Swap in a different picture, undoably.
    fn replace_image(&mut self, image: RgbaImage, cx: &mut Context<Self>) {
        self.commit_text();
        self.checkpoint();
        self.doc = Doc { base: Arc::new(image), annots: Vec::new(), next_counter: 1 };
        self.generation += 1;
        self.selected = None;
        self.marquee = None;
        self.fit = true;
        cx.notify();
    }

    /// Crop to the selected area.
    pub(super) fn crop(&mut self, cx: &mut Context<Self>) {
        let Some(area) = self.marquee else {
            self.set_tool(Tool::Select, cx);
            self.say("Drag over the part to keep, then press Enter.", false, cx);
            return;
        };
        let Some((x, y, w, h)) = area.pixels(self.doc.base.width(), self.doc.base.height()) else { return };
        self.commit_text();
        self.checkpoint();
        self.doc.base = Arc::new(image::imageops::crop_imm(self.doc.base.as_ref(), x, y, w, h).to_image());
        for annot in &mut self.doc.annots {
            annot.translate(Pt::new(-(x as f32), -(y as f32)));
        }
        self.generation += 1;
        self.marquee = None;
        self.selected = None;
        self.fit = true;
        cx.notify();
    }

    /// Merge the annotations into the picture and transform the result.
    fn rasterize(
        &mut self,
        cx: &mut Context<Self>,
        transform: impl FnOnce(RgbaImage) -> anyhow::Result<RgbaImage> + Send + 'static,
    ) {
        self.busy = true;
        self.with_flat(cx, move |_, image, cx| {
            cx.spawn(async move |this, cx| {
                let result = cx.background_spawn(async move { transform(image) }).await;
                let _ = this.update(cx, |this, cx| {
                    this.busy = false;
                    match result {
                        Ok(image) => this.replace_image(image, cx),
                        Err(err) => this.say(format!("{err:#}"), true, cx),
                    }
                });
            })
            .detach();
        });
        cx.notify();
    }

    pub(super) fn apply_backdrop(&mut self, cx: &mut Context<Self>) {
        let backdrop = self.backdrop;
        self.popup = None;
        self.rasterize(cx, move |image| raster::apply_backdrop(&image, &backdrop));
    }

    pub(super) fn resize_to(&mut self, factor: f32, cx: &mut Context<Self>) {
        self.popup = None;
        self.scale = (self.scale * factor).max(0.5);
        self.rasterize(cx, move |image| {
            let w = ((image.width() as f32 * factor).round() as u32).max(1);
            let h = ((image.height() as f32 * factor).round() as u32).max(1);
            Ok(image::imageops::resize(&image, w, h, image::imageops::FilterType::CatmullRom))
        });
    }

    pub(super) fn toggle_popup(&mut self, popup: Popup, cx: &mut Context<Self>) {
        self.popup = if self.popup == Some(popup) { None } else { Some(popup) };
        cx.notify();
    }

    /// Read the text (or a QR code) in the selected area, or in the whole
    /// picture, and copy it.
    pub(super) fn recognize(&mut self, cx: &mut Context<Self>) {
        if !ocr::available() {
            self.say("Text recognition is only available on macOS for now.", true, cx);
            return;
        }
        let join = self.snap_config(cx).ocr_join_lines;
        let base = self.doc.base.clone();
        let area = self.marquee;
        self.busy = true;
        cx.notify();
        cx.spawn(async move |this, cx| {
            let read = cx
                .background_spawn(async move {
                    let region = match area.and_then(|r| r.pixels(base.width(), base.height())) {
                        Some((x, y, w, h)) => image::imageops::crop_imm(base.as_ref(), x, y, w, h).to_image(),
                        None => base.as_ref().clone(),
                    };
                    ocr::recognize(&region)
                })
                .await;
            let _ = this.update(cx, |this, cx| {
                this.busy = false;
                match read {
                    Ok(found) if found.is_empty() => this.say("No text found.", true, cx),
                    Ok(found) => {
                        let text = found.text(join);
                        let summary = match (found.codes.len(), found.lines.len()) {
                            (0, lines) => format!("Copied {lines} line{} of text", if lines == 1 { "" } else { "s" }),
                            (_, 0) => "Copied the code's content".to_string(),
                            (_, lines) => {
                                format!("Copied the code and {lines} line{}", if lines == 1 { "" } else { "s" })
                            }
                        };
                        cx.write_to_clipboard(ClipboardItem::new_string(text));
                        this.say(summary, false, cx);
                    }
                    Err(err) => this.say(format!("{err:#}"), true, cx),
                }
            });
        })
        .detach();
    }

    /// Copy the colour under the pointer, in the configured format.
    pub(super) fn copy_color(&mut self, cx: &mut Context<Self>) {
        let Some(color) = self.hover.and_then(|p| raster::pixel(&self.doc.base, p)) else {
            self.say("Point at a pixel first.", true, cx);
            return;
        };
        let text = format_color(color, &self.snap_config(cx).color_format);
        cx.write_to_clipboard(ClipboardItem::new_string(text.clone()));
        self.say(format!("Copied {text}"), false, cx);
    }

    pub(super) fn copy_average_color(&mut self, cx: &mut Context<Self>) {
        let Some(color) = self.marquee.and_then(|r| raster::average_color(&self.doc.base, r)) else { return };
        let text = format_color(color, &self.snap_config(cx).color_format);
        cx.write_to_clipboard(ClipboardItem::new_string(text.clone()));
        self.say(format!("Copied the average colour, {text}"), false, cx);
    }

    /// Float the picture above everything else.
    pub(super) fn pin(&mut self, cx: &mut Context<Self>) {
        let (panel, scale) = (self.panel.clone(), self.scale);
        self.with_flat(cx, move |this, image, cx| {
            let protection = panel.upgrade().is_none_or(|p| p.read(cx).cfg.window.protection);
            let capture = Capture::new(image, scale, "Pinned");
            cx.defer(move |cx| super::pin::open(capture, protection, cx));
            this.say("Pinned — drag it anywhere, Esc or double-click closes it", false, cx);
        });
    }

    /// Hand the annotated picture to the assistant.
    pub(super) fn ask(&mut self, cx: &mut Context<Self>) {
        let panel = self.panel.clone();
        self.with_flat(cx, move |this, image, cx| {
            let Some(panel) = panel.upgrade() else { return };
            cx.defer(move |cx| {
                let window = panel.read(cx).window_handle;
                let _ = cx.update_window(window, |_, window, cx| {
                    panel.update(cx, |panel, cx| panel.attach_image(image, window, cx));
                });
            });
            this.say("Attached to VeilPilot — type your question there", false, cx);
        });
    }
}

impl Editor {
    pub fn dev_state(&self) -> serde_json::Value {
        serde_json::json!({
            "width": self.doc.base.width(),
            "height": self.doc.base.height(),
            "annotations": self.doc.annots.iter().map(|a| format!("{:?}", a.kind)).collect::<Vec<_>>(),
            "tool": format!("{:?}", self.tool),
            "selected": self.selected,
            "marquee": self.marquee.map(|r| [r.x, r.y, r.w, r.h]),
            "undo": self.undo.len(),
            "zoom": self.zoom_percent().round(),
            "note": self.note.as_ref().map(|n| n.1.clone()),
        })
    }

    /// Dev control: `tool <key>`, `drag x0 y0 x1 y1` (image pixels), `type
    /// text`, `key <keystroke>`, `select x y w h`, `copy`, `save`, `undo`,
    /// `ocr`, `pin`, `ask`, `backdrop`, `close`.
    pub fn dev(&mut self, line: &str, window: &mut Window, cx: &mut Context<Self>) -> String {
        let (verb, rest) = line.split_once(' ').unwrap_or((line, ""));
        let nums: Vec<f32> = rest.split_whitespace().filter_map(|n| n.parse().ok()).collect();
        let at = |this: &Self, x: f32, y: f32| this.view().to_screen(Pt::new(x, y));
        match verb {
            "tool" => {
                let Some(tool) = TOOLS.iter().flatten().find(|t| t.3.eq_ignore_ascii_case(rest)).map(|t| t.0) else {
                    return format!("error: no tool on {rest}");
                };
                self.set_tool(tool, cx);
            }
            "drag" if nums.len() == 4 => {
                let (a, b) = (at(self, nums[0], nums[1]), at(self, nums[2], nums[3]));
                let down = MouseDownEvent {
                    button: MouseButton::Left,
                    position: a,
                    modifiers: Modifiers::default(),
                    click_count: 1,
                    first_mouse: false,
                };
                self.mouse_down(&down, window, cx);
                for i in 1..=8 {
                    let t = i as f32 / 8.0;
                    let p = point(a.x + (b.x - a.x) * t, a.y + (b.y - a.y) * t);
                    let event = MouseMoveEvent {
                        position: p,
                        pressed_button: Some(MouseButton::Left),
                        modifiers: Modifiers::default(),
                    };
                    self.mouse_move(&event, cx);
                }
                self.mouse_up(
                    &MouseUpEvent {
                        button: MouseButton::Left,
                        position: b,
                        modifiers: Modifiers::default(),
                        click_count: 1,
                    },
                    cx,
                );
            }
            "hover" if nums.len() == 2 => {
                let event = MouseMoveEvent {
                    position: at(self, nums[0], nums[1]),
                    pressed_button: None,
                    modifiers: Modifiers::default(),
                };
                self.mouse_move(&event, cx);
            }
            "type" => self.type_text(rest, cx),
            "key" => match gpui::Keystroke::parse(rest) {
                Ok(keystroke) => {
                    let event = KeyDownEvent { keystroke, is_held: false };
                    self.key_down(&event, window, cx);
                }
                Err(err) => return format!("error: {err}"),
            },
            "select" if nums.len() == 4 => {
                self.set_tool(Tool::Select, cx);
                self.marquee = Some(RectF::new(nums[0], nums[1], nums[2], nums[3]));
                cx.notify();
            }
            "copy" => self.copy_image(cx),
            "save" => self.save(cx),
            "undo" => self.undo(cx),
            "ocr" => self.recognize(cx),
            "pin" => self.pin(cx),
            "ask" => self.ask(cx),
            "backdrop" => self.toggle_popup(Popup::Backdrop, cx),
            "apply-backdrop" => self.apply_backdrop(cx),
            "close" => window.remove_window(),
            _ => return format!("error: unknown editor command {verb}"),
        }
        "ok".into()
    }
}

/// Hold Shift for a square, a circle, or a line at a multiple of 45°.
fn constrain(kind: Kind, anchor: Pt, p: Pt, shift: bool) -> Pt {
    if !shift {
        return p;
    }
    let d = p.sub(anchor);
    if kind.is_segment() {
        let step = std::f32::consts::FRAC_PI_4;
        let angle = (d.y.atan2(d.x) / step).round() * step;
        let len = d.len();
        anchor.add(Pt::new(angle.cos() * len, angle.sin() * len))
    } else if kind.is_box() {
        let side = d.x.abs().max(d.y.abs());
        anchor.add(Pt::new(side * d.x.signum(), side * d.y.signum()))
    } else {
        p
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shift_constrains_shapes() {
        let o = Pt::new(10.0, 10.0);
        assert_eq!(constrain(Kind::Rect, o, Pt::new(50.0, 30.0), true), Pt::new(50.0, 50.0));
        assert_eq!(constrain(Kind::Rect, o, Pt::new(50.0, 30.0), false), Pt::new(50.0, 30.0));
        let end = constrain(Kind::Arrow, o, Pt::new(110.0, 14.0), true);
        assert!((end.y - 10.0).abs() < 0.01 && end.x > 100.0, "near-horizontal snaps flat: {end:?}");
        let diagonal = constrain(Kind::Line, o, Pt::new(60.0, 55.0), true);
        assert!(((diagonal.x - 10.0) - (diagonal.y - 10.0)).abs() < 0.01, "{diagonal:?}");
    }

    #[test]
    fn every_tool_has_its_own_key() {
        let mut keys = std::collections::HashSet::new();
        for tool in TOOLS.iter().flatten() {
            assert!(keys.insert(tool.3), "{} is bound twice", tool.3);
        }
    }
}
