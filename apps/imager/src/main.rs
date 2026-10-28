//! Disk Imager — thin binary entry point. All logic lives in the library.
//!
//! The binary is the framework app [`imager::ImagerShell`]: the write wizard,
//! the guided setup with `--setup`, the folder imports with
//! `--import-folders`, the setup with example data `--demo` (those three with
//! `installation`; the Tilo app is the write wizard alone), and the standard
//! `--shots`, `--shots-dir` and `--headless-drive` semantic actions on the
//! production app. Shots render its scenarios. Started with a helper's own command
//! line it is that helper instead (the privileged writer, the staging steps,
//! the packaging probes), which the app declares as its helpers.

#[cfg(feature = "thin")]
extern crate runtime_dylib;
// Account custody with the WebDAV client, and the projection worker's FUSE
// presentation, come from their engine libraries, not the tapp.
#[cfg(feature = "runtime-engine")]
extern crate tilo_accounts_engine;
#[cfg(all(feature = "runtime-engine", target_os = "linux"))]
extern crate tilo_fuse_engine;

fn main() {
    #[cfg(not(target_arch = "wasm32"))]
    ui::framework::run::<imager::ImagerShell>();
}
