//! Pixels: the effects that rewrite part of the screenshot (blur, pixelate,
//! erase, highlight, magnify), the bitmap tiles for text and counters, and
//! the exporter that flattens a document into one image with tiny-skia.

use anyhow::{Result, anyhow};
use image::{RgbaImage, imageops};
use tiny_skia::{
    FillRule, FilterQuality, GradientStop, IntSize, LineCap, LineJoin, LinearGradient, PathBuilder, Pattern, Pixmap,
    PixmapPaint, Rect, SpreadMode, Stroke, Transform,
};

use super::annot::{Annot, Color, FillMode, Kind, Paint, Pt, RectF, Seg, Shape, contrast, rounded_rect, text_padding};
use super::text;

/// A bitmap that belongs at `(x, y)` in the image. Straight (not
/// premultiplied) RGBA.
pub struct Tile {
    pub image: RgbaImage,
    pub x: i32,
    pub y: i32,
}

/// How dark everything outside a spotlight gets.
pub const SPOTLIGHT_DIM: Color = [0, 0, 0, 150];

/* ---------- pixmap plumbing ---------- */

pub fn to_pixmap(image: &RgbaImage) -> Option<Pixmap> {
    let mut data = image.as_raw().clone();
    for px in data.as_chunks_mut::<4>().0 {
        let a = px[3] as u32;
        if a != 255 {
            px[0] = ((px[0] as u32 * a + 127) / 255) as u8;
            px[1] = ((px[1] as u32 * a + 127) / 255) as u8;
            px[2] = ((px[2] as u32 * a + 127) / 255) as u8;
        }
    }
    Pixmap::from_vec(data, IntSize::from_wh(image.width(), image.height())?)
}

/// Back to straight alpha. Fully transparent pixels take `bleed`'s colour so
/// that a scaled tile has no dark fringe.
pub fn from_pixmap(pixmap: Pixmap, bleed: Option<Color>) -> RgbaImage {
    let (w, h) = (pixmap.width(), pixmap.height());
    let mut data = pixmap.take();
    for px in data.as_chunks_mut::<4>().0 {
        let a = px[3] as u32;
        if a == 0 {
            if let Some(c) = bleed {
                px[0] = c[0];
                px[1] = c[1];
                px[2] = c[2];
            }
        } else if a != 255 {
            px[0] = ((px[0] as u32 * 255 + a / 2) / a).min(255) as u8;
            px[1] = ((px[1] as u32 * 255 + a / 2) / a).min(255) as u8;
            px[2] = ((px[2] as u32 * 255 + a / 2) / a).min(255) as u8;
        }
    }
    RgbaImage::from_raw(w, h, data).expect("pixmap data matches its size")
}

fn paint_of(color: Color) -> tiny_skia::Paint<'static> {
    let mut paint = tiny_skia::Paint::default();
    paint.set_color_rgba8(color[0], color[1], color[2], color[3]);
    paint.anti_alias = true;
    paint
}

fn round_stroke(width: f32) -> Stroke {
    Stroke { width, line_cap: LineCap::Round, line_join: LineJoin::Round, ..Default::default() }
}

pub fn path_of(segs: &[Seg]) -> Option<tiny_skia::Path> {
    let mut b = PathBuilder::new();
    for seg in segs {
        match *seg {
            Seg::Move(p) => b.move_to(p.x, p.y),
            Seg::Line(p) => b.line_to(p.x, p.y),
            Seg::Quad(c, p) => b.quad_to(c.x, c.y, p.x, p.y),
            Seg::Cubic(c1, c2, p) => b.cubic_to(c1.x, c1.y, c2.x, c2.y, p.x, p.y),
            Seg::Close => b.close(),
        }
    }
    b.finish()
}

fn draw_shape(pixmap: &mut Pixmap, shape: &Shape, transform: Transform) {
    let Some(path) = path_of(&shape.segs) else { return };
    match shape.paint {
        Paint::Fill(color) => pixmap.fill_path(&path, &paint_of(color), FillRule::Winding, transform, None),
        Paint::Stroke { color, width } => {
            pixmap.stroke_path(&path, &paint_of(color), &round_stroke(width), transform, None)
        }
    }
}

/* ---------- effects ---------- */

fn crop(base: &RgbaImage, rect: RectF) -> Option<(RgbaImage, u32, u32)> {
    let (x, y, w, h) = rect.pixels(base.width(), base.height())?;
    Some((imageops::crop_imm(base, x, y, w, h).to_image(), x, y))
}

/// Blur by shrinking, blurring the small copy and growing it back: the same
/// look as a wide Gaussian at a fraction of the work.
fn blur(base: &RgbaImage, rect: RectF, strength: f32) -> Option<Tile> {
    let (region, x, y) = crop(base, rect)?;
    let (w, h) = region.dimensions();
    let shrink = 4u32;
    let small = imageops::thumbnail(&region, (w / shrink).max(1), (h / shrink).max(1));
    let blurred = imageops::fast_blur(&small, (strength * 0.9).clamp(2.0, 12.0));
    let image = imageops::resize(&blurred, w, h, imageops::FilterType::Triangle);
    Some(Tile { image, x: x as i32, y: y as i32 })
}

