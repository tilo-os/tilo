//! Firewall — thin binary entry point. All logic lives in the library.
//!
//! The standard launcher serves every mode: the live window, `--shots`
//! captures (with `--scenario NAME`, `--viewport WxH`, `--theme light`) and
//! the headless drives. A capture shows the bundled example ruleset; only the
//! live window reads the host firewall.

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
/// the example ruleset and never interrogate the host firewall.
fn is_capture(args: impl Iterator<Item = String>) -> bool {
    let mut args = args;
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--data-dir" => {
                let _ = args.next();
            }
            _ if [
                "--shots",
                "--responsive-matrix",
                "--native-audit",
                "--headless",
            ]
            .iter()
            .any(|mode| arg == *mode || arg.starts_with(&format!("{mode}="))) =>
            {
                return true;
            }
            _ => {}
        }
    }
    false
}

pub fn main() {
    firewall::resource::read_host_firewall(!is_capture(std::env::args().skip(1)));
    ui::framework::run::<firewall::FirewallShell>();
}
