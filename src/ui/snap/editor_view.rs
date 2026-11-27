//! Drawing the editor: the toolbar, the canvas (the picture, its annotations
//! and whatever is being dragged), the floating bars, and the status bar.

use std::sync::Arc;

use gpui::{
    AnyElement, App, Bounds, Context, Corners, CursorStyle, DispatchPhase, Entity, Focusable, FontWeight, Hsla,
    MouseDownEvent, MouseMoveEvent, MouseUpEvent, PathBuilder, PathStyle, Pixels, Rgba, ScrollWheelEvent, SharedString,
    StrokeOptions, Window, canvas, div, fill, point, prelude::*, px, size,
};
use lyon::path::{LineCap, LineJoin};

use super::editor::{Editor, FIT_MARGIN, Popup, SIZES, SWATCHES, TOOLS, Tool, View};
use crate::snap::annot::{Annot, ArrowStyle, Color, FillMode, Kind, Paint, Pt, RectF, Seg, Shape, to_hex};
use crate::snap::raster::{self, BACKDROPS};
use crate::ui::theme;
use crate::ui::widgets::{button, icon_button, key_badge, primary_button, swallow_mouse_down, tooltip};

pub(super) fn rgba(c: Color) -> Rgba {
    Rgba { r: c[0] as f32 / 255.0, g: c[1] as f32 / 255.0, b: c[2] as f32 / 255.0, a: c[3] as f32 / 255.0 }
}

/// "⌘" on macOS, "Ctrl+" elsewhere.
fn cmd(key: &str) -> String {
    if cfg!(target_os = "macos") { format!("⌘{key}") } else { format!("Ctrl+{key}") }
}

/// A [`Shape`] as a GPUI path at the view's zoom.
fn gpui_path(shape: &Shape, view: &View) -> Option<gpui::Path<Pixels>> {
    let mut builder = match shape.paint {
        Paint::Fill(_) => PathBuilder::fill()
            .with_style(PathStyle::Fill(gpui::FillOptions::default().with_fill_rule(gpui::FillRule::NonZero))),
        Paint::Stroke { width, .. } => PathBuilder::fill().with_style(PathStyle::Stroke(
            StrokeOptions::default()
                .with_line_width((width * view.zoom).max(1.0))
                .with_line_cap(LineCap::Round)
                .with_line_join(LineJoin::Round),
        )),
    };
    let at = |p: Pt| view.to_screen(p);
    let mut any = false;
    for seg in &shape.segs {
        match *seg {
            Seg::Move(p) => builder.move_to(at(p)),
            Seg::Line(p) => {
                builder.line_to(at(p));
                any = true;
            }
            Seg::Quad(c, p) => {
                builder.curve_to(at(p), at(c));
                any = true;
            }
            Seg::Cubic(c1, c2, p) => {
                builder.cubic_bezier_to(at(p), at(c1), at(c2));
                any = true;
            }
            Seg::Close => builder.close(),
        }
    }
    if !any {
        return None;
    }
    builder.build().ok()
}

enum Item {
    Shapes(Vec<Shape>),
    Tile(Arc<gpui::RenderImage>, RectF),
}

/// Everything one frame of the canvas draws, copied out of the editor so the
/// paint closure owns it.
struct Scene {
    strips: Vec<(Arc<gpui::RenderImage>, u32, u32)>,
    image: (f32, f32),
    effects: Vec<(Arc<gpui::RenderImage>, RectF)>,
    dims: Vec<RectF>,
    items: Vec<Item>,
    selection: Option<(RectF, Vec<Pt>)>,
    marquee: Option<RectF>,
    caret: Option<(Pt, f32, Color)>,
    ruler: Option<(Pt, (u32, u32, u32, u32))>,
    cursor: CursorStyle,
}

