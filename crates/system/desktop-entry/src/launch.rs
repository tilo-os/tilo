//! Starting a host application, and what that means.
//!
//! # The decision this module encodes
//!
//! A qOS package runs inside qOS's sandbox: bubblewrap with `--unshare-all`, a seccomp
//! deny-list, Landlock, one writable directory, and an environment carrying the app's room. A
//! host application **does not**, and cannot be made to:
//!
//! - A Flatpak brings its own sandbox, which needs `unshare(2)`/`mount(2)`/`pivot_root(2)` —
//!   denied by qOS's baseline seccomp filter — plus `/usr/bin` and its own runtime tree, not
//!   bound in qOS's sandbox. `flatpak run` inside qOS's sandbox doesn't doubly-confine it;
//!   it just fails to start.
//! - A plain distribution binary has no sandbox and no manifest to derive a policy from —
//!   qOS's sandbox is built out of a package manifest, and a `.desktop` file is not one.
//!
//! So a host application runs **unconfined**, as the user, with the user's own files and
//! network: no `QOS_WORKSPACE_ID` (no collaboration room, nothing partitioned per room), no
//! capability file (there's no sandbox to enforce workspace grants against, and passing them
//! would falsely imply the app was limited to them), and no per-room state partition (it
//! writes to the user's real `$HOME`).
//!
//! Those absences are why [`HostLaunch::is_confined`] is `false`, and why the desktop labels
//! these apps separately everywhere. The failure mode guarded against is not "a host app
//! escapes the sandbox" (it never had one) but "the user cannot tell which kind of app they
//! are looking at" — blurring the two is the bug.
//!
//! # What is still controlled
//!
//! The environment. A host app is spawned from an explicit allowlist of the launcher's own
//! variables plus a fixed set of overrides, not whatever the compositor happens to be
//! carrying — keeping qOS's internal variables (room id, app directory, identity path) from
//! leaking into a process not entitled to them.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::process::Command;

use crate::entry::{DesktopEntry, EntrySource, EntryType};
use crate::exec::{ExecError, ExpandContext, expand, split_exec};

/// Variables inherited from the launching process, when set. Everything a normal desktop
/// app needs to find its user, theme, locale and session bus — nothing that describes qOS's
/// own confinement, since a host app is not subject to it.
const INHERITED_ENV: &[&str] = &[
    "HOME",
    "USER",
    "LOGNAME",
    "SHELL",
    "PATH",
    "LANG",
    "LANGUAGE",
    "LC_ALL",
    "LC_CTYPE",
    "LC_MESSAGES",
    "LC_NUMERIC",
    "LC_TIME",
    "TZ",
    "TERM",
    "XDG_DATA_HOME",
    "XDG_CONFIG_HOME",
    "XDG_CACHE_HOME",
    "XDG_STATE_HOME",
    "XDG_DATA_DIRS",
    "XDG_CONFIG_DIRS",
    "DBUS_SESSION_BUS_ADDRESS",
    // Input-method bridges are desktop interoperability, not qOS authority.
    // Omitting these makes compose/IME input fail specifically in Electron,
    // GTK and Qt host applications while native qOS fields keep working.
    "XMODIFIERS",
    "GTK_IM_MODULE",
    "QT_IM_MODULE",
    // Qt 6 on Fedora advertises its ordered Wayland/IBus fallback through
    // the plural spelling; keeping only QT_IM_MODULE loses that selection.
    "QT_IM_MODULES",
    "SDL_IM_MODULE",
];

/// Variables that are always cleared, whatever the parent had. `DISPLAY` and
/// `XAUTHORITY` are accepted only as the explicit, matched rootless-Xwayland
/// pair in [`LaunchEnv`]; stale values from the preceding desktop can point at
/// another session. `QOS_*` describe confinement this process does not have.
const CLEARED_ENV: &[&str] = &[
    "DISPLAY",
    "XAUTHORITY",
    "QOS_WORKSPACE_ID",
    "QOS_APP_DIR",
    "QOS_USER_IDENTITY",
    "QOS_IDENTITY_BROKER_FD",
    "QOS_CLI_COMMAND",
    "QOS_NET_GATE",
    "QOS_HOST_ACTIVATION_TOKEN",
];

