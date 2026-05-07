//! Compositor entry point — Phase 1 of the real Wayland compositor plan.
//!
//! The shell binary supports two run modes:
//!
//! - [`Mode::Hosted`] — the default. Runs the existing `winit`-on-host event
//!   loop (i.e. the shell renders inside an existing X11/Wayland session).
//!   This is what `cargo run -p quick-desktop` has always done and is the
//!   path used by every developer running on a normal Linux desktop.
//! - [`Mode::Native`] — placeholder for the future smithay-on-TTY compositor
//!   path. Phase 1 only wires the dispatch; the actual implementation lands
//!   in Phase 2+.
//!
//! Mode selection is driven by a single `--mode <hosted|native>` (or
//! `--mode=<...>`) command-line flag. Anything unrecognised — including the
//! flag being absent — falls back to [`Mode::Hosted`] so existing invocations
//! keep working.
//!
//! See `docs/superpowers/plans/2026-05-02-real-wayland-compositor.md`
//! (Phase 1) for the full design intent.

pub mod backend;
pub mod bridge;
pub mod frame;
pub mod hosted;
pub mod native;
pub mod state;

/// Which run mode the compositor should boot into.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    /// Default. Render inside an existing X11/Wayland host session via winit.
    Hosted,
    /// Future smithay-on-TTY compositor. Currently a stub — see
    /// [`native::run`].
    Native,
}

/// Parse the run mode from the process's command-line arguments.
///
/// Thin wrapper around [`parse_mode`] so callers don't need to materialise a
/// `Vec<String>` themselves. Tests should drive [`parse_mode`] directly with
/// a fixture slice; `std::env::args` is process-global and cannot be
/// monkey-patched.
pub fn parse_mode_from_args() -> Mode {
    let owned: Vec<String> = std::env::args().skip(1).collect();
    let borrowed: Vec<&str> = owned.iter().map(|s| s.as_str()).collect();
    parse_mode(&borrowed)
}

/// Pure, testable mode parser. Accepts both `--mode <value>` and
/// `--mode=<value>` forms. Unknown values (and a missing flag) default to
/// [`Mode::Hosted`] so the compositor never refuses to start over a typo.
pub fn parse_mode(args: &[&str]) -> Mode {
    let mut iter = args.iter().copied();
    while let Some(arg) = iter.next() {
        if let Some(value) = arg.strip_prefix("--mode=") {
            return value_to_mode(value);
        }
        if arg == "--mode" {
            if let Some(value) = iter.next() {
                return value_to_mode(value);
            }
            // Trailing `--mode` with no value: fall through to default.
        }
    }
    Mode::Hosted
}

fn value_to_mode(value: &str) -> Mode {
    match value {
        "native" => Mode::Native,
        _ => Mode::Hosted,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn no_flag_defaults_hosted() {
        assert!(matches!(parse_mode(&[]), Mode::Hosted));
    }

    #[test]
    fn dash_dash_mode_native() {
        assert!(matches!(parse_mode(&["--mode", "native"]), Mode::Native));
    }

    #[test]
    fn dash_dash_mode_eq_native() {
        assert!(matches!(parse_mode(&["--mode=native"]), Mode::Native));
    }

    #[test]
    fn unrecognized_value_defaults_hosted() {
        assert!(matches!(parse_mode(&["--mode", "junk"]), Mode::Hosted));
    }
}