fn pixelate(base: &RgbaImage, rect: RectF, strength: f32) -> Option<Tile> {
    let (mut region, x, y) = crop(base, rect)?;
    let (w, h) = region.dimensions();
    let block = (strength * 3.0).round().clamp(6.0, 48.0) as u32;
    for by in (0..h).step_by(block as usize) {
        for bx in (0..w).step_by(block as usize) {
            let (bw, bh) = (block.min(w - bx), block.min(h - by));
            let mut sum = [0u64; 4];
            for yy in by..by + bh {
                for xx in bx..bx + bw {
                    let p = region.get_pixel(xx, yy).0;
                    for c in 0..4 {
                        sum[c] += p[c] as u64;
                    }
                }
            }
            let n = (bw * bh) as u64;
            let avg = image::Rgba([(sum[0] / n) as u8, (sum[1] / n) as u8, (sum[2] / n) as u8, (sum[3] / n) as u8]);
            for yy in by..by + bh {
                for xx in bx..bx + bw {
                    region.put_pixel(xx, yy, avg);
                }
            }
        }
    }
    Some(Tile { image: region, x: x as i32, y: y as i32 })
}

/// Paint over a region with what surrounds it: every pixel is a blend of the
/// colours just outside the four edges, so a flat or gently shaded
/// background closes over the hole.
fn erase(base: &RgbaImage, rect: RectF) -> Option<Tile> {
    let (x, y, w, h) = rect.pixels(base.width(), base.height())?;
    let (bw, bh) = (base.width(), base.height());
    let px = |xx: i64, yy: i64| -> [f32; 4] {
        let p = base.get_pixel(xx.clamp(0, bw as i64 - 1) as u32, yy.clamp(0, bh as i64 - 1) as u32).0;
        [p[0] as f32, p[1] as f32, p[2] as f32, p[3] as f32]
    };
    // An edge sample is the average of a few pixels around it, which keeps a
    // single stray pixel from drawing a streak across the patch.
    let smooth = |line: Vec<[f32; 4]>| -> Vec<[f32; 4]> {
        let n = line.len() as i64;
        (0..n)
            .map(|i| {
                let mut acc = [0f32; 4];
                let mut count = 0f32;
                for j in (i - 3).max(0)..=(i + 3).min(n - 1) {
                    for c in 0..4 {
                        acc[c] += line[j as usize][c];
                    }
                    count += 1.0;
                }
                acc.map(|v| v / count)
            })
            .collect()
    };
    let (x0, y0, x1, y1) = (x as i64, y as i64, (x + w) as i64, (y + h) as i64);
    let top = smooth((x0..x1).map(|xx| px(xx, y0 - 2)).collect());
    let bottom = smooth((x0..x1).map(|xx| px(xx, y1 + 1)).collect());
    let left = smooth((y0..y1).map(|yy| px(x0 - 2, yy)).collect());
    let right = smooth((y0..y1).map(|yy| px(x1 + 1, yy)).collect());

    let mut image = RgbaImage::new(w, h);
    for v in 0..h {
        let ty = (v as f32 + 0.5) / h as f32;
        for u in 0..w {
            let tx = (u as f32 + 0.5) / w as f32;
            // The nearer an edge, the more it counts.
            let (wl, wr, wt, wb) = (1.0 / (tx + 0.02), 1.0 / (1.02 - tx), 1.0 / (ty + 0.02), 1.0 / (1.02 - ty));
            let total = wl + wr + wt + wb;
            let mut out = [0u8; 4];
            for c in 0..4 {
                let value = (left[v as usize][c] * wl
                    + right[v as usize][c] * wr
                    + top[u as usize][c] * wt
                    + bottom[u as usize][c] * wb)
                    / total;
                out[c] = value.round().clamp(0.0, 255.0) as u8;
            }
            image.put_pixel(u, v, image::Rgba(out));
        }
    }
    Some(Tile { image, x: x as i32, y: y as i32 })
}

/// A marker: the region multiplied by a light tint, so dark text stays dark.
fn highlight(base: &RgbaImage, rect: RectF, color: Color) -> Option<Tile> {
    let (mut region, x, y) = crop(base, rect)?;
    let tint = [0, 1, 2].map(|c| color[c] as u32 + (255 - color[c] as u32) * 30 / 100);
    for p in region.pixels_mut() {
        for c in 0..3 {
            p.0[c] = (p.0[c] as u32 * tint[c] / 255) as u8;
        }
    }
    Some(Tile { image: region, x: x as i32, y: y as i32 })
}

