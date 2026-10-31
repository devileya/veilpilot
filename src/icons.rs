//! The mark, drawn in code: a half-silvered square — one half filled, the
//! other open, split by a hairline diagonal. A beamsplitter, which is
//! what a teleprompter mirror and this app have in common.

pub const LAMP: [u8; 3] = [0xe8, 0xa3, 0x3d];
pub const VOID: [u8; 3] = [0x15, 0x13, 0x1c];
/// Template images are black-with-alpha; macOS recolours them for light and
/// dark menu bars itself.
pub const BLACK: [u8; 3] = [0, 0, 0];

pub struct MarkStyle {
    pub inset: f64,
    pub radius: f64,
    pub fill: [u8; 3],
    pub stroke: [u8; 3],
    pub background: Option<[u8; 3]>,
}

/// RGBA pixels, `size * size * 4` bytes. 4x supersampled so the diagonal and
/// the rounded corners stay clean at 16 px.
pub fn draw_mark(size: u32, style: &MarkStyle) -> Vec<u8> {
    const SS: u32 = 4;
    // Proportional so the outline reads at 16 px and does not go slab-thick
    // at 512.
    let stroke_width = (size as f64 * 0.02).max(1.0) * SS as f64;
    let (x0, y0) = (style.inset * SS as f64, style.inset * SS as f64);
    let (x1, y1) = ((size as f64 - style.inset) * SS as f64, (size as f64 - style.inset) * SS as f64);
    let r = style.radius * SS as f64;

    let inside_rounded = |x: f64, y: f64| {
        if x < x0 || x > x1 || y < y0 || y > y1 {
            return false;
        }
        let cx = x.clamp(x0 + r, x1 - r);
        let cy = y.clamp(y0 + r, y1 - r);
        (x - cx).powi(2) + (y - cy).powi(2) <= r * r
    };
    let edge_distance = |x: f64, y: f64| (x - x0).min(x1 - x).min(y - y0).min(y1 - y);

    let sample = |x: f64, y: f64| -> Option<[u8; 3]> {
        if inside_rounded(x, y) {
            let border = edge_distance(x, y) < stroke_width;
            // The diagonal runs corner to corner of the box.
            let t = (x - x0) / (x1 - x0) + (y - y0) / (y1 - y0);
            let on_diagonal = (t - 1.0).abs() < 0.045;
            if border || on_diagonal {
                Some(style.stroke)
            } else if t < 1.0 {
                Some(style.fill)
            } else {
                style.background
            }
        } else {
            style.background
        }
    };

    let mut out = vec![0u8; (size * size * 4) as usize];
    for y in 0..size {
        for x in 0..size {
            let (mut r0, mut g0, mut b0, mut a0) = (0.0, 0.0, 0.0, 0.0);
            for sy in 0..SS {
                for sx in 0..SS {
                    if let Some(c) = sample((x * SS + sx) as f64, (y * SS + sy) as f64) {
                        r0 += c[0] as f64;
                        g0 += c[1] as f64;
                        b0 += c[2] as f64;
                        a0 += 1.0;
                    }
                }
            }
            let o = ((y * size + x) * 4) as usize;
            if a0 > 0.0 {
                out[o] = (r0 / a0).round() as u8;
                out[o + 1] = (g0 / a0).round() as u8;
                out[o + 2] = (b0 / a0).round() as u8;
                out[o + 3] = (a0 / (SS * SS) as f64 * 255.0).round() as u8;
            }
        }
    }
    out
}

/// The menu-bar / tray icon at the given pixel size.
pub fn tray(size: u32) -> Vec<u8> {
    let inset = size as f64 / 8.0;
    if cfg!(target_os = "macos") {
        draw_mark(size, &MarkStyle { inset, radius: inset, fill: BLACK, stroke: BLACK, background: None })
    } else {
        draw_mark(size, &MarkStyle { inset, radius: inset, fill: LAMP, stroke: LAMP, background: None })
    }
}

/// The app icon: the amber mark on the panel's graphite.
pub fn app_icon(size: u32) -> Vec<u8> {
    let s = size as f64 / 512.0;
    draw_mark(size, &MarkStyle { inset: 112.0 * s, radius: 40.0 * s, fill: LAMP, stroke: LAMP, background: Some(VOID) })
}

/// `veilpilot --write-icons <dir>`: PNGs for packaging.
pub fn write_icons(dir: &std::path::Path) -> anyhow::Result<()> {
    std::fs::create_dir_all(dir)?;
    for size in [16u32, 32, 64, 128, 256, 512, 1024] {
        let rgba = app_icon(size);
        let img = image::RgbaImage::from_raw(size, size, rgba).expect("size matches");
        img.save(dir.join(format!("icon_{size}.png")))?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mark_has_filled_and_open_halves() {
        let px = tray(32);
        let alpha = |x: u32, y: u32| px[((y * 32 + x) * 4 + 3) as usize];
        // y grows downward, so `t < 1` is the upper-left half — the same
        // mark make-icons.js drew.
        assert!(alpha(9, 9) > 200, "upper-left is filled");
        assert_eq!(alpha(22, 22), 0, "lower-right is open");
        assert_eq!(alpha(0, 0), 0, "outside the box is clear");
    }
}
