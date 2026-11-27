//! Text and QR-code recognition.
//!
//! macOS has both built in (the Vision framework), so nothing is downloaded
//! and nothing leaves the machine. Other platforms have no recognizer here
//! yet and say so.

use image::RgbaImage;

/// What was read from an image.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Recognized {
    /// Lines of text, top to bottom.
    pub lines: Vec<String>,
    /// Payloads of QR codes and barcodes.
    pub codes: Vec<String>,
}

impl Recognized {
    pub fn is_empty(&self) -> bool {
        self.lines.is_empty() && self.codes.is_empty()
    }

    /// The text to put on the clipboard. A code's payload comes first: when
    /// there is one in the picture, it is what was being scanned.
    pub fn text(&self, join_lines: bool) -> String {
        let body = if join_lines { join(&self.lines) } else { self.lines.join("\n") };
        let mut parts: Vec<String> = self.codes.clone();
        if !body.is_empty() {
            parts.push(body);
        }
        parts.join("\n")
    }
}

/// One paragraph out of wrapped lines: a hyphen at a line's end glues the
/// word back together, anything else becomes a space.
fn join(lines: &[String]) -> String {
    let mut out = String::new();
    for line in lines {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        if out.ends_with('-') && out.chars().rev().nth(1).is_some_and(|c| c.is_alphabetic()) {
            out.pop();
        } else if !out.is_empty() {
            out.push(' ');
        }
        out.push_str(line);
    }
    out
}

/// Blocking; call it off the main thread.
pub fn recognize(image: &RgbaImage) -> anyhow::Result<Recognized> {
    imp::recognize(image)
}

pub fn available() -> bool {
    cfg!(target_os = "macos")
}

#[cfg(not(target_os = "macos"))]
mod imp {
    pub fn recognize(_: &image::RgbaImage) -> anyhow::Result<super::Recognized> {
        anyhow::bail!("Text recognition is only available on macOS for now.")
    }
}

#[cfg(target_os = "macos")]
mod imp {
    use std::ffi::{CStr, c_char, c_void};

    use anyhow::{Result, anyhow, bail};
    use objc2::encode::{Encode, Encoding};
    use objc2::runtime::{AnyClass, AnyObject, Bool};
    use objc2::{class, msg_send};

    use super::Recognized;

    type Id = *mut AnyObject;

    #[link(name = "Vision", kind = "framework")]
    unsafe extern "C" {}

    #[repr(C)]
    #[derive(Clone, Copy, Default)]
    struct CGPoint {
        x: f64,
        y: f64,
    }
    #[repr(C)]
    #[derive(Clone, Copy, Default)]
    struct CGSize {
        width: f64,
        height: f64,
    }
    #[repr(C)]
    #[derive(Clone, Copy, Default)]
    struct CGRect {
        origin: CGPoint,
        size: CGSize,
    }
    unsafe impl Encode for CGPoint {
        const ENCODING: Encoding = Encoding::Struct("CGPoint", &[f64::ENCODING, f64::ENCODING]);
    }
    unsafe impl Encode for CGSize {
        const ENCODING: Encoding = Encoding::Struct("CGSize", &[f64::ENCODING, f64::ENCODING]);
    }
    unsafe impl Encode for CGRect {
        const ENCODING: Encoding = Encoding::Struct("CGRect", &[CGPoint::ENCODING, CGSize::ENCODING]);
    }

    unsafe fn string(ns: Id) -> Option<String> {
        if ns.is_null() {
            return None;
        }
        let utf8: *const c_char = unsafe { msg_send![ns, UTF8String] };
        (!utf8.is_null()).then(|| unsafe { CStr::from_ptr(utf8) }.to_string_lossy().into_owned())
    }

