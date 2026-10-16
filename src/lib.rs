//! QpdfDesk — library crate holding everything that is not the window: the
//! QPDF core, settings, the single-instance lock, shared Win32 helpers and the
//! app icon.
//!
//! All of it is deliberately UI-agnostic (no `fltk` dependency) so it can be
//! tested without opening a window. The FLTK front-end lives in
//! the binary crate (`src/main.rs`, `src/app/`).

pub mod core;
#[cfg(test)]
mod fuzz;
/// The window/executable icon, drawn in code and shared with `build.rs`.
pub mod icon;
pub mod instance;
pub mod platform;
/// Named sets of steps and options.
pub mod profiles;
/// The Check report.
pub mod report;
pub mod settings;
pub mod text;
/// Background work: finding QPDF, adding files, the probe and the batch.
pub mod tasks;

// Win32 is called directly throughout; there is no other target.
#[cfg(not(windows))]
compile_error!("QpdfDesk is a Windows application");