impl Editor {
    fn scene(&self) -> Scene {
        let tile = |a: &Annot| self.tiles.get(&a.id).map(|t| (t.image.clone(), t.rect));
        let annots = &self.doc.annots;
        let spots: Vec<RectF> = annots.iter().filter(|a| a.kind == Kind::Spotlight).map(|a| a.rect()).collect();
        let (w, h) = self.image_size();
        let selection = self
            .selected
            .and_then(|id| annots.iter().find(|a| a.id == id))
            .filter(|_| self.editing.is_none())
            .map(|a| (a.bounds(Self::text_size_of(a)), a.handles().into_iter().map(|(_, p)| p).collect()));
        let caret =
            self.editing.and_then(|id| annots.iter().find(|a| a.id == id)).filter(|_| self.caret_on).and_then(|a| {
                let layout = crate::snap::text::layout(if a.text.is_empty() { " " } else { &a.text }, a.width)?;
                let row = layout.line_ends.len().saturating_sub(1);
                let x = if a.text.is_empty() { 0.0 } else { *layout.line_ends.last()? };
                let ink = match a.fill {
                    FillMode::Outline => a.color,
                    FillMode::Soft => [255, 255, 255, 255],
                    FillMode::Solid => crate::snap::annot::contrast(a.color),
                };
                Some((
                    Pt::new(a.a.x + x + a.width * 0.04, a.a.y + row as f32 * layout.line_height),
                    layout.line_height,
                    ink,
                ))
            });
        let ruler = (self.tool == Tool::Ruler)
            .then(|| self.hover.and_then(|p| raster::measure(&self.doc.base, p).map(|m| (p, m))))
            .flatten();
        Scene {
            strips: self.strips.clone(),
            image: (w, h),
            effects: annots.iter().filter(|a| a.kind.is_effect()).filter_map(tile).collect(),
            dims: raster::dim_rects(&spots, w, h),
            items: annots
                .iter()
                .filter(|a| !a.kind.is_effect() && a.kind != Kind::Spotlight)
                .filter_map(|a| {
                    if a.kind.is_tile() {
                        tile(a).map(|(i, r)| Item::Tile(i, r))
                    } else {
                        Some(Item::Shapes(a.shapes()))
                    }
                })
                .collect(),
            selection,
            marquee: self.marquee,
            caret,
            ruler,
            cursor: match self.tool {
                Tool::Select => CursorStyle::Arrow,
                Tool::Make(Kind::Text) => CursorStyle::IBeam,
                _ => CursorStyle::Crosshair,
            },
        }
    }

    fn render_canvas(&mut self, cx: &mut Context<Self>) -> impl IntoElement {
        let scene = self.scene();
        let entity = cx.entity();
        let for_prepaint = entity.clone();
        canvas(
            move |bounds, window, cx| {
                // The view depends on the canvas's size, which is only known
                // here; a change redraws the overlays that sit on top of it.
                let changed = for_prepaint.update(cx, |this, _| {
                    let changed = this.canvas != bounds;
                    this.canvas = bounds;
                    changed
                });
                if changed {
                    let weak = for_prepaint.downgrade();
                    cx.defer(move |cx| {
                        let _ = weak.update(cx, |_, cx| cx.notify());
                    });
                }
                (window.insert_hitbox(bounds, gpui::HitboxBehavior::Normal), for_prepaint.read(cx).view())
            },
            move |bounds, (hitbox, view), window, cx| {
                window.paint_quad(fill(bounds, theme::void()));
                window.with_content_mask(Some(gpui::ContentMask { bounds }), |window| {
                    paint_scene(&scene, &view, window);
                });
                window.set_cursor_style(scene.cursor, &hitbox);
                register_mouse(entity, hitbox, window, cx);
            },
        )
        .size_full()
    }
}

