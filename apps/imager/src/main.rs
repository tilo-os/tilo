//! Disk Imager — thin binary entry point. All logic lives in the library.
//!
//! The binary is the framework app [`imager::ImagerShell`]: the write wizard,
//! the guided setup with `--setup`, the folder imports with
//! `--import-folders`, and the standard `--shots`, `--shots-dir` and
//! `--headless` modes over its scenarios. Started with a helper's own command
//! line it is that helper instead (the privileged writer, the staging steps,
//! the packaging probes), which the app declares as its helpers.

#[cfg(feature = "thin")]
extern crate runtime_dylib;

ui::main!(imager::ImagerShell);
