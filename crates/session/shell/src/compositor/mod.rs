//! Compositor entry point.
//!
//! The shell binary has one session: a real smithay-driven Wayland server (auto-binds
//! `wayland-N`) that paints chrome through vello+wgpu — see [`native::run`]. Where it draws is the
//! backend's business (`--backend <winit|drm|headless>`): `winit` is a window inside an existing
//! desktop for development, `drm` the shipped session, `headless` a display-free one for CI.
//!
//! The legacy `--mode hosted` loop was removed; [`removed_run_mode_refusal`] refuses it rather than
//! silently booting a different session. Its bootstrap now lives once, in `session_bootstrap`.

#[cfg(target_os = "linux")]
pub mod accessibility;
#[cfg(target_os = "linux")]
pub mod activation;
pub mod activity;
#[cfg(target_os = "linux")]
pub(crate) mod admin_control;
#[cfg(target_os = "linux")]
pub(crate) mod agent_door;
#[cfg(target_os = "linux")]
pub mod agent_seats;
pub mod app_liveness;
#[cfg(target_os = "linux")]
mod audio_capture;
pub mod backend;
#[cfg(target_os = "linux")]
pub mod capture;
#[cfg(target_os = "linux")]
pub(crate) mod capture_io;
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
#[cfg(target_os = "linux")]
pub mod descriptor_table;
pub mod dev_env;
pub mod dev_test_identity;
#[cfg(target_os = "linux")]
pub mod device_rotation;
pub(crate) mod diagnostic_actions;
#[cfg(target_os = "linux")]
pub mod dictation;
pub mod dictation_status;
#[cfg(target_os = "linux")]
pub mod downloaded_programs;
pub mod drag_feedback;
#[cfg(target_os = "linux")]
pub mod floating_service;
pub mod frame_stall;
#[cfg(target_os = "linux")]
pub mod gpu_health;
#[cfg(target_os = "linux")]
pub mod gpu_import;
pub mod idle;
#[cfg(target_os = "linux")]
pub mod idle_inhibit;
pub mod input_environment;
pub mod input_method;
#[cfg(target_os = "linux")]
pub mod input_method_service;
pub mod input_order;
pub mod key_latency;
#[cfg(target_os = "linux")]
pub mod keymap_typing;
#[cfg(target_os = "linux")]
pub mod launch_permissions;
pub(crate) mod listing_snapshots;
pub(crate) mod lock_actions;
pub mod lock_auth;
pub mod lock_controller;
pub mod lock_screen;
#[cfg(target_os = "macos")]
pub mod macos;
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
#[cfg(target_os = "linux")]
pub mod presentation;
#[cfg(target_os = "linux")]
pub mod private_text_input;
pub mod projection_capture;
pub mod published_room;
#[cfg(target_os = "linux")]
pub mod remote_seat;
#[cfg(target_os = "linux")]
pub mod render_broker;
pub mod render_node;
#[cfg(target_os = "linux")]
pub mod scanout_feedback;
pub mod screen_dim;
#[cfg(target_os = "linux")]
pub mod seat_lock;
pub mod selection;
#[cfg(target_os = "linux")]
pub(crate) mod session_bootstrap;
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
#[cfg(target_os = "linux")]
pub mod system_stats_service;
#[cfg(target_os = "linux")]
pub mod tablet;
#[cfg(target_os = "linux")]
pub mod tablet_pad;
#[cfg(target_os = "linux")]
pub mod text_commit;
#[cfg(target_os = "linux")]
pub mod toplevel_icon;
pub mod touch_pointer;
#[cfg(target_os = "linux")]
pub mod watchdog;
#[cfg(target_os = "linux")]
pub mod window_handles;
#[cfg(target_os = "linux")]
pub mod window_previews;
#[cfg(target_os = "linux")]
mod window_provenance;
#[cfg(target_os = "linux")]
pub(crate) mod workspace_door;
#[cfg(target_os = "linux")]
pub(crate) mod x11_windows;
#[cfg(target_os = "linux")]
pub(crate) mod xwayland;
#[cfg(target_os = "linux")]
pub(crate) mod xwayland_composer;

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

