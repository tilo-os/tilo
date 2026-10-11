//! Compositor entry point.
//!
//! The shell binary supports two run modes: [`Mode::Native`] (default) spins up a real
//! smithay-driven Wayland server (auto-binds `wayland-N`) and paints chrome through vello+wgpu;
//! [`Mode::Hosted`] is the legacy `winit`-on-host `ui::App` loop, an explicit dev opt-in via `--mode hosted` for when no DRM/KMS or spare Wayland session is available.
//!
//! Mode selection is a single `--mode <hosted|native>` (or `--mode=<...>`) flag; anything unrecognised, including the flag being absent, falls back to [`Mode::Native`].

#[cfg(target_os = "linux")]
pub mod accessibility;
#[cfg(target_os = "linux")]
pub mod activation;
#[cfg(target_os = "linux")]
pub(crate) mod admin_control;
#[cfg(target_os = "linux")]
pub(crate) mod agent_door;
#[cfg(target_os = "linux")]
pub mod agent_seats;
#[cfg(target_os = "linux")]
mod audio_capture;
pub mod backend;
#[cfg(target_os = "linux")]
pub mod bridge;
#[cfg(target_os = "linux")]
pub mod capture;
#[cfg(target_os = "linux")]
pub mod chooser_service;
#[cfg(target_os = "linux")]
pub mod client_cursor;
#[cfg(target_os = "linux")]
pub mod client_input;
#[cfg(target_os = "linux")]
pub mod client_settle;
#[cfg(target_os = "linux")]
pub(crate) mod control_invocation;
#[cfg(target_os = "linux")]
pub(crate) mod control_listing;
pub mod controller_nav;
pub mod cursor;
pub mod dev_env;
pub mod dev_test_identity;
#[cfg(target_os = "linux")]
pub mod device_rotation;
pub(crate) mod diagnostic_actions;
#[cfg(target_os = "linux")]
pub mod dictation;
pub mod drag_feedback;
#[cfg(target_os = "linux")]
pub mod floating_service;
pub mod foreign;
pub mod frame;
pub mod frame_stall;
#[cfg(target_os = "linux")]
pub mod gpu_health;
pub mod gpu_import;
#[cfg(target_os = "linux")]
pub mod hosted;
#[cfg(feature = "hosted-test-control")]
#[cfg(target_os = "linux")]
pub mod hosted_test_control;
pub mod idle;
#[cfg(target_os = "linux")]
pub mod idle_inhibit;
pub mod input_environment;
pub mod input_method;
#[cfg(target_os = "linux")]
pub mod input_method_service;
pub mod key_latency;
pub(crate) mod lock_actions;
pub mod lock_auth;
pub mod lock_controller;
pub mod lock_screen;
#[cfg(target_os = "macos")]
pub mod macos;
pub mod metrics;
pub mod mp4_recording;
#[cfg(target_os = "linux")]
pub mod native;
#[cfg(target_os = "linux")]
pub mod opaque_region;
#[cfg(target_os = "linux")]
pub mod output_globals;
pub mod output_heads;
#[cfg(target_os = "linux")]
pub mod output_management;
pub mod pacing;
pub mod pad_layout;
#[cfg(target_os = "linux")]
pub mod pointer_lock;
pub mod present;
#[cfg(target_os = "linux")]
pub mod presentation;
#[cfg(target_os = "linux")]
pub mod private_text_input;
pub mod projection_capture;
pub mod published_room;
#[cfg(target_os = "linux")]
pub mod remote_seat;
pub mod render_node;
#[cfg(target_os = "linux")]
pub mod scanout_feedback;
pub mod screen_dim;
pub mod selection;
pub mod session_env;
#[cfg(target_os = "linux")]
mod session_recovery;
#[cfg(target_os = "linux")]
mod session_termination;
#[cfg(target_os = "linux")]
pub mod shell_text_input;
#[cfg(target_os = "linux")]
pub mod shortcut_inhibit;
#[cfg(target_os = "linux")]
pub mod state;
pub mod surface_outputs;
#[cfg(target_os = "linux")]
pub mod surface_readback;
pub mod switch_control;
pub mod switch_latency;
pub mod tab_binding;
#[cfg(target_os = "linux")]
pub mod tablet;
#[cfg(target_os = "linux")]
pub mod tablet_pad;
#[cfg(target_os = "linux")]
pub mod test_control;
pub mod text_commit;
#[cfg(target_os = "linux")]
pub mod toplevel_icon;
pub mod touch_pointer;
#[cfg(target_os = "linux")]
pub mod window_previews;
#[cfg(target_os = "linux")]
pub(crate) mod xwayland;
#[cfg(target_os = "linux")]
pub(crate) mod xwayland_composer;

