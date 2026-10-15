//! Native Direct3D 11 / WARP host, run in a fresh dashboard process after
//! OpenGL fails. Windows supplies the software rasterizer; the painter embeds
//! precompiled shaders. Adapted from Taskbar Hub's `src/dashboard/warp.rs`
//! (Apache-2.0, see NOTICE).

use std::time::{Duration, Instant};

use eframe::egui;
use raw_window_handle::{HasWindowHandle, RawWindowHandle};
use windows::{
    Win32::{
        Foundation::{HMODULE, HWND},
        Graphics::{
            Direct3D::{D3D_DRIVER_TYPE_WARP, D3D_FEATURE_LEVEL_11_0},
            Direct3D11::{
                D3D11_CREATE_DEVICE_BGRA_SUPPORT, D3D11_SDK_VERSION, D3D11CreateDeviceAndSwapChain,
                ID3D11Device, ID3D11DeviceContext, ID3D11RenderTargetView, ID3D11Texture2D,
            },
            Dxgi::{
                Common::{
                    DXGI_FORMAT_R8G8B8A8_UNORM, DXGI_FORMAT_UNKNOWN, DXGI_MODE_DESC,
                    DXGI_SAMPLE_DESC,
                },
                DXGI_MWA_NO_ALT_ENTER, DXGI_PRESENT, DXGI_SWAP_CHAIN_DESC, DXGI_SWAP_CHAIN_FLAG,
                DXGI_SWAP_EFFECT_FLIP_DISCARD, DXGI_USAGE_RENDER_TARGET_OUTPUT, IDXGIFactory,
                IDXGISwapChain,
            },
        },
    },
    core::BOOL,
};
use winit::{
    application::ApplicationHandler,
    event::WindowEvent,
    event_loop::{ActiveEventLoop, ControlFlow, EventLoop},
    window::{Window, WindowId},
};

use super::{MIN_SIZE, Surface, initial_size};

/// Builds the dashboard content once the window exists; `false`: no Mica.
pub type CreateSurface = Box<dyn FnOnce(&egui::Context, &Window) -> Box<dyn Surface>>;

struct Painter {
    device: ID3D11Device,
    context: ID3D11DeviceContext,
    swap_chain: IDXGISwapChain,
    target: Option<ID3D11RenderTargetView>,
    renderer: egui_directx11::Renderer,
    size: [u32; 2],
}

impl Painter {
    fn new(window: &Window) -> Result<Self, String> {
        let RawWindowHandle::Win32(handle) = window
            .window_handle()
            .map_err(|error| error.to_string())?
            .as_raw()
        else {
            return Err("WARP requires a Windows window".into());
        };
        let size = window.inner_size();
        let desc = DXGI_SWAP_CHAIN_DESC {
            BufferDesc: DXGI_MODE_DESC {
                Width: size.width.max(1),
                Height: size.height.max(1),
                Format: DXGI_FORMAT_R8G8B8A8_UNORM,
                ..Default::default()
            },
            SampleDesc: DXGI_SAMPLE_DESC {
                Count: 1,
                Quality: 0,
            },
            BufferUsage: DXGI_USAGE_RENDER_TARGET_OUTPUT,
            BufferCount: 2,
            OutputWindow: HWND(handle.hwnd.get() as *mut _),
            Windowed: BOOL(1),
            SwapEffect: DXGI_SWAP_EFFECT_FLIP_DISCARD,
            ..Default::default()
        };
        let (mut device, mut context, mut swap_chain) = (None, None, None);
        unsafe {
            D3D11CreateDeviceAndSwapChain(
                None,
                D3D_DRIVER_TYPE_WARP,
                HMODULE::default(),
                D3D11_CREATE_DEVICE_BGRA_SUPPORT,
                Some(&[D3D_FEATURE_LEVEL_11_0]),
                D3D11_SDK_VERSION,
                Some(&desc),
                Some(&mut swap_chain),
                Some(&mut device),
                None,
                Some(&mut context),
            )
            .map_err(|error| format!("create D3D11 WARP device: {error}"))?;
        }
        let device = device.ok_or("WARP returned no device")?;
        let context = context.ok_or("WARP returned no context")?;
        let swap_chain = swap_chain.ok_or("WARP returned no swap chain")?;
        // Disable DXGI's automatic Alt+Enter transition; window state belongs to winit.
        unsafe {
            let factory: IDXGIFactory =
                swap_chain.GetParent().map_err(|error| error.to_string())?;
            factory
                .MakeWindowAssociation(desc.OutputWindow, DXGI_MWA_NO_ALT_ENTER)
                .map_err(|error| error.to_string())?;
        }
        let renderer = egui_directx11::Renderer::new(&device).map_err(|error| error.to_string())?;
        let mut painter = Self {
            device,
            context,
            swap_chain,
            renderer,
            target: None,
            size: [size.width.max(1), size.height.max(1)],
        };
        painter.create_target()?;
        Ok(painter)
    }