/// Which backend the compositor should drive: `--backend <winit|drm>` (or `--backend=<...>`), unknown/absent falling back to [`BackendKind::Winit`].
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
/// refusing it, more specific than [`usage_request`]'s answer for a flag it
/// does not know.
const REMOVED_RUN_MODES: &[(&str, &str)] = &[(
    "--portal-server-only",
    "--portal-server-only was removed; the compositor serves the xdg-desktop-portal \
     backend in-process (docs/architecture/security.md). Disable any unit that still passes it.",
)];

/// Why `--mode hosted` is refused. The hosted loop kept its own copy of the
/// session bootstrap — salt, tree load, ownership adoption, collaboration
/// broker, portal — and the copies had drifted apart; the compositor's winit
/// backend is the development session inside an existing desktop.
const REMOVED_HOSTED_MODE: &str = "--mode hosted was removed; run the compositor in a window \
     inside your desktop with `--backend winit` (the default) instead.";

/// Pure, testable check for a removed run-mode flag: the refusal `main` prints
/// before exiting with status 2, or `None` when no removed flag is present.
pub fn removed_run_mode_refusal(args: &[&str]) -> Option<&'static str> {
    REMOVED_RUN_MODES
        .iter()
        .find(|(flag, _)| args.contains(flag))
        .map(|(_, refusal)| *refusal)
        .or_else(|| (flag_value(args, "--mode") == Some("hosted")).then_some(REMOVED_HOSTED_MODE))
}

/// Flags the compositor reads on its own command line that take no value.
/// `--enable-portals` and `--enable-peer-sync` are still accepted, and
/// ignored, for existing units and scripts.
const SWITCHES: &[&str] = &[
    "--host-desktop-session",
    "--firstboot",
    "--firstboot-only",
    "--provision-identity",
    "--verify-collaboration-identity",
    "--disable-peer-sync",
    "--enable-peer-sync",
    "--enable-portals",
    "--headless-owner-control",
];

/// Flags that take a value, as `<flag> <value>` or `<flag>=<value>`.
const VALUED: &[&str] = &["--backend", "--mode", "--approved-checks"];

/// What `tilo-shell --help` prints.
pub const USAGE: &str = "\
usage: tilo-shell [--backend winit|drm|headless] [OPTIONS]

The Tilo compositor. A display manager starts it through the session
wrapper (/usr/libexec/tilo-session); run it by hand only to develop Tilo.

  --backend winit|drm|headless   where to draw: a window in this desktop
                                 (the default), the seat's displays, or none
  --host-desktop-session         a Tilo session added to another Linux
                                 desktop, keeping identity in this login
  --firstboot                    prepare this login before starting
  --firstboot-only               prepare this login and exit
  --provision-identity           run the identity ceremony and exit
  --verify-collaboration-identity
                                 check the identity chain and exit
  --disable-peer-sync            start without syncing with other devices
  --approved-checks PATH         load approved project checks from PATH
  -V, --version                  print the version and exit
  -h, --help                     print this help and exit
";

/// Why the command line asks for usage instead of a session.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UsageRequest {
    /// `--help` or `-h`: print [`USAGE`] and exit 0.
    Help,
    /// A flag the compositor does not know: say so, print [`USAGE`], exit 2.
    Unknown(String),
}