/// A loupe: the middle of the region at twice its size, in a white frame.
fn magnify(base: &RgbaImage, rect: RectF) -> Option<Tile> {
    let (x, y, w, h) = rect.pixels(base.width(), base.height())?;
    if w < 12 || h < 12 {
        return None;
    }
    let source = to_pixmap(&imageops::crop_imm(base, x, y, w, h).to_image())?;
    let mut pixmap = Pixmap::new(w, h)?;
    const ZOOM: f32 = 2.0;
    // Scale about the centre so the loupe shows what is under its middle.
    let transform = Transform::from_translate(-(w as f32) / 2.0, -(h as f32) / 2.0)
        .post_scale(ZOOM, ZOOM)
        .post_translate(w as f32 / 2.0, h as f32 / 2.0);
    let frame = RectF::new(1.5, 1.5, w as f32 - 3.0, h as f32 - 3.0);
    let radius = (w.min(h) as f32 * 0.12).min(18.0);
    let path = path_of(&rounded_rect(frame, radius))?;
    let mut paint = tiny_skia::Paint::default();
    paint.anti_alias = true;
    paint.shader = Pattern::new(source.as_ref(), SpreadMode::Pad, FilterQuality::Bicubic, 1.0, transform);
    pixmap.fill_path(&path, &paint, FillRule::Winding, Transform::identity(), None);
    pixmap.stroke_path(&path, &paint_of([255, 255, 255, 255]), &round_stroke(3.0), Transform::identity(), None);
    let outer = path_of(&rounded_rect(frame.inflate(1.5), radius + 1.5))?;
    pixmap.stroke_path(&outer, &paint_of([0, 0, 0, 90]), &round_stroke(1.0), Transform::identity(), None);
    Some(Tile { image: from_pixmap(pixmap, Some([255, 255, 255, 255])), x: x as i32, y: y as i32 })
}

/* ---------- text and counters ---------- */

fn text_tile(annot: &Annot) -> Option<Tile> {
    let shown = if annot.text.is_empty() { " " } else { annot.text.as_str() };
    let layout = text::layout(shown, annot.width)?;
    let pad = text_padding(annot);
    // Room for the outline stroke and anti-aliasing beyond the plate.
    let margin = (annot.width * 0.12).ceil() + 2.0;
    let w = (layout.width.max(annot.width * 0.3) + 2.0 * (pad + margin)).ceil() as u32;
    let h = (layout.height + 2.0 * (pad + margin)).ceil() as u32;
    let mut pixmap = Pixmap::new(w.max(1), h.max(1))?;
    let ink = match annot.fill {
        FillMode::Outline => annot.color,
        FillMode::Soft => [255, 255, 255, 255],
        FillMode::Solid => contrast(annot.color),
    };
    let mut bleed = contrast(annot.color);
    if annot.fill != FillMode::Outline {
        let plate = RectF::new(margin, margin, w as f32 - 2.0 * margin, h as f32 - 2.0 * margin);
        let fill = if annot.fill == FillMode::Solid { annot.color } else { [20, 18, 26, 200] };
        bleed = fill;
        if let Some(path) = path_of(&rounded_rect(plate, (annot.width * 0.28).max(3.0))) {
            pixmap.fill_path(&path, &paint_of(fill), FillRule::Winding, Transform::identity(), None);
        }
    }
    if let Some(path) = text::outline(&layout, pad + margin, pad + margin) {
        if annot.fill == FillMode::Outline {
            // A thin halo in the opposite tone keeps the label readable on
            // any background.
            let halo = round_stroke((annot.width * 0.16).max(2.0));
            pixmap.stroke_path(&path, &paint_of(contrast(annot.color)), &halo, Transform::identity(), None);
        }
        pixmap.fill_path(&path, &paint_of(ink), FillRule::Winding, Transform::identity(), None);
    }
    let offset = pad + margin;
    Some(Tile {
        image: from_pixmap(pixmap, Some(bleed)),
        x: (annot.a.x - offset).round() as i32,
        y: (annot.a.y - offset).round() as i32,
    })
}

fn counter_tile(annot: &Annot) -> Option<Tile> {
    let r = annot.counter_radius();
    let size = (2.0 * r + 6.0).ceil();
    let mut pixmap = Pixmap::new(size as u32, size as u32)?;
    let c = size / 2.0;
    let disc = PathBuilder::from_circle(c, c, r - 1.0)?;
    pixmap.fill_path(&disc, &paint_of(annot.color), FillRule::Winding, Transform::identity(), None);
    pixmap.stroke_path(&disc, &paint_of([255, 255, 255, 235]), &round_stroke(2.0), Transform::identity(), None);
    let label = annot.number.to_string();
    let font_size = r * if label.len() > 2 { 0.85 } else { 1.15 };
    if let Some(layout) = text::layout(&label, font_size) {
        // Centred on the digits themselves, not on the line box.
        if let Some(path) = text::outline(&layout, 0.0, 0.0) {
            let b = path.bounds();
            let dx = c - (b.left() + b.width() / 2.0);
            let dy = c - (b.top() + b.height() / 2.0);
            pixmap.fill_path(
                &path,
                &paint_of(contrast(annot.color)),
                FillRule::Winding,
                Transform::from_translate(dx, dy),
                None,
            );
        }
    }
    Some(Tile {
        image: from_pixmap(pixmap, Some([255, 255, 255, 255])),
        x: (annot.a.x - c).round() as i32,
        y: (annot.a.y - c).round() as i32,
    })
}