/// Which run mode the compositor should boot into.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    /// The legacy `winit`-on-host `ui::App` loop, kept as an explicit dev opt-in via `--mode hosted`.
    Hosted,
    /// Default. Real smithay-driven Wayland server — see [`native::run`].
    Native,
}

/// Whether this session is part of a complete Tilo installation or was added
/// to an existing Linux host.
///
/// This is deliberately an explicit launch contract.  A DRM backend can be
/// used in both cases, while desktop-name and distribution environment
/// variables are display-manager implementation details rather than install
/// identity.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InstallMode {
    /// The Tilo image: machine identity material lives in the system identity
    /// tree and the complete first-login experience is available.
    FullSystem,
    /// A Tilo session installed alongside another desktop: identity material
    /// must remain in this login's XDG-owned storage and setup is limited to
    /// the Tilo identity ceremony.
    HostDesktop,
}

/// Process-global wrapper around [`parse_install_mode`].
pub fn parse_install_mode_from_args() -> InstallMode {
    let owned: Vec<String> = std::env::args().skip(1).collect();
    let borrowed: Vec<&str> = owned.iter().map(std::string::String::as_str).collect();
    parse_install_mode(&borrowed)
}

/// Resolve the installation context from the package-owned session wrapper.
/// Absence means full Tilo, preserving the existing image boot path exactly.
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
    /// into an offscreen texture via [`backend::HeadlessBackend`]. Lets the live binary boot in CI, distrobox without a display, or SSH without X-forwarding; drivable via `TILO_TEST_CONTROL`.
    Headless,
}

/// Parse the run mode from the process's command-line arguments. Thin wrapper around
/// [`parse_mode`] so callers don't materialise a `Vec<String>`; tests should drive [`parse_mode`] directly since `std::env::args` can't be monkey-patched.
pub fn parse_mode_from_args() -> Mode {
    let owned: Vec<String> = std::env::args().skip(1).collect();
    let borrowed: Vec<&str> = owned.iter().map(std::string::String::as_str).collect();
    parse_mode(&borrowed)
}

/// Process-global wrapper around [`parse_peer_sync`]. Peer sync is part of a
/// normal Tilo session; `--disable-peer-sync` is the explicit safe override
/// for tests, recovery, and deliberately offline sessions.
pub fn parse_peer_sync_from_args() -> bool {
    let owned: Vec<String> = std::env::args().skip(1).collect();
    let borrowed: Vec<&str> = owned.iter().map(std::string::String::as_str).collect();
    !parse_peer_sync_disabled(&borrowed)
}

/// Pure, testable compatibility parser for the historical enable flag.
pub fn parse_peer_sync(args: &[&str]) -> bool {
    args.contains(&"--enable-peer-sync")
}

/// Pure, testable parser for the explicit offline/recovery override.
pub fn parse_peer_sync_disabled(args: &[&str]) -> bool {
    args.contains(&"--disable-peer-sync")
}

/// Run-mode flags that were removed, each with the reason `main` gives when
/// refusing it. The argument parser is otherwise permissive, so without this a
/// stale unit passing one would silently start a full compositor instead.
const REMOVED_RUN_MODES: &[(&str, &str)] = &[(
    "--portal-server-only",
    "--portal-server-only was removed; the compositor serves the xdg-desktop-portal \
     backend in-process (docs/architecture/security.md). Disable any unit that still passes it.",
)];

/// Pure, testable check for a removed run-mode flag: the refusal `main` prints
/// before exiting with status 2, or `None` when no removed flag is present.
pub fn removed_run_mode_refusal(args: &[&str]) -> Option<&'static str> {
    REMOVED_RUN_MODES
        .iter()
        .find(|(flag, _)| args.contains(flag))
        .map(|(_, refusal)| *refusal)
}

/// Pure, testable `--firstboot` flag parser (mirrors [`parse_peer_sync`]) — a "make sure dirs exist before we proceed" hook, not a separate entry point.
pub fn parse_firstboot(args: &[&str]) -> bool {
    args.contains(&"--firstboot")
}

/// Process-global wrapper around [`parse_firstboot`].
pub fn parse_firstboot_from_args() -> bool {
    process_args_match(parse_firstboot)
}

/// Pure, testable parser for the system-unit-only first-boot entry point.
/// Unlike [`parse_firstboot`], this flag never falls through to compositor
/// startup.
pub fn parse_firstboot_only(args: &[&str]) -> bool {
    args.contains(&"--firstboot-only")
}

/// Process-global wrapper around [`parse_firstboot_only`].
pub fn parse_firstboot_only_from_args() -> bool {
    process_args_match(parse_firstboot_only)
}