fn paint_scene(scene: &Scene, view: &View, window: &mut Window) {
    let (w, h) = scene.image;
    let whole = view.rect(RectF::new(0.0, 0.0, w, h));
    // A neutral card under the picture, so transparent corners read as such.
    window.paint_quad(fill(whole, gpui::rgb(0x2b2932)));
    for (strip, y, rows) in &scene.strips {
        let _ = window.paint_image(
            view.rect(RectF::new(0.0, *y as f32, w, *rows as f32)),
            Corners::default(),
            strip.clone(),
            0,
            false,
        );
    }
    for (image, rect) in &scene.effects {
        let _ = window.paint_image(view.rect(*rect), Corners::default(), image.clone(), 0, false);
    }
    for r in &scene.dims {
        window.paint_quad(fill(view.rect(*r), rgba(raster::SPOTLIGHT_DIM)));
    }
    for item in &scene.items {
        match item {
            Item::Tile(image, rect) => {
                let _ = window.paint_image(view.rect(*rect), Corners::default(), image.clone(), 0, false);
            }
            Item::Shapes(shapes) => {
                for shape in shapes {
                    let color = match shape.paint {
                        Paint::Fill(c) | Paint::Stroke { color: c, .. } => rgba(c),
                    };
                    if let Some(path) = gpui_path(shape, view) {
                        window.paint_path(path, color);
                    }
                }
            }
        }
    }

    let accent: Hsla = theme::lamp().into();
    let frame = |window: &mut Window, b: Bounds<Pixels>, color: Hsla| {
        let t = px(1.5);
        window.paint_quad(fill(Bounds::new(b.origin, size(b.size.width, t)), color));
        window.paint_quad(fill(Bounds::new(point(b.left(), b.bottom() - t), size(b.size.width, t)), color));
        window.paint_quad(fill(Bounds::new(b.origin, size(t, b.size.height)), color));
        window.paint_quad(fill(Bounds::new(point(b.right() - t, b.top()), size(t, b.size.height)), color));
    };

    if let Some(area) = scene.marquee {
        let shade = gpui::rgba(0x0c0b1099);
        for r in raster::dim_rects(&[area], w, h) {
            window.paint_quad(fill(view.rect(r), shade));
        }
        frame(window, view.rect(area), accent);
    }

    if let Some((bounds, handles)) = &scene.selection {
        let b = view.rect(*bounds);
        let pad = px(3.);
        frame(
            window,
            Bounds::new(point(b.left() - pad, b.top() - pad), size(b.size.width + pad * 2., b.size.height + pad * 2.)),
            Hsla { a: 0.75, ..accent },
        );
        for handle in handles {
            let c = view.to_screen(*handle);
            let outer = Bounds::new(point(c.x - px(5.), c.y - px(5.)), size(px(10.), px(10.)));
            window.paint_quad(gpui::quad(outer, px(5.), theme::white(), px(2.), accent, gpui::BorderStyle::default()));
        }
    }

    if let Some((top, height, color)) = scene.caret {
        let at = view.to_screen(top);
        window.paint_quad(fill(
            Bounds::new(at, size(px((2.0f32).max(height * view.zoom * 0.06)), px(height * view.zoom))),
            rgba(color),
        ));
    }

    if let Some((p, (l, r, u, d))) = scene.ruler {
        let (x, y) = (p.x.floor(), p.y.floor());
        let thin = (1.0 / view.zoom).max(1.0);
        let line: Hsla = theme::live().into();
        let across = RectF::new(x - l as f32, y, (l + r + 1) as f32, thin);
        let down = RectF::new(x, y - u as f32, thin, (u + d + 1) as f32);
        for mut r in [across, down] {
            // Never thinner than a screen pixel.
            r.w = r.w.max(1.5 / view.zoom);
            r.h = r.h.max(1.5 / view.zoom);
            window.paint_quad(fill(view.rect(r), line));
        }
        // End caps.
        let cap = 5.0 / view.zoom;
        for (cx_, cy_, horizontal) in [
            (x - l as f32, y, false),
            (x + r as f32 + 1.0, y, false),
            (x, y - u as f32, true),
            (x, y + d as f32 + 1.0, true),
        ] {
            let r = if horizontal {
                RectF::new(cx_ - cap, cy_, 2.0 * cap, 1.5 / view.zoom)
            } else {
                RectF::new(cx_, cy_ - cap, 1.5 / view.zoom, 2.0 * cap)
            };
            window.paint_quad(fill(view.rect(r), line));
        }
    }
}

fn register_mouse(entity: Entity<Editor>, hitbox: gpui::Hitbox, window: &mut Window, _: &mut App) {
    window.on_mouse_event({
        let (entity, hitbox) = (entity.clone(), hitbox.clone());
        move |event: &MouseDownEvent, phase, window, cx| {
            if phase == DispatchPhase::Bubble && hitbox.is_hovered(window) {
                entity.update(cx, |this, cx| this.mouse_down(event, window, cx));
            }
        }
    });
    window.on_mouse_event({
        let entity = entity.clone();
        move |event: &MouseMoveEvent, phase, _, cx| {
            if phase == DispatchPhase::Bubble {
                entity.update(cx, |this, cx| this.mouse_move(event, cx));
            }
        }
    });
    window.on_mouse_event({
        let entity = entity.clone();
        move |event: &MouseUpEvent, phase, _, cx| {
            if phase == DispatchPhase::Bubble {
                entity.update(cx, |this, cx| this.mouse_up(event, cx));
            }
        }
    });
    window.on_mouse_event(move |event: &ScrollWheelEvent, phase, window, cx| {
        if phase == DispatchPhase::Bubble && hitbox.is_hovered(window) {
            entity.update(cx, |this, cx| this.scroll(event, window, cx));
        }
    });
}

