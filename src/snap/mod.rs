//! The screenshot tool: capture modes, the annotation document, the pixel
//! work behind the editor, scrolling capture and text recognition.
//!
//! The windows (editor, pinned image, pickers) are in `ui`; this module has
//! no GPUI in it, so everything here is unit-tested without a screen.

pub mod annot;
pub mod ocr;
pub mod raster;
pub mod stitch;
pub mod text;

use std::sync::Arc;

use anyhow::{Result, anyhow};
use image::RgbaImage;

use crate::platform::{self, WindowInfo};

/// What a screenshot hotkey asks for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
    Fullscreen,
    Area,
    Window,
    AnyWindow,
    Scrolling,
    RepeatArea,
    Ocr,
}

impl Mode {
    pub fn label(self) -> &'static str {
        match self {
            Mode::Fullscreen => "Fullscreen",
            Mode::Area => "Area",
            Mode::Window => "Window",
            Mode::AnyWindow => "Window",
            Mode::Scrolling => "Scrolling",
            Mode::RepeatArea => "Area",
            Mode::Ocr => "Text",
        }
    }
}

/// A finished capture at full resolution.
#[derive(Clone, Debug)]
pub struct Capture {
    pub image: Arc<RgbaImage>,
    /// Pixels per point of the screen it came from (2 on a Retina display).
    pub scale: f32,
    pub label: String,
}

impl Capture {
    pub fn new(image: RgbaImage, scale: f32, label: impl Into<String>) -> Self {
        Self { image: Arc::new(image), scale: scale.max(0.5), label: label.into() }
    }
}

/// The windows a screenshot can be taken of: every ordinary window that is
/// not ours.
pub fn candidate_windows() -> Vec<WindowInfo> {
    let me = std::process::id();
    platform::windows().into_iter().filter(|w| w.pid != me).collect()
}

/// The window the user is working in: the frontmost window of the frontmost
/// app, or failing that the frontmost window of all.
pub fn active_window() -> Option<WindowInfo> {
    let list = candidate_windows();
    let front = platform::frontmost_pid();
    list.iter().find(|w| Some(w.pid) == front).or_else(|| list.first()).cloned()
}

/// Blocking: capture one window by itself, whatever is in front of it.
pub fn grab_window(window: &WindowInfo) -> Result<RgbaImage> {
    crate::capture::ensure_permission()?;
    let target = xcap::Window::all()
        .map_err(|e| anyhow!("Could not list windows: {e}"))?
        .into_iter()
        .find(|w| w.id().ok() == Some(window.id))
        .ok_or_else(|| anyhow!("That window is gone."))?;
    let image = target.capture_image().map_err(|e| anyhow!("Could not capture the window: {e}"))?;
    if image.width() == 0 || image.height() == 0 {
        return Err(anyhow!("The window came back empty."));
    }
    Ok(image)
}

/// A file name for a capture taken now: "VeilPilot 2026-01-31 at 14.05.09".
pub fn file_stem() -> String {
    chrono::Local::now().format("VeilPilot %Y-%m-%d at %H.%M.%S").to_string()
}

/// Write `image` into `folder` under a fresh name; returns the path.
pub fn save_to_folder(image: &RgbaImage, folder: &std::path::Path, format: &str) -> Result<std::path::PathBuf> {
    let (bytes, extension) = raster::encode(image, format)?;
    std::fs::create_dir_all(folder)?;
    let stem = file_stem();
    let mut path = folder.join(format!("{stem}.{extension}"));
    let mut n = 2;
    while path.exists() {
        path = folder.join(format!("{stem} ({n}).{extension}"));
        n += 1;
    }
    std::fs::write(&path, bytes)?;
    Ok(path)
}

/// Write `image` to exactly `path`; the extension picks the format.
pub fn save_as(image: &RgbaImage, path: &std::path::Path) -> Result<()> {
    let format = match path.extension().and_then(|e| e.to_str()).map(str::to_ascii_lowercase).as_deref() {
        Some("jpg") | Some("jpeg") => "jpg",
        _ => "png",
    };
    let (bytes, _) = raster::encode(image, format)?;
    std::fs::write(path, bytes)?;
    Ok(())
}

/// PNG bytes as a clipboard / GPUI image.
pub fn png_image(image: &RgbaImage) -> Result<gpui::Image> {
    let (bytes, _) = raster::encode(image, "png")?;
    Ok(gpui::Image::from_bytes(gpui::ImageFormat::Png, bytes))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn saving_never_overwrites() {
        let dir = std::env::temp_dir().join(format!("vp-snap-{}", uuid::Uuid::new_v4()));
        let image = RgbaImage::from_pixel(8, 8, image::Rgba([1, 2, 3, 255]));
        let a = save_to_folder(&image, &dir, "png").unwrap();
        let b = save_to_folder(&image, &dir, "png").unwrap();
        assert_ne!(a, b);
        assert!(a.exists() && b.exists());
        let jpg = dir.join("x.JPG");
        save_as(&image, &jpg).unwrap();
        assert_eq!(&std::fs::read(&jpg).unwrap()[..2], &[0xFF, 0xD8], "the extension picked JPEG");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
