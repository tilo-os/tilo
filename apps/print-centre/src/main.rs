#[cfg(feature = "thin")]
extern crate runtime_dylib;
#[cfg(feature = "runtime-engine")]
extern crate tilo_pdf_engine;

fn main() {
    ui::framework::run::<print_centre::PrintCentreShell>();
}
