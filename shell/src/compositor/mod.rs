//! Compositor entry point.
//!
//! The shell binary supports two run modes: [`Mode::Native`] (default) spins up a real
//! smithay-driven Wayland server (auto-binds `wayland-N`) and paints chrome through vello+wgpu;
//! [`Mode::Hosted`] is the legacy `winit`-on-host iced loop, an explicit dev opt-in via `--mode hosted` for when no DRM/KMS or spare Wayland session is available.
//!
//! Mode selection is a single `--mode <hosted|native>` (or `--mode=<...>`) flag; anything unrecognised, including the flag being absent, falls back to [`Mode::Native`].

pub mod backend;
pub mod bridge;
pub mod client_input;
pub mod dev_env;
pub mod drag_feedback;
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
pub mod osk_service;
pub mod output_globals;
pub mod pacing;
pub mod present;
pub mod projection_capture;
pub mod remote_seat;
pub mod render_node;
pub mod selection;
pub mod state;
pub mod surface_readback;
pub mod switch_control;
pub mod tab_binding;
pub mod test_control;

/// Which run mode the compositor should boot into.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    /// The legacy `winit`-on-host iced loop, kept as an explicit dev opt-in via `--mode hosted`.
    Hosted,
    /// Default. Real smithay-driven Wayland server — see [`native::run`].
    Native,
}

/// Whether this session is part of a complete qOS installation or was added
/// to an existing Linux host.
///
/// This is deliberately an explicit launch contract.  A DRM backend can be
/// used in both cases, while desktop-name and distribution environment
/// variables are display-manager implementation details rather than install
/// identity.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InstallMode {
    /// The qOS image: machine identity material lives in the system identity
    /// tree and the complete first-login experience is available.
    FullSystem,
    /// A qOS session installed alongside another desktop: identity material
    /// must remain in this login's XDG-owned storage and setup is limited to
    /// the qOS identity ceremony.
    HostDesktop,
}

/// Process-global wrapper around [`parse_install_mode`].
pub fn parse_install_mode_from_args() -> InstallMode {
    let owned: Vec<String> = std::env::args().skip(1).collect();
    let borrowed: Vec<&str> = owned.iter().map(std::string::String::as_str).collect();
    parse_install_mode(&borrowed)
}

/// Resolve the installation context from the package-owned session wrapper.
/// Absence means full qOS, preserving the existing image boot path exactly.
pub fn parse_install_mode(args: &[&str]) -> InstallMode {
    if args.contains(&"--host-desktop-session") {
        InstallMode::HostDesktop
    } else {
        InstallMode::FullSystem
    }
}

/// Which backend `Mode::Native` should drive: `--backend <winit|drm>` (or `--backend=<...>`), unknown/absent falling back to [`BackendKind::Winit`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BackendKind {
    /// Default. Host on an existing Wayland/X11 session via `winit`.
    Winit,
    /// Drive a TTY directly via DRM/KMS + libinput — see [`backend::DrmBackend`].
    Drm,
    /// Display-free wgpu-offscreen backend: no window, no event loop, no GPU surface — paints
    /// into an offscreen texture via [`backend::HeadlessBackend`]. Lets the live binary boot in CI, distrobox without a display, or SSH without X-forwarding; drivable via `QOS_TEST_CONTROL`.
    Headless,
}

/// Parse the run mode from the process's command-line arguments. Thin wrapper around
/// [`parse_mode`] so callers don't materialise a `Vec<String>`; tests should drive [`parse_mode`] directly since `std::env::args` can't be monkey-patched.
pub fn parse_mode_from_args() -> Mode {
    let owned: Vec<String> = std::env::args().skip(1).collect();
    let borrowed: Vec<&str> = owned.iter().map(std::string::String::as_str).collect();
    parse_mode(&borrowed)
}