    fn create_target(&mut self) -> Result<(), String> {
        unsafe {
            let buffer: ID3D11Texture2D = self
                .swap_chain
                .GetBuffer(0)
                .map_err(|error| error.to_string())?;
            self.device
                .CreateRenderTargetView(&buffer, None, Some(&mut self.target))
                .map_err(|error| error.to_string())?;
        }
        Ok(())
    }

    fn paint(
        &mut self,
        size: [u32; 2],
        context: &egui::Context,
        output: egui_directx11::RendererOutput,
        clear: [f32; 4],
    ) -> Result<(), String> {
        if size != self.size {
            unsafe {
                self.context.OMSetRenderTargets(None, None);
                self.target = None;
                self.swap_chain
                    .ResizeBuffers(
                        0,
                        size[0],
                        size[1],
                        DXGI_FORMAT_UNKNOWN,
                        DXGI_SWAP_CHAIN_FLAG(0),
                    )
                    .map_err(|error| error.to_string())?;
            }
            self.size = size;
            self.create_target()?;
        }
        let target = self.target.as_ref().ok_or("WARP render target missing")?;
        unsafe {
            self.context.ClearRenderTargetView(target, &clear);
        }
        self.renderer
            .render(&self.context, target, context, output)
            .map_err(|error| error.to_string())?;
        unsafe {
            self.swap_chain
                .Present(1, DXGI_PRESENT(0))
                .ok()
                .map_err(|error| format!("WARP present: {error}"))?;
        }
        Ok(())
    }
}

struct Desktop {
    // Graphics resources go before the window.
    painter: Painter,
    app: Box<dyn Surface>,
    state: egui_winit::State,
    window: Window,
    close_requested: bool,
}

struct Repaint {
    at: Instant,
    pass: u64,
}

struct Host {
    context: egui::Context,
    create: Option<CreateSurface>,
    desktop: Option<Desktop>,
    next_repaint: Instant,
    error: Option<String>,
    painted: bool,
}

impl Host {
    fn initialize(&mut self, event_loop: &ActiveEventLoop) -> Result<(), String> {
        let window = event_loop
            .create_window(
                Window::default_attributes()
                    .with_title(super::TITLE)
                    .with_visible(false)
                    .with_inner_size(winit::dpi::LogicalSize::new(
                        initial_size()[0],
                        initial_size()[1],
                    ))
                    .with_min_inner_size(winit::dpi::LogicalSize::new(MIN_SIZE[0], MIN_SIZE[1])),
            )
            .map_err(|error| error.to_string())?;
        if let Some(monitor) = window.current_monitor() {
            let size = window.outer_size();
            let screen = monitor.size();
            let position = monitor.position();
            window.set_outer_position(winit::dpi::PhysicalPosition::new(
                position.x + (screen.width.saturating_sub(size.width) / 2) as i32,
                position.y + (screen.height.saturating_sub(size.height) / 2) as i32,
            ));
        }
        let painter = Painter::new(&window)?;
        let state = egui_winit::State::new(
            self.context.clone(),
            egui::ViewportId::ROOT,
            &window,
            Some(window.scale_factor() as f32),
            window.theme(),
            Some(16384),
        );
        self.context.set_embed_viewports(true);
        let create = self.create.take().ok_or("dashboard already created")?;
        let app = create(&self.context, &window);
        window.set_visible(true);
        window.request_redraw();
        self.desktop = Some(Desktop {
            painter,
            app,
            state,
            window,
            close_requested: false,
        });
        Ok(())
    }

    fn draw(&mut self, event_loop: &ActiveEventLoop) -> Result<(), String> {
        let Some(desktop) = &mut self.desktop else {
            return Ok(());
        };
        let size = desktop.window.inner_size();
        if !desktop.close_requested
            && (size.width == 0 || size.height == 0 || desktop.window.is_minimized() == Some(true))
        {
            self.next_repaint = Instant::now() + Duration::from_millis(100);
            return Ok(());
        }
        let info = desktop
            .state
            .egui_input_mut()
            .viewports
            .entry(egui::ViewportId::ROOT)
            .or_default();
        egui_winit::update_viewport_info(info, &self.context, &desktop.window, !self.painted);
        if desktop.close_requested {
            info.events.push(egui::ViewportEvent::Close);
        }
        let input = desktop.state.take_egui_input(&desktop.window);
        let output = self.context.run_ui(input, |ui| desktop.app.draw(ui));
        let (render, platform, viewports) = egui_directx11::split_output(output);
        desktop
            .state
            .handle_platform_output(&desktop.window, platform);
        let mut close = desktop.close_requested;
        desktop.close_requested = false;
        let mut delay = Duration::from_millis(500);
        if let Some(root) = viewports.get(&egui::ViewportId::ROOT) {
            delay = root.repaint_delay;
            close = close_after_commands(close, &root.commands);
            let mut info = egui::ViewportInfo::default();
            egui_winit::process_viewport_commands(
                &self.context,
                &mut info,
                root.commands
                    .iter()
                    .filter(|command| {
                        !matches!(
                            command,
                            egui::ViewportCommand::Close | egui::ViewportCommand::CancelClose
                        )
                    })
                    .cloned(),
                &desktop.window,
                &mut Vec::new(),
            );
        }
        let paint_size = if size.width == 0 || size.height == 0 {
            desktop.painter.size
        } else {
            [size.width, size.height]
        };
        // The swap chain is opaque: clear with the panel color.
        let clear = self
            .context
            .style_of(self.context.theme())
            .visuals
            .panel_fill
            .to_normalized_gamma_f32();
        desktop.painter.paint(
            paint_size,
            &self.context,
            render,
            [clear[0], clear[1], clear[2], 1.0],
        )?;
        self.painted = true;
        self.next_repaint = Instant::now() + delay.min(Duration::from_secs(1));
        if close {
            event_loop.exit();
        }
        Ok(())
    }