/// The bitmap for a tile annotation, cut from or laid over `base`.
pub fn tile(annot: &Annot, base: &RgbaImage) -> Option<Tile> {
    match annot.kind {
        Kind::Blur => blur(base, annot.rect(), annot.width),
        Kind::Pixelate => pixelate(base, annot.rect(), annot.width),
        Kind::Erase => erase(base, annot.rect()),
        Kind::Highlight => highlight(base, annot.rect(), annot.color),
        Kind::Magnify => magnify(base, annot.rect()),
        Kind::Text => text_tile(annot),
        Kind::Counter => counter_tile(annot),
        _ => None,
    }
}

/* ---------- spotlight ---------- */

/// The rectangles to darken so that only the spotlights stay lit: the image
/// minus the union of `spots`, cut into non-overlapping pieces.
pub fn dim_rects(spots: &[RectF], width: f32, height: f32) -> Vec<RectF> {
    let image = RectF::new(0.0, 0.0, width, height);
    let spots: Vec<RectF> = spots.iter().filter_map(|s| s.intersect(&image)).collect();
    if spots.is_empty() {
        return Vec::new();
    }
    let mut ys: Vec<f32> = vec![0.0, height];
    for s in &spots {
        ys.push(s.y);
        ys.push(s.bottom());
    }
    ys.sort_by(f32::total_cmp);
    ys.dedup();
    let mut out = Vec::new();
    for band in ys.windows(2) {
        let (y0, y1) = (band[0], band[1]);
        let mut lit: Vec<(f32, f32)> =
            spots.iter().filter(|s| s.y <= y0 && s.bottom() >= y1).map(|s| (s.x, s.right())).collect();
        lit.sort_by(|a, b| a.0.total_cmp(&b.0));
        let mut x = 0.0;
        for (from, to) in lit {
            if from > x {
                out.push(RectF::new(x, y0, from - x, y1 - y0));
            }
            x = x.max(to);
        }
        if x < width {
            out.push(RectF::new(x, y0, width - x, y1 - y0));
        }
    }
    out
}

/* ---------- export ---------- */

/// The document as one image: effects first, then the spotlight's shade,
/// then shapes, text and counters in the order they were made.
pub fn flatten(base: &RgbaImage, annots: &[Annot]) -> RgbaImage {
    let Some(mut pixmap) = to_pixmap(base) else { return base.clone() };
    let put = |pixmap: &mut Pixmap, tile: Tile| {
        if let Some(src) = to_pixmap(&tile.image) {
            pixmap.draw_pixmap(tile.x, tile.y, src.as_ref(), &PixmapPaint::default(), Transform::identity(), None);
        }
    };
    for annot in annots.iter().filter(|a| a.kind.is_effect()) {
        if let Some(tile) = tile(annot, base) {
            put(&mut pixmap, tile);
        }
    }
    let spots: Vec<RectF> = annots.iter().filter(|a| a.kind == Kind::Spotlight).map(|a| a.rect()).collect();
    for r in dim_rects(&spots, base.width() as f32, base.height() as f32) {
        if let Some(rect) = Rect::from_xywh(r.x, r.y, r.w, r.h) {
            let mut paint = paint_of(SPOTLIGHT_DIM);
            paint.anti_alias = false;
            pixmap.fill_rect(rect, &paint, Transform::identity(), None);
        }
    }
    for annot in annots.iter().filter(|a| !a.kind.is_effect() && a.kind != Kind::Spotlight) {
        if annot.kind.is_tile() {
            if let Some(tile) = tile(annot, base) {
                put(&mut pixmap, tile);
            }
        } else {
            for shape in annot.shapes() {
                draw_shape(&mut pixmap, &shape, Transform::identity());
            }
        }
    }
    from_pixmap(pixmap, None)
}

/* ---------- backdrop ---------- */

/// A frame around the screenshot: padding filled with a gradient, rounded
/// corners and a drop shadow.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Backdrop {
    /// Padding as a fraction of the image's longer side.
    pub padding: f32,
    /// Corner radius as a fraction of the image's shorter side.
    pub radius: f32,
    pub shadow: bool,
    pub from: Color,
    pub to: Color,
}

/// Gradients to pick from, as (name, from, to).
pub const BACKDROPS: [(&str, Color, Color); 8] = [
    ("Dusk", [0x4b, 0x3f, 0x9e, 255], [0xe0, 0x7a, 0x8c, 255]),
    ("Ocean", [0x1f, 0x6f, 0xd6, 255], [0x4f, 0xd3, 0xc4, 255]),
    ("Ember", [0xf2, 0x70, 0x3a, 255], [0xf7, 0xc1, 0x4e, 255]),
    ("Mint", [0x2f, 0xa8, 0x7b, 255], [0xc8, 0xf0, 0x9a, 255]),
    ("Orchid", [0x8a, 0x3f, 0xd1, 255], [0xf0, 0x8a, 0xd8, 255]),
    ("Slate", [0x2b, 0x30, 0x3d, 255], [0x6c, 0x76, 0x8c, 255]),
    ("Paper", [0xf4, 0xf1, 0xea, 255], [0xdd, 0xd8, 0xcc, 255]),
    ("Ink", [0x12, 0x11, 0x18, 255], [0x12, 0x11, 0x18, 255]),
];

