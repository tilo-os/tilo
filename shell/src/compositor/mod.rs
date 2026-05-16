//! Compositor entry point — Phase 1 of the real Wayland compositor plan.
//!
//! The shell binary supports two run modes:
//!
//! - [`Mode::Native`] — the default as of Phase 7. Spins up a real
//!   smithay-driven Wayland server (auto-binds `wayland-N`), advertises the
//!   compositor / xdg-shell / shm / seat / output globals, and paints chrome
//!   through the vello+wgpu backend wired up in Phase 4. This is what
//!   `cargo run -p quick-desktop` does with no flags.
//! - [`Mode::Hosted`] — the legacy `winit`-on-host iced loop, kept as an
//!   explicit dev opt-in via `--mode hosted`. Useful when no DRM/KMS or
//!   spare Wayland session is available (e.g. running inside another
//!   compositor that you don't want to nest into).
//!
//! Mode selection is driven by a single `--mode <hosted|native>` (or
//! `--mode=<...>`) command-line flag. Anything unrecognised — including the
//! flag being absent — falls back to [`Mode::Native`] (the Phase 7 default
//! flip). `--mode hosted` is the explicit opt-in for the legacy path.
//!
//! See `docs/superpowers/plans/2026-05-02-real-wayland-compositor.md`
//! (Phases 1 and 7) for the full design intent.

pub mod backend;
pub mod bridge;
pub mod frame;
pub mod hosted;
pub mod native;
pub mod state;
pub mod tab_binding;

/// Which run mode the compositor should boot into.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    /// Default. Render inside an existing X11/Wayland host session via winit.
    Hosted,
    /// Future smithay-on-TTY compositor. Currently a stub — see
    /// [`native::run`].
    Native,
}

/// Which backend `Mode::Native` should drive. Mirrors `--mode` in shape:
/// a single `--backend <winit|drm>` (or `--backend=<...>`) flag, with any
/// unknown value (or absence) falling back to [`BackendKind::Winit`].
///
/// `Drm` is a Phase 5 skeleton — the flag is wired through so the
/// follow-up TTY implementation plugs in without touching the shell
/// entry point. See [`backend::DrmBackend`] for the deferred work.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BackendKind {
    /// Default. Host on an existing Wayland/X11 session via `winit`.
    Winit,
    /// Phase 5 stub: drive a TTY directly via DRM/KMS + libinput.
    /// Currently errors at construction with a "not yet implemented"
    /// message — see [`backend::DrmBackend`].
    Drm,
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

/// Process-global wrapper around [`parse_peer_sync`]. v1 default OFF —
/// callers must opt in via `--enable-peer-sync` to spin up the libp2p
/// stack. Used by `main` to decide whether to construct a
/// [`crate::shell::network::ShellNetwork`].
pub fn parse_peer_sync_from_args() -> bool {
    let owned: Vec<String> = std::env::args().skip(1).collect();
    let borrowed: Vec<&str> = owned.iter().map(|s| s.as_str()).collect();
    parse_peer_sync(&borrowed)
}

/// Pure, testable `--enable-peer-sync` flag parser. Returns `true` iff
/// the flag is present anywhere in `args`. Mirrors [`parse_mode`]'s
/// "parse from a borrowed slice" shape so tests can drive it with a
/// fixture without touching `std::env`.
pub fn parse_peer_sync(args: &[&str]) -> bool {
    args.iter().any(|a| *a == "--enable-peer-sync")
}

/// Process-global wrapper around [`parse_portals`]. Sandbox Phase 4.3:
/// callers opt in via `--enable-portals` to spin up the
/// `xdg-desktop-portal` D-Bus backend (FileChooser / ScreenCast /
/// Access). v1 default OFF — the same opt-in shape `parse_peer_sync`
/// uses, so the hosted entry point keeps working bit-for-bit without
/// the flag.
pub fn parse_portals_from_args() -> bool {
    let owned: Vec<String> = std::env::args().skip(1).collect();
    let borrowed: Vec<&str> = owned.iter().map(|s| s.as_str()).collect();
    parse_portals(&borrowed)
}

/// Pure, testable `--enable-portals` flag parser. Mirrors
/// [`parse_peer_sync`]'s shape so the hosted boot path treats both
/// flags identically (`main.rs` parses each from `std::env::args` and
/// hands the boolean to `hosted::run_with_options`).
pub fn parse_portals(args: &[&str]) -> bool {
    args.iter().any(|a| *a == "--enable-portals")
}