/// The desktop name qOS announces, and what `OnlyShowIn`/`NotShowIn` are matched against.
pub const XDG_CURRENT_DESKTOP: &str = "nirios";

/// Marker set in a host application's environment. Not a capability and not read by anything
/// qOS enforces — there so `ps`, a crash report or a confused user can tell it was started by
/// qOS and is not confined by it.
pub const HOST_APP_ENV: &str = "QOS_HOST_APP";

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum HostLaunchError {
    #[error("{id} is a {kind:?} entry, not an application, so there is nothing to run")]
    NotAnApplication { id: String, kind: String },
    #[error("{id} declares no Exec=, so there is nothing to run")]
    NoExec { id: String },
    #[error("{id} has an unusable Exec= line: {source}")]
    BadExec {
        id: String,
        #[source]
        source: ExecError,
    },
    #[error(
        "{id} asks to be run inside a terminal window (Terminal=true). qOS does not \
         yet host a terminal for applications it did not package — run it from the \
         qOS terminal instead."
    )]
    NeedsTerminal { id: String },
    #[error("no application on this system has the desktop-file id {0}")]
    NotFound(String),
}

/// A resolved host-application launch: the exact argv and environment. Pure data — building
/// one performs no I/O and starts nothing, so the command it *would* use is assertable in a
/// test without a process appearing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HostLaunch {
    /// Desktop-file id of the entry this came from.
    pub desktop_id: String,
    /// The entry's `Name=`, for the UI.
    pub display_name: String,
    pub program: PathBuf,
    pub args: Vec<String>,
    /// The child's complete environment. Not overrides — the child is spawned with `env_clear()`
    /// and then exactly this.
    pub env: BTreeMap<String, String>,
    /// Where its window is expected to announce itself. See [`DesktopEntry::window_identities`].
    pub window_identities: Vec<String>,
    pub source: EntrySource,
}

impl HostLaunch {
    /// Always `false`, and a method rather than a constant so a caller asking "is this app
    /// confined?" gets an answer from the type that knows, not a comment. Nothing in this crate
    /// can return `true` here — a confined launch is a `.qpkg` going through `qos-launcher run`.
    pub fn is_confined(&self) -> bool {
        false
    }

    /// Build the `Command` this launch describes.
    pub fn to_command(&self) -> Command {
        let mut cmd = Command::new(&self.program);
        cmd.args(&self.args);
        cmd.env_clear();
        for (k, v) in &self.env {
            cmd.env(k, v);
        }
        cmd
    }
}

/// What the launching desktop knows that the `.desktop` file does not.
#[derive(Debug, Clone, Default)]
pub struct LaunchEnv {
    /// Host path of the compositor's Wayland socket, e.g. `/run/user/1000/wayland-1`.
    /// `WAYLAND_DISPLAY` is set to its file name and `XDG_RUNTIME_DIR` to its directory, so a
    /// host app connects to the compositor that launched it rather than whatever the launcher's
    /// own environment pointed at.
    pub wayland_socket: Option<PathBuf>,
    /// Session-wide rootless Xwayland display, when the compositor started a
    /// compatibility bridge. Native Wayland clients still receive both and
    /// choose Wayland normally; X11-only applications use this fallback.
    pub x11_display: Option<String>,
    /// MIT-MAGIC-COOKIE authority file for [`Self::x11_display`].
    pub x11_authority: Option<PathBuf>,
    /// A compositor-minted, single-use activation token for this user
    /// initiated launch. It selects/focuses the first toplevel only; it is
    /// not a sandbox capability or qOS authority.
    pub activation_token: Option<String>,
    /// Documents to open, as absolute host paths. Feed `%f`/`%F`, and `%u`/`%U` as `file://` URLs.
    pub files: Vec<String>,
    /// URLs to open. Feed `%u`/`%U`.
    pub urls: Vec<String>,
}