pub fn apply_backdrop(image: &RgbaImage, backdrop: &Backdrop) -> Result<RgbaImage> {
    let (w, h) = (image.width() as f32, image.height() as f32);
    let pad = (w.max(h) * backdrop.padding).round().max(0.0);
    let radius = w.min(h) * backdrop.radius;
    let (cw, ch) = ((w + 2.0 * pad) as u32, (h + 2.0 * pad) as u32);
    let mut canvas = Pixmap::new(cw.max(1), ch.max(1)).ok_or_else(|| anyhow!("backdrop is too large"))?;

    let stop = |c: Color| tiny_skia::Color::from_rgba8(c[0], c[1], c[2], c[3]);
    let mut fill = tiny_skia::Paint::default();
    fill.shader = LinearGradient::new(
        tiny_skia::Point::from_xy(0.0, 0.0),
        tiny_skia::Point::from_xy(cw as f32, ch as f32),
        vec![GradientStop::new(0.0, stop(backdrop.from)), GradientStop::new(1.0, stop(backdrop.to))],
        SpreadMode::Pad,
        Transform::identity(),
    )
    .unwrap_or(tiny_skia::Shader::SolidColor(stop(backdrop.from)));
    if let Some(all) = Rect::from_xywh(0.0, 0.0, cw as f32, ch as f32) {
        canvas.fill_rect(all, &fill, Transform::identity(), None);
    }

    let card = RectF::new(pad, pad, w, h);
    let card_path = path_of(&rounded_rect(card, radius)).ok_or_else(|| anyhow!("empty image"))?;
    if backdrop.shadow && pad >= 4.0 {
        // The card's silhouette, blurred and nudged down.
        let reach = (pad * 0.35).clamp(4.0, 60.0);
        let mut shade = Pixmap::new(cw, ch).ok_or_else(|| anyhow!("backdrop is too large"))?;
        shade.fill_path(
            &card_path,
            &paint_of([0, 0, 0, 120]),
            FillRule::Winding,
            Transform::from_translate(0.0, reach * 0.45),
            None,
        );
        let shade = from_pixmap(shade, Some([0, 0, 0, 0]));
        let shrink = 4u32;
        let small = imageops::thumbnail(&shade, (cw / shrink).max(1), (ch / shrink).max(1));
        let soft = imageops::fast_blur(&small, (reach / shrink as f32).max(1.0));
        let soft = imageops::resize(&soft, cw, ch, imageops::FilterType::Triangle);
        if let Some(soft) = to_pixmap(&soft) {
            canvas.draw_pixmap(0, 0, soft.as_ref(), &PixmapPaint::default(), Transform::identity(), None);
        }
    }

    let source = to_pixmap(image).ok_or_else(|| anyhow!("empty image"))?;
    let mut paint = tiny_skia::Paint::default();
    paint.anti_alias = true;
    paint.shader = Pattern::new(
        source.as_ref(),
        SpreadMode::Pad,
        FilterQuality::Nearest,
        1.0,
        Transform::from_translate(pad, pad),
    );
    canvas.fill_path(&card_path, &paint, FillRule::Winding, Transform::identity(), None);
    Ok(from_pixmap(canvas, None))
}

/* ---------- files ---------- */

/// Looks like a photograph: nearly every sampled pixel is its own colour.
fn photographic(image: &RgbaImage) -> bool {
    let (w, h) = image.dimensions();
    let step = ((w as u64 * h as u64 / 4096) as f64).sqrt().max(1.0) as u32;
    let mut seen = std::collections::HashSet::new();
    let mut total = 0usize;
    for y in (0..h).step_by(step as usize) {
        for x in (0..w).step_by(step as usize) {
            let p = image.get_pixel(x, y).0;
            if p[3] != 255 {
                return false;
            }
            // Drop the low bits so sensor noise is not what decides it.
            seen.insert([p[0] >> 2, p[1] >> 2, p[2] >> 2]);
            total += 1;
        }
    }
    total > 256 && seen.len() * 100 > total * 55
}

/// Encode for saving. `format` is "png", "jpg" or "auto"; returns the bytes
/// and the file extension used.
pub fn encode(image: &RgbaImage, format: &str) -> Result<(Vec<u8>, &'static str)> {
    let jpeg = match format {
        "jpg" | "jpeg" => true,
        "auto" => photographic(image),
        _ => false,
    };
    let mut out = Vec::new();
    if jpeg {
        // JPEG has no alpha: flatten onto white.
        let mut rgb = image::RgbImage::new(image.width(), image.height());
        for (dst, src) in rgb.pixels_mut().zip(image.pixels()) {
            let a = src.0[3] as u32;
            for c in 0..3 {
                dst.0[c] = ((src.0[c] as u32 * a + 255 * (255 - a)) / 255) as u8;
            }
        }
        let encoder = image::codecs::jpeg::JpegEncoder::new_with_quality(&mut out, 90);
        rgb.write_with_encoder(encoder)?;
        Ok((out, "jpg"))
    } else {
        image.write_to(&mut std::io::Cursor::new(&mut out), image::ImageFormat::Png)?;
        Ok((out, "png"))
    }
}

