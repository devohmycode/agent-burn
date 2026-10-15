//! Checks the widget window on this machine without any input: GDI and USER
//! handles after windows opened and closed, focus during refreshes,
//! transparency against what the screen showed before, and which window
//! receives clicks. A small widget shows near the bottom-right corner for a
//! few seconds. `cargo run --example widget_check`

use std::{
    thread,
    time::{Duration, Instant},
};

use agent_burn_windows::{
    quota::{CLAUDE, CLAUDE_SESSION, QuotaHistory, QuotaReading, QuotaWindow},
    settings::Provider,
    widget::{
        canvas::Canvas,
        draw, shell, view,
        window::{WidgetWindow, passive_procedure},
    },
};
use windows::Win32::{
    Foundation::{POINT, RECT},
    Graphics::Gdi::{
        BI_RGB, BITMAPINFO, BITMAPINFOHEADER, BitBlt, CreateCompatibleDC, CreateDIBSection,
        DIB_RGB_COLORS, DeleteDC, DeleteObject, GetDC, ReleaseDC, SRCCOPY, SelectObject,
    },
    System::Threading::{GR_GDIOBJECTS, GR_USEROBJECTS, GetCurrentProcess, GetGuiResources},
    UI::WindowsAndMessaging::*,
};

fn pump() {
    let mut message = MSG::default();
    unsafe {
        while PeekMessageW(&mut message, None, 0, 0, PM_REMOVE).as_bool() {
            let _ = TranslateMessage(&message);
            DispatchMessageW(&message);
        }
    }
}

fn handles() -> (u32, u32) {
    unsafe {
        let process = GetCurrentProcess();
        (
            GetGuiResources(process, GR_GDIOBJECTS),
            GetGuiResources(process, GR_USEROBJECTS),
        )
    }
}

fn canvas(scale: f64) -> Canvas {
    let now = 1_791_979_200_000;
    let reading = |series: &str, used: f64, minutes: f64| QuotaReading {
        series: series.into(),
        observed_at_ms: now - 60_000,
        window: QuotaWindow {
            window_minutes: minutes,
            used_percent: used,
            elapsed_percent: 40.0,
            api_equivalent_spent: 0.0,
        },
    };
    let mut history = QuotaHistory::default();
    history.readings.insert(
        CLAUDE_SESSION.into(),
        vec![reading(CLAUDE_SESSION, 38.0, 300.0)],
    );
    history
        .readings
        .insert(CLAUDE.into(), vec![reading(CLAUDE, 46.0, 10_080.0)]);
    draw::draw(
        &view::view(&history, Provider::Claude, now),
        true,
        scale,
        now,
        false,
    )
}

/// Screen pixels of `rect`, as `0x00RRGGBB`.
fn screen(rect: RECT) -> Vec<u32> {
    let (width, height) = (rect.right - rect.left, rect.bottom - rect.top);
    unsafe {
        let screen_dc = GetDC(None);
        let memory_dc = CreateCompatibleDC(Some(screen_dc));
        let info = BITMAPINFO {
            bmiHeader: BITMAPINFOHEADER {
                biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
                biWidth: width,
                biHeight: -height,
                biPlanes: 1,
                biBitCount: 32,
                biCompression: BI_RGB.0,
                ..Default::default()
            },
            ..Default::default()
        };
        let mut bits = std::ptr::null_mut();
        let bitmap =
            CreateDIBSection(Some(memory_dc), &info, DIB_RGB_COLORS, &mut bits, None, 0).unwrap();
        let old = SelectObject(memory_dc, bitmap.into());
        let _ = BitBlt(
            memory_dc,
            0,
            0,
            width,
            height,
            Some(screen_dc),
            rect.left,
            rect.top,
            SRCCOPY,
        );
        let pixels = std::slice::from_raw_parts(bits as *const u32, (width * height) as usize)
            .iter()
            .map(|pixel| pixel & 0x00FF_FFFF)
            .collect();
        SelectObject(memory_dc, old);
        let _ = DeleteObject(bitmap.into());
        let _ = DeleteDC(memory_dc);
        ReleaseDC(None, screen_dc);
        pixels
    }
}