    pub fn recognize(image: &image::RgbaImage) -> Result<Recognized> {
        let mut png = Vec::new();
        image.write_to(&mut std::io::Cursor::new(&mut png), image::ImageFormat::Png)?;
        let (Some(text_class), Some(code_class), Some(handler_class)) = (
            AnyClass::get(c"VNRecognizeTextRequest"),
            AnyClass::get(c"VNDetectBarcodesRequest"),
            AnyClass::get(c"VNImageRequestHandler"),
        ) else {
            bail!("The Vision framework is not available on this Mac.");
        };

        unsafe {
            let pool: Id = msg_send![class!(NSAutoreleasePool), new];
            let result = (|| {
                let data: Id =
                    msg_send![class!(NSData), dataWithBytes: png.as_ptr() as *const c_void, length: png.len()];
                let options: Id = msg_send![class!(NSDictionary), dictionary];
                let handler: Id = msg_send![handler_class, alloc];
                let handler: Id = msg_send![handler, initWithData: data, options: options];
                if handler.is_null() {
                    bail!("Vision could not read the image.");
                }

                let text: Id = msg_send![text_class, new];
                // Accurate (0) rather than fast (1): a screenshot is small.
                let _: () = msg_send![text, setRecognitionLevel: 0isize];
                let _: () = msg_send![text, setUsesLanguageCorrection: Bool::YES];
                let detects: bool = msg_send![text, respondsToSelector: objc2::sel!(setAutomaticallyDetectsLanguage:)];
                if detects {
                    let _: () = msg_send![text, setAutomaticallyDetectsLanguage: Bool::YES];
                }
                let codes: Id = msg_send![code_class, new];

                let requests: Id = msg_send![class!(NSMutableArray), array];
                let _: () = msg_send![requests, addObject: text];
                let _: () = msg_send![requests, addObject: codes];
                let mut error: Id = std::ptr::null_mut();
                let ok: bool = msg_send![handler, performRequests: requests, error: &mut error];

                let mut found: Vec<(f64, f64, String)> = Vec::new();
                let observations: Id = msg_send![text, results];
                let count: usize = if observations.is_null() { 0 } else { msg_send![observations, count] };
                for i in 0..count {
                    let observation: Id = msg_send![observations, objectAtIndex: i];
                    let candidates: Id = msg_send![observation, topCandidates: 1usize];
                    let n: usize = msg_send![candidates, count];
                    if n == 0 {
                        continue;
                    }
                    let best: Id = msg_send![candidates, objectAtIndex: 0usize];
                    let value: Id = msg_send![best, string];
                    let bounds: CGRect = msg_send![observation, boundingBox];
                    if let Some(line) = string(value) {
                        // Vision's box is normalised with the origin at the
                        // bottom left; flip it to read top to bottom.
                        found.push((1.0 - (bounds.origin.y + bounds.size.height), bounds.origin.x, line));
                    }
                }
                // Reading order: by row, then left to right within a row.
                found.sort_by(|a, b| if (a.0 - b.0).abs() < 0.012 { a.1.total_cmp(&b.1) } else { a.0.total_cmp(&b.0) });

                let mut payloads = Vec::new();
                let observations: Id = msg_send![codes, results];
                let count: usize = if observations.is_null() { 0 } else { msg_send![observations, count] };
                for i in 0..count {
                    let observation: Id = msg_send![observations, objectAtIndex: i];
                    let value: Id = msg_send![observation, payloadStringValue];
                    if let Some(payload) = string(value)
                        && !payloads.contains(&payload)
                    {
                        payloads.push(payload);
                    }
                }

                let _: () = msg_send![text, release];
                let _: () = msg_send![codes, release];
                let _: () = msg_send![handler, release];
                if !ok && found.is_empty() && payloads.is_empty() {
                    let description: Id =
                        if error.is_null() { std::ptr::null_mut() } else { msg_send![error, localizedDescription] };
                    return Err(anyhow!(
                        "Text recognition failed: {}",
                        string(description).unwrap_or_else(|| "unknown error".into())
                    ));
                }
                Ok(Recognized { lines: found.into_iter().map(|(_, _, line)| line).collect(), codes: payloads })
            })();
            let _: () = msg_send![pool, drain];
            result
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lines_join_into_a_paragraph() {
        let r = Recognized {
            lines: vec!["The quick brown fox jumps over the la-".into(), "zy dog.".into(), " And again. ".into()],
            codes: vec![],
        };
        assert_eq!(r.text(true), "The quick brown fox jumps over the lazy dog. And again.");
        assert_eq!(r.text(false).lines().count(), 3);
        let dash = Recognized { lines: vec!["a -".into(), "b".into()], codes: vec![] };
        assert_eq!(dash.text(true), "a - b", "a free-standing dash is not a hyphen");
    }

    #[test]
    fn a_code_comes_before_the_text_around_it() {
        let r = Recognized { lines: vec!["Scan me".into()], codes: vec!["https://example.com".into()] };
        assert_eq!(r.text(false), "https://example.com\nScan me");
        assert!(Recognized::default().is_empty());
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn vision_reads_rendered_text() {
        use crate::snap::annot::{Annot, FillMode, Kind, Pt};
        if !crate::snap::text::available() {
            return;
        }
        let base = RgbaImage::from_pixel(900, 220, image::Rgba([255, 255, 255, 255]));
        let mut label = Annot::new(1, Kind::Text, Pt::new(40.0, 60.0), [0, 0, 0, 255], 72.0);
        label.fill = FillMode::Solid;
        label.color = [255, 255, 255, 255];
        label.text = "Hello VeilPilot 2026".into();
        let image = crate::snap::raster::flatten(&base, &[label]);
        let read = recognize(&image).expect("vision runs");
        let text = read.text(true).to_lowercase();
        assert!(text.contains("hello") && text.contains("2026"), "read: {text:?}");
    }
}