/// The colour at a pixel, or the average over a rectangle.
pub fn average_color(image: &RgbaImage, rect: RectF) -> Option<Color> {
    let (x, y, w, h) = rect.pixels(image.width(), image.height())?;
    let mut sum = [0u64; 4];
    for yy in y..y + h {
        for xx in x..x + w {
            let p = image.get_pixel(xx, yy).0;
            for c in 0..4 {
                sum[c] += p[c] as u64;
            }
        }
    }
    let n = (w as u64 * h as u64).max(1);
    Some([(sum[0] / n) as u8, (sum[1] / n) as u8, (sum[2] / n) as u8, (sum[3] / n) as u8])
}

pub fn pixel(image: &RgbaImage, p: Pt) -> Option<Color> {
    if p.x < 0.0 || p.y < 0.0 {
        return None;
    }
    let (x, y) = (p.x as u32, p.y as u32);
    (x < image.width() && y < image.height()).then(|| image.get_pixel(x, y).0)
}

/// How far the colour under `p` runs in each direction before it changes:
/// `(left, right, up, down)` in pixels, `p`'s own pixel not counted. This is
/// the ruler — it measures the gap, the margin or the box the pointer is in.
pub fn measure(image: &RgbaImage, p: Pt) -> Option<(u32, u32, u32, u32)> {
    let origin = pixel(image, p)?;
    let (x, y) = (p.x as i64, p.y as i64);
    let same = |xx: i64, yy: i64| {
        if xx < 0 || yy < 0 || xx >= image.width() as i64 || yy >= image.height() as i64 {
            return false;
        }
        let q = image.get_pixel(xx as u32, yy as u32).0;
        (0..3).all(|c| (q[c] as i32 - origin[c] as i32).abs() <= 10)
    };
    let run = |dx: i64, dy: i64| {
        let mut n = 0u32;
        while same(x + dx * (n as i64 + 1), y + dy * (n as i64 + 1)) {
            n += 1;
        }
        n
    };
    Some((run(-1, 0), run(1, 0), run(0, -1), run(0, 1)))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn solid(w: u32, h: u32, c: Color) -> RgbaImage {
        RgbaImage::from_pixel(w, h, image::Rgba(c))
    }

    fn annot(kind: Kind, r: RectF) -> Annot {
        let mut a = Annot::new(1, kind, Pt::new(r.x, r.y), [255, 0, 0, 255], 4.0);
        a.b = Pt::new(r.right(), r.bottom());
        a
    }

    #[test]
    fn premultiply_round_trips() {
        let mut img = solid(4, 4, [200, 100, 50, 255]);
        img.put_pixel(1, 1, image::Rgba([200, 100, 50, 128]));
        img.put_pixel(2, 2, image::Rgba([0, 0, 0, 0]));
        let back = from_pixmap(to_pixmap(&img).unwrap(), None);
        assert_eq!(back.get_pixel(0, 0).0, [200, 100, 50, 255]);
        let p = back.get_pixel(1, 1).0;
        assert!((p[0] as i32 - 200).abs() <= 2 && p[3] == 128, "{p:?}");
        assert_eq!(back.get_pixel(2, 2).0[3], 0);
    }

    #[test]
    fn a_rectangle_is_drawn_where_it_was_placed() {
        let base = solid(100, 80, [255, 255, 255, 255]);
        let mut r = annot(Kind::Rect, RectF::new(20.0, 20.0, 40.0, 30.0));
        r.fill = FillMode::Solid;
        let out = flatten(&base, &[r]);
        assert_eq!(out.get_pixel(40, 35).0, [255, 0, 0, 255]);
        assert_eq!(out.get_pixel(5, 5).0, [255, 255, 255, 255]);
        assert_eq!(out.dimensions(), (100, 80));
    }

    #[test]
    fn erase_closes_over_a_mark_on_a_flat_background() {
        let mut base = solid(120, 80, [240, 240, 240, 255]);
        for y in 30..50 {
            for x in 40..80 {
                base.put_pixel(x, y, image::Rgba([10, 10, 10, 255]));
            }
        }
        let out = flatten(&base, &[annot(Kind::Erase, RectF::new(35.0, 25.0, 50.0, 30.0))]);
        assert_eq!(out.get_pixel(60, 40).0, [240, 240, 240, 255]);
    }

    #[test]
    fn pixelate_and_blur_keep_the_region_size_and_change_it() {
        let mut base = solid(96, 96, [255, 255, 255, 255]);
        for y in 0..96 {
            for x in 0..96 {
                if (x / 4 + y / 4) % 2 == 0 {
                    base.put_pixel(x, y, image::Rgba([0, 0, 0, 255]));
                }
            }
        }
        for kind in [Kind::Pixelate, Kind::Blur] {
            let t = tile(&annot(kind, RectF::new(16.0, 16.0, 64.0, 48.0)), &base).unwrap();
            assert_eq!((t.x, t.y, t.image.width(), t.image.height()), (16, 16, 64, 48), "{kind:?}");
            let p = t.image.get_pixel(30, 20).0;
            assert!(p[0] > 60 && p[0] < 200, "{kind:?} should have mixed the checkerboard: {p:?}");
        }
    }

    #[test]
    fn highlight_tints_light_pixels_and_keeps_dark_ones() {
        let mut base = solid(40, 20, [255, 255, 255, 255]);
        base.put_pixel(5, 5, image::Rgba([0, 0, 0, 255]));
        let mut h = annot(Kind::Highlight, RectF::new(0.0, 0.0, 40.0, 20.0));
        h.color = [255, 235, 0, 255];
        let out = flatten(&base, &[h]);
        let lit = out.get_pixel(20, 10).0;
        assert!(lit[0] > 240 && lit[2] < 120, "{lit:?}");
        assert_eq!(out.get_pixel(5, 5).0, [0, 0, 0, 255]);
    }

    #[test]
    fn spotlights_darken_everything_else_once() {
        let rects = dim_rects(&[RectF::new(10.0, 10.0, 30.0, 30.0), RectF::new(20.0, 20.0, 40.0, 40.0)], 100.0, 100.0);
        let inside =
            |p: Pt| rects.iter().filter(|r| p.x >= r.x && p.x < r.right() && p.y >= r.y && p.y < r.bottom()).count();
        assert_eq!(inside(Pt::new(15.0, 15.0)), 0, "inside the first spotlight");
        assert_eq!(inside(Pt::new(50.0, 50.0)), 0, "inside the second");
        assert_eq!(inside(Pt::new(25.0, 25.0)), 0, "inside both");
        assert_eq!(inside(Pt::new(5.0, 5.0)), 1, "outside: dimmed exactly once");
        assert_eq!(inside(Pt::new(50.0, 15.0)), 1);
        assert_eq!(inside(Pt::new(90.0, 90.0)), 1);
        assert!(dim_rects(&[], 100.0, 100.0).is_empty(), "no spotlight, no shade");

        let base = solid(100, 100, [200, 200, 200, 255]);
        let out = flatten(&base, &[annot(Kind::Spotlight, RectF::new(10.0, 10.0, 30.0, 30.0))]);
        assert_eq!(out.get_pixel(20, 20).0, [200, 200, 200, 255]);
        assert!(out.get_pixel(80, 80).0[0] < 100);
    }

    #[test]
    fn text_and_counters_make_tiles() {
        if !text::available() {
            return;
        }
        let base = solid(300, 200, [255, 255, 255, 255]);
        let mut t = Annot::new(1, Kind::Text, Pt::new(40.0, 40.0), [255, 0, 0, 255], 32.0);
        t.text = "Hi".into();
        let tile_t = tile(&t, &base).unwrap();
        assert!(tile_t.image.pixels().any(|p| p.0[3] == 255 && p.0[0] > 200 && p.0[1] < 60), "red glyph pixels");
        let mut c = Annot::new(2, Kind::Counter, Pt::new(150.0, 100.0), [0, 90, 255, 255], 4.0);
        c.number = 3;
        let out = flatten(&base, &[t, c]);
        assert_eq!(out.get_pixel(150, 100 - 14).0, [0, 90, 255, 255], "the counter's disc");
        assert!(out != base);
    }

    #[test]
    fn backdrop_adds_padding_around_the_image() {
        let img = solid(200, 100, [10, 200, 10, 255]);
        let b = Backdrop { padding: 0.1, radius: 0.0, shadow: true, from: [255, 0, 0, 255], to: [255, 0, 0, 255] };
        let out = apply_backdrop(&img, &b).unwrap();
        assert_eq!(out.dimensions(), (240, 140));
        assert_eq!(out.get_pixel(2, 2).0, [255, 0, 0, 255]);
        assert_eq!(out.get_pixel(120, 70).0, [10, 200, 10, 255]);
    }

    #[test]
    fn encode_picks_a_format() {
        let flat = solid(64, 64, [30, 30, 30, 255]);
        assert_eq!(encode(&flat, "auto").unwrap().1, "png");
        assert_eq!(encode(&flat, "jpg").unwrap().1, "jpg");
        let noisy = RgbaImage::from_fn(256, 256, |x, y| {
            let v = x.wrapping_mul(2654435761).wrapping_add(y.wrapping_mul(40503)) >> 8;
            image::Rgba([v as u8, (v >> 8) as u8, (v >> 16) as u8, 255])
        });
        assert_eq!(encode(&noisy, "auto").unwrap().1, "jpg");
        let (png, _) = encode(&flat, "png").unwrap();
        assert_eq!(image::load_from_memory(&png).unwrap().to_rgba8(), flat);
    }

    #[test]
    fn the_ruler_measures_the_run_of_one_colour() {
        let mut base = solid(100, 60, [255, 255, 255, 255]);
        for y in 0..60 {
            base.put_pixel(20, y, image::Rgba([0, 0, 0, 255]));
            base.put_pixel(70, y, image::Rgba([0, 0, 0, 255]));
        }
        assert_eq!(measure(&base, Pt::new(40.0, 30.0)), Some((19, 29, 30, 29)));
        assert_eq!(average_color(&base, RectF::new(30.0, 10.0, 10.0, 10.0)), Some([255, 255, 255, 255]));
        assert_eq!(pixel(&base, Pt::new(20.5, 3.0)), Some([0, 0, 0, 255]));
        assert_eq!(pixel(&base, Pt::new(-1.0, 3.0)), None);
    }
}