    fn fail(&mut self, event_loop: &ActiveEventLoop, error: String) {
        self.error = Some(error);
        event_loop.exit();
    }
}

fn close_after_commands(mut close: bool, commands: &[egui::ViewportCommand]) -> bool {
    for command in commands {
        match command {
            egui::ViewportCommand::Close => close = true,
            egui::ViewportCommand::CancelClose => close = false,
            _ => {}
        }
    }
    close
}

impl ApplicationHandler<Repaint> for Host {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        if self.desktop.is_none()
            && let Err(error) = self.initialize(event_loop)
        {
            self.fail(event_loop, error);
        }
    }

    fn user_event(&mut self, _event_loop: &ActiveEventLoop, event: Repaint) {
        // Repaint callbacks can arrive after their frame was already rendered.
        let current = self.context.cumulative_pass_nr();
        if current == event.pass || current == event.pass.saturating_add(1) {
            self.next_repaint = self.next_repaint.min(event.at);
        }
    }

    fn window_event(&mut self, event_loop: &ActiveEventLoop, id: WindowId, event: WindowEvent) {
        let Some(desktop) = &mut self.desktop else {
            return;
        };
        if id != desktop.window.id() {
            return;
        }
        let response = desktop.state.on_window_event(&desktop.window, &event);
        // egui-winit marks RedrawRequested as needing paint too. That event is
        // already being serviced below; scheduling another would render forever.
        if response.repaint && !matches!(event, WindowEvent::RedrawRequested) {
            desktop.window.request_redraw();
        }
        match event {
            WindowEvent::CloseRequested => {
                desktop.close_requested = true;
                // Minimized windows may never receive RedrawRequested.
                if let Err(error) = self.draw(event_loop) {
                    self.fail(event_loop, error);
                }
            }
            WindowEvent::RedrawRequested => {
                if let Err(error) = self.draw(event_loop) {
                    self.fail(event_loop, error);
                }
            }
            WindowEvent::Destroyed => event_loop.exit(),
            _ => {}
        }
    }

    fn about_to_wait(&mut self, event_loop: &ActiveEventLoop) {
        if let Some(desktop) = &self.desktop {
            if self.next_repaint <= Instant::now() {
                desktop.window.request_redraw();
                // Hidden or minimized windows may not receive redraw events.
                self.next_repaint = Instant::now() + Duration::from_millis(100);
            }
            event_loop.set_control_flow(ControlFlow::WaitUntil(self.next_repaint));
        }
    }

    fn exiting(&mut self, _event_loop: &ActiveEventLoop) {
        if let Some(mut desktop) = self.desktop.take() {
            desktop.app.on_close(&self.context);
        }
    }
}

pub fn run(create: CreateSurface) -> Result<(), String> {
    let event_loop = EventLoop::<Repaint>::with_user_event()
        .build()
        .map_err(|error| error.to_string())?;
    let context = egui::Context::default();
    let proxy = event_loop.create_proxy();
    context.set_request_repaint_callback(move |info| {
        if let Some(at) = Instant::now().checked_add(info.delay) {
            let _ = proxy.send_event(Repaint {
                at,
                pass: info.current_cumulative_pass_nr,
            });
        }
    });
    let mut host = Host {
        context,
        create: Some(create),
        desktop: None,
        next_repaint: Instant::now(),
        error: None,
        painted: false,
    };
    event_loop
        .run_app(&mut host)
        .map_err(|error| error.to_string())?;
    host.error.map_or(Ok(()), Err)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_cancelled_close_keeps_the_window() {
        assert!(!close_after_commands(
            true,
            &[egui::ViewportCommand::CancelClose]
        ));
        assert!(close_after_commands(false, &[egui::ViewportCommand::Close]));
        assert!(!close_after_commands(false, &[]));
        assert!(close_after_commands(true, &[]));
    }
}
