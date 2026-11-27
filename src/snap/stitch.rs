//! Scrolling capture: successive frames of the same region, joined where
//! they overlap.
//!
//! Each row of a frame is reduced to a short signature (average brightness
//! in a couple dozen columns). Two frames are aligned by finding the shift
//! at which the rows of one match the rows of the other; whatever does not
//! move between frames — a sticky header, a bottom bar — is recognised first
//! and kept out of the comparison, then written once.

use image::RgbaImage;

const BUCKETS: usize = 24;
/// Mean brightness difference per bucket (out of 765) still called "same".
const ROW_TOLERANCE: u32 = 9;
/// A row that differs this much from the one above carries information;
/// blank rows match anything and prove nothing.
const DETAIL: u32 = 14;

type Signature = [u16; BUCKETS];

fn signatures(image: &RgbaImage) -> Vec<Signature> {
    let (w, h) = (image.width() as usize, image.height() as usize);
    let raw = image.as_raw();
    let bucket_w = (w / BUCKETS).max(1);
    (0..h)
        .map(|y| {
            let row = &raw[y * w * 4..(y + 1) * w * 4];
            let mut sig = [0u16; BUCKETS];
            for (b, slot) in sig.iter_mut().enumerate() {
                let start = (b * bucket_w).min(w.saturating_sub(1));
                let end = ((b + 1) * bucket_w).min(w);
                let mut sum = 0u32;
                let mut n = 0u32;
                let mut x = start;
                while x < end {
                    let p = &row[x * 4..x * 4 + 3];
                    sum += p[0] as u32 + p[1] as u32 + p[2] as u32;
                    n += 1;
                    x += 2;
                }
                *slot = (sum / n.max(1)) as u16;
            }
            sig
        })
        .collect()
}

fn distance(a: &Signature, b: &Signature) -> u32 {
    a.iter().zip(b).map(|(x, y)| (*x as i32 - *y as i32).unsigned_abs()).sum()
}