/// Resolve an entry into a launch.
///
/// # Errors
///
/// Returns [`HostLaunchError::NotAnApplication`] if `entry` is not
/// `Type=Application`, [`HostLaunchError::NeedsTerminal`] if it declares
/// `Terminal=true`, [`HostLaunchError::NoExec`] if it has no usable `Exec=`,
/// or [`HostLaunchError::BadExec`] if `Exec=` fails to parse.
pub fn plan(entry: &DesktopEntry, env: &LaunchEnv) -> Result<HostLaunch, HostLaunchError> {
    if entry.entry_type != EntryType::Application {
        return Err(HostLaunchError::NotAnApplication {
            id: entry.id.clone(),
            kind: format!("{:?}", entry.entry_type),
        });
    }
    if entry.terminal {
        return Err(HostLaunchError::NeedsTerminal {
            id: entry.id.clone(),
        });
    }
    let exec = entry
        .exec
        .as_deref()
        .ok_or_else(|| HostLaunchError::NoExec {
            id: entry.id.clone(),
        })?;
    let raw = split_exec(exec).map_err(|source| HostLaunchError::BadExec {
        id: entry.id.clone(),
        source,
    })?;
    let argv = expand(
        &raw,
        &ExpandContext {
            files: &env.files,
            urls: &env.urls,
            icon: entry.icon.as_deref(),
            name: Some(&entry.name),
            path: Some(&entry.path),
        },
    );
    let mut argv = argv.into_iter();
    let program = argv.next().ok_or_else(|| HostLaunchError::NoExec {
        id: entry.id.clone(),
    })?;

    Ok(HostLaunch {
        desktop_id: entry.id.clone(),
        display_name: entry.name.clone(),
        program: PathBuf::from(program),
        args: argv.collect(),
        env: build_env(env),
        window_identities: entry.window_identities(),
        source: entry.source.clone(),
    })
}

/// The child's whole environment: an allowlist of the parent's, plus the overrides the
/// desktop is authoritative for.
fn build_env(env_spec: &LaunchEnv) -> BTreeMap<String, String> {
    let mut env = BTreeMap::new();
    for key in INHERITED_ENV {
        if let Some(value) = std::env::var_os(key)
            && !value.is_empty()
        {
            env.insert((*key).to_string(), value.to_string_lossy().to_string());
        }
    }
    // The compositor's socket wins over anything inherited. Both halves must move together:
    // WAYLAND_DISPLAY resolves relative to XDG_RUNTIME_DIR, so mixing sources here is how an
    // app ends up connecting to a different compositor.
    if let Some(socket) = env_spec.wayland_socket.as_deref() {
        if let Some(name) = socket.file_name() {
            env.insert(
                "WAYLAND_DISPLAY".to_string(),
                name.to_string_lossy().to_string(),
            );
        }
        if let Some(dir) = socket.parent() {
            env.insert(
                "XDG_RUNTIME_DIR".to_string(),
                dir.to_string_lossy().to_string(),
            );
        }
    } else if let Some(dir) = std::env::var_os("XDG_RUNTIME_DIR") {
        env.insert(
            "XDG_RUNTIME_DIR".to_string(),
            dir.to_string_lossy().to_string(),
        );
    }
    env.insert(
        "XDG_CURRENT_DESKTOP".to_string(),
        XDG_CURRENT_DESKTOP.to_string(),
    );
    env.insert(
        "XDG_SESSION_DESKTOP".to_string(),
        XDG_CURRENT_DESKTOP.to_string(),
    );
    env.insert("XDG_SESSION_TYPE".to_string(), "wayland".to_string());
    // Electron releases differ on whether Wayland auto-selection is the
    // default.  This asks Electron to inspect the authoritative Wayland/X11
    // pair above instead of inheriting a stale backend preference.
    env.insert(
        "ELECTRON_OZONE_PLATFORM_HINT".to_string(),
        "auto".to_string(),
    );
    env.insert(HOST_APP_ENV.to_string(), "1".to_string());
    for key in CLEARED_ENV {
        env.remove(*key);
    }
    if let Some(token) = env_spec
        .activation_token
        .as_deref()
        .filter(|token| !token.is_empty())
    {
        env.insert("XDG_ACTIVATION_TOKEN".to_string(), token.to_string());
    }
    if let (Some(display), Some(authority)) = (&env_spec.x11_display, &env_spec.x11_authority) {
        env.insert("DISPLAY".to_string(), display.clone());
        env.insert(
            "XAUTHORITY".to_string(),
            authority.to_string_lossy().to_string(),
        );
        env.insert("_JAVA_AWT_WM_NONREPARENTING".to_string(), "1".to_string());
        // Keep the conventional startup-id channel for X11 clients.  The
        // installed xwayland-satellite also reads XDG_ACTIVATION_TOKEN above
        // from the X client's environment and relays it on map.
        if let Some(token) = env_spec
            .activation_token
            .as_deref()
            .filter(|token| !token.is_empty())
        {
            env.insert("DESKTOP_STARTUP_ID".to_string(), token.to_string());
        }
    }
    env
}