/// Process-global wrapper around [`parse_peer_sync`]. v1 default OFF — callers opt in via `--enable-peer-sync` to spin up the Iroh stack.
pub fn parse_peer_sync_from_args() -> bool {
    let owned: Vec<String> = std::env::args().skip(1).collect();
    let borrowed: Vec<&str> = owned.iter().map(std::string::String::as_str).collect();
    parse_peer_sync(&borrowed)
}

/// Pure, testable `--enable-peer-sync` flag parser: `true` iff the flag is present anywhere in `args`, so tests can drive it with a fixture slice without touching `std::env`.
pub fn parse_peer_sync(args: &[&str]) -> bool {
    args.contains(&"--enable-peer-sync")
}

/// Source of truth for the peer-sync marker file. Mockable so tests don't touch the
/// filesystem; production wires up [`XdgConfigMarkerSource`], checking
/// `$XDG_CONFIG_HOME/quick-desktop/peer-sync.enabled` (falling back to `~/.config/...`). See
/// `os/docs/peer-sync.md`: the `shell-peer-sync.service` user unit drops this marker so the next launch picks up peer-sync without the CLI flag.
pub trait MarkerSource {
    /// Returns true if the peer-sync marker exists.
    fn peer_sync_marker_exists(&self) -> bool;
}

/// Production marker source — checks `$XDG_CONFIG_HOME/quick-desktop/peer-sync.enabled`, falling back to `$HOME/.config/...` when `XDG_CONFIG_HOME` is unset.
pub struct XdgConfigMarkerSource;

impl XdgConfigMarkerSource {
    /// Resolve the marker file path using the same rules [`peer_sync_marker_exists`] uses; exposed for diagnostics and for tests asserting resolution without touching the filesystem.
    pub fn marker_path() -> Option<std::path::PathBuf> {
        peer_sync_marker_path_from_env(
            std::env::var_os("XDG_CONFIG_HOME"),
            std::env::var_os("HOME"),
        )
    }
}

impl MarkerSource for XdgConfigMarkerSource {
    fn peer_sync_marker_exists(&self) -> bool {
        Self::marker_path().is_some_and(|p| p.exists())
    }
}

/// Pure helper for [`XdgConfigMarkerSource::marker_path`], so tests can drive the path-resolution rules with fixture env-var values.
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

/// Combined peer-sync resolution: CLI flag wins, then marker file, then default OFF. The trait split lets unit tests substitute an in-memory marker source without writing to disk.
pub fn peer_sync_enabled_with(args: &[&str], markers: &dyn MarkerSource) -> bool {
    if parse_peer_sync(args) {
        return true;
    }
    markers.peer_sync_marker_exists()
}

/// Process-global wrapper around [`peer_sync_enabled_with`], reading CLI args from
/// `std::env::args` and the marker via [`XdgConfigMarkerSource`] — what makes the systemd unit opt-in path (`os/docs/peer-sync.md`) actually toggle peer-sync at boot.
pub fn peer_sync_enabled_from_env_or_marker() -> bool {
    let owned: Vec<String> = std::env::args().skip(1).collect();
    let borrowed: Vec<&str> = owned.iter().map(std::string::String::as_str).collect();
    peer_sync_enabled_with(&borrowed, &XdgConfigMarkerSource)
}

/// Process-global wrapper around [`parse_portal_server_only`]: when true, `main` skips the
/// full compositor boot and runs the xdg-desktop-portal D-Bus backend stand-alone — the shape `quick-desktop-portal.service` expects.
pub fn parse_portal_server_only_from_args() -> bool {
    let owned: Vec<String> = std::env::args().skip(1).collect();
    let borrowed: Vec<&str> = owned.iter().map(std::string::String::as_str).collect();
    parse_portal_server_only(&borrowed)
}

/// Pure, testable `--portal-server-only` flag parser (mirrors [`parse_peer_sync`]). When set,
/// `main` runs only `portal::serve` and skips the compositor, so the portal unit doesn't race for `wayland-N`.
pub fn parse_portal_server_only(args: &[&str]) -> bool {
    args.contains(&"--portal-server-only")
}

