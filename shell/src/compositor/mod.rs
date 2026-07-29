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
pub mod client_input;
pub mod foreign;
pub mod frame;
pub mod gpu_import;
pub mod hosted;
#[cfg(feature = "hosted-test-control")]
pub mod hosted_test_control;
pub mod idle;
pub mod lock_auth;
pub mod lock_controller;
pub mod lock_screen;
pub mod metrics;
pub mod native;
pub mod pacing;
pub mod present;
pub mod remote_seat;
pub mod render_node;
pub mod state;
pub mod surface_readback;
pub mod tab_binding;
pub mod test_control;

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
    /// Display-free wgpu-offscreen backend. No window, no event loop,
    /// no GPU surface — paints into an offscreen texture via
    /// [`backend::HeadlessBackend`]. Lets the live binary boot in CI,
    /// distrobox without a display, or remote SSH sessions without
    /// X-forwarding. Drivable end-to-end through the
    /// `QOS_TEST_CONTROL` socket protocol.
    Headless,
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

/// Source of truth for the peer-sync marker file. Mockable so tests
/// don't actually touch the filesystem — production wires up
/// [`XdgConfigMarkerSource`] which checks
/// `$XDG_CONFIG_HOME/quick-desktop/peer-sync.enabled` (falling back to
/// `~/.config/quick-desktop/peer-sync.enabled`).
///
/// See `os/docs/peer-sync.md` for the user-facing contract — enabling
/// the `quick-desktop-peer-sync.service` user unit drops this marker so
/// the next compositor launch picks up peer-sync without needing the
/// CLI flag.
pub trait MarkerSource {
    /// Returns true if the peer-sync marker exists.
    fn peer_sync_marker_exists(&self) -> bool;
}

/// Production marker source — checks `$XDG_CONFIG_HOME/quick-desktop/peer-sync.enabled`,
/// falling back to `$HOME/.config/quick-desktop/peer-sync.enabled` when
/// `XDG_CONFIG_HOME` is unset.
pub struct XdgConfigMarkerSource;

impl XdgConfigMarkerSource {
    /// Resolve the marker file path using the same rules
    /// [`peer_sync_marker_exists`] uses. Exposed for diagnostics and
    /// for tests that want to assert the resolution rules without
    /// touching the filesystem.
    pub fn marker_path() -> Option<std::path::PathBuf> {
        peer_sync_marker_path_from_env(
            std::env::var_os("XDG_CONFIG_HOME"),
            std::env::var_os("HOME"),
        )
    }
}

impl MarkerSource for XdgConfigMarkerSource {
    fn peer_sync_marker_exists(&self) -> bool {
        Self::marker_path().map(|p| p.exists()).unwrap_or(false)
    }
}

/// Pure helper for [`XdgConfigMarkerSource::marker_path`]. Split out so
/// tests can drive the path-resolution rules with fixture env-var
/// values without touching the real process environment.
pub fn peer_sync_marker_path_from_env(
    xdg_config_home: Option<std::ffi::OsString>,
    home: Option<std::ffi::OsString>,
) -> Option<std::path::PathBuf> {
    if let Some(xdg) = xdg_config_home.filter(|v| !v.is_empty()) {
        let mut p = std::path::PathBuf::from(xdg);
        p.push("quick-desktop");
        p.push("peer-sync.enabled");
        return Some(p);
    }
    let home = home.filter(|v| !v.is_empty())?;
    let mut p = std::path::PathBuf::from(home);
    p.push(".config");
    p.push("quick-desktop");
    p.push("peer-sync.enabled");
    Some(p)
}

/// Combined peer-sync resolution: CLI flag wins, then marker file,
/// then default OFF. Used by `main` to decide whether to spin up the
/// libp2p stack. The trait split lets unit tests substitute an
/// in-memory marker source without writing to disk.
pub fn peer_sync_enabled_with(args: &[&str], markers: &dyn MarkerSource) -> bool {
    if parse_peer_sync(args) {
        return true;
    }
    markers.peer_sync_marker_exists()
}