/// Find the entry with this desktop-file id on the current system and plan a launch for it.
///
/// # Errors
///
/// Returns [`HostLaunchError::NotFound`] if no scanned entry has this id, or
/// any error [`plan`] can return.
pub fn plan_by_id(desktop_id: &str, env: &LaunchEnv) -> Result<HostLaunch, HostLaunchError> {
    let scan = crate::cache::cached_scan();
    let entry = scan
        .find(desktop_id)
        .ok_or_else(|| HostLaunchError::NotFound(desktop_id.to_string()))?;
    plan(entry, env)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::entry::parse;
    use std::path::Path;

    fn entry(src: &str, id: &str) -> DesktopEntry {
        parse(src, id, Path::new("/usr/share/applications/x.desktop"), &[]).expect("parses")
    }

    fn spotify() -> DesktopEntry {
        entry(
            "[Desktop Entry]\nType=Application\nName=Spotify\nIcon=com.spotify.Client\n\
             Exec=/usr/bin/flatpak run --file-forwarding com.spotify.Client @@u %U @@\n\
             StartupWMClass=spotify\nX-Flatpak=com.spotify.Client\n",
            "com.spotify.Client",
        )
    }

    #[test]
    fn a_flatpak_launch_is_the_flatpak_command_line_unwrapped() {
        let launch = plan(&spotify(), &LaunchEnv::default()).expect("plans");
        assert_eq!(launch.program, PathBuf::from("/usr/bin/flatpak"));
        assert_eq!(
            launch.args,
            vec!["run", "--file-forwarding", "com.spotify.Client"]
        );
        assert_eq!(
            launch.source,
            EntrySource::Flatpak {
                app_id: "com.spotify.Client".into()
            }
        );
    }

    /// The whole point of the module. A host launch is never confined, and the type says so
    /// rather than a comment.
    #[test]
    fn a_host_launch_is_never_confined() {
        let launch = plan(&spotify(), &LaunchEnv::default()).expect("plans");
        assert!(!launch.is_confined());
    }

    /// The three things a confined qOS app has and a host app must not be told it has.
    #[test]
    fn the_environment_carries_no_room_no_app_dir_and_no_identity() {
        let launch = plan(&spotify(), &LaunchEnv::default()).expect("plans");
        for absent in [
            "QOS_WORKSPACE_ID",
            "QOS_APP_DIR",
            "QOS_USER_IDENTITY",
            "QOS_NET_GATE",
        ] {
            assert!(
                !launch.env.contains_key(absent),
                "{absent} describes a confinement a host app does not have"
            );
        }
        assert_eq!(launch.env.get(HOST_APP_ENV).map(String::as_str), Some("1"));
    }

    #[test]
    fn the_wayland_socket_sets_both_halves_of_the_display_handoff() {
        let launch = plan(
            &spotify(),
            &LaunchEnv {
                wayland_socket: Some(PathBuf::from("/run/user/1000/wayland-3")),
                ..Default::default()
            },
        )
        .expect("plans");
        assert_eq!(
            launch.env.get("WAYLAND_DISPLAY").map(String::as_str),
            Some("wayland-3")
        );
        assert_eq!(
            launch.env.get("XDG_RUNTIME_DIR").map(String::as_str),
            Some("/run/user/1000")
        );
        assert_eq!(
            launch.env.get("XDG_CURRENT_DESKTOP").map(String::as_str),
            Some("nirios")
        );
        assert!(
            !launch.env.contains_key("DISPLAY"),
            "without an explicit Xwayland bridge, a stale DISPLAY is worse than none"
        );
    }

    #[test]
    fn activation_token_is_standard_child_environment_not_qos_authority() {
        let launch = plan(
            &spotify(),
            &LaunchEnv {
                activation_token: Some("one-shot-token".into()),
                x11_display: Some(":42".into()),
                x11_authority: Some(PathBuf::from("/run/user/1000/qos/Xauthority")),
                ..Default::default()
            },
        )
        .expect("plans");
        assert_eq!(
            launch.env.get("XDG_ACTIVATION_TOKEN").map(String::as_str),
            Some("one-shot-token")
        );
        assert_eq!(
            launch.env.get("DESKTOP_STARTUP_ID").map(String::as_str),
            Some("one-shot-token"),
            "classic X11 clients receive the startup-notification bridge too; xwayland-satellite itself reads XDG_ACTIVATION_TOKEN"
        );
        assert!(!launch.env.contains_key("QOS_HOST_ACTIVATION_TOKEN"));
    }

    #[test]
    fn a_rootless_xwayland_bridge_adds_an_authenticated_x11_fallback() {
        let launch = plan(
            &spotify(),
            &LaunchEnv {
                wayland_socket: Some(PathBuf::from("/run/user/1000/wayland-3")),
                x11_display: Some(":1234".into()),
                x11_authority: Some(PathBuf::from("/run/user/1000/qos-session/Xauthority")),
                ..Default::default()
            },
        )
        .expect("plans");
        assert_eq!(launch.env.get("DISPLAY").map(String::as_str), Some(":1234"));
        assert_eq!(
            launch.env.get("XAUTHORITY").map(String::as_str),
            Some("/run/user/1000/qos-session/Xauthority")
        );
        assert_eq!(
            launch
                .env
                .get("_JAVA_AWT_WM_NONREPARENTING")
                .map(String::as_str),
            Some("1")
        );
    }

    #[test]
    #[serial_test::serial]
    fn host_apps_keep_input_methods_and_select_the_qos_desktop() {
        let previous = [
            "XMODIFIERS",
            "GTK_IM_MODULE",
            "QT_IM_MODULE",
            "QT_IM_MODULES",
            "SDL_IM_MODULE",
        ]
        .map(|key| {
            let value = std::env::var_os(key);
            unsafe { std::env::set_var(key, "ibus-test") };
            (key, value)
        });
        let launch = plan(&spotify(), &LaunchEnv::default()).expect("plans");
        for key in [
            "XMODIFIERS",
            "GTK_IM_MODULE",
            "QT_IM_MODULE",
            "QT_IM_MODULES",
            "SDL_IM_MODULE",
        ] {
            assert_eq!(launch.env.get(key).map(String::as_str), Some("ibus-test"));
        }
        assert_eq!(
            launch.env.get("XDG_SESSION_DESKTOP").map(String::as_str),
            Some("nirios")
        );
        assert_eq!(
            launch
                .env
                .get("ELECTRON_OZONE_PLATFORM_HINT")
                .map(String::as_str),
            Some("auto")
        );
        for (key, value) in previous {
            match value {
                Some(value) => unsafe { std::env::set_var(key, value) },
                None => unsafe { std::env::remove_var(key) },
            }
        }
    }

    #[test]
    fn a_document_reaches_the_command_line() {
        let launch = plan(
            &spotify(),
            &LaunchEnv {
                files: vec!["/tmp/a.txt".into()],
                ..Default::default()
            },
        )
        .expect("plans");
        assert_eq!(
            launch.args,
            vec![
                "run",
                "--file-forwarding",
                "com.spotify.Client",
                "@@u",
                "file:///tmp/a.txt",
                "@@"
            ]
        );
    }

    #[test]
    fn a_terminal_entry_is_refused_with_a_reason() {
        let e = entry(
            "[Desktop Entry]\nType=Application\nName=htop\nExec=/usr/bin/htop\nTerminal=true\n",
            "htop",
        );
        assert!(matches!(
            plan(&e, &LaunchEnv::default()),
            Err(HostLaunchError::NeedsTerminal { .. })
        ));
    }

    #[test]
    fn a_link_entry_has_nothing_to_run() {
        let e = entry(
            "[Desktop Entry]\nType=Link\nName=Site\nURL=https://example.com\n",
            "site",
        );
        assert!(matches!(
            plan(&e, &LaunchEnv::default()),
            Err(HostLaunchError::NotAnApplication { .. })
        ));
    }

    #[test]
    fn window_identities_ride_along_for_correlation() {
        let launch = plan(&spotify(), &LaunchEnv::default()).expect("plans");
        assert_eq!(
            launch.window_identities,
            vec!["spotify", "com.spotify.Client"]
        );
    }

    /// `env_clear()` then the map: nothing from the launcher's own environment reaches the
    /// child except through the allowlist.
    #[test]
    fn to_command_starts_from_an_empty_environment() {
        let launch = plan(&spotify(), &LaunchEnv::default()).expect("plans");
        let cmd = launch.to_command();
        let vars: Vec<String> = cmd
            .get_envs()
            .filter_map(|(k, v)| v.map(|_| k.to_string_lossy().to_string()))
            .collect();
        for k in launch.env.keys() {
            assert!(vars.contains(k), "{k} must reach the child");
        }
        assert_eq!(cmd.get_program(), std::ffi::OsStr::new("/usr/bin/flatpak"));
    }

    #[test]
    fn a_real_host_process_receives_only_the_planned_desktop_handoff() {
        let probe = entry(
            "[Desktop Entry]\nType=Application\nName=Environment probe\nExec=/usr/bin/env\n",
            "org.example.EnvProbe",
        );
        let launch = plan(
            &probe,
            &LaunchEnv {
                wayland_socket: Some(PathBuf::from("/run/user/1000/wayland-qos")),
                x11_display: Some(":71".into()),
                x11_authority: Some(PathBuf::from("/run/user/1000/qos/Xauthority")),
                ..Default::default()
            },
        )
        .expect("plans");
        let output = launch.to_command().output().expect("run environment probe");
        assert!(output.status.success());
        let stdout = String::from_utf8(output.stdout).expect("environment is UTF-8");
        for expected in [
            "WAYLAND_DISPLAY=wayland-qos",
            "XDG_RUNTIME_DIR=/run/user/1000",
            "DISPLAY=:71",
            "XDG_CURRENT_DESKTOP=nirios",
            "XDG_SESSION_DESKTOP=nirios",
            "ELECTRON_OZONE_PLATFORM_HINT=auto",
            "QOS_HOST_APP=1",
        ] {
            assert!(
                stdout.lines().any(|line| line == expected),
                "missing {expected}"
            );
        }
        assert!(
            !stdout
                .lines()
                .any(|line| line.starts_with("QOS_WORKSPACE_ID=")),
            "portal attribution must never become ambient workspace authority"
        );
    }
}
