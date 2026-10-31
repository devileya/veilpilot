//! Screenshots: pick a display, grab it, crop a region, downscale, encode.

use std::sync::Arc;

use anyhow::{Result, anyhow, bail};
use base64::Engine as _;
use image::RgbaImage;

use crate::platform::{self, DisplayInfo, Permission, Rect};

pub const PERMISSION_HELP: &str = "macOS is refusing screen capture. Grant Screen Recording to VeilPilot in \
System Settings › Privacy & Security › Screen Recording, then quit and \
relaunch the app — the permission only takes effect on a fresh launch.";

/// A finished screenshot, ready to attach and to send.
#[derive(Clone, Debug)]
pub struct Shot {
    pub id: String,
    pub base64: Arc<String>,
    /// For drawing thumbnails.
    pub image: Arc<gpui::Image>,
    pub width: u32,
    pub height: u32,
}

impl Shot {
    pub fn mime(&self) -> &'static str {
        "image/png"
    }
}

/// A raw frame of one display, before cropping and scaling.
pub struct Frame {
    pub image: RgbaImage,
    pub display: DisplayInfo,
}

pub fn now_ms() -> i64 {
    chrono::Utc::now().timestamp_millis()
}

/// Screen Recording status, as Settings shows it.
pub fn screen_permission() -> Permission {
    platform::screen_permission()
}

/// Lets a dev build exercise capture before Screen Recording is granted. Without
/// the permission macOS still returns the wallpaper and our own windows.
fn permission_check_skipped() -> bool {
    std::env::var_os("VEILPILOT_ALLOW_UNPERMITTED_CAPTURE").is_some()
}

pub fn ensure_permission() -> Result<()> {
    if cfg!(target_os = "macos") && !permission_check_skipped() && screen_permission() != Permission::Granted {
        // Registers the app in System Settings so there is a switch to flip.
        platform::request_screen_permission();
        bail!(PERMISSION_HELP);
    }
    Ok(())
}

/// 'cursor' | 'primary' | a display id.
pub fn pick_display(pref: &str) -> Option<DisplayInfo> {
    let all = platform::displays();
    if pref != "cursor" && pref != "primary" {
        if let Some(d) = all.iter().find(|d| d.id == pref) {
            return Some(d.clone());
        }
    }
    if pref == "primary" {
        return platform::primary_display();
    }
    let (x, y) = platform::cursor_position();
    platform::display_at(x, y)
}

pub fn list_displays() -> Vec<(String, String)> {
    platform::displays()
        .into_iter()
        .enumerate()
        .map(|(i, d)| {
            let label = format!(
                "Display {}{} · {}×{}",
                i + 1,
                if d.primary { " (primary)" } else { "" },
                d.bounds.width.round(),
                d.bounds.height.round()
            );
            (d.id, label)
        })
        .collect()
}

fn find_monitor(display: &DisplayInfo) -> Result<xcap::Monitor> {
    let monitors = xcap::Monitor::all().map_err(|e| anyhow!("Screen capture failed: {e}"))?;
    if monitors.is_empty() {
        bail!("No screen sources available");
    }
    let by_id = monitors.iter().position(|m| m.id().is_ok_and(|id| id.to_string() == display.id));
    // Ids differ in kind on some platforms; the origin is a reliable second key.
    let by_origin = || {
        monitors.iter().position(|m| {
            let (Ok(x), Ok(y)) = (m.x(), m.y()) else { return false };
            let scale = m.scale_factor().unwrap_or(1.0) as f64;
            let near = |a: f64, b: f64| (a - b).abs() < 2.0;
            (near(x as f64, display.bounds.x) && near(y as f64, display.bounds.y))
                || (near(x as f64 / scale, display.bounds.x) && near(y as f64 / scale, display.bounds.y))
        })
    };
    let index = by_id.or_else(by_origin).unwrap_or(0);
    Ok(monitors.into_iter().nth(index).expect("index in range"))
}

/// Blocking: call it off the main thread.
pub fn grab(display: &DisplayInfo) -> Result<Frame> {
    ensure_permission()?;
    let monitor = find_monitor(display)?;
    let image = monitor.capture_image().map_err(|err| {
        if cfg!(target_os = "macos") { anyhow!(PERMISSION_HELP) } else { anyhow!("Screen capture failed: {err}") }
    })?;
    if image.width() == 0 || image.height() == 0 {
        bail!("Screen capture returned an empty frame — grant Screen Recording permission");
    }
    Ok(Frame { image, display: display.clone() })
}

