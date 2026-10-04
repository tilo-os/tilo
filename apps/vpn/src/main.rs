//! VPN — standard Tilo application entry point.
//!
//! The standard launcher serves every mode: the live window, `--shots`
//! captures (with `--scenario NAME`, `--viewport WxH`, `--theme light`) and
//! the headless drives. A capture shows the sample connections; the live
//! window only ever shows the person's own saved tunnels.

// An app runs as its own sandboxed process, so an integer that wraps here
// costs one app rather than the desktop, and proving every layout sum cannot
// overflow is a tax a third-party app author would pay too. The lints that
// produce a *crash* stay denied — indexing_slicing, string_slice,
// unwrap_used, expect_used, panic — because a panicking app is a bad app
// whoever wrote it. `shell`, `ui` and everything under `crates/` keep the
// full set: a fault there takes the session, or every app with it.
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
/// the sample connections and never touch the real `NetworkManager`.
fn is_capture(args: impl Iterator<Item = String>) -> bool {
    let mut args = args;
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--data-dir" => {
                let _ = args.next();
            }
            "--shots" | "--responsive-matrix" | "--native-audit" | "--headless" => return true,
            _ if arg.starts_with("--shots=") || arg.starts_with("--headless=") => return true,
            _ => {}
        }
    }
    false
}

#[allow(
    clippy::disallowed_methods,
    reason = "set TILO_VPN_SAMPLE once on the main thread before the framework host starts threads"
)]
pub fn main() {
    if is_capture(std::env::args().skip(1)) {
        // SAFETY: set once at startup before any threads read the environment.
        unsafe {
            std::env::set_var("TILO_VPN_SAMPLE", "1");
        }
    }
    ui::framework::run::<vpn::VpnShell>();
}