/* ---------- chrome ---------- */

fn divider() -> gpui::Div {
    div().flex_none().w(px(1.)).h(px(18.)).mx(px(4.)).bg(theme::hairline())
}

/// A chip floating over the canvas.
fn floating() -> gpui::Div {
    div()
        .flex()
        .items_center()
        .gap(px(4.))
        .p(px(4.))
        .bg(theme::raise())
        .border_1()
        .border_color(theme::edge())
        .rounded(px(theme::RADIUS_LG))
        .shadow_lg()
}

impl Editor {
    fn render_toolbar(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let tools = TOOLS.iter().enumerate().map(|(i, entry)| match entry {
            None => divider().into_any_element(),
            Some((tool, glyph, name, key)) => {
                let tool = *tool;
                icon_button(
                    ("tool", i),
                    glyph,
                    self.tool == tool,
                    cx.listener(move |this, _, _, cx| this.set_tool(tool, cx)),
                )
                .tooltip(tooltip(*name, Some(*key)))
                .into_any_element()
            }
        });

        let sizes = SIZES.iter().enumerate().map(|(i, (name, value))| {
            let value = *value;
            let on = (self.size - value).abs() < 0.01;
            swallow_mouse_down(
                div()
                    .id(("size", i))
                    .flex_none()
                    .size(px(24.))
                    .flex()
                    .items_center()
                    .justify_center()
                    .rounded(px(6.))
                    .cursor_pointer()
                    .when(on, |d| d.bg(theme::lamp_soft()))
                    .hover(|s| s.bg(theme::control_hover()))
                    .child(div().size(px(3.0 + i as f32 * 3.5)).rounded_full().bg(if on {
                        theme::lamp()
                    } else {
                        theme::haze()
                    }))
                    .tooltip(tooltip(format!("Size {name}"), Some("[ ]")))
                    .on_click(cx.listener(move |this, _, _, cx| this.set_size(value, cx))),
            )
        });

        let style = self.style_kind().and_then(|kind| match kind {
            Kind::Arrow => Some(match self.arrow {
                ArrowStyle::Shape => "Tapered",
                ArrowStyle::Line => "Line",
                ArrowStyle::Double => "Two-way",
            }),
            Kind::Rect | Kind::Oval | Kind::Text => Some(match (kind, self.fill) {
                (Kind::Text, FillMode::Outline) => "Plain",
                (Kind::Text, FillMode::Soft) => "On dark",
                (Kind::Text, FillMode::Solid) => "On colour",
                (_, FillMode::Outline) => "Outline",
                (_, FillMode::Soft) => "Tinted",
                (_, FillMode::Solid) => "Filled",
            }),
            _ => None,
        });

        div()
            .id("editor-toolbar")
            .flex_none()
            .h(px(46.))
            .flex()
            .items_center()
            .gap(px(2.))
            .px(px(10.))
            .bg(theme::glass())
            .border_b_1()
            .border_color(theme::hairline())
            .children(tools)
            .child(div().flex_1())
            .when_some(style, |d, label| {
                d.child(
                    button("style", label, false, cx.listener(|this, _, _, cx| this.cycle_style(cx)))
                        .tooltip(tooltip("Change the style", Some("F"))),
                )
                .child(divider())
            })
            .children(sizes)
            .child(divider())
            .child(swallow_mouse_down(
                div()
                    .id("colour")
                    .flex_none()
                    .size(px(28.))
                    .flex()
                    .items_center()
                    .justify_center()
                    .rounded(px(theme::RADIUS))
                    .cursor_pointer()
                    .when(self.popup == Some(Popup::Color), |d| d.bg(theme::control_hover()))
                    .hover(|s| s.bg(theme::control_hover()))
                    .child(
                        div()
                            .size(px(16.))
                            .rounded_full()
                            .bg(rgba(self.color))
                            .border_2()
                            .border_color(theme::alpha(theme::read(), 0.85)),
                    )
                    .tooltip(tooltip("Colour", None::<&str>))
                    .on_click(cx.listener(|this, _, _, cx| this.toggle_popup(Popup::Color, cx))),
            ))
            .child(divider())
            .child(
                icon_button("undo", "undo", false, cx.listener(|this, _, _, cx| this.undo(cx)))
                    .when(!self.can_undo(), |d| d.opacity(0.35))
                    .tooltip(tooltip("Undo", Some(cmd("Z")))),
            )
            .child(
                icon_button("redo", "redo", false, cx.listener(|this, _, _, cx| this.redo(cx)))
                    .when(!self.can_redo(), |d| d.opacity(0.35))
                    .tooltip(tooltip("Redo", Some(cmd("⇧Z")))),
            )
    }