/// Pure, testable `--provision-identity` flag parser.
///
/// Unlike `--firstboot`, this is a *separate entry point*: runs the identity ceremony and exits
/// without starting a compositor. It runs as the user, putting the key in the user's own home
/// instead of root's.
pub fn parse_provision_identity(args: &[&str]) -> bool {
    args.contains(&"--provision-identity")
}

/// Process-global wrapper around [`parse_provision_identity`].
pub fn parse_provision_identity_from_args() -> bool {
    process_args_match(parse_provision_identity)
}

/// Read-only identity gate used by the shipped graphical-session wrapper.
///
/// This is deliberately distinct from `--provision-identity`: verification
/// must never repair, rotate, or mint keys merely because login is checking
/// whether the ceremony completed.
pub fn parse_verify_collaboration_identity(args: &[&str]) -> bool {
    args.contains(&"--verify-collaboration-identity")
}

/// Process-global wrapper around [`parse_verify_collaboration_identity`].
pub fn parse_verify_collaboration_identity_from_args() -> bool {
    process_args_match(parse_verify_collaboration_identity)
}

fn process_args_match(parser: fn(&[&str]) -> bool) -> bool {
    let owned: Vec<String> = std::env::args().skip(1).collect();
    let borrowed: Vec<&str> = owned.iter().map(String::as_str).collect();
    parser(&borrowed)
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
    flag_value(args, "--mode").map_or(Mode::Native, value_to_mode)
}

/// The value of the first `<flag> <value>` or `<flag>=<value>`, if any. A
/// trailing `<flag>` with no value has none.
fn flag_value<'a>(args: &[&'a str], flag: &str) -> Option<&'a str> {
    let mut iter = args.iter().copied();
    while let Some(arg) = iter.next() {
        if let Some(value) = arg
            .strip_prefix(flag)
            .and_then(|rest| rest.strip_prefix('='))
        {
            return Some(value);
        }
        if arg == flag {
            return iter.next();
        }
    }
    None
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
    flag_value(args, "--backend").map_or(BackendKind::Winit, value_to_backend)
}

fn value_to_backend(value: &str) -> BackendKind {
    match value {
        "drm" => BackendKind::Drm,
        "headless" => BackendKind::Headless,
        _ => BackendKind::Winit,
    }
}

