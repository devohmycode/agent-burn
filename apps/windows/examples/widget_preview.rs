//! Draws the widget in several states at 100, 125, 150 and 200 % on dark
//! and light taskbar backgrounds, into one PNG, for visual checks.
//! `cargo run --example widget_preview -- <out.png>`

use std::collections::BTreeMap;

use agent_burn_windows::{
    quota::{
        CLAUDE, CLAUDE_SESSION, CODEX, CURSOR, Failure, QuotaHistory, QuotaReading, QuotaWindow,
    },
    settings::Provider,
    widget::{draw, view},
};

const NOW: i64 = 1_791_979_200_000;
const MINUTE: i64 = 60_000;

fn reading(series: &str, minutes_ago: i64, used: f64, minutes: f64, left: i64) -> QuotaReading {
    let observed = NOW - minutes_ago * MINUTE;
    let length = minutes * MINUTE as f64;
    let reset = NOW + left * MINUTE;
    QuotaReading {
        series: series.into(),
        observed_at_ms: observed,
        window: QuotaWindow {
            window_minutes: minutes,
            used_percent: used,
            elapsed_percent: (length - (reset - observed) as f64) / length * 100.0,
            api_equivalent_spent: 0.0,
        },
    }
}

fn history(readings: Vec<QuotaReading>, failures: &[(&str, &str)]) -> QuotaHistory {
    let mut by_series: BTreeMap<String, Vec<QuotaReading>> = BTreeMap::new();
    for reading in readings {
        by_series
            .entry(reading.series.clone())
            .or_default()
            .push(reading);
    }
    QuotaHistory {
        readings: by_series,
        failures: failures
            .iter()
            .map(|(series, message)| {
                (
                    series.to_string(),
                    Failure {
                        message: message.to_string(),
                        at_ms: NOW,
                    },
                )
            })
            .collect(),
    }
}

fn states() -> Vec<view::WidgetView> {
    vec![
        view::view(
            &history(
                vec![
                    reading(CLAUDE_SESSION, 1, 38.0, 300.0, 130),
                    reading(CLAUDE, 1, 46.0, 10_080.0, 3 * 1440 + 200),
                ],
                &[],
            ),
            Provider::Claude,
            NOW,
        ),
        view::view(
            &history(
                vec![
                    reading(CLAUDE_SESSION, 1, 93.0, 300.0, 50),
                    reading(CLAUDE, 1, 78.0, 10_080.0, 1440),
                ],
                &[],
            ),
            Provider::Claude,
            NOW,
        ),
        view::view(
            &history(vec![reading(CODEX, 25, 16.0, 10_080.0, 5 * 1440)], &[]),
            Provider::Codex,
            NOW,
        ),
        view::view(
            &history(vec![], &[(CLAUDE, "signed out")]),
            Provider::Claude,
            NOW,
        ),
        view::view(
            &history(vec![reading(CURSOR, 600, 10.0, 525_600.0, -5)], &[]),
            Provider::Cursor,
            NOW,
        ),
    ]
}

fn main() {
    let out = std::env::args().nth(1).expect("an output path");
    let scales = [1.0, 1.25, 1.5, 2.0];
    let views = states();
    let cell_width = (draw::WIDTH * 2.0) as u32 + 16;
    let cell_height = (draw::HEIGHT * 2.0) as u32 + 16;
    let width = cell_width * scales.len() as u32;
    let height = cell_height * views.len() as u32 * 2;
    let mut image = vec![0u32; (width * height) as usize];
    for (theme, dark) in [true, false].into_iter().enumerate() {
        let background: u32 = if dark { 0xFF20_2020 } else { 0xFFF3_F3F3 };
        for (row, shown) in views.iter().enumerate() {
            for (column, scale) in scales.iter().enumerate() {
                let canvas = draw::draw(shown, dark, *scale, NOW, false);
                let left = column as u32 * cell_width;
                let top = (theme * views.len() + row) as u32 * cell_height;
                for y in 0..cell_height {
                    for x in 0..cell_width {
                        image[((top + y) * width + left + x) as usize] = background;
                    }
                }
                for y in 0..canvas.height() {
                    for x in 0..canvas.width() {
                        let source = canvas.pixels()[(y * canvas.width() + x) as usize];
                        let target = &mut image[((top + 8 + y) * width + left + 8 + x) as usize];
                        *target = over(source, *target);
                    }
                }
            }
        }
    }
    std::fs::write(&out, png(width, height, &image)).unwrap();
    println!("{out}: {width}x{height}");
}

/// Premultiplied `source` over the opaque `target`.
fn over(source: u32, target: u32) -> u32 {
    let alpha = source >> 24;
    let channel = |shift: u32| {
        let s = (source >> shift) & 0xFF;
        let t = (target >> shift) & 0xFF;
        (s + t * (255 - alpha) / 255).min(255)
    };
    0xFF00_0000 | (channel(16) << 16) | (channel(8) << 8) | channel(0)
}

/// An uncompressed PNG of opaque `0xAARRGGBB` pixels.
fn png(width: u32, height: u32, pixels: &[u32]) -> Vec<u8> {
    let mut raw = Vec::new();
    for y in 0..height {
        raw.push(0);
        for x in 0..width {
            let pixel = pixels[(y * width + x) as usize];
            raw.extend([(pixel >> 16) as u8, (pixel >> 8) as u8, pixel as u8]);
        }
    }
    let mut zlib = vec![0x78, 0x01];
    for (index, block) in raw.chunks(65_535).enumerate() {
        let last = (index + 1) * 65_535 >= raw.len();
        zlib.push(u8::from(last));
        let length = block.len() as u16;
        zlib.extend(length.to_le_bytes());
        zlib.extend((!length).to_le_bytes());
        zlib.extend(block);
    }
    let (mut a, mut b) = (1u32, 0u32);
    for byte in &raw {
        a = (a + u32::from(*byte)) % 65_521;
        b = (b + a) % 65_521;
    }
    zlib.extend(((b << 16) | a).to_be_bytes());
    let mut header = Vec::new();
    header.extend(width.to_be_bytes());
    header.extend(height.to_be_bytes());
    header.extend([8, 2, 0, 0, 0]);
    let mut file = vec![0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A];
    for (kind, data) in [(*b"IHDR", header), (*b"IDAT", zlib), (*b"IEND", Vec::new())] {
        file.extend((data.len() as u32).to_be_bytes());
        let mut chunk = kind.to_vec();
        chunk.extend(&data);
        file.extend(&chunk);
        file.extend(crc32(&chunk).to_be_bytes());
    }
    file
}

fn crc32(bytes: &[u8]) -> u32 {
    let mut crc = 0xFFFF_FFFFu32;
    for byte in bytes {
        crc ^= u32::from(*byte);
        for _ in 0..8 {
            crc = if crc & 1 == 1 {
                (crc >> 1) ^ 0xEDB8_8320
            } else {
                crc >> 1
            };
        }
    }
    !crc
}