    fn render_status(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let (w, h) = (self.doc.base.width(), self.doc.base.height());
        let hover_color = self.hover.and_then(|p| raster::pixel(&self.doc.base, p));
        let info = div()
            .flex()
            .items_center()
            .gap(px(10.))
            .font_family(theme::mono_family())
            .text_size(px(11.))
            .text_color(theme::haze())
            .child(format!("{w} × {h}"))
            .when(self.scale > 1.01, |d| {
                d.child(div().text_color(theme::faint()).child(format!("@{}x", (self.scale * 10.0).round() / 10.0)))
            });

        let zoom = div()
            .flex()
            .items_center()
            .child(
                icon_button("zoom-out", "zoom-out", false, cx.listener(|this, _, _, cx| this.zoom_by(0.8, cx)))
                    .tooltip(tooltip("Zoom out", Some(cmd("−")))),
            )
            .child(swallow_mouse_down(
                div()
                    .id("zoom-level")
                    .w(px(46.))
                    .flex()
                    .justify_center()
                    .py(px(4.))
                    .rounded(px(6.))
                    .font_family(theme::mono_family())
                    .text_size(px(11.))
                    .text_color(if self.fit { theme::haze() } else { theme::read() })
                    .cursor_pointer()
                    .hover(|s| s.bg(theme::control_hover()))
                    .child(format!("{}%", self.zoom_percent().round()))
                    .tooltip(tooltip("Fit to the window — click again for actual size", Some(cmd("0"))))
                    .on_click(cx.listener(
                        |this, _, _, cx| {
                            if this.fit { this.zoom_actual(cx) } else { this.zoom_fit(cx) }
                        },
                    )),
            ))
            .child(
                icon_button("zoom-in", "zoom-in", false, cx.listener(|this, _, _, cx| this.zoom_by(1.25, cx)))
                    .tooltip(tooltip("Zoom in", Some(cmd("+")))),
            );

        let note = self.note.as_ref().map(|(_, text, error)| {
            div()
                .flex_1()
                .min_w_0()
                .truncate()
                .text_size(px(11.5))
                .text_color(if *error { theme::live() } else { theme::ok() })
                .child(text.clone())
        });

        div()
            .id("editor-status")
            .flex_none()
            .h(px(46.))
            .flex()
            .items_center()
            .gap(px(8.))
            .px(px(12.))
            .bg(theme::glass())
            .border_t_1()
            .border_color(theme::hairline())
            .child(info)
            .child(zoom)
            .when_some(hover_color.filter(|_| note.is_none()), |d, color| {
                d.child(
                    div()
                        .flex()
                        .items_center()
                        .gap(px(6.))
                        .child(
                            div()
                                .size(px(12.))
                                .rounded(px(3.))
                                .bg(rgba(color))
                                .border_1()
                                .border_color(theme::hairline()),
                        )
                        .child(
                            div()
                                .font_family(theme::mono_family())
                                .text_size(px(11.))
                                .text_color(theme::haze())
                                .child(to_hex(color)),
                        )
                        .child(key_badge("Tab")),
                )
            })
            .when(self.busy && note.is_none(), |d| {
                d.child(div().text_size(px(11.5)).text_color(theme::lamp()).child("Working…"))
            })
            .map(|d| match note {
                Some(note) => d.child(note),
                None => d.child(div().flex_1()),
            })
            .child(
                icon_button("open", "folder", false, cx.listener(|this, _, _, cx| this.open_file(cx)))
                    .tooltip(tooltip("Open an image", Some(cmd("O")))),
            )
            .child(
                icon_button("crop", "crop", false, cx.listener(|this, _, _, cx| this.crop(cx)))
                    .tooltip(tooltip("Crop to the selected area", Some("↩"))),
            )
            .child(
                icon_button(
                    "resize",
                    "resize",
                    self.popup == Some(Popup::Resize),
                    cx.listener(|this, _, _, cx| this.toggle_popup(Popup::Resize, cx)),
                )
                .tooltip(tooltip("Resize the image", None::<&str>)),
            )
            .child(
                icon_button(
                    "backdrop",
                    "frame",
                    self.popup == Some(Popup::Backdrop),
                    cx.listener(|this, _, _, cx| this.toggle_popup(Popup::Backdrop, cx)),
                )
                .tooltip(tooltip("Backdrop: frame it on a gradient", Some(cmd("B")))),
            )
            .child(
                icon_button("ocr", "scan-text", false, cx.listener(|this, _, _, cx| this.recognize(cx)))
                    .tooltip(tooltip("Copy the text or QR code in the picture", Some(cmd("T")))),
            )
            .child(
                icon_button("pin", "pin", false, cx.listener(|this, _, _, cx| this.pin(cx)))
                    .tooltip(tooltip("Pin on top of everything", Some(cmd("P")))),
            )
            .child(
                icon_button("ask", "sparkle", false, cx.listener(|this, _, _, cx| this.ask(cx)))
                    .tooltip(tooltip("Ask VeilPilot about this picture", None::<&str>)),
            )
            .child(divider())
            .child(
                button("save", "Save", false, cx.listener(|this, _, _, cx| this.save(cx)))
                    .tooltip(tooltip("Save to the screenshots folder — ⇧ for Save As", Some(cmd("S")))),
            )
            .child(
                primary_button("copy", "Copy", cx.listener(|this, _, _, cx| this.copy_image(cx)))
                    .tooltip(tooltip("Copy the image", Some(cmd("C")))),
            )
    }

