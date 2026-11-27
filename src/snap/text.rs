//! Text for annotations: one system font, shaped with rustybuzz and turned
//! into outlines that tiny-skia fills. The same outlines are what the editor
//! shows, so a label looks the same on screen and in the saved file.

use std::sync::OnceLock;

use rustybuzz::ttf_parser::{self, GlyphId, OutlineBuilder};
use tiny_skia::PathBuilder;

struct Font {
    data: Vec<u8>,
    index: u32,
    /// The weight asked of a variable font; 0 leaves it alone.
    weight: f32,
}

/// Bold-ish sans faces that ship with each OS, best first.
fn candidates() -> Vec<(&'static str, u32, f32)> {
    if cfg!(target_os = "macos") {
        vec![
            // SF Compact is the system face whose outlines ttf-parser reads
            // (SFNS.ttf, the UI font itself, is in a layout it does not).
            ("/System/Library/Fonts/SFCompact.ttf", 0, 620.0),
            ("/System/Library/Fonts/HelveticaNeue.ttc", 1, 0.0),
            ("/System/Library/Fonts/Supplemental/Arial Bold.ttf", 0, 0.0),
            ("/System/Library/Fonts/Helvetica.ttc", 1, 0.0),
            ("/Library/Fonts/Arial Bold.ttf", 0, 0.0),
        ]
    } else if cfg!(windows) {
        vec![
            ("C:\\Windows\\Fonts\\seguisb.ttf", 0, 0.0),
            ("C:\\Windows\\Fonts\\segoeuib.ttf", 0, 0.0),
            ("C:\\Windows\\Fonts\\arialbd.ttf", 0, 0.0),
            ("C:\\Windows\\Fonts\\arial.ttf", 0, 0.0),
        ]
    } else {
        vec![
            ("/usr/share/fonts/truetype/dejavu/DejaVuSans-Bold.ttf", 0, 0.0),
            ("/usr/share/fonts/TTF/DejaVuSans-Bold.ttf", 0, 0.0),
            ("/usr/share/fonts/dejavu/DejaVuSans-Bold.ttf", 0, 0.0),
            ("/usr/share/fonts/truetype/liberation/LiberationSans-Bold.ttf", 0, 0.0),
            ("/usr/share/fonts/liberation/LiberationSans-Bold.ttf", 0, 0.0),
            ("/usr/share/fonts/truetype/noto/NotoSans-Bold.ttf", 0, 0.0),
            ("/usr/share/fonts/noto/NotoSans-Bold.ttf", 0, 0.0),
            ("/usr/share/fonts/truetype/freefont/FreeSansBold.ttf", 0, 0.0),
        ]
    }
}

fn font() -> Option<&'static Font> {
    static FONT: OnceLock<Option<Font>> = OnceLock::new();
    FONT.get_or_init(|| {
        for (path, index, weight) in candidates() {
            let Ok(data) = std::fs::read(path) else { continue };
            let font = Font { data, index, weight };
            // A font only counts if its outlines can actually be read.
            let usable = face(&font).is_some_and(|face| {
                struct Probe(bool);
                impl OutlineBuilder for Probe {
                    fn move_to(&mut self, _: f32, _: f32) {
                        self.0 = true;
                    }
                    fn line_to(&mut self, _: f32, _: f32) {}
                    fn quad_to(&mut self, _: f32, _: f32, _: f32, _: f32) {}
                    fn curve_to(&mut self, _: f32, _: f32, _: f32, _: f32, _: f32, _: f32) {}
                    fn close(&mut self) {}
                }
                let mut probe = Probe(false);
                face.glyph_index('H').is_some_and(|glyph| face.outline_glyph(glyph, &mut probe).is_some()) && probe.0
            });
            if usable {
                return Some(font);
            }
        }
        eprintln!("[snap] no usable system font: text annotations are unavailable");
        None
    })
    .as_ref()
}

/// Whether text can be drawn at all on this machine.
pub fn available() -> bool {
    font().is_some()
}

fn face(font: &Font) -> Option<rustybuzz::Face<'_>> {
    let mut face = rustybuzz::Face::from_slice(&font.data, font.index)?;
    if font.weight > 0.0 {
        face.set_variations(&[rustybuzz::Variation { tag: ttf_parser::Tag::from_bytes(b"wght"), value: font.weight }]);
    }
    Some(face)
}

struct Glyph {
    id: u16,
    /// Pen position, in pixels from the layout's top-left corner.
    x: f32,
    baseline: f32,
}