/// Process-global wrapper around [`peer_sync_enabled_with`]. Reads CLI
/// args from `std::env::args` and the marker via
/// [`XdgConfigMarkerSource`]. `main.rs` uses this in lieu of
/// [`parse_peer_sync_from_args`] so the systemd user unit opt-in path
/// (see `os/docs/peer-sync.md`) actually toggles peer-sync at boot.
pub fn peer_sync_enabled_from_env_or_marker() -> bool {
    let owned: Vec<String> = std::env::args().skip(1).collect();
    let borrowed: Vec<&str> = owned.iter().map(|s| s.as_str()).collect();
    peer_sync_enabled_with(&borrowed, &XdgConfigMarkerSource)
}

/// Process-global wrapper around [`parse_portal_server_only`]. When
/// true, `main` skips the full compositor boot and runs the
/// xdg-desktop-portal D-Bus backend as a stand-alone process — the
/// shape the `quick-desktop-portal.service` user unit expects (see
/// `os/usr/lib/systemd/user/quick-desktop-portal.service`).
pub fn parse_portal_server_only_from_args() -> bool {
    let owned: Vec<String> = std::env::args().skip(1).collect();
    let borrowed: Vec<&str> = owned.iter().map(|s| s.as_str()).collect();
    parse_portal_server_only(&borrowed)
}

/// Pure, testable `--portal-server-only` flag parser. Mirrors
/// [`parse_peer_sync`]: returns true iff the flag is present anywhere
/// in `args`. When set, `main` runs only `portal::serve` and skips the
/// compositor — used by the user-level `quick-desktop-portal.service`
/// unit so the portal backend doesn't race for `wayland-N`.
pub fn parse_portal_server_only(args: &[&str]) -> bool {
    args.iter().any(|a| *a == "--portal-server-only")
}

/// Process-global wrapper around [`parse_firstboot`]. When true,
/// `main` runs the first-boot setup routine (creating
/// `$XDG_CONFIG_HOME/quick-desktop/` and friends) **before** continuing
/// with the normal compositor boot. Used by the
/// `quick-desktop-firstboot.service` system unit on first login.
pub fn parse_firstboot_from_args() -> bool {
    let owned: Vec<String> = std::env::args().skip(1).collect();
    let borrowed: Vec<&str> = owned.iter().map(|s| s.as_str()).collect();
    parse_firstboot(&borrowed)
}

