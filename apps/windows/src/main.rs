#![windows_subsystem = "windows"]

use std::process::ExitCode;

use agent_burn_windows::{
    cli_client::EngineLocation,
    collector::{self, Collection},
    dashboard::{self, Launch},
    hub,
    mode::{self, StartMode},
    settings,
};

fn main() -> ExitCode {
    let args: Vec<_> = std::env::args_os().skip(1).collect();
    let Ok(options) = mode::parse(args.clone()) else {
        return ExitCode::from(mode::EXIT_USAGE);
    };
    if let Some(data) = &options.data {
        // SAFETY: set once, before any other thread starts.
        unsafe { std::env::set_var(settings::DIRECTORY_ENV, data) };
    }
    match options.mode {
        StartMode::Dashboard => dashboard::run(Launch {
            engine: options
                .engine
                .map_or(EngineLocation::Bundled, EngineLocation::Custom),
            fixture: options.fixture,
            warp: options.warp,
            // The WARP process gets the same arguments, plus `--warp`.
            args: args
                .into_iter()
                .filter(|argument| argument != "--warp")
                .collect(),
        }),
        StartMode::Collect => match collector::run_once(options.engine) {
            Collection::Failed(_) => ExitCode::FAILURE,
            Collection::Disabled | Collection::Offline | Collection::Busy | Collection::Done(_) => {
                ExitCode::SUCCESS
            }
        },
        StartMode::Widget => hub::run(options.engine),
    }
}
