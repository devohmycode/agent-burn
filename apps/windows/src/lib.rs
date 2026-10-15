//! Agent Burn for Windows: the dashboard, the taskbar widget and the quota
//! collector, all fed by the bundled `agent-burn.exe` engine.

pub mod cli_client;
pub mod collector;
pub mod dashboard;
pub mod dates;
pub mod files;
pub mod forecast;
pub mod history;
pub mod hub;
pub mod mode;
pub mod models;
pub mod native;
pub mod quota;
pub mod redact;
pub mod scheduled;
pub mod settings;
pub mod startup;
pub mod state;
pub mod ui;
pub mod update;
pub mod widget;
pub mod windows_theme;