    /// What can be done with the selected area, floating under it.
    fn render_marquee_bar(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let area = self.marquee?;
        let view = self.view();
        let b = view.rect(area);
        let local = |p: gpui::Point<Pixels>| point(p.x - self.canvas.origin.x, p.y - self.canvas.origin.y);
        let below = local(point(b.left(), b.bottom() + px(8.)));
        // Above the area when there is no room below it.
        let top = if below.y + px(40.) > self.canvas.size.height { local(b.origin).y - px(46.) } else { below.y };
        let left = below.x.max(px(8.)).min((self.canvas.size.width - px(330.)).max(px(8.)));
        let (pw, ph) = (area.w.round() as u32, area.h.round() as u32);
        Some(
            floating()
                .absolute()
                .left(left)
                .top(top.max(px(8.)))
                .occlude()
                .child(
                    div()
                        .px(px(6.))
                        .font_family(theme::mono_family())
                        .text_size(px(11.))
                        .text_color(theme::haze())
                        .child(format!("{pw} × {ph}")),
                )
                .child(
                    primary_button("marquee-crop", "Crop", cx.listener(|this, _, _, cx| this.crop(cx)))
                        .tooltip(tooltip("Keep only this area", Some("↩"))),
                )
                .child(button("marquee-text", "Copy text", false, cx.listener(|this, _, _, cx| this.recognize(cx))))
                .child(button(
                    "marquee-colour",
                    "Average colour",
                    false,
                    cx.listener(|this, _, _, cx| this.copy_average_color(cx)),
                ))
                .into_any_element(),
        )
    }

    /// The ruler's readout, by the pointer.
    fn render_ruler_label(&self) -> Option<AnyElement> {
        if self.tool != Tool::Ruler {
            return None;
        }
        let p = self.hover?;
        let (l, r, u, d) = raster::measure(&self.doc.base, p)?;
        let at = self.view().to_screen(p);
        let scale = self.scale.max(1.0);
        let (w, h) = (l + r + 1, u + d + 1);
        let points =
            (scale > 1.01).then(|| format!("{} × {} pt", (w as f32 / scale).round(), (h as f32 / scale).round()));
        Some(
            floating()
                .absolute()
                .left(at.x - self.canvas.origin.x + px(14.))
                .top(at.y - self.canvas.origin.y + px(14.))
                .px(px(8.))
                .font_family(theme::mono_family())
                .text_size(px(11.))
                .child(format!("{w} × {h} px"))
                .when_some(points, |d, points| d.child(div().text_color(theme::faint()).child(points)))
                .into_any_element(),
        )
    }

