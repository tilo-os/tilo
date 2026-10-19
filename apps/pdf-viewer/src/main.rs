//! PDF — standard Tilo application entry point.

#![cfg_attr(target_os = "windows", windows_subsystem = "windows")]

#[cfg(feature = "thin")]
extern crate runtime_dylib;
// The PDF engine comes from its engine library, not the runtime.
#[cfg(feature = "runtime-engine")]
extern crate tilo_pdf_engine;

fn main() {
    ui::framework::run::<pdf_viewer::PdfViewerShell>();
}
