//! Text drawn by GDI into a coverage mask, then blended in color. Adapted
//! from Taskbar Hub's `src/render/text.rs` (Apache-2.0, see NOTICE).

use std::cell::Cell;

use windows::{
    Win32::{
        Foundation::{COLORREF, RECT, SIZE},
        Graphics::Gdi::*,
    },
    core::PCWSTR,
};

use super::color::{Rgba, blend, premultiply};

thread_local! {
    /// Texts this thread drew with GDI, to check the cache.
    static RENDERED: Cell<u64> = const { Cell::new(0) };
}

/// How many texts this thread drew with GDI so far.
pub fn rendered() -> u64 {
    RENDERED.with(Cell::get)
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum FontRendering {
    #[default]
    ClearType,
    Antialiased,
}

impl FontRendering {
    fn gdi_quality(self) -> u8 {
        match self {
            Self::Antialiased => ANTIALIASED_QUALITY.0,
            Self::ClearType => CLEARTYPE_QUALITY.0,
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum TextAlign {
    #[default]
    Left,
    Center,
    Right,
}

/// How a line of text is drawn. Sizes are logical.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct TextStyle {
    pub family: &'static str,
    pub size: f64,
    /// GDI weight: 400 regular, 500 medium, 600 semibold.
    pub weight: i32,
    pub rendering: FontRendering,
    pub align: TextAlign,
    pub color: Rgba,
}

impl TextStyle {
    /// Segoe UI, as the Windows taskbar uses it.
    pub const fn segoe(size: f64, weight: i32, color: Rgba) -> Self {
        Self {
            family: "Segoe UI",
            size,
            weight,
            rendering: FontRendering::ClearType,
            align: TextAlign::Left,
            color,
        }
    }
}

unsafe fn create_font(family: &str, size: f64, weight: i32, rendering: FontRendering) -> HFONT {
    let name: Vec<u16> = family.encode_utf16().chain(Some(0)).collect();
    unsafe {
        CreateFontW(
            -(size.round() as i32),
            0,
            0,
            0,
            weight,
            0,
            0,
            0,
            DEFAULT_CHARSET,
            OUT_TT_PRECIS,
            CLIP_DEFAULT_PRECIS,
            FONT_QUALITY(rendering.gdi_quality()),
            (DEFAULT_PITCH.0 | FF_DONTCARE.0) as u32,
            PCWSTR::from_raw(name.as_ptr()),
        )
    }
}

/// One line of `text`, vertically centered in a `width` × `height` box of
/// device pixels and ellipsized to fit, blended into `target`. `size` is in
/// device pixels.
pub fn render_text(
    target: &mut [u32],
    width: u32,
    height: u32,
    text: &str,
    size: f64,
    style: &TextStyle,
) {
    // An empty UTF-16 buffer has a dangling pointer DrawTextW still probes.
    if text.is_empty() || width == 0 || height == 0 || target.is_empty() {
        return;
    }
    RENDERED.with(|count| count.set(count.get() + 1));
    unsafe {
        let memory_dc = CreateCompatibleDC(None);
        if memory_dc.is_invalid() {
            return;
        }
        let info = BITMAPINFO {
            bmiHeader: BITMAPINFOHEADER {
                biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
                biWidth: width as i32,
                biHeight: -(height as i32),
                biPlanes: 1,
                biBitCount: 32,
                biCompression: BI_RGB.0,
                ..Default::default()
            },
            ..Default::default()
        };
        let mut bits = std::ptr::null_mut();
        let bitmap = CreateDIBSection(Some(memory_dc), &info, DIB_RGB_COLORS, &mut bits, None, 0)
            .unwrap_or_default();
        if bitmap.is_invalid() || bits.is_null() {
            let _ = DeleteDC(memory_dc);
            return;
        }
        let old_bitmap = SelectObject(memory_dc, bitmap.into());
        std::ptr::write_bytes(bits, 0, width as usize * height as usize * 4);
        let font = create_font(style.family, size, style.weight, style.rendering);
        let old_font = SelectObject(memory_dc, font.into());
        let _ = SetBkMode(memory_dc, TRANSPARENT);
        let _ = SetTextColor(memory_dc, COLORREF(0x00FF_FFFF));
        let mut rect = RECT {
            left: 0,
            top: 0,
            right: width as i32,
            bottom: height as i32,
        };
        let mut wide: Vec<u16> = text.encode_utf16().collect();
        let alignment = match style.align {
            TextAlign::Left => DT_LEFT,
            TextAlign::Center => DT_CENTER,
            TextAlign::Right => DT_RIGHT,
        };
        let _ = DrawTextW(
            memory_dc,
            &mut wide,
            &mut rect,
            alignment | DT_VCENTER | DT_SINGLELINE | DT_END_ELLIPSIS | DT_NOPREFIX,
        );
        let mask = std::slice::from_raw_parts(bits as *const u32, width as usize * height as usize);
        let source = premultiply(style.color);
        for (destination, pixel) in target.iter_mut().zip(mask) {
            let coverage = coverage(*pixel, style.rendering);
            if coverage > 0.0 {
                blend(destination, source, coverage);
            }
        }
        SelectObject(memory_dc, old_font);
        let _ = DeleteObject(font.into());
        SelectObject(memory_dc, old_bitmap);
        let _ = DeleteObject(bitmap.into());
        let _ = DeleteDC(memory_dc);
    }
}

/// How much of a mask pixel the text covers, 0–1.
fn coverage(pixel: u32, rendering: FontRendering) -> f64 {
    let blue = (pixel & 0xFF) as f64;
    let green = ((pixel >> 8) & 0xFF) as f64;
    let red = ((pixel >> 16) & 0xFF) as f64;
    match rendering {
        FontRendering::ClearType => (red + green + blue) / (3.0 * 255.0),
        FontRendering::Antialiased => red.max(green).max(blue) / 255.0,
    }
}

/// Logical width of one line of `text` drawn with `style`.
pub fn measure_text(text: &str, style: &TextStyle) -> f64 {
    if text.is_empty() {
        return 0.0;
    }
    unsafe {
        let memory_dc = CreateCompatibleDC(None);
        if memory_dc.is_invalid() {
            return 0.0;
        }
        let font = create_font(style.family, style.size, style.weight, style.rendering);
        let old_font = SelectObject(memory_dc, font.into());
        let wide: Vec<u16> = text.encode_utf16().collect();
        let mut size = SIZE::default();
        let _ = GetTextExtentPoint32W(memory_dc, &wide, &mut size);
        SelectObject(memory_dc, old_font);
        let _ = DeleteObject(font.into());
        let _ = DeleteDC(memory_dc);
        f64::from(size.cx)
    }
}
