//! The screenshot editor's document: annotations in image-pixel coordinates,
//! their geometry, and hit testing. Nothing here knows how it is drawn — the
//! editor paints the same [`Shape`]s with GPUI that `raster` paints with
//! tiny-skia for export.

pub type Color = [u8; 4];

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Pt {
    pub x: f32,
    pub y: f32,
}

impl Pt {
    pub const fn new(x: f32, y: f32) -> Self {
        Self { x, y }
    }
    pub fn add(self, o: Pt) -> Pt {
        Pt::new(self.x + o.x, self.y + o.y)
    }
    pub fn sub(self, o: Pt) -> Pt {
        Pt::new(self.x - o.x, self.y - o.y)
    }
    pub fn mul(self, k: f32) -> Pt {
        Pt::new(self.x * k, self.y * k)
    }
    pub fn len(self) -> f32 {
        self.x.hypot(self.y)
    }
    pub fn dist(self, o: Pt) -> f32 {
        self.sub(o).len()
    }
    pub fn mid(self, o: Pt) -> Pt {
        Pt::new((self.x + o.x) / 2.0, (self.y + o.y) / 2.0)
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct RectF {
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
}

impl RectF {
    pub const fn new(x: f32, y: f32, w: f32, h: f32) -> Self {
        Self { x, y, w, h }
    }
    pub fn from_points(a: Pt, b: Pt) -> Self {
        Self { x: a.x.min(b.x), y: a.y.min(b.y), w: (a.x - b.x).abs(), h: (a.y - b.y).abs() }
    }
    pub fn right(&self) -> f32 {
        self.x + self.w
    }
    pub fn bottom(&self) -> f32 {
        self.y + self.h
    }
    pub fn center(&self) -> Pt {
        Pt::new(self.x + self.w / 2.0, self.y + self.h / 2.0)
    }
    pub fn contains(&self, p: Pt) -> bool {
        p.x >= self.x && p.x <= self.right() && p.y >= self.y && p.y <= self.bottom()
    }
    pub fn inflate(&self, d: f32) -> Self {
        Self { x: self.x - d, y: self.y - d, w: (self.w + 2.0 * d).max(0.0), h: (self.h + 2.0 * d).max(0.0) }
    }
    pub fn union(&self, o: &RectF) -> Self {
        let x = self.x.min(o.x);
        let y = self.y.min(o.y);
        Self { x, y, w: self.right().max(o.right()) - x, h: self.bottom().max(o.bottom()) - y }
    }
    pub fn intersect(&self, o: &RectF) -> Option<Self> {
        let x = self.x.max(o.x);
        let y = self.y.max(o.y);
        let r = self.right().min(o.right());
        let b = self.bottom().min(o.bottom());
        (r > x && b > y).then_some(Self { x, y, w: r - x, h: b - y })
    }
    /// The whole pixels this rectangle covers inside a `w`×`h` image:
    /// `(x, y, width, height)`, or `None` when it covers none.
    pub fn pixels(&self, w: u32, h: u32) -> Option<(u32, u32, u32, u32)> {
        let c = self.intersect(&RectF::new(0.0, 0.0, w as f32, h as f32))?;
        let x0 = c.x.round() as u32;
        let y0 = c.y.round() as u32;
        let x1 = (c.right().round() as u32).min(w);
        let y1 = (c.bottom().round() as u32).min(h);
        (x1 > x0 && y1 > y0).then(|| (x0, y0, x1 - x0, y1 - y0))
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Kind {
    Arrow,
    Line,
    Rect,
    Oval,
    Draw,
    Text,
    Counter,
    Highlight,
    Blur,
    Pixelate,
    Erase,
    Spotlight,
    Magnify,
}

impl Kind {
    /// Rewrites the pixels under it, from the base image.
    pub fn is_effect(self) -> bool {
        matches!(self, Kind::Highlight | Kind::Blur | Kind::Pixelate | Kind::Erase | Kind::Magnify)
    }

    /// Drawn as a bitmap tile rather than as vector shapes.
    pub fn is_tile(self) -> bool {
        self.is_effect() || matches!(self, Kind::Text | Kind::Counter)
    }

    /// Defined by the rectangle between `a` and `b`.
    pub fn is_box(self) -> bool {
        matches!(self, Kind::Rect | Kind::Oval | Kind::Spotlight) || self.is_effect()
    }

    /// Defined by the segment from `a` to `b`.
    pub fn is_segment(self) -> bool {
        matches!(self, Kind::Arrow | Kind::Line)
    }
}

/// How a rectangle, an oval or a text plate is filled.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum FillMode {
    #[default]
    Outline,
    Soft,
    Solid,
}

impl FillMode {
    pub fn next(self) -> Self {
        match self {
            FillMode::Outline => FillMode::Soft,
            FillMode::Soft => FillMode::Solid,
            FillMode::Solid => FillMode::Outline,
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ArrowStyle {
    /// A tapered, filled arrow.
    #[default]
    Shape,
    /// A plain line with an open head.
    Line,
    /// Heads at both ends.
    Double,
}

impl ArrowStyle {
    pub fn next(self) -> Self {
        match self {
            ArrowStyle::Shape => ArrowStyle::Line,
            ArrowStyle::Line => ArrowStyle::Double,
            ArrowStyle::Double => ArrowStyle::Shape,
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct Annot {
    pub id: u64,
    pub kind: Kind,
    /// Start point, the first corner, or (text, counter) the anchor.
    pub a: Pt,
    pub b: Pt,
    /// The pen's path, for freehand strokes.
    pub points: Vec<Pt>,
    pub color: Color,
    /// Stroke width in image pixels; for text, the font size; for a counter,
    /// its radius.
    pub width: f32,
    pub fill: FillMode,
    pub arrow: ArrowStyle,
    pub text: String,
    pub number: u32,
}

impl Annot {
    pub fn new(id: u64, kind: Kind, at: Pt, color: Color, width: f32) -> Self {
        Self {
            id,
            kind,
            a: at,
            b: at,
            points: if kind == Kind::Draw { vec![at] } else { Vec::new() },
            color,
            width,
            fill: FillMode::Outline,
            arrow: ArrowStyle::Shape,
            text: String::new(),
            number: 0,
        }
    }

    pub fn rect(&self) -> RectF {
        RectF::from_points(self.a, self.b)
    }

    /// Too small to keep: a click that never became a drag.
    pub fn is_degenerate(&self) -> bool {
        match self.kind {
            Kind::Text => self.text.trim().is_empty(),
            Kind::Counter => false,
            Kind::Draw => self.points.len() < 2,
            k if k.is_segment() => self.a.dist(self.b) < 4.0,
            _ => {
                let r = self.rect();
                r.w < 4.0 || r.h < 4.0
            }
        }
    }

    pub fn translate(&mut self, d: Pt) {
        self.a = self.a.add(d);
        self.b = self.b.add(d);
        for p in &mut self.points {
            *p = p.add(d);
        }
    }

    /// The box around everything this annotation paints. Text needs its
    /// measured size, which the caller passes in.
    pub fn bounds(&self, text_size: Option<(f32, f32)>) -> RectF {
        match self.kind {
            Kind::Arrow | Kind::Line => RectF::from_points(self.a, self.b).inflate(self.width * 2.5 + 2.0),
            Kind::Draw => {
                let mut r = RectF::new(self.a.x, self.a.y, 0.0, 0.0);
                for p in &self.points {
                    r = r.union(&RectF::new(p.x, p.y, 0.0, 0.0));
                }
                r.inflate(self.width / 2.0 + 1.0)
            }
            Kind::Text => {
                let (w, h) = text_size.unwrap_or((self.width * 4.0, self.width * 1.3));
                let pad = text_padding(self);
                RectF::new(self.a.x - pad, self.a.y - pad, w + 2.0 * pad, h + 2.0 * pad)
            }
            Kind::Counter => {
                let r = self.counter_radius();
                RectF::new(self.a.x - r, self.a.y - r, 2.0 * r, 2.0 * r)
            }
            Kind::Rect | Kind::Oval => self.rect().inflate(self.width / 2.0),
            _ => self.rect(),
        }
    }

    pub fn counter_radius(&self) -> f32 {
        (self.width * 3.2 + 8.0).max(10.0)
    }

    /// Whether a click at `p` (within `tol` pixels) picks this annotation.
    pub fn hit(&self, p: Pt, tol: f32, text_size: Option<(f32, f32)>) -> bool {
        match self.kind {
            Kind::Arrow | Kind::Line => segment_distance(p, self.a, self.b) <= self.width * 1.2 + tol,
            Kind::Draw => self.points.windows(2).any(|w| segment_distance(p, w[0], w[1]) <= self.width / 2.0 + tol),
            Kind::Rect | Kind::Oval if self.fill == FillMode::Outline => {
                let r = self.rect();
                let reach = self.width / 2.0 + tol;
                if self.kind == Kind::Rect {
                    r.inflate(reach).contains(p)
                        && !(r.w > 2.0 * reach && r.h > 2.0 * reach && r.inflate(-reach).contains(p))
                } else {
                    // Distance to the ellipse, approximated along the ray
                    // from its centre.
                    let c = r.center();
                    let (rx, ry) = ((r.w / 2.0).max(1.0), (r.h / 2.0).max(1.0));
                    let d = Pt::new((p.x - c.x) / rx, (p.y - c.y) / ry).len();
                    ((d - 1.0) * rx.min(ry)).abs() <= reach
                }
            }
            Kind::Oval => {
                let r = self.rect();
                let c = r.center();
                Pt::new((p.x - c.x) / (r.w / 2.0).max(1.0), (p.y - c.y) / (r.h / 2.0).max(1.0)).len() <= 1.0
            }
            Kind::Counter => p.dist(self.a) <= self.counter_radius() + tol,
            _ => self.bounds(text_size).inflate(tol).contains(p),
        }
    }

    /// The points a selected annotation can be reshaped by.
    pub fn handles(&self) -> Vec<(Handle, Pt)> {
        if self.kind.is_segment() {
            vec![(Handle::A, self.a), (Handle::B, self.b)]
        } else if self.kind.is_box() {
            let r = self.rect();
            vec![
                (Handle::Corner(0), Pt::new(r.x, r.y)),
                (Handle::Corner(1), Pt::new(r.right(), r.y)),
                (Handle::Corner(2), Pt::new(r.right(), r.bottom())),
                (Handle::Corner(3), Pt::new(r.x, r.bottom())),
            ]
        } else {
            Vec::new()
        }
    }

    /// Move one handle to `p`.
    pub fn drag_handle(&mut self, handle: Handle, p: Pt) {
        match handle {
            Handle::A => self.a = p,
            Handle::B => self.b = p,
            Handle::Corner(i) => {
                let r = self.rect();
                // The opposite corner stays where it is.
                let anchor = match i {
                    0 => Pt::new(r.right(), r.bottom()),
                    1 => Pt::new(r.x, r.bottom()),
                    2 => Pt::new(r.x, r.y),
                    _ => Pt::new(r.right(), r.y),
                };
                self.a = anchor;
                self.b = p;
            }
        }
    }

    /// The vector outline of a non-tile annotation.
    pub fn shapes(&self) -> Vec<Shape> {
        let stroke = Paint::Stroke { color: self.color, width: self.width };
        match self.kind {
            Kind::Line => vec![Shape { segs: vec![Seg::Move(self.a), Seg::Line(self.b)], paint: stroke }],
            Kind::Arrow => arrow_shapes(self),
            Kind::Rect | Kind::Oval => {
                let r = self.rect();
                let segs = if self.kind == Kind::Rect {
                    rounded_rect(r, (self.width * 1.2).min(r.w / 2.0).min(r.h / 2.0))
                } else {
                    ellipse(r)
                };
                let mut out = Vec::new();
                match self.fill {
                    FillMode::Outline => {}
                    FillMode::Soft => {
                        let [red, green, blue, _] = self.color;
                        out.push(Shape { segs: segs.clone(), paint: Paint::Fill([red, green, blue, 64]) });
                    }
                    FillMode::Solid => out.push(Shape { segs: segs.clone(), paint: Paint::Fill(self.color) }),
                }
                if self.fill != FillMode::Solid {
                    out.push(Shape { segs, paint: stroke });
                }
                out
            }
            Kind::Draw => vec![Shape { segs: smooth(&self.points), paint: stroke }],
            _ => Vec::new(),
        }
    }
}

/// Space between a text's glyphs and the edge of its plate.
pub fn text_padding(annot: &Annot) -> f32 {
    if annot.fill == FillMode::Outline { 2.0 } else { (annot.width * 0.32).max(4.0) }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Handle {
    A,
    B,
    Corner(u8),
}

/* ---------- shapes ---------- */

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Seg {
    Move(Pt),
    Line(Pt),
    /// Control point, end point.
    Quad(Pt, Pt),
    /// Two control points, end point.
    Cubic(Pt, Pt, Pt),
    Close,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Paint {
    Fill(Color),
    /// Round caps and joins.
    Stroke {
        color: Color,
        width: f32,
    },
}

#[derive(Clone, Debug, PartialEq)]
pub struct Shape {
    pub segs: Vec<Seg>,
    pub paint: Paint,
}

pub fn rounded_rect(r: RectF, radius: f32) -> Vec<Seg> {
    let rad = radius.max(0.0).min(r.w / 2.0).min(r.h / 2.0);
    if rad < 0.5 {
        return vec![
            Seg::Move(Pt::new(r.x, r.y)),
            Seg::Line(Pt::new(r.right(), r.y)),
            Seg::Line(Pt::new(r.right(), r.bottom())),
            Seg::Line(Pt::new(r.x, r.bottom())),
            Seg::Close,
        ];
    }
    let (x0, y0, x1, y1) = (r.x, r.y, r.right(), r.bottom());
    vec![
        Seg::Move(Pt::new(x0 + rad, y0)),
        Seg::Line(Pt::new(x1 - rad, y0)),
        Seg::Quad(Pt::new(x1, y0), Pt::new(x1, y0 + rad)),
        Seg::Line(Pt::new(x1, y1 - rad)),
        Seg::Quad(Pt::new(x1, y1), Pt::new(x1 - rad, y1)),
        Seg::Line(Pt::new(x0 + rad, y1)),
        Seg::Quad(Pt::new(x0, y1), Pt::new(x0, y1 - rad)),
        Seg::Line(Pt::new(x0, y0 + rad)),
        Seg::Quad(Pt::new(x0, y0), Pt::new(x0 + rad, y0)),
        Seg::Close,
    ]
}

pub fn ellipse(r: RectF) -> Vec<Seg> {
    // The usual four-cubic approximation of a circle.
    const K: f32 = 0.552_284_8;
    let c = r.center();
    let (rx, ry) = (r.w / 2.0, r.h / 2.0);
    let (kx, ky) = (rx * K, ry * K);
    vec![
        Seg::Move(Pt::new(c.x + rx, c.y)),
        Seg::Cubic(Pt::new(c.x + rx, c.y + ky), Pt::new(c.x + kx, c.y + ry), Pt::new(c.x, c.y + ry)),
        Seg::Cubic(Pt::new(c.x - kx, c.y + ry), Pt::new(c.x - rx, c.y + ky), Pt::new(c.x - rx, c.y)),
        Seg::Cubic(Pt::new(c.x - rx, c.y - ky), Pt::new(c.x - kx, c.y - ry), Pt::new(c.x, c.y - ry)),
        Seg::Cubic(Pt::new(c.x + kx, c.y - ry), Pt::new(c.x + rx, c.y - ky), Pt::new(c.x + rx, c.y)),
        Seg::Close,
    ]
}

/// A pen path as a curve through the midpoints of its samples, which takes
/// the corners off a mouse's straight segments.
fn smooth(points: &[Pt]) -> Vec<Seg> {
    let Some(first) = points.first() else { return Vec::new() };
    let mut segs = vec![Seg::Move(*first)];
    if points.len() == 2 {
        segs.push(Seg::Line(points[1]));
        return segs;
    }
    for pair in points.windows(2).skip(1) {
        segs.push(Seg::Quad(pair[0], pair[0].mid(pair[1])));
    }
    if let Some(last) = points.last() {
        segs.push(Seg::Line(*last));
    }
    segs
}

fn arrow_shapes(annot: &Annot) -> Vec<Shape> {
    let (a, b, w) = (annot.a, annot.b, annot.width);
    let len = a.dist(b);
    if len < 0.5 {
        return Vec::new();
    }
    let dir = b.sub(a).mul(1.0 / len);
    let n = Pt::new(-dir.y, dir.x);
    let head_len = (w * 4.2 + 6.0).min(len * 0.62);
    let head_half = head_len * 0.52;
    let fill = Paint::Fill(annot.color);
    let head = |tip: Pt, dir: Pt| {
        let base = tip.sub(dir.mul(head_len));
        Shape {
            segs: vec![
                Seg::Move(tip),
                Seg::Line(base.add(n.mul(head_half))),
                Seg::Line(base.add(dir.mul(head_len * 0.22))),
                Seg::Line(base.sub(n.mul(head_half))),
                Seg::Close,
            ],
            paint: fill,
        }
    };
    match annot.arrow {
        ArrowStyle::Shape => {
            let base = b.sub(dir.mul(head_len));
            let neck = base.add(dir.mul(head_len * 0.16));
            let (tail, shaft) = ((w * 0.18).max(0.6), w * 0.62);
            vec![Shape {
                segs: vec![
                    Seg::Move(a.add(n.mul(tail))),
                    Seg::Line(neck.add(n.mul(shaft))),
                    Seg::Line(base.add(n.mul(head_half))),
                    Seg::Line(b),
                    Seg::Line(base.sub(n.mul(head_half))),
                    Seg::Line(neck.sub(n.mul(shaft))),
                    Seg::Line(a.sub(n.mul(tail))),
                    Seg::Close,
                ],
                paint: fill,
            }]
        }
        ArrowStyle::Line => {
            let base = b.sub(dir.mul(head_len));
            let stroke = Paint::Stroke { color: annot.color, width: w };
            vec![
                Shape { segs: vec![Seg::Move(a), Seg::Line(b)], paint: stroke },
                Shape {
                    segs: vec![
                        Seg::Move(base.add(n.mul(head_half))),
                        Seg::Line(b),
                        Seg::Line(base.sub(n.mul(head_half))),
                    ],
                    paint: stroke,
                },
            ]
        }
        ArrowStyle::Double => {
            let inset = head_len * 0.7;
            vec![
                Shape {
                    segs: vec![Seg::Move(a.add(dir.mul(inset))), Seg::Line(b.sub(dir.mul(inset)))],
                    paint: Paint::Stroke { color: annot.color, width: w },
                },
                head(b, dir),
                head(a, dir.mul(-1.0)),
            ]
        }
    }
}

pub fn segment_distance(p: Pt, a: Pt, b: Pt) -> f32 {
    let ab = b.sub(a);
    let len2 = ab.x * ab.x + ab.y * ab.y;
    if len2 < 1e-6 {
        return p.dist(a);
    }
    let t = (((p.x - a.x) * ab.x + (p.y - a.y) * ab.y) / len2).clamp(0.0, 1.0);
    p.dist(a.add(ab.mul(t)))
}

/* ---------- colours ---------- */

pub fn parse_hex(text: &str) -> Option<Color> {
    let hex = text.trim().trim_start_matches('#');
    if hex.len() != 6 {
        return None;
    }
    let n = u32::from_str_radix(hex, 16).ok()?;
    Some([(n >> 16) as u8, (n >> 8) as u8, n as u8, 255])
}

pub fn to_hex(c: Color) -> String {
    format!("#{:02X}{:02X}{:02X}", c[0], c[1], c[2])
}

/// Black or white, whichever reads on `c`.
pub fn contrast(c: Color) -> Color {
    let luma = 0.299 * c[0] as f32 + 0.587 * c[1] as f32 + 0.114 * c[2] as f32;
    if luma > 150.0 { [20, 18, 26, 255] } else { [255, 255, 255, 255] }
}

/// A colour the way the picker copies it: "hex", "rgb" or "hsl".
pub fn format_color(c: Color, format: &str) -> String {
    match format {
        "rgb" => format!("rgb({}, {}, {})", c[0], c[1], c[2]),
        "hsl" => {
            let (r, g, b) = (c[0] as f32 / 255.0, c[1] as f32 / 255.0, c[2] as f32 / 255.0);
            let (max, min) = (r.max(g).max(b), r.min(g).min(b));
            let l = (max + min) / 2.0;
            let d = max - min;
            let (h, s) = if d < 1e-6 {
                (0.0, 0.0)
            } else {
                let s = d / (1.0 - (2.0 * l - 1.0).abs());
                let h = if max == r {
                    ((g - b) / d).rem_euclid(6.0)
                } else if max == g {
                    (b - r) / d + 2.0
                } else {
                    (r - g) / d + 4.0
                };
                (h * 60.0, s)
            };
            format!("hsl({}, {}%, {}%)", h.round(), (s * 100.0).round(), (l * 100.0).round())
        }
        _ => to_hex(c),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn annot(kind: Kind, a: (f32, f32), b: (f32, f32)) -> Annot {
        let mut x = Annot::new(1, kind, Pt::new(a.0, a.1), [255, 0, 0, 255], 4.0);
        x.b = Pt::new(b.0, b.1);
        x
    }

    #[test]
    fn rect_pixels_clamp_to_the_image() {
        assert_eq!(RectF::new(-5.0, 2.0, 20.0, 4.0).pixels(10, 10), Some((0, 2, 10, 4)));
        assert_eq!(RectF::new(20.0, 2.0, 5.0, 4.0).pixels(10, 10), None);
        assert_eq!(RectF::new(1.2, 1.2, 0.2, 0.2).pixels(10, 10), None);
    }

    #[test]
    fn an_outline_is_picked_by_its_border_only() {
        let r = annot(Kind::Rect, (10.0, 10.0), (110.0, 90.0));
        assert!(r.hit(Pt::new(10.0, 50.0), 3.0, None));
        assert!(!r.hit(Pt::new(60.0, 50.0), 3.0, None), "the inside of an outline is not the outline");
        let mut filled = r.clone();
        filled.fill = FillMode::Soft;
        assert!(filled.hit(Pt::new(60.0, 50.0), 3.0, None));
        let o = annot(Kind::Oval, (0.0, 0.0), (100.0, 50.0));
        assert!(o.hit(Pt::new(100.0, 25.0), 3.0, None));
        assert!(!o.hit(Pt::new(50.0, 25.0), 3.0, None));
    }

    #[test]
    fn a_line_is_picked_along_its_length() {
        let l = annot(Kind::Line, (0.0, 0.0), (100.0, 0.0));
        assert!(l.hit(Pt::new(50.0, 5.0), 3.0, None));
        assert!(!l.hit(Pt::new(50.0, 30.0), 3.0, None));
        assert!(!l.hit(Pt::new(130.0, 0.0), 3.0, None));
    }

    #[test]
    fn dragging_a_corner_keeps_the_opposite_one() {
        let mut r = annot(Kind::Rect, (10.0, 10.0), (50.0, 40.0));
        r.drag_handle(Handle::Corner(0), Pt::new(0.0, 0.0));
        assert_eq!(r.rect(), RectF::new(0.0, 0.0, 50.0, 40.0));
        r.drag_handle(Handle::Corner(2), Pt::new(80.0, 90.0));
        assert_eq!(r.rect(), RectF::new(0.0, 0.0, 80.0, 90.0));
    }

    #[test]
    fn every_arrow_style_has_an_outline() {
        for style in [ArrowStyle::Shape, ArrowStyle::Line, ArrowStyle::Double] {
            let mut a = annot(Kind::Arrow, (0.0, 0.0), (120.0, 40.0));
            a.arrow = style;
            assert!(!a.shapes().is_empty(), "{style:?}");
        }
        assert!(annot(Kind::Arrow, (5.0, 5.0), (5.0, 5.0)).shapes().is_empty(), "a zero-length arrow draws nothing");
    }

    #[test]
    fn colours_format_and_parse() {
        assert_eq!(parse_hex("#FF3B30"), Some([255, 59, 48, 255]));
        assert_eq!(parse_hex("nope"), None);
        assert_eq!(to_hex([255, 59, 48, 255]), "#FF3B30");
        assert_eq!(format_color([255, 0, 0, 255], "rgb"), "rgb(255, 0, 0)");
        assert_eq!(format_color([255, 0, 0, 255], "hsl"), "hsl(0, 100%, 50%)");
        assert_eq!(contrast([255, 255, 0, 255]), [20, 18, 26, 255]);
        assert_eq!(contrast([20, 20, 120, 255]), [255, 255, 255, 255]);
    }
}