#[cfg(test)]
mod gallery {
    use super::*;
    use crate::snap::annot::ArrowStyle;

    /// `cargo test gallery -- --ignored` writes every annotation onto one
    /// image, for looking at.
    #[test]
    #[ignore = "writes a picture to the temp dir"]
    fn gallery() {
        let mut base = RgbaImage::from_fn(1200, 760, |x, y| {
            let v = 236 + ((x / 40 + y / 40) % 2) as u8 * 10;
            image::Rgba([v, v, (v as u32 + 4).min(255) as u8, 255])
        });
        let mut para = Annot::new(90, Kind::Text, Pt::new(60.0, 420.0), [30, 30, 30, 255], 26.0);
        para.fill = FillMode::Solid;
        para.color = [250, 250, 250, 255];
        para.text =
            "The quick brown fox jumps over\nthe lazy dog. Secret: hunter2\nAnother line of body text here.".into();
        base = flatten(&base, &[para]);

        let red = [255, 59, 48, 255];
        let mut id = 0;
        let mut make = |kind, a: (f32, f32), b: (f32, f32)| {
            id += 1;
            let mut x = Annot::new(id, kind, Pt::new(a.0, a.1), red, 6.0);
            x.b = Pt::new(b.0, b.1);
            x
        };
        let mut all = Vec::new();
        for (i, style) in [ArrowStyle::Shape, ArrowStyle::Line, ArrowStyle::Double].into_iter().enumerate() {
            let mut a = make(Kind::Arrow, (60.0, 60.0 + 70.0 * i as f32), (330.0, 110.0 + 70.0 * i as f32));
            a.arrow = style;
            all.push(a);
        }
        for (i, fill) in [FillMode::Outline, FillMode::Soft, FillMode::Solid].into_iter().enumerate() {
            let x = 400.0 + 190.0 * i as f32;
            let mut r = make(Kind::Rect, (x, 50.0), (x + 160.0, 150.0));
            r.fill = fill;
            all.push(r);
            let mut o = make(Kind::Oval, (x, 180.0), (x + 160.0, 280.0));
            o.fill = fill;
            o.color = [0, 122, 255, 255];
            all.push(o);
            let mut t = Annot::new(100 + i as u64, Kind::Text, Pt::new(x, 310.0), red, 30.0);
            t.fill = fill;
            t.text = "Label 42".into();
            all.push(t);
        }
        all.push(make(Kind::Line, (60.0, 300.0), (330.0, 300.0)));
        let mut pen = make(Kind::Draw, (60.0, 350.0), (60.0, 350.0));
        pen.points = (0..60).map(|i| Pt::new(60.0 + i as f32 * 4.5, 350.0 + (i as f32 * 0.4).sin() * 22.0)).collect();
        all.push(pen);
        for n in 1..=3 {
            let mut c = Annot::new(200 + n, Kind::Counter, Pt::new(980.0 + 60.0 * n as f32, 90.0), red, 6.0);
            c.number = n as u32;
            all.push(c);
        }
        let mut hi = make(Kind::Highlight, (60.0, 425.0), (330.0, 455.0));
        hi.color = [255, 214, 10, 255];
        all.push(hi);
        all.push(make(Kind::Blur, (190.0, 455.0), (420.0, 490.0)));
        all.push(make(Kind::Pixelate, (60.0, 487.0), (300.0, 522.0)));
        all.push(make(Kind::Erase, (560.0, 430.0), (700.0, 520.0)));
        all.push(make(Kind::Magnify, (760.0, 400.0), (1000.0, 540.0)));
        let out = flatten(&base, &all);
        let spot = flatten(&out, &[make(Kind::Spotlight, (380.0, 30.0), (960.0, 370.0))]);
        let framed = apply_backdrop(
            &spot,
            &Backdrop { padding: 0.06, radius: 0.03, shadow: true, from: BACKDROPS[0].1, to: BACKDROPS[0].2 },
        )
        .unwrap();
        let dir = std::env::temp_dir();
        out.save(dir.join("vp-gallery.png")).unwrap();
        framed.save(dir.join("vp-gallery-framed.png")).unwrap();
        eprintln!("wrote {}", dir.join("vp-gallery.png").display());
    }
}