    fn render_popup(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let popup = self.popup?;
        let card = div()
            .id("editor-popup")
            .absolute()
            .right(px(12.))
            .map(|d| if popup == Popup::Color { d.top(px(8.)).w(px(212.)) } else { d.bottom(px(12.)).w(px(292.)) })
            .p(px(14.))
            .flex()
            .flex_col()
            .gap(px(12.))
            .bg(theme::raise())
            .border_1()
            .border_color(theme::edge())
            .rounded(px(theme::RADIUS_LG))
            .shadow_lg()
            .occlude()
            .on_mouse_down(gpui::MouseButton::Left, |_, _, cx| cx.stop_propagation());
        let heading = |text: &str| div().text_size(px(13.)).font_weight(FontWeight::SEMIBOLD).child(text.to_string());
        let caption = |text: &str| div().text_size(px(11.)).text_color(theme::haze()).child(text.to_string());

        Some(match popup {
            Popup::Color => {
                let swatches = SWATCHES.iter().enumerate().map(|(i, color)| {
                    let color = *color;
                    let on = self.color == color;
                    swallow_mouse_down(
                        div()
                            .id(("swatch", i))
                            .flex_none()
                            .size(px(30.))
                            .flex()
                            .items_center()
                            .justify_center()
                            .rounded_full()
                            .border_2()
                            .border_color(if on { theme::read() } else { theme::alpha(theme::read(), 0.0) })
                            .cursor_pointer()
                            .hover(|s| s.border_color(theme::haze()))
                            .child(
                                div()
                                    .size(px(22.))
                                    .rounded_full()
                                    .bg(rgba(color))
                                    .border_1()
                                    .border_color(theme::hairline()),
                            )
                            .tooltip(tooltip(to_hex(color), None::<&str>))
                            .on_click(cx.listener(move |this, _, _, cx| this.set_color(color, cx))),
                    )
                });
                card.child(heading("Colour"))
                    .child(div().flex().flex_wrap().gap(px(6.)).children(swatches))
                    .child(caption("Also changes the selected annotation."))
                    .into_any_element()
            }
            Popup::Backdrop => {
                let current = self.backdrop;
                let gradients = BACKDROPS.iter().enumerate().map(|(i, (name, from, to))| {
                    let (from, to) = (*from, *to);
                    let on = current.from == from && current.to == to;
                    swallow_mouse_down(
                        div()
                            .id(("backdrop", i))
                            .size(px(28.))
                            .rounded(px(8.))
                            .border_2()
                            .border_color(if on { theme::lamp() } else { theme::alpha(theme::read(), 0.12) })
                            .bg(gpui::linear_gradient(
                                135.,
                                gpui::linear_color_stop(rgba(from), 0.),
                                gpui::linear_color_stop(rgba(to), 1.),
                            ))
                            .cursor_pointer()
                            .tooltip(tooltip(*name, None::<&str>))
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.backdrop.from = from;
                                this.backdrop.to = to;
                                cx.notify();
                            })),
                    )
                });
                let choice = |id: &'static str, label: &'static str, on: bool| {
                    swallow_mouse_down(
                        div()
                            .id(id)
                            .px(px(9.))
                            .py(px(4.))
                            .rounded(px(6.))
                            .text_size(px(11.))
                            .cursor_pointer()
                            .text_color(if on { theme::lamp() } else { theme::haze() })
                            .bg(if on { theme::lamp_soft() } else { theme::control() })
                            .hover(|s| s.text_color(theme::read()))
                            .child(label),
                    )
                };
                let padding = [("Slim", 0.03f32), ("Regular", 0.06), ("Wide", 0.11)].into_iter().enumerate().map(
                    |(i, (label, value))| {
                        choice(["pad-0", "pad-1", "pad-2"][i], label, (current.padding - value).abs() < 0.001).on_click(
                            cx.listener(move |this, _, _, cx| {
                                this.backdrop.padding = value;
                                cx.notify();
                            }),
                        )
                    },
                );
                let corners = [("Square", 0.0f32), ("Rounded", 0.02), ("Round", 0.045)].into_iter().enumerate().map(
                    |(i, (label, value))| {
                        choice(["rad-0", "rad-1", "rad-2"][i], label, (current.radius - value).abs() < 0.001).on_click(
                            cx.listener(move |this, _, _, cx| {
                                this.backdrop.radius = value;
                                cx.notify();
                            }),
                        )
                    },
                );
                card.child(heading("Backdrop"))
                    .child(div().flex().flex_wrap().gap(px(6.)).children(gradients))
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .gap(px(6.))
                            .child(caption("Padding"))
                            .child(div().flex().gap(px(6.)).children(padding)),
                    )
                    .child(div().flex().flex_col().gap(px(6.)).child(caption("Corners")).child(
                        div().flex().gap(px(6.)).children(corners).child(div().flex_1()).child(
                            choice("shadow", "Shadow", current.shadow).on_click(cx.listener(|this, _, _, cx| {
                                this.backdrop.shadow = !this.backdrop.shadow;
                                cx.notify();
                            })),
                        ),
                    ))
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap(px(8.))
                            .child(div().flex_1().child(caption("Annotations become part of the picture.")))
                            .child(primary_button(
                                "backdrop-apply",
                                "Apply",
                                cx.listener(|this, _, _, cx| this.apply_backdrop(cx)),
                            )),
                    )
                    .into_any_element()
            }
            Popup::Resize => {
                let (w, h) = (self.doc.base.width() as f32, self.doc.base.height() as f32);
                let mut options: Vec<(SharedString, f32)> = Vec::new();
                if self.scale > 1.01 {
                    options.push((
                        format!("Screen size (@1x) — {} × {}", (w / self.scale).round(), (h / self.scale).round())
                            .into(),
                        1.0 / self.scale,
                    ));
                }
                for (label, factor) in [("75%", 0.75f32), ("50%", 0.5), ("25%", 0.25), ("200%", 2.0)] {
                    options.push((
                        format!("{label} — {} × {}", (w * factor).round(), (h * factor).round()).into(),
                        factor,
                    ));
                }
                card.child(heading("Resize"))
                    .child(div().flex().flex_col().gap(px(5.)).children(options.into_iter().enumerate().map(
                        |(i, (label, factor))| {
                            swallow_mouse_down(
                                div()
                                    .id(("resize-option", i))
                                    .px(px(9.))
                                    .py(px(6.))
                                    .rounded(px(6.))
                                    .text_size(px(11.5))
                                    .bg(theme::control())
                                    .cursor_pointer()
                                    .hover(|s| s.bg(theme::control_hover()))
                                    .child(label)
                                    .on_click(cx.listener(move |this, _, _, cx| this.resize_to(factor, cx))),
                            )
                        },
                    )))
                    .child(caption("Annotations become part of the picture."))
                    .into_any_element()
            }
        })
    }

    /// Shown until the first annotation: what the keys are.
    fn render_hint(&self) -> Option<AnyElement> {
        if !self.doc.annots.is_empty() || self.marquee.is_some() || self.hover.is_some() {
            return None;
        }
        let (_, h) = self.image_size();
        // Only when there is room under the picture.
        (f32::from(self.canvas.size.height) - h * self.zoom() > 2.0 * FIT_MARGIN + 30.0).then(|| {
            div()
                .absolute()
                .left_0()
                .right_0()
                .bottom(px(10.))
                .flex()
                .justify_center()
                .text_size(px(11.))
                .text_color(theme::faint())
                .child("Pick a tool and drag on the picture · Esc copies and closes")
                .into_any_element()
        })
    }
}

impl Focusable for Editor {
    fn focus_handle(&self, _: &App) -> gpui::FocusHandle {
        self.focus.clone()
    }
}

impl Render for Editor {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.sync(window);
        div()
            .id("editor")
            .key_context("SnapEditor")
            .track_focus(&self.focus)
            .on_key_down(cx.listener(Self::key_down))
            .size_full()
            .flex()
            .flex_col()
            .bg(theme::void())
            .font_family(theme::ui_family())
            .text_color(theme::read())
            .text_size(px(12.))
            .child(self.render_toolbar(cx))
            .child(
                div()
                    .relative()
                    .flex_1()
                    .min_h_0()
                    .overflow_hidden()
                    .child(self.render_canvas(cx))
                    .children(self.render_hint())
                    .children(self.render_ruler_label())
                    .children(self.render_marquee_bar(cx))
                    .children(self.render_popup(cx)),
            )
            .child(self.render_status(cx))
    }
}