/// Crop a region given in points relative to the display's top-left corner.
/// `None` when the selection is too small to be useful.
pub fn crop(frame: &Frame, rect: Rect) -> Option<RgbaImage> {
    let scale = frame.image.width() as f64 / frame.display.bounds.width.max(1.0);
    let x = (rect.x * scale).round().max(0.0) as u32;
    let y = (rect.y * scale).round().max(0.0) as u32;
    if x >= frame.image.width() || y >= frame.image.height() {
        return None;
    }
    let w = ((rect.width * scale).round() as u32).min(frame.image.width() - x);
    let h = ((rect.height * scale).round() as u32).min(frame.image.height() - y);
    if w < 8 || h < 8 {
        return None;
    }
    Some(image::imageops::crop_imm(&frame.image, x, y, w, h).to_image())
}

/// Longest side scaled down to `max_dimension` — that is what you pay for.
pub fn downscale(image: RgbaImage, max_dimension: u32) -> RgbaImage {
    let (w, h) = image.dimensions();
    let longest = w.max(h);
    if max_dimension == 0 || longest <= max_dimension {
        return image;
    }
    let ratio = max_dimension as f64 / longest as f64;
    let nw = ((w as f64 * ratio).round() as u32).max(1);
    let nh = ((h as f64 * ratio).round() as u32).max(1);
    // Area averaging: the right filter for a large reduction (text stays
    // legible), and an order of magnitude faster than a convolution resize.
    image::imageops::thumbnail(&image, nw, nh)
}

pub fn finish(image: RgbaImage, max_dimension: u32) -> Result<Shot> {
    let scaled = downscale(image, max_dimension);
    let (width, height) = scaled.dimensions();
    let mut png = Vec::new();
    scaled.write_to(&mut std::io::Cursor::new(&mut png), image::ImageFormat::Png)?;
    let base64 = base64::engine::general_purpose::STANDARD.encode(&png);
    let image = Arc::new(gpui::Image::from_bytes(gpui::ImageFormat::Png, png));
    Ok(Shot { id: String::new(), base64: Arc::new(base64), image, width, height })
}

/// The frame as a GPUI render image (BGRA), for the region picker backdrop.
pub fn render_image(image: &RgbaImage) -> Arc<gpui::RenderImage> {
    let mut bgra = image.clone();
    for px in bgra.chunks_exact_mut(4) {
        px.swap(0, 2);
    }
    let frame = image::Frame::new(bgra);
    Arc::new(gpui::RenderImage::new(smallvec::smallvec![frame]))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn frame(w: u32, h: u32, points_w: f64) -> Frame {
        Frame {
            image: RgbaImage::new(w, h),
            display: DisplayInfo {
                id: "1".into(),
                bounds: Rect { x: 0.0, y: 0.0, width: points_w, height: points_w * h as f64 / w as f64 },
                work_area: Rect::default(),
                scale: w as f64 / points_w,
                primary: true,
            },
        }
    }

    #[test]
    fn crop_scales_points_to_pixels_and_clamps() {
        let f = frame(200, 100, 100.0);
        let c = crop(&f, Rect { x: 10.0, y: 10.0, width: 20.0, height: 10.0 }).unwrap();
        assert_eq!(c.dimensions(), (40, 20));
        let c = crop(&f, Rect { x: 90.0, y: 40.0, width: 50.0, height: 50.0 }).unwrap();
        assert_eq!(c.dimensions(), (20, 20));
        assert!(crop(&f, Rect { x: 0.0, y: 0.0, width: 3.0, height: 3.0 }).is_none());
    }

    #[test]
    fn downscale_caps_longest_side() {
        let img = downscale(RgbaImage::new(3200, 1000), 1600);
        assert_eq!(img.dimensions(), (1600, 500));
        let img = downscale(RgbaImage::new(800, 600), 1600);
        assert_eq!(img.dimensions(), (800, 600));
    }

    #[test]
    fn finish_encodes_png() {
        let shot = finish(RgbaImage::new(64, 32), 1600).unwrap();
        assert_eq!((shot.width, shot.height), (64, 32));
        assert!(shot.base64.starts_with("iVBORw0KGgo"), "base64 of a PNG signature");
    }
}

#[cfg(test)]
mod bench {
    #[test]
    #[ignore = "timing only; run with --release --ignored"]
    fn downscale_and_encode_retina_frame() {
        let img = image::RgbaImage::from_fn(3456, 2234, |x, y| {
            image::Rgba([(x % 251) as u8, (y % 241) as u8, ((x ^ y) % 239) as u8, 255])
        });
        let t = std::time::Instant::now();
        let shot = super::finish(img, 1600).unwrap();
        eprintln!("finish took {:?} -> {}x{}", t.elapsed(), shot.width, shot.height);
    }
}