/// Process-global wrapper around [`parse_firstboot`]: when true, `main` runs first-boot setup (creating `$XDG_CONFIG_HOME/quick-desktop/` etc.) before the normal boot.
pub fn parse_firstboot_from_args() -> bool {
    let owned: Vec<String> = std::env::args().skip(1).collect();
    let borrowed: Vec<&str> = owned.iter().map(std::string::String::as_str).collect();
    parse_firstboot(&borrowed)
}

/// Pure, testable `--firstboot` flag parser (mirrors [`parse_peer_sync`]) — a "make sure dirs exist before we proceed" hook, not a separate entry point.
pub fn parse_firstboot(args: &[&str]) -> bool {
    args.contains(&"--firstboot")
}

/// Process-global wrapper around [`parse_firstboot_only`]. This is the
/// system-unit entry point: perform idempotent first-boot setup and exit
/// without attempting to run a compositor.
pub fn parse_firstboot_only_from_args() -> bool {
    let owned: Vec<String> = std::env::args().skip(1).collect();
    let borrowed: Vec<&str> = owned.iter().map(std::string::String::as_str).collect();
    parse_firstboot_only(&borrowed)
}

/// Pure, testable parser for the system-unit-only first-boot entry point.
/// Unlike [`parse_firstboot`], this flag never falls through to compositor
/// startup.
pub fn parse_firstboot_only(args: &[&str]) -> bool {
    args.contains(&"--firstboot-only")
}

/// Process-global wrapper around [`parse_provision_identity`].
pub fn parse_provision_identity_from_args() -> bool {
    let owned: Vec<String> = std::env::args().skip(1).collect();
    let borrowed: Vec<&str> = owned.iter().map(std::string::String::as_str).collect();
    parse_provision_identity(&borrowed)
}

/// Pure, testable `--provision-identity` flag parser.
///
/// Unlike `--firstboot`, this is a *separate entry point*: runs the identity ceremony and exits
/// without starting a compositor. The `qos-identity.service` **user** unit calls it, putting the key in the user's own home instead of root's.
pub fn parse_provision_identity(args: &[&str]) -> bool {
    args.contains(&"--provision-identity")
}

/// Process-global wrapper around [`parse_portals`]: opt in via `--enable-portals` for the `xdg-desktop-portal` D-Bus backend. v1 default OFF, same shape as `parse_peer_sync`.
pub fn parse_portals_from_args() -> bool {
    let owned: Vec<String> = std::env::args().skip(1).collect();
    let borrowed: Vec<&str> = owned.iter().map(std::string::String::as_str).collect();
    parse_portals(&borrowed)
}

/// Pure, testable `--enable-portals` flag parser, mirroring [`parse_peer_sync`] so the hosted boot path treats both flags identically.
pub fn parse_portals(args: &[&str]) -> bool {
    args.contains(&"--enable-portals")
}

/// Pure, testable mode parser. Accepts `--mode <value>` and `--mode=<value>`; unknown/missing values default to [`Mode::Native`] so a typo never refuses to start the compositor.
pub fn parse_mode(args: &[&str]) -> Mode {
    let mut iter = args.iter().copied();
    while let Some(arg) = iter.next() {
        if let Some(value) = arg.strip_prefix("--mode=") {
            return value_to_mode(value);
        }
        if arg == "--mode"
            && let Some(value) = iter.next()
        {
            return value_to_mode(value);
        }
        // Trailing `--mode` with no value: fall through to default.
    }
    Mode::Native
}

fn value_to_mode(value: &str) -> Mode {
    match value {
        "hosted" => Mode::Hosted,
        _ => Mode::Native,
    }
}

/// Process-global wrapper around [`parse_backend`], used by [`native::run`] to decide which [`backend::Backend`] impl `--mode native` instantiates.
pub fn parse_backend_from_args() -> BackendKind {
    let owned: Vec<String> = std::env::args().skip(1).collect();
    let borrowed: Vec<&str> = owned.iter().map(std::string::String::as_str).collect();
    parse_backend(&borrowed)
}

