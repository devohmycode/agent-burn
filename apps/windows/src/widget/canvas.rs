//! A pixel canvas in logical coordinates. Adapted from Taskbar Hub's
//! `src/render/canvas.rs` (Apache-2.0, see NOTICE).
//!
//! Pixels are premultiplied BGRA stored as `0xAARRGGBB`, the format
//! `UpdateLayeredWindow` expects. Every primitive draws into its own box
//! snapped to device pixels, then blends onto the canvas, so edges land on
//! whole pixels at fractional display scales.

use std::{cell::RefCell, collections::HashMap};

use super::{
    color::{Rgba, blend},
    shapes::{draw_progress, fill_rounded, stroke_rounded_rectangle},
    text::{FontRendering, TextAlign, TextStyle, render_text},
};

/// Texts kept by `Canvas::draw_text`; all are dropped once it is full.
const TEXT_CACHE_SIZE: usize = 256;

/// Everything a drawn text's pixels depend on.
#[derive(Clone, PartialEq, Eq, Hash)]
struct TextKey {
    text: String,
    family: &'static str,
    size: u64,
    weight: i32,
    rendering: FontRendering,
    align: TextAlign,
    color: Rgba,
    box_size: (u32, u32),
}

thread_local! {
    static TEXT_CACHE: RefCell<HashMap<TextKey, Vec<u32>>> = RefCell::new(HashMap::new());
}

/// A rectangle in logical pixels (96 DPI).
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Rect {
    pub x: f64,
    pub y: f64,
    pub width: f64,
    pub height: f64,
}

impl Rect {
    pub const fn new(x: f64, y: f64, width: f64, height: f64) -> Self {
        Self {
            x,
            y,
            width,
            height,
        }
    }