/// Pure, testable mode parser. Accepts both `--mode <value>` and
/// `--mode=<value>` forms. Unknown values (and a missing flag) default to
/// [`Mode::Native`] (the Phase 7 flip) so the compositor never refuses to
/// start over a typo. `--mode hosted` is the explicit dev opt-in for the
/// legacy iced-on-winit path.
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
    Mode::Native
}

fn value_to_mode(value: &str) -> Mode {
    match value {
        "hosted" => Mode::Hosted,
        _ => Mode::Native,
    }
}

/// Process-global wrapper around [`parse_backend`]. Used by
/// [`native::run`] to decide which [`backend::Backend`] impl to
/// instantiate inside `--mode native`.
pub fn parse_backend_from_args() -> BackendKind {
    let owned: Vec<String> = std::env::args().skip(1).collect();
    let borrowed: Vec<&str> = owned.iter().map(|s| s.as_str()).collect();
    parse_backend(&borrowed)
}

/// Pure, testable backend parser. Mirrors [`parse_mode`] — accepts both
/// `--backend <value>` and `--backend=<value>` forms, defaults to
/// [`BackendKind::Winit`] for unknown / missing values so a typo never
/// refuses to start the compositor.
pub fn parse_backend(args: &[&str]) -> BackendKind {
    let mut iter = args.iter().copied();
    while let Some(arg) = iter.next() {
        if let Some(value) = arg.strip_prefix("--backend=") {
            return value_to_backend(value);
        }
        if arg == "--backend" {
            if let Some(value) = iter.next() {
                return value_to_backend(value);
            }
            // Trailing `--backend` with no value: fall through to default.
        }
    }
    BackendKind::Winit
}

fn value_to_backend(value: &str) -> BackendKind {
    match value {
        "drm" => BackendKind::Drm,
        _ => BackendKind::Winit,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn no_flag_defaults_native() {
        // Phase 7 flip: the default is `Native` so `cargo run -p
        // quick-desktop` (no flags) boots the real smithay server.
        assert!(matches!(parse_mode(&[]), Mode::Native));
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
    fn dash_dash_mode_hosted_returns_hosted() {
        // `--mode hosted` is the Phase 7 explicit dev opt-in for the
        // legacy iced-on-winit path; this guards that the hosted
        // fallback path stays reachable from the CLI.
        assert!(matches!(parse_mode(&["--mode", "hosted"]), Mode::Hosted));
        assert!(matches!(parse_mode(&["--mode=hosted"]), Mode::Hosted));
    }

    #[test]
    fn unrecognized_value_defaults_native() {
        // Phase 7 flip: a typo no longer silently demotes to the
        // hosted iced path; it falls through to the real Wayland
        // server. The hosted path is opt-in (`--mode hosted`).
        assert!(matches!(parse_mode(&["--mode", "junk"]), Mode::Native));
    }

    #[test]
    fn peer_sync_flag_default_off() {
        assert!(!parse_peer_sync(&[]));
        assert!(!parse_peer_sync(&["--mode", "hosted"]));
    }

    #[test]
    fn peer_sync_flag_present_anywhere() {
        assert!(parse_peer_sync(&["--enable-peer-sync"]));
        assert!(parse_peer_sync(&["--mode", "hosted", "--enable-peer-sync"]));
        assert!(parse_peer_sync(&["--enable-peer-sync", "--mode=native"]));
    }

    #[test]
    fn portals_flag_default_off() {
        assert!(!parse_portals(&[]));
        assert!(!parse_portals(&["--mode", "hosted"]));
        assert!(!parse_portals(&["--enable-peer-sync"]));
    }

    #[test]
    fn portals_flag_present_anywhere() {
        assert!(parse_portals(&["--enable-portals"]));
        assert!(parse_portals(&["--mode", "native", "--enable-portals"]));
        assert!(parse_portals(&[
            "--enable-portals",
            "--enable-peer-sync",
            "--mode=native"
        ]));
    }

    #[test]
    fn default_backend_is_winit() {
        assert!(matches!(parse_backend(&[]), BackendKind::Winit));
    }

    #[test]
    fn dash_dash_backend_drm_returns_drm() {
        assert!(matches!(
            parse_backend(&["--backend", "drm"]),
            BackendKind::Drm
        ));
    }

    #[test]
    fn dash_dash_backend_eq_drm_returns_drm() {
        assert!(matches!(parse_backend(&["--backend=drm"]), BackendKind::Drm));
    }

    #[test]
    fn unrecognized_backend_value_defaults_winit() {
        assert!(matches!(
            parse_backend(&["--backend", "junk"]),
            BackendKind::Winit
        ));
        assert!(matches!(
            parse_backend(&["--backend=nonsense"]),
            BackendKind::Winit
        ));
    }
}
