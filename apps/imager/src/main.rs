//! Disk Imager — thin binary entry point. All logic lives in the library.

#[cfg(feature = "thin")]
extern crate runtime_dylib;
#[cfg(feature = "runtime-engine")]
extern crate tilo_accounts_engine;
#[cfg(all(feature = "runtime-engine", target_os = "linux"))]
extern crate tilo_fuse_engine;

fn main() {
    #[cfg(not(target_arch = "wasm32"))]
    ui::framework::run::<imager::ImagerShell>();
}