/// The chord that releases a held pointer or captured keys, as the on-screen
/// display names it.
pub fn release_chord_label() -> String {
    ui::keycaps::active().chord(["Ctrl", "Alt"], "escape")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mode_and_install_mode_resolve_every_flag_form() {
        let modes: &[(&str, &[&str], Mode)] = &[
            // `cargo run --bin shell` (no flags) boots the real smithay server.
            ("no flag", &[], Mode::Native),
            ("--mode native", &["--mode", "native"], Mode::Native),
            ("--mode=native", &["--mode=native"], Mode::Native),
            // `--mode hosted` is the explicit dev opt-in for the legacy winit-hosted path; guards that fallback stays reachable from the CLI.
            ("--mode hosted", &["--mode", "hosted"], Mode::Hosted),
            ("--mode=hosted", &["--mode=hosted"], Mode::Hosted),
            // A typo doesn't demote to the hosted path — it falls through to the real Wayland server (hosted is opt-in only).
            ("--mode junk", &["--mode", "junk"], Mode::Native),
        ];
        for (case, args, want) in modes {
            assert_eq!(parse_mode(args), *want, "case {case}");
        }

        let installs: &[(&str, &[&str], InstallMode)] = &[
            ("no flag", &[], InstallMode::FullSystem),
            (
                "drm backend",
                &["--backend", "drm"],
                InstallMode::FullSystem,
            ),
            // The package wrapper explicitly selects a host desktop session.
            (
                "wrapper with firstboot",
                &["--firstboot", "--host-desktop-session", "--backend", "drm"],
                InstallMode::HostDesktop,
            ),
            (
                "wrapper with mode",
                &["--host-desktop-session", "--mode", "native"],
                InstallMode::HostDesktop,
            ),
        ];
        for (case, args, want) in installs {
            assert_eq!(parse_install_mode(args), *want, "case {case}");
        }
    }

    #[test]
    fn backend_flag_selects_drm_or_headless_and_defaults_to_winit() {
        // `--backend headless` / `--backend=headless` is the third path alongside winit (default) and drm,
        // letting the binary boot without a display server so e2e harnesses can drive it via `TILO_TEST_CONTROL`.
        let cases: &[(&str, &[&str], BackendKind)] = &[
            ("default", &[], BackendKind::Winit),
            ("--backend drm", &["--backend", "drm"], BackendKind::Drm),
            ("--backend=drm", &["--backend=drm"], BackendKind::Drm),
            ("--backend junk", &["--backend", "junk"], BackendKind::Winit),
            (
                "--backend=nonsense",
                &["--backend=nonsense"],
                BackendKind::Winit,
            ),
            (
                "--backend headless",
                &["--backend", "headless"],
                BackendKind::Headless,
            ),
            (
                "--backend=headless",
                &["--backend=headless"],
                BackendKind::Headless,
            ),
        ];
        for (case, args, want) in cases {
            assert_eq!(parse_backend(args), *want, "case {case}");
        }
    }

    #[test]
    fn boolean_flags_default_off_and_are_found_anywhere() {
        type Parse = fn(&[&str]) -> bool;
        // (flag, parser, argument lists that must parse false, argument lists that must parse true)
        #[allow(
            clippy::type_complexity,
            reason = "one row per flag: name, parser, accepted and rejected argv sets"
        )]
        let cases: &[(&str, Parse, &[&[&str]], &[&[&str]])] = &[
            // Peer sync is default-on elsewhere; the legacy enable flag is not required for it.
            (
                "peer_sync",
                parse_peer_sync,
                &[&[], &["--mode", "hosted"]],
                &[
                    &["--enable-peer-sync"],
                    &["--mode", "hosted", "--enable-peer-sync"],
                    &["--enable-peer-sync", "--mode=native"],
                ],
            ),
            (
                "portals",
                parse_portals,
                &[&[], &["--mode", "hosted"], &["--enable-peer-sync"]],
                &[
                    &["--enable-portals"],
                    &["--mode", "native", "--enable-portals"],
                    &["--enable-portals", "--enable-peer-sync", "--mode=native"],
                ],
            ),
            (
                "firstboot",
                parse_firstboot,
                &[&[], &["--mode", "native"], &["--enable-peer-sync"]],
                &[
                    &["--firstboot"],
                    &["--firstboot", "--mode", "native"],
                    &["--mode=native", "--firstboot"],
                ],
            ),
            (
                "firstboot_only",
                parse_firstboot_only,
                &[&[], &["--firstboot"], &["--mode", "native"]],
                &[
                    &["--firstboot-only"],
                    &["--mode", "native", "--firstboot-only"],
                ],
            ),
            // Identity entry points are explicit and independent.
            (
                "provision_identity",
                parse_provision_identity,
                &[&[], &["--verify-collaboration-identity"]],
                &[&["--provision-identity"]],
            ),
            (
                "verify_collaboration_identity",
                parse_verify_collaboration_identity,
                &[&[], &["--provision-identity"]],
                &[&["--verify-collaboration-identity"]],
            ),
        ];
        for (flag, parse, off, on) in cases {
            for args in *off {
                assert!(!parse(args), "case {flag} must be off for {args:?}");
            }
            for args in *on {
                assert!(parse(args), "case {flag} must be on for {args:?}");
            }
        }
    }

    #[test]
    fn a_removed_run_mode_is_refused_wherever_it_appears() {
        // The retired portal unit passed `--portal-server-only` alone; a script
        // may combine it with other flags. Either way it must be refused, not
        // ignored into a compositor boot.
        let refused: [&[&str]; 3] = [
            &["--portal-server-only"],
            &["--mode", "native", "--portal-server-only"],
            &["--portal-server-only", "--enable-portals"],
        ];
        for args in refused {
            let refusal = removed_run_mode_refusal(args)
                .unwrap_or_else(|| panic!("{args:?} must be refused"));
            assert!(
                refusal.contains("--portal-server-only was removed")
                    && refusal.contains("docs/architecture/security.md"),
                "the refusal names the flag and where the portal lives now: {refusal}"
            );
        }
        // No removed flag: the ordinary boot paths are untouched.
        let accepted: [&[&str]; 5] = [
            &[],
            &["--mode", "native"],
            &["--enable-portals"],
            &["--firstboot"],
            &["--firstboot-only"],
        ];
        for args in accepted {
            assert_eq!(removed_run_mode_refusal(args), None, "{args:?}");
        }
    }

    #[test]
    fn peer_sync_is_on_unless_explicitly_disabled() {
        assert!(!parse_peer_sync_disabled(&[]));
        assert!(!parse_peer_sync_disabled(&["--mode", "native"]));
        assert!(!parse_peer_sync_disabled(&["--enable-peer-sync"]));
        assert!(parse_peer_sync_disabled(&["--disable-peer-sync"]));
        assert!(parse_peer_sync_disabled(&[
            "--enable-peer-sync",
            "--disable-peer-sync"
        ]));
    }
}

#[cfg(target_os = "linux")]
mod spatial_pointer;