    pub fn contains(&self, (x, y): (f64, f64)) -> bool {
        (self.x..self.x + self.width).contains(&x) && (self.y..self.y + self.height).contains(&y)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct DeviceBox {
    x: i32,
    y: i32,
    width: u32,
    height: u32,
}

pub struct Canvas {
    width: u32,
    height: u32,
    scale: f64,
    pixels: Vec<u32>,
}

impl Canvas {
    /// A transparent canvas of `logical_width` × `logical_height` at `scale`.
    pub fn new(logical_width: f64, logical_height: f64, scale: f64) -> Self {
        let scale = if scale.is_finite() && scale > 0.0 {
            scale
        } else {
            1.0
        };
        let width = device_length(logical_width, scale);
        let height = device_length(logical_height, scale);
        Self {
            width,
            height,
            scale,
            pixels: vec![0; width as usize * height as usize],
        }
    }

    pub fn width(&self) -> u32 {
        self.width
    }

    pub fn height(&self) -> u32 {
        self.height
    }

    pub fn pixels(&self) -> &[u32] {
        &self.pixels
    }

    pub fn into_pixels(self) -> Vec<u32> {
        self.pixels
    }

    pub fn fill_rounded(&mut self, rect: Rect, color: Rgba, radius: f64) {
        let radius = radius * self.scale;
        self.draw(rect, |pixels, width, height| {
            fill_rounded(pixels, width, height, color, radius)
        });
    }

    pub fn stroke_rounded(&mut self, rect: Rect, color: Rgba, radius: f64, stroke: f64) {
        let (radius, stroke) = (radius * self.scale, (stroke * self.scale).max(1.0));
        self.draw(rect, |pixels, width, height| {
            stroke_rounded_rectangle(pixels, width, height, color, radius, stroke)
        });
    }

    /// A bar filled to `amount` (0–1), in `segments` separated by `gap`
    /// logical pixels.
    pub fn progress(
        &mut self,
        rect: Rect,
        color: Rgba,
        radius: f64,
        amount: f64,
        segments: u32,
        gap: f64,
    ) {
        let (radius, gap) = (radius * self.scale, gap * self.scale);
        self.draw(rect, |pixels, width, height| {
            draw_progress(pixels, width, height, color, radius, amount, segments, gap)
        });
    }

    /// Single-line text, vertically centered in `rect` and ellipsized to fit.
    /// The pixels of recent texts are reused: GDI text costs most of a
    /// redraw, and the same values come back again and again.
    pub fn draw_text(&mut self, rect: Rect, text: &str, style: &TextStyle) {
        let size = (style.size * self.scale).max(1.0);
        self.draw(rect, |pixels, width, height| {
            let key = TextKey {
                text: text.to_string(),
                family: style.family,
                size: size.to_bits(),
                weight: style.weight,
                rendering: style.rendering,
                align: style.align,
                color: style.color,
                box_size: (width, height),
            };
            TEXT_CACHE.with(|cache| {
                let mut cache = cache.borrow_mut();
                if let Some(cached) = cache.get(&key) {
                    pixels.copy_from_slice(cached);
                    return;
                }
                render_text(pixels, width, height, text, size, style);
                if cache.len() >= TEXT_CACHE_SIZE {
                    cache.clear();
                }
                cache.insert(key, pixels.to_vec());
            });
        });
    }

    /// Runs `paint` on a scratch buffer covering `rect`, then blends it in.
    fn draw(&mut self, rect: Rect, paint: impl FnOnce(&mut [u32], u32, u32)) {
        let Some(area) = self.device_box(rect) else {
            return;
        };
        let mut scratch = vec![0u32; area.width as usize * area.height as usize];
        paint(&mut scratch, area.width, area.height);
        for row in 0..area.height as i32 {
            let y = area.y + row;
            if y < 0 || y >= self.height as i32 {
                continue;
            }
            for column in 0..area.width as i32 {
                let x = area.x + column;
                if x < 0 || x >= self.width as i32 {
                    continue;
                }
                let source = scratch[(row as u32 * area.width + column as u32) as usize];
                if source != 0 {
                    blend(
                        &mut self.pixels[(y as u32 * self.width + x as u32) as usize],
                        source,
                        1.0,
                    );
                }
            }
        }
    }

    fn device_box(&self, rect: Rect) -> Option<DeviceBox> {
        let left = (rect.x * self.scale).round() as i32;
        let top = (rect.y * self.scale).round() as i32;
        let right = ((rect.x + rect.width) * self.scale).round() as i32;
        let bottom = ((rect.y + rect.height) * self.scale).round() as i32;
        (right > left && bottom > top).then_some(DeviceBox {
            x: left,
            y: top,
            width: (right - left) as u32,
            height: (bottom - top) as u32,
        })
    }
}

fn device_length(logical: f64, scale: f64) -> u32 {
    ((logical.max(0.0) * scale).round() as u32).max(1)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::widget::text::rendered;

    #[test]
    fn sizes_and_boxes_snap_to_device_pixels() {
        let canvas = Canvas::new(107.0, 46.0, 1.25);
        assert_eq!((canvas.width(), canvas.height()), (134, 58));
        let area = canvas.device_box(Rect::new(43.0, 9.0, 64.0, 13.0)).unwrap();
        assert_eq!(
            area,
            DeviceBox {
                x: 54,
                y: 11,
                width: 80,
                height: 17
            }
        );
        assert!(canvas.device_box(Rect::new(10.0, 10.0, 0.0, 5.0)).is_none());
    }

    #[test]
    fn an_unchanged_text_is_drawn_by_gdi_once() {
        let style = TextStyle::segoe(12.0, 500, Rgba::rgb(255, 255, 255));
        let draw = |text: &str| {
            let mut canvas = Canvas::new(60.0, 16.0, 1.25);
            canvas.draw_text(Rect::new(2.0, 1.0, 56.0, 14.0), text, &style);
            canvas.into_pixels()
        };
        let before = rendered();
        let first = draw("42% · 3h");
        assert!(first.iter().any(|pixel| *pixel != 0));
        for _ in 0..10 {
            assert_eq!(draw("42% · 3h"), first, "from the cache");
        }
        assert_eq!(rendered() - before, 1);
        assert_ne!(draw("43% · 3h"), first);
        assert_eq!(rendered() - before, 2);
    }

    #[test]
    fn opaque_fill_covers_only_its_rectangle() {
        let mut canvas = Canvas::new(4.0, 2.0, 1.0);
        canvas.fill_rounded(Rect::new(1.0, 0.0, 2.0, 2.0), Rgba::rgb(255, 0, 0), 0.0);
        let red = 0xFFFF_0000;
        assert_eq!(canvas.pixels(), &[0, red, red, 0, 0, red, red, 0]);
    }

    #[test]
    fn drawing_outside_the_canvas_is_clipped() {
        let mut canvas = Canvas::new(2.0, 2.0, 1.0);
        canvas.fill_rounded(Rect::new(-1.0, -1.0, 2.0, 2.0), Rgba::rgb(0, 0, 255), 0.0);
        assert_eq!(canvas.pixels(), &[0xFF00_00FF, 0, 0, 0]);
    }
}