/// Lines of shaped text at one size.
pub struct Layout {
    glyphs: Vec<Glyph>,
    scale: f32,
    pub width: f32,
    pub height: f32,
    pub line_height: f32,
    /// Where each line ends, for the caret.
    pub line_ends: Vec<f32>,
}

pub fn layout(text: &str, size: f32) -> Option<Layout> {
    let face = face(font()?)?;
    let scale = size / face.units_per_em() as f32;
    let ascent = face.ascender() as f32 * scale;
    let descent = -(face.descender() as f32) * scale;
    let line_height = (ascent + descent) * 1.08;
    let mut glyphs = Vec::new();
    let mut line_ends = Vec::new();
    let mut width = 0f32;
    let mut lines = 0;
    for (row, line) in text.split('\n').enumerate() {
        lines = row + 1;
        let mut buffer = rustybuzz::UnicodeBuffer::new();
        buffer.push_str(line);
        let shaped = rustybuzz::shape(&face, &[], buffer);
        let mut pen = 0f32;
        let baseline = row as f32 * line_height + ascent;
        for (info, pos) in shaped.glyph_infos().iter().zip(shaped.glyph_positions()) {
            glyphs.push(Glyph {
                id: info.glyph_id as u16,
                x: pen + pos.x_offset as f32 * scale,
                baseline: baseline - pos.y_offset as f32 * scale,
            });
            pen += pos.x_advance as f32 * scale;
        }
        line_ends.push(pen);
        width = width.max(pen);
    }
    Some(Layout { glyphs, scale, width, height: lines.max(1) as f32 * line_height, line_height, line_ends })
}

/// The size a text takes, without building its outlines.
pub fn measure(text: &str, size: f32) -> Option<(f32, f32)> {
    layout(text, size).map(|l| (l.width.max(size * 0.3), l.height))
}

struct Outline<'a> {
    builder: &'a mut PathBuilder,
    x: f32,
    y: f32,
    scale: f32,
}

impl OutlineBuilder for Outline<'_> {
    fn move_to(&mut self, x: f32, y: f32) {
        self.builder.move_to(self.x + x * self.scale, self.y - y * self.scale);
    }
    fn line_to(&mut self, x: f32, y: f32) {
        self.builder.line_to(self.x + x * self.scale, self.y - y * self.scale);
    }
    fn quad_to(&mut self, x1: f32, y1: f32, x: f32, y: f32) {
        self.builder.quad_to(
            self.x + x1 * self.scale,
            self.y - y1 * self.scale,
            self.x + x * self.scale,
            self.y - y * self.scale,
        );
    }
    fn curve_to(&mut self, x1: f32, y1: f32, x2: f32, y2: f32, x: f32, y: f32) {
        self.builder.cubic_to(
            self.x + x1 * self.scale,
            self.y - y1 * self.scale,
            self.x + x2 * self.scale,
            self.y - y2 * self.scale,
            self.x + x * self.scale,
            self.y - y * self.scale,
        );
    }
    fn close(&mut self) {
        self.builder.close();
    }
}

/// The glyph outlines of a layout whose top-left corner is at `(x, y)`.
/// `None` for text with nothing to draw (spaces only).
pub fn outline(layout: &Layout, x: f32, y: f32) -> Option<tiny_skia::Path> {
    let face = face(font()?)?;
    let mut builder = PathBuilder::new();
    for glyph in &layout.glyphs {
        let mut sink = Outline { builder: &mut builder, x: x + glyph.x, y: y + glyph.baseline, scale: layout.scale };
        face.outline_glyph(GlyphId(glyph.id), &mut sink);
    }
    builder.finish()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn text_is_measured_and_outlined() {
        if !available() {
            eprintln!("no system font here; skipping");
            return;
        }
        let one = layout("Hello", 40.0).unwrap();
        let two = layout("Hello\nHello there", 40.0).unwrap();
        assert!(one.width > 40.0 && one.width < 200.0, "{}", one.width);
        assert!(two.width > one.width);
        assert!((two.height - 2.0 * one.height).abs() < 0.5);
        assert_eq!(two.line_ends.len(), 2);
        let path = outline(&one, 10.0, 10.0).unwrap();
        let b = path.bounds();
        assert!(b.left() >= 9.0 && b.top() >= 9.0, "the outline starts at its origin: {b:?}");
        assert!(b.bottom() <= 10.0 + one.height + 1.0);
        assert!(outline(&layout("   ", 40.0).unwrap(), 0.0, 0.0).is_none());
    }
}