fn same(a: &Signature, b: &Signature) -> bool {
    distance(a, b) <= ROW_TOLERANCE * BUCKETS as u32
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Step {
    /// The content moved by this many rows, which were appended.
    Added(usize),
    /// Nothing moved since the last frame.
    Still,
    /// The frames could not be aligned (scrolled too far, or the content
    /// changed); nothing was appended.
    Lost,
    /// The image reached its maximum height.
    Full,
}

pub struct Stitcher {
    width: u32,
    frame_h: usize,
    /// Output rows so far, RGBA, without the bottom band that never moves.
    rows: Vec<u8>,
    last: RgbaImage,
    last_sig: Vec<Signature>,
    /// Rows at the top / bottom that stay put while the rest scrolls; fixed
    /// by the first pair of frames that moves.
    bands: Option<(usize, usize)>,
    max_height: usize,
}

impl Stitcher {
    pub fn new(first: RgbaImage, max_height: u32) -> Self {
        Self {
            width: first.width(),
            frame_h: first.height() as usize,
            rows: first.as_raw().clone(),
            last_sig: signatures(&first),
            last: first,
            bands: None,
            max_height: (max_height as usize).max(1),
        }
    }

    /// Height of the image so far.
    pub fn height(&self) -> usize {
        self.rows.len() / (self.width as usize * 4) + self.bands.map(|b| b.1).unwrap_or(0)
    }

    pub fn push(&mut self, next: RgbaImage) -> Step {
        if next.width() != self.width || next.height() as usize != self.frame_h {
            return Step::Lost;
        }
        let sig = signatures(&next);
        let h = self.frame_h;
        if (0..h).all(|y| same(&self.last_sig[y], &sig[y])) {
            return Step::Still;
        }
        let (top, bottom) = self.bands.unwrap_or_else(|| {
            let top = (0..h).take_while(|&y| same(&self.last_sig[y], &sig[y])).count();
            let bottom = (0..h - top).take_while(|&y| same(&self.last_sig[h - 1 - y], &sig[h - 1 - y])).count();
            // A band has to be a band: a couple of matching rows at an edge
            // are just the page's own margin.
            (if top >= 8 { top } else { 0 }, if bottom >= 8 { bottom } else { 0 })
        });
        let mid = h - top - bottom;
        if mid < 24 {
            return Step::Lost;
        }
        let Some(shift) = find_shift(&self.last_sig[top..h - bottom], &sig[top..h - bottom]) else {
            return Step::Lost;
        };

        let stride = self.width as usize * 4;
        if self.bands.is_none() {
            self.bands = Some((top, bottom));
            // The bottom band comes off now and goes back on at the end.
            self.rows.truncate((h - bottom) * stride);
        }
        let from = h - bottom - shift;
        self.rows.extend_from_slice(&next.as_raw()[from * stride..(h - bottom) * stride]);
        self.last = next;
        self.last_sig = sig;
        if self.height() >= self.max_height { Step::Full } else { Step::Added(shift) }
    }

    pub fn finish(mut self) -> RgbaImage {
        let stride = self.width as usize * 4;
        if let Some((_, bottom)) = self.bands {
            let h = self.frame_h;
            self.rows.extend_from_slice(&self.last.as_raw()[(h - bottom) * stride..h * stride]);
        }
        let max = self.max_height * stride;
        if self.rows.len() > max {
            self.rows.truncate(max);
        }
        let height = (self.rows.len() / stride) as u32;
        RgbaImage::from_raw(self.width, height, self.rows).expect("whole rows")
    }
}

/// How many rows `next` has moved up relative to `prev`: the `d` for which
/// `next[j]` matches `prev[j + d]` over the rows they share.
fn find_shift(prev: &[Signature], next: &[Signature]) -> Option<usize> {
    let n = prev.len();
    let detail: Vec<bool> = (0..n).map(|y| y > 0 && distance(&next[y], &next[y - 1]) > DETAIL).collect();
    let mut best: Option<(usize, f32)> = None;
    // At least a strip of overlap is needed to trust an alignment.
    let min_overlap = (n / 10).max(16);
    for d in 1..=n.saturating_sub(min_overlap) {
        let overlap = n - d;
        let (mut matched, mut informative, mut informative_matched) = (0usize, 0usize, 0usize);
        for j in 0..overlap {
            let ok = same(&next[j], &prev[j + d]);
            matched += ok as usize;
            if detail[j] {
                informative += 1;
                informative_matched += ok as usize;
            }
            // Give up on this shift as soon as it cannot reach the bar.
            if j + 1 - matched > overlap / 10 {
                break;
            }
        }
        if informative < 6 || matched * 100 < overlap * 92 || informative_matched * 100 < informative * 88 {
            continue;
        }
        let score = informative_matched as f32 / informative as f32 + matched as f32 / overlap as f32;
        if best.is_none_or(|(_, s)| score > s + 1e-4) {
            best = Some((d, score));
        }
    }
    best.map(|(d, _)| d)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A tall "page" where every row is different, with distinct columns.
    fn page(width: u32, height: u32) -> RgbaImage {
        RgbaImage::from_fn(width, height, |x, y| {
            let v = (y.wrapping_mul(2654435761) >> 7) as u8;
            let stripe = if (x / 16 + y / 5) % 3 == 0 { 255 } else { v };
            image::Rgba([stripe, v.wrapping_mul(3), v.wrapping_add((x / 32) as u8 * 20), 255])
        })
    }

    fn view(page: &RgbaImage, y: u32, h: u32) -> RgbaImage {
        image::imageops::crop_imm(page, 0, y, page.width(), h).to_image()
    }

    #[test]
    fn frames_are_joined_where_they_overlap() {
        let page = page(240, 1400);
        let mut s = Stitcher::new(view(&page, 0, 300), 20000);
        let mut y = 0;
        for step in [120, 200, 90, 250, 170, 270] {
            y += step;
            assert_eq!(s.push(view(&page, y, 300)), Step::Added(step as usize));
        }
        assert_eq!(s.push(view(&page, y, 300)), Step::Still);
        let out = s.finish();
        assert_eq!(out.height(), y + 300);
        assert_eq!(out, view(&page, 0, y + 300), "the stitched image is the page");
    }

    #[test]
    fn a_sticky_header_and_footer_are_written_once() {
        let page = page(240, 1200);
        let frame = |y: u32| {
            // 40 fixed rows on top, 30 at the bottom, the page scrolling between.
            let mut f = RgbaImage::from_pixel(240, 320, image::Rgba([30, 60, 200, 255]));
            image::imageops::replace(&mut f, &view(&page, y, 250), 0, 40);
            for yy in 290..320 {
                for x in 0..240 {
                    f.put_pixel(x, yy, image::Rgba([200, 60, 30, 255]));
                }
            }
            f
        };
        let mut s = Stitcher::new(frame(0), 20000);
        assert_eq!(s.push(frame(150)), Step::Added(150));
        assert_eq!(s.push(frame(330)), Step::Added(180));
        let out = s.finish();
        assert_eq!(out.height(), 40 + 250 + 330 + 30);
        assert_eq!(out.get_pixel(5, 10).0, [30, 60, 200, 255], "header on top");
        assert_eq!(out.get_pixel(5, out.height() - 5).0, [200, 60, 30, 255], "footer at the very end");
        assert_eq!(image::imageops::crop_imm(&out, 0, 40, 240, 580).to_image(), view(&page, 0, 580));
    }

    #[test]
    fn a_jump_with_no_overlap_is_not_guessed() {
        let page = page(200, 2000);
        let mut s = Stitcher::new(view(&page, 0, 300), 20000);
        assert_eq!(s.push(view(&page, 900, 300)), Step::Lost);
        assert_eq!(s.finish().height(), 300, "a lost frame adds nothing");
    }

    #[test]
    fn the_height_cap_ends_the_capture() {
        let page = page(200, 2000);
        let mut s = Stitcher::new(view(&page, 0, 300), 500);
        assert_eq!(s.push(view(&page, 150, 300)), Step::Added(150));
        assert_eq!(s.push(view(&page, 300, 300)), Step::Full);
        assert_eq!(s.finish().height(), 500);
    }
}