/// Pure, testable backend parser, mirroring [`parse_mode`]: `--backend <value>`/`--backend=<value>`, defaulting to [`BackendKind::Winit`] for unknown/missing values.
pub fn parse_backend(args: &[&str]) -> BackendKind {
    let mut iter = args.iter().copied();
    while let Some(arg) = iter.next() {
        if let Some(value) = arg.strip_prefix("--backend=") {
            return value_to_backend(value);
        }
        if arg == "--backend"
            && let Some(value) = iter.next()
        {
            return value_to_backend(value);
        }
        // Trailing `--backend` with no value: fall through to default.
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
        // `cargo run -p quick-desktop` (no flags) boots the real smithay server.
        assert!(matches!(parse_mode(&[]), Mode::Native));
    }

    #[test]
    fn install_mode_defaults_to_the_full_qos_system() {
        assert_eq!(parse_install_mode(&[]), InstallMode::FullSystem);
        assert_eq!(
            parse_install_mode(&["--backend", "drm"]),
            InstallMode::FullSystem
        );
    }

    #[test]
    fn package_wrapper_explicitly_selects_a_host_desktop_session() {
        assert_eq!(
            parse_install_mode(&["--firstboot", "--host-desktop-session", "--backend", "drm"]),
            InstallMode::HostDesktop
        );
        assert_eq!(
            parse_install_mode(&["--host-desktop-session", "--mode", "native"]),
            InstallMode::HostDesktop
        );
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
        // `--mode hosted` is the explicit dev opt-in for the legacy iced-on-winit path; guards that fallback stays reachable from the CLI.
        assert!(matches!(parse_mode(&["--mode", "hosted"]), Mode::Hosted));
        assert!(matches!(parse_mode(&["--mode=hosted"]), Mode::Hosted));
    }

    #[test]
    fn unrecognized_value_defaults_native() {
        // A typo doesn't demote to the hosted iced path — it falls through to the real Wayland server (hosted is opt-in only).
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
        // Both `--backend headless` and `--backend=headless` resolve to `BackendKind::Headless` — the
        // third path alongside winit (default) and drm, letting the binary boot without a display server so e2e harnesses can drive it via `QOS_TEST_CONTROL`.
        assert!(matches!(
            parse_backend(&["--backend", "headless"]),
            BackendKind::Headless
        ));
        assert!(matches!(
            parse_backend(&["--backend=headless"]),
            BackendKind::Headless
        ));
    }

    // --portal-server-only

    #[test]
    fn portal_server_only_default_false() {
        // Bare cargo run -p quick-desktop must still boot the compositor — no flag means no portal-only short-circuit.
        assert!(!parse_portal_server_only(&[]));
        assert!(!parse_portal_server_only(&["--mode", "native"]));
        assert!(!parse_portal_server_only(&["--enable-peer-sync"]));
    }

    #[test]
    fn portal_server_only_flag_present_anywhere() {
        assert!(parse_portal_server_only(&["--portal-server-only"]));
        // The systemd unit invokes `--portal-server-only` alone, but combining with other flags (e.g. tracing knobs) must still work.
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

    // --firstboot

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

    // --firstboot-only

    #[test]
    fn firstboot_only_default_false() {
        assert!(!parse_firstboot_only(&[]));
        assert!(!parse_firstboot_only(&["--firstboot"]));
        assert!(!parse_firstboot_only(&["--mode", "native"]));
    }

    #[test]
    fn firstboot_only_flag_present_anywhere() {
        assert!(parse_firstboot_only(&["--firstboot-only"]));
        assert!(parse_firstboot_only(&[
            "--mode",
            "native",
            "--firstboot-only"
        ]));
    }

    // peer-sync marker file

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
        // No CLI flag, but marker file present → peer-sync ON — the systemd-unit opt-in path documented in `os/docs/peer-sync.md`.
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
