//! The taskbar widget: a pixel canvas with GDI text, drawn into a layered
//! window docked in the taskbar or floating. Its rendering is adapted from
//! Taskbar Hub's `src/render/` (Apache-2.0, see NOTICE).

pub mod canvas;
pub mod color;
pub mod draw;
pub mod layout;
pub mod occupancy;
pub mod shapes;
pub mod shell;
pub mod text;
pub mod view;
pub mod window;