fn main() {
    let monitor = shell::find_monitors()
        .into_iter()
        .next()
        .expect("a monitor");
    let scale = 1.0_f64.max(agent_burn_windows::widget::window::monitor_scale(
        monitor.handle,
    ));
    let sample = canvas(scale);
    let (width, height) = (sample.width() as i32, sample.height() as i32);
    let rect = RECT {
        left: monitor.work.right - width - 24,
        top: monitor.work.bottom - height - 24,
        right: monitor.work.right - 24,
        bottom: monitor.work.bottom - 24,
    };
    println!(
        "scale {scale}, widget {width}x{height} at {},{}",
        rect.left, rect.top
    );

    // Handles: one window first, so lazy system objects are counted once.
    let warm = WidgetWindow::create(Some(passive_procedure)).unwrap();
    warm.present(&sample);
    warm.destroy();
    pump();
    let before = handles();
    for _ in 0..50 {
        let window = WidgetWindow::create(Some(passive_procedure)).unwrap();
        for _ in 0..5 {
            window.present(&canvas(scale));
        }
        window.float_at(rect);
        pump();
        window.destroy();
        pump();
    }
    let after = handles();
    println!("handles GDI/USER before {before:?}, after 50 windows {after:?}");
    let window = WidgetWindow::create(Some(passive_procedure)).unwrap();
    window.float_at(rect);
    window.present(&sample);
    pump();
    let open = handles();
    for _ in 0..200 {
        window.present(&canvas(scale));
        pump();
    }
    println!(
        "handles GDI/USER with the widget open {open:?}, after 200 redraws {:?}",
        handles()
    );
    window.destroy();
    pump();

    // Transparency and focus.
    let background = screen(rect);
    let foreground = unsafe { GetForegroundWindow() };
    let window = WidgetWindow::create(Some(passive_procedure)).unwrap();
    window.present(&sample);
    window.float_at(rect);
    pump();
    let started = Instant::now();
    let mut stole_focus = false;
    while started.elapsed() < Duration::from_secs(3) {
        window.present(&canvas(scale));
        window.float_at(rect);
        pump();
        let now = unsafe { GetForegroundWindow() };
        stole_focus |= now == window.hwnd || now != foreground;
        thread::sleep(Duration::from_millis(100));
    }
    let shown = screen(rect);
    let (mut clear_same, mut clear_total, mut solid_same, mut solid_total) = (0, 0, 0, 0);
    for (index, pixel) in sample.pixels().iter().enumerate() {
        let close = |a: u32, b: u32| {
            (0..3).all(|shift| ((a >> (8 * shift)) & 0xFF).abs_diff((b >> (8 * shift)) & 0xFF) <= 2)
        };
        match pixel >> 24 {
            0 => {
                clear_total += 1;
                clear_same += usize::from(close(shown[index], background[index]));
            }
            0xFF => {
                solid_total += 1;
                solid_same += usize::from(close(shown[index], pixel & 0x00FF_FFFF));
            }
            _ => {}
        }
    }
    println!(
        "transparent pixels showing what was beneath: {clear_same}/{clear_total}; opaque pixels showing the widget: {solid_same}/{solid_total}"
    );
    let ex_style = unsafe { GetWindowLongW(window.hwnd, GWL_EXSTYLE) } as u32;
    println!(
        "no activation style: {}; focus taken during 3 s of refreshes: {stole_focus}",
        ex_style & WS_EX_NOACTIVATE.0 != 0
    );
    // A transparent corner and the middle both receive clicks; outside does not.
    let inside = [
        POINT {
            x: rect.left + 1,
            y: rect.top + 1,
        },
        POINT {
            x: (rect.left + rect.right) / 2,
            y: (rect.top + rect.bottom) / 2,
        },
    ];
    let outside = POINT {
        x: rect.left - 3,
        y: rect.top - 3,
    };
    let hit = |point: POINT| unsafe { WindowFromPoint(point) } == window.hwnd;
    println!(
        "clicks reach the widget: corner {}, middle {}; outside {}",
        hit(inside[0]),
        hit(inside[1]),
        hit(outside)
    );
    window.destroy();
    pump();
}
