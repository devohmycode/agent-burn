//! The dashboard window: drawn with OpenGL, or with Direct3D 11 / WARP in a
//! fresh process when OpenGL cannot start (`warp.rs`). The renderer choice
//! and fallback are adapted from Taskbar Hub's `src/dashboard/runner.rs`
//! (Apache-2.0, see NOTICE).

mod app;
pub mod clock;
pub mod format;
mod frame;
pub mod overview;
pub mod quota_history;
pub mod quotas;
mod settings_page;
pub mod usage;
mod warp;

use std::{ffi::OsString, path::PathBuf, process::ExitCode};

use eframe::egui;
use raw_window_handle::HasWindowHandle;

use crate::{cli_client::EngineLocation, hub};

pub use frame::NO_MICA_ENV;

pub const TITLE: &str = "Agent Burn";
/// Smallest dashboard window, in logical pixels.
pub const MIN_SIZE: [f32; 2] = [780.0, 540.0];
/// Size of the first dashboard window, in logical pixels.
pub const DEFAULT_SIZE: [f32; 2] = [1040.0, 700.0];
/// `WIDTHxHEIGHT` in logical pixels: another first size, for visual checks.
pub const SIZE_ENV: &str = "AGENT_BURN_APP_SIZE";

/// The size of the first dashboard window.
pub fn initial_size() -> [f32; 2] {
    std::env::var(SIZE_ENV)
        .ok()
        .and_then(|size| {
            let (width, height) = size.split_once('x')?;
            let size = [width.trim().parse().ok()?, height.trim().parse().ok()?];
            (size[0] >= MIN_SIZE[0] && size[1] >= MIN_SIZE[1]).then_some(size)
        })
        .unwrap_or(DEFAULT_SIZE)
}
/// `warp` starts with Direct3D 11 / WARP. `fail-opengl` and `fail-all`
/// make OpenGL, or both renderers, fail at start, to check the fallback.
pub const RENDERER_ENV: &str = "AGENT_BURN_RENDERER";
/// The OpenGL failure, handed to the WARP process for its error message.
const OPENGL_ERROR_ENV: &str = "AGENT_BURN_OPENGL_ERROR";

/// The content of the dashboard window, whatever draws it.
pub trait Surface {
    fn draw(&mut self, ui: &mut egui::Ui);
    /// Called once, when the window closes.
    fn on_close(&mut self, context: &egui::Context);
}

/// How the dashboard was started.
#[derive(Clone, Debug, Default)]
pub struct Launch {
    pub engine: EngineLocation,
    /// Saved engine outputs to show instead of running the engine.
    pub fixture: Option<PathBuf>,
    /// This process is the WARP fallback.
    pub warp: bool,
    /// The arguments to start the WARP process with.
    pub args: Vec<OsString>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Renderer {
    OpenGl,
    Warp,
}

pub fn run(launch: Launch) -> ExitCode {
    // One dashboard per application folder. Fixtures are views for checks
    // and run apart, without the background process.
    let mut instance = None;
    if launch.fixture.is_none()
        && let Some(folder) = crate::settings::directory()
    {
        match hub::instance::claim("Dashboard", &folder) {
            Some(claimed) => instance = Some(claimed),
            None => {
                if let Some(hwnd) = hub::instance::dashboard_window(&folder) {
                    hub::instance::activate(hwnd);
                }
                return ExitCode::SUCCESS;
            }
        }
        // The widget and the notification-area icon live there.
        if hub::instance::hub_window(&folder).is_none() {
            let mut arguments = vec!["--widget".to_string()];
            if let EngineLocation::Custom(engine) = &launch.engine {
                arguments.extend(["--engine".to_string(), engine.display().to_string()]);
            }
            let _ = hub::instance::spawn(&arguments);
        }
    }
    let simulated = std::env::var(RENDERER_ENV).unwrap_or_default();
    let result = if launch.warp || simulated == "warp" {
        run_warp(&launch, &simulated).map_err(|error| match std::env::var(OPENGL_ERROR_ENV) {
            Ok(opengl) => format!("OpenGL: {opengl}\nWARP: {error}"),
            Err(_) => format!("WARP: {error}"),
        })
    } else {
        run_with_fallback(|renderer, opengl_error| match renderer {
            Renderer::OpenGl => run_opengl(&launch, &simulated),
            Renderer::Warp => {
                // The WARP process is the dashboard now.
                drop(instance.take());
                relaunch_with_warp(&launch, opengl_error.unwrap_or_default())
                    .map_err(|error| eframe::Error::AppCreation(error.into()))
            }
        })
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(message) => {
            crate::native::error_box(
                "Agent Burn could not open its window",
                &format!(
                    "Neither OpenGL nor the Direct3D software renderer could draw the dashboard.\n\n{message}"
                ),
            );
            ExitCode::FAILURE
        }
    }
}

/// Runs OpenGL, then WARP once if OpenGL itself failed; never again.
fn run_with_fallback(
    mut run: impl FnMut(Renderer, Option<&str>) -> eframe::Result,
) -> Result<(), String> {
    let opengl_error = match run(Renderer::OpenGl, None) {
        Ok(()) => return Ok(()),
        Err(error) => error,
    };
    // Context and configuration failures can occur before the painter exists.
    if matches!(
        &opengl_error,
        eframe::Error::OpenGL(_) | eframe::Error::Glutin(_) | eframe::Error::NoGlutinConfigs(..)
    ) {
        run(Renderer::Warp, Some(&opengl_error.to_string()))
            .map_err(|error| format!("OpenGL: {opengl_error}\nWARP: {error}"))
    } else {
        Err(format!("OpenGL: {opengl_error}"))
    }
}

/// winit cannot create a second event loop in a process that had one, so
/// WARP runs in a new process, which never falls back again.
fn relaunch_with_warp(launch: &Launch, opengl_error: &str) -> Result<(), String> {
    let executable = std::env::current_exe().map_err(|error| error.to_string())?;
    std::process::Command::new(executable)
        .args(&launch.args)
        .arg("--warp")
        .env(OPENGL_ERROR_ENV, opengl_error)
        .spawn()
        .map(drop)
        .map_err(|error| error.to_string())
}

fn create_app(
    context: &egui::Context,
    window: &impl HasWindowHandle,
    mica: bool,
    launch: &Launch,
) -> Box<dyn Surface> {
    let frame = frame::NativeFrame::new(window, mica);
    Box::new(app::DashboardApp::new(context, frame, launch))
}

fn run_opengl(launch: &Launch, simulated: &str) -> eframe::Result {
    if matches!(simulated, "fail-opengl" | "fail-all") {
        return Err(eframe::Error::OpenGL(
            format!("OpenGL disabled by {RENDERER_ENV}={simulated}").into(),
        ));
    }
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title(TITLE)
            .with_inner_size(initial_size())
            .with_min_inner_size(MIN_SIZE)
            .with_transparent(true),
        renderer: eframe::Renderer::Glow,
        centered: true,
        ..Default::default()
    };
    let launch = launch.clone();
    eframe::run_native(
        "AgentBurn.Dashboard",
        options,
        Box::new(move |context| {
            let surface = create_app(&context.egui_ctx, context, true, &launch);
            Ok(Box::new(EframeSurface {
                surface,
                context: context.egui_ctx.clone(),
            }))
        }),
    )
}

