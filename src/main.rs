#![windows_subsystem = "windows"]

//! QpdfDesk — a small, safe Windows GUI for QPDF.
//!
//! Entry point only: the window and the FLTK event loop live in [`app`].

mod app;

fn main() {
    // Before anything can load a DLL by name.
    qpdf_desk::platform::harden_dll_search();
    app::run();
}
