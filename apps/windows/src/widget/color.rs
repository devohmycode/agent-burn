//! Colors and premultiplied blending. Adapted from Taskbar Hub's
//! `src/render/color.rs` (Apache-2.0, see NOTICE).

/// A straight (not premultiplied) 8-bit color.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Rgba {
    pub r: u8,
    pub g: u8,
    pub b: u8,
    pub a: u8,
}

impl Rgba {
    pub const fn rgb(r: u8, g: u8, b: u8) -> Self {
        Self { r, g, b, a: 255 }
    }

    pub const fn with_alpha(self, a: u8) -> Self {
        Self { a, ..self }
    }

    /// `0xRRGGBB`.
    pub const fn hex(rgb: u32) -> Self {
        Self::rgb((rgb >> 16) as u8, (rgb >> 8) as u8, rgb as u8)
    }
}

/// `color` as premultiplied `0xAARRGGBB`.
pub fn premultiply(color: Rgba) -> u32 {
    let a = color.a as u32;
    let r = color.r as u32 * a / 255;
    let g = color.g as u32 * a / 255;
    let b = color.b as u32 * a / 255;
    (a << 24) | (r << 16) | (g << 8) | b
}

/// Draws premultiplied `source` over `destination` at `opacity`.
pub fn blend(destination: &mut u32, source: u32, opacity: f64) {
    let opacity = opacity.clamp(0.0, 1.0);
    let sa = (((source >> 24) & 0xFF) as f64 * opacity).round() as u32;
    if sa == 0 {
        return;
    }
    let sr = (((source >> 16) & 0xFF) as f64 * opacity).round() as u32;
    let sg = (((source >> 8) & 0xFF) as f64 * opacity).round() as u32;
    let sb = ((source & 0xFF) as f64 * opacity).round() as u32;
    let da = (*destination >> 24) & 0xFF;
    let dr = (*destination >> 16) & 0xFF;
    let dg = (*destination >> 8) & 0xFF;
    let db = *destination & 0xFF;
    let inverse = 255 - sa;
    let oa = sa + da * inverse / 255;
    let or = sr + dr * inverse / 255;
    let og = sg + dg * inverse / 255;
    let ob = sb + db * inverse / 255;
    *destination = (oa << 24) | (or << 16) | (og << 8) | ob;
}

/// Whether `pixel` is valid premultiplied color: no channel above alpha.
pub fn is_premultiplied(pixel: u32) -> bool {
    let alpha = pixel >> 24;
    [(pixel >> 16) & 0xFF, (pixel >> 8) & 0xFF, pixel & 0xFF]
        .iter()
        .all(|channel| *channel <= alpha)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn blending_keeps_colors_premultiplied() {
        let mut pixel = 0;
        blend(
            &mut pixel,
            premultiply(Rgba::rgb(0x3B, 0x82, 0xF6).with_alpha(128)),
            1.0,
        );
        assert!(is_premultiplied(pixel));
        blend(&mut pixel, premultiply(Rgba::rgb(255, 255, 255)), 0.3);
        assert!(is_premultiplied(pixel));
        assert_eq!(premultiply(Rgba::hex(0xFF0000)), 0xFFFF_0000);
        assert!(!is_premultiplied(0x10FF_0000));
    }
}