/// Pure, testable `--firstboot` flag parser. Mirrors
/// [`parse_peer_sync`]: returns true iff the flag is present anywhere
/// in `args`. When set, `main` runs first-boot setup and then
/// continues with the compositor — first-boot is a "make sure dirs
/// exist before we proceed" hook, not a separate entry point.
pub fn parse_firstboot(args: &[&str]) -> bool {
    args.iter().any(|a| *a == "--firstboot")
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
        "headless" => BackendKind::Headless,
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
        assert!(matches!(
            parse_backend(&["--backend=drm"]),
            BackendKind::Drm
        ));
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

    #[test]
    fn parse_backend_headless_returns_headless() {
        // Both `--backend headless` and `--backend=headless` must
        // resolve to `BackendKind::Headless`. The headless backend is
        // the third path alongside winit (default) and drm; it lets
        // the live binary boot without a display server so e2e
        // harnesses can drive it via `QOS_TEST_CONTROL`.
        assert!(matches!(
            parse_backend(&["--backend", "headless"]),
            BackendKind::Headless
        ));
        assert!(matches!(
            parse_backend(&["--backend=headless"]),
            BackendKind::Headless
        ));
    }

    // ---- Subtask A: --portal-server-only -----------------------------

    #[test]
    fn portal_server_only_default_false() {
        // Bare cargo run -p quick-desktop must still boot the
        // compositor — no flag means no portal-only short-circuit.
        assert!(!parse_portal_server_only(&[]));
        assert!(!parse_portal_server_only(&["--mode", "native"]));
        assert!(!parse_portal_server_only(&["--enable-peer-sync"]));
    }

    #[test]
    fn portal_server_only_flag_present_anywhere() {
        assert!(parse_portal_server_only(&["--portal-server-only"]));
        // The systemd unit invokes
        // `/usr/bin/quick-desktop --portal-server-only` with no other
        // args, but allow combining with other flags so a developer
        // can still mix in tracing knobs.
        assert!(parse_portal_server_only(&[
            "--mode",
            "native",
            "--portal-server-only"
        ]));
        assert!(parse_portal_server_only(&[
            "--portal-server-only",
            "--enable-portals"
        ]));
    }

    // ---- Subtask B: --firstboot -------------------------------------

    #[test]
    fn firstboot_default_false() {
        assert!(!parse_firstboot(&[]));
        assert!(!parse_firstboot(&["--mode", "native"]));
        assert!(!parse_firstboot(&["--enable-peer-sync"]));
    }

    #[test]
    fn firstboot_flag_present_anywhere() {
        assert!(parse_firstboot(&["--firstboot"]));
        assert!(parse_firstboot(&["--firstboot", "--mode", "native"]));
        assert!(parse_firstboot(&["--mode=native", "--firstboot"]));
    }

    // ---- Subtask C: peer-sync marker file ---------------------------

    struct FakeMarker {
        present: bool,
    }
    impl MarkerSource for FakeMarker {
        fn peer_sync_marker_exists(&self) -> bool {
            self.present
        }
    }

    #[test]
    fn peer_sync_resolution_default_off() {
        let m = FakeMarker { present: false };
        assert!(!peer_sync_enabled_with(&[], &m));
        assert!(!peer_sync_enabled_with(&["--mode", "native"], &m));
    }

    #[test]
    fn peer_sync_resolution_cli_flag_wins() {
        // CLI flag enables peer-sync regardless of marker absence.
        let m_off = FakeMarker { present: false };
        assert!(peer_sync_enabled_with(&["--enable-peer-sync"], &m_off));
        // And marker presence + flag is still on (no double-disable).
        let m_on = FakeMarker { present: true };
        assert!(peer_sync_enabled_with(&["--enable-peer-sync"], &m_on));
    }

    #[test]
    fn peer_sync_resolution_marker_enables_without_flag() {
        // No CLI flag, but marker file present → peer-sync ON. This
        // is the systemd-unit opt-in path documented in
        // `os/docs/peer-sync.md`.
        let m = FakeMarker { present: true };
        assert!(peer_sync_enabled_with(&[], &m));
        assert!(peer_sync_enabled_with(&["--mode", "native"], &m));
    }

    #[test]
    fn marker_path_prefers_xdg_config_home() {
        let path = peer_sync_marker_path_from_env(
            Some(std::ffi::OsString::from("/custom/xdg")),
            Some(std::ffi::OsString::from("/home/ignored")),
        )
        .expect("path resolves with XDG set");
        assert_eq!(
            path,
            std::path::PathBuf::from("/custom/xdg/quick-desktop/peer-sync.enabled")
        );
    }

    #[test]
    fn marker_path_falls_back_to_home_dot_config() {
        let path =
            peer_sync_marker_path_from_env(None, Some(std::ffi::OsString::from("/home/alice")))
                .expect("path resolves via HOME");
        assert_eq!(
            path,
            std::path::PathBuf::from("/home/alice/.config/quick-desktop/peer-sync.enabled")
        );
    }

    #[test]
    fn marker_path_empty_xdg_treated_as_unset() {
        // POSIX convention: an empty env-var means "use the default".
        let path = peer_sync_marker_path_from_env(
            Some(std::ffi::OsString::from("")),
            Some(std::ffi::OsString::from("/home/bob")),
        )
        .expect("falls back through empty XDG to HOME");
        assert_eq!(
            path,
            std::path::PathBuf::from("/home/bob/.config/quick-desktop/peer-sync.enabled")
        );
    }

    #[test]
    fn marker_path_no_xdg_no_home_returns_none() {
        // No way to resolve a path → caller defaults to "marker absent".
        assert!(peer_sync_marker_path_from_env(None, None).is_none());
        assert!(
            peer_sync_marker_path_from_env(
                Some(std::ffi::OsString::from("")),
                Some(std::ffi::OsString::from("")),
            )
            .is_none()
        );
    }
}