/// Pure, testable check, made before anything starts: `--help`/`-h`, or the
/// first flag the compositor does not read. Without it an unknown flag, or
/// a request for help, booted a whole session.
pub fn usage_request(args: &[&str]) -> Option<UsageRequest> {
    let mut iter = args.iter().copied();
    while let Some(arg) = iter.next() {
        if matches!(arg, "--help" | "-h") {
            return Some(UsageRequest::Help);
        }
        if !arg.starts_with('-') || SWITCHES.contains(&arg) {
            continue;
        }
        if VALUED.contains(&arg) {
            iter.next();
            continue;
        }
        let valued_inline = arg
            .split_once('=')
            .is_some_and(|(flag, _)| VALUED.contains(&flag));
        let removed = REMOVED_RUN_MODES.iter().any(|(flag, _)| *flag == arg);
        if !valued_inline && !removed {
            return Some(UsageRequest::Unknown(arg.to_owned()));
        }
    }
    None
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

/// Process-global wrapper around [`parse_backend`], used by [`native::run`] to decide which [`backend::Backend`] impl the compositor instantiates.
pub fn parse_backend_from_args() -> BackendKind {
    let owned: Vec<String> = std::env::args().skip(1).collect();
    let borrowed: Vec<&str> = owned.iter().map(std::string::String::as_str).collect();
    parse_backend(&borrowed)
}

/// Pure, testable backend parser: `--backend <value>`/`--backend=<value>`, defaulting to [`BackendKind::Winit`] for unknown/missing values.
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
    fn install_mode_resolves_every_flag_form() {
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
                &[&[], &["--mode", "native"]],
                &[
                    &["--enable-peer-sync"],
                    &["--backend", "winit", "--enable-peer-sync"],
                    &["--enable-peer-sync", "--mode=native"],
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

    /// `tilo-shell --help` booted a session (spawner, identity checks)
    /// instead of answering. Help and unknown flags are answered before
    /// anything starts; every flag a unit, wrapper or harness passes still
    /// starts the session.
    #[test]
    fn help_and_unknown_flags_are_answered_before_a_session_starts() {
        for args in [&["--help"][..], &["-h"], &["--backend", "drm", "--help"]] {
            assert_eq!(usage_request(args), Some(UsageRequest::Help), "{args:?}");
        }
        assert_eq!(
            usage_request(&["--backend", "drm", "--frobnicate"]),
            Some(UsageRequest::Unknown("--frobnicate".into()))
        );
        assert_eq!(
            usage_request(&["-x"]),
            Some(UsageRequest::Unknown("-x".into()))
        );
        assert_eq!(
            usage_request(&["--version", "--backend", "drm"]),
            Some(UsageRequest::Unknown("--version".into())),
            "--version is answered alone; with a session's flags it is a mistake"
        );
        let starts: [&[&str]; 12] = [
            &[],
            &["--backend", "drm"],
            &["--backend=headless"],
            &["--mode", "native", "--backend", "headless"],
            &["--host-desktop-session", "--firstboot", "--backend", "drm"],
            &["--host-desktop-session", "--provision-identity"],
            &["--host-desktop-session", "--verify-collaboration-identity"],
            &["--firstboot-only"],
            &[
                "--enable-portals",
                "--enable-peer-sync",
                "--disable-peer-sync",
            ],
            &["--headless-owner-control", "--backend", "headless"],
            &["--approved-checks", "/etc/tilo/checks.toml"],
            &["--approved-checks=/etc/tilo/checks.toml"],
        ];
        for args in starts {
            assert_eq!(usage_request(args), None, "{args:?} must start a session");
        }
        // A removed run mode keeps its own, more specific refusal.
        assert_eq!(usage_request(&["--portal-server-only"]), None);
        assert!(USAGE.contains("--backend") && USAGE.contains("--help"));
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
        // The hosted loop is refused in both flag forms and among others,
        // naming the backend that replaced it, so a stale script fails loudly
        // instead of booting a session with different semantics.
        let hosted: [&[&str]; 3] = [
            &["--mode", "hosted"],
            &["--mode=hosted"],
            &["--enable-peer-sync", "--mode", "hosted", "--enable-portals"],
        ];
        for args in hosted {
            let refusal = removed_run_mode_refusal(args)
                .unwrap_or_else(|| panic!("{args:?} must be refused"));
            assert!(
                refusal.contains("--mode hosted was removed")
                    && refusal.contains("--backend winit"),
                "the refusal names the flag and its replacement: {refusal}"
            );
        }
        // No removed flag: the ordinary boot paths are untouched, including
        // the still-accepted `--mode native` and an unknown mode value.
        let accepted: [&[&str]; 8] = [
            &[],
            &["--mode", "native"],
            &["--mode=native"],
            &["--mode", "junk"],
            &["--mode"],
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
