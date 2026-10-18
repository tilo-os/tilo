#[cfg(feature = "thin")]
extern crate runtime_dylib;
// The PDF engine comes from its engine library, not the runtime.
#[cfg(feature = "runtime-engine")]
extern crate tilo_pdf_engine;

fn main() {
    ui::framework::run::<print_centre::PrintCentreShell>();
}