fn run_warp(launch: &Launch, simulated: &str) -> Result<(), String> {
    if simulated == "fail-all" {
        return Err(format!("WARP disabled by {RENDERER_ENV}={simulated}"));
    }
    let launch = launch.clone();
    // The WARP swap chain is opaque: no Mica.
    warp::run(Box::new(move |context, window| {
        create_app(context, window, false, &launch)
    }))
}

struct EframeSurface {
    surface: Box<dyn Surface>,
    context: egui::Context,
}

impl eframe::App for EframeSurface {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        self.surface.draw(ui);
    }

    fn clear_color(&self, _visuals: &egui::Visuals) -> [f32; 4] {
        // Mica, when shown, fills what the dashboard leaves transparent.
        [0.0; 4]
    }

    fn on_exit(&mut self, _gl: Option<&eframe::glow::Context>) {
        self.surface.on_close(&self.context);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_working_opengl_is_not_retried() {
        let mut attempts = Vec::new();
        run_with_fallback(|renderer, _| {
            attempts.push(renderer);
            Ok(())
        })
        .unwrap();

        assert_eq!(attempts, [Renderer::OpenGl]);
    }

    #[test]
    fn an_opengl_failure_tries_warp_once_with_its_message() {
        let mut attempts = Vec::new();
        run_with_fallback(|renderer, opengl_error| {
            attempts.push(renderer);
            match renderer {
                Renderer::OpenGl => Err(eframe::Error::OpenGL("no OpenGL 2.0".to_owned().into())),
                Renderer::Warp => {
                    assert!(opengl_error.unwrap().contains("no OpenGL 2.0"));
                    Ok(())
                }
            }
        })
        .unwrap();

        assert_eq!(attempts, [Renderer::OpenGl, Renderer::Warp]);
    }

    #[test]
    fn two_failures_are_reported_together_without_a_third_try() {
        let mut attempts = Vec::new();
        let error = run_with_fallback(|renderer, _| {
            attempts.push(renderer);
            match renderer {
                Renderer::OpenGl => Err(eframe::Error::OpenGL("no OpenGL".to_owned().into())),
                Renderer::Warp => Err(eframe::Error::AppCreation("no WARP".into())),
            }
        })
        .unwrap_err();

        assert_eq!(attempts, [Renderer::OpenGl, Renderer::Warp]);
        assert!(error.contains("OpenGL: ") && error.contains("no OpenGL"));
        assert!(error.contains("WARP: ") && error.contains("no WARP"));
    }

    #[test]
    fn application_errors_do_not_switch_renderer() {
        let mut attempts = Vec::new();
        let error = run_with_fallback(|renderer, _| {
            attempts.push(renderer);
            Err(eframe::Error::AppCreation("bad state".into()))
        })
        .unwrap_err();

        assert_eq!(attempts, [Renderer::OpenGl]);
        assert!(error.contains("bad state"));
    }
}
