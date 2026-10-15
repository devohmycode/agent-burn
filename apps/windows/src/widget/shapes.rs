//! Antialiased rounded rectangles and segmented bars. Adapted from Taskbar
//! Hub's `src/render/shapes.rs` (Apache-2.0, see NOTICE).

use super::color::{Rgba, blend, premultiply};

pub fn fill_rounded(pixels: &mut [u32], width: u32, height: u32, color: Rgba, radius: f64) {
    let radius = radius.clamp(0.0, width.min(height) as f64 / 2.0);
    let source = premultiply(color);
    if radius <= 0.0 {
        for pixel in pixels {
            blend(pixel, source, 1.0);
        }
        return;
    }
    for y in 0..height {
        for x in 0..width {
            let dx = if x as f64 + 0.5 < radius {
                radius - (x as f64 + 0.5)
            } else if x as f64 + 0.5 > width as f64 - radius {
                x as f64 + 0.5 - (width as f64 - radius)
            } else {
                0.0
            };
            let dy = if y as f64 + 0.5 < radius {
                radius - (y as f64 + 0.5)
            } else if y as f64 + 0.5 > height as f64 - radius {
                y as f64 + 0.5 - (height as f64 - radius)
            } else {
                0.0
            };
            let distance = (dx * dx + dy * dy).sqrt();
            let coverage = (radius + 0.5 - distance).clamp(0.0, 1.0);
            if coverage > 0.0 {
                blend(&mut pixels[(y * width + x) as usize], source, coverage);
            }
        }
    }
}

pub fn stroke_rounded_rectangle(
    pixels: &mut [u32],
    width: u32,
    height: u32,
    color: Rgba,
    radius: f64,
    stroke_width: f64,
) {
    let mut outer = vec![0u32; pixels.len()];
    fill_rounded(&mut outer, width, height, color, radius);
    if width as f64 > stroke_width * 2.0 && height as f64 > stroke_width * 2.0 {
        let inner_width = (width as f64 - stroke_width * 2.0).max(1.0) as u32;
        let inner_height = (height as f64 - stroke_width * 2.0).max(1.0) as u32;
        let mut inner = vec![0u32; inner_width as usize * inner_height as usize];
        fill_rounded(
            &mut inner,
            inner_width,
            inner_height,
            Rgba::rgb(0, 0, 0),
            (radius - stroke_width).max(0.0),
        );
        let offset = stroke_width.round() as i32;
        for row in 0..inner_height {
            for column in 0..inner_width {
                let inner_coverage = inner[(row * inner_width + column) as usize] >> 24;
                if inner_coverage != 0 {
                    let outer_index = ((row as i32 + offset) as u32 * width
                        + (column as i32 + offset) as u32)
                        as usize;
                    let keep = 255 - inner_coverage;
                    let pixel = outer[outer_index];
                    let scale = |component: u32| (component * keep + 127) / 255;
                    outer[outer_index] = (scale(pixel >> 24) << 24)
                        | (scale((pixel >> 16) & 0xff) << 16)
                        | (scale((pixel >> 8) & 0xff) << 8)
                        | scale(pixel & 0xff);
                }
            }
        }
    }
    for (destination, source) in pixels.iter_mut().zip(outer) {
        if source != 0 {
            blend(destination, source, 1.0);
        }
    }
}

/// Fills the first `amount` (0–1) of a left-to-right bar, cut into
/// `segments` separated by `gap` device pixels.
#[allow(clippy::too_many_arguments)]
pub fn draw_progress(
    pixels: &mut [u32],
    width: u32,
    height: u32,
    color: Rgba,
    radius: f64,
    amount: f64,
    segments: u32,
    gap: f64,
) {
    let layout = SegmentLayout::new(width, segments, gap);
    let used = layout.map_or(width, |layout| layout.extent());
    let mut fill = vec![0u32; used as usize * height as usize];
    fill_rounded(&mut fill, used, height, color, radius);
    for y in 0..height {
        for x in 0..used {
            let progress = (x + 1) as f64 / used as f64;
            if progress <= amount + 1e-9 && layout.is_none_or(|layout| layout.contains(x)) {
                blend(
                    &mut pixels[(y * width + x) as usize],
                    fill[(y * used + x) as usize],
                    1.0,
                );
            }
        }
    }
}

/// Whole-pixel geometry for a segmented bar: every segment and every gap has
/// the same width at fractional display scales; the bar may end a few
/// pixels short of its box.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SegmentLayout {
    pub count: u32,
    pub segment: u32,
    pub gap: u32,
}

impl SegmentLayout {
    pub fn new(extent: u32, count: u32, gap: f64) -> Option<Self> {
        if count <= 1 || extent <= 1 {
            return None;
        }
        let count = count.min(extent);
        let gap = if gap.is_finite() {
            gap.max(0.0).round().min(u32::MAX as f64) as u32
        } else {
            0
        };
        let gap = gap.min((extent - count) / (count - 1));
        if gap == 0 {
            return None;
        }
        let segment = (extent - gap * (count - 1)) / count;
        Some(Self {
            count,
            segment,
            gap,
        })
    }

    pub fn extent(self) -> u32 {
        self.segment * self.count + self.gap * (self.count - 1)
    }

    /// Whether the pixel `along` the bar is inside a segment.
    pub fn contains(self, along: u32) -> bool {
        along < self.extent() && along % (self.segment + self.gap) < self.segment
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn segments_and_gaps_keep_whole_equal_pixels() {
        // 39 logical at 125%: 49 device pixels, 4 segments, gap 1 → 1.
        let layout = SegmentLayout::new(49, 4, 1.25).unwrap();
        assert_eq!((layout.segment, layout.gap), (11, 1));
        assert_eq!(layout.extent(), 47);
        assert!(layout.contains(10) && !layout.contains(11) && layout.contains(12));
        assert!(SegmentLayout::new(49, 4, 0.2).is_none(), "no visible gap");
    }

    #[test]
    fn a_bar_fills_its_share_only() {
        let mut pixels = vec![0u32; 10];
        draw_progress(&mut pixels, 10, 1, Rgba::rgb(255, 0, 0), 0.0, 0.5, 1, 0.0);
        let filled = pixels.iter().filter(|pixel| **pixel != 0).count();
        assert_eq!(filled, 5);
        assert_eq!(pixels[0], 0xFFFF_0000);
        assert_eq!(pixels[9], 0);
    }
}
