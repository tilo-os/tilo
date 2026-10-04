//! Disks — thin binary entry point. All logic lives in the library.
//!
//! The standard launcher serves every mode: the live window, `--shots`
//! captures (with `--scenario NAME`, `--viewport WxH`, `--theme light`) and
//! the headless drives. A capture reads the curated sample drives; the live
//! window probes the real system.

// Apps run in isolated processes, so app-local arithmetic checks are relaxed.
// Panic-producing lints remain denied; shared crates retain the full lint set
// because a fault there can affect the session or every app.
#![allow(
    clippy::arithmetic_side_effects,
    clippy::cast_possible_truncation,
    clippy::cast_precision_loss,
    clippy::cast_sign_loss,
    clippy::cast_possible_wrap,
    reason = "app-local arithmetic; see the note above"
)]

#[cfg(feature = "thin")]
extern crate runtime_dylib;

/// Whether this launch is a capture or a headless drive, which must stay on
/// the sample drives and never touch the real devices.
fn is_capture(args: impl Iterator<Item = String>) -> bool {
    let mut args = args;
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--data-dir" => {
                let _ = args.next();
            }
            "--shots" | "--responsive-matrix" | "--native-audit" | "--headless" => return true,
            _ => {}
        }
    }
    false
}

#[allow(
    clippy::disallowed_methods,
    reason = "set DISKS_LIVE once on the main thread before the framework host starts threads"
)]
pub fn main() {
    if !is_capture(std::env::args().skip(1)) {
        // Live mode: probe the real system (lsblk + smartctl). Set once before
        // the framework host starts threads.
        // SAFETY: set once at startup before any threads read the environment.
        unsafe {
            std::env::set_var("DISKS_LIVE", "1");
        }
    }
    ui::framework::run::<disks::DisksShell>();
}
