//! Disk Imager — thin binary entry point. All logic lives in the library.

fn main() {
    #[cfg(not(target_arch = "wasm32"))]
    ui::framework::run::<imager::ImagerShell>();
}
